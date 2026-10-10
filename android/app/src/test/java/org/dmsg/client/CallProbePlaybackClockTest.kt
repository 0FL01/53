package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class CallProbePlaybackClockTest {
    private fun calibrated(): CallProbePlaybackClock = CallProbePlaybackClock().also {
        it.observe(0, 1, 1, 16_000, 0)
        it.observe(160_000, 10_000_000_001, 10_000_000_001, 16_000, 0)
        it.observe(320_000, 20_000_000_001, 20_000_000_001, 16_000, 0)
    }

    @Test fun fractionalHertzDoesNotAccumulateHundredPpmResidual() {
        for (ppm in listOf(-500, -100, 100, 500)) {
            val clock = calibrated()
            var consumed = 0L
            for (second in 0 until 2_700) {
                val now = 20_000_000_001L + second * 1_000_000_000L
                clock.observe(320_000L + second * 16_000L, now, now, 16_000, 0)
                val output = clock.choose(now, 48_000L * (1_000_000 + ppm), 1_000_000_000_000_000L, true)
                assertTrue(output.calibrated && output.healthy)
                consumed += output.rate
            }
            val expected = 16_000.0 * (1.0 + ppm / 1e6) * 2_700
            assertTrue("fractional command error must stay below one sample", kotlin.math.abs(consumed - expected) < 1.01)
        }
    }

    @Test fun clockUnavailableUnderrunAndStallsDoNotBecomeFrequency() {
        val clock = CallProbePlaybackClock()
        clock.observe(0, 1, 1, 16_000, 0)
        clock.observe(320_000, 20_000_000_001, 20_000_000_001, 16_000, 1)
        assertFalse(clock.choose(20_000_000_001, 48_000, 1_000_000_000, true).calibrated)
        val established = calibrated()
        val good = established.choose(20_000_000_001, 48_024, 1_000_000_000, true)
        assertEquals(16_008, good.rate)
        val stale = established.choose(24_000_000_001, 48_000, 1_000_000_000, false)
        assertEquals(good.rate, stale.rate)
        assertFalse(stale.healthy)
        assertFalse(established.choose(24_000_000_001, 0, 0, true).healthy)
    }

    @Test fun relativeHardwareSkewIsCorrectedAndUnsupportedRatesAreExplicit() {
        val clock = CallProbePlaybackClock()
        clock.observe(0, 1, 1, 16_000, 0)
        clock.observe(160_016, 10_000_000_001, 10_000_000_001, 16_000, 0)
        clock.observe(320_032, 20_000_000_001, 20_000_000_001, 16_000, 0)
        var sum = 0L
        for (second in 0 until 1_000) {
            val now = 20_000_000_001L + second * 1_000_000_000L
            clock.observe(320_032L + second * 16_002L, now, now, 16_000, 0)
            val result = clock.choose(now, 48_000, 1_000_000_000, true)
            sum += result.rate
        }
        assertTrue(kotlin.math.abs(sum - 16_000_000.0 / 1.0001) < 1.01)
        assertFalse(clock.choose(1_019_000_000_001, 48_096, 1_000_000_000, true).healthy)
    }

    @Test fun manualActuatorRestoresAutomaticAndLostClockIsObservable() {
        val clock = calibrated()
        assertEquals(16_008, clock.choose(20_000_000_001, 48_000, 1_000_000_000, true, 8).rate)
        assertEquals(16_000, clock.choose(21_000_000_001, 48_000, 1_000_000_000, true, 0).rate)
        clock.observe(-1, -1, 21_000_000_001, 16_000, 0)
        val absent = clock.choose(21_000_000_001, 48_000, 1_000_000_000, true)
        assertTrue(absent.calibrated)
        assertFalse(absent.healthy)
        assertEquals(16_000, absent.rate)
    }

    @Test fun unreportedStartupStallCannotPoisonLaterSteadyHardwareWindow() {
        val clock = CallProbePlaybackClock()
        val origin = 1_000_000_000L
        // Some HALs initially publish a position before steady presentation,
        // without advancing underrunCount. That window is not a rate sample.
        for (step in 0..200) {
            val elapsed = step * 100_000_000L
            val frame = 640L + elapsed.coerceAtMost(1_000_000_000L) / 125_000L +
                (elapsed - 1_000_000_000L).coerceAtLeast(0L) / 62_500L
            clock.observe(frame, origin + elapsed, origin + elapsed, 16_000, 0)
        }
        assertFalse(clock.choose(origin + 20_000_000_000L, 48_000, 1_000_000_000, true).calibrated)
        // A new complete steady 20s observation must calibrate, not retain the
        // rejected startup anchor for 60s or certify the earlier stalled span.
        for (step in 201..401) {
            val elapsed = step * 100_000_000L
            clock.observe(640L + 8_000L + (elapsed - 1_000_000_000L) / 62_500L,
                origin + elapsed, origin + elapsed, 16_000, 0)
        }
        val recovered = clock.choose(origin + 40_100_000_000L, 48_000, 1_000_000_000, true)
        assertTrue(recovered.calibrated && recovered.healthy)
        assertEquals(16_000, recovered.rate)
        assertEquals(0L, recovered.sinkPpb)
    }

    @Test fun inRangeStartupPauseMustNotFreezeAnUnsupportedNeutral() {
        for (frameStart in listOf(0L, 640L)) {
            val clock = CallProbePlaybackClock()
            val origin = 1_000_000_000L
            val startupPause = 18_340_000L
            var initial: CallProbePlaybackClock.Output? = null
            for (step in 0..400) {
                val elapsed = step * 100_000_000L
                // App-consumed frames: initially nothing is played, then the
                // physical clock consumes exactly 16k/s. No underrun transition
                // is reported for this HAL startup pause; all timestamps are
                // real monotonic presentation instants, retrieved 40ms later.
                val frame = frameStart + (elapsed - startupPause).coerceAtLeast(0L) / 62_500L
                clock.observe(frame, origin + elapsed, origin + elapsed + 40_000_000L, 16_000, 3)
                if (step == 200) initial = clock.choose(origin + elapsed + 40_000_000L,
                    48_000, 1_000_000_000, true)
            }
            val recovered = clock.choose(origin + 40_040_000_000L, 48_000, 1_000_000_000, true)
            assertTrue("a steady second 20s window must recover: first=$initial, second=$recovered",
                recovered.calibrated && recovered.healthy)
            assertFalse("the biased first window must not freeze: $initial", initial!!.calibrated)
            assertFalse(initial.healthy)
            assertEquals(16_000, recovered.rate)
            assertEquals(0L, recovered.sinkPpb)
            assertEquals(0.0, recovered.relativePpm!!, 1e-6)
        }
    }

    @Test fun initialQualificationUsesActualTimestampSpansAndFrameTickResolution() {
        for (ppm in listOf(-500, -100, 0, 100, 500)) {
            val clock = CallProbePlaybackClock()
            val origin = 1_000_000_000L
            fun observe(step: Int, tickError: Long = 0) {
                val elapsed = step * 100_000_000L
                val frame = 640L + 16_000L * (1_000_000 + ppm) * step / 10_000_000 + tickError
                clock.observe(frame, origin + elapsed, origin + elapsed + 40_000_000L, 16_000, 0)
            }
            // One frame of endpoint uncertainty, including opposite midpoint
            // error, must not reject a genuine +/-500ppm oscillator.
            observe(0, -1)
            observe(100, 1)
            observe(199)
            assertFalse(clock.choose(origin + 19_940_000_000L, 48_000, 1_000_000_000, true).calibrated)
            observe(200, -1)
            val output = clock.choose(origin + 20_040_000_000L,
                48_000L * (1_000_000 + ppm), 1_000_000_000_000_000L, true)
            assertTrue("physical $ppm ppm: $output", output.calibrated && output.healthy)
            assertEquals(16_000, output.rate)
            assertEquals(ppm * 1_000L, output.sinkPpb)
            assertEquals(0.0, output.relativePpm!!, 1e-6)
        }
        val sparse = CallProbePlaybackClock()
        sparse.observe(640, 1, 1, 16_000, 0)
        sparse.observe(320_640, 20_000_000_001, 20_000_000_001, 16_000, 0)
        assertFalse("two endpoints alone cannot qualify startup",
            sparse.choose(20_000_000_001, 48_000, 1_000_000_000, true).calibrated)
        sparse.observe(480_640, 30_000_000_001, 30_000_000_001, 16_000, 0)
        assertTrue(sparse.choose(30_000_000_001, 48_000, 1_000_000_000, true).healthy)
    }

    @Test fun stableUnsupportedHardwareIsNotReclassifiedAsStartup() {
        for (ppm in listOf(-1_500, -900, 900, 1_500)) {
            val clock = CallProbePlaybackClock()
            val origin = 1_000_000_000L
            for (step in 0..600) {
                val now = origin + step * 100_000_000L
                val frame = 640L + 16_000L * (1_000_000 + ppm) * step / 10_000_000
                clock.observe(frame, now, now, 16_000, 3)
            }
            val output = clock.choose(origin + 60_000_000_000L, 48_000, 1_000_000_000, true)
            assertFalse("unsupported physical $ppm ppm must stay explicit: $output", output.healthy)
            assertEquals("no unsupported actuator command", 16_000, output.rate)
            if (kotlin.math.abs(ppm) < 1_000) {
                assertTrue(output.calibrated)
                assertEquals(ppm * 1_000L, output.sinkPpb)
                assertTrue(kotlin.math.abs(output.relativePpm!!) > 500)
                assertEquals(0L, output.rejectionMask)
            } else {
                assertFalse(output.calibrated)
                assertNull(output.sinkPpb)
                assertEquals(8L, output.rejectionMask)
            }
        }
    }

    @Test fun initialQualificationCannotSpanRateUnderrunOrMissingTimestamp() {
        for (change in listOf("rate", "underrun", "missing")) {
            val clock = CallProbePlaybackClock()
            val origin = 1_000_000_000L
            val readyStep = if (change == "missing") 351 else 350
            for (step in 0..readyStep) {
                val now = origin + step * 100_000_000L
                val rate = if (change == "rate" && step >= 150) 16_008 else 16_000
                val underruns = if (change == "underrun" && step >= 150) 1 else 0
                val frame = 640L + step.coerceAtMost(150) * 1_600L +
                    (step - 150).coerceAtLeast(0) * rate / 10L
                if (change == "missing" && step == 150) clock.observe(-1, -1, now, rate, underruns)
                else clock.observe(frame, now, now, rate, underruns)
                val output = clock.choose(now, 48_000, 1_000_000_000, true)
                if (step < readyStep) {
                    assertFalse("$change must restart both spans at step=$step: $output", output.calibrated)
                } else {
                    assertTrue("fresh 20s after $change: $output", output.calibrated && output.healthy)
                    assertEquals(0L, output.sinkPpb)
                }
            }
        }
    }

    @Test fun establishedNeutralSurvivesActualDitherJitterAndUnderrunChanges() {
        for (ppm in listOf(-500, -100, 100, 500)) {
            val clock = CallProbePlaybackClock()
            val origin = 1_000_000_000L
            var frames = 640.0
            var actualRate = 16_000
            var commands = 0L
            for (step in 0..1_200) {
                if (step > 0) frames += actualRate / 10.0
                val now = origin + step * 100_000_000L
                // After valid calibration, sub-ms timestamp-position jitter and
                // real rate/underrun changes cannot replace the neutral ratio.
                val jitter = if (step <= 200) 0L else if (step % 2 == 0) 12L else -12L
                val underruns = if (step >= 900) 3 else if (step >= 600) 1 else 0
                clock.observe(kotlin.math.floor(frames).toLong() + jitter, now, now, actualRate, underruns)
                val output = clock.choose(now, 48_000L * (1_000_000 + ppm), 1_000_000_000_000_000L, true)
                actualRate = output.rate
                if (step >= 200) {
                    assertTrue("$ppm ppm at step=$step: $output", output.calibrated && output.healthy)
                    assertEquals("the measured neutral must stay 1.0",
                        kotlin.math.round((output.rate / 16_000.0 - 1.0) * 1e9).toLong(), output.sinkPpb)
                    if (step % 10 == 0 && step < 1_200) commands += output.rate
                }
            }
            val expected = 16_000.0 * (1.0 + ppm / 1e6) * 100
            assertTrue("actual dither command error below one sample", kotlin.math.abs(commands - expected) < 1.01)
        }
    }

    @Test fun actualTimestampFreshnessAndDiscontinuitiesRemainExplicit() {
        val date = 20_000_000_001L
        val clock = calibrated()
        clock.observe(320_000, date, date + 400_000_000L, 16_000, 0)
        assertTrue(clock.choose(date + 500_000_000L, 48_000, 1_000_000_000, true).healthy)
        assertFalse("retrieving a duplicate does not refresh its presentation date",
            clock.choose(date + 500_000_001L, 48_000, 1_000_000_000, true).healthy)
        assertFalse(clock.choose(date - 1, 48_000, 1_000_000_000, true).healthy)
        assertFalse(clock.choose(date, 0, 1_000_000_000, true).healthy)
        assertFalse(clock.choose(date, 48_000, 0, true).healthy)
        assertFalse(clock.choose(date, 48_000, 1_000_000_000, false).healthy)

        data class Bad(val frame: Long, val timestamp: Long, val now: Long, val rate: Int, val mask: Long)
        for (bad in listOf(
            Bad(-1, date + 100_000_000L, date + 100_000_000L, 16_000, 1),
            Bad(321_600, 0, date + 100_000_000L, 16_000, 1),
            Bad(321_600, date + 100_000_000L, date, 16_000, 1),
            Bad(321_600, date + 100_000_000L, date + 600_000_001L, 16_000, 2),
            Bad(321_600, date + 100_000_000L, date + 100_000_000L, 15_991, 2),
            Bad(319_999, date + 100_000_000L, date + 100_000_000L, 16_000, 4),
            Bad(320_001, date - 1, date, 16_000, 4),
            Bad(320_000, date + 100_000_000L, date + 100_000_000L, 16_000, 4),
            Bad(320_001, date, date, 16_000, 4)
        )) {
            val established = calibrated()
            established.observe(bad.frame, bad.timestamp, bad.now, bad.rate, 0)
            val rejected = established.choose(bad.now, 48_000, 1_000_000_000, true)
            assertTrue(rejected.calibrated)
            assertFalse("invalid hardware must be unhealthy: $bad => $rejected", rejected.healthy)
            assertEquals(bad.mask, rejected.rejectionMask)
            assertEquals(0L, rejected.sinkPpb)
            established.observe(640_000, date + 20_000_000_000L, date + 20_000_000_000L, 16_000, 0)
            assertFalse(established.choose(date + 20_000_000_000L, 48_000, 1_000_000_000, true).healthy)
            established.observe(960_000, date + 40_000_000_000L, date + 40_000_000_000L, 16_000, 0)
            val recovered = established.choose(date + 40_000_000_000L, 48_000, 1_000_000_000, true)
            assertTrue(recovered.calibrated && recovered.healthy)
            assertEquals(0L, recovered.sinkPpb)
        }
    }
}

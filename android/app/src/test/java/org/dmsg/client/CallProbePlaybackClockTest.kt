package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class CallProbePlaybackClockTest {
    private fun calibrated(): CallProbePlaybackClock = CallProbePlaybackClock().also {
        it.observe(0, 1, 1, 16_000, 0)
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
}

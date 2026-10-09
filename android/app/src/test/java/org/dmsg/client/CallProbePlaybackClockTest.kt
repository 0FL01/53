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
}

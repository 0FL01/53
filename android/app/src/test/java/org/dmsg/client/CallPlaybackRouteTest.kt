package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class CallPlaybackRouteTest {
    @Test fun telecomRouteChangeRequiresNewHardwareCertification() {
        val clock = CallPlaybackClock()
        clock.observe(0, 1_000_000_000L, 1_000_000_000L, 16_000, 0)
        clock.observe(160_000, 11_000_000_000L, 11_000_000_000L, 16_000, 0)
        clock.observe(320_000, 21_000_000_000L, 21_000_000_000L, 16_000, 0)
        assertTrue(clock.choose(21_000_000_000L, 960_000, 20_000_000_000L, true).calibrated)
        clock.resetHardwareCalibration()
        clock.observe(320_160, 21_010_000_000L, 21_010_000_000L, 16_000, 0)
        val after = clock.choose(21_010_000_000L, 960_000, 20_000_000_000L, true)
        assertFalse(after.calibrated)
        assertFalse(after.healthy)
        assertNull("previous route ratio must not drive the new sink", after.sinkPpb)
        assertEquals(16_000, after.rate)
    }
}

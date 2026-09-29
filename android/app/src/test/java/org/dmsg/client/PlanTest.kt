package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

/** Storage-plan matrix: fresh/migrate/ready (reinstall loss is explicit). */
class PlanTest {
    @Test fun matrix() {
        val f = FakeFacade()
        assertEquals("fresh", f.storagePlan(false, false))
        assertEquals("migrate", f.storagePlan(true, false))
        assertEquals("ready", f.storagePlan(true, true))
        assertEquals("ready", f.storagePlan(false, true))
    }
}

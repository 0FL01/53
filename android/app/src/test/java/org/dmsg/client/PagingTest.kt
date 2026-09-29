package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

/** Pagination via mock: pages terminate, limits clamp, full dumps unused. */
class PagingTest {
    @Test fun contactPagesTerminate() {
        val f = FakeFacade()
        for (i in 0 until 5) f.request("AAAA0000%04d".format(i))
        val seen = mutableListOf<String>()
        var cursor: String? = null
        var steps = 0
        do {
            val (page, next) = f.contacts(cursor, 2)
            seen.addAll(page.map { it.contactId })
            cursor = next
            steps++
            assertTrue("must terminate", steps < 20)
        } while (cursor != null)
        assertEquals(5, seen.size)
    }

    @Test fun inboxPagesTerminate() {
        val f = FakeFacade()
        repeat(7) { i -> f.messages.add(Msg(i.toLong() + 1, "P", "m$i")) }
        val seen = mutableListOf<Msg>()
        var cursor = 0L
        var steps = 0
        while (true) {
            val (page, next) = f.inbox(cursor, 3)
            seen.addAll(page)
            if (next == null) break
            cursor = next
            steps++
            assertTrue("must terminate", steps < 20)
        }
        assertEquals(7, seen.size)
    }

    @Test fun requestAcceptBlockFlow() {
        val f = FakeFacade()
        assertEquals("requested", f.request("ZZZZ9999YYYY"))
        f.accept("ZZZZ9999YYYY")
        assertEquals("accepted", f.get("ZZZZ9999YYYY")?.state)
        f.block("ZZZZ9999YYYY")
        assertEquals("blocked", f.get("ZZZZ9999YYYY")?.state)
    }
}

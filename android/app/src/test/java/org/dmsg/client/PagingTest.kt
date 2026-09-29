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

    @Test fun chatPagingScrollsPast500WithPeerFilteredPages() {
        val f = FakeFacade()
        repeat(601) { i -> f.messages.add(Msg(i.toLong() + 1, if (i < 50) "OTHER" else "P", "m$i")) }
        val paging = InboxPaging()
        val seen = mutableListOf<Msg>()
        val cursors = mutableListOf<Long>()
        paging.reset()
        while (true) {
            val request = paging.begin() ?: break
            cursors.add(request.cursor)
            assertNull("concurrent scroll must not start another page", paging.begin())
            val (page, next) = f.inbox(request.cursor, 50)
            assertTrue(page.size <= 50)
            seen.addAll(page.filter { it.contactId == "P" })
            assertTrue(paging.complete(request, next))
            assertTrue("must terminate", cursors.size < 20)
        }
        assertEquals(551, seen.size)
        assertEquals((51L..601L).toList(), seen.map { it.seq })
        assertEquals(cursors.size, cursors.distinct().size)
        assertNull("end must not restart the first page", paging.begin())
    }

    @Test fun chatPagingStopsOnTerminalPageWithoutRepeatingIt() {
        val paging = InboxPaging()
        assertNull(paging.begin())
        paging.reset()
        val first = paging.begin()!!
        assertEquals(0L, first.cursor)
        assertTrue(paging.complete(first, 50L))
        val last = paging.begin()!!
        assertEquals(50L, last.cursor)
        assertFalse("duplicate callback must not finish a later request", paging.complete(first, 50L))
        assertNull(paging.begin())
        assertTrue(paging.complete(last, null))
        assertNull(paging.begin())
        assertFalse(paging.complete(last, null))
    }

    @Test fun chatPagingIgnoresResultsFromReloadAndPausedActivity() {
        val paging = InboxPaging()
        paging.reset()
        val old = paging.begin()!!
        paging.reset()
        val current = paging.begin()!!
        assertFalse(paging.complete(old, 50L))
        assertNull("stale result must not release the loading guard", paging.begin())
        assertTrue(paging.complete(current, 50L))
        val paused = paging.begin()!!
        paging.stop()
        assertFalse(paging.complete(paused, 100L))
        assertNull(paging.begin())
        paging.reset()
        assertEquals(0L, paging.begin()!!.cursor)
        assertFalse(paging.complete(paused, 100L))
        assertNull(paging.begin())
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

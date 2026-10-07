package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.DeliveryState

class PagingTest {
    private fun row(id: Long, peer: String = "P", direction: MessageDirection = MessageDirection.INCOMING) =
        HistoryMessage(id, id.toString(16).padStart(32, '0'), peer, direction, "fixture", id, if (direction == MessageDirection.OUTGOING) DeliveryState.QUEUED else null, null, null)

    @Test fun boundedPerContactPagesPrependChronologicallyPast500() {
        val f = FakeFacade()
        repeat(601) { i -> f.history.add(row(i + 1L, if (i < 50) "OTHER" else "P")) }
        val history = HistoryWindow()
        history.latest(f.historyPage("P", null, 50))
        var steps = 1
        while (history.nextBefore != null) {
            val page = f.historyPage("P", history.nextBefore, 50)
            assertTrue(page.rows.size <= 50)
            history.older(page)
            assertTrue(++steps < 20)
        }
        assertEquals((51L..601L).toList(), history.rows.map { it.localId })
        assertEquals(551, history.rows.map { it.localId }.distinct().size)
        assertEquals(0L, f.readCursors["P"] ?: 0L)
    }

    @Test fun insertAboveAnchorDoesNotDuplicateOrLoseOldRows() {
        val f = FakeFacade()
        (1L..5L).forEach { f.history.add(row(it)) }
        val history = HistoryWindow()
        history.latest(f.historyPage("P", null, 2))
        f.history.add(row(6))
        history.latest(f.historyPage("P", null, 2))
        assertEquals(4L, history.nextBefore)
        while (history.nextBefore != null) history.older(f.historyPage("P", history.nextBefore, 2))
        assertEquals((1L..6L).toList(), history.rows.map { it.localId })
    }

    @Test fun receivedBurstLargerThanOnePageIsBridgedWithoutLosingOlderCursor() {
        val f = FakeFacade()
        (1L..100L).forEach { f.history.add(row(it)) }
        val history = HistoryWindow()
        history.latest(f.historyPage("P", null, 50))
        (101L..301L).forEach { f.history.add(row(it)) }
        history.latest(f.historyPage("P", null, 50))
        assertNotNull(history.gapBefore)
        while (history.gapBefore != null) history.latest(f.historyPage("P", history.gapBefore, 50))
        assertEquals(51L, history.nextBefore)
        assertEquals((51L..301L).toList(), history.rows.map { it.localId })
        history.older(f.historyPage("P", history.nextBefore, 50))
        assertEquals((1L..301L).toList(), history.rows.map { it.localId })
    }

    @Test fun burstIsBridgedEvenWhenThePreviousWholeHistoryFitOnePage() {
        val f = FakeFacade()
        (1L..10L).forEach { f.history.add(row(it)) }
        val window = HistoryWindow()
        window.latest(f.timelinePage("P", null, 50))
        assertNull(window.nextBefore)
        (11L..201L).forEach { f.history.add(row(it)) }
        window.latest(f.timelinePage("P", null, 50))
        assertNotNull(window.gapBefore)
        while (window.gapBefore != null) window.latest(f.timelinePage("P", window.gapBefore, 50))
        assertEquals((1L..201L).toList(), window.rows.map { it.localId })
        assertNull(window.nextBefore)
    }

    @Test fun onlyViewedRowsAdvanceLocalReadCursor() {
        val f = FakeFacade()
        f.dialogs.add(Dialog("P", "accepted", hasKeys = true))
        (1L..6L).forEach { f.history.add(row(it)) }
        val history = HistoryWindow()
        history.latest(f.historyPage("P", null, 100))
        assertEquals(6uL, f.dialogsPage(null, 10).rows.single().localUnread)
        assertNull(history.viewedAnchor(emptyList()))
        assertNull(history.viewedAnchor(listOf(7, 8)))
        assertEquals(3L, f.markRead("P", history.viewedAnchor(listOf(2, 3))!!))
        assertEquals(3uL, f.dialogsPage(null, 10).rows.single().localUnread)
        assertEquals(3L, f.markRead("P", 1))
        try { f.markRead("OTHER", 3); fail() } catch (e: DmsgError) { assertEquals(ErrorKind.InvalidInput, e.kind) }
    }

    @Test fun dialogSummaryPagingPreservesMetadataAndAliasDoesNotPromote() {
        val f = FakeFacade()
        (1L..5L).forEach { f.dialogs.add(Dialog("P$it", "requested")); f.history.add(row(it, "P$it")) }
        f.setContactAlias("P1", "Local")
        var cursor: String? = null
        val ids = mutableListOf<String>()
        do {
            val page = f.dialogsPage(cursor, 2)
            ids.addAll(page.rows.map { it.contactId }); cursor = page.nextCursor
        } while (cursor != null)
        assertEquals(listOf("P5", "P4", "P3", "P2", "P1"), ids)
        assertEquals("Local", f.summary("P1")?.localAlias)
        assertEquals(false, f.summary("P1")?.hasKeys)
    }

    @Test fun exactStatusesDoNotInterpretAbsentOutboxAsDelivered() {
        val f = FakeFacade()
        val mid = f.send("P", "fixture")
        assertTrue(f.outbox(0, 50).first.isEmpty())
        assertEquals(DeliveryState.QUEUED, f.messageStatus(mid))
        f.history.single().deliveryState = DeliveryState.ACCEPTED
        assertEquals(R.string.delivery_accepted, deliveryLabelRes(f.messageStatus(mid)))
        f.history.single().deliveryState = DeliveryState.DELIVERED
        assertEquals(R.string.delivery_delivered, deliveryLabelRes(f.messageStatus(mid)))
        assertNull(f.messageStatus("f".repeat(32)))
        assertEquals(R.string.delivery_unknown, deliveryLabelRes(null))
        f.history.add(row(2))
        assertNull(f.messageStatus(row(2).messageIdHex))
    }

    @Test fun requestAcceptBlockFlow() {
        val f = FakeFacade()
        f.request("P"); f.accept("P"); assertEquals("accepted", f.get("P")?.state)
        f.block("P"); assertEquals("blocked", f.get("P")?.state)
    }

    @Test fun serverPagingAndLateInsertionBelowTheNewestPagePreserveAll601Rows() {
        val f = FakeFacade()
        (1L..601L).filter { it != 502L }.forEach { id ->
            f.history.add(row(id).copy(serverSeq = 602L - id, serverTimestampMs = 1_000L))
        }
        val window = HistoryWindow()
        window.latest(f.timelinePage("P", null, 50), localHead = 601L)
        val boundary = window.nextBefore
        val late = row(602L).copy(serverSeq = 100L, serverTimestampMs = 1_000L)
        f.history.add(late)
        window.latest(f.timelinePage("P", null, 50), listOf(late), 602L)
        assertEquals(boundary, window.nextBefore)
        while (window.gapBefore != null) window.latest(f.timelinePage("P", window.gapBefore, 50))
        while (window.nextBefore != null) window.older(f.timelinePage("P", window.nextBefore, 50))
        assertEquals((1L..601L).toList(), window.rows.map { it.serverSeq })
        assertEquals(601, window.rows.map { it.localId }.distinct().size)
        assertEquals(602L, window.latestLocalId)
        assertEquals(601L, f.historyPage("P", 602L, 1).rows.single().localId)
        assertEquals(0L, f.readCursors["P"] ?: 0L)
    }

    @Test fun retainedPendingRowMovesBelow50ConfirmedRowsWithoutChangingItsLocalId() {
        val f = FakeFacade()
        (1L..600L).forEach { id -> f.history.add(row(id).copy(serverSeq = 1000L + id, serverTimestampMs = 2_000L)) }
        val pending = row(700L, direction = MessageDirection.OUTGOING)
        f.history.add(pending)
        val window = HistoryWindow()
        window.latest(f.timelinePage("P", null, 50), localHead = 700L)
        assertEquals(700L, window.rows.last().localId)
        val accepted = pending.copy(deliveryState = DeliveryState.ACCEPTED, serverSeq = 500L, serverTimestampMs = 1_000L)
        f.history[f.history.lastIndex] = accepted
        window.latest(f.timelinePage("P", null, 50), window.rows.map { f.historyMessage("P", it.localId) }, 700L)
        assertEquals(700L, window.rows.first().localId)
        while (window.gapBefore != null) window.latest(f.timelinePage("P", window.gapBefore, 50))
        while (window.nextBefore != null) window.older(f.timelinePage("P", window.nextBefore, 50))
        assertEquals(601, window.rows.size)
        assertEquals(601, window.rows.map { it.localId }.distinct().size)
        assertEquals(500L, window.rows.first().serverSeq)
        assertEquals(700L, window.viewedAnchor(listOf(700L)))
        assertEquals(700L, f.historyPage("P", null, 1).rows.single().localId)
    }

    @Test fun bridgeDoesNotAdvanceIngestionPastAnUnseenLateReceive() {
        val f = FakeFacade()
        (1L..100L).forEach { f.history.add(row(it).copy(serverSeq = it * 10, serverTimestampMs = 1_000L)) }
        val window = HistoryWindow()
        window.latest(f.timelinePage("P", null, 50), localHead = 100L)
        (101L..201L).forEach { f.history.add(row(it).copy(serverSeq = it * 10, serverTimestampMs = 1_000L)) }
        window.latest(f.timelinePage("P", null, 50), localHead = 201L)
        // A receive between pages: one new row is in the bridge, another is far below it.
        f.history.add(row(202L).copy(serverSeq = 5L, serverTimestampMs = 1_000L))
        f.history.add(row(203L).copy(serverSeq = 1_305L, serverTimestampMs = 1_000L))
        while (window.gapBefore != null) window.latest(f.timelinePage("P", window.gapBefore, 50))
        assertTrue(window.rows.any { it.localId == 203L })
        assertTrue(window.rows.none { it.localId == 202L })
        assertEquals(201L, window.latestLocalId)
        val appended = f.historyPage("P", null, 50).rows.filter { it.localId > window.latestLocalId!! }
        window.latest(f.timelinePage("P", null, 50), appended, 203L)
        assertEquals(1, window.rows.count { it.localId == 202L })
        assertEquals(203L, window.latestLocalId)
    }
}

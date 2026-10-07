package org.dmsg.client

import android.content.Intent
import android.database.sqlite.SQLiteDatabase
import android.view.View
import android.view.ViewGroup
import android.widget.ListView
import android.widget.TextView
import androidx.test.core.app.ActivityScenario
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import uniffi.dmsg_core.HistoryMessage

/** Run after the exact two-phone OneQrContactGatesTest fixture sequence. */
class ChronologyGatesTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val app get() = instrumentation.targetContext.also { check(it.packageName == "org.dmsg.client.gate") }
    private val peer get() = JSONObject(File(app.filesDir, "one-qr-sent.json").readText()).getString("peer")
    private fun rows(f: DmsgFacade) = f.timelinePage(peer, null, 100).rows.asReversed()
    private fun corruptOnlyFixtureLocalClock(value: Long) {
        synchronized(Core.storeLock) {
            SQLiteDatabase.openDatabase(File(app.filesDir, "core.db").path, null, SQLiteDatabase.OPEN_READWRITE).use {
                it.execSQL("UPDATE core_history SET local_timestamp_ms=?", arrayOf(value))
            }
        }
    }
    private fun assertTimeline(f: DmsgFacade) {
        val ordered = rows(f)
        assertEquals(listOf("One QR first text before consent", "One QR reply without reverse scan",
            "Chronology earlier server acceptance", "Chronology later server acceptance"), ordered.map { it.text })
        assertEquals(4, ordered.map { it.localId }.distinct().size)
        ordered.forEach { assertEquals(it, f.historyMessage(peer, it.localId)) }
        assertTrue(ordered.all { it.serverSeq != null && it.serverTimestampMs != null })
        assertEquals(ordered.map { it.serverSeq }.sortedBy { it }, ordered.map { it.serverSeq })
        val json = JSONArray()
        ordered.forEach { json.put(JSONObject().put("mid", it.messageIdHex).put("seq", it.serverSeq).put("time", it.serverTimestampMs)) }
        File(app.filesDir, "chronology-proof.json").apply {
            writeText(json.toString()); setReadable(false, false); setWritable(false, false)
            setReadable(true, true); setWritable(true, true)
        }
        val intent = Intent(app, ChatActivity::class.java).putExtra("peer", peer)
        ActivityScenario.launch<ChatActivity>(intent).use { scenario ->
            fun assertUi() {
                val end = System.currentTimeMillis() + 15_000
                var ready = false
                while (!ready && System.currentTimeMillis() < end) {
                    scenario.onActivity { ready = it.findViewById<ListView>(R.id.messages).adapter?.count == 4 }
                    if (!ready) Thread.sleep(100)
                }
                assertTrue("timeline populated", ready)
                scenario.onActivity { activity ->
                    val list = activity.findViewById<ListView>(R.id.messages)
                    val adapter = list.adapter
                    assertTrue(adapter.hasStableIds())
                    (0 until 4).forEach { i ->
                        val row = adapter.getItem(i) as HistoryMessage
                        assertEquals(ordered[i].messageIdHex, row.messageIdHex)
                        assertEquals(ordered[i].localId, adapter.getItemId(i))
                        fun labels(v: View): List<String> = if (v is ViewGroup) (0 until v.childCount).flatMap { labels(v.getChildAt(it)) }
                            else if (v is TextView) listOf(v.text.toString()) else emptyList()
                        val time = activity.getString(R.string.server_timestamp, localTime(row.serverTimestampMs))
                        assertTrue(labels(adapter.getView(i, null, list)).any { time in it })
                    }
                }
            }
            assertUi(); scenario.recreate(); assertUi()
        }
    }
    @Test fun receiverSendsEarlierBeforeSenderFetch() {
        val f = Core.facade(app)
        try {
            assertEquals(2, rows(f).size)
            f.reconnect(); f.send(peer, "Chronology earlier server acceptance")
            assertEquals(3, rows(f).size)
        } finally { f.dnsStop() }
    }
    @Test fun senderSendsLaterThenFetchesEarlierMessageAndReopensUi() {
        val f = Core.facade(app)
        try {
            assertEquals(2, rows(f).size)
            f.reconnect(); f.send(peer, "Chronology later server acceptance")
            assertEquals(3, rows(f).size) // earlier peer event is still absent locally
            corruptOnlyFixtureLocalClock(9_000_000_000_000L)
            val receive = f.fetch(); assertEquals(1, receive.received.size); assertTrue(receive.skipped.all { it == 0L })
            // The local ingest order is deliberately the OPPOSITE of confirmed order.
            assertEquals("Chronology earlier server acceptance", f.historyPage(peer, null, 1).rows.single().text)
            assertTimeline(f)
        } finally { f.dnsStop() }
    }
    @Test fun receiverFetchesLaterTextAndRetainsOrderThroughRetryAndReopen() {
        val f = Core.facade(app)
        try {
            f.reconnect(); val receive = f.fetch(); assertEquals(1, receive.received.size); assertTrue(receive.skipped.all { it == 0L })
            corruptOnlyFixtureLocalClock(1L)
            val before = rows(f).map { Triple(it.messageIdHex, it.serverSeq, it.serverTimestampMs) }
            f.retry(); assertTrue(f.fetch().received.isEmpty())
            assertEquals(before, rows(f).map { Triple(it.messageIdHex, it.serverSeq, it.serverTimestampMs) })
            assertTimeline(f)
        } finally { f.dnsStop() }
    }

    @Test fun reopenConfirmedTimelineOnFinalApk() {
        assertTimeline(Core.facade(app))
    }

    /** Fresh disposable package, no server account, radio changes or live data. */
    @Test fun exactRowAndRetainedRefreshOnlyInFreshGatePackage() {
        val context = app
        assertFalse(DmsgService.running(context))
        val file = File(context.filesDir, "core.db")
        assertFalse("fresh disposable fixture required", file.exists())
        uniffi.dmsg_core.DmsgClient.open(file.absolutePath).use { it.accountInfo() }
        val first = "PEER1234ABCD"
        val other = "OTHER1234567"
        SQLiteDatabase.openDatabase(file.path, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
            db.beginTransaction()
            try {
                db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(ByteArray(32) { 7 }))
                db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, "7K3MP9TX4V2N"))
                for (contact in listOf(first, other)) {
                    db.execSQL("INSERT INTO core_contacts(contact_id,state) VALUES(?,'requested')", arrayOf(contact))
                }
                for (id in 1..601) {
                    val mid = ByteArray(16)
                    mid[0] = (id shr 8).toByte(); mid[1] = id.toByte()
                    db.execSQL("INSERT INTO core_history(message_id,contact_id,direction,sender_device,text,local_timestamp_ms,server_seq,server_timestamp_ms,order_checked) VALUES(?,?,'incoming',?,?,?,?,?,1)",
                        arrayOf(mid, first, ByteArray(32) { 2 }, "Chronology fixture $id", id.toLong(), 602L-id, 1000L))
                }
                db.execSQL("INSERT INTO core_history(message_id,contact_id,direction,text,local_timestamp_ms,delivery_state) VALUES(?,?,'outgoing',?,2,'queued')",
                    arrayOf(ByteArray(16) { 99 }, other, "Chronology retained fixture"))
                db.setTransactionSuccessful()
            } finally { db.endTransaction() }
        }
        val f = Core.facade(context) // current-schema fixture is sealed by the real store
        assertTrue(f.account().authenticated)
        assertEquals(601L, f.historyMessage(first, 601L).localId)
        assertEquals(1L, f.historyMessage(first, 601L).serverSeq)
        assertEquals(602L, f.historyMessage(other, 602L).localId)
        for (invalid in listOf(0L, -1L, 602L, Long.MAX_VALUE)) {
            try { f.historyMessage(first, invalid); fail("invalid/foreign row accepted") }
            catch (e: DmsgError) { assertEquals(ErrorKind.InvalidInput, e.kind) }
        }
        val all = mutableListOf<HistoryMessage>()
        var anchor: Long? = null
        do {
            val page = f.timelinePage(first, anchor, 50)
            all.addAll(page.rows); anchor = page.nextBeforeLocalId
        } while (anchor != null)
        assertEquals((1L..601L).toList(), all.asReversed().map { it.serverSeq })
        all.forEach { assertEquals(it, f.historyMessage(first, it.localId)) }
        ActivityScenario.launch<ChatActivity>(Intent(context, ChatActivity::class.java).putExtra("peer", other)).use { scenario ->
            fun awaitRow(state: uniffi.dmsg_core.DeliveryState, seq: Long?) {
                val end = System.nanoTime() + 20_000_000_000L
                while (System.nanoTime() < end) {
                    var ready = false
                    scenario.onActivity {
                        val adapter = it.findViewById<ListView>(R.id.messages).adapter
                        val row = if (adapter.count == 1) adapter.getItem(0) as HistoryMessage else null
                        ready = row?.deliveryState == state && row.serverSeq == seq && adapter.getItemId(0) == 602L
                    }
                    if (ready) return
                    Thread.sleep(100)
                }
                fail("retained last row did not refresh")
            }
            awaitRow(uniffi.dmsg_core.DeliveryState.QUEUED, null)
            synchronized(Core.storeLock) {
                SQLiteDatabase.openDatabase(file.path, null, SQLiteDatabase.OPEN_READWRITE).use {
                    it.execSQL("UPDATE core_history SET delivery_state='delivered',server_seq=1000,server_timestamp_ms=1000000,order_checked=1 WHERE local_id=602")
                }
            }
            awaitRow(uniffi.dmsg_core.DeliveryState.DELIVERED, 1000L)
            assertEquals(602L, f.historyMessage(other, 602L).localId)
            scenario.recreate()
            awaitRow(uniffi.dmsg_core.DeliveryState.DELIVERED, 1000L)
        }
    }
}

package org.dmsg.client

import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import androidx.test.core.app.ActivityScenario
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.security.MessageDigest
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.DeliveryState

/** Exact methods, two disposable .gate phones, packaged profile, recursive DNS.
 * Only A imports B's public QR; credentials are private file-only fixtures.
 */
class OneQrContactGatesTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private fun app(): Context = instrumentation.targetContext.also {
        check(it.packageName == "org.dmsg.client.gate")
        assertTrue(Core.facade(it).isReady())
    }
    private fun write(app: Context, name: String, value: String) {
        File(app.filesDir, name).apply {
            writeText(value); setReadable(false, false); setWritable(false, false)
            setReadable(true, true); setWritable(true, true)
        }
    }
    private fun field(a: Any, name: String): Any? = a.javaClass.getDeclaredField(name).also { it.isAccessible = true }.get(a)
    private fun await(label: String, condition: () -> Boolean) {
        val end = System.currentTimeMillis() + 90_000
        while (System.currentTimeMillis() < end) { if (condition()) return; Thread.sleep(150) }
        fail(label)
    }
    private fun hash(app: Context, mid: String): String {
        SQLiteDatabase.openDatabase(File(app.filesDir, "core.db").path, null, SQLiteDatabase.OPEN_READONLY).use { db ->
            db.rawQuery("SELECT ciphertext FROM core_outbox WHERE lower(hex(message_id))=?", arrayOf(mid)).use {
                assertTrue(it.moveToFirst())
                return MessageDigest.getInstance("SHA-256").digest(it.getBlob(0)).joinToString("") { b -> "%02x".format(b.toInt() and 255) }
            }
        }
    }
    private fun pending(f: DmsgFacade) = f.contacts(null, 100).first.single { it.state == "incoming" }

    @Test fun bootstrapDisposableAccountThroughDns() {
        val app = app(); val f = Core.facade(app)
        val file = File(app.filesDir, "one-qr-auth.json")
        try {
            assertFalse(f.account().authenticated)
            val fixture = JSONObject(file.readText())
            assertEquals(setOf("login", "password", "invitation"), fixture.keys().asSequence().toSet())
            assertTrue(TrustedServerProfile.configureIfFresh(f, { DnsNetwork.resolvers(app) }, { app.assets.open(TrustedServerProfile.ASSET) }))
            f.signupDns(fixture.getString("login"), fixture.getString("password"), fixture.getString("invitation"))
            f.reconnect()
            assertEquals("ready", f.dnsStatus())
            assertEquals(0, f.contacts(null, 100).first.size)
            write(app, "one-qr-public.txt", f.myQr())
        } finally { SecureStore.wipe(file); f.dnsStop() }
    }

    @Test fun senderOneQrPreviewAddAndFirstText() {
        val app = app(); val f = Core.facade(app)
        val file = File(app.filesDir, "one-qr-peer.txt")
        val qr = file.readText(); val id = f.contactQrId(qr)
        assertNull(f.get(id))
        val chatMonitor = instrumentation.addMonitor(ChatActivity::class.java.name, null, false)
        try {
            ActivityScenario.launch(ScannerActivity::class.java).use { scenario ->
                fun scan() {
                    scenario.onActivity { ScannerActivity::class.java.getDeclaredMethod("onText", String::class.java).also { m -> m.isAccessible = true }.invoke(it, qr) }
                    await("QR preview") { var shown = false; scenario.onActivity { shown = (field(it, "prompt") as? AlertDialog)?.isShowing == true }; shown }
                }
                scan()
                assertNull("preview must not create/approve contact", f.get(id))
                scenario.onActivity { (field(it, "prompt") as AlertDialog).getButton(AlertDialog.BUTTON_NEGATIVE).performClick() }
                assertNull("cancel must not mutate contact", f.get(id))
                scan()
                scenario.onActivity { (field(it, "prompt") as AlertDialog).getButton(AlertDialog.BUTTON_POSITIVE).performClick() }
                val chat = instrumentation.waitForMonitorWithTimeout(chatMonitor, 90_000) as? ChatActivity
                assertNotNull("one Add opens chat without a second Accept", chat)
                assertEquals(ContactCta.Chat, contactCta(f.get(id)))
                await("chat send enabled") { var enabled = false; instrumentation.runOnMainSync { enabled = chat!!.findViewById<Button>(R.id.btn_send).isEnabled }; enabled }
                instrumentation.runOnMainSync {
                    chat!!.findViewById<EditText>(R.id.composer).setText("One QR first text before consent")
                    chat.findViewById<Button>(R.id.btn_send).performClick()
                }
                await("first text stored/accepted") { f.historyPage(id, null, 100).rows.any { it.direction == MessageDirection.OUTGOING && it.deliveryState == DeliveryState.ACCEPTED } }
                val row = f.historyPage(id, null, 100).rows.single()
                write(app, "one-qr-sent.json", JSONObject().put("peer", id).put("mid", row.messageIdHex).put("cipherHash", hash(app, row.messageIdHex)).toString())
                instrumentation.runOnMainSync { chat!!.finish() }
            }
        } finally { instrumentation.removeMonitor(chatMonitor); SecureStore.wipe(file); f.dnsStop() }
    }

    @Test fun receiverRequestAndFirstTextRemainPendingAcrossProcess() {
        val app = app(); val f = Core.facade(app)
        try {
            f.reconnect(); val r = f.fetch()
            assertTrue(r.received.isEmpty()); assertEquals(1L, r.skipped[0]); assertEquals(0L, r.cursor)
            val c = pending(f)
            assertEquals(ContactCta.Accept, contactCta(c)); assertEquals(R.string.trust_incoming, trustLabelRes(c))
            assertTrue("no history before explicit consent", f.historyPage(c.contactId, null, 100).rows.isEmpty())
            write(app, "one-qr-incoming.json", JSONObject().put("peer", c.contactId).toString())
        } finally { f.dnsStop() }
    }

    @Test fun receiverAcceptFromDialogsAndReplyWithoutReverseQr() {
        val app = app(); val f = Core.facade(app)
        val id = JSONObject(File(app.filesDir, "one-qr-incoming.json").readText()).getString("peer")
        assertEquals("incoming", f.get(id)!!.state)
        assertTrue(f.historyPage(id, null, 100).rows.isEmpty())
        val profileMonitor = instrumentation.addMonitor(ProfileActivity::class.java.name, null, false)
        val chatMonitor = instrumentation.addMonitor(ChatActivity::class.java.name, null, false)
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                await("incoming request in dialogs") { var ready = false; scenario.onActivity { ready = field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false }; ready }
                scenario.onActivity { MainActivity::class.java.getDeclaredMethod("openChat", String::class.java).also { m -> m.isAccessible = true }.invoke(it, id) }
                val profile = instrumentation.waitForMonitorWithTimeout(profileMonitor, 20_000) as? ProfileActivity
                assertNotNull("incoming dialog opens Accept card directly", profile)
                await("visible Accept and server-key provenance") { var ready = false; instrumentation.runOnMainSync { ready = profile!!.findViewById<Button>(R.id.btn_accept).visibility == View.VISIBLE && profile.findViewById<TextView>(R.id.info).text.toString() == profile.getString(R.string.trust_incoming) }; ready }
                instrumentation.runOnMainSync { profile!!.findViewById<Button>(R.id.btn_accept).performClick() }
                val chat = instrumentation.waitForMonitorWithTimeout(chatMonitor, 90_000) as? ChatActivity
                assertNotNull("accept opens chat and fetches deferred message", chat)
                val incoming = f.historyPage(id, null, 100).rows.single()
                assertEquals("One QR first text before consent", incoming.text)
                assertEquals(MessageDirection.INCOMING, incoming.direction)
                assertEquals("accepted_server", f.get(id)!!.state)
                assertEquals(R.string.trust_server_keys, trustLabelRes(f.get(id)))
                assertTrue(f.fetch().received.isEmpty())
                await("reverse send enabled") { var enabled = false; instrumentation.runOnMainSync { enabled = chat!!.findViewById<Button>(R.id.btn_send).isEnabled }; enabled }
                instrumentation.runOnMainSync {
                    chat!!.findViewById<EditText>(R.id.composer).setText("One QR reply without reverse scan")
                    chat.findViewById<Button>(R.id.btn_send).performClick()
                }
                await("reverse text accepted") { f.historyPage(id, null, 100).rows.any { it.direction == MessageDirection.OUTGOING && it.deliveryState == DeliveryState.ACCEPTED } }
                val sent = f.historyPage(id, null, 100).rows.single { it.direction == MessageDirection.OUTGOING }
                write(app, "one-qr-sent.json", JSONObject().put("peer", id).put("mid", sent.messageIdHex).put("cipherHash", hash(app, sent.messageIdHex)).toString())
                instrumentation.runOnMainSync { chat!!.finish() }
            }
        } finally { instrumentation.removeMonitor(profileMonitor); instrumentation.removeMonitor(chatMonitor); f.dnsStop() }
    }

    @Test fun senderReceivesReplyAndOriginalCiphertextBecomesDelivered() {
        val app = app(); val f = Core.facade(app)
        val proof = JSONObject(File(app.filesDir, "one-qr-sent.json").readText())
        try {
            // A host timeout may disconnect instrumentation after the durable
            // receive. A restarted gate still requires exactly one reply,
            // never a second delivery and never an empty local history.
            val before = f.historyPage(proof.getString("peer"), null, 100).rows.filter { it.direction == MessageDirection.INCOMING }
            assertTrue(before.size <= 1)
            f.reconnect(); val r = f.fetch(); assertEquals(1 - before.size, r.received.size)
            val reply = f.historyPage(proof.getString("peer"), null, 100).rows.single { it.direction == MessageDirection.INCOMING }
            assertEquals("One QR reply without reverse scan", reply.text)
            assertEquals(2, f.historyPage(proof.getString("peer"), null, 100).rows.size)
            assertTrue(r.skipped.all { it == 0L }); f.retry()
            assertEquals(DeliveryState.DELIVERED, f.messageStatus(proof.getString("mid")))
            assertEquals(proof.getString("cipherHash"), hash(app, proof.getString("mid")))
            assertTrue(f.fetch().received.isEmpty())
        } finally { f.dnsStop() }
    }

    @Test fun receiverReplyDeliveredAndHistorySurvivesReopen() {
        val app = app(); val f = Core.facade(app)
        val proof = JSONObject(File(app.filesDir, "one-qr-sent.json").readText())
        try {
            f.reconnect(); f.retry()
            assertEquals(DeliveryState.DELIVERED, f.messageStatus(proof.getString("mid")))
            assertEquals(proof.getString("cipherHash"), hash(app, proof.getString("mid")))
            assertEquals(2, f.historyPage(proof.getString("peer"), null, 100).rows.size)
            assertTrue(f.fetch().received.isEmpty())
        } finally { f.dnsStop() }
    }
}

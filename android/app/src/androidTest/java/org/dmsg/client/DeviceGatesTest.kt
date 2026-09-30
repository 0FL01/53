package org.dmsg.client

import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.database.Cursor.FIELD_TYPE_BLOB
import android.database.sqlite.SQLiteDatabase
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.MessageDigest
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.Rule
import org.junit.rules.TestName
import org.junit.Assume.assumeTrue
import org.junit.runner.RunWith
import uniffi.dmsg_core.DmsgClient

/** Routine tests use throwaway DBs. Named one-off gates require private fixtures;
 * run manually with am instrument, never connected tests on the live package. */
@RunWith(AndroidJUnit4::class)
class DeviceGatesTest {
    @Test fun configureAndProbeDnsOnlyOnExistingAccount() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-dns-profile.qr")
        assumeTrue("explicit public server profile fixture required", input.exists())
        var facade: DmsgFacade? = null
        try {
            val f = Core.facade(app)
            facade = f
            val before = f.account()
            assertTrue("preserve the existing account", before.first)
            f.configureDns(input.readText().trim(), DnsNetwork.resolvers(app))
            DnsNetwork.mirrorProfile(app, f)
            assertNotNull(f.dnsProfile())
            assertTrue(f.reconnect("dns", Prefs.serverPub(app)!!, Prefs.domain(app)) > 0L)
            assertEquals("ready", f.dnsStatus())
            val first = f.fetch("dns", Prefs.serverPub(app)!!, Prefs.domain(app))
            val second = f.fetch("dns", Prefs.serverPub(app)!!, Prefs.domain(app))
            assertEquals(0, second.received.size)
            assertTrue(second.cursor >= first.cursor)
            assertEquals(before, f.account())
        } finally {
            try { facade?.stopDns() } finally { input.delete() }
        }
    }

    @Test fun startDnsForegroundForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        assertTrue(f.account().first)
        assertNotNull(f.dnsProfile())
        val before = f.account()
        InstrumentationRegistry.getInstrumentation().startActivitySync(
            Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        )
        DmsgService.start(app)
        val deadline = System.nanoTime() + 45_000_000_000L
        while (f.dnsStatus() != "ready" && System.nanoTime() < deadline) Thread.sleep(250)
        assertEquals("ready", f.dnsStatus())
        assertTrue(DmsgService.running(app))
        assertEquals(before, f.account())
    }

    @Test fun rejectLiveCarrierPinAndNoiseKeyBeforeEnrolForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val inputs = listOf("gate-wrong-pin.qr", "gate-wrong-noise.qr").map { File(app.filesDir, it) }
        assumeTrue("explicit public negative profile fixtures required", inputs.all { it.exists() })
        val main = Core.facade(app)
        val before = main.account()
        try {
            for ((index, input) in inputs.withIndex()) {
                val dir = File(app.cacheDir, "dns-negative-${System.nanoTime()}")
                assertTrue(dir.mkdir())
                assertTrue(dir.setReadable(false, false)); assertTrue(dir.setWritable(false, false))
                assertTrue(dir.setExecutable(false, false))
                assertTrue(dir.setReadable(true, true)); assertTrue(dir.setWritable(true, true))
                assertTrue(dir.setExecutable(true, true))
                val temporary = object : ContextWrapper(app) { override fun getFilesDir(): File = dir }
                val key = SecureStore.key(temporary)
                val client = uniffi.dmsg_core.DmsgClient.openEncrypted(File(dir, "core.db").absolutePath, key)
                key.fill(0)
                try {
                    try {
                        client.enrolDns(input.readText().trim(), DnsNetwork.resolvers(app))
                        fail("untrusted carrier/server must not enrol")
                    } catch (e: uniffi.dmsg_core.FfiException) {
                        if (index == 0) assertTrue("carrier pin must have its own error", e is uniffi.dmsg_core.FfiException.PinMismatch)
                        else assertTrue("wrong Noise key must fail before bearer authentication", e is uniffi.dmsg_core.FfiException.Transport)
                    }
                    assertFalse(client.accountInfo().enrolled)
                    assertTrue(client.contactsPage(null, 100u).rows.isEmpty())
                } finally {
                    client.stopDns()
                    client.close()
                    dir.deleteRecursively()
                }
            }
            assertEquals(before, main.account())
        } finally { inputs.forEach { it.delete() } }
    }

    @Test fun restartDnsWithSameResolversPreservesIdentityForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val profile = f.dnsProfile()
        assumeTrue("configure a DNS profile explicitly first", profile != null)
        val before = f.account()
        assertTrue(before.first)
        val outbox = f.outbox(0, 100)
        val inbox = f.inbox(0, 100)
        try {
            f.reconnect(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app))
            assertEquals("ready", f.dnsStatus())
            f.dnsNetworkChanged(DnsNetwork.resolvers(app))
            assertEquals("stopped", f.dnsStatus())
            assertTrue(f.reconnect(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app)) > 0)
            assertEquals("ready", f.dnsStatus())
            assertEquals(before, f.account())
            assertEquals(profile!!.fingerprint, f.dnsProfile()!!.fingerprint)
            assertTrue(profile.pub.contentEquals(f.dnsProfile()!!.pub))
            assertEquals(outbox, f.outbox(0, 100))
            assertEquals(inbox, f.inbox(0, 100))
        } finally { f.stopDns() }
    }
    @get:Rule val testName = TestName()
    private lateinit var dir: File
    private lateinit var context: Context
    private val id = "ABCD1234EFGH"

    @Before fun fixture() {
        val app = ApplicationProvider.getApplicationContext<Context>()
        dir = File(app.cacheDir, "gate-${System.nanoTime()}")
        assertTrue(dir.mkdirs())
        context = object : ContextWrapper(app) {
            override fun getFilesDir(): File = dir
        }
    }

    @After fun cleanup() { dir.deleteRecursively() }

    /** Stateful operator gates are never run by a whole-class/routine suite. */
    private fun explicitGate() {
        val selected = InstrumentationRegistry.getArguments().getString("class").orEmpty().split(',')
        assumeTrue("select this gate method explicitly", "${javaClass.name}#${testName.methodName}" in selected)
    }

    private fun seedLegacyDb(name: String = "core.db", deviceByte: Byte = 7): File {
        val file = File(dir, name)
        DmsgClient.open(file.absolutePath).accountInfo() // create the legacy schema
        SQLiteDatabase.openDatabase(file.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
            db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(ByteArray(32) { deviceByte }))
            db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, id))
            db.execSQL("INSERT INTO core_inbox(sender_device,message_id,contact_id,text,seq) VALUES(?,?,?,?,1)",
                arrayOf(ByteArray(32) { 2 }, ByteArray(16) { 3 }, id, "fixture private text"))
        }
        return file
    }

    @Test fun migrateEncryptedReopenAndRestoreOnlyWithOriginalKey() {
        val db = seedLegacyDb()
        val f = Core.facade(context)
        assertEquals(Pair(true, id), f.account())
        assertEquals("fixture private text", f.inbox(0, 10).first.single().text)
        SQLiteDatabase.openDatabase(db.absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { sql ->
            sql.rawQuery("SELECT device_priv FROM core_identity", null).use {
                assertTrue(it.moveToFirst()); assertEquals(FIELD_TYPE_BLOB, it.getType(0))
                assertFalse(it.getBlob(0).contentEquals(ByteArray(32) { 7 }))
            }
            sql.rawQuery("SELECT text FROM core_inbox", null).use {
                assertTrue(it.moveToFirst()); assertEquals(FIELD_TYPE_BLOB, it.getType(0))
            }
        }
        SecureStore.seal(context)
        assertEquals("ready", SecureStore.plan(context))
        SecureStore.wipe(db) // only the test fixture; the live app's DB is never deleted
        SecureStore.unseal(context)
        assertEquals(Pair(true, id), Core.facade(context).account())
        assertEquals("fixture private text", Core.facade(context).inbox(0, 10).first.single().text)
        assertFailsWithMessage("live db already exists") { SecureStore.unseal(context) }
        // A wiped wrapping key cannot be silently recreated even with a sealed backup present.
        File(dir, "core.key.sealed").delete()
        assertFailsWithMessage("reinstall_loss") { Core.facade(context).account() }
    }

    @Test fun invalidQrIsRejectedWithoutAddingContact() {
        seedLegacyDb()
        val f = Core.facade(context)
        val before = f.contacts(null, 50).first.size
        for (qr in listOf("garbage", "dmsg://contact/", "dmsg://contact/broken", "dmsg://join/broken", "dmsg://join/" + "x".repeat(9000))) {
            assertFailsWithMessage("") { f.qrKind(qr) }
            assertFailsWithMessage("") { f.addQr(qr) }
        }
        assertEquals(before, f.contacts(null, 50).first.size)
    }

    @Test fun changedIdentityStopsSendUntilExplicitConfirm() {
        seedLegacyDb()
        val f = Core.facade(context)
        val otherDb = seedLegacyDb("other.db", 9)
        val key = ByteArray(32) { 4 } // test fixture only, never used for installed identity
        val other = UniFfiFacade(otherDb.absolutePath, key)
        val originalQr = f.myQr()
        val changedQr = other.myQr()
        assertEquals("added", f.addQr(originalQr))
        f.accept(id)
        assertEquals("identity_changed", f.addQr(changedQr))
        assertEquals(true, f.get(id)?.identityMismatch)
        assertFailsWithMessage("identity changed") {
            f.send("127.0.0.1:1", ByteArray(32), "test.invalid", id, "fixture")
        }
        assertEquals(0, f.outbox(0, 50).first.size)
        f.confirm(id)
        assertEquals(false, f.get(id)?.identityMismatch)
        assertFailsWithMessage("connect") {
            f.send("127.0.0.1:1", ByteArray(32), "test.invalid", id, "fixture")
        }
    }

    /** Dedicated .gate installation only; fixture is not the installed user's account. */
    @Test fun seedLargeChatOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        val dbFile = File(app.filesDir, "core.db")
        assertFalse("fixture package must be fresh", dbFile.exists())
        DmsgClient.open(dbFile.absolutePath).accountInfo()
        val peer = "PEER1234ABCD"
        SQLiteDatabase.openDatabase(dbFile.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
            db.beginTransaction()
            try {
                db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(ByteArray(32) { 7 }))
                db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, id))
                db.execSQL("INSERT INTO core_contacts(contact_id,state) VALUES(?,'requested')", arrayOf(peer))
                for (seq in 1..551) {
                    val mid = ByteArray(16)
                    mid[0] = (seq shr 8).toByte()
                    mid[1] = seq.toByte()
                    db.execSQL("INSERT INTO core_inbox(sender_device,message_id,contact_id,text,seq) VALUES(?,?,?,?,?)",
                        arrayOf(ByteArray(32) { 2 }, mid, peer, "fixture message $seq", seq))
                }
                db.setTransactionSuccessful()
            } finally { db.endTransaction() }
        }
        assertEquals(Pair(true, id), Core.facade(app).account())
        assertEquals(50, Core.facade(app).inbox(0, 50).first.size)
    }

    @Test fun sealSyntheticAccountBeforeResetOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        assertEquals(Pair(true, id), Core.facade(app).account())
        SecureStore.seal(app)
        assertTrue(SecureStore.sealedDb(app).exists())
    }

    @Test fun verifyFreshAfterClearDataOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        assertFalse(Core.dbFile(app).exists())
        assertFalse(SecureStore.sealedDb(app).exists())
        assertFalse(Core.facade(app).account().first)
        assertTrue(app.getString(R.string.reinstall_loss).contains("Keystore"))
    }

    @Test fun verifySealedSnapshotCannotRestoreAfterClearDataOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        assertTrue(SecureStore.sealedDb(app).exists())
        assertFalse(File(app.filesDir, "core.key.sealed").exists())
        val keystore = java.security.KeyStore.getInstance("AndroidKeyStore").also { it.load(null) }
        assertFalse("Clear data must delete the original device-bound master key",
            keystore.containsAlias("_androidx_security_master_key_"))
        assertEquals("reinstall_loss", SecureStore.plan(app))
        assertFailsWithMessage("reinstall_loss") { Core.facade(app).account() }
        assertFailsWithMessage("reinstall_loss") { SecureStore.unseal(app) }
        assertFalse("must not silently create a replacement identity", Core.dbFile(app).exists())
        assertFalse(File(app.filesDir, "core.key.sealed").exists())
    }

    @Test fun verifyLostKeyUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        assertTrue(SecureStore.sealedDb(app).exists())
        assertFalse(File(app.filesDir, "core.key.sealed").exists())
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val activity = instrumentation.startActivitySync(
            Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        try {
            val deadline = System.nanoTime() + 5_000_000_000L
            var label = ""
            while (!label.contains("reinstall_loss") && System.nanoTime() < deadline) {
                instrumentation.runOnMainSync {
                    label = activity.findViewById<android.widget.TextView>(R.id.status).text.toString()
                }
                Thread.sleep(50)
            }
            assertTrue("loss must be shown rather than an uncaught worker-thread crash", label.contains("reinstall_loss"))
        } finally { instrumentation.runOnMainSync { activity.finish() } }
    }

    /** Explicitly selected on-device gate, not part of the routine test suite. */
    @Test fun enrolPrivateInviteOnce() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val invite = File(app.filesDir, "gate-invite.qr")
        val transport = File(app.filesDir, "gate-transport.txt")
        assumeTrue("explicit private invite fixture is required", invite.exists() && transport.exists())
        try {
            val values = transport.readLines()
            assertEquals(3, values.size)
            val (addr, domain, pub) = values
            val f = Core.facade(app)
            assertFalse("never overwrite an existing account", f.account().first)
            val qr = invite.readText().trim()
            assertEquals(domain, f.preview(qr).first)
            val enrolled = f.enrol(qr, addr, null)
            assertEquals(enrolled, f.account().second)
            Prefs.setTransport(app, addr, domain, pub)
        } finally {
            SecureStore.wipe(invite)
            SecureStore.wipe(transport)
        }
    }

    /** Fresh registration is destructive only to the deliberately separate gate package. */
    @Test fun enrolPrivateDnsInviteOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assertTrue("fresh device registration requires the isolated gate package", app.packageName.endsWith(".gate"))
        val input = File(app.filesDir, "gate-invite.qr")
        assumeTrue("private operator invitation is required", input.exists())
        val f = Core.facade(app)
        assertFalse("refusing to replace an existing identity", f.account().first)
        try {
            val uri = input.readText().trim()
            val preview = f.preview(uri)
            val id = f.enrolDns(uri, DnsNetwork.resolvers(app))
            assertEquals(Pair(true, id), f.account())
            assertEquals(preview.first, f.dnsProfile()!!.domain)
            assertEquals(preview.second, f.dnsProfile()!!.fingerprint)
            DnsNetwork.mirrorProfile(app, f)
            f.stopDns()
            assertTrue(f.reconnect(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app)) > 0)
            val first = f.fetch(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app))
            val second = f.fetch(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app))
            assertTrue(first.received.isEmpty()); assertTrue(second.received.isEmpty())
            assertTrue(second.cursor >= first.cursor)
            assertEquals(Pair(true, id), Core.facade(app).account())
        } finally { f.stopDns(); input.delete() }
    }

    /** One-off paired-device gate: only the public contact QR leaves the app sandbox. */
    @Test fun exportMyContactQrForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val output = File(app.filesDir, "gate-my-contact.qr")
        assertFalse("refusing to overwrite contact fixture", output.exists())
        val f = Core.facade(app)
        assertTrue("phone must already be enrolled", f.account().first)
        assertTrue(output.createNewFile())
        output.writeText(f.myQr())
        assertTrue(output.setReadable(false, false))
        assertTrue(output.setWritable(false, false))
        assertTrue(output.setReadable(true, true))
        assertTrue(output.setWritable(true, true))
    }

    /** The peer QR is public, but use an app-private file rather than exported Activities. */
    @Test fun acceptPeerFromPrivateFile() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-peer-contact.qr")
        assumeTrue("explicit peer contact fixture is required", input.exists())
        try {
            val f = Core.facade(app)
            assertTrue("phone must already be enrolled", f.account().first)
            val before = f.contacts(null, 100).first.map { it.contactId }.toSet()
            assertEquals("added", f.addQr(input.readText().trim()))
            val after = f.contacts(null, 100).first.map { it.contactId }.toSet()
            val peer = (after - before).single()
            f.accept(peer)
            val selection = File(app.filesDir, "gate-peer-contact-id")
            selection.writeText(peer)
            assertTrue(selection.setReadable(false, false)); assertTrue(selection.setWritable(false, false))
            assertTrue(selection.setReadable(true, true)); assertTrue(selection.setWritable(true, true))
            f.reconnect(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app))
        } finally {
            input.delete()
        }
    }

    /** Record the encrypted inbox's current page count before a physical gate. */
    @Test fun recordInboxBaselineForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        assertTrue(f.account().first)
        val baseline = File(app.filesDir, "gate-inbox-baseline")
        assertFalse("finish previous gate first", baseline.exists())
        baseline.writeText(f.inbox(0, 100).first.size.toString())
        assertTrue(baseline.setReadable(false, false))
        assertTrue(baseline.setWritable(false, false))
        assertTrue(baseline.setReadable(true, true))
        assertTrue(baseline.setWritable(true, true))
    }

    /** A live FGS may already fetch; check durable cursor/dedup, not a single fetch response. */
    @Test fun verifyNextDeliveryAndDedupForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val baseline = File(app.filesDir, "gate-inbox-baseline")
        assumeTrue("record baseline first", baseline.exists())
        try {
            val f = Core.facade(app)
            val previous = baseline.readText().toInt()
            val addr = Prefs.addr(app)
            val pub = Prefs.serverPub(app)!!
            val domain = Prefs.domain(app)
            val first = f.fetch(addr, pub, domain)
            val page = f.inbox(0, 100).first
            assertEquals("exactly one additional delivered message", previous + 1, page.size)
            assertEquals(page.size, page.map { it.seq }.toSet().size)
            val second = f.fetch(addr, pub, domain)
            assertEquals(0, second.received.size)
            assertTrue(second.cursor >= first.cursor)
            assertEquals(page.size, f.inbox(0, 100).first.size)
        } finally {
            baseline.delete()
        }
    }

    /** Select manually only with a pre-provisioned paired contact. Establish an outgoing session. */
    private fun gatePeer(app: Context, f: DmsgFacade): String {
        val selection = File(app.filesDir, "gate-peer-contact-id")
        val peer = if (selection.exists()) selection.readText().trim()
            else f.contacts(null, 100).first.single().contactId
        assertNotNull("explicit paired peer must exist", f.get(peer))
        assertFalse("cannot send to a changed peer", f.get(peer)!!.identityMismatch)
        return peer
    }

    @Test fun sendSessionProbeForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(File(app.filesDir, "gate-offline-request").exists())
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        f.send(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app), peer,
            "Paired transport session probe.")
    }

    private fun ciphertextHash(app: Context, mid: String): String {
        SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { db ->
            db.rawQuery("SELECT ciphertext FROM core_outbox WHERE lower(hex(message_id))=?", arrayOf(mid)).use { c ->
                assertTrue("persisted ciphertext is required", c.moveToFirst())
                return MessageDigest.getInstance("SHA-256").digest(c.getBlob(0))
                    .joinToString("") { "%02x".format(it.toInt() and 255) }
            }
        }
    }

    /** Radio is disabled and adb reverse removed by the orchestrator before this gate. */
    @Test fun queueOfflineMessageForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(File(app.filesDir, "gate-offline-request").exists())
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        val addr = Prefs.addr(app)
        val pub = Prefs.serverPub(app)!!
        val domain = Prefs.domain(app)
        assertFailsWithMessage("connect") { f.fetch(addr, pub, domain) }
        assertFailsWithMessage("connect") { f.retry(addr, pub, domain) }
        val mid = f.send(addr, pub, domain, peer, "Paired offline persisted probe.")
        assertEquals("queued", f.outbox(0, 100).first.single { it.mid == mid }.status)
        val record = JSONObject()
            .put("mid", mid).put("ciphertextHash", ciphertextHash(app, mid))
            .put("account", f.account().second).put("inboxCount", f.inbox(0, 100).first.size)
        val output = File(app.filesDir, "gate-queued-record")
        assertFalse(output.exists())
        output.writeText(record.toString())
        assertTrue(output.setReadable(false, false)); assertTrue(output.setWritable(false, false))
        assertTrue(output.setReadable(true, true)); assertTrue(output.setWritable(true, true))
    }

    /** Run after actual process death and restored bridge; do not print ciphertext or its hash. */
    @Test fun retryAndVerifyPreservedCiphertextForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-queued-record")
        assumeTrue(input.exists())
        val record = JSONObject(input.readText())
        val f = Core.facade(app)
        val mid = record.getString("mid")
        assertEquals(record.getString("account"), f.account().second)
        assertEquals(record.getInt("inboxCount"), f.inbox(0, 100).first.size)
        assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
        assertEquals("queued", f.outbox(0, 100).first.single { it.mid == mid }.status)
        f.retry(Prefs.addr(app), Prefs.serverPub(app)!!, Prefs.domain(app))
        assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
        assertEquals("accepted", f.outbox(0, 100).first.single { it.mid == mid }.status)
        input.delete()
        File(app.filesDir, "gate-offline-request").delete()
    }

    /** Explicit fixture only. Shell orchestrator force-stops during this bounded hold. */
    @Test fun holdForegroundServiceForForceStopGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(File(app.filesDir, "gate-force-stop-request").exists())
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val activity = instrumentation.startActivitySync(
            Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        instrumentation.runOnMainSync { DmsgService.start(activity) }
        val deadline = System.nanoTime() + 10_000_000_000L
        while (!DmsgService.running(app) && System.nanoTime() < deadline) Thread.sleep(100)
        assertTrue("foreground worker must be running before force-stop", DmsgService.running(app))
        File(app.filesDir, "gate-fgs-ready").writeText("ready")
        try {
            Thread.sleep(30_000)
        } finally {
            DmsgService.stop(app)
            instrumentation.runOnMainSync { activity.finish() }
        }
    }

    /** Input-stage UI gate, not an optical camera claim. Never touches the live installation. */
    @Test fun scannerErrorsAndProfileConfirmUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        val f = Core.facade(app)
        assertEquals(Pair(true, id), f.account())
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        fun start(type: Class<out android.app.Activity>, peer: String? = null): android.app.Activity =
            instrumentation.startActivitySync(Intent(app, type)
                .putExtra("peer", peer).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        fun waitLabel(activity: android.app.Activity, viewId: Int, expected: String): String {
            val deadline = System.nanoTime() + 10_000_000_000L
            var label = ""
            while (System.nanoTime() < deadline) {
                instrumentation.runOnMainSync {
                    label = activity.findViewById<android.widget.TextView>(viewId).text.toString()
                }
                if (label.contains(expected)) return label
                Thread.sleep(50)
            }
            fail("UI did not show expected static status: $expected")
            return label
        }
        fun scan(uri: String, expected: String) {
            val activity = start(ScannerActivity::class.java)
            try {
                // Exercise the same private input callback used by ZXing, without an exported hook.
                val callback = ScannerActivity::class.java.getDeclaredMethod("onText", String::class.java)
                    .also { it.isAccessible = true }
                instrumentation.runOnMainSync { callback.invoke(activity, uri) }
                waitLabel(activity, R.id.result, expected)
            } finally { instrumentation.runOnMainSync { activity.finish() } }
        }
        val before = f.contacts(null, 100).first.size
        for (bad in listOf("garbage", "dmsg://contact/", "dmsg://join/broken", "dmsg://join/" + "x".repeat(9000))) {
            scan(bad, "битый QR:")
        }
        assertEquals(before, f.contacts(null, 100).first.size)
        assertEquals(Pair(true, id), f.account())
        scan(f.myQr(), "контакт: added")
        f.accept(id)
        val changed = UniFfiFacade(seedLegacyDb("changed-ui.db", 9).absolutePath, ByteArray(32) { 4 })
        scan(changed.myQr(), "identity_changed")
        Prefs.setTransport(app, "127.0.0.1:1", "test.invalid", "00".repeat(32))
        fun sendAndExpect(expected: String) {
            val activity = start(ChatActivity::class.java, id)
            try {
                instrumentation.runOnMainSync {
                    activity.findViewById<android.widget.EditText>(R.id.composer).setText("fixture draft")
                    activity.findViewById<android.widget.Button>(R.id.btn_send).performClick()
                }
                waitLabel(activity, R.id.info, expected)
                instrumentation.runOnMainSync {
                    assertEquals("fixture draft", activity.findViewById<android.widget.EditText>(R.id.composer).text.toString())
                }
            } finally { instrumentation.runOnMainSync { activity.finish() } }
        }
        sendAndExpect("identity changed")
        assertTrue(f.outbox(0, 100).first.isEmpty())
        val profile = start(ProfileActivity::class.java)
        try {
            instrumentation.runOnMainSync {
                profile.findViewById<android.widget.EditText>(R.id.peer_id).setText(id)
                profile.findViewById<android.widget.Button>(R.id.btn_check).performClick()
            }
            waitLabel(profile, R.id.info, "отправка СТОП")
            instrumentation.runOnMainSync { profile.findViewById<android.widget.Button>(R.id.btn_confirm).performClick() }
            waitLabel(profile, R.id.info, "confirmed")
            assertEquals(false, f.get(id)?.identityMismatch)
            instrumentation.runOnMainSync { profile.findViewById<android.widget.Button>(R.id.btn_check).performClick() }
            val label = waitLabel(profile, R.id.info, "identity_changed=false")
            assertFalse(label.contains("СТОП"))
        } finally { instrumentation.runOnMainSync { profile.finish() } }
        sendAndExpect("connect")
        assertTrue(f.outbox(0, 100).first.isEmpty())
    }

    private fun assertFailsWithMessage(needle: String, action: () -> Unit) {
        try { action(); fail("must fail closed") }
        catch (e: DmsgError) { assertTrue("unexpected error ${e.message}", e.message.orEmpty().contains(needle)) }
    }
}

package org.dmsg.client

import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.database.Cursor.FIELD_TYPE_BLOB
import android.database.sqlite.SQLiteDatabase
import androidx.test.core.app.ApplicationProvider
import androidx.test.core.app.ActivityScenario
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
import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.RegistrationPolicy

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
            assertTrue("preserve the existing account", before.authenticated)
            f.configureDns(input.readText().trim(), DnsNetwork.resolvers(app))
            DnsNetwork.mirrorProfile(app, f)
            assertNotNull(f.dnsProfile())
            assertTrue(f.reconnect() > 0L)
            assertEquals("ready", f.dnsStatus())
            val first = f.fetch()
            val second = f.fetch()
            assertEquals(0, second.received.size)
            assertTrue(second.cursor >= first.cursor)
            assertEquals(before, f.account())
        } finally {
            try { facade?.dnsStop() } finally { input.delete() }
        }
    }

    @Test fun startDnsForegroundForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        assertTrue(f.account().authenticated)
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

    @Test fun rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val inputs = listOf("gate-wrong-pin.qr", "gate-wrong-noise.qr").map { File(app.filesDir, it) }
        assumeTrue("explicit public negative profile fixtures required", inputs.all { it.exists() })
        val main = Core.facade(app)
        val before = main.account()
        val local = File(app.filesDir, "gate-carrier.json")
        val resolvers = if (local.exists()) fixtureResolvers(app, privateFixture(local)) else DnsNetwork.resolvers(app)
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
                        client.configureDns(QrGate.normalize(input.readText()), resolvers)
                        client.registrationPolicyDns()
                        fail("untrusted carrier/server must fail before credentials")
                    } catch (e: uniffi.dmsg_core.FfiException) {
                        if (index == 0) assertTrue("carrier pin must have its own error", e is uniffi.dmsg_core.FfiException.PinMismatch)
                        else assertTrue("wrong Noise key must fail before account authentication", e is uniffi.dmsg_core.FfiException.Transport)
                    }
                    assertFalse(client.accountInfo().authenticated)
                    assertTrue(client.contactsPage(null, 100u).rows.isEmpty())
                } finally {
                    client.dnsStop()
                    client.close()
                    dir.deleteRecursively()
                }
            }
            assertEquals(before, main.account())
        } finally { inputs.forEach { it.delete() }; local.delete() }
    }

    @Test fun restartDnsWithSameResolversPreservesIdentityForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val profile = f.dnsProfile()
        assumeTrue("configure a DNS profile explicitly first", profile != null)
        val before = f.account()
        assertTrue(before.authenticated)
        val outbox = f.outbox(0, 100)
        val inbox = f.inbox(0, 100)
        try {
            f.reconnect()
            assertEquals("ready", f.dnsStatus())
            f.dnsNetworkChanged(DnsNetwork.resolvers(app))
            assertEquals("stopped", f.dnsStatus())
            assertTrue(f.reconnect() > 0)
            assertEquals("ready", f.dnsStatus())
            assertEquals(before, f.account())
            assertEquals(profile!!.fingerprint, f.dnsProfile()!!.fingerprint)
            assertTrue(profile.pub.contentEquals(f.dnsProfile()!!.pub))
            assertEquals(outbox, f.outbox(0, 100))
            assertEquals(inbox, f.inbox(0, 100))
        } finally { f.dnsStop() }
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
        assertTrue("operator gates require the isolated package", ApplicationProvider.getApplicationContext<Context>().packageName.endsWith(".gate"))
        val selected = InstrumentationRegistry.getArguments().getString("class").orEmpty().split(',')
        assumeTrue("select this gate method explicitly", "${javaClass.name}#${testName.methodName}" in selected)
    }

    /** Local authoritative fixtures only: no hostname lookup or production resolver override. */
    private fun fixtureResolvers(app: Context, fixture: JSONObject): List<String> {
        if (!fixture.has("resolvers")) return DnsNetwork.resolvers(app)
        assertTrue("local carrier fixtures require the isolated package", app.packageName.endsWith(".gate"))
        val values = fixture.getJSONArray("resolvers")
        assertTrue("bounded local resolver list", values.length() in 1..8)
        return (0 until values.length()).map { index ->
            val value = values.getString(index)
            assertTrue("bounded numeric local IPv4 endpoint", value.length <= 21)
            val match = Regex("([0-9]{1,3})\\.([0-9]{1,3})\\.([0-9]{1,3})\\.([0-9]{1,3}):([0-9]{1,5})").matchEntire(value)
                ?: throw AssertionError("local resolver must be numeric IPv4 with an explicit high UDP port")
            val parts = (1..4).map { match.groupValues[it].toInt() }
            assertTrue("canonical IPv4", parts.all { it in 0..255 } && parts.joinToString(".") == value.substringBefore(':'))
            assertTrue("local carrier only", parts[0] == 10 || parts[0] == 127 ||
                (parts[0] == 192 && parts[1] == 168) || (parts[0] == 172 && parts[1] in 16..31))
            assertTrue("unprivileged local UDP port", match.groupValues[5].toInt() in 1024..65535)
            value
        }.distinct()
    }

    private fun privateFixture(input: File): JSONObject {
        assertTrue("private file-based fixture required", input.isFile)
        assertTrue("bounded private fixture", input.length() in 1..16_384)
        return JSONObject(input.readText())
    }

    /** The real facade/native core, without active-network refresh for explicit local fixtures. */
    private fun fixtureFacade(app: Context, fixture: JSONObject): DmsgFacade {
        if (!fixture.has("resolvers")) return Core.facade(app)
        fixtureResolvers(app, fixture)
        System.loadLibrary("dmsg_core")
        val key = SecureStore.key(app)
        return try { UniFfiFacade(Core.dbFile(app).absolutePath, key) } finally { key.fill(0) }
    }

    private fun seedFixtureDb(name: String = "core.db", deviceByte: Byte = 7): File {
        val file = File(dir, name)
        val key = if (name == "core.db") SecureStore.key(context) else ByteArray(32) { 4 }
        DmsgClient.openEncrypted(file.absolutePath, key).use { client ->
            client.accountInfo()
            client.configureDns(syntheticServerCode(), listOf("127.0.0.1:1"))
        }
        SQLiteDatabase.openDatabase(file.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
            db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(sealedFixtureValue(key,"device_priv",ByteArray(32) { deviceByte })))
            db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, id))
            db.execSQL("INSERT INTO core_messages(sender_device,message_id,contact_id,direction,kind,text,local_timestamp_ms,server_seq,server_timestamp_ms) VALUES(?,?,?,'incoming','text',?,1,1,1)",
                arrayOf(ByteArray(32) { 2 }, ByteArray(16) { 3 }, id, sealedFixtureValue(key,"message_text","fixture private text".toByteArray())))
        }
        key.fill(0)
        return file
    }

    /** Public binary-format fixture only; never a usable server or credential. */
    private fun syntheticServerCode(): String {
        val domain = "test.invalid".toByteArray()
        val raw = byteArrayOf(1, domain.size.toByte()) + domain + byteArrayOf(0, 64) + ByteArray(64) { 0x30 } + ByteArray(32)
        return "dmsg://server/" + android.util.Base64.encodeToString(raw, android.util.Base64.URL_SAFE or android.util.Base64.NO_WRAP or android.util.Base64.NO_PADDING)
    }

    @Test fun freshEncryptedReopenAndRestoreOnlyWithOriginalKey() {
        val db = seedFixtureDb()
        val f = Core.facade(context)
        assertEquals(AccountInfo(true, id), f.account())
        assertEquals("fixture private text", f.inbox(0, 10).first.single().text)
        SQLiteDatabase.openDatabase(db.absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { sql ->
            sql.rawQuery("SELECT device_priv FROM core_identity", null).use {
                assertTrue(it.moveToFirst()); assertEquals(FIELD_TYPE_BLOB, it.getType(0))
                assertFalse(it.getBlob(0).contentEquals(ByteArray(32) { 7 }))
            }
            sql.rawQuery("SELECT text FROM core_messages WHERE kind='text'", null).use {
                assertTrue(it.moveToFirst()); assertEquals(FIELD_TYPE_BLOB, it.getType(0))
            }
        }
        SecureStore.seal(context)
        assertEquals("ready", SecureStore.plan(context))
        SecureStore.wipe(db) // only the test fixture; the live app's DB is never deleted
        SecureStore.unseal(context)
        assertEquals(AccountInfo(true, id), Core.facade(context).account())
        assertEquals("fixture private text", Core.facade(context).inbox(0, 10).first.single().text)
        assertFailsWithMessage("live db already exists") { SecureStore.unseal(context) }
        // A wiped wrapping key cannot be silently recreated even with a sealed backup present.
        File(dir, "core.key.sealed").delete()
        assertFailsWithMessage("reinstall_loss") { Core.facade(context).account() }
    }

    @Test fun invalidQrIsRejectedWithoutAddingContact() {
        seedFixtureDb()
        val f = Core.facade(context)
        val before = f.contacts(null, 50).first.size
        for (qr in listOf("garbage", "dmsg://contact/", "dmsg://contact/broken", "dmsg://server/broken", "dmsg://server/" + "x".repeat(9000))) {
            assertFailsWithMessage("") { f.qrKind(qr) }
            assertFailsWithMessage("") { f.addQr(qr) }
        }
        assertEquals(before, f.contacts(null, 50).first.size)
    }

    @Test fun changedIdentityStopsSendUntilExplicitConfirm() {
        seedFixtureDb()
        val f = Core.facade(context)
        val otherDb = seedFixtureDb("other.db", 9)
        val key = ByteArray(32) { 4 } // test fixture only, never used for installed identity
        val other = UniFfiFacade(otherDb.absolutePath, key)
        val originalQr = f.myQr()
        val changedQr = other.myQr()
        assertEquals(uniffi.dmsg_core.QrOutcome.ADDED, f.addQr(originalQr))
        f.accept(id)
        assertEquals(uniffi.dmsg_core.QrOutcome.IDENTITY_CHANGED, f.addQr(changedQr))
        assertEquals(true, f.get(id)?.identityMismatch)
        assertFailsWithKind(ErrorKind.IdentityMismatch) {
            f.send(id, "fixture")
        }
        assertEquals(0, f.outbox(0, 50).first.size)
        f.confirm(id)
        assertEquals(false, f.get(id)?.identityMismatch)
        try { assertFailsWithKind(ErrorKind.Transport) { f.send(id, "fixture") } } finally { f.dnsStop() }
    }

    /** Dedicated .gate installation only; fixture is not the installed user's account. */
    @Test fun seedLargeChatOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        val dbFile = File(app.filesDir, "core.db")
        assertFalse("fixture package must be fresh", dbFile.exists())
        val key = SecureStore.key(app)
        DmsgClient.openEncrypted(dbFile.absolutePath,key).use { it.accountInfo(); it.configureDns(syntheticServerCode(), listOf("127.0.0.1:1")) }
        val peer = "PEER1234ABCD"
        SQLiteDatabase.openDatabase(dbFile.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
            db.beginTransaction()
            try {
                db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(sealedFixtureValue(key,"device_priv",ByteArray(32) { 7 })))
                db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, id))
                db.execSQL("INSERT INTO core_contacts(contact_id,state) VALUES(?,'requested')", arrayOf(peer))
                for (seq in 1..551) {
                    val mid = ByteArray(16)
                    mid[0] = (seq shr 8).toByte()
                    mid[1] = seq.toByte()
                    db.execSQL("INSERT INTO core_messages(message_id,contact_id,direction,kind,sender_device,text,local_timestamp_ms,server_seq,server_timestamp_ms) VALUES(?,?,'incoming','text',?,?,?,?,?)",
                        arrayOf(mid, peer, ByteArray(32) { 2 }, sealedFixtureValue(key,"message_text","fixture message $seq".toByteArray()), seq.toLong(),seq.toLong(),seq.toLong()))
                }
                db.setTransactionSuccessful()
            } finally { db.endTransaction() }
        }
        key.fill(0)
        assertEquals(AccountInfo(true, id), Core.facade(app).account())
        assertEquals(50, Core.facade(app).inbox(0, 50).first.size)
        assertEquals(50, Core.facade(app).historyPage(peer, null, 50).rows.size)
    }

    /** Run after seedLargeChatOnlyInGatePackage, with rebuilt R18 native libraries. */
    @Test fun textHistoryPagingAndDraftUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val peer = InstrumentationRegistry.getArguments().getString("peer") ?: "PEER1234ABCD"
        assertTrue("background worker must be stopped for deterministic local UI evidence", !DmsgService.running(app))
        assertTrue("seeded isolated account required", f.account().authenticated)
        val initial = f.historyPage(peer, null, 50)
        assumeTrue("seeded contact with older history required", initial.nextBeforeLocalId != null)
        val outboxBefore = f.outbox(0, 100)
        val readBefore = f.summary(peer)?.readCursor ?: 0L
        var renderedThrough = readBefore
        fun observe(activity: ChatActivity): android.widget.ListView {
            val list = activity.findViewById<android.widget.ListView>(R.id.messages)
            for (i in 0 until list.childCount) {
                val child = list.getChildAt(i)
                if (child.bottom > list.paddingTop && child.top < list.height - list.paddingBottom)
                    (child.tag as? Long)?.let { renderedThrough = maxOf(renderedThrough, it) }
            }
            return list
        }
        fun await(scenario: ActivityScenario<ChatActivity>, label: String, predicate: (ChatActivity) -> Boolean) {
            val deadline = System.nanoTime() + 20_000_000_000L
            while (System.nanoTime() < deadline) {
                var ready = false
                scenario.onActivity { observe(it); ready = predicate(it) }
                if (ready) return
                Thread.sleep(50)
            }
            fail(label)
        }
        val intent = Intent(app, ChatActivity::class.java).putExtra("peer", peer)
        ActivityScenario.launch<ChatActivity>(intent).use { scenario ->
            await(scenario, "local R18 history renders") { observe(it).adapter.count >= initial.rows.size && observe(it).childCount > 0 }
            scenario.onActivity {
                val list = observe(it)
                val ids = (0 until list.adapter.count).map { index -> (list.adapter.getItem(index) as uniffi.dmsg_core.HistoryMessage).localId }
                assertTrue("bubbles are chronological without duplicates", ids == ids.distinct().sorted())
                assertTrue("app-owned buttons are square and flat", it.findViewById<android.widget.Button>(R.id.btn_send).elevation == 0f)
                it.findViewById<android.widget.EditText>(R.id.composer).setText("disposable UI draft")
                list.setSelection(0)
            }
            await(scenario, "older pages are prepended") { observe(it).adapter.count > initial.rows.size }
            var anchor: Long? = null
            scenario.onActivity {
                val list = observe(it)
                anchor = list.getChildAt(0)?.tag as? Long
            }
            scenario.recreate()
            await(scenario, "retained history and draft survive recreation") {
                it.findViewById<android.widget.EditText>(R.id.composer).text.toString() == "disposable UI draft" &&
                    observe(it).adapter.count > initial.rows.size
            }
            scenario.onActivity {
                val list = observe(it)
                assertTrue("scroll anchor survives recreation", anchor == null || (0 until list.childCount).any { index -> list.getChildAt(index).tag == anchor })
            }
        }
        assertTrue("page reads never invent a newer read anchor", (f.summary(peer)?.readCursor ?: 0L) <= renderedThrough)
        assertTrue("draft and lifecycle never create an outgoing row", f.outbox(0, 100) == outboxBefore)
    }

    @Test fun sealSyntheticAccountBeforeResetOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        assertEquals(AccountInfo(true, id), Core.facade(app).account())
        SecureStore.seal(app)
        assertTrue(SecureStore.sealedDb(app).exists())
    }

    @Test fun verifyFreshAfterClearDataOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(app.packageName.endsWith(".gate"))
        assertFalse(Core.dbFile(app).exists())
        assertFalse(SecureStore.sealedDb(app).exists())
        assertFalse(Core.facade(app).account().authenticated)
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
        val expected = humanError(activity.resources, DmsgError("reinstall_loss", ErrorKind.StorageKeyLost))
        try {
            val deadline = System.nanoTime() + 5_000_000_000L
            var label = ""
            while (!label.contains(expected) && System.nanoTime() < deadline) {
                instrumentation.runOnMainSync {
                    label = activity.findViewById<android.widget.TextView>(R.id.status).text.toString()
                }
                Thread.sleep(50)
            }
            assertTrue("loss must be shown rather than an uncaught worker-thread crash", label.contains(expected))
        } finally { instrumentation.runOnMainSync { activity.finish() } }
    }

    /** Explicitly selected on-device gate, not part of the routine test suite. */
    @Test fun signupPrivateInviteOnce() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-auth.json")
        assumeTrue("private file-based signup fixture required", input.exists())
        val f = Core.facade(app)
        try {
            val auth = JSONObject(input.readText())
            assertFalse("never overwrite an existing account", f.account().authenticated)
            val preview = QrGate.serverPreview(f, auth.getString("serverCode"))
            f.configureDns(preview.code, DnsNetwork.resolvers(app))
            val flow = AuthFlow(f)
            val policy = flow.policy()
            assertEquals(RegistrationPolicy.INVITE_ONLY, policy)
            val secret = AuthSecrets(auth.getString("password").toCharArray(), auth.getString("invitation").toCharArray())
            assertTrue(flow.submit(AuthAction.Signup, policy, auth.getString("login"), secret) is LoginOutcome.Authenticated)
            assertTrue(f.account().authenticated)
            DnsNetwork.mirrorProfile(app, f)
        } finally {
            f.dnsStop()
            SecureStore.wipe(input)
        }
    }

    /** Fresh registration is destructive only to the deliberately separate gate package. */
    @Test fun signupPrivateDnsAccountOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assertTrue("fresh device registration requires the isolated gate package", app.packageName.endsWith(".gate"))
        val input = File(app.filesDir, "gate-auth.json")
        val auth = privateFixture(input)
        val f = fixtureFacade(app, auth)
        assertFalse("refusing to replace an existing identity", f.account().authenticated)
        try {
            val preview = QrGate.serverPreview(f, auth.getString("serverCode"))
            f.configureDns(preview.code, fixtureResolvers(app, auth))
            assertFalse("profile import does not create an account", f.account().authenticated)
            val flow = AuthFlow(f)
            val policy = flow.policy()
            assertEquals(auth.getString("expectedPolicy"), if (policy == RegistrationPolicy.OPEN) "open" else "invite_only")
            val invitation = if (policy == RegistrationPolicy.INVITE_ONLY) auth.getString("invitation") else ""
            val out = flow.submit(AuthAction.Signup, policy, auth.getString("login"), AuthSecrets(auth.getString("password").toCharArray(), invitation.toCharArray()))
            assertTrue(out is LoginOutcome.Authenticated)
            val id = (out as LoginOutcome.Authenticated).contactId
            assertEquals(AccountInfo(true, id), f.account())
            assertEquals(preview.domain, f.dnsProfile()!!.domain)
            assertEquals(preview.fingerprint, f.dnsProfile()!!.fingerprint)
            DnsNetwork.mirrorProfile(app, f)
            f.dnsStop()
            assertTrue(f.reconnect() > 0)
            val first = f.fetch()
            val second = f.fetch()
            assertTrue(first.received.isEmpty()); assertTrue(second.received.isEmpty())
            assertTrue(second.cursor >= first.cursor)
            assertEquals(AccountInfo(true, id), Core.facade(app).account())
            SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { sql ->
                sql.rawQuery("PRAGMA user_version", null).use { row -> assertTrue(row.moveToFirst()); assertEquals(9, row.getInt(0)) }
            }
            val output = File(app.filesDir, "gate-account.json")
            assertFalse("finish previous gate first", output.exists())
            output.writeText(JSONObject().put("contactId", id).toString())
            assertTrue(output.setReadable(false, false)); assertTrue(output.setWritable(false, false))
            assertTrue(output.setReadable(true, true)); assertTrue(output.setWritable(true, true))
        } finally { f.dnsStop(); SecureStore.wipe(input) }
    }

    /** Run in a second instrumentation process after signup; no password/invitation fixture. */
    @Test fun reopenPrivateDnsAccountOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-reopen.json")
        val saved = File(app.filesDir, "gate-account.json")
        val fixture = privateFixture(input)
        val expected = privateFixture(saved).getString("contactId")
        assertFalse("key-only fixture", fixture.has("password") || fixture.has("invitation"))
        val f = fixtureFacade(app, fixture)
        try {
            assertTrue("native facade loaded", f.isReady())
            assertEquals(AccountInfo(true, expected), f.account())
            val before = f.dnsProfile()!!
            f.configureDns(QrGate.serverPreview(f, fixture.getString("serverCode")).code, fixtureResolvers(app, fixture))
            assertEquals(before.fingerprint, f.dnsProfile()!!.fingerprint)
            assertTrue(f.reconnect() > 0)
            assertEquals("ready", f.dnsStatus())
            val first = f.fetch(); val second = f.fetch()
            assertTrue(first.received.isEmpty()); assertTrue(second.received.isEmpty())
            assertTrue(second.cursor >= first.cursor)
            assertEquals(AccountInfo(true, expected), Core.facade(app).account())
        } finally { f.dnsStop(); SecureStore.wipe(input); saved.delete() }
    }

    /** No replacement without an explicit private operator fixture and exact CAS key. */
    @Test fun loginPrivateAccountReplacementOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-login.json")
        val auth = privateFixture(input)
        val f = fixtureFacade(app, auth)
        val flow = AuthFlow(f)
        try {
            assertFalse("replacement gate needs a fresh disposable identity", f.account().authenticated)
            val preview = QrGate.serverPreview(f, auth.getString("serverCode"))
            f.configureDns(preview.code, fixtureResolvers(app, auth))
            flow.policy() // Close pre-auth transport before credential input.
            val out = flow.submit(AuthAction.Login, null, auth.getString("login"), AuthSecrets(auth.getString("password").toCharArray()))
            assertTrue("this fixture must name an account bound to another device", out is LoginOutcome.ReplacementRequired)
            assertFalse("first login must not bind the new device", f.account().authenticated)
            assertEquals("stopped", f.dnsStatus())
            assertEquals(auth.getString("expectedDevice"), (out as LoginOutcome.ReplacementRequired).expectedDevice)
            if (auth.getBoolean("confirmReplacement")) {
                val accepted = flow.confirm()
                assertTrue("a new CAS challenge requires new operator confirmation", accepted is LoginOutcome.Authenticated)
                assertEquals(auth.getString("contactId"), (accepted as LoginOutcome.Authenticated).contactId)
                assertEquals(AccountInfo(true, accepted.contactId), f.account())
                assertTrue("replacement does not import old history", f.inbox(0, 100).first.isEmpty())
                DnsNetwork.mirrorProfile(app, f)
                assertTrue(f.reconnect() > 0)
                assertEquals(AccountInfo(true, accepted.contactId), Core.facade(app).account())
            } else {
                flow.cancel()
                assertFalse(flow.awaitingConfirmation)
                assertFalse(f.account().authenticated)
            }
        } finally { flow.cancel(); f.dnsStop(); SecureStore.wipe(input) }
    }

    /** Typed negative auth errors; fresh fixture prevents mutation of a working account. */
    @Test fun rejectPrivateCredentialsOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-auth-error.json")
        val auth = privateFixture(input)
        val f = fixtureFacade(app, auth)
        val flow = AuthFlow(f)
        try {
            assertFalse(f.account().authenticated)
            f.configureDns(QrGate.serverPreview(f, auth.getString("serverCode")).code, fixtureResolvers(app, auth))
            val policy = flow.policy()
            val action = when (auth.getString("action")) {
                "login" -> AuthAction.Login
                "signup" -> AuthAction.Signup
                else -> throw AssertionError("fixture action must be login/signup")
            }
            val expected = ErrorKind.valueOf(auth.getString("expectedError"))
            assertFailsWithKind(expected) {
                flow.submit(action, policy, auth.getString("login"),
                    AuthSecrets(auth.getString("password").toCharArray(), auth.optString("invitation", "").toCharArray()))
            }
            assertFalse(f.account().authenticated)
            assertTrue(f.contacts(null, 100).first.isEmpty())
            assertFalse(flow.awaitingConfirmation)
            assertEquals("stopped", f.dnsStatus())
        } finally { flow.cancel(); f.dnsStop(); SecureStore.wipe(input) }
    }

    /** Real UI + native facade; absent resolvers means the actual active-network DNS path. */
    @Test fun unifiedAuthUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-ui.json")
        val fixture = privateFixture(input)
        val real = fixtureFacade(app, fixture)
        assertFalse(real.account().authenticated)
        assertNull("fresh disposable UI profile", real.dnsProfile())
        val local = if (fixture.has("resolvers")) object : DmsgFacade by real {
            override fun configureDns(code: String, resolvers: List<String>) = real.configureDns(code, fixtureResolvers(app, fixture))
        } else real
        fun field(activity: MainActivity, name: String): Any? = MainActivity::class.java.getDeclaredField(name)
            .also { it.isAccessible = true }.get(activity)
        fun setField(activity: MainActivity, name: String, value: Any) = MainActivity::class.java.getDeclaredField(name)
            .also { it.isAccessible = true }.set(activity, value)
        fun await(scenario: ActivityScenario<MainActivity>, message: String, predicate: (MainActivity) -> Boolean) {
            val deadline = System.nanoTime() + 45_000_000_000L
            while (System.nanoTime() < deadline) {
                var ready = false
                scenario.onActivity { ready = predicate(it) }
                if (ready) return
                Thread.sleep(50)
            }
            fail(message)
        }
        fun dialog(activity: MainActivity) = field(activity, "prompt") as? androidx.appcompat.app.AlertDialog
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                await(scenario, "connection UI ready and focused") {
                    field(it, "state") == LaunchState.Connection && field(it, "busy") == false && it.hasWindowFocus()
                }
                scenario.onActivity {
                    setField(it, "facade", local); setField(it, "flow", AuthFlow(local))
                    assertEquals(android.view.View.GONE, it.findViewById<android.view.View>(R.id.dialogs_panel).visibility)
                    assertFalse(DmsgService.running(app))
                }
                val preview = QrGate.serverPreview(real, fixture.getString("serverCode"))
                fun previewUi() {
                    scenario.onActivity {
                        val code = fixture.getString("serverCode")
                        val editor = it.findViewById<android.widget.EditText>(R.id.connection_code)
                        val clipboard = it.getSystemService(Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
                        val previous = clipboard.primaryClip
                        val clip = android.content.ClipData.newPlainText("disposable gate fixture", code.chunked(72).joinToString("\n"))
                        if (android.os.Build.VERSION.SDK_INT >= 24) clip.description.extras = android.os.PersistableBundle().apply {
                            putBoolean("android.content.extra.IS_SENSITIVE", true)
                        }
                        try {
                            clipboard.setPrimaryClip(clip)
                            assertTrue("foreground app can read its pasted fixture", clipboard.primaryClip?.getItemAt(0)?.text?.toString() == clip.getItemAt(0).text.toString())
                            editor.setText(""); editor.requestFocus()
                            assertTrue("actual multiline clipboard paste", editor.onTextContextMenuItem(android.R.id.paste))
                            assertTrue("clipboard pasted the complete public profile", QrGate.normalize(editor.text.toString()) == code)
                        } finally {
                            if (previous != null) clipboard.setPrimaryClip(previous)
                            else if (android.os.Build.VERSION.SDK_INT >= 28) clipboard.clearPrimaryClip()
                            else clipboard.setPrimaryClip(android.content.ClipData.newPlainText("", ""))
                        }
                        assertTrue("preview enabled before click", field(it, "busy") == false)
                        it.findViewById<android.widget.Button>(R.id.btn_preview).performClick()
                    }
                    await(scenario, "offline profile confirmation") { dialog(it)?.isShowing == true && field(it, "busy") == false }
                    scenario.onActivity {
                        val message = dialog(it)!!.findViewById<android.widget.TextView>(android.R.id.message)!!.text.toString()
                        assertTrue("preview domain", message.contains(preview.domain))
                        assertTrue("preview certificate fingerprint", message.contains(preview.fingerprint))
                    }
                    assertNull("preview must remain offline and unimported", real.dnsProfile())
                    assertFalse(real.account().authenticated)
                    assertEquals("stopped", real.dnsStatus())
                }
                previewUi()
                scenario.onActivity { dialog(it)!!.getButton(android.content.DialogInterface.BUTTON_NEGATIVE).performClick() }
                assertNull(real.dnsProfile())
                previewUi()
                scenario.onActivity { dialog(it)!!.getButton(android.content.DialogInterface.BUTTON_POSITIVE).performClick() }
                await(scenario, "DNS policy closes before typing") { field(it, "state") == LaunchState.Authentication && field(it, "policy") == RegistrationPolicy.INVITE_ONLY && field(it, "busy") == false }
                assertFalse(real.account().authenticated)
                assertEquals("stopped", real.dnsStatus())
                scenario.onActivity {
                    assertEquals(android.view.View.GONE, it.findViewById<android.view.View>(R.id.invitation_group).visibility)
                    it.findViewById<android.widget.Button>(R.id.btn_signup).performClick()
                    assertEquals(android.view.View.VISIBLE, it.findViewById<android.view.View>(R.id.invitation_group).visibility)
                    it.findViewById<android.widget.EditText>(R.id.auth_password).setText("disposable draft")
                    (field(it, "invitation") as InvitationMemory).replace(InvitationInput.parse("A".repeat(43)))
                    it.findViewById<android.widget.Button>(R.id.btn_login).performClick()
                    assertTrue(it.findViewById<android.widget.EditText>(R.id.auth_password).text.isEmpty())
                    assertFalse((field(it, "invitation") as InvitationMemory).hasInvitation)
                    assertEquals(android.view.View.GONE, it.findViewById<android.view.View>(R.id.invitation_group).visibility)
                }
                fun loginUi() {
                    scenario.onActivity {
                        it.findViewById<android.widget.EditText>(R.id.auth_login).setText(fixture.getString("login"))
                        it.findViewById<android.widget.EditText>(R.id.auth_password).setText(fixture.getString("password"))
                        it.findViewById<android.widget.Button>(R.id.btn_auth_submit).performClick()
                    }
                    await(scenario, "replacement warning after DNS login") { dialog(it)?.isShowing == true && (field(it, "flow") as AuthFlow).awaitingConfirmation }
                    assertFalse(real.account().authenticated)
                    assertEquals("stopped", real.dnsStatus())
                    scenario.onActivity {
                        val message = dialog(it)!!.findViewById<android.widget.TextView>(android.R.id.message)!!.text.toString()
                        assertEquals(it.getString(R.string.replacement_warning), message)
                        assertTrue(it.findViewById<android.widget.EditText>(R.id.auth_password).text.isEmpty())
                        assertFalse((field(it, "invitation") as InvitationMemory).hasInvitation)
                    }
                }
                loginUi()
                scenario.onActivity {
                    dialog(it)!!.getButton(android.content.DialogInterface.BUTTON_NEGATIVE).performClick()
                }
                await(scenario, "replacement cancellation callback") {
                    !(field(it, "flow") as AuthFlow).awaitingConfirmation && dialog(it)?.isShowing != true
                }
                assertFalse(real.account().authenticated)
                loginUi()
                scenario.onActivity { dialog(it)!!.getButton(android.content.DialogInterface.BUTTON_POSITIVE).performClick() }
                await(scenario, "authenticated dialogs after explicit replacement") { field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false }
                assertEquals(AccountInfo(true, fixture.getString("contactId")), real.account())
                assertTrue(real.inbox(0, 100).first.isEmpty())
                scenario.onActivity {
                    assertEquals(android.view.View.GONE, it.findViewById<android.view.View>(R.id.auth_panel).visibility)
                    assertEquals(android.view.View.VISIBLE, it.findViewById<android.view.View>(R.id.dialogs_panel).visibility)
                    assertFalse(DmsgService.running(app))
                }
                scenario.recreate()
                await(scenario, "authenticated restart routes to dialogs") { field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false }
                assertEquals(AccountInfo(true, fixture.getString("contactId")), Core.facade(app).account())
            }
            assertTrue(real.reconnect() > 0)
            assertTrue(real.fetch().received.isEmpty())
        } finally { real.dnsStop(); SecureStore.wipe(input) }
    }

    /** One-off paired-device gate: only the public contact QR leaves the app sandbox. */
    @Test fun exportMyContactQrForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val output = File(app.filesDir, "gate-my-contact.qr")
        assertFalse("refusing to overwrite contact fixture", output.exists())
        val f = Core.facade(app)
        assertTrue("phone must already be authenticated", f.account().authenticated)
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
            assertTrue("phone must already be authenticated", f.account().authenticated)
            val before = f.contacts(null, 100).first.map { it.contactId }.toSet()
            assertEquals(uniffi.dmsg_core.QrOutcome.ADDED, f.addQr(input.readText().trim()))
            val after = f.contacts(null, 100).first.map { it.contactId }.toSet()
            val peer = (after - before).single()
            f.accept(peer)
            val selection = File(app.filesDir, "gate-peer-contact-id")
            selection.writeText(peer)
            assertTrue(selection.setReadable(false, false)); assertTrue(selection.setWritable(false, false))
            assertTrue(selection.setReadable(true, true)); assertTrue(selection.setWritable(true, true))
            f.reconnect()
        } finally {
            input.delete()
        }
    }

    /** Record the encrypted inbox's current page count before a physical gate. */
    @Test fun recordInboxBaselineForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        assertTrue(f.account().authenticated)
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
            val first = f.fetch()
            val page = f.inbox(0, 100).first
            assertEquals("exactly one additional delivered message", previous + 1, page.size)
            assertEquals(page.size, page.map { it.seq }.toSet().size)
            val second = f.fetch()
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
        f.send(peer,
            "Paired transport session probe.")
    }

    private fun ciphertextHash(app: Context, mid: String): String {
        SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { db ->
            db.rawQuery("SELECT ciphertext FROM core_messages WHERE direction='outgoing' AND lower(hex(message_id))=?", arrayOf(mid)).use { c ->
                assertTrue("persisted ciphertext is required", c.moveToFirst())
                return MessageDigest.getInstance("SHA-256").digest(c.getBlob(0))
                    .joinToString("") { "%02x".format(it.toInt() and 255) }
            }
        }
    }

    private fun gateWrite(app: Context, name: String, value: String) {
        val output = File(app.filesDir, name)
        output.writeText(value)
        assertTrue(output.setReadable(false, false)); assertTrue(output.setWritable(false, false))
        assertTrue(output.setReadable(true, true)); assertTrue(output.setWritable(true, true))
    }

    private fun gateScreenshot(app: Context, name: String) {
        // Insets/orientation assertions can precede SurfaceFlinger's completed animation frame.
        InstrumentationRegistry.getInstrumentation().waitForIdleSync()
        Thread.sleep(750)
        InstrumentationRegistry.getInstrumentation().waitForIdleSync()
        val bitmap = InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot()
        assertNotNull("actual device screenshot", bitmap)
        val output = File(app.filesDir, "$name.png")
        output.outputStream().use { bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it) }
        assertTrue(output.setReadable(false, false)); assertTrue(output.setWritable(false, false))
        assertTrue(output.setReadable(true, true)); assertTrue(output.setWritable(true, true))
        bitmap.recycle()
    }

    private fun <A : android.app.Activity> gateAwait(scenario: ActivityScenario<A>, label: String, predicate: (A) -> Boolean) {
        val deadline = System.nanoTime() + 60_000_000_000L
        while (System.nanoTime() < deadline) {
            var ready = false
            scenario.onActivity { ready = predicate(it) }
            if (ready) return
            Thread.sleep(50)
        }
        fail(label)
    }

    private fun texts(view: android.view.View): List<String> = when (view) {
        is android.view.ViewGroup -> (0 until view.childCount).flatMap { texts(view.getChildAt(it)) }
        is android.widget.TextView -> listOf(view.text.toString())
        else -> emptyList()
    }

    @Test fun exportActiveNetworkResolversOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val cm = app.getSystemService(android.net.ConnectivityManager::class.java)
        val lp = cm.getLinkProperties(cm.activeNetwork) ?: throw AssertionError("active network required")
        val v4 = lp.dnsServers.filterIsInstance<java.net.Inet4Address>()
        val selected = if (v4.isNotEmpty()) v4 else lp.dnsServers
        val expected = selected.take(8).map { if (it is java.net.Inet4Address) "${it.hostAddress}:53" else "[${it.hostAddress}]:53" }
        assertEquals("production resolver source is current LinkProperties", expected, DnsNetwork.resolvers(app))
        assertTrue(expected.isNotEmpty())
        gateWrite(app, "gate-resolvers.txt", expected.joinToString("\n") + "\n")
    }

    /** Incoming transport evidence and real summary/bubbles, not a synthetic history fixture. */
    @Test fun pairedIncomingHistoryUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-incoming.json")
        val fixture = privateFixture(input)
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        try {
            val first = f.fetch(); val second = f.fetch()
            assertEquals(1, first.received.size); assertEquals(0, second.received.size)
            assertTrue(first.skipped.all { it == 0L }); assertTrue(second.skipped.all { it == 0L })
            assertEquals(fixture.getString("text"), first.received.single().text)
            val summary = f.summary(peer)!!
            assertTrue(summary.localUnread > 0uL)
            assertTrue(summary.lastLocalTimestampMs!! > 1_700_000_000_000L)
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                gateAwait(scenario, "real unread dialog summary") {
                    val list = it.findViewById<android.widget.ListView>(R.id.dialogs)
                    list.childCount > 0 && texts(list).any { text -> text.contains(it.getString(R.string.local_unread, summary.localUnread.toString())) }
                }
                gateScreenshot(app, "dialogs-unread")
            }
            ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer)).use { scenario ->
                gateAwait(scenario, "decrypted incoming bubble and local time") {
                    val content = texts(it.findViewById(R.id.messages))
                    fixture.getString("text") in content && content.any { label -> label.contains(it.getString(R.string.message_incoming)) && label.contains(it.getString(R.string.server_timestamp, "")) }
                }
                gateScreenshot(app, "chat-incoming")
                scenario.recreate()
                gateAwait(scenario, "incoming history after recreation") { fixture.getString("text") in texts(it.findViewById(R.id.messages)) }
            }
            assertEquals(0uL, f.summary(peer)!!.localUnread)
        } finally { f.dnsStop(); SecureStore.wipe(input) }
    }

    /** Cancel only this app's connecting native carrier; Wi-Fi and its resolvers stay intact. */
    @Test fun pairedQueuedSendUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val fixture = privateFixture(File(app.filesDir, "gate-send.json"))
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        val before = f.historyPage(peer, null, 100).rows
        assertTrue("a real peer session must be established first", before.isNotEmpty())
        f.dnsStop()
        val native = UniFfiFacade::class.java.getDeclaredField("core").also { it.isAccessible = true }.get(f) as DmsgClient
        ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer)).use { scenario ->
            gateAwait(scenario, "trusted chat ready") { it.findViewById<android.widget.Button>(R.id.btn_send).isEnabled }
            scenario.onActivity {
                it.findViewById<android.widget.EditText>(R.id.composer).setText(fixture.getString("text"))
                it.findViewById<android.widget.Button>(R.id.btn_send).performClick()
                it.findViewById<android.widget.Button>(R.id.btn_send).performClick()
                assertFalse("pending guard disables double submit", it.findViewById<android.widget.Button>(R.id.btn_send).isEnabled)
            }
            val deadline = System.nanoTime() + 5_000_000_000L
            while (native.dnsStatus() != "connecting" && System.nanoTime() < deadline) Thread.sleep(5)
            assertEquals("real native connection is pending", "connecting", native.dnsStatus())
            native.dnsStop()
            gateAwait(scenario, "durably saved text clears draft") {
                it.findViewById<android.widget.EditText>(R.id.composer).text.isEmpty() && it.findViewById<android.widget.TextView>(R.id.info).text.contains(it.getString(R.string.message_saved, it.getString(R.string.delivery_queued)))
            }
            val rows = f.historyPage(peer, null, 100).rows
            assertEquals("one history row despite repeated click", before.size + 1, rows.size)
            val sent = rows.single { row -> row.messageIdHex !in before.map { old -> old.messageIdHex } }
            assertEquals(fixture.getString("text"), sent.text)
            assertEquals(uniffi.dmsg_core.DeliveryState.QUEUED, f.messageStatus(sent.messageIdHex))
            gateWrite(app, "gate-queued-record", JSONObject().put("mid", sent.messageIdHex).put("ciphertextHash", ciphertextHash(app, sent.messageIdHex)).put("account", f.account().contactId).put("inboxCount", f.inbox(0, 100).first.size).toString())
            gateAwait(scenario, "queued bubble renders exact local status") {
                val content = texts(it.findViewById(R.id.messages))
                fixture.getString("text") in content && content.any { label -> label.contains(it.getString(R.string.delivery_queued)) }
            }
            gateScreenshot(app, "chat-queued")
        }
        f.dnsStop()
    }

    /** Run after process-death retry and before the real native peer fetches the text. */
    @Test fun pairedAcceptedHistoryUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val fixture = privateFixture(File(app.filesDir, "gate-send.json"))
        val mid = privateFixture(File(app.filesDir, "gate-delivered.json")).getString("mid")
        val peer = gatePeer(app, f)
        assertEquals(uniffi.dmsg_core.DeliveryState.ACCEPTED, f.messageStatus(mid))
        assertEquals(1, f.historyPage(peer, null, 100).rows.count { it.messageIdHex == mid })
        ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer)).use { scenario ->
            gateAwait(scenario, "accepted bubble renders server acceptance") {
                val content = texts(it.findViewById(R.id.messages))
                fixture.getString("text") in content && content.any { label -> label.contains(it.getString(R.string.delivery_accepted)) }
            }
            gateScreenshot(app, "chat-accepted")
        }
        f.dnsStop()
    }

    @Test fun pairedDeliveredHistoryReopenUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val fixture = privateFixture(File(app.filesDir, "gate-send.json"))
        val saved = privateFixture(File(app.filesDir, "gate-delivered.json"))
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        val mid = saved.getString("mid")
        assertTrue("accepted retry or already delivered on reopened process", f.messageStatus(mid) in listOf(uniffi.dmsg_core.DeliveryState.ACCEPTED, uniffi.dmsg_core.DeliveryState.DELIVERED))
        f.retry()
        assertEquals(uniffi.dmsg_core.DeliveryState.DELIVERED, f.messageStatus(mid))
        assertTrue(f.outbox(0, 100).first.none { it.mid == mid })
        assertEquals(1, f.historyPage(peer, null, 100).rows.count { it.messageIdHex == mid })
        ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer)).use { scenario ->
            gateAwait(scenario, "delivered history renders exact status") {
                val content = texts(it.findViewById(R.id.messages))
                fixture.getString("text") in content && content.any { label -> label.contains(it.getString(R.string.delivery_delivered)) }
            }
            gateScreenshot(app, "chat-delivered")
            scenario.recreate()
            gateAwait(scenario, "delivered history survives activity restart") { fixture.getString("text") in texts(it.findViewById(R.id.messages)) }
        }
        ActivityScenario.launch(OutboxActivity::class.java).use { scenario ->
            gateAwait(scenario, "queue emptiness is not a false delivery inference") { it.findViewById<android.widget.TextView>(R.id.outbox_empty).text.toString() == it.getString(R.string.outbox_empty) }
            gateScreenshot(app, "outbox-empty")
        }
        f.dnsStop()
    }

    @Test fun frontendCardsConnectionAndLargeTextOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val layout = File(app.filesDir, "gate-layout.json")
        val peer = if (layout.exists()) privateFixture(layout).getString("contactId") else gatePeer(app, f)
        assertTrue("actual 200 percent system font required", app.resources.configuration.fontScale >= 1.99f)
        ActivityScenario.launch<ProfileActivity>(Intent(app, ProfileActivity::class.java).putExtra("peer", peer)).use { scenario ->
            gateAwait(scenario, "contact card ready") {
                it.findViewById<android.widget.Button>(R.id.btn_alias_save).isEnabled && it.findViewById<android.widget.TextView>(R.id.info).text.toString() == trustLabel(it.resources, f.get(peer))
            }
            if (f.get(peer)!!.state == "requested") {
                scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_accept).performClick() }
                gateAwait(scenario, "explicit contact acceptance persisted and view idle") {
                    f.get(peer)!!.state == "accepted" && it.findViewById<android.widget.Button>(R.id.btn_accept).visibility == android.view.View.GONE && it.findViewById<android.widget.Button>(R.id.btn_alias_save).isEnabled
                }
            }
            scenario.onActivity {
                it.findViewById<android.widget.EditText>(R.id.contact_alias).setText("Native DNS peer")
                it.findViewById<android.widget.Button>(R.id.btn_alias_save).performClick()
            }
            gateAwait(scenario, "local alias actually persisted") { it.findViewById<android.widget.Button>(R.id.btn_alias_save).isEnabled && f.summary(peer)?.localAlias == "Native DNS peer" }
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_block).performClick() }
            scenario.onActivity {
                val prompt = ProfileActivity::class.java.getDeclaredField("prompt").also { field -> field.isAccessible = true }.get(it) as androidx.appcompat.app.AlertDialog
                assertEquals(it.getString(R.string.block_warning), prompt.findViewById<android.widget.TextView>(android.R.id.message)!!.text.toString())
                prompt.getButton(android.content.DialogInterface.BUTTON_NEGATIVE).performClick()
            }
            assertEquals("accepted", f.get(peer)!!.state)
            gateScreenshot(app, "contact-200")
        }
        ActivityScenario.launch<ProfileActivity>(Intent(app, ProfileActivity::class.java).putExtra("mine", true)).use { scenario ->
            gateAwait(scenario, "my public QR and ID are real") {
                it.findViewById<android.widget.ImageView>(R.id.qr).drawable != null && it.findViewById<android.widget.TextView>(R.id.my_id).text.toString() == f.account().contactId
            }
            gateScreenshot(app, "my-qr-200")
        }
        ActivityScenario.launch(DiagnosticsActivity::class.java).use { scenario ->
            val economy = Prefs.economy(app)
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_economy).performClick() }
            assertEquals(!economy, Prefs.economy(app))
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_economy).performClick(); it.findViewById<android.widget.Button>(R.id.btn_reconnect).performClick() }
            gateAwait(scenario, "real manual DNS success while service disabled") {
                it.findViewById<android.widget.TextView>(R.id.stats).text.contains(it.getString(R.string.dns_check_complete).substringBefore("%1\$d")) && !DmsgService.connectionState().serviceEnabled && DmsgService.connectionState().lastSuccessAt != null
            }
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_fgs).performClick() }
            gateAwait(scenario, "real foreground DNS started") { DmsgService.running(app) && DmsgService.connectionState().serviceEnabled }
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_fgs).performClick() }
            gateAwait(scenario, "foreground stop reported honestly") { !DmsgService.running(app) && !DmsgService.connectionState().serviceEnabled && f.dnsStatus() == "stopped" }
            gateScreenshot(app, "connection-200")
        }
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            gateAwait(scenario, "alias rendered in dialogs") { "Native DNS peer" in texts(it.findViewById(R.id.dialogs)) }
            gateScreenshot(app, "dialogs-200")
        }
        ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer).putExtra("alias", "Native DNS peer")).use { scenario ->
            gateAwait(scenario, "large-font chat ready") { it.findViewById<android.widget.Button>(R.id.btn_send).isEnabled && it.hasWindowFocus() }
            scenario.onActivity {
                val send = it.findViewById<android.widget.Button>(R.id.btn_send)
                assertTrue("send target >=48dp", send.height >= NativeUi.dp(it, 48))
                assertEquals(0f, send.elevation)
                val editor = it.findViewById<android.widget.EditText>(R.id.composer)
                editor.setText("Large font retained draft\nsecond line")
                editor.requestFocus()
                it.getSystemService(android.view.inputmethod.InputMethodManager::class.java).showSoftInput(editor, android.view.inputmethod.InputMethodManager.SHOW_IMPLICIT)
            }
            gateAwait(scenario, "actual IME with accessible composer and send") {
                val decor = it.window.decorView
                val ime = androidx.core.view.ViewCompat.getRootWindowInsets(decor)?.isVisible(androidx.core.view.WindowInsetsCompat.Type.ime()) == true
                val send = it.findViewById<android.widget.Button>(R.id.btn_send)
                val rect = android.graphics.Rect()
                ime && send.getGlobalVisibleRect(rect) && rect.height() == send.height
            }
            gateScreenshot(app, "chat-ime-200")
            scenario.onActivity {
                assertTrue("IME remains visible after animation", androidx.core.view.ViewCompat.getRootWindowInsets(it.window.decorView)?.isVisible(androidx.core.view.WindowInsetsCompat.Type.ime()) == true)
                assertEquals(android.view.View.GONE, it.findViewById<android.view.View>(R.id.chat_actions).visibility)
            }
            scenario.onActivity { it.requestedOrientation = android.content.pm.ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE }
            gateAwait(scenario, "actual landscape with retained draft") { it.resources.configuration.orientation == android.content.res.Configuration.ORIENTATION_LANDSCAPE && it.findViewById<android.widget.EditText>(R.id.composer).text.toString().startsWith("Large font retained draft") }
            gateScreenshot(app, "chat-landscape-200")
            scenario.onActivity {
                assertTrue("landscape surface has completed rotation", it.window.decorView.width > it.window.decorView.height)
                val send = it.findViewById<android.widget.Button>(R.id.btn_send)
                val rect = android.graphics.Rect()
                assertTrue("landscape send remains visible", send.getGlobalVisibleRect(rect) && rect.height() == send.height)
                val ime = androidx.core.view.ViewCompat.getRootWindowInsets(it.window.decorView)!!.getInsets(androidx.core.view.WindowInsetsCompat.Type.ime())
                assertTrue("landscape send is physically above keyboard", rect.bottom <= it.window.decorView.height - ime.bottom)
            }
            scenario.moveToState(androidx.lifecycle.Lifecycle.State.CREATED)
            scenario.moveToState(androidx.lifecycle.Lifecycle.State.RESUMED)
            gateAwait(scenario, "background return preserves draft") { it.findViewById<android.widget.EditText>(R.id.composer).text.toString().startsWith("Large font retained draft") }
            scenario.onActivity { it.requestedOrientation = android.content.pm.ActivityInfo.SCREEN_ORIENTATION_PORTRAIT }
            gateAwait(scenario, "portrait restored") { it.resources.configuration.orientation == android.content.res.Configuration.ORIENTATION_PORTRAIT }
        }
        f.dnsStop()
    }

    /** After the disposable native peer was password-replaced and sent once over recursive DNS. */
    @Test fun pairedChangedIdentityStopConfirmUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val selection = File(app.filesDir, "gate-peer-contact-id").readText().trim()
        val first = f.fetch()
        assertEquals(1L, first.skipped[3]); assertTrue(first.received.isEmpty())
        assertTrue(f.get(selection)!!.identityMismatch)
        ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", selection)).use { scenario ->
            gateAwait(scenario, "changed identity blocks actual send") {
                !it.findViewById<android.widget.Button>(R.id.btn_send).isEnabled && it.findViewById<android.widget.TextView>(R.id.info).text.contains(it.getString(R.string.trust_changed))
            }
            gateScreenshot(app, "chat-stop-changed-key")
        }
        ActivityScenario.launch<ProfileActivity>(Intent(app, ProfileActivity::class.java).putExtra("peer", selection)).use { scenario ->
            gateAwait(scenario, "explicit trust confirmation enabled") { it.findViewById<android.widget.Button>(R.id.btn_confirm).visibility == android.view.View.VISIBLE && it.findViewById<android.widget.Button>(R.id.btn_confirm).isEnabled }
            fun prompt(activity: ProfileActivity) = ProfileActivity::class.java.getDeclaredField("prompt").also { field -> field.isAccessible = true }.get(activity) as androidx.appcompat.app.AlertDialog
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_confirm).performClick() }
            scenario.onActivity { prompt(it).getButton(android.content.DialogInterface.BUTTON_NEGATIVE).performClick() }
            assertTrue("cancel retains STOP", f.get(selection)!!.identityMismatch)
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_confirm).performClick() }
            scenario.onActivity { prompt(it).getButton(android.content.DialogInterface.BUTTON_POSITIVE).performClick() }
            gateAwait(scenario, "explicit confirm installs new keys") { !f.get(selection)!!.identityMismatch && it.findViewById<android.widget.Button>(R.id.btn_confirm).visibility == android.view.View.GONE }
        }
        val delivered = f.fetch(); val repeat = f.fetch()
        assertEquals(1, delivered.received.size); assertEquals(0, repeat.received.size)
        assertTrue(delivered.skipped.all { it == 0L }); assertTrue(repeat.skipped.all { it == 0L })
        f.dnsStop()
    }

    @Test fun pairedBlockConfirmationUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        ActivityScenario.launch<ProfileActivity>(Intent(app, ProfileActivity::class.java).putExtra("peer", peer)).use { scenario ->
            gateAwait(scenario, "block button ready") { it.findViewById<android.widget.Button>(R.id.btn_block).isEnabled }
            scenario.onActivity { it.findViewById<android.widget.Button>(R.id.btn_block).performClick() }
            scenario.onActivity {
                val prompt = ProfileActivity::class.java.getDeclaredField("prompt").also { field -> field.isAccessible = true }.get(it) as androidx.appcompat.app.AlertDialog
                prompt.getButton(android.content.DialogInterface.BUTTON_POSITIVE).performClick()
            }
            gateAwait(scenario, "block is terminal actual storage state") { f.get(peer)!!.state == "blocked" && !it.findViewById<android.widget.Button>(R.id.btn_block).isEnabled }
            gateScreenshot(app, "contact-blocked")
        }
        ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer)).use { scenario ->
            gateAwait(scenario, "blocked chat STOP") { !it.findViewById<android.widget.Button>(R.id.btn_send).isEnabled && it.findViewById<android.widget.TextView>(R.id.info).text.contains(it.getString(R.string.trust_blocked)) }
        }
    }

    /** Real local trust state from a disposable peer's public QR; no message transport override. */
    @Test fun requestedAndMissingKeysUiOnlyInGatePackage() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val f = Core.facade(app)
        assertTrue(f.account().authenticated)
        val input = File(app.filesDir, "gate-trust.qr")
        assertTrue(input.exists())
        try {
            val before = f.contacts(null, 100).first.map { it.contactId }.toSet()
            assertEquals(uniffi.dmsg_core.QrOutcome.ADDED, f.addQr(input.readText().trim()))
            val requested = f.contacts(null, 100).first.single { it.contactId !in before }.contactId
            assertEquals("requested", f.get(requested)!!.state)
            assertTrue(f.get(requested)!!.hasKeys)
            val missing = "NOKEY123ABCD"
            f.request(missing)
            assertFalse(f.get(missing)!!.hasKeys)
            for ((peer, label, shot) in listOf(
                Triple(requested, R.string.trust_requested, "chat-requested"),
                Triple(missing, R.string.trust_no_keys, "chat-no-keys")
            )) {
                ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer)).use { scenario ->
                    gateAwait(scenario, "actual trust warning renders") {
                        it.findViewById<android.widget.TextView>(R.id.info).text.contains(it.getString(label)) && !it.findViewById<android.widget.Button>(R.id.btn_send).isEnabled
                    }
                    gateScreenshot(app, shot)
                }
            }
        } finally { SecureStore.wipe(input) }
    }

    /** Radio is disabled and adb reverse removed by the orchestrator before this gate. */
    @Test fun queueOfflineMessageForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        assumeTrue(File(app.filesDir, "gate-offline-request").exists())
        val f = Core.facade(app)
        val peer = gatePeer(app, f)
        assertFailsWithKind(ErrorKind.Transport) { f.fetch() }
        assertFailsWithKind(ErrorKind.Transport) { f.retry() }
        val mid = f.send(peer, "Paired offline persisted probe.")
        assertEquals("queued", f.outbox(0, 100).first.single { it.mid == mid }.status)
        val record = JSONObject()
            .put("mid", mid).put("ciphertextHash", ciphertextHash(app, mid))
            .put("account", f.account().contactId).put("inboxCount", f.inbox(0, 100).first.size)
        val output = File(app.filesDir, "gate-queued-record")
        assertFalse(output.exists())
        output.writeText(record.toString())
        assertTrue(output.setReadable(false, false)); assertTrue(output.setWritable(false, false))
        assertTrue(output.setReadable(true, true)); assertTrue(output.setWritable(true, true))
    }

    /** Run after actual process death and restored DNS network; do not print ciphertext or its hash. */
    @Test fun retryAndVerifyPreservedCiphertextForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val input = File(app.filesDir, "gate-queued-record")
        assumeTrue(input.exists())
        val record = JSONObject(input.readText())
        val f = Core.facade(app)
        val mid = record.getString("mid")
        assertEquals(record.getString("account"), f.account().contactId)
        assertEquals(record.getInt("inboxCount"), f.inbox(0, 100).first.size)
        assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
        assertEquals("queued", f.outbox(0, 100).first.single { it.mid == mid }.status)
        f.retry()
        assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
        assertEquals("accepted", f.outbox(0, 100).first.single { it.mid == mid }.status)
        input.delete()
        File(app.filesDir, "gate-offline-request").delete()
    }

    /** Operator SIGKILLs this live process; no FGS may consume the queued fixture. */
    @Test fun holdQueuedProcessForSigkillGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val record = privateFixture(File(app.filesDir, "gate-queued-record"))
        val f = Core.facade(app)
        assertFalse("FGS must not retry during the kill handshake", DmsgService.running(app))
        val mid = record.getString("mid")
        assertEquals(record.getString("account"), f.account().contactId)
        assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
        assertEquals(uniffi.dmsg_core.DeliveryState.QUEUED, f.messageStatus(mid))
        gateWrite(app, "gate-kill-ready", android.os.Process.myPid().toString())
        Thread.sleep(120_000)
        fail("operator must SIGKILL the verified live PID within the bounded hold")
    }

    /** Both phones hold a real connection until the operator restarts only dmsg53. */
    @Test fun reconnectQueuedAfterServerRestartForGate() {
        explicitGate()
        val app = ApplicationProvider.getApplicationContext<Context>()
        val record = privateFixture(File(app.filesDir, "gate-queued-record"))
        val ready = File(app.filesDir, "gate-restart-ready")
        val resume = File(app.filesDir, "gate-restart-resume")
        assertFalse("stale restart marker", ready.exists())
        assertFalse("operator may resume only after restart", resume.exists())
        val f = Core.facade(app)
        val before = f.account()
        val profile = f.dnsProfile()!!
        val mid = record.getString("mid")
        try {
            assertFalse(DmsgService.running(app))
            assertEquals(record.getString("account"), before.contactId)
            assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
            assertEquals(uniffi.dmsg_core.DeliveryState.QUEUED, f.messageStatus(mid))
            assertTrue(f.reconnect() > 0L)
            assertEquals("ready", f.dnsStatus())
            gateWrite(app, ready.name, android.os.Process.myPid().toString())
            val deadline = System.nanoTime() + 120_000_000_000L
            while (!resume.exists() && System.nanoTime() < deadline) Thread.sleep(100)
            assertTrue("scoped restart must finish before operator resumes", resume.exists())
            assertEquals(before, f.account())
            assertEquals(profile.fingerprint, f.dnsProfile()!!.fingerprint)
            assertTrue(profile.pub.contentEquals(f.dnsProfile()!!.pub))
            assertEquals(record.getInt("inboxCount"), f.inbox(0, 100).first.size)
            assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
            assertEquals(uniffi.dmsg_core.DeliveryState.QUEUED, f.messageStatus(mid))
            // A stale QUIC connection may fail before the Rust supervisor observes loss.
            // Require bounded eventual recovery through normal commands; never reset the carrier.
            val recoveryDeadline = System.nanoTime() + 90_000_000_000L
            var transientErrors = 0
            while (true) {
                try {
                    assertTrue(f.reconnect() > 0L)
                    break
                } catch (e: DmsgError) {
                    if (e.kind != ErrorKind.Transport) throw e
                    transientErrors++
                    assertEquals(before, f.account())
                    assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
                    assertEquals(uniffi.dmsg_core.DeliveryState.QUEUED, f.messageStatus(mid))
                    assertTrue("normal reconnect must recover within 90 seconds", System.nanoTime() < recoveryDeadline)
                    Thread.sleep(1_000)
                }
            }
            gateWrite(app, "gate-restart-recovery.json", JSONObject().put("transientTransportErrors", transientErrors).toString())
            assertEquals("ready", f.dnsStatus())
            f.retry()
            assertEquals(before, f.account())
            assertEquals(record.getString("ciphertextHash"), ciphertextHash(app, mid))
            assertEquals(uniffi.dmsg_core.DeliveryState.ACCEPTED, f.messageStatus(mid))
            assertEquals(1, f.historyPage(gatePeer(app, f), null, 100).rows.count { it.messageIdHex == mid })
            File(app.filesDir, "gate-queued-record").delete()
        } finally {
            f.dnsStop()
            ready.delete()
            resume.delete()
        }
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
        assertEquals(AccountInfo(true, id), f.account())
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        fun start(type: Class<out android.app.Activity>, peer: String? = null): android.app.Activity =
            instrumentation.startActivitySync(Intent(app, type)
                .putExtra("peer", peer).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        fun waitLabel(activity: android.app.Activity, viewId: Int, expected: String): String {
            val deadline = System.nanoTime() + 30_000_000_000L
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
                // Operator denies CAMERA only for .gate. The real paste control stays usable.
                waitLabel(activity, R.id.result, activity.getString(R.string.camera_denied_code))
                instrumentation.runOnMainSync {
                    activity.findViewById<android.widget.EditText>(R.id.scanner_code).setText(uri)
                    activity.findViewById<android.widget.Button>(R.id.btn_scanner_paste).performClick()
                }
                waitLabel(activity, R.id.result, expected)
                instrumentation.runOnMainSync { assertTrue(activity.findViewById<android.widget.EditText>(R.id.scanner_code).text.isEmpty()) }
            } finally { instrumentation.runOnMainSync { activity.finish() } }
        }
        val before = f.contacts(null, 100).first.size
        for (bad in listOf("garbage", "dmsg://contact/", "dmsg://server/broken", "dmsg://server/" + "x".repeat(9000))) {
            scan(bad, app.getString(R.string.qr_failed, ""))
        }
        assertEquals(before, f.contacts(null, 100).first.size)
        assertEquals(AccountInfo(true, id), f.account())
        scan(f.myQr(), app.getString(R.string.qr_contact_added))
        f.accept(id)
        val changed = UniFfiFacade(seedFixtureDb("changed-ui.db", 9).absolutePath, ByteArray(32) { 4 })
        scan(changed.myQr(), app.getString(R.string.qr_identity_changed))
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
        sendAndExpect(app.getString(R.string.trust_changed))
        assertTrue(f.outbox(0, 100).first.isEmpty())
        val profile = start(ProfileActivity::class.java)
        try {
            instrumentation.runOnMainSync {
                profile.findViewById<android.widget.EditText>(R.id.peer_id).setText(id)
                profile.findViewById<android.widget.Button>(R.id.btn_check).performClick()
            }
            waitLabel(profile, R.id.info, profile.getString(R.string.trust_changed))
            instrumentation.runOnMainSync { profile.findViewById<android.widget.Button>(R.id.btn_confirm).performClick() }
            instrumentation.runOnMainSync {
                val prompt = ProfileActivity::class.java.getDeclaredField("prompt").also { it.isAccessible = true }.get(profile) as androidx.appcompat.app.AlertDialog
                assertTrue("trust change requires separate explicit confirmation", prompt.isShowing)
                assertTrue(f.get(id)?.identityMismatch == true)
                prompt.getButton(android.content.DialogInterface.BUTTON_POSITIVE).performClick()
            }
            waitLabel(profile, R.id.info, profile.getString(R.string.trust_pinned))
            assertEquals(false, f.get(id)?.identityMismatch)
            instrumentation.runOnMainSync { profile.findViewById<android.widget.Button>(R.id.btn_check).performClick() }
            val label = waitLabel(profile, R.id.info, profile.getString(R.string.trust_pinned))
            assertEquals(profile.getString(R.string.trust_pinned), label)
        } finally { instrumentation.runOnMainSync { profile.finish() } }
        sendAndExpect(humanError(app.resources, ffiError(uniffi.dmsg_core.FfiException.Transport("synthetic gate error"))))
        assertTrue(f.outbox(0, 100).first.isEmpty())
    }

    private fun assertFailsWithMessage(needle: String, action: () -> Unit) {
        try { action(); fail("must fail closed") }
        catch (e: DmsgError) { assertTrue("unexpected error ${e.message}", e.message.orEmpty().contains(needle)) }
    }
    private fun assertFailsWithKind(kind: ErrorKind, action: () -> Unit) {
        try { action(); fail("must fail closed") } catch (e: DmsgError) { assertEquals(kind, e.kind) }
    }
}

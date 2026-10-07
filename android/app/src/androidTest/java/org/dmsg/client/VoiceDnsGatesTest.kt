package org.dmsg.client

import android.Manifest
import android.accessibilityservice.AccessibilityServiceInfo
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.database.sqlite.SQLiteDatabase
import android.graphics.Rect
import android.os.Bundle
import android.os.Process
import android.os.SystemClock
import android.system.Os
import android.system.OsConstants
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.Button
import android.widget.ListView
import androidx.core.content.ContextCompat
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONArray
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.MessageKind
import java.io.File
import java.io.FileOutputStream
import java.util.Locale

/**
 * Operator-selected fresh server6/core9 gates over the fixture's actual UDP DNS resolver.
 * This controlled resolver is not default-network/ISP delegation evidence. No TCP fallback.
 * Sequence: bootstrap -> host invites/accepts phone QR -> accept peer -> host fetches/accepts reciprocal
 * contact request -> record two -> host fetch/download
 * -> self hide -> everyone delete -> host fetch control + send/upload 12 s voice -> first chunk
 * -> NEW instrumentation process resumes through UI -> reopen (host has fetched both own notes/control).
 * Inputs: owner-only gate-voice-auth.json and gate-voice-peer.qr in the target's files/.
 * The operator grants microphone permission before the recording method and restores it after
 * instrumentation exits: revoking it from this process kills the runner and hides its result.
 * Outputs below contain public IDs/QR and bounded metadata only, never credentials/audio/keys/hashes.
 */
@RunWith(AndroidJUnit4::class)
class VoiceDnsGatesTest {
    @get:Rule val testName = TestName()
    private val app get() = ApplicationProvider.getApplicationContext<Context>()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private var previousAccessibilityFlags: Int? = null

    @Before fun gate() {
        assertTrue("isolated package required", app.packageName.endsWith(".gate"))
        assertEquals("select exactly one operator method", listOf("${javaClass.name}#${testName.methodName}"),
            InstrumentationRegistry.getArguments().getString("class").orEmpty().split(','))
        assertFalse("DNS FGS must be off", DmsgService.running(app))
        val service = instrumentation.uiAutomation.serviceInfo
        previousAccessibilityFlags = service.flags
        service.flags = service.flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        instrumentation.uiAutomation.serviceInfo = service
    }
    @After fun restoreAccessibility() {
        previousAccessibilityFlags?.let { flags ->
            val service = instrumentation.uiAutomation.serviceInfo
            service.flags = flags; instrumentation.uiAutomation.serviceInfo = service
        }
    }
    private class Auth(val serverCode: String, val resolvers: List<String>, val login: String,
        val password: String, val invitation: String?)
    private fun read(name: String): String {
        val file = File(app.filesDir, name)
        val stat = Os.lstat(file.absolutePath)
        assertTrue("owner-only regular fixture required: $name", OsConstants.S_ISREG(stat.st_mode) &&
            stat.st_uid == Process.myUid() && (stat.st_mode and 511) in listOf(256, 384) && stat.st_size in 1..16_384) // 0400/0600
        return file.readText(Charsets.UTF_8)
    }
    private fun json(name: String): JSONObject = try { JSONObject(read(name)) }
        catch (_: Exception) { throw AssertionError("Invalid bounded fixture JSON: $name") }
    private fun auth(): Auth = try {
        val input = json("gate-voice-auth.json")
        val addresses = input.getJSONArray("resolvers")
        val resolvers = (0 until addresses.length()).map { addresses.getString(it) }
        val result = Auth(input.getString("serverCode").trim(), resolvers, input.getString("login"),
            input.getString("password"), if (input.isNull("invitation")) null else input.getString("invitation").trim())
        require(result.serverCode.isNotEmpty() && resolvers.size in 1..8 && resolvers.all { it.length in 1..128 })
        require(result.login.isNotEmpty() && result.password.isNotEmpty())
        result
    } catch (_: Exception) { throw AssertionError("Invalid voice auth fixture") } // Never reflect secret JSON values.
    private fun write(name: String, value: String) {
        val bytes = value.toByteArray(Charsets.UTF_8)
        require(bytes.size in 1..16_384)
        val fd = Os.open(File(app.filesDir, name).absolutePath,
            OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_TRUNC or OsConstants.O_NOFOLLOW, 384)
        FileOutputStream(fd).use { output -> Os.fchmod(fd, 384); output.write(bytes); output.fd.sync() }
    }
    private fun write(name: String, value: JSONObject) = write(name, value.toString())
    private fun waitFor(label: String, timeout: Long = 90_000, condition: () -> Boolean) {
        val deadline = SystemClock.uptimeMillis() + timeout
        while (SystemClock.uptimeMillis() < deadline) { if (condition()) return; SystemClock.sleep(75) }
        fail(label)
    }
    private fun assertManualProfile(f: DmsgFacade, fixture: Auth) {
        val preview = f.profilePreview(fixture.serverCode)
        val profile = requireNotNull(f.dnsProfile())
        assertEquals(preview.first, profile.domain); assertEquals(preview.second, profile.fingerprint)
        assertEquals("controlled UDP resolver must remain explicit", fixture.resolvers, profile.resolvers)
    }
    /** Observe/apply the real Network first; subsequent unchanged observations preserve this override. */
    private fun preset(): UniFfiFacade {
        assertTrue("bootstrap the fresh DNS account first", Core.dbFile(app).isFile)
        val fixture = auth()
        val f = Core.facade(app) as? UniFfiFacade ?: throw AssertionError("Real native facade required")
        f.refreshDnsNetwork { true }
        f.dnsNetworkChanged(fixture.resolvers)
        assertManualProfile(f, fixture)
        assertTrue(f.account().authenticated)
        assertEquals(json("gate-voice-account.json").getString("ownCid"), f.account().contactId)
        return f
    }
    private fun peer(f: DmsgFacade): String {
        val cid = read("gate-voice-peer.cid").trim()
        assertEquals("peer CID must be parsed by native validation", f.contactQrId(read("gate-voice-peer.qr").trim()), cid)
        assertEquals(json("gate-voice-peer.json").getString("peerCid"), cid)
        assertEquals(ContactCta.Chat, contactCta(f.get(cid)))
        return cid
    }
    private fun rows(f: DmsgFacade, cid: String): List<HistoryMessage> {
        val page = f.historyPage(cid, null, 100)
        assertNull("small fresh fixture must not truncate history", page.nextBeforeLocalId)
        return page.rows
    }
    private fun snapshot(row: HistoryMessage): JSONObject {
        val voice = requireNotNull(row.voice)
        assertEquals(MessageKind.VOICE, row.kind); assertEquals("", row.text)
        assertNotNull(row.serverSeq); assertNotNull(row.serverTimestampMs)
        assertTrue(voice.sampleCount in 1u..960_000u && voice.byteLen in 1u..131_072u)
        assertEquals(64, voice.waveform.size)
        return JSONObject().put("id", row.localId).put("mid", row.messageIdHex).put("samples", voice.sampleCount.toLong())
            .put("bytes", voice.byteLen.toLong()).put("localTimestampMs", row.localTimestampMs)
            .put("serverSeq", row.serverSeq).put("serverTimestampMs", row.serverTimestampMs)
            .put("direction", row.direction.name).put("downloaded", voice.downloaded)
    }
    private fun original(row: HistoryMessage, proof: JSONObject) {
        assertEquals(proof.getLong("id"), row.localId); assertEquals(proof.getString("mid"), row.messageIdHex)
        assertEquals(MessageKind.VOICE, row.kind); assertEquals(proof.getString("direction"), row.direction.name)
        assertEquals(proof.getLong("localTimestampMs"), row.localTimestampMs)
        assertEquals(proof.getLong("serverSeq"), row.serverSeq); assertEquals(proof.getLong("serverTimestampMs"), row.serverTimestampMs)
        if (messageVisible(row)) {
            val voice = requireNotNull(row.voice)
            assertEquals(proof.getLong("samples"), voice.sampleCount.toLong()); assertEquals(proof.getLong("bytes"), voice.byteLen.toLong())
        } else { assertEquals("", row.text); assertNull(row.voice) }
    }
    private fun envelope(f: DmsgFacade, cid: String) = JSONObject().put("ownCid", f.account().contactId).put("peerCid", cid)
    private fun sent(index: Int) = json("gate-voice-sent.json").getJSONArray("rows").getJSONObject(index)
    private fun views(view: View): List<View> = listOf(view) + if (view is ViewGroup)
        (0 until view.childCount).flatMap { views(view.getChildAt(it)) } else emptyList()
    private fun nodes(node: AccessibilityNodeInfo?): List<AccessibilityNodeInfo> = if (node == null) emptyList()
        else listOf(node) + (0 until node.childCount).flatMap { nodes(node.getChild(it)) }
    private fun labelled(label: String): AccessibilityNodeInfo? {
        val roots = instrumentation.uiAutomation.windows.map { it.root } + instrumentation.uiAutomation.rootInActiveWindow
        return roots.flatMap(::nodes).firstOrNull { it.text?.toString() == label || it.contentDescription?.toString() == label }
    }
    private fun clickLabel(resource: Int) {
        val label = app.getString(resource)
        waitFor("accessible dialog control unavailable", 20_000) { labelled(label) != null }
        assertTrue(labelled(label)!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }
    private fun launch(cid: String) = ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java)
        .putExtra("peer", cid).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    private fun memory(scenario: ActivityScenario<ChatActivity>, inspect: (ChatMemory) -> Unit) =
        scenario.onActivity { inspect(ViewModelProvider(it)[ChatMemory::class.java]) }
    private fun ready(scenario: ActivityScenario<ChatActivity>) = waitFor("native chat did not become ready", 20_000) {
        var result = false
        scenario.onActivity { result = it.hasWindowFocus() && it.findViewById<Button>(R.id.btn_history_retry).isEnabled &&
            it.findViewById<VoiceMicView>(R.id.btn_mic).isEnabled }
        result
    }
    private fun displayed(scenario: ActivityScenario<ChatActivity>, id: Long, action: (View) -> Unit) {
        scenario.onActivity {
            val list = it.findViewById<ListView>(R.id.messages)
            list.setSelection((0 until list.adapter.count).first { p -> list.adapter.getItemId(p) == id })
        }
        waitFor("stable voice row not laid out", 20_000) {
            var result = false
            scenario.onActivity {
                val list = it.findViewById<ListView>(R.id.messages)
                val root = (0 until list.childCount).map(list::getChildAt).firstOrNull { child -> child.tag == id }
                if (root != null && root.getGlobalVisibleRect(Rect())) { action(root); result = true }
            }; result
        }
    }
    private fun click(scenario: ActivityScenario<ChatActivity>, id: Int) = scenario.onActivity {
        val view = it.findViewById<View>(id)
        assertTrue(view.isEnabled && view.isShown && view.height >= NativeUi.dp(it, 48)); assertTrue(view.performClick())
    }
    private fun voiceButton(scenario: ActivityScenario<ChatActivity>, id: Long, label: Int) {
        var clicked = false
        waitFor("native voice button not ready", 20_000) {
            displayed(scenario, id) { root ->
                val button = views(root).filterIsInstance<Button>().singleOrNull { it.text.toString() == app.getString(label) }
                if (button != null && button.isEnabled && button.isShown) {
                    assertTrue(button.height >= NativeUi.dp(app, 48)); assertTrue(button.performClick()); clicked = true
                }
            }
            clicked
        }
    }
    private fun touch(down: Long, action: Int, x: Float, y: Float) {
        val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, x, y, 0)
        event.source = InputDevice.SOURCE_TOUCHSCREEN
        try { instrumentation.sendPointerSync(event) } finally { event.recycle() }
    }
    private fun openDelete(scenario: ActivityScenario<ChatActivity>, id: Long) {
        ready(scenario)
        displayed(scenario, id) { assertTrue(it.performLongClick()) }
        assertNull("voice must have no Edit menu", labelled(app.getString(R.string.edit_whole_message)))
        clickLabel(R.string.delete_whole_message)
        waitFor("default self-only choice unavailable", 20_000) { labelled(app.getString(R.string.delete_self)) != null }
        assertTrue("SelfOnly must be the default", labelled(app.getString(R.string.delete_self))!!.isChecked)
    }
    private fun absent(scenario: ActivityScenario<ChatActivity>, id: Long) = waitFor("deleted voice left a placeholder", 20_000) {
        var result = false
        scenario.onActivity { val adapter = it.findViewById<ListView>(R.id.messages).adapter
            result = (0 until adapter.count).none { adapter.getItemId(it) == id } }
        result
    }
    private fun chunkBytes(id: Long): Long = SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null,
        SQLiteDatabase.OPEN_READONLY).use { db ->
        db.rawQuery("SELECT coalesce(sum(length(ciphertext)),0) FROM core_blob_chunks WHERE local_id=? AND confirmed=1", arrayOf(id.toString()))
            .use { cursor -> assertTrue(cursor.moveToFirst()); cursor.getLong(0) }
    }

    @Test fun bootstrapFreshDnsVoiceAccount() {
        assertFalse("fresh gate only; parent must clear the disposable local fixture", Core.dbFile(app).exists())
        assertFalse(File(app.filesDir, "core.db.sealed").exists())
        val fixture = auth()
        System.loadLibrary("dmsg_core")
        val key = SecureStore.key(app)
        val f = try { UniFfiFacade(Core.dbFile(app).absolutePath, key, null) } finally { key.fill(0) }
        assertFalse(f.account().authenticated)
        f.configureDns(fixture.serverCode, fixture.resolvers)
        assertTrue(f.signupDns(fixture.login, fixture.password, fixture.invitation).authenticated)
        assertTrue("real DNS reconnect/prekey publication required", f.reconnect() > 0)
        assertManualProfile(f, fixture)
        val qr = f.myQr()
        assertEquals(f.account().contactId, f.contactQrId(qr))
        write("gate-voice-phone.qr", qr)
        write("gate-voice-account.json", JSONObject().put("ownCid", f.account().contactId).put("registered", true))
    }

    @Test fun acceptFreshPeer() {
        val f = preset()
        val qr = read("gate-voice-peer.qr").trim()
        val cid = f.contactQrId(qr) // Native Crockford/QR validation, never a fabricated fixture CID.
        assertNotEquals(f.account().contactId, cid)
        f.inviteQr(qr)
        assertTrue(f.reconnect() > 0)
        assertEquals(ContactCta.Chat, contactCta(f.get(cid)))
        val fetched = f.fetch() // Processes/accepts the fresh host's reciprocal public contact request.
        assertTrue("voice payloads belong to later methods", fetched.received.isEmpty())
        assertTrue(fetched.skipped.all { it == 0L })
        write("gate-voice-peer.cid", cid)
        write("gate-voice-peer.json", envelope(f, cid).put("accepted", true))
        assertManualProfile(f, auth())
    }

    @Test fun recordAndSendTwoVoicesThroughUi() {
        val f = preset(); val cid = peer(f)
        assertTrue("fresh peer history required; never duplicate a previous send", rows(f, cid).isEmpty())
        assertEquals("operator must grant microphone permission before instrumentation",
            PackageManager.PERMISSION_GRANTED, ContextCompat.checkSelfPermission(app, Manifest.permission.RECORD_AUDIO))
        val proofs = JSONArray()
        launch(cid).use { scenario ->
            ready(scenario)
            for (index in 0..1) {
                if (index == 0) {
                    val rect = Rect()
                    scenario.onActivity { assertTrue(it.findViewById<View>(R.id.btn_mic).getGlobalVisibleRect(rect)) }
                    val down = SystemClock.uptimeMillis()
                    touch(down, MotionEvent.ACTION_DOWN, rect.centerX().toFloat(), rect.centerY().toFloat())
                    SystemClock.sleep(2_000)
                    memory(scenario) { assertEquals(VoiceMode.Holding, it.voice.mode); assertTrue(it.voice.samples >= 16_000) }
                    touch(down, MotionEvent.ACTION_UP, rect.centerX().toFloat(), rect.centerY().toFloat())
                } else {
                    click(scenario, R.id.btn_mic) // Accessible hands-free lock, then explicit preview/Send.
                    SystemClock.sleep(3_000)
                    memory(scenario) { assertEquals(VoiceMode.Locked, it.voice.mode); assertTrue(it.voice.samples >= 32_000) }
                    click(scenario, R.id.voice_preview)
                    waitFor("real microphone preview did not finish", 20_000) {
                        var result = false; memory(scenario) { result = it.voice.mode == VoiceMode.Preview && it.voice.bytes != null }; result
                    }
                    assertEquals("preview alone must not send", 1, rows(f, cid).size)
                    click(scenario, R.id.voice_send)
                }
                var accepted: HistoryMessage? = null
                waitFor("UI voice did not become canonical Accepted over UDP DNS") {
                    val own = rows(f, cid).filter { it.direction == MessageDirection.OUTGOING && it.kind == MessageKind.VOICE }.sortedBy { it.localId }
                    accepted = own.getOrNull(index)
                    own.size == index + 1 && accepted?.deliveryState == DeliveryState.ACCEPTED
                }
                val row = requireNotNull(accepted)
                assertFalse(canEditMessage(row, f.get(cid))); assertEquals(0uL, row.revision)
                displayed(scenario, row.localId) { assertFalse(it.performAccessibilityAction(R.id.action_edit, null)) }
                proofs.put(snapshot(row))
                waitFor("saved queue must clear memory-only preview", 20_000) {
                    var result = false; memory(scenario) { result = !it.pending && it.voice.mode == VoiceMode.Idle && it.voice.bytes == null }; result
                }
                ready(scenario)
            }
            val timeline = f.timelinePage(cid, null, 100).rows.filter(::messageVisible)
            // Native pages are newest-first; the visible chat reverses them into timeline order.
            assertEquals(proofs.getJSONObject(1).getLong("id"), timeline[0].localId)
            assertEquals(proofs.getJSONObject(0).getLong("id"), timeline[1].localId)
            assertTrue(timeline[0].serverSeq!! > timeline[1].serverSeq!!)
            assertTrue(timeline[0].serverTimestampMs!! >= timeline[1].serverTimestampMs!!)
            scenario.onActivity { activity ->
                val adapter = activity.findViewById<ListView>(R.id.messages).adapter
                assertEquals(2, adapter.count)
                assertEquals(proofs.getJSONObject(0).getLong("id"), adapter.getItemId(0))
                assertEquals(proofs.getJSONObject(1).getLong("id"), adapter.getItemId(1))
            }
        }
        write("gate-voice-sent.json", envelope(f, cid).put("rows", proofs))
        assertManualProfile(f, auth())
    }

    @Test fun resumeInterruptedOwnVoiceUploadThroughUi() {
        val f = preset(); val cid = peer(f)
        val queued = rows(f, cid).filter { it.kind == MessageKind.VOICE &&
            it.direction == MessageDirection.OUTGOING && it.deliveryState == DeliveryState.QUEUED }
        assertTrue("operator must select a genuinely interrupted voice queue", queued.isNotEmpty())
        launch(cid).use { scenario ->
            ready(scenario)
            waitFor("reopened UI did not deliver the saved voice manifest") {
                queued.all { f.historyMessage(cid, it.localId).deliveryState == DeliveryState.ACCEPTED }
            }
        }
        val proofs = JSONArray()
        queued.forEach { before ->
            val after = f.historyMessage(cid, before.localId)
            assertEquals(before.messageIdHex, after.messageIdHex)
            assertEquals(before.localId, after.localId)
            assertEquals(before.localTimestampMs, after.localTimestampMs)
            assertEquals(before.voice!!.sampleCount, after.voice!!.sampleCount)
            proofs.put(snapshot(after))
        }
        write("gate-voice-resumed-own.json", envelope(f, cid).put("rows", proofs))
    }

    @Test fun selfDeleteDefaultVoiceThroughUi() {
        val f = preset(); val cid = peer(f); val proof = sent(0); val id = proof.getLong("id")
        val before = f.historyMessage(cid, id); original(before, proof)
        assertTrue(messageVisible(before)); assertFalse(canEditMessage(before, f.get(cid)))
        launch(cid).use { scenario ->
            openDelete(scenario, id); clickLabel(R.string.cancel)
            assertFalse(f.historyMessage(cid, id).hiddenSelf)
            openDelete(scenario, id); clickLabel(R.string.delete_confirm)
            waitFor("self-only voice hide not committed") { f.historyMessage(cid, id).hiddenSelf }
            absent(scenario, id)
        }
        val hidden = f.historyMessage(cid, id); original(hidden, proof)
        assertTrue(hidden.hiddenSelf); assertFalse(hidden.deletedAll); assertEquals(0uL, hidden.revision)
        assertNull(hidden.changeDeliveryState)
        write("gate-voice-self-delete.json", envelope(f, cid).put("id", id).put("mid", hidden.messageIdHex).put("hiddenSelf", true))
        assertManualProfile(f, auth())
    }

    /** The control is durable; its Queued phase can finish before a read-only observer samples it. */
    private fun observeSavedDelete(mid: String, confirm: () -> Unit) {
        confirm()
        waitFor("actual durable delete control was not saved", 10_000) {
            SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { db ->
                db.rawQuery("SELECT count(*) FROM core_messages WHERE kind='delete' AND hex(target_mid)=? " +
                    "AND delivery_state IN ('queued','accepted','delivered')", arrayOf(mid.uppercase(Locale.ROOT))).use { cursor ->
                    assertTrue(cursor.moveToFirst()); cursor.getLong(0) == 1L
                }
            }
        }
    }

    @Test fun everyoneDeleteVoiceThroughUi() {
        val f = preset(); val cid = peer(f); val proof = sent(1); val id = proof.getLong("id")
        val before = f.historyMessage(cid, id); original(before, proof)
        assertTrue(canChangeMessage(before, f.get(cid))); assertFalse(canEditMessage(before, f.get(cid)))
        assertTrue(before.voice!!.downloaded)
        launch(cid).use { scenario ->
            ready(scenario); voiceButton(scenario, id, R.string.voice_play)
            waitFor("real AudioTrack did not start the own note", 20_000) {
                var result = false; memory(scenario) { result = it.audio?.playback?.key == VoiceKey(before) && it.audio!!.playback.playing && it.audio!!.playback.sample > 0 }; result
            }
            openDelete(scenario, id); clickLabel(R.string.delete_everyone)
            observeSavedDelete(before.messageIdHex) {
                memory(scenario) { assertTrue("delete must begin while the matching player is active", it.audio!!.playback.playing) }
                clickLabel(R.string.delete_confirm)
            }
            waitFor("everyone voice delete not committed") { f.historyMessage(cid, id).deletedAll }
            absent(scenario, id)
            memory(scenario) { assertFalse("deletion must stop the matching player", it.audio!!.playback.playing) }
            waitFor("UI command writer did not finish", 20_000) {
                var result = false; memory(scenario) { result = !it.pending }; result
            }
        }
        f.retry() // Existing queued control/status lane, still DNS; no direct transport or store mutation.
        val deleted = f.historyMessage(cid, id); original(deleted, proof)
        assertTrue(deleted.deletedAll); assertEquals(1uL, deleted.revision)
        assertTrue(deleted.changeDeliveryState in listOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED))
        write("gate-voice-everyone-delete.json", envelope(f, cid).put("id", id).put("mid", deleted.messageIdHex)
            .put("queuedObserved", true).put("revision", 1).put("changeState", deleted.changeDeliveryState!!.name))
        assertManualProfile(f, auth())
    }

    @Test fun receiveAndCommitFirstDownloadChunk() {
        val f = preset(); val cid = peer(f)
        assertTrue("one fresh host voice is expected", rows(f, cid).none { it.direction == MessageDirection.INCOMING })
        waitFor("isolated host voice not received through DNS") {
            val fetched = f.fetch(); assertTrue("no dropped E2E rows", fetched.skipped.all { it == 0L })
            fetched.received.forEach { assertEquals(MessageKind.VOICE, it.kind); assertEquals(cid, it.contactId) }
            rows(f, cid).any { it.direction == MessageDirection.INCOMING && it.kind == MessageKind.VOICE }
        }
        val incoming = rows(f, cid).single { it.direction == MessageDirection.INCOMING }
        assertEquals(MessageKind.VOICE, incoming.kind); assertEquals(192_000u, incoming.voice!!.sampleCount)
        assertTrue("host fixture must genuinely span several encrypted chunks", incoming.voice!!.byteLen > 8192u)
        assertFalse(incoming.voice!!.downloaded); assertEquals(0L, chunkBytes(incoming.localId))
        launch(cid).use { scenario ->
            ready(scenario); SystemClock.sleep(3_500)
            assertFalse("foreground chat must not auto-download", f.historyMessage(cid, incoming.localId).voice!!.downloaded)
            assertEquals(0L, chunkBytes(incoming.localId))
        }
        val transfer = f.prepareVoiceTransfer(cid, incoming.localId, true)
        try {
            val status = transfer.advance(); assertEquals(0u, status.transferred); assertFalse(status.complete)
            assertEquals(incoming.voice!!.byteLen, status.total)
            assertEquals(status, transfer.commit()); assertEquals(0L, chunkBytes(incoming.localId))
            val first = transfer.advance(); val committed = transfer.commit()
            assertEquals(first, committed)
            assertTrue(committed.transferred > 0u && committed.transferred < committed.total); assertFalse(committed.complete)
            assertEquals(committed.transferred.toLong(), chunkBytes(incoming.localId))
            assertFalse(f.historyMessage(cid, incoming.localId).voice!!.downloaded)
            write("gate-voice-download.json", envelope(f, cid).put("row", snapshot(incoming))
                .put("firstChunkBytes", committed.transferred.toLong()).put("totalBytes", committed.total.toLong()))
        } finally { transfer.cancel() } // This exact method must end; resume is a new instrumentation process.
        assertManualProfile(f, auth())
    }

    @Test fun resumeDownloadAndPlaybackThroughUi() {
        val f = preset(); val cid = peer(f); val partial = json("gate-voice-download.json")
        val proof = partial.getJSONObject("row"); val id = proof.getLong("id")
        val incoming = f.historyMessage(cid, id); original(incoming, proof)
        assertFalse(incoming.voice!!.downloaded); assertEquals(partial.getLong("firstChunkBytes"), chunkBytes(id))
        var played = 0
        val seek = incoming.voice!!.sampleCount.toInt() / 2
        launch(cid).use { scenario ->
            ready(scenario); SystemClock.sleep(3_500)
            assertEquals("no resume before the manual Download tap", partial.getLong("firstChunkBytes"), chunkBytes(id))
            assertFalse(f.historyMessage(cid, id).voice!!.downloaded)
            voiceButton(scenario, id, R.string.voice_download)
            waitFor("manual UI download did not resume to completion over DNS") { f.historyMessage(cid, id).voice!!.downloaded }
            val progress = VoiceTransferCoordinator.state(VoiceKey(incoming))
            assertTrue(progress.complete); assertEquals(incoming.voice!!.byteLen, progress.total); assertEquals(progress.total, progress.transferred)
            assertEquals(progress.total.toLong(), chunkBytes(id))
            voiceButton(scenario, id, R.string.voice_play)
            waitFor("downloaded native AudioTrack did not play", 20_000) {
                var result = false; memory(scenario) { val p = it.audio!!.playback
                    result = p.key == VoiceKey(incoming) && p.playing && p.total == incoming.voice!!.sampleCount.toInt() && p.sample > 0 }; result
            }
            displayed(scenario, id) { root ->
                val wave = views(root).filterIsInstance<VoiceWaveformView>().single()
                val args = Bundle().apply { putFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE, seek.toFloat()) }
                assertTrue(wave.performAccessibilityAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_PROGRESS.id, args))
            }
            waitFor("native decoder/AudioTrack seek did not advance", 20_000) {
                var result = false; memory(scenario) { val p = it.audio!!.playback; played = p.sample
                    result = p.key == VoiceKey(incoming) && p.playing && p.total == incoming.voice!!.sampleCount.toInt() && p.sample > seek }; result
            }
            voiceButton(scenario, id, R.string.voice_pause)
            memory(scenario) { assertFalse(it.audio!!.playback.playing) }
            scenario.recreate(); ready(scenario)
            memory(scenario) { assertFalse("recreate must not resume audio", it.audio!!.playback.playing) }
        }
        val completed = f.historyMessage(cid, id); original(completed, proof); assertTrue(completed.voice!!.downloaded)
        write("gate-voice-playback.json", envelope(f, cid).put("row", snapshot(completed)).put("seekSample", seek)
            .put("playedSample", played).put("firstChunkBytes", partial.getLong("firstChunkBytes")))
        assertManualProfile(f, auth())
    }

    @Test fun reopenVerifyVoiceState() {
        val f = preset(); val cid = peer(f)
        assertTrue("real saved-key DNS resume required", f.reconnect() > 0)
        f.retry() // Parent must have fetched the two base notes AND delete control first.
        val self = f.historyMessage(cid, sent(0).getLong("id")); original(self, sent(0))
        val deleted = f.historyMessage(cid, sent(1).getLong("id")); original(deleted, sent(1))
        assertEquals(self.messageIdHex, json("gate-voice-self-delete.json").getString("mid"))
        assertEquals(deleted.messageIdHex, json("gate-voice-everyone-delete.json").getString("mid"))
        assertTrue(self.hiddenSelf); assertFalse(self.deletedAll); assertEquals(0uL, self.revision)
        assertTrue(deleted.deletedAll); assertEquals(1uL, deleted.revision)
        assertNull(self.voice); assertNull(deleted.voice)
        assertEquals(DeliveryState.DELIVERED, self.deliveryState); assertEquals(DeliveryState.DELIVERED, deleted.deliveryState)
        assertEquals(DeliveryState.DELIVERED, deleted.changeDeliveryState)
        val playback = json("gate-voice-playback.json").getJSONObject("row")
        val incoming = f.historyMessage(cid, playback.getLong("id")); original(incoming, playback)
        assertTrue(incoming.voice!!.downloaded)
        assertEquals(listOf(incoming.localId), f.timelinePage(cid, null, 100).rows.filter(::messageVisible).map { it.localId })
        write("gate-voice-reopen.json", envelope(f, cid).put("hiddenMid", self.messageIdHex).put("deletedMid", deleted.messageIdHex)
            .put("incomingMid", incoming.messageIdHex).put("baseState", "DELIVERED").put("changeState", "DELIVERED").put("downloaded", true))
        assertManualProfile(f, auth())
    }
}

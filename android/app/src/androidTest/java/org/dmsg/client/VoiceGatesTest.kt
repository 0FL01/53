package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.Intent
import android.database.sqlite.SQLiteDatabase
import android.graphics.Rect
import android.os.SystemClock
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.ViewConfiguration
import android.view.InputDevice
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.Button
import android.widget.ListView
import android.widget.TextView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import uniffi.dmsg_core.DmsgClient
import uniffi.dmsg_core.MessageKind

/** Exact-method, fresh .gate, real JNI/core/microphone/player/UI. Network evidence is a separate gate. */
@RunWith(AndroidJUnit4::class)
class VoiceGatesTest {
    @get:Rule val testName = TestName()
    private val app get() = ApplicationProvider.getApplicationContext<Context>()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val own = "A0123456789B"
    private val peer = "B0123456789C"
    private fun gate() {
        assertTrue("isolated package required", app.packageName.endsWith(".gate"))
        val selected = InstrumentationRegistry.getArguments().getString("class").orEmpty().split(',')
        assertTrue("select exactly this native gate", "${javaClass.name}#${testName.methodName}" in selected)
        assertFalse("DNS FGS must be off for local lifecycle evidence", DmsgService.running(app))
    }
    private fun waitFor(label: String, predicate: () -> Boolean) {
        val deadline = SystemClock.uptimeMillis() + 20_000
        while (SystemClock.uptimeMillis() < deadline) { if (predicate()) return; Thread.sleep(75) }
        fail(label)
    }
    private fun sampleNote(): ByteArray {
        System.loadLibrary("dmsg_core")
        var encoder = VoiceCodecJni.nativeEncoderCreate()
        try {
            repeat(10) { batch ->
                val pcm = ShortArray(1600) { i -> (kotlin.math.sin((batch * 1600 + i) * 2.0 * Math.PI * 220 / 16000) * 8000).toInt().toShort() }
                try { assertEquals((batch + 1) * 1600, VoiceCodecJni.nativeEncoderPush(encoder, pcm)) } finally { pcm.fill(0) }
            }
            val handle = encoder; encoder = 0
            return VoiceCodecJni.nativeEncoderFinish(handle)
        } finally { if (encoder != 0L) VoiceCodecJni.nativeEncoderCancel(encoder) }
    }
    @Test fun seedFreshVoiceChatOnlyInGatePackage() {
        gate()
        val file = Core.dbFile(app)
        assertFalse("fresh fixture account only; never overwrite an existing store", file.exists())
        val key = SecureStore.key(app)
        val note = sampleNote()
        try {
            DmsgClient.openEncrypted(file.absolutePath, key).use { it.accountInfo() }
            SQLiteDatabase.openDatabase(file.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
                db.beginTransaction()
                try {
                    db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(sealedFixtureValue(key, "device_priv", ByteArray(32) { 7 })))
                    db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, own))
                    db.execSQL("INSERT INTO core_contacts(contact_id,state,user_id,device_key,ed_identity,curve_identity) VALUES(?,'accepted',?,?,?,?)",
                        arrayOf(peer, ByteArray(16) { 3 }, ByteArray(32) { 2 }, ByteArray(32) { 4 }, ByteArray(32) { 5 }))
                    seedIncomingVoiceFixture(db, key, peer, ByteArray(16) { 6 }, VoiceCodecJni.nativeNoteSamples(note), note.size, VoiceCodecJni.nativeNoteWaveform(note))
                    db.execSQL("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,text,local_timestamp_ms,server_seq,server_timestamp_ms) VALUES(?,?,?,'incoming','text',?,2,2,2)",
                        arrayOf(ByteArray(16) { 8 }, ByteArray(32) { 2 }, peer, sealedFixtureValue(key, "message_text", "Selectable text beside voice".toByteArray())))
                    db.setTransactionSuccessful()
                } finally { db.endTransaction() }
                db.rawQuery("PRAGMA user_version", null).use { assertTrue(it.moveToFirst()); assertEquals(10, it.getInt(0)) }
                db.rawQuery("SELECT media_manifest FROM core_messages WHERE kind='voice'", null).use {
                    assertTrue(it.moveToFirst()); assertEquals("DMSG-S1", String(it.getBlob(0).copyOfRange(0, 7), Charsets.US_ASCII))
                }
            }
            val f = Core.facade(app)
            assertTrue(f.account().authenticated)
            assertEquals(own, f.account().contactId)
            val voice = f.historyPage(peer, null, 50).rows.single { it.kind == MessageKind.VOICE }
            assertEquals(16_000u, voice.voice!!.sampleCount); assertFalse(voice.voice!!.downloaded)
            assertEquals(64, voice.voice!!.waveform.size)
            assertEquals(MessageKind.TEXT, f.dialogsPage(null, 50).rows.single().previewKind)
            assertEquals(2, f.inbox(0, 50).first.size)
        } finally { key.fill(0); note.fill(0) }
    }
    private fun launch() = ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java)
        .putExtra("peer", peer).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    private fun ready(scenario: ActivityScenario<ChatActivity>) = waitFor("native chat did not load") {
        var ready = false
        scenario.onActivity { ready = it.findViewById<ListView>(R.id.messages).adapter.count == 2 && it.findViewById<VoiceMicView>(R.id.btn_mic).isEnabled }
        ready
    }
    private fun memory(scenario: ActivityScenario<ChatActivity>, inspect: (ChatMemory) -> Unit) {
        scenario.onActivity { inspect(ViewModelProvider(it)[ChatMemory::class.java]) }
    }
    private fun click(scenario: ActivityScenario<ChatActivity>, id: Int) = scenario.onActivity {
        val button = it.findViewById<View>(id)
        assertTrue(button.isEnabled && button.visibility == View.VISIBLE)
        assertTrue("48 dp access target", button.height >= NativeUi.dp(it, 48))
        assertTrue(button.performClick())
    }
    private fun hold(scenario: ActivityScenario<ChatActivity>, lock: Boolean): Pair<Long, Rect> {
        val rect = Rect()
        scenario.onActivity { assertTrue(it.findViewById<View>(R.id.btn_mic).getGlobalVisibleRect(rect)) }
        val time = SystemClock.uptimeMillis()
        touch(time, MotionEvent.ACTION_DOWN, rect.centerX().toFloat(), rect.centerY().toFloat())
        Thread.sleep(600)
        if (lock) {
            val threshold = NativeUi.dp(app, 80)
            touch(time, MotionEvent.ACTION_MOVE, rect.centerX().toFloat(), rect.centerY() - threshold.toFloat())
            touch(time, MotionEvent.ACTION_UP, rect.centerX().toFloat(), rect.centerY() - threshold.toFloat())
        }
        return Pair(time, rect)
    }
    private fun touch(down: Long, action: Int, x: Float, y: Float) {
        val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, x, y, 0)
        event.source = InputDevice.SOURCE_TOUCHSCREEN
        try { instrumentation.sendPointerSync(event) } finally { event.recycle() }
    }

    @Test fun microphonePreviewPlayerLifecycleOnlyInGatePackage() {
        gate()
        assertTrue("run fresh native seed first", Core.facade(app).account().authenticated)
        instrumentation.uiAutomation.grantRuntimePermission(app.packageName, Manifest.permission.RECORD_AUDIO)
        val before = Core.facade(app).historyPage(peer, null, 50).rows.size
        launch().use { scenario ->
            ready(scenario)
            hold(scenario, true)
            waitFor("mic did not capture PCM or lock") {
                var result = false; memory(scenario) { result = it.voice.mode == VoiceMode.Locked && it.voice.samples > 0 }; result
            }
            click(scenario, R.id.voice_pause)
            waitFor("pause did not retain an encoded snapshot") {
                var result = false; memory(scenario) { result = it.voice.mode == VoiceMode.Paused && it.voice.bytes != null }; result
            }
            var paused = 0
            memory(scenario) { paused = it.voice.samples; assertEquals(paused, VoiceCodecJni.nativeNoteSamples(it.voice.bytes!!)) }
            click(scenario, R.id.voice_pause)
            waitFor("resume did not add real microphone samples") {
                var result = false; memory(scenario) { result = it.voice.mode == VoiceMode.Locked && it.voice.samples > paused }; result
            }
            click(scenario, R.id.voice_preview)
            waitFor("finish did not make an unsent preview") {
                var result = false; memory(scenario) { result = it.voice.mode == VoiceMode.Preview && it.voice.bytes != null }; result
            }
            click(scenario, R.id.voice_preview)
            waitFor("AudioTrack did not play the native decoded recording") {
                var result = false; memory(scenario) { result = it.audio?.playback?.preview == true && (it.audio?.playback?.sample ?: 0) > 0 }; result
            }
            scenario.moveToState(Lifecycle.State.CREATED)
            scenario.moveToState(Lifecycle.State.RESUMED)
            ready(scenario)
            memory(scenario) { assertEquals(VoiceMode.Preview, it.voice.mode); assertNotNull(it.voice.bytes); assertFalse(it.audio!!.playback.playing) }
            scenario.recreate(); ready(scenario)
            memory(scenario) { assertEquals(VoiceMode.Preview, it.voice.mode); assertNotNull(it.voice.bytes); assertFalse(it.audio!!.playback.playing) }
            assertEquals("no lifecycle auto-send", before, Core.facade(app).historyPage(peer, null, 50).rows.size)
            click(scenario, R.id.voice_discard)
            memory(scenario) { assertEquals(VoiceMode.Idle, it.voice.mode); assertNull(it.voice.bytes) }
            val (down, rect) = hold(scenario, false)
            val left = rect.centerX() - NativeUi.dp(app, 80).toFloat()
            touch(down, MotionEvent.ACTION_MOVE, left, rect.centerY().toFloat())
            touch(down, MotionEvent.ACTION_UP, left, rect.centerY().toFloat())
            memory(scenario) { assertEquals(VoiceMode.Idle, it.voice.mode); assertNull(it.voice.bytes) }
            assertEquals("cancel must not queue", before, Core.facade(app).historyPage(peer, null, 50).rows.size)
            Thread.sleep(200) // Let the dedicated recorder finish cancellation before the next gesture.
            hold(scenario, false)
            scenario.moveToState(Lifecycle.State.CREATED)
            Thread.sleep(300)
            scenario.moveToState(Lifecycle.State.RESUMED); ready(scenario)
            waitFor("background holding must finish into UNSENT preview") {
                var result = false; memory(scenario) { result = it.voice.mode == VoiceMode.Preview && !it.voice.sendOnFinish }; result
            }
            assertEquals(before, Core.facade(app).historyPage(peer, null, 50).rows.size)
            click(scenario, R.id.voice_discard)
        }
    }

    private fun views(view: View): List<View> = listOf(view) + if (view is ViewGroup)
        (0 until view.childCount).flatMap { views(view.getChildAt(it)) } else emptyList()
    private fun nodes(node: AccessibilityNodeInfo?): List<AccessibilityNodeInfo> = if (node == null) emptyList()
        else listOf(node) + (0 until node.childCount).flatMap { nodes(node.getChild(it)) }
    private fun systemCopyPresent(): Boolean {
        // The floating toolbar uses the Activity locale, which may differ from Resources.getSystem().
        val labels = listOf(java.util.Locale.ENGLISH, java.util.Locale("ru")).map { locale ->
            val config = android.content.res.Configuration(app.resources.configuration).apply { setLocale(locale) }
            app.createConfigurationContext(config).getString(android.R.string.copy)
        }.toSet()
        val roots = instrumentation.uiAutomation.windows.map { it.root } + instrumentation.uiAutomation.rootInActiveWindow
        return roots.flatMap(::nodes).any { node -> labels.any {
            node.text?.toString()?.equals(it, ignoreCase = true) == true || node.contentDescription?.toString()?.equals(it, ignoreCase = true) == true
        } }
    }
    @Test fun mixedVoiceRowsKeepSystemCopyAcrossNativeRefreshOnlyInGatePackage() {
        gate()
        val automation = instrumentation.uiAutomation
        val service = automation.serviceInfo
        val previousFlags = service.flags
        service.flags = previousFlags or android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        automation.serviceInfo = service
        val text = Core.facade(app).historyPage(peer, null, 50).rows.single { it.kind == MessageKind.TEXT }
        try { launch().use { scenario ->
            ready(scenario)
            instrumentation.waitForIdleSync()
            var body: TextView? = null
            var x = 0f; var y = 0f
            scenario.onActivity { activity ->
                val list = activity.findViewById<ListView>(R.id.messages)
                list.setSelection((0 until list.adapter.count).first { list.adapter.getItemId(it) == text.localId })
            }
            waitFor("selectable fixture text not laid out in a focused window") {
                var visible = false
                scenario.onActivity { activity ->
                    val view = views(activity.findViewById(R.id.messages)).filterIsInstance<TextView>().firstOrNull { it.text.toString() == text.text }
                    val rect = Rect()
                    if (view != null && view.layout != null && view.hasWindowFocus() && view.getGlobalVisibleRect(rect)) {
                        body = view
                        assertTrue(view.isTextSelectable); assertTrue(view.requestFocus())
                        val location = IntArray(2); view.getLocationOnScreen(location)
                        x = location[0] + view.totalPaddingLeft + NativeUi.dp(app, 8).toFloat()
                        y = location[1] + view.totalPaddingTop + (view.layout.getLineTop(0) + view.layout.getLineBottom(0)) / 2f
                        visible = rect.contains(x.toInt(), y.toInt())
                    }
                }
                visible
            }
            val down = SystemClock.uptimeMillis()
            touch(down, MotionEvent.ACTION_DOWN, x, y)
            SystemClock.sleep(ViewConfiguration.getLongPressTimeout().toLong() + 250)
            touch(down, MotionEvent.ACTION_UP, x, y)
            var selection = ""
            scenario.onActivity { selection = "${body?.selectionStart}..${body?.selectionEnd}" }
            waitFor("native system Copy unavailable; selected range $selection", ::systemCopyPresent)
            SystemClock.sleep(3_500)
            assertTrue("new native waveform arrays must not destroy Copy selection", systemCopyPresent())
            scenario.onActivity {
                assertSame(body, views(it.findViewById(R.id.messages)).filterIsInstance<TextView>().first { view -> view.text.toString() == text.text })
            }
        } } finally {
            val restored = automation.serviceInfo
            restored.flags = previousFlags
            automation.serviceInfo = restored
        }
    }

    @Test fun codecPartialFramesSeekAndStaticFailureOnlyInGatePackage() {
        gate(); System.loadLibrary("dmsg_core")
        var encoder = VoiceCodecJni.nativeEncoderCreate()
        try {
            val partial = ShortArray(321) { 2000 }
            try { assertEquals(321, VoiceCodecJni.nativeEncoderPush(encoder, partial)) } finally { partial.fill(0) }
            val snapshot = VoiceCodecJni.nativeEncoderSnapshot(encoder)
            try { assertEquals(321, VoiceCodecJni.nativeNoteSamples(snapshot)) } finally { snapshot.fill(0) }
            val more = ShortArray(319) { 1000 }
            try { assertEquals(640, VoiceCodecJni.nativeEncoderPush(encoder, more)) } finally { more.fill(0) }
            val handle = encoder; encoder = 0
            val note = VoiceCodecJni.nativeEncoderFinish(handle)
            try {
                assertEquals(640, VoiceCodecJni.nativeNoteSamples(note)); assertEquals(64, VoiceCodecJni.nativeNoteWaveform(note).size)
                val decoder = VoiceCodecJni.nativeDecoderCreate(note)
                try {
                    val full = VoiceCodecJni.nativeDecoderRead(decoder, 1600)
                    try { assertEquals(640, full.size) } finally { full.fill(0) }
                    assertTrue(VoiceCodecJni.nativeDecoderRead(decoder, 1600).isEmpty())
                    VoiceCodecJni.nativeDecoderSeek(decoder, 320)
                    val pcm = VoiceCodecJni.nativeDecoderRead(decoder, 1600)
                    try { assertEquals(320, pcm.size) } finally { pcm.fill(0) }
                } finally { VoiceCodecJni.nativeDecoderClose(decoder) }
                try { VoiceCodecJni.nativeNoteSamples(byteArrayOf(1)); fail("malformed note accepted") }
                catch (_: IllegalArgumentException) { /* Static JNI error, no payload reflection. */ }
            } finally { note.fill(0) }
        } finally { if (encoder != 0L) VoiceCodecJni.nativeEncoderCancel(encoder) }
    }
}

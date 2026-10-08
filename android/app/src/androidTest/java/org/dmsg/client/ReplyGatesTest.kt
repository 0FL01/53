package org.dmsg.client

import android.Manifest
import android.accessibilityservice.AccessibilityServiceInfo
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.database.sqlite.SQLiteDatabase
import android.graphics.Rect
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewGroup
import android.view.ViewTreeObserver
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.ScrollView
import android.widget.TextView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import uniffi.dmsg_core.DeleteScope
import uniffi.dmsg_core.DmsgClient
import uniffi.dmsg_core.ReplyTargetState

/** Fresh keyed local UI + real microphone, not successful Android DNS Reply delivery. */
@RunWith(AndroidJUnit4::class)
class ReplyGatesTest {
    @get:Rule val testName = TestName()
    private val app get() = ApplicationProvider.getApplicationContext<Context>()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val peer = "B0123456789C"
    private val source = "**Incoming** reply body [safe](https://example.test/path)"
    // X25519 public key of the fixture's [7;32] Noise private key; not a live identity.
    private val ownDevice = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d"
        .chunked(2).map { it.toInt(16).toByte() }.toByteArray()
    private fun mid(seq: Int) = ByteBuffer.allocate(16).putLong(0).putLong(seq.toLong()).array()
    private fun gate() {
        assertTrue("isolated package required", app.packageName.endsWith(".gate"))
        assertEquals("select exactly this method", "${javaClass.name}#${testName.methodName}",
            InstrumentationRegistry.getArguments().getString("class"))
        assertFalse("local UI evidence requires DNS FGS off", DmsgService.running(app))
    }
    private fun seed(deep: Boolean): Map<Int, Long> {
        val file = Core.dbFile(app)
        assertFalse("never overwrite an existing store", file.exists())
        val key = SecureStore.key(app)
        try {
            DmsgClient.openEncrypted(file.absolutePath, key).use { it.accountInfo() }
            SQLiteDatabase.openDatabase(file.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
                db.beginTransaction()
                try {
                    db.execSQL("INSERT INTO core_identity(id,device_priv) VALUES(1,?)", arrayOf(sealedFixtureValue(key, "device_priv", ByteArray(32) { 7 })))
                    db.execSQL("INSERT INTO core_account(id,user_id,contact_id) VALUES(1,?,?)", arrayOf(ByteArray(16) { 1 }, "A0123456789B"))
                    db.execSQL("INSERT INTO core_contacts(contact_id,state,user_id,device_key,ed_identity,curve_identity) VALUES(?,'accepted',?,?,?,?)",
                        arrayOf(peer, ByteArray(16) { 3 }, ByteArray(32) { 2 }, ByteArray(32) { 4 }, ByteArray(32) { 5 }))
                    // Older server-order rows have HIGHER local IDs. Prefetch read marking is observable.
                    val seqs = if (deep) (244 downTo 1).toList() else listOf(244, 243, 242, 241, 10)
                    for (seq in seqs) {
                        val outgoing = seq == 244 || seq == 243 || seq == 10 || seq in 20..194
                        if (seq == 242 || seq == 243) {
                            seedIncomingVoiceFixture(db, key, peer, mid(seq), 16_000, 100, ByteArray(64) { 20 }, seq.toLong(), mid(seq))
                            if (outgoing) db.execSQL("UPDATE core_messages SET direction='outgoing',sender_device=?,ciphertext=?,recipient_binding=?,delivery_state='accepted' WHERE message_id=?",
                                arrayOf(ownDevice, byteArrayOf(1), ByteArray(32) { 2 }, mid(seq)))
                        } else {
                            val raw = when (seq) { 244 -> "**Own** raw Markdown"; 241 -> source; 10 -> "**Original** deep target"; else -> "row $seq" }
                            db.execSQL("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,text,ciphertext,recipient_binding,delivery_state,local_timestamp_ms,server_seq,server_timestamp_ms,hidden_self) VALUES(?,?,?,?,'text',?,?,?,?,?,?,?,?)",
                                arrayOf(mid(seq), if (outgoing) ownDevice else ByteArray(32) { 2 }, peer, if (outgoing) "outgoing" else "incoming",
                                    if (seq in 20..194) null else sealedFixtureValue(key, "message_text", raw.toByteArray()), if (outgoing) byteArrayOf(1) else null,
                                    if (outgoing) ByteArray(32) { 2 } else null, if (outgoing) "accepted" else null,
                                    seq, seq, seq, if (seq in 20..194) 1 else 0))
                        }
                    }
                    db.execSQL("UPDATE core_messages SET reply_ref=? WHERE message_id=?", arrayOf(ownDevice + mid(10), mid(241)))
                    db.setTransactionSuccessful()
                } finally { db.endTransaction() }
                db.rawQuery("PRAGMA user_version", null).use { assertTrue(it.moveToFirst()); assertEquals(10, it.getInt(0)) }
                val ids = mutableMapOf<Int, Long>()
                db.rawQuery("SELECT local_id,server_seq FROM core_messages", null).use { while (it.moveToNext()) ids[it.getInt(1)] = it.getLong(0) }
                return ids
            }
        } finally { key.fill(0) }
    }
    private fun launch() = ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java)
        .putExtra("peer", peer).putExtra("alias", "Peer").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    private fun waitFor(label: String, detail: () -> String = { "" }, predicate: () -> Boolean) {
        val deadline = SystemClock.uptimeMillis() + 20_000
        while (SystemClock.uptimeMillis() < deadline) { if (predicate()) return; Thread.sleep(75) }
        fail("$label ${detail()}")
    }
    private fun ready(s: ActivityScenario<ChatActivity>) = waitFor("native chat not ready") {
        var ready = false
        s.onActivity { ready = it.hasWindowFocus() && it.findViewById<ListView>(R.id.messages).adapter.count > 0 && it.findViewById<Button>(R.id.btn_history_retry).isEnabled }
        ready
    }
    private fun memory(a: ChatActivity) = ViewModelProvider(a)[ChatMemory::class.java]
    private fun views(v: View): List<View> = listOf(v) + if (v is ViewGroup) (0 until v.childCount).flatMap { views(v.getChildAt(it)) } else emptyList()
    private fun nodes(n: AccessibilityNodeInfo?): List<AccessibilityNodeInfo> = if (n == null) emptyList() else listOf(n) + (0 until n.childCount).flatMap { nodes(n.getChild(it)) }
    private fun allNodes() = (instrumentation.uiAutomation.windows.map { it.root } + instrumentation.uiAutomation.rootInActiveWindow).flatMap(::nodes)
    private fun labels(label: String) = allNodes().filter { it.isVisibleToUser && (it.text?.toString() == label || it.contentDescription?.toString() == label) }
    private fun menuClick(label: String) {
        if (labels(label).isEmpty()) allNodes().firstOrNull { it.viewIdResourceName?.endsWith("/overflow") == true || it.contentDescription?.toString() in listOf("More options", "Ещё", "Другие параметры") }
            ?.performAction(AccessibilityNodeInfo.ACTION_CLICK)
        // The floating toolbar can replace its nodes between two accessibility snapshots.
        waitFor("native menu action missing: $label") { labels(label).firstOrNull()?.performAction(AccessibilityNodeInfo.ACTION_CLICK) == true }
    }
    private fun row(s: ActivityScenario<ChatActivity>, id: Long): View {
        s.onActivity { a -> val list = a.findViewById<ListView>(R.id.messages); list.setSelection((0 until list.adapter.count).first { list.adapter.getItemId(it) == id }) }
        var row: View? = null
        waitFor("stable-ID row $id not visible") {
            s.onActivity { a -> val list = a.findViewById<ListView>(R.id.messages); row = (0 until list.childCount).map(list::getChildAt).firstOrNull { it.tag == id } }
            row?.getGlobalVisibleRect(Rect()) == true
        }
        return row!!
    }
    private fun body(s: ActivityScenario<ChatActivity>, id: Long) = row(s, id).findViewById<View>(R.id.message_body)
    private fun touch(down: Long, action: Int, x: Float, y: Float) {
        val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, x, y, 0).apply { source = InputDevice.SOURCE_TOUCHSCREEN }
        try { instrumentation.sendPointerSync(event) } finally { event.recycle() }
    }
    private fun hold(v: TextView, selectable: Boolean) {
        var x = 0f; var y = 0f
        instrumentation.runOnMainSync {
            if (selectable) { assertTrue(v.isTextSelectable); assertTrue(v.requestFocus()) }
            val p = IntArray(2); v.getLocationOnScreen(p)
            x = (p[0] + v.totalPaddingLeft + NativeUi.dp(app, 8)).toFloat()
            y = p[1] + v.totalPaddingTop + (v.layout.getLineTop(0) + v.layout.getLineBottom(0)) / 2f
            val rect = Rect(); assertTrue(v.getGlobalVisibleRect(rect)); assertTrue("hold point visible", rect.contains(x.toInt(), y.toInt()))
        }
        val down = SystemClock.uptimeMillis()
        touch(down, MotionEvent.ACTION_DOWN, x, y); SystemClock.sleep(ViewConfiguration.getLongPressTimeout().toLong() + 250)
        touch(down, MotionEvent.ACTION_UP, x, y)
    }
    private fun selectReply(s: ActivityScenario<ChatActivity>, id: Long) {
        val b = body(s, id)
        val text = if (b is TextView) b else views(b).filterIsInstance<TextView>().first { it !is Button }
        hold(text, b is TextView)
        menuClick(app.getString(R.string.reply_whole_message))
        waitFor("Reply did not select stable source $id") { var found = false; s.onActivity { found = memory(it).outgoing.reply?.targetLocalId == id }; found }
    }
    private fun click(s: ActivityScenario<ChatActivity>, id: Int) = s.onActivity {
        val view = it.findViewById<View>(id); assertTrue(view.isShown && view.isEnabled); assertTrue(view.performClick())
    }
    private fun jump(a: ChatActivity): Any? = ChatActivity::class.java.getDeclaredField("quoteJump").apply { isAccessible = true }.get(a)
    private fun shell(command: String) = ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand(command)).bufferedReader().use { it.readText().trim() }
    private fun nativeUnread() = Core.facade(app).dialogsPage(null, 50).rows.single { it.contactId == peer }.localUnread
    private fun blockedWorker(action: () -> Unit) {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        Core.dispatch { entered.countDown(); release.await(20, TimeUnit.SECONDS) }
        assertTrue(entered.await(20, TimeUnit.SECONDS))
        try { action() } finally { release.countDown() }
    }
    private fun tap(v: View) {
        val r = Rect(); instrumentation.runOnMainSync { assertTrue(v.getGlobalVisibleRect(r)) }
        val down = SystemClock.uptimeMillis(); touch(down, MotionEvent.ACTION_DOWN, r.centerX().toFloat(), r.centerY().toFloat()); touch(down, MotionEvent.ACTION_UP, r.centerX().toFloat(), r.centerY().toFloat())
    }

    private fun verifyNavigation(s: ActivityScenario<ChatActivity>, ids: Map<Int, Long>) {
        // Do this before recreation/ordinary scrolling can legitimately load older pages.
        val quote = row(s, ids.getValue(241)).findViewById<View>(R.id.reply_quote)
        s.onActivity { assertFalse("original must still require paging", memory(it).history.rows.any { r -> r.localId == ids.getValue(10) }) }
        run {
            val baseline = nativeUnread(); assertTrue("non-vacuous native unread fixture", baseline > 9uL)
            val failure = AtomicReference<String?>(null)
            var activity: ChatActivity? = null
            s.onActivity { activity = it }
            val monitor = ViewTreeObserver.OnPreDrawListener {
                if (jump(activity!!) != null) { val unread = nativeUnread(); if (unread != baseline) failure.compareAndSet(null, "prefetch unread $baseline -> $unread") }
                true
            }
            s.onActivity { it.findViewById<ListView>(R.id.messages).viewTreeObserver.addOnPreDrawListener(monitor) }
            try {
                tap(quote)
                waitFor("quote did not cross initial page and >150 hidden rows") {
                    var found = false
                    s.onActivity { a -> val list = a.findViewById<ListView>(R.id.messages); found = jump(a) == null && (0 until list.childCount).any { i -> val child = list.getChildAt(i); child.tag == ids.getValue(10) && child.bottom > list.paddingTop && child.top < list.height - list.paddingBottom } }
                    found
                }
                assertNull(failure.get(), failure.get())
                var detail = ""
                waitFor("settled original must be marked only once rendered", { detail }) {
                    val unread = nativeUnread()
                    s.onActivity { a ->
                        val list = a.findViewById<ListView>(R.id.messages); val m = memory(a)
                        val pending = ChatActivity::class.java.getDeclaredField("readPending").apply { isAccessible = true }.get(a)
                        detail = "baseline=$baseline unread=$unread memory=${m.readThrough} readPending=$pending page=${(ChatActivity::class.java.getDeclaredField("pageGuard").apply { isAccessible = true }.get(a) as UiGuard).pending} jump=${jump(a)} action=${m.action(a.resources)} children=" +
                            (0 until list.childCount).joinToString { i -> val child = list.getChildAt(i); "${child.tag}:${child.top}..${child.bottom}/${list.height}" }
                    }
                    unread <= 9uL
                }
            } finally { s.onActivity { it.findViewById<ListView>(R.id.messages).viewTreeObserver.removeOnPreDrawListener(monitor) } }
        }
        ready(s)
        val cancelQuote = row(s, ids.getValue(241)).findViewById<View>(R.id.reply_quote)
        blockedWorker {
            tap(cancelQuote)
            s.onActivity { assertNotNull(jump(it)) }
            val rect = Rect(); s.onActivity { assertTrue(it.findViewById<ListView>(R.id.messages).getGlobalVisibleRect(rect)) }
            val down = SystemClock.uptimeMillis(); val x = rect.centerX().toFloat(); val y = rect.centerY().toFloat()
            touch(down, MotionEvent.ACTION_DOWN, x, y); SystemClock.sleep(100)
            touch(down, MotionEvent.ACTION_MOVE, x, y + NativeUi.dp(app, 24)); SystemClock.sleep(100)
            touch(down, MotionEvent.ACTION_UP, x, y + NativeUi.dp(app, 24))
            s.onActivity { assertNull("user drag cancels pending navigation", jump(it)) }
        }
        ready(s); SystemClock.sleep(500)
        s.onActivity {
            assertNull(jump(it))
            val list = it.findViewById<ListView>(R.id.messages)
            assertFalse("stale callback must not move viewport to the original", (0 until list.childCount).any { list.getChildAt(it).tag == ids.getValue(10) })
        }
    }

    @Test fun freshReplyComposerAndQuoteNavigationOnlyInGatePackage() {
        gate(); val ids = seed(true); val f = Core.facade(app); val beforeOutbox = f.outbox(0, 50)
        val automation = instrumentation.uiAutomation; val flags = automation.serviceInfo.flags
        automation.serviceInfo = automation.serviceInfo.apply { this.flags = flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS or AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS }
        val clipboard = app.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val font = shell("settings get system font_scale")
        try { launch().use { s ->
            ready(s); var oldClip: ClipData? = null; var orientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            s.onActivity { oldClip = clipboard.primaryClip; orientation = it.requestedOrientation; assertFalse(memory(it).history.rows.any { r -> r.localId == ids.getValue(10) }) }
            try {
                verifyNavigation(s, ids)
                val selected = body(s, ids.getValue(241)) as TextView
                hold(selected, true)
                waitFor("system Copy missing") { labels(app.getString(android.R.string.copy)).isNotEmpty() }
                var copy = ""; var start = 0; var end = 0
                s.onActivity { start = selected.selectionStart; end = selected.selectionEnd; assertTrue(end > start); copy = selected.text.subSequence(start, end).toString(); assertFalse(copy.contains("Original")) }
                SystemClock.sleep(3_500)
                s.onActivity { assertEquals(start, selected.selectionStart); assertEquals(end, selected.selectionEnd); assertTrue(views(it.findViewById(R.id.messages)).any { v -> v === selected }) }
                menuClick(app.getString(android.R.string.copy))
                waitFor("Copy must copy only rendered body") { clipboard.primaryClip?.getItemAt(0)?.text?.toString() == copy }
                for (seq in listOf(241, 244, 242, 243)) {
                    ready(s); selectReply(s, ids.getValue(seq))
                    s.onActivity { assertTrue(it.findViewById<View>(R.id.reply_banner).isShown); it.findViewById<EditText>(R.id.composer).setText("normal **draft**") }
                    click(s, R.id.btn_reply_cancel)
                    s.onActivity { assertNull(memory(it).outgoing.reply); assertEquals("normal **draft**", it.findViewById<EditText>(R.id.composer).text.toString()) }
                }
                ready(s); selectReply(s, ids.getValue(241))
                val own = body(s, ids.getValue(244))
                waitFor("eligible edit did not start outside read-only paging") {
                    var started = false
                    s.onActivity { a ->
                        val guard = ChatActivity::class.java.getDeclaredField("pageGuard").apply { isAccessible = true }.get(a) as UiGuard
                        if (!guard.pending) { assertTrue(own.performAccessibilityAction(R.id.action_edit, null)); started = memory(a).composition.edit != null }
                    }
                    started
                }
                s.onActivity { assertEquals("**Own** raw Markdown", it.findViewById<EditText>(R.id.composer).text.toString()); assertEquals(View.GONE, it.findViewById<View>(R.id.reply_banner).visibility) }
                s.recreate(); ready(s); click(s, R.id.btn_cancel_edit)
                s.onActivity { assertEquals(ids.getValue(241), memory(it).outgoing.reply!!.targetLocalId); assertEquals("normal **draft**", it.findViewById<EditText>(R.id.composer).text.toString()) }
                s.recreate(); ready(s)
                blockedWorker {
                    waitFor("read-only ticker query did not become pending") {
                        var pending = false
                        s.onActivity { a -> pending = (ChatActivity::class.java.getDeclaredField("pageGuard").apply { isAccessible = true }.get(a) as UiGuard).pending }
                        pending
                    }
                    s.onActivity { assertFalse(memory(it).pending); assertTrue(it.findViewById<Button>(R.id.btn_reply_cancel).isEnabled); it.findViewById<Button>(R.id.btn_reply_cancel).performClick(); assertNull(memory(it).outgoing.reply) }
                }
                ready(s); s.onActivity { assertNull(memory(it).outgoing.reply) }
                ready(s)
                val originalBody = body(s, ids.getValue(10))
                s.onActivity { assertTrue(originalBody.performAccessibilityAction(R.id.action_reply, null)) }
                f.deleteMessage(peer, ids.getValue(10), DeleteScope.SELF_ONLY)
                waitFor("unavailable original must suppress preview and new send") {
                    var unavailable = false; s.onActivity { unavailable = memory(it).outgoing.reply?.available == false && !it.findViewById<Button>(R.id.btn_send).isEnabled && it.findViewById<TextView>(R.id.reply_preview).text.toString() == it.getString(R.string.reply_message_unavailable) }; unavailable
                }
                assertEquals(ReplyTargetState.HIDDEN, f.historyMessage(peer, ids.getValue(241)).reply!!.state)
                assertEquals("", f.historyMessage(peer, ids.getValue(241)).reply!!.preview)
                click(s, R.id.btn_reply_cancel)
                ready(s); selectReply(s, ids.getValue(241))
                shell("settings put system font_scale 2.0")
                waitFor("large font not applied") { var ok = false; s.onActivity { ok = it.resources.configuration.fontScale >= 1.9f }; ok }
                s.onActivity { it.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE }
                waitFor("landscape not focused") { var ok = false; s.onActivity { ok = it.resources.configuration.orientation == Configuration.ORIENTATION_LANDSCAPE && it.window.decorView.width > it.window.decorView.height && it.hasWindowFocus() }; ok }
                s.onActivity { val editor = it.findViewById<EditText>(R.id.composer); editor.requestFocus(); (it.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager).showSoftInput(editor, InputMethodManager.SHOW_IMPLICIT) }
                waitFor("IME not shown") { var ok = false; s.onActivity { ok = ViewCompat.getRootWindowInsets(it.window.decorView)?.isVisible(WindowInsetsCompat.Type.ime()) == true }; ok }
                s.onActivity { it.findViewById<ScrollView>(R.id.chat_footer).scrollTo(0, 0) }
                waitFor("48dp Reply Cancel not reachable at 200% landscape IME") { var ok = false; s.onActivity { val v = it.findViewById<Button>(R.id.btn_reply_cancel); val r = Rect(); ok = v.isEnabled && v.getGlobalVisibleRect(r) && r.height() >= NativeUi.dp(it, 48) }; ok }
                click(s, R.id.btn_reply_cancel)
                s.onActivity { assertEquals("normal **draft**", it.findViewById<EditText>(R.id.composer).text.toString()) }
                assertEquals("local UI actions must not add/change outbox", beforeOutbox, f.outbox(0, 50))
            } finally { s.onActivity { if (oldClip == null) clipboard.clearPrimaryClip() else clipboard.setPrimaryClip(oldClip!!); it.requestedOrientation = orientation; (it.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(it.window.decorView.windowToken, 0) } }
        } } finally { shell(if (font == "null") "settings delete system font_scale" else "settings put system font_scale $font"); automation.serviceInfo = automation.serviceInfo.apply { this.flags = flags } }
    }

    @Test fun replyVoiceRecordingRetainsTargetOnlyInGatePackage() {
        gate(); val ids = seed(false); val f = Core.facade(app)
        instrumentation.uiAutomation.grantRuntimePermission(app.packageName, Manifest.permission.RECORD_AUDIO)
        val before = f.historyPage(peer, null, 50).rows.size
        val beforeOutbox = f.outbox(0, 50)
        launch().use { s ->
            ready(s); val b = body(s, ids.getValue(244))
            s.onActivity { assertTrue(b.performAccessibilityAction(R.id.action_reply, null)); assertEquals(ids.getValue(244), memory(it).outgoing.reply!!.targetLocalId) }
            val rect = Rect(); s.onActivity { assertTrue(it.findViewById<View>(R.id.btn_mic).getGlobalVisibleRect(rect)) }
            val down = SystemClock.uptimeMillis(); val x = rect.centerX().toFloat(); val y = rect.centerY().toFloat()
            touch(down, MotionEvent.ACTION_DOWN, x, y); SystemClock.sleep(700)
            touch(down, MotionEvent.ACTION_MOVE, x, y - NativeUi.dp(app, 80)); touch(down, MotionEvent.ACTION_UP, x, y - NativeUi.dp(app, 80))
            waitFor("real replied microphone recording did not lock") { var ok = false; s.onActivity { ok = memory(it).voice.mode == VoiceMode.Locked && memory(it).voice.samples > 0 }; ok }
            s.onActivity { assertEquals(ids.getValue(244), memory(it).voice.replyToLocalId); assertFalse(it.findViewById<Button>(R.id.btn_reply_cancel).isEnabled); it.findViewById<Button>(R.id.btn_reply_cancel).performClick(); assertEquals(ids.getValue(244), memory(it).outgoing.reply!!.targetLocalId) }
            click(s, R.id.voice_pause)
            waitFor("pause did not retain audio") { var ok = false; s.onActivity { ok = memory(it).voice.mode == VoiceMode.Paused && memory(it).voice.bytes != null }; ok }
            var samples = 0; s.onActivity { samples = memory(it).voice.samples }
            click(s, R.id.voice_pause)
            waitFor("resume did not capture more samples") { var ok = false; s.onActivity { ok = memory(it).voice.mode == VoiceMode.Locked && memory(it).voice.samples > samples }; ok }
            click(s, R.id.voice_preview)
            waitFor("preview missing") { var ok = false; s.onActivity { ok = memory(it).voice.mode == VoiceMode.Preview && memory(it).voice.bytes != null }; ok }
            click(s, R.id.voice_preview)
            waitFor("real preview playback missing") { var ok = false; s.onActivity { ok = memory(it).audio?.playback?.preview == true && (memory(it).audio?.playback?.sample ?: 0) > 0 }; ok }
            s.moveToState(Lifecycle.State.CREATED); s.moveToState(Lifecycle.State.RESUMED); ready(s)
            s.recreate(); ready(s)
            s.onActivity { val m = memory(it); assertEquals(VoiceMode.Preview, m.voice.mode); assertEquals(ids.getValue(244), m.voice.replyToLocalId); assertNotNull(m.voice.bytes); assertFalse(m.audio!!.playback.playing) }
            f.deleteMessage(peer, ids.getValue(244), DeleteScope.SELF_ONLY)
            waitFor("unavailable frozen target must block queue but keep audio") { var ok = false; s.onActivity { val m = memory(it); ok = m.voice.replyDraft?.available == false && m.voice.bytes != null && !it.findViewById<Button>(R.id.voice_send).isEnabled }; ok }
            s.onActivity { it.findViewById<Button>(R.id.voice_send).performClick(); assertFalse(memory(it).pending); assertEquals(ids.getValue(244), memory(it).voice.replyToLocalId) }
            assertEquals("no lifecycle auto-send", before, f.historyPage(peer, null, 50).rows.size)
            assertEquals(beforeOutbox, f.outbox(0, 50))
            click(s, R.id.voice_discard)
            s.onActivity { assertEquals(VoiceMode.Idle, memory(it).voice.mode); assertNull(memory(it).voice.bytes); assertEquals(ids.getValue(244), memory(it).outgoing.reply!!.targetLocalId) }
            click(s, R.id.btn_reply_cancel)
            s.onActivity { assertNull(memory(it).outgoing.reply); assertEquals("", it.findViewById<EditText>(R.id.composer).text.toString()) }
        }
    }
}

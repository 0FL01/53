package org.dmsg.client

import android.accessibilityservice.AccessibilityServiceInfo
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.database.sqlite.SQLiteDatabase
import android.graphics.Rect
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.text.Selection
import android.text.Spannable
import android.text.Spanned
import android.view.ActionMode
import android.view.InputDevice
import android.view.Menu
import android.view.MenuInflater
import android.view.MenuItem
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.view.menu.MenuBuilder
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
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
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.DmsgClient
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.MessageKind

/** Local native UI evidence only: no profile/session and no Android DNS delivery claim. */
@RunWith(AndroidJUnit4::class)
class MarkdownMessagesGatesTest {
    @get:Rule val testName = TestName()
    private val app get() = ApplicationProvider.getApplicationContext<Context>()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val peer = "B0123456789C"
    private val source = "**Markdown** _raw_ [safe label](https://example.test/path)\n`code` ~~strike~~"

    private fun gate() {
        assertTrue("isolated package required", app.packageName.endsWith(".gate"))
        assertEquals("select exactly one method", "${javaClass.name}#${testName.methodName}",
            InstrumentationRegistry.getArguments().getString("class"))
        assertFalse("local UI gate requires DNS FGS off", DmsgService.running(app))
    }

    private class SelectionMode(context: Context, private val menu: Menu, private val onFinish: () -> Unit) : ActionMode() {
        var finished = false
        override fun finish() { finished = true; onFinish() }
        override fun invalidate() {}
        override fun getMenu() = menu
        private val inflater = MenuInflater(context)
        override fun getMenuInflater() = inflater
        override fun setTitle(title: CharSequence?) {}
        override fun setTitle(resId: Int) {}
        override fun setSubtitle(subtitle: CharSequence?) {}
        override fun setSubtitle(resId: Int) {}
        override fun getTitle(): CharSequence? = null
        override fun getSubtitle(): CharSequence? = null
        override fun setCustomView(view: View?) {}
        override fun getCustomView(): View? = null
    }

    @Test fun rendererLinksAndRawFallbackOnlyInGatePackage() {
        gate()
        instrumentation.runOnMainSync {
            val context = object : ContextWrapper(app) {
                var opened: Intent? = null
                override fun startActivity(intent: Intent) { opened = intent }
            }
            val renderer = MessageMarkdown(context)
            val cases = listOf("**bold**" to "StrongEmphasisSpan", "_italic_" to "EmphasisSpan",
                "~~strike~~" to "StrikethroughSpan", "`code`" to "CodeSpan",
                "```\na < b\n```" to "CodeBlockSpan", "> quote" to "BlockQuoteSpan",
                "# heading" to "HeadingSpan", "- item" to "BulletListItemSpan", "1. item" to "OrderedListItemSpan")
            for ((raw, spanClass) in cases) {
                val text = renderer.render(raw)
                assertTrue("missing $spanClass", text.getSpans(0, text.length, Any::class.java)
                    .any { it.javaClass.simpleName == spanClass })
            }
            assertEquals("first\nsecond", renderer.render("first\nsecond").toString())
            assertEquals("*literal* and **unclosed", renderer.render("\\*literal\\* and **unclosed").toString())
            assertEquals("<b>literal</b>", renderer.render("<b>literal</b>").toString())
            assertTrue(renderer.render("<script>noop()</script>\n\ntext").toString().contains("<script>noop()</script>"))
            assertEquals("alt text", renderer.render("![alt text](https://example.test/image.png)").toString())
            val unsafe = renderer.render("[java](javascript:alert(1)) [intent](intent://example) [file](file:///tmp/x) [relative](/route)")
            assertTrue(unsafe.getSpans(0, unsafe.length, android.text.style.ClickableSpan::class.java).isEmpty())
            for (length in listOf(4000, 4096)) {
                val prefix = "> ".repeat(65)
                val raw = prefix + "x".repeat(length - prefix.length)
                assertEquals("full fallback including durable legacy source", raw, renderer.render(raw).toString())
            }

            val row = HistoryMessage(1, "01".repeat(16), peer, MessageDirection.OUTGOING, MessageKind.TEXT,
                null, source, 1, DeliveryState.ACCEPTED, 1, 1, 0uL, false, false, null, null)
            for (outgoing in listOf(true, false)) {
                val message = if (outgoing) row else row.copy(direction = MessageDirection.INCOMING, deliveryState = null)
                val adapter = HistoryAdapter(context, listOf(message), { true }, {}, {}, {}, { _, _ -> })
                val root = adapter.getView(0, null, LinearLayout(context))
                val body = views(root).filterIsInstance<TextView>().first()
                assertEquals(source, adapter.getItem(0).text)
                assertTrue(body.isTextSelectable); assertEquals(0, body.autoLinkMask)
                val text = body.text as Spannable
                val span = (text as Spanned).getSpans(0, text.length, MessageMarkdownLinkSpan::class.java).single()
                Selection.setSelection(text, text.getSpanStart(span), text.getSpanEnd(span))
                assertEquals("https://example.test/path", MessageMarkdown.selectedLink(body))
                val menu = MenuBuilder(context).apply { add(0, android.R.id.copy, 0, android.R.string.copy) }
                val mode = SelectionMode(context, menu) { Selection.removeSelection(text) }
                val callback = body.customSelectionActionModeCallback!!
                assertTrue(callback.onCreateActionMode(mode, menu))
                assertNotNull(menu.findItem(android.R.id.copy))
                assertEquals(outgoing, menu.findItem(R.id.action_edit) != null)
                assertEquals(outgoing, menu.findItem(R.id.action_delete) != null)
                assertNotNull(menu.findItem(R.id.action_open_link))
                Selection.setSelection(text, 0, text.length)
                callback.onPrepareActionMode(mode, menu)
                assertNull("cross-span selection cannot open a link", menu.findItem(R.id.action_open_link))
                Selection.setSelection(text, text.getSpanStart(span), text.getSpanEnd(span))
                callback.onPrepareActionMode(mode, menu)
                assertTrue(callback.onActionItemClicked(mode, menu.findItem(R.id.action_open_link)))
                assertTrue(mode.finished)
                assertEquals(Intent.ACTION_VIEW, context.opened!!.action)
                assertEquals("https://example.test/path", context.opened!!.dataString)
                assertTrue(context.opened!!.hasCategory(Intent.CATEGORY_BROWSABLE))
            }
            val noHandler = object : ContextWrapper(app) {
                override fun startActivity(intent: Intent) { throw ActivityNotFoundException() }
            }
            assertFalse(MessageMarkdown.openLink(noHandler, "https://example.test"))
            assertFalse(MessageMarkdown.openLink(context, "javascript:alert(1)"))
        }
    }

    private fun seedFreshChat() {
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
                    db.execSQL("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,text,local_timestamp_ms,server_seq,server_timestamp_ms) VALUES(?,?,?,'incoming','text',?,1,1,1)",
                        arrayOf(ByteArray(16) { 8 }, ByteArray(32) { 2 }, peer, sealedFixtureValue(key, "message_text", source.toByteArray())))
                    db.execSQL("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,text,ciphertext,recipient_binding,delivery_state,local_timestamp_ms,server_seq,server_timestamp_ms) VALUES(?,?,?,'outgoing','text',?,?,?,'accepted',2,2,2)",
                        arrayOf(ByteArray(16) { 9 }, ByteArray(32) { 7 }, peer, sealedFixtureValue(key, "message_text", source.toByteArray()), byteArrayOf(1), ByteArray(32) { 2 }))
                    seedIncomingVoiceFixture(db, key, peer, ByteArray(16) { 10 }, 16000, 100, ByteArray(64) { 20 }, 3)
                    db.setTransactionSuccessful()
                } finally { db.endTransaction() }
                db.rawQuery("PRAGMA user_version", null).use { assertTrue(it.moveToFirst()); assertEquals(10, it.getInt(0)) }
            }
        } finally { key.fill(0) }
    }

    private fun waitFor(label: String, detail: () -> String = { "" }, condition: () -> Boolean) {
        val deadline = SystemClock.uptimeMillis() + 20_000
        while (SystemClock.uptimeMillis() < deadline) { if (condition()) return; Thread.sleep(75) }
        fail("$label ${detail()}")
    }
    private fun views(view: View): List<View> = listOf(view) + if (view is ViewGroup)
        (0 until view.childCount).flatMap { views(view.getChildAt(it)) } else emptyList()
    private fun nodes(node: AccessibilityNodeInfo?): List<AccessibilityNodeInfo> = if (node == null) emptyList()
        else listOf(node) + (0 until node.childCount).flatMap { nodes(node.getChild(it)) }
    private fun labelNodes(label: String) = (instrumentation.uiAutomation.windows.map { it.root } + instrumentation.uiAutomation.rootInActiveWindow)
        .flatMap(::nodes).filter { it.text?.toString() == label || it.contentDescription?.toString() == label }
    private fun ready(scenario: ActivityScenario<ChatActivity>) = waitFor("fresh native chat did not load") {
        var result = false
        scenario.onActivity { result = it.hasWindowFocus() && it.findViewById<Button>(R.id.btn_history_retry).isEnabled && it.findViewById<ListView>(R.id.messages).adapter.count == 3 }
        result
    }
    private fun body(scenario: ActivityScenario<ChatActivity>, localId: Long): TextView {
        scenario.onActivity {
            val list = it.findViewById<ListView>(R.id.messages)
            list.setSelection((0 until list.adapter.count).first { p -> list.adapter.getItemId(p) == localId })
        }
        var body: TextView? = null
        waitFor("native stable-ID TEXT body unavailable") {
            scenario.onActivity {
                val list = it.findViewById<ListView>(R.id.messages)
                val row = (0 until list.childCount).map(list::getChildAt).firstOrNull { view -> view.tag == localId }
                body = row?.let(::views)?.filterIsInstance<TextView>()?.firstOrNull()
            }
            body?.getGlobalVisibleRect(Rect()) == true && body?.layout != null
        }
        return body!!
    }
    private fun shell(command: String): String = ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand(command))
        .bufferedReader().use { it.readText().trim() }

    @Test fun freshComposerMarkdownCopyAndActionsOnlyInGatePackage() {
        gate()
        assertTrue("clipboard restoration requires API28+", Build.VERSION.SDK_INT >= 28)
        seedFreshChat()
        val f = Core.facade(app)
        val before = f.historyPage(peer, null, 50).rows
        val own = before.single { it.direction == MessageDirection.OUTGOING }
        assertEquals(source, own.text)
        val beforeOutbox = f.outbox(0, 50)
        val clipboard = app.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val automation = instrumentation.uiAutomation
        val previousFlags = automation.serviceInfo.flags
        automation.serviceInfo = automation.serviceInfo.apply { flags = flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS }
        val previousFont = shell("settings get system font_scale")
        try {
            ActivityScenario.launch<ChatActivity>(Intent(app, ChatActivity::class.java).putExtra("peer", peer).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)).use { scenario ->
                ready(scenario)
                var previousClip: ClipData? = null
                var previousOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
                scenario.onActivity { previousClip = clipboard.primaryClip; previousOrientation = it.requestedOrientation }
                try {
                    scenario.onActivity {
                        assertTrue(it.findViewById<Button>(R.id.btn_send).isEnabled)
                        assertEquals(View.GONE, it.findViewById<Button>(R.id.btn_send).visibility)
                        assertEquals(View.VISIBLE, it.findViewById<View>(R.id.btn_mic).visibility)
                        it.findViewById<Button>(R.id.btn_send).performClick() // Handler validation even while hidden.
                        val memory = ViewModelProvider(it)[ChatMemory::class.java]
                        assertFalse(memory.pending); assertEquals(it.getString(R.string.message_bounds), memory.action(it.resources))
                        val editor = it.findViewById<EditText>(R.id.composer)
                        editor.requestFocus()
                        clipboard.setPrimaryClip(ClipData.newPlainText("dmsg gate", "🦀".repeat(4001)))
                        assertTrue("actual editor paste action", editor.onTextContextMenuItem(android.R.id.paste))
                        assertEquals("🦀".repeat(4001), editor.text.toString())
                        assertEquals(it.getString(R.string.message_counter_overflow, 4001, 4000), it.findViewById<TextView>(R.id.composer_counter).text.toString())
                        assertFalse(it.findViewById<Button>(R.id.btn_send).isEnabled)
                        assertEquals(View.VISIBLE, it.findViewById<Button>(R.id.btn_send).visibility)
                        assertEquals(View.GONE, it.findViewById<View>(R.id.btn_mic).visibility)
                        it.findViewById<Button>(R.id.btn_send).performClick()
                        assertFalse(memory.pending)
                    }
                    scenario.recreate(); ready(scenario)
                    scenario.onActivity {
                        val editor = it.findViewById<EditText>(R.id.composer)
                        assertEquals("🦀".repeat(4001), editor.text.toString())
                        editor.text.delete(editor.text.length - 2, editor.text.length)
                        assertEquals(it.getString(R.string.message_counter, 4000, 4000), it.findViewById<TextView>(R.id.composer_counter).text.toString())
                        assertTrue(it.findViewById<Button>(R.id.btn_send).isEnabled)
                        (it.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(editor.windowToken, 0)
                    }
                    assertEquals(beforeOutbox, f.outbox(0, 50))
                    val selected = body(scenario, own.localId)
                    var x = 0f; var y = 0f
                    scenario.onActivity {
                        assertTrue(selected.requestFocus()); assertTrue(selected.isTextSelectable)
                        assertTrue(selected.text.toString().startsWith("Markdown raw"))
                        val location = IntArray(2); selected.getLocationOnScreen(location)
                        x = location[0] + selected.totalPaddingLeft + NativeUi.dp(app, 8).toFloat()
                        y = location[1] + selected.totalPaddingTop + (selected.layout.getLineTop(0) + selected.layout.getLineBottom(0)) / 2f
                    }
                    val down = SystemClock.uptimeMillis()
                    fun touch(action: Int) {
                        val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, x, y, 0)
                        event.source = InputDevice.SOURCE_TOUCHSCREEN
                        try { instrumentation.sendPointerSync(event) } finally { event.recycle() }
                    }
                    touch(MotionEvent.ACTION_DOWN)
                    SystemClock.sleep(ViewConfiguration.getLongPressTimeout().toLong() + 250)
                    touch(MotionEvent.ACTION_UP)
                    val copyLabel = app.getString(android.R.string.copy)
                    waitFor("real system Copy unavailable") { labelNodes(copyLabel).isNotEmpty() }
                    var copied = ""
                    var selectionStart = -1; var selectionEnd = -1
                    scenario.onActivity {
                        selectionStart = selected.selectionStart; selectionEnd = selected.selectionEnd
                        assertTrue(selectionStart >= 0 && selectionEnd > selectionStart)
                        copied = selected.text.subSequence(selectionStart, selectionEnd).toString()
                        assertFalse(copied.contains("**"))
                    }
                    SystemClock.sleep(3_500)
                    assertTrue("ticker must preserve actual Copy", labelNodes(copyLabel).isNotEmpty())
                    scenario.onActivity {
                        assertEquals(selectionStart, selected.selectionStart); assertEquals(selectionEnd, selected.selectionEnd)
                        assertTrue(views(it.findViewById(R.id.messages)).any { view -> view === selected })
                    }
                    assertTrue(labelNodes(copyLabel).first().performAction(AccessibilityNodeInfo.ACTION_CLICK))
                    waitFor("Copy did not put displayed selection in clipboard") { clipboard.primaryClip?.getItemAt(0)?.text?.toString() == copied }
                    ready(scenario)
                    val ownBody = body(scenario, own.localId)
                    scenario.onActivity { assertTrue(ownBody.performAccessibilityAction(R.id.action_edit, null)) }
                    scenario.onActivity {
                        val editor = it.findViewById<EditText>(R.id.composer)
                        assertEquals("Edit receives exact raw Markdown", source, editor.text.toString())
                        editor.setText("я".repeat(4001))
                        assertFalse(it.findViewById<Button>(R.id.btn_save_edit).isEnabled)
                        it.findViewById<Button>(R.id.btn_save_edit).performClick()
                        assertFalse(ViewModelProvider(it)[ChatMemory::class.java].pending)
                    }
                    scenario.recreate(); ready(scenario)
                    scenario.onActivity {
                        assertEquals("я".repeat(4001), it.findViewById<EditText>(R.id.composer).text.toString())
                        it.findViewById<EditText>(R.id.composer).setText("я".repeat(4000))
                        assertTrue(it.findViewById<Button>(R.id.btn_save_edit).isEnabled)
                        it.findViewById<Button>(R.id.btn_cancel_edit).performClick()
                        assertEquals("🦀".repeat(4000), it.findViewById<EditText>(R.id.composer).text.toString())
                    }
                    scenario.recreate(); ready(scenario)
                    val deleteBody = body(scenario, own.localId)
                    scenario.onActivity { assertTrue(deleteBody.performAccessibilityAction(R.id.action_delete, null)) }
                    waitFor("Delete modal Cancel unavailable") { labelNodes(app.getString(R.string.cancel)).isNotEmpty() }
                    assertTrue(labelNodes(app.getString(R.string.cancel)).first().performAction(AccessibilityNodeInfo.ACTION_CLICK))
                    assertEquals(own, f.historyMessage(peer, own.localId))
                    assertEquals(beforeOutbox, f.outbox(0, 50))

                    shell("settings put system font_scale 2.0")
                    waitFor("200% font did not apply") {
                        var result = false; scenario.onActivity { result = it.resources.configuration.fontScale >= 1.9f }; result
                    }
                    ready(scenario)
                    val editBody = body(scenario, own.localId)
                    scenario.onActivity {
                        assertTrue(editBody.performAccessibilityAction(R.id.action_edit, null))
                        it.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
                    }
                    waitFor("landscape not restored") {
                        var result = false; scenario.onActivity {
                            result = it.resources.configuration.orientation == Configuration.ORIENTATION_LANDSCAPE &&
                                it.window.decorView.width > it.window.decorView.height && it.hasWindowFocus() &&
                                it.findViewById<View>(R.id.edit_banner).isShown
                        }; result
                    }
                    scenario.onActivity {
                        val editor = it.findViewById<EditText>(R.id.composer)
                        editor.setText("я".repeat(4001)); assertTrue(editor.requestFocus())
                        (it.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager).showSoftInput(editor, InputMethodManager.SHOW_IMPLICIT)
                    }
                    waitFor("landscape IME not visible") {
                        var result = false
                        scenario.onActivity { result = ViewCompat.getRootWindowInsets(it.window.decorView)?.isVisible(WindowInsetsCompat.Type.ime()) == true }
                        result
                    }
                    // Reachability includes the footer's native scroll when safety notices consume
                    // the remaining height; do not demand all footer rows simultaneously visible.
                    scenario.onActivity { it.findViewById<ScrollView>(R.id.chat_footer).scrollTo(0, 0) }
                    waitFor("overflow counter not reachable in landscape IME") {
                        var result = false
                        scenario.onActivity {
                            val counter = it.findViewById<TextView>(R.id.composer_counter); val rect = Rect()
                            result = counter.getGlobalVisibleRect(rect) && rect.height() == counter.height &&
                                counter.text.toString() == it.getString(R.string.message_counter_overflow, 4001, 4000)
                        }
                        result
                    }
                    scenario.onActivity {
                        val footer = it.findViewById<ScrollView>(R.id.chat_footer)
                        footer.scrollTo(0, footer.getChildAt(0).height)
                    }
                    var layoutState = ""
                    waitFor("composer/Save/Cancel not reachable with landscape IME", { layoutState }) {
                        var result = false
                        scenario.onActivity {
                            val ime = ViewCompat.getRootWindowInsets(it.window.decorView)?.isVisible(WindowInsetsCompat.Type.ime()) == true
                            val sizes = listOf(R.id.composer_counter, R.id.composer, R.id.btn_save_edit, R.id.btn_cancel_edit).map { id ->
                                    val view = it.findViewById<View>(id); val rect = Rect()
                                    val visible = view.isShown && view.getGlobalVisibleRect(rect)
                                    Triple(view, rect, visible)
                            }
                            layoutState = "ime=$ime focused=${it.currentFocus?.id} window=${it.window.decorView.width}x${it.window.decorView.height} min48=${NativeUi.dp(it, 48)} " +
                                sizes.joinToString { (view, rect, visible) -> "${it.resources.getResourceEntryName(view.id)} shown=$visible size=${view.width}x${view.height} rect=$rect" }
                            layoutState += " ancestors=" + generateSequence(it.findViewById<View>(R.id.composer)) { view -> view.parent as? View }.joinToString { view ->
                                val rect = Rect(); view.getGlobalVisibleRect(rect)
                                "${view.javaClass.simpleName} h=${view.height} sy=${view.scrollY} clip=${view.clipBounds} rect=$rect"
                            }
                            result = ime && sizes.all { (view, rect, visible) -> visible && rect.width() > 0 &&
                                rect.height() >= if (view.id == R.id.composer_counter) view.height else NativeUi.dp(it, 48) }
                            assertFalse(it.findViewById<Button>(R.id.btn_save_edit).isEnabled)
                        }
                        result
                    }
                    scenario.onActivity { it.findViewById<Button>(R.id.btn_cancel_edit).performClick() }
                    assertEquals(own, f.historyMessage(peer, own.localId))
                } finally {
                    scenario.onActivity {
                        if (previousClip == null) clipboard.clearPrimaryClip() else clipboard.setPrimaryClip(previousClip!!)
                        it.requestedOrientation = previousOrientation
                        (it.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(it.window.decorView.windowToken, 0)
                    }
                }
            }
        } finally {
            shell(if (previousFont == "null") "settings delete system font_scale" else "settings put system font_scale $previousFont")
            automation.serviceInfo = automation.serviceInfo.apply { flags = previousFlags }
        }
    }
}

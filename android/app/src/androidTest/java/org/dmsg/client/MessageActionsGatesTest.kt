package org.dmsg.client

import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.graphics.Rect
import android.os.SystemClock
import android.view.MotionEvent
import android.view.InputDevice
import android.view.View
import android.view.ViewGroup
import android.view.ViewConfiguration
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import java.io.File
import java.security.MessageDigest
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.MessageDirection

/** Operator-selected, real native/DNS actions on one fresh isolated account. No skips or fake facade. */
@RunWith(AndroidJUnit4::class)
class MessageActionsGatesTest {
    @get:Rule val testName = TestName()
    private val app get() = ApplicationProvider.getApplicationContext<Context>()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private fun gate() {
        assertTrue("isolated package required", app.packageName.endsWith(".gate"))
        val selected = InstrumentationRegistry.getArguments().getString("class").orEmpty().split(',')
        assertTrue("select exactly this operator gate", "${javaClass.name}#${testName.methodName}" in selected)
        val info = instrumentation.uiAutomation.serviceInfo
        info.flags = info.flags or android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        instrumentation.uiAutomation.serviceInfo = info
    }
    private fun input(name: String): String {
        val file = File(app.filesDir, name)
        assertTrue("bounded app-private fixture required", file.isFile && file.length() in 1..16_384)
        return file.readText()
    }
    private fun write(name: String, value: String) {
        File(app.filesDir, name).apply {
            writeText(value)
            assertTrue(setReadable(false, false)); assertTrue(setWritable(false, false))
            assertTrue(setReadable(true, true)); assertTrue(setWritable(true, true))
        }
    }
    private fun id(f: DmsgFacade): String = f.contactQrId(input("actions-peer.qr").trim())
    private fun proof() = JSONObject(input("actions-seed.json"))
    private fun rowId(index: Int) = proof().getJSONArray("rows").getJSONObject(index).getLong("id")
    private fun waitFor(label: String, condition: () -> Boolean) {
        val deadline = System.nanoTime() + 30_000_000_000L
        while (System.nanoTime() < deadline) {
            if (condition()) return
            Thread.sleep(75)
        }
        fail(label)
    }
    private fun views(view: View): List<View> = listOf(view) + if (view is ViewGroup)
        (0 until view.childCount).flatMap { views(view.getChildAt(it)) } else emptyList()
    private fun nodes(node: AccessibilityNodeInfo?): List<AccessibilityNodeInfo> = if (node == null) emptyList()
        else listOf(node) + (0 until node.childCount).flatMap { nodes(node.getChild(it)) }
    private fun clickLabel(label: String): Boolean {
        val roots = instrumentation.uiAutomation.windows.map { it.root } + instrumentation.uiAutomation.rootInActiveWindow
        val node = roots.flatMap(::nodes).firstOrNull { it.text?.toString() == label || it.contentDescription?.toString() == label }
        return node?.performAction(AccessibilityNodeInfo.ACTION_CLICK) == true
    }
    private fun hasLabel(label: String): Boolean {
        val roots = instrumentation.uiAutomation.windows.map { it.root } + instrumentation.uiAutomation.rootInActiveWindow
        return roots.flatMap(::nodes).any { it.text?.toString() == label || it.contentDescription?.toString() == label }
    }
    private fun showOverflow() {
        val resources = android.content.res.Resources.getSystem()
        for (name in listOf("floating_toolbar_open_overflow_description", "action_menu_overflow_description")) {
            val resource = resources.getIdentifier(name, "string", "android")
            if (resource != 0 && clickLabel(resources.getString(resource))) return
        }
    }
    private fun launch(contactId: String) = ActivityScenario.launch<ChatActivity>(
        Intent(app, ChatActivity::class.java).putExtra("peer", contactId).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    private fun ready(scenario: ActivityScenario<ChatActivity>) {
        waitFor("chat did not load", {
            var result = false
            scenario.onActivity { result = it.findViewById<Button>(R.id.btn_history_retry).isEnabled && it.findViewById<ListView>(R.id.messages).adapter.count > 0 }
            result
        })
    }
    private fun displayed(scenario: ActivityScenario<ChatActivity>, localId: Long, action: (View) -> Unit) {
        scenario.onActivity {
            val list = it.findViewById<ListView>(R.id.messages)
            val position = (0 until list.adapter.count).first { p -> list.adapter.getItemId(p) == localId }
            list.setSelection(position)
        }
        waitFor("stable message row unavailable", {
            var found = false
            scenario.onActivity {
                val list = it.findViewById<ListView>(R.id.messages)
                val child = (0 until list.childCount).map { p -> list.getChildAt(p) }.firstOrNull { v -> v.tag == localId }
                if (child != null && child.getGlobalVisibleRect(Rect())) { action(child); found = true }
            }
            found
        })
    }
    private fun beginEdit(scenario: ActivityScenario<ChatActivity>, localId: Long) {
        ready(scenario)
        displayed(scenario, localId) { assertTrue(it.performAccessibilityAction(R.id.action_edit, null)) }
        waitFor("edit banner unavailable", {
            var editing = false
            scenario.onActivity { editing = it.findViewById<View>(R.id.edit_banner).visibility == View.VISIBLE }
            editing
        })
    }
    private fun selectBody(scenario: ActivityScenario<ChatActivity>, localId: Long, text: String) {
        var textX = 0f
        var textY = 0f
        displayed(scenario, localId) { root ->
            val body = views(root).filterIsInstance<TextView>().first { it.text.toString() == text }
            assertTrue(body.isTextSelectable)
            assertTrue("selectable message must accept focus", body.requestFocus())
            val location = IntArray(2)
            body.getLocationOnScreen(location)
            textX = location[0] + body.totalPaddingLeft + NativeUi.dp(app, 8).toFloat()
            textY = location[1] + body.totalPaddingTop + (body.layout.getLineTop(0) + body.layout.getLineBottom(0)) / 2f
        }
        val down = SystemClock.uptimeMillis()
        fun touch(action: Int) {
            val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, textX, textY, 0)
            event.source = InputDevice.SOURCE_TOUCHSCREEN
            try { instrumentation.sendPointerSync(event) } finally { event.recycle() }
        }
        touch(MotionEvent.ACTION_DOWN)
        SystemClock.sleep(ViewConfiguration.getLongPressTimeout().toLong() + 250)
        touch(MotionEvent.ACTION_UP)
    }
    private fun cancelEdit(scenario: ActivityScenario<ChatActivity>) = scenario.onActivity {
        assertTrue(it.findViewById<Button>(R.id.btn_cancel_edit).performClick())
    }
    private fun openDelete(scenario: ActivityScenario<ChatActivity>, localId: Long, footerMenu: Boolean = false) {
        ready(scenario)
        displayed(scenario, localId) {
            if (footerMenu) assertTrue(views(it).filterIsInstance<TextView>().last().performLongClick())
            else assertTrue(it.performAccessibilityAction(R.id.action_delete, null))
        }
        if (footerMenu) {
            waitFor("footer whole-message menu unavailable", { hasLabel(app.getString(R.string.delete_whole_message)) })
            assertTrue(clickLabel(app.getString(R.string.delete_whole_message)))
        }
        waitFor("delete modal unavailable", { hasLabel(app.getString(R.string.delete_self)) })
    }
    private fun originalUnchanged(f: DmsgFacade, index: Int) {
        val saved = proof().getJSONArray("rows").getJSONObject(index)
        val row = f.historyMessage(id(f), saved.getLong("id"))
        assertEquals(saved.getString("mid"), row.messageIdHex)
        assertEquals(saved.getLong("time"), row.localTimestampMs)
        assertEquals(saved.getLong("seq"), row.serverSeq)
        assertEquals(saved.getLong("serverTime"), row.serverTimestampMs)
        android.database.sqlite.SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null,
            android.database.sqlite.SQLiteDatabase.OPEN_READONLY).use { sql ->
            sql.rawQuery("SELECT ciphertext FROM core_messages WHERE local_id=?", arrayOf(row.localId.toString())).use { result ->
                assertTrue(result.moveToFirst())
                val digest = MessageDigest.getInstance("SHA-256").digest(result.getBlob(0)).joinToString("") { "%02x".format(it) }
                assertEquals(saved.getString("cipherHash"), digest)
            }
        }
    }

    @Test fun seedThreeOwnTextsOverRecursiveDns() {
        gate()
        val f = Core.facade(app)
        assertTrue(f.account().authenticated)
        val peer = id(f)
        f.addQr(input("actions-peer.qr").trim()); f.accept(peer)
        assertEquals(listOf(peer), f.contacts(null, 100).first.map { it.contactId })
        try {
            assertTrue(f.reconnect() > 0)
            val values = JSONArray()
            for (text in listOf("Gate original edit", "Gate self only", "Gate delete both")) {
                val existing = f.historyPage(peer, null, 100).rows.singleOrNull { it.direction == MessageDirection.OUTGOING && it.text == text }
                val mid = existing?.messageIdHex ?: f.send(peer, text)
                f.retry()
                assertTrue(f.messageStatus(mid) in listOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED))
                val row = f.historyPage(peer, null, 100).rows.single { it.messageIdHex == mid }
                val digest = android.database.sqlite.SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null,
                    android.database.sqlite.SQLiteDatabase.OPEN_READONLY).use { sql ->
                    sql.rawQuery("SELECT ciphertext FROM core_messages WHERE local_id=?", arrayOf(row.localId.toString())).use { result ->
                        assertTrue(result.moveToFirst())
                        MessageDigest.getInstance("SHA-256").digest(result.getBlob(0)).joinToString("") { "%02x".format(it) }
                    }
                }
                values.put(JSONObject().put("id", row.localId).put("mid", mid).put("time", row.localTimestampMs)
                    .put("seq", row.serverSeq).put("serverTime", row.serverTimestampMs).put("cipherHash", digest))
            }
            assertEquals(3, f.historyPage(peer, null, 100).rows.size)
            write("actions-seed.json", JSONObject().put("rows", values).toString())
            write("actions-phone.qr", f.myQr())
        } finally { f.dnsStop() }
    }

    @Test fun editCopyCancelRecreateSaveAndLargeFontIme() {
        gate()
        val f = Core.facade(app)
        val peer = id(f)
        val localId = rowId(0)
        assertEquals(0uL, f.historyMessage(peer, localId).revision)
        launch(peer).use { scenario ->
            ready(scenario)
            scenario.onActivity { it.findViewById<EditText>(R.id.composer).setText("Gate normal draft") }
            selectBody(scenario, localId, "Gate original edit")
            waitFor("system Copy missing from real text selection", { hasLabel(android.content.res.Resources.getSystem().getString(android.R.string.copy)) })
            if (!hasLabel(app.getString(R.string.edit_whole_message))) showOverflow()
            waitFor("selection Edit action missing", { hasLabel(app.getString(R.string.edit_whole_message)) })
            assertTrue(clickLabel(app.getString(R.string.edit_whole_message)))
            waitFor("selection must edit the whole message", {
                var result = false
                scenario.onActivity { result = it.findViewById<EditText>(R.id.composer).text.toString() == "Gate original edit" && it.findViewById<View>(R.id.edit_banner).visibility == View.VISIBLE }
                result
            })
            scenario.onActivity { it.findViewById<EditText>(R.id.composer).setText("Gate unsaved edit draft") }
            scenario.recreate()
            scenario.onActivity { assertEquals("Gate unsaved edit draft", it.findViewById<EditText>(R.id.composer).text.toString()) }
            cancelEdit(scenario)
            scenario.onActivity { assertEquals("Gate normal draft", it.findViewById<EditText>(R.id.composer).text.toString()) }
            assertEquals(0uL, f.historyMessage(peer, localId).revision)
            beginEdit(scenario, localId)
            scenario.onActivity {
                assertTrue("200% font fixture required", it.resources.configuration.fontScale >= 1.9f)
                it.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
            }
            waitFor("landscape edit controls not restored", {
                var result = false
                scenario.onActivity {
                    result = it.resources.configuration.orientation == android.content.res.Configuration.ORIENTATION_LANDSCAPE &&
                        it.window.decorView.width > it.window.decorView.height && it.hasWindowFocus() &&
                        it.findViewById<View>(R.id.edit_banner).visibility == View.VISIBLE
                }
                result
            })
            scenario.onActivity {
                val composer = it.findViewById<EditText>(R.id.composer)
                composer.requestFocus()
                (it.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager).showSoftInput(composer, InputMethodManager.SHOW_IMPLICIT)
            }
            waitFor("landscape keyboard did not appear", {
                var result = false
                scenario.onActivity {
                    result = ViewCompat.getRootWindowInsets(it.window.decorView)?.isVisible(WindowInsetsCompat.Type.ime()) == true
                }
                result
            })
            waitFor("Save/Cancel clipped in landscape IME", {
                var result = false
                scenario.onActivity {
                    result = listOf(R.id.btn_save_edit, R.id.btn_cancel_edit).all { resource ->
                        val view = it.findViewById<Button>(resource); val rect = Rect()
                        view.isShown && view.getGlobalVisibleRect(rect) && rect.width() > 0 && rect.height() >= NativeUi.dp(it, 48)
                    }
                }
                result
            })
            scenario.onActivity {
                it.findViewById<EditText>(R.id.composer).setText("Gate edited original")
                assertTrue(it.findViewById<Button>(R.id.btn_save_edit).performClick())
            }
            waitFor("native edit not committed", { f.historyMessage(peer, localId).revision == 1uL })
            waitFor("normal draft not restored after Save", {
                var result = false
                scenario.onActivity { result = it.findViewById<View>(R.id.edit_banner).visibility == View.GONE && it.findViewById<EditText>(R.id.composer).text.toString() == "Gate normal draft" }
                result
            })
            scenario.recreate()
            scenario.onActivity { assertEquals("Gate normal draft", it.findViewById<EditText>(R.id.composer).text.toString()) }
            assertEquals("Gate edited original", f.historyMessage(peer, localId).text)
            originalUnchanged(f, 0)
        }
        f.dnsStop()
    }

    @Test fun selectionSurvivesUnchangedRetainedRefresh() {
        gate()
        val f = Core.facade(app)
        val row = f.historyMessage(id(f), rowId(0))
        launch(id(f)).use { scenario ->
            ready(scenario)
            selectBody(scenario, row.localId, row.text)
            val copy = android.content.res.Resources.getSystem().getString(android.R.string.copy)
            waitFor("Copy unavailable", { hasLabel(copy) })
            SystemClock.sleep(3_500)
            assertTrue("unchanged periodic refresh destroyed text selection", hasLabel(copy))
        }
    }

    @Test fun selfDeleteDefaultsCancelThenHidesOnlyHere() {
        gate()
        val f = Core.facade(app)
        val peer = id(f)
        val localId = rowId(1)
        launch(peer).use { scenario ->
            openDelete(scenario, localId)
            val self = instrumentation.uiAutomation.windows.flatMap { nodes(it.root) }.first { it.text?.toString() == app.getString(R.string.delete_self) }
            assertTrue("self-only must be selected initially", self.isChecked)
            assertTrue(clickLabel(app.getString(R.string.cancel)))
            assertFalse(f.historyMessage(peer, localId).hiddenSelf)
            openDelete(scenario, localId)
            assertTrue(clickLabel(app.getString(R.string.delete_confirm)))
            waitFor("local self-hide not committed", { f.historyMessage(peer, localId).hiddenSelf })
            val row = f.historyMessage(peer, localId)
            assertFalse(row.deletedAll); assertEquals("", row.text); assertEquals(0uL, row.revision)
            waitFor("hidden row remains in adapter", {
                var absent = false
                scenario.onActivity {
                    val list = it.findViewById<ListView>(R.id.messages)
                    absent = (0 until list.adapter.count).none { index -> list.adapter.getItemId(index) == localId }
                }
                absent
            })
            originalUnchanged(f, 1)
        }
    }

    @Test fun deleteEveryoneCommitsWithNoActiveNetwork() {
        gate()
        assertNull("offline operator fixture required", DnsNetwork.snapshot(app))
        val f = Core.facade(app)
        val peer = id(f)
        val localId = rowId(2)
        launch(peer).use { scenario ->
            openDelete(scenario, localId, footerMenu = true)
            assertTrue(clickLabel(app.getString(R.string.delete_everyone)))
            assertTrue(clickLabel(app.getString(R.string.delete_confirm)))
            waitFor("offline deletion not durable", { f.historyMessage(peer, localId).deletedAll })
            val row = f.historyMessage(peer, localId)
            assertEquals("", row.text); assertEquals(1uL, row.revision)
            assertEquals(DeliveryState.QUEUED, row.changeDeliveryState)
            waitFor("offline tombstone remains visible", {
                var absent = false
                scenario.onActivity {
                    val list = it.findViewById<ListView>(R.id.messages)
                    absent = (0 until list.adapter.count).none { index -> list.adapter.getItemId(index) == localId }
                }
                absent
            })
            originalUnchanged(f, 2)
        }
    }

    @Test fun retryAndVerifySavedActionsAfterNewProcess() {
        gate()
        val f = Core.facade(app)
        val peer = id(f)
        assertTrue(f.reconnect() > 0)
        f.retry()
        val rows = f.historyPage(peer, null, 100).rows
        assertEquals(3, rows.size)
        assertEquals("Gate edited original", f.historyMessage(peer, rowId(0)).text)
        assertTrue(f.historyMessage(peer, rowId(1)).hiddenSelf)
        assertTrue(f.historyMessage(peer, rowId(2)).deletedAll)
        assertTrue(f.historyMessage(peer, rowId(2)).changeDeliveryState in listOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED))
        for (index in 0..2) originalUnchanged(f, index)
        f.dnsStop()
    }
}

package org.dmsg.client

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** Optional public LAN resolver fixture; never changes the packaged trust anchor. */
internal fun configureInvitationGateNetwork(app: Context) {
    val file = File(app.filesDir, "gate-self-network.json")
    if (!file.exists()) return
    assertEquals("isolated resolver fixture only", "org.dmsg.client.gate", app.packageName)
    assertTrue("bounded public resolver fixture", file.isFile && file.length() in 1..4096)
    assertEquals("owner-only resolver fixture", 0, android.system.Os.stat(file.absolutePath).st_mode and 0x3f)
    val input = JSONObject(file.readText())
    assertEquals(setOf("resolvers"), input.keys().asSequence().toSet())
    val values = input.getJSONArray("resolvers")
    assertTrue("bounded controlled resolver fixture", values.length() in 1..8)
    val resolvers = (0 until values.length()).map(values::getString)
    val f = Core.facade(app) as UniFfiFacade
    TrustedServerProfile.configureIfFresh(f, { resolvers }, { app.assets.open(TrustedServerProfile.ASSET) })
    f.refreshDnsNetwork { true }
    f.dnsNetworkChanged(resolvers)
    assertEquals("controlled UDP resolver remains explicit", resolvers, f.dnsProfile()!!.resolvers)
}

/** Exact-method .gate contract in android/SELF_SERVICE_INVITATION_GATES.md. No secret arguments. */
class SelfServiceInvitationGatesTest {
    @get:Rule val name = TestName()
    private fun gate(): Context = ApplicationProvider.getApplicationContext<Context>().also {
        assertEquals("isolated target required", "org.dmsg.client.gate", it.packageName)
        assertEquals("select exactly one method", "${javaClass.name}#${name.methodName}",
            InstrumentationRegistry.getArguments().getString("class"))
        assertFalse("foreground service off", DmsgService.running(it))
        configureInvitationGateNetwork(it)
    }
    private fun privateFile(app: Context, name: String, maximum: Long) = File(app.filesDir, name).also {
        assertTrue("bounded owner fixture required", it.isFile && it.length() in 1..maximum)
        assertEquals("owner-only fixture", 0, android.system.Os.stat(it.absolutePath).st_mode and 0x3f)
    }
    private fun writePrivate(app: Context, name: String, text: String) {
        val file = File(app.filesDir, name)
        assertTrue("fresh output required", file.createNewFile())
        assertTrue(file.setReadable(false, false)); assertTrue(file.setWritable(false, false))
        assertTrue(file.setReadable(true, true)); assertTrue(file.setWritable(true, true))
        file.writeText(text)
    }
    private fun <T : android.app.Activity> await(s: ActivityScenario<T>, label: String, ready: (T) -> Boolean) {
        val deadline = System.nanoTime() + 120_000_000_000L
        while (System.nanoTime() < deadline) {
            var done = false; s.onActivity { done = ready(it) }
            if (done) return
            Thread.sleep(50)
        }
        fail(label)
    }
    private fun field(a: MainActivity, name: String): Any? = MainActivity::class.java.getDeclaredField(name).also { it.isAccessible = true }.get(a)
    private fun ready(a: MainActivity) = field(a, "state") == LaunchState.Authentication && field(a, "busy") == false && field(a, "policy") != null
    private fun memory(a: MainActivity) = field(a, "invitation") as InvitationMemory

    @Test fun senderIssueRecoverAndPrivateGrantThroughDns() {
        val app = gate(); val f = Core.facade(app)
        assertTrue("authenticated sender fixture required", f.account().authenticated)
        ActivityScenario.launch(InvitationsActivity::class.java).use { s ->
            await(s, "list ready") { !it.flow.busy }
            s.onActivity {
                assertNull("opening never creates a grant", it.flow.grant)
                assertTrue(it.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE != 0)
                it.findViewById<Button>(R.id.btn_invitation_create).performClick()
                it.findViewById<Button>(R.id.btn_invitation_create).performClick()
            }
            await(s, "issued active grant") { !it.flow.busy && it.flow.mayShare }
            var id: ByteArray? = null
            var phrase: CharArray? = null
            s.onActivity { id = it.flow.selected!!.copyOf(); phrase = it.flow.grant!!.phrase()!!.toCharArray() }
            try {
                s.recreate()
                await(s, "same id recovers original grant") { !it.flow.busy && it.flow.mayShare }
                s.onActivity {
                    assertTrue("same id recovered", id!!.contentEquals(it.flow.selected))
                    assertTrue("same phrase recovered", phrase!!.contentEquals(it.flow.grant!!.phrase()!!.toCharArray()))
                    writePrivate(app, "gate-self-issued.json", JSONObject()
                        .put("issueIdHex", id!!.joinToString("") { b -> "%02x".format(b.toInt() and 255) })
                        .put("phrase", phrase!!.concatToString()).toString())
                }
                assertTrue("own list contains id", f.listInvitations().invitations.any { it.issueId.contentEquals(id) })
                // Capture the exact private PNG export for a same-phone recipient fixture.
                val bitmap = InvitationImages.qr(f.normalizeInvitation(phrase!!.concatToString()))
                try {
                    val uri = InvitationShare.export(app, bitmap)
                    val output = File(app.filesDir, "gate-self-invitation.png")
                    assertTrue("fresh PNG output", output.createNewFile())
                    assertTrue(output.setReadable(false, false)); assertTrue(output.setWritable(false, false))
                    assertTrue(output.setReadable(true, true)); assertTrue(output.setWritable(true, true))
                    app.contentResolver.openInputStream(uri)!!.use { source -> output.outputStream().use { source.copyTo(it) } }
                    assertTrue("private PNG export roundtrip", output.inputStream().use(InvitationImages::decode) == f.normalizeInvitation(phrase!!.concatToString()))
                } finally { bitmap.eraseColor(0); bitmap.recycle() }
            } finally { phrase?.fill('\u0000') }
        }
    }

    @Test fun senderPngProviderReadAfterPauseAndRevokeThroughDns() {
        val app = gate(); val f = Core.facade(app)
        assertTrue("authenticated sender required", f.account().authenticated)
        val grantFile = privateFile(app, "gate-self-issued.json", 4096)
        val input = JSONObject(grantFile.readText())
        val id = input.getString("issueIdHex").chunked(2).map { it.toInt(16).toByte() }.toByteArray()
        val read = CountDownLatch(1)
        var readOk = false
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) { readOk = intent.getBooleanExtra("readPngAfterPause", false); read.countDown() }
        }
        if (Build.VERSION.SDK_INT >= 33) app.registerReceiver(receiver, IntentFilter("org.dmsg.client.gate.INVITATION_SHARE_READ"), Context.RECEIVER_EXPORTED)
        else app.registerReceiver(receiver, IntentFilter("org.dmsg.client.gate.INVITATION_SHARE_READ"))
        try {
            ActivityScenario.launch(InvitationsActivity::class.java).use { s ->
                await(s, "list ready") { !it.flow.busy }
                s.onActivity { it.flow.restore(null, id, null); it.findViewById<Button>(R.id.btn_invitation_refresh).performClick() }
                await(s, "active sender grant") { !it.flow.busy && it.flow.mayShare }
                await(s, "QR render enables real Share button") { it.findViewById<Button>(R.id.btn_invitation_share).isEnabled }
                s.onActivity { it.findViewById<Button>(R.id.btn_invitation_share).performClick() }
                val automation = uiAutomation()
                val deadline = System.nanoTime() + 30_000_000_000L
                var selected = false
                while (!selected && System.nanoTime() < deadline) {
                    if (Build.VERSION.SDK_INT >= 33) automation.clearCache()
                    val all = uiNodes(automation.rootInActiveWindow)
                    val target = all.firstOrNull { it.text?.toString() == "53 invitation PNG gate" ||
                        it.contentDescription?.toString()?.startsWith("53 invitation PNG gate") == true }
                    selected = target?.let {
                        var row: android.view.accessibility.AccessibilityNodeInfo? = it
                        repeat(4) {
                            if (row?.viewIdResourceName != "android:id/item") row = row?.parent
                        }
                        row?.takeIf { it.viewIdResourceName == "android:id/item" && it.isClickable }
                            ?.performAction(android.view.accessibility.AccessibilityNodeInfo.ACTION_CLICK)
                            ?: uiTap(automation, it)
                    } == true
                    if (!selected) {
                        all.firstOrNull { it.isScrollable }?.performAction(android.view.accessibility.AccessibilityNodeInfo.ACTION_SCROLL_FORWARD)
                        Thread.sleep(100)
                    }
                }
                assertTrue("real chooser exposes the owned PNG target", selected)
                assertTrue("granted target reads PNG after sender pause", read.await(60, TimeUnit.SECONDS))
                assertTrue("actual PNG decode after sender pause", readOk)
                s.moveToState(Lifecycle.State.RESUMED)
                await(s, "resume reloads active grant") { !it.flow.busy && it.flow.mayShare }
                s.onActivity { it.findViewById<Button>(R.id.btn_invitation_revoke).performClick() }
                await(s, "revoke terminal") { !it.flow.busy && it.flow.grant?.state == InvitationState.REVOKED }
                s.onActivity {
                    assertFalse(it.flow.mayShare)
                    assertTrue(it.findViewById<TextView>(R.id.invitation_phrase).text.isEmpty())
                }
                val terminal = f.issueInvitation(id)
                try { assertEquals(InvitationState.REVOKED, terminal.state); assertNull(terminal.phrase()) }
                finally { terminal.clear() }
            }
        } finally { app.unregisterReceiver(receiver); SecureStore.wipe(grantFile) }
    }

    @Test fun phraseSignupThroughDns() = recipient(false)
    @Test fun privatePngImportSignupThroughDns() = recipient(true)
    @Test fun systemPickerPngSignupThroughDns() = recipient(true, true)

    @Test fun reopenAccountAndOwnInvitationsThroughDns() {
        val app = gate(); val f = Core.facade(app)
        val before = f.account()
        assertTrue("existing accepted account required", before.authenticated)
        assertFalse("no credential fixture on reopen", File(app.filesDir, "gate-self-auth.json").exists())
        ActivityScenario.launch(MainActivity::class.java).use { s ->
            await(s, "new process saved-key DNS resume reaches dialogs") {
                field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false
            }
            assertTrue("bounded own list also resumes by saved key", f.listInvitations().invitations.size <= 8)
            assertEquals(before, f.account())
        }
        f.dnsStop()
    }

    private fun uiAutomation(): android.app.UiAutomation {
        val automation = InstrumentationRegistry.getInstrumentation().uiAutomation
        val info = automation.serviceInfo
        info.flags = info.flags or android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS or
            android.accessibilityservice.AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS
        info.eventTypes = android.view.accessibility.AccessibilityEvent.TYPES_ALL_MASK
        automation.serviceInfo = info
        return automation
    }
    private fun uiNodes(root: android.view.accessibility.AccessibilityNodeInfo?): List<android.view.accessibility.AccessibilityNodeInfo> =
        if (root == null) emptyList() else listOf(root) + (0 until root.childCount).flatMap { uiNodes(root.getChild(it)) }
    private fun uiTap(automation: android.app.UiAutomation, node: android.view.accessibility.AccessibilityNodeInfo): Boolean {
            if (!node.isEnabled || !node.isVisibleToUser) return false
            val bounds = android.graphics.Rect().also { node.getBoundsInScreen(it) }
            if (bounds.isEmpty) return false
            val time = android.os.SystemClock.uptimeMillis()
            fun event(action: Int): android.view.MotionEvent {
                val pointer = android.view.MotionEvent.PointerProperties().apply {
                    id = 0; toolType = android.view.MotionEvent.TOOL_TYPE_FINGER
                }
                val position = android.view.MotionEvent.PointerCoords().apply {
                    x = bounds.exactCenterX(); y = bounds.exactCenterY(); pressure = 1f; size = 1f
                }
                return android.view.MotionEvent.obtain(time, android.os.SystemClock.uptimeMillis(), action, 1,
                    arrayOf(pointer), arrayOf(position), 0, 0, 1f, 1f, 0, 0,
                    android.view.InputDevice.SOURCE_TOUCHSCREEN, 0)
            }
            val down = event(android.view.MotionEvent.ACTION_DOWN)
            val started = try { automation.injectInputEvent(down, true) } finally { down.recycle() }
            Thread.sleep(100)
            val up = event(android.view.MotionEvent.ACTION_UP)
            return try { automation.injectInputEvent(up, true) && started } finally { up.recycle() }
    }
    private fun selectOwnedPickerImage() {
        val automation = uiAutomation()
        fun inDirectory(node: android.view.accessibility.AccessibilityNodeInfo): Boolean {
            var current: android.view.accessibility.AccessibilityNodeInfo? = node
            repeat(8) {
                if (current?.viewIdResourceName?.endsWith("/dir_list") == true) return true
                current = current?.parent
            }
            return false
        }
        val deadline = System.nanoTime() + 30_000_000_000L
        var openedDownloads = false
        var openedRoots = false
        var openedSearch = false
        var enteredSearch = false
        var listMode = false
        var rootPackage = "none"
        var nodeCount = 0
        while (System.nanoTime() < deadline) {
            if (android.os.Build.VERSION.SDK_INT >= 33) automation.clearCache()
            val root = automation.rootInActiveWindow
            rootPackage = root?.packageName?.toString() ?: "none"
            val all = uiNodes(root)
            nodeCount = all.size
            val listButton = all.firstOrNull { it.viewIdResourceName?.endsWith("/sub_menu_list") == true }
            if (!listMode && listButton != null && uiTap(automation, listButton)) {
                listMode = true; Thread.sleep(200); continue
            }
            val chosen = all.firstOrNull { inDirectory(it) && it.text?.toString() == "dmsg-self-invite-gate.png" }
                ?: all.firstOrNull { inDirectory(it) && it.contentDescription?.toString()?.startsWith("dmsg-self-invite-gate.png") == true }
            if (chosen != null && uiTap(automation, chosen)) return
            if (!openedDownloads) all.firstOrNull { it.text?.toString() in listOf("Downloads", "Загрузки") }?.let {
                openedDownloads = uiTap(automation, it)
            }
            if (!openedDownloads && !openedRoots) all.firstOrNull {
                it.contentDescription?.toString() in listOf("Show roots", "Показать корневые папки")
            }?.let { openedRoots = uiTap(automation, it) }
            if (openedDownloads && !openedSearch) all.firstOrNull {
                it.viewIdResourceName?.endsWith("/option_menu_search") == true
            }?.let { openedSearch = uiTap(automation, it) }
            if (openedSearch && !enteredSearch) all.firstOrNull {
                it.isEditable
            }?.let {
                val args = android.os.Bundle().apply { putCharSequence(
                    android.view.accessibility.AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                    "dmsg-self-invite-gate.png") }
                enteredSearch = it.performAction(android.view.accessibility.AccessibilityNodeInfo.ACTION_SET_TEXT, args)
                if (enteredSearch) {
                    automation.injectInputEvent(android.view.KeyEvent(android.view.KeyEvent.ACTION_DOWN, android.view.KeyEvent.KEYCODE_ENTER), true)
                    automation.injectInputEvent(android.view.KeyEvent(android.view.KeyEvent.ACTION_UP, android.view.KeyEvent.KEYCODE_ENTER), true)
                }
            }
            Thread.sleep(100)
        }
        fail("system image picker must expose the owned test PNG; package=$rootPackage nodes=$nodeCount roots=$openedRoots downloads=$openedDownloads search=$openedSearch query=$enteredSearch")
    }

    private fun recipient(image: Boolean, picker: Boolean = false) {
        val app = gate(); val f = Core.facade(app)
        assertFalse("fresh recipient required", f.account().authenticated)
        val credentials = privateFile(app, "gate-self-auth.json", 4096)
        val fixture = JSONObject(credentials.readText())
        assertEquals(setOf("login", "password"), fixture.keys().asSequence().toSet())
        val invite = privateFile(app, if (image) "gate-self-invitation.png" else "gate-self-phrase.txt", if (image) InvitationImages.MAX_BYTES.toLong() else 256L)
        try {
            ActivityScenario.launch(MainActivity::class.java).use { s ->
                await(s, "packaged profile DNS policy ready", ::ready)
                s.onActivity {
                    if (picker) it.window.addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                    assertEquals(AuthAction.Signup, field(it, "action"))
                    assertEquals(View.VISIBLE, it.findViewById<View>(R.id.invitation_group).visibility)
                    if (picker) it.findViewById<Button>(R.id.btn_invitation_image).performClick()
                    else if (image) it.importInvitationImage { invite.inputStream() }
                    else {
                        it.findViewById<EditText>(R.id.auth_invitation_phrase).setText(invite.readText())
                        it.findViewById<Button>(R.id.btn_invitation_phrase).performClick()
                    }
                }
                if (picker) selectOwnedPickerImage()
                var inputState = ""
                try {
                    await(s, "explicit input accepted without signup") {
                        inputState = "state=${field(it, "state")} busy=${field(it, "busy")} pendingImage=${field(it, "pendingInvitationImage") != null} policy=${field(it, "policy") != null} status=${it.findViewById<TextView>(R.id.status).text}"
                        field(it, "busy") == false && memory(it).hasInvitation
                    }
                } catch (e: AssertionError) { throw AssertionError("input lifecycle: $inputState", e) }
                assertFalse("import never submits", f.account().authenticated)
                s.onActivity {
                    it.findViewById<EditText>(R.id.auth_login).setText("ab")
                    it.findViewById<EditText>(R.id.auth_password).setText(fixture.getString("password"))
                    it.findViewById<Button>(R.id.btn_auth_submit).performClick()
                    assertTrue("local preflight retains invite", memory(it).hasInvitation)
                    assertTrue("local preflight retains password", it.findViewById<EditText>(R.id.auth_password).text.isNotEmpty())
                    it.findViewById<EditText>(R.id.auth_login).setText(fixture.getString("login"))
                    it.findViewById<Button>(R.id.btn_auth_submit).performClick()
                    assertFalse(memory(it).hasInvitation)
                    assertTrue(it.findViewById<EditText>(R.id.auth_password).text.isEmpty())
                }
                await(s, "real DNS explicit signup reaches dialogs") { field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false }
                assertTrue(f.account().authenticated)
                writePrivate(app, "gate-self-account.json", JSONObject().put("contactId", f.account().contactId).toString())
            }
        } finally { SecureStore.wipe(credentials); SecureStore.wipe(invite); f.dnsStop() }
    }

    @Test fun signupDefaultLoginGuardsAndForegroundValidation() {
        val app = gate(); assertFalse("fresh recipient required", Core.facade(app).account().authenticated)
        ActivityScenario.launch(MainActivity::class.java).use { s ->
            await(s, "authentication policy ready", ::ready)
            s.onActivity {
                assertEquals(AuthAction.Signup, field(it, "action"))
                it.findViewById<EditText>(R.id.auth_invitation_phrase).setText("not valid words")
                it.findViewById<Button>(R.id.btn_invitation_phrase).performClick()
                assertEquals("not valid words", it.findViewById<EditText>(R.id.auth_invitation_phrase).text.toString())
                it.findViewById<Button>(R.id.btn_login).performClick()
                assertEquals(View.GONE, it.findViewById<View>(R.id.invitation_group).visibility)
                it.importInvitationImage { fail("login handler must not open image"); throw AssertionError() }
                it.importInvitationFile { fail("login handler must not open raw file"); throw AssertionError() }
            }
            s.moveToState(Lifecycle.State.CREATED); s.moveToState(Lifecycle.State.RESUMED)
            await(s, "login selection survives refresh", ::ready)
            s.onActivity { assertEquals(AuthAction.Login, field(it, "action")); it.findViewById<Button>(R.id.btn_signup).performClick() }
            s.recreate(); await(s, "signup selection recreated", ::ready)
            s.onActivity {
                assertEquals(AuthAction.Signup, field(it, "action"))
                assertFalse(memory(it).hasInvitation)
                assertTrue(it.findViewById<EditText>(R.id.auth_invitation_phrase).text.isEmpty())
            }
        }
    }

    @Test fun rasterBoundsRoundTripAndStrictInvitationRoute() {
        val app = gate(); val f = Core.facade(app)
        val bitmap = InvitationImages.qr("A".repeat(43))
        val out = java.io.ByteArrayOutputStream()
        val png = try { assertTrue(bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)); out.toByteArray() }
            finally { bitmap.eraseColor(0); bitmap.recycle(); out.reset(); out.close() }
        try {
            val decoded = png.inputStream().use(InvitationImages::decode)
            assertEquals("A".repeat(43), f.normalizeInvitation(decoded))
            assertTrue(QrGate.route(decoded).isFailure)
            assertTrue("encoded bound", runCatching { java.io.ByteArrayInputStream(ByteArray(InvitationImages.MAX_BYTES + 1)).use(InvitationImages::decode) }.isFailure)
            assertTrue("SVG rejected", runCatching { "<svg/>".byteInputStream().use(InvitationImages::decode) }.isFailure)
            assertTrue("contact URI cannot authorize signup", runCatching { f.normalizeInvitation("dmsg://contact/AAAA") }.isFailure)
            assertTrue("network URI cannot authorize signup", runCatching { f.normalizeInvitation("https://example.invalid/") }.isFailure)
        } finally { png.fill(0) }
    }
}

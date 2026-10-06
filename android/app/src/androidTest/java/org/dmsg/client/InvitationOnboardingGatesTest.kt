package org.dmsg.client

import android.content.Context
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import java.io.ByteArrayInputStream
import java.io.File
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.QrKind
import uniffi.dmsg_core.RegistrationPolicy

/** Selected .gate only. Fixtures are private files; no secrets or resolver overrides in args. */
class InvitationOnboardingGatesTest {
    @get:Rule val name = TestName()

    private fun gate(): Context {
        val app = ApplicationProvider.getApplicationContext<Context>()
        assertEquals("isolated target required", "org.dmsg.client.gate", app.packageName)
        assertEquals("select exactly one method", "${javaClass.name}#${name.methodName}",
            InstrumentationRegistry.getArguments().getString("class"))
        assertFalse("foreground service must be off", DmsgService.running(app))
        return app
    }

    private fun field(activity: MainActivity, name: String): Any? = MainActivity::class.java.getDeclaredField(name)
        .also { it.isAccessible = true }.get(activity)

    private fun memory(activity: MainActivity) = field(activity, "invitation") as InvitationMemory

    private fun await(scenario: ActivityScenario<MainActivity>, label: String, predicate: (MainActivity) -> Boolean) {
        val deadline = System.nanoTime() + 120_000_000_000L
        while (System.nanoTime() < deadline) {
            var ready = false
            scenario.onActivity { ready = predicate(it) }
            if (ready) return
            Thread.sleep(50)
        }
        fail(label)
    }

    private fun privateFile(app: Context, name: String, maximum: Long): File = File(app.filesDir, name).also {
        assertTrue("required bounded private fixture", it.isFile && it.length() in 1..maximum)
        val mode = android.system.Os.stat(it.absolutePath).st_mode
        assertEquals("fixture must be owner-only", 0, mode and 0x3f)
    }

    private fun writeProof(app: Context, id: String) {
        val file = File(app.filesDir, "gate-invite-account.json")
        assertTrue("do not overwrite prior evidence", file.createNewFile())
        assertTrue(file.setReadable(false, false)); assertTrue(file.setWritable(false, false))
        assertTrue(file.setReadable(true, true)); assertTrue(file.setWritable(true, true))
        file.writeText(JSONObject().put("contactId", id).toString())
    }

    @Test fun canonicalParserOnlyInGatePackage() {
        gate()
        val token = "A".repeat(43) // Synthetic, no real administrator secret.
        val raw = InvitationInput.parse(token)
        try {
            assertArrayEquals(raw, InvitationInput.read(ByteArrayInputStream((token + "\n").toByteArray())))
            for (bad in listOf(token + "=", "A".repeat(42) + "B", "dmsg://invite/$token", " " + token)) {
                assertTrue("malformed invitation rejected", runCatching { InvitationInput.parse(bad) }.isFailure)
            }
            assertTrue(QrGate.route(token).isFailure)
            assertEquals(QrKind.SERVER, QrGate.route("dmsg://server/AAAA").getOrThrow())
            assertEquals(QrKind.CONTACT, QrGate.route("dmsg://contact/AAAA").getOrThrow())
        } finally { raw.fill('\u0000') }
    }

    @Test fun simpleAuthLabelsAndSignupExampleOnlyInGatePackage() {
        val app = gate()
        val f = Core.facade(app)
        assertFalse("fresh disposable account required", f.account().authenticated)
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                await(scenario, "authentication ready") {
                    field(it, "state") == LaunchState.Authentication && field(it, "busy") == false && field(it, "policy") != null
                }
                fun labelsAndHint(expected: String) = scenario.onActivity {
                    assertEquals("Логин", it.findViewById<TextView>(R.id.auth_login_label).text.toString())
                    assertEquals("Пароль", it.findViewById<TextView>(R.id.auth_password_label).text.toString())
                    assertEquals(expected, it.findViewById<EditText>(R.id.auth_login).hint.toString())
                    assertTrue(it.findViewById<EditText>(R.id.auth_password).hint.isNullOrEmpty())
                }
                labelsAndHint("")
                scenario.onActivity { it.findViewById<Button>(R.id.btn_signup).performClick() }
                labelsAndHint("Например, marina53")
                scenario.onActivity { it.findViewById<Button>(R.id.btn_login).performClick() }
                labelsAndHint("")
                scenario.onActivity {
                    it.findViewById<EditText>(R.id.auth_login).setText("ab")
                    it.findViewById<EditText>(R.id.auth_password).setText("synthetic password")
                    it.findViewById<Button>(R.id.btn_auth_submit).performClick()
                }
                await(scenario, "ordinary validation error") {
                    field(it, "busy") == false && it.findViewById<TextView>(R.id.status).text.toString() ==
                        "Логин должен содержать от 3 до 32 знаков"
                }
                scenario.onActivity {
                    assertTrue(it.findViewById<EditText>(R.id.auth_password).text.isEmpty())
                    assertFalse(memory(it).hasInvitation)
                    // Dispose the synthetic form context, never save it into the
                    // device's password manager when this test activity closes.
                    it.getSystemService(android.view.autofill.AutofillManager::class.java)?.cancel()
                }
                assertFalse("local invalid input never creates an account", f.account().authenticated)
            }
        } finally { f.dnsStop() }
    }

    /** Real activity-result lifecycle, injected synthetic decoder text; NOT optical evidence. */
    @Test fun scannerResultLifecycleOnlyInGatePackage() {
        val app = gate()
        val f = Core.facade(app)
        assertFalse("fresh disposable account required", f.account().authenticated)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            await(scenario, "trusted profile authentication ready") {
                field(it, "state") == LaunchState.Authentication && field(it, "busy") == false && field(it, "policy") != null
            }
            val monitor = instrumentation.addMonitor(ScannerActivity::class.java.name, null, false)
            try {
                scenario.onActivity {
                    assertEquals(AuthAction.Login, field(it, "action"))
                    assertEquals(View.VISIBLE, it.findViewById<View>(R.id.btn_invitation_scan).visibility)
                    it.findViewById<Button>(R.id.btn_invitation_scan).performClick()
                }
                val scanner = instrumentation.waitForMonitorWithTimeout(monitor, 10_000) as? ScannerActivity
                assertNotNull("scanner opened from login", scanner)
                instrumentation.runOnMainSync {
                    ScannerActivity::class.java.getDeclaredMethod("onText", String::class.java)
                        .also { it.isAccessible = true }.invoke(scanner, "A".repeat(43))
                }
                await(scenario, "one-shot scanner result survives normal return to signup") {
                    field(it, "state") == LaunchState.Authentication && field(it, "busy") == false &&
                        field(it, "action") == AuthAction.Signup && memory(it).hasInvitation
                }
                scenario.onActivity {
                    assertEquals("Приглашение считано", it.findViewById<TextView>(R.id.invitation_status).text.toString())
                    it.findViewById<Button>(R.id.btn_invitation_cancel).performClick()
                    assertFalse(memory(it).hasInvitation)
                }
                assertFalse("synthetic scan is not account creation", f.account().authenticated)
            } finally { instrumentation.removeMonitor(monitor); f.dnsStop() }
        }
    }

    /** Fresh .gate with packaged profile; invokes the exact SAF stream importer, not optical scan. */
    @Test fun privateFileImportSignupThroughRecursiveDns() {
        val app = gate()
        val credentials = privateFile(app, "gate-invite-auth.json", 4096)
        val invitation = privateFile(app, "gate-invitation.txt", InvitationInput.FILE_MAX.toLong())
        val f = Core.facade(app)
        try {
            val fixture = JSONObject(credentials.readText())
            assertEquals("credentials only, no profile/resolver injection", setOf("login", "password"), fixture.keys().asSequence().toSet())
            assertTrue(f.isReady())
            assertFalse("fresh disposable account required", f.account().authenticated)
            assertNull("fresh disposable profile required", f.dnsProfile())
            assertTrue("build with serverProfileFile", TrustedServerProfile.ASSET in app.assets.list("").orEmpty())
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                await(scenario, "packaged server and real DNS policy ready") {
                    field(it, "state") == LaunchState.Authentication && field(it, "busy") == false &&
                        field(it, "policy") == RegistrationPolicy.INVITE_ONLY
                }
                val profile = f.dnsProfile()!!
                assertEquals("production network resolvers", DnsNetwork.resolvers(app), profile.resolvers)
                assertFalse("public profile alone cannot create account", f.account().authenticated)
                fun importFile() {
                    scenario.onActivity { it.importInvitationFile { invitation.inputStream() } }
                    await(scenario, "bounded file import selects signup") {
                        field(it, "busy") == false && field(it, "action") == AuthAction.Signup && memory(it).hasInvitation
                    }
                    scenario.onActivity {
                        assertEquals("Приглашение считано", it.findViewById<TextView>(R.id.invitation_status).text.toString())
                        assertEquals(View.VISIBLE, it.findViewById<View>(R.id.invitation_group).visibility)
                        assertTrue("import has no implicit submit", it.findViewById<EditText>(R.id.auth_password).text.isEmpty())
                    }
                    assertFalse("import is not server validation/signup", f.account().authenticated)
                }
                importFile()
                scenario.onActivity {
                    it.findViewById<Button>(R.id.btn_login).performClick()
                    assertFalse("action change wipes invitation", memory(it).hasInvitation)
                }
                importFile()
                scenario.onActivity {
                    it.findViewById<Button>(R.id.btn_invitation_cancel).performClick()
                    assertFalse("cancel wipes invitation", memory(it).hasInvitation)
                    it.importInvitationFile { ByteArrayInputStream("dmsg://invite/not-raw".toByteArray()) }
                }
                await(scenario, "malformed file leaves no invitation") { field(it, "busy") == false && !memory(it).hasInvitation }
                assertFalse(f.account().authenticated)
                importFile()
                scenario.moveToState(Lifecycle.State.CREATED)
                scenario.moveToState(Lifecycle.State.RESUMED)
                await(scenario, "background return reloads policy without credentials") {
                    field(it, "state") == LaunchState.Authentication && field(it, "busy") == false && field(it, "policy") != null
                }
                scenario.onActivity { assertFalse("background wipes invitation", memory(it).hasInvitation) }
                importFile()
                scenario.recreate()
                await(scenario, "recreation reloads auth without invitation") {
                    field(it, "state") == LaunchState.Authentication && field(it, "busy") == false && field(it, "policy") != null
                }
                scenario.onActivity { assertFalse("no saved-state token", memory(it).hasInvitation) }
                importFile()
                scenario.onActivity {
                    it.findViewById<EditText>(R.id.auth_login).setText(fixture.getString("login"))
                    it.findViewById<EditText>(R.id.auth_password).setText(fixture.getString("password"))
                    assertTrue(it.findViewById<Button>(R.id.btn_auth_submit).isEnabled)
                    it.findViewById<Button>(R.id.btn_auth_submit).performClick()
                    assertFalse(memory(it).hasInvitation)
                    assertTrue(it.findViewById<EditText>(R.id.auth_password).text.isEmpty())
                }
                await(scenario, "explicit signup reaches dialogs through real DNS") {
                    field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false
                }
                val account = f.account()
                assertTrue(account.authenticated)
                assertNotNull(account.contactId)
                scenario.recreate()
                await(scenario, "authenticated recreation goes to dialogs") {
                    field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false
                }
                assertEquals(account, Core.facade(app).account())
                assertEquals(profile.fingerprint, f.dnsProfile()!!.fingerprint)
                writeProof(app, account.contactId!!)
            }
            assertTrue("key-only reconnect", f.reconnect() > 0)
            assertEquals("ready", f.dnsStatus())
        } finally {
            f.dnsStop()
            SecureStore.wipe(credentials); SecureStore.wipe(invitation)
        }
    }

    /** Select in a new instrumentation process after signup. No auth input is accepted. */
    @Test fun reopenImportedAccountWithSavedDeviceKey() {
        val app = gate()
        val proof = privateFile(app, "gate-invite-account.json", 4096)
        val fixture = JSONObject(proof.readText())
        assertEquals(setOf("contactId"), fixture.keys().asSequence().toSet())
        val f = Core.facade(app)
        try {
            assertFalse(File(app.filesDir, "gate-invite-auth.json").exists())
            assertFalse(File(app.filesDir, "gate-invitation.txt").exists())
            val account = AccountInfo(true, fixture.getString("contactId"))
            assertEquals(account, f.account())
            val before = f.dnsProfile()!!
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                await(scenario, "reopened account goes to dialogs") {
                    field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false
                }
                scenario.onActivity { assertFalse(memory(it).hasInvitation) }
            }
            assertEquals(before.domain, f.dnsProfile()!!.domain)
            assertEquals(before.fingerprint, f.dnsProfile()!!.fingerprint)
            assertTrue("actual recursive DNS device-key resume", f.reconnect() > 0)
            assertEquals("ready", f.dnsStatus())
            assertTrue(f.fetch().received.isEmpty())
            assertTrue(f.fetch().received.isEmpty())
            assertEquals(account, Core.facade(app).account())
        } finally { f.dnsStop(); SecureStore.wipe(proof) }
    }
}

package org.dmsg.client

import android.app.LocaleManager
import android.app.NotificationManager
import android.content.Context
import android.content.res.Configuration
import android.os.LocaleList
import android.view.ContextThemeWrapper
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.view.autofill.AutofillManager
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.FfiException

/** Exact-method disposable package only; never change main or the device system language. */
class LocaleGatesTest {
    @get:Rule val name = TestName()

    private fun gate(): Context = ApplicationProvider.getApplicationContext<Context>().also {
        assertEquals("org.dmsg.client.gate", it.packageName)
        assertEquals("${javaClass.name}#${name.methodName}", InstrumentationRegistry.getArguments().getString("class"))
        assertFalse("foreground service must initially be off", DmsgService.running(it))
    }

    private fun localized(app: Context, tags: String): Context = app.createConfigurationContext(
        Configuration(app.resources.configuration).apply { setLocales(LocaleList.forLanguageTags(tags)) }
    )

    private fun await(label: String, ready: () -> Boolean) {
        val deadline = System.nanoTime() + 90_000_000_000L
        while (System.nanoTime() < deadline) {
            if (ready()) return
            Thread.sleep(50)
        }
        fail(label)
    }

    private fun field(activity: MainActivity, name: String): Any? = MainActivity::class.java.getDeclaredField(name)
        .also { it.isAccessible = true }.get(activity)

    @Test fun preferredLocalesAndSafeErrorsOnlyInGatePackage() {
        val app = gate()
        for ((tags, login) in listOf("en" to "Login", "ru" to "Логин", "fr" to "Login",
            "en,ru" to "Login", "ru,en" to "Логин", "fr,ru" to "Логин")) {
            val res = localized(app, tags).resources
            assertEquals("Android preferred-language resolution: $tags", login, res.getString(R.string.auth_login))
            assertEquals(if (login == "Login") "Password" else "Пароль", res.getString(R.string.auth_password))
            val secret = "private-native-password-invitation"
            for (error in listOf(FfiException.Transport(secret), FfiException.BadQr(secret),
                FfiException.Store(secret), FfiException.Protocol(secret), FfiException.Server(secret))) {
                assertFalse(humanError(res, ffiError(error)).contains(secret))
            }
            assertEquals(res.getString(R.string.error_operation), humanError(res, IllegalStateException(secret)))
            assertEquals(res.getString(R.string.error_operation), humanError(res, DmsgError(secret)))
            val lost = humanError(res, DmsgError(secret, ErrorKind.StorageKeyLost))
            assertFalse(lost.contains(secret)); assertTrue(lost.contains("Keystore"))
            assertEquals(res.getString(R.string.error_pin), humanError(res, ffiError(FfiException.PinMismatch())))
            assertEquals(res.getString(R.string.delivery_delivered), deliveryLabel(res, DeliveryState.DELIVERED))
            assertEquals(res.getString(R.string.trust_changed), trustLabel(res, Dialog("PUBLIC-ID", "accepted", true, true)))
            val short = runCatching { AuthForm.validate(AuthAction.Login, null, "abc", "short", null) }.exceptionOrNull()!!
            val long = runCatching { AuthForm.validate(AuthAction.Login, null, "abc", "é".repeat(65), null) }.exceptionOrNull()!!
            assertEquals(res.getString(R.string.error_password_short), humanError(res, short))
            assertEquals(res.getString(R.string.error_password_long), humanError(res, long))
            assertNotEquals(humanError(res, short), humanError(res, long))
        }
    }

    @Test fun retainedSendStateRendersCurrentResourcesOnlyInGatePackage() {
        val app = gate()
        val ru = localized(app, "ru").resources
        val en = localized(app, "en").resources
        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            val memory = ChatMemory()
            memory.draft = "User text is never translated"
            memory.outgoing.begin()
            val uncertain = TextSendOutcome.Uncertain(10, memory.draft)
            val activity = ChatActivity()
            ChatActivity::class.java.getDeclaredField("memory").also { it.isAccessible = true }.set(activity, memory)
            val complete = ChatActivity::class.java.getDeclaredMethod("completeSend", TextSendOutcome::class.java, DeliveryState::class.java)
                .also { it.isAccessible = true }
            complete.invoke(activity, uncertain, null)
            val action = memory.action
            assertEquals(ru.getString(R.string.send_uncertain), action(ru))
            assertEquals(en.getString(R.string.send_uncertain), action(en))
            assertSame(uncertain, memory.uncertain)
            assertEquals(uncertain.text, memory.draft)
            complete.invoke(activity, TextSendOutcome.Saved("PUBLIC-MESSAGE-ID", true), DeliveryState.DELIVERED)
            assertNull(memory.uncertain)
            assertEquals("", memory.draft)
            assertEquals(ru.getString(R.string.message_saved, deliveryLabel(ru, DeliveryState.DELIVERED)) + "\n" + ru.getString(R.string.send_recovered), memory.action(ru))
            assertEquals(en.getString(R.string.message_saved, deliveryLabel(en, DeliveryState.DELIVERED)) + "\n" + en.getString(R.string.send_recovered), memory.action(en))
        }
    }

    @Test fun englishLargeFontDisclosuresOnlyInGatePackage() {
        val app = gate()
        val configured = app.createConfigurationContext(Configuration(app.resources.configuration).apply {
            setLocales(LocaleList.forLanguageTags("en")); fontScale = 2f
        })
        val themed = ContextThemeWrapper(configured, R.style.Theme_Dmsg)
        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            val root = LayoutInflater.from(themed).inflate(R.layout.activity_storage, null)
            val width = app.resources.displayMetrics.widthPixels
            root.measure(View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED))
            root.layout(0, 0, width, root.measuredHeight)
            fun textViews(view: View): List<TextView> = (if (view is TextView) listOf(view) else emptyList()) +
                if (view is ViewGroup) (0 until view.childCount).flatMap { textViews(view.getChildAt(it)) } else emptyList()
            val disclosure = textViews(root).single { it.text.toString() == themed.getString(R.string.reinstall_loss) }
            assertTrue("English security warning wraps at 200% font", disclosure.lineCount > 1)
            assertTrue("all warning lines fit", disclosure.measuredHeight >= disclosure.layout.height + disclosure.compoundPaddingTop + disclosure.compoundPaddingBottom)
            assertTrue(disclosure.text.contains("Keystore"))
            assertEquals(themed.getString(R.string.storage_password_disclosure), textViews(root).single {
                it.text.toString() == themed.getString(R.string.storage_password_disclosure)
            }.text.toString())
        }
    }

    @Test fun authConfigurationClearsSecretsAndUpdatesLabelsOnlyInGatePackage() {
        val app = gate()
        assertTrue("device locale fixture requires API33+", android.os.Build.VERSION.SDK_INT >= 33)
        val manager = app.getSystemService(LocaleManager::class.java)
        val original = manager.applicationLocales
        val f = Core.facade(app)
        assertFalse("fresh disposable account required", f.account().authenticated)
        try {
            manager.applicationLocales = LocaleList.forLanguageTags("ru")
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                fun ready(login: String): Boolean {
                    var ok = false
                    scenario.onActivity {
                        ok = field(it, "state") == LaunchState.Authentication && field(it, "busy") == false &&
                            field(it, "policy") != null && it.findViewById<TextView>(R.id.auth_login_label).text.toString() == login
                    }
                    return ok
                }
                await("Russian auth ready") { ready("Логин") }
                scenario.onActivity {
                    it.findViewById<Button>(R.id.btn_signup).performClick()
                    (field(it, "invitation") as InvitationMemory).replace(InvitationInput.parse("A".repeat(43)))
                    it.findViewById<EditText>(R.id.auth_password).setText("synthetic draft password")
                    it.getSystemService(AutofillManager::class.java)?.cancel()
                }
                manager.applicationLocales = LocaleList.forLanguageTags("en")
                await("English auth ready after Android recreation") { ready("Login") }
                scenario.onActivity {
                    assertEquals("Password", it.findViewById<TextView>(R.id.auth_password_label).text.toString())
                    assertTrue(it.findViewById<EditText>(R.id.auth_password).text.isEmpty())
                    assertFalse((field(it, "invitation") as InvitationMemory).hasInvitation)
                    assertEquals(AuthAction.Login, field(it, "action"))
                    assertEquals("", it.findViewById<EditText>(R.id.auth_login).hint.toString())
                    it.findViewById<Button>(R.id.btn_signup).performClick()
                    assertEquals("For example, marina53", it.findViewById<EditText>(R.id.auth_login).hint.toString())
                    it.getSystemService(AutofillManager::class.java)?.cancel()
                }
                assertFalse("locale change never submits signup", f.account().authenticated)
            }
        } finally { manager.applicationLocales = original; f.dnsStop() }
    }

    @Test fun notificationLocaleRefreshKeepsWorkerAndCountOnlyInGatePackage() {
        val app = gate()
        assertTrue(android.os.Build.VERSION.SDK_INT >= 33)
        val f = Core.facade(app)
        assertTrue("requires previous isolated DNS signup/reopen", f.account().authenticated)
        val account = f.account()
        val locale = app.getSystemService(LocaleManager::class.java)
        val original = locale.applicationLocales
        val economy = Prefs.economy(app)
        val nm = app.getSystemService(NotificationManager::class.java)
        val workerType = DmsgService::class.java.declaredClasses.single { it.simpleName == "Worker" }
        val count = workerType.getDeclaredField("total").also { it.isAccessible = true }
        try {
            locale.applicationLocales = LocaleList.forLanguageTags("en")
            Prefs.setEconomy(app, true)
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                await("authorized foreground ready") {
                    var ok = false
                    scenario.onActivity { ok = field(it, "state") == LaunchState.Dialogs && field(it, "busy") == false }
                    ok
                }
                DmsgService.start(app)
                await("first successful poll complete") {
                    val facts = DmsgService.connectionState()
                    DmsgService.running(app) && facts.lastSuccessAt != null && facts.lastFailure == null && !facts.pollInFlight
                }
                val pollThread = Thread.getAllStackTraces().keys.single { it.name == "dmsg-poll" }
                val revision = DmsgService.connectionState().revision
                val channel = nm.getNotificationChannel(DmsgService.CH)
                assertNotNull(channel)
                val importance = channel.importance
                // A known count fixture, not a claim of seven received messages.
                count.setInt(null, 7)
                locale.applicationLocales = LocaleList.forLanguageTags("ru")
                val ru = localized(app, "ru").resources
                await("same channel and ongoing notification translated") {
                    val notification = nm.activeNotifications.singleOrNull { it.id == DmsgService.ID }?.notification
                    nm.getNotificationChannel(DmsgService.CH).name.toString() == ru.getString(R.string.notif_channel) &&
                        notification?.extras?.getCharSequence("android.title").toString() == ru.getString(R.string.fgs_title) &&
                        notification?.extras?.getCharSequence("android.text").toString() == ru.getString(R.string.fgs_body, 7)
                }
                assertSame(pollThread, Thread.getAllStackTraces().keys.single { it.name == "dmsg-poll" })
                assertEquals(revision, DmsgService.connectionState().revision)
                assertEquals(7, count.getInt(null))
                assertEquals(importance, nm.getNotificationChannel(DmsgService.CH).importance)
                assertEquals(account, f.account())
                assertEquals("ready", f.dnsStatus())
            }
        } finally {
            DmsgService.stop(app); f.dnsStop()
            Prefs.setEconomy(app, economy)
            locale.applicationLocales = original
        }
    }
}

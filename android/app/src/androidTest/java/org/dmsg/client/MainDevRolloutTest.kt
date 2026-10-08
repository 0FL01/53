package org.dmsg.client

import android.app.Activity
import android.content.Context
import android.content.DialogInterface
import android.content.Intent
import android.database.sqlite.SQLiteDatabase
import android.graphics.Rect
import android.net.ConnectivityManager
import android.os.Process
import android.system.Os
import android.system.OsConstants
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.RadioButton
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.FileOutputStream
import java.net.Inet4Address
import java.security.MessageDigest
import java.util.concurrent.Callable
import java.util.concurrent.ExecutionException
import java.util.concurrent.FutureTask
import java.util.concurrent.TimeUnit
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.QrOutcome
import uniffi.dmsg_core.RegistrationPolicy

/** Manual, sequential main-development acceptance after an externally authorised fresh reset.
 * Select one method explicitly with allowMainDevReset=true. Nothing here resets an identity,
 * migrates an old DB, substitutes a facade/resolver, or starts a foreground service.
 * All paths below are relative to the target app's filesDir; outputs are exclusive-create 0600.
 * Auth JSON is a one-shot 0400 file with exactly serverCode/login/password/invitation strings.
 * Peer QR and exact UTF-8 synthetic text fixtures (0400/0600) remain for the reopen check.
 */
@RunWith(AndroidJUnit4::class)
class MainDevRolloutTest {
    @get:Rule val testName = TestName()

    @Test fun freshMainOnboardingAndDnsSignup() {
        val app = mainApp()
        val input = File(app.filesDir, "dev-main-auth.json")
        var auth: JSONObject? = null
        val proof = try {
            unusedOutputs(app, "dev-main-contact.qr", "dev-main-resolvers", "dev-main-onboarding-proof.json")
            val fixture = json(readPrivate(input, 16_384, readOnly = true))
            auth = fixture
            assertTrue("auth fixture contains only the four required fields",
                fixture.keys().asSequence().toSet() == setOf("serverCode", "login", "password", "invitation"))
            for (key in listOf("serverCode", "login", "password", "invitation")) string(fixture, key)
            core(app) {
                assertFalse("signup requires the externally reset, unauthenticated main app", account().authenticated)
                assertTrue("fresh main has no contacts", contacts(null, 100).first.isEmpty())
                assertTrue("fresh main has no inbox", inbox(0, 100).first.isEmpty())
                assertTrue("fresh main has no outbox", outbox(0, 100).first.isEmpty())
            }
            val preview = core(app) { QrGate.serverPreview(this, string(fixture, "serverCode")) }
            val packagedProfile = TrustedServerProfile.ASSET in app.assets.list("").orEmpty()
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                if (!packagedProfile) {
                    awaitMain(scenario, LaunchState.Connection)
                    scenario.onActivity {
                        it.findViewById<EditText>(R.id.connection_code).setText(string(fixture, "serverCode"))
                        assertTrue("actual preview button clicked", it.findViewById<Button>(R.id.btn_preview).performClick())
                    }
                    await(scenario, "offline server confirmation ready") { prompt(it)?.isShowing == true && field(it, "busy") == false }
                    scenario.onActivity {
                        val message = prompt(it)!!.findViewById<TextView>(android.R.id.message)!!.text.toString()
                        assertTrue("offline confirmation shows actual domain and full pin fingerprint",
                            message.contains(preview.domain) && message.contains(preview.fingerprint))
                    }
                    core(app) {
                        assertNull("preview stays offline and unimported", dnsProfile())
                        assertFalse(account().authenticated)
                        assertEquals("stopped", dnsStatus())
                    }
                    scenario.onActivity { prompt(it)!!.getButton(DialogInterface.BUTTON_POSITIVE).performClick() }
                }
                await(scenario, "actual DNS invitation policy ready") {
                    field(it, "state") == LaunchState.Authentication && field(it, "busy") == false &&
                        field(it, "policy") == RegistrationPolicy.INVITE_ONLY
                }
                core(app) {
                    val profile = dnsProfile() ?: throw AssertionError("actual saved server profile required")
                    assertEquals(preview.domain, profile.domain)
                    assertEquals(preview.fingerprint, profile.fingerprint)
                    assertEquals(activeResolvers(app), profile.resolvers)
                }
                core(app) { assertFalse(account().authenticated); assertEquals("stopped", dnsStatus()) }
                scenario.onActivity {
                    assertEquals(View.GONE, it.findViewById<View>(R.id.invitation_group).visibility)
                    it.findViewById<Button>(R.id.btn_signup).performClick()
                    assertEquals(View.VISIBLE, it.findViewById<View>(R.id.invitation_group).visibility)
                    it.importInvitationFile { java.io.ByteArrayInputStream(string(fixture, "invitation").toByteArray(Charsets.US_ASCII)) }
                }
                await(scenario, "invitation imported through bounded file parser") {
                    field(it, "busy") == false && (field(it, "invitation") as InvitationMemory).hasInvitation
                }
                scenario.onActivity {
                    it.findViewById<EditText>(R.id.auth_login).setText(string(fixture, "login"))
                    it.findViewById<EditText>(R.id.auth_password).setText(string(fixture, "password"))
                    assertTrue("actual signup button enabled", it.findViewById<Button>(R.id.btn_auth_submit).isEnabled)
                    it.findViewById<Button>(R.id.btn_auth_submit).performClick()
                    assertSecretsCleared(it)
                }
                awaitMain(scenario, LaunchState.Dialogs)
                scenario.onActivity { assertDialogs(it); assertSecretsCleared(it) }
                val account = core(app) { authenticatedAccount(app) }
                scenario.recreate()
                awaitMain(scenario, LaunchState.Dialogs)
                scenario.moveToState(Lifecycle.State.STARTED)
                scenario.moveToState(Lifecycle.State.RESUMED)
                awaitMain(scenario, LaunchState.Dialogs)
                scenario.onActivity { assertDialogs(it); assertSecretsCleared(it) }
                core(app) { assertEquals("recreate/resume preserves the authenticated account", account, account()) }
            }
            core(app) {
                val account = authenticatedAccount(app)
                val resolvers = activeResolvers(app)
                assertTrue("real DNS reconnect succeeds", reconnect() > 0L)
                assertEquals("ready", dnsStatus())
                val profile = dnsProfile()!!
                assertTrue("saved DNS profile is the actual confirmed server",
                    profile.domain == preview.domain && profile.fingerprint == preview.fingerprint)
                assertTrue("saved DNS profile uses the actual active network", profile.resolvers == resolvers)
                val first = fetch(); val second = fetch()
                assertEmptyFetch(first); assertEmptyFetch(second)
                assertTrue("empty fetch cursor does not regress", second.cursor >= first.cursor)
                assertTrue("fresh authenticated inbox remains empty", inbox(0, 100).first.isEmpty())
                assertEquals(account, account())
                writePrivate(app, "dev-main-contact.qr", myQr())
                writePrivate(app, "dev-main-resolvers", resolvers.joinToString("\n") + "\n")
                evidence(account).put("coreSchema", 8).put("policyInviteOnly", true)
                    .put("secretsCleared", true).put("recreatedAndResumed", true)
                    .put("dnsReady", true).put("activeResolversMatch", true).put("resolverCount", resolvers.size)
                    .put("firstReceived", first.received.size).put("secondReceived", second.received.size)
                    .put("skipped", 0).put("inboxCount", 0)
            }
        } finally {
            auth?.let { value -> listOf("password", "invitation", "login", "serverCode").forEach { value.remove(it) } }
            consumeAuth(input)
        }
        assertNoService(app)
        writePrivate(app, "dev-main-onboarding-proof.json", proof.put("authFixtureConsumed", true).toString())
    }

    /** Coordinator exports the phone QR, provisions the native peer, then queues one incoming text. */
    @Test fun acceptNativePeerAndReceive() {
        val app = mainApp()
        unusedOutputs(app, "dev-native-id", "dev-main-receive-proof.json")
        val qr = readPrivate(File(app.filesDir, "dev-native-contact.qr"), QrGate.URI_MAX).trim()
        val expected = syntheticText(app, "dev-incoming.txt")
        val proof = core(app) {
            val account = establishedAccount(app)
            assertTrue("peer import requires no previous contacts", contacts(null, 100).first.isEmpty())
            assertTrue("peer receive requires an empty inbox", inbox(0, 100).first.isEmpty())
            assertEquals("actual QR imports a new peer", QrOutcome.ADDED, addQr(qr))
            val contacts = contacts(null, 100)
            assertNull("one complete contact page", contacts.second)
            assertEquals(1, contacts.first.size)
            val peer = contacts.first.single().contactId
            assertTrue("peer is distinct from own public ID", peer != account.contactId)
            accept(peer)
            setContactAlias(peer, PEER_ALIAS)
            trustedPeer(peer)
            assertTrue("accepted QR stores the local alias", summary(peer)?.localAlias == PEER_ALIAS)
            assertTrue("actual recursive DNS reconnect", reconnect() > 0L)
            assertEquals("ready", dnsStatus())
            val first = fetch(); val second = fetch()
            assertEquals("one actual incoming plaintext", 1, first.received.size)
            assertTrue("incoming plaintext and sender match the private synthetic fixture",
                first.received.single().text == expected && first.received.single().contactId == peer)
            zeroSkipped(first); assertEmptyFetch(second)
            assertTrue("dedup cursor is persistent", second.cursor >= first.cursor)
            val rows = exactHistory(peer, expected, null)
            val inbox = inbox(0, 100)
            assertNull(inbox.second); assertEquals(1, inbox.first.size)
            assertTrue("durable inbox matches received plaintext", inbox.first.single().text == expected)
            assertEquals(account, account())
            writePrivate(app, "dev-native-id", peer)
            evidence(account).put("peerContactId", peer).put("aliasSaved", true)
                .put("firstReceived", first.received.size).put("secondReceived", second.received.size)
                .put("skipped", 0).put("plaintextEqual", true).put("historyCount", rows.size)
                .put("incomingCount", 1).put("outgoingCount", 0).put("cursor", second.cursor)
        }
        assertNoService(app)
        writePrivate(app, "dev-main-receive-proof.json", proof.toString())
    }

    /** Native peer must not fetch/ACK the reply until the accepted proof has been exported. */
    @Test fun sendAndVerifyMainHistory() {
        val app = mainApp()
        unusedOutputs(app, "dev-main-message-id", "dev-main-send-proof.json")
        val peer = peerId(app)
        val incoming = syntheticText(app, "dev-incoming.txt")
        val outgoing = syntheticText(app, "dev-outgoing.txt")
        val account = core(app) { establishedAccount(app).also { trustedPeer(peer) } }
        val before = core(app) { exactHistory(peer, incoming, null) }
        var sent: HistoryMessage? = null
        ActivityScenario.launch<ChatActivity>(chatIntent(app, peer)).use { scenario ->
            await(scenario, "real accepted-peer chat ready") { it.findViewById<Button>(R.id.btn_send).isEnabled }
            scenario.onActivity {
                it.findViewById<EditText>(R.id.composer).setText(outgoing)
                it.findViewById<Button>(R.id.btn_send).performClick()
                assertTrue("real UI send is pending", chatMemory(it).pending)
                assertFalse("pending send disables double submit", it.findViewById<Button>(R.id.btn_send).isEnabled)
                assertFalse("pending send disables composer", it.findViewById<EditText>(R.id.composer).isEnabled)
                it.findViewById<Button>(R.id.btn_send).performClick()
            }
            await(scenario, "durably saved send clears draft and completes") {
                !chatMemory(it).pending && it.findViewById<EditText>(R.id.composer).text.isEmpty() &&
                    it.findViewById<TextView>(R.id.info).text.contains(it.getString(R.string.message_saved, ""))
            }
            sent = core(app) {
                val rows = exactHistory(peer, incoming, outgoing)
                assertEquals("double submit produces exactly one new history row", before.size + 1, rows.size)
                assertTrue("incoming history is preserved", before.all { old -> rows.any { it == old } })
                val newRows = rows.filter { row -> before.none { it.messageIdHex == row.messageIdHex } }
                assertEquals("exactly one new message ID", 1, newRows.size)
                newRows.single().also {
                    assertEquals(MessageDirection.OUTGOING, it.direction)
                    val status = messageStatus(it.messageIdHex)
                    assertTrue("initial exact outgoing status is queued or accepted, never unknown",
                        status == DeliveryState.QUEUED || status == DeliveryState.ACCEPTED)
                    assertEquals("persisted history and exact status agree", status, it.deliveryState)
                }
            }
            scenario.onActivity {
                assertTrue("actual retry button enabled", it.findViewById<Button>(R.id.btn_retry).isEnabled)
                it.findViewById<Button>(R.id.btn_retry).performClick()
                assertTrue("real queue retry is pending", chatMemory(it).pending)
            }
            await(scenario, "real DNS queue retry completes successfully") {
                !chatMemory(it).pending && it.findViewById<TextView>(R.id.info).text.contains(it.getString(R.string.retry_complete))
            }
            val accepted = core(app) {
                assertEquals("real server acceptance before host ACK", DeliveryState.ACCEPTED, messageStatus(sent!!.messageIdHex))
                exactHistory(peer, incoming, outgoing).single { it.direction == MessageDirection.OUTGOING }.also {
                    assertEquals(DeliveryState.ACCEPTED, it.deliveryState)
                    assertTrue("retry reuses the same public message ID", it.messageIdHex == sent!!.messageIdHex)
                }
            }
            awaitRow(scenario, accepted, R.string.delivery_accepted)
        }
        val proof = core(app) {
            assertEquals("UI send/retry preserves main account", account, establishedAccount(app))
            val rows = exactHistory(peer, incoming, outgoing)
            assertEquals(DeliveryState.ACCEPTED, messageStatus(sent!!.messageIdHex))
            writePrivate(app, "dev-main-message-id", sent!!.messageIdHex)
            evidence(account).put("peerContactId", peer).put("messageId", sent!!.messageIdHex)
                .put("pendingGuardVerified", true).put("newOutgoingCount", 1).put("historyCount", rows.size)
                .put("incomingCount", 1).put("outgoingCount", 1).put("plaintextEqual", true)
                .put("status", "accepted").put("acceptedRowVisible", true)
        }
        assertNoService(app)
        writePrivate(app, "dev-main-send-proof.json", proof.toString())
    }

    /** Run in a new instrumentation invocation after the native peer's actual receive + ACK. */
    @Test fun reopenedMainHasDeliveredHistory() {
        val app = mainApp()
        unusedOutputs(app, "dev-main-delivered-proof.json")
        val peer = peerId(app)
        val mid = readPrivate(File(app.filesDir, "dev-main-message-id"), 64).trim()
        assertTrue("public message ID is canonical hex", Regex("[0-9a-f]{32}").matches(mid))
        val incoming = syntheticText(app, "dev-incoming.txt")
        val outgoing = syntheticText(app, "dev-outgoing.txt")
        val account = core(app) {
            val account = establishedAccount(app)
            trustedPeer(peer)
            exactHistory(peer, incoming, outgoing)
            assertTrue("reopened status is accepted or already delivered",
                messageStatus(mid) in listOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED))
            assertTrue("real DNS reconnect on reopened main", reconnect() > 0L)
            val retried = retry()
            assertEquals(4, retried.size); assertEquals("retry skips nothing", 0L, retried[3])
            assertEquals("delivery is an exact persistent server fact", DeliveryState.DELIVERED, messageStatus(mid))
            assertTrue("delivered ID has left the outbox", outbox(0, 100).first.none { it.mid == mid })
            assertEmptyFetch(fetch())
            assertEquals(account, account())
            account
        }
        // Core.facade opens the durable DB again; do not infer delivery from queue emptiness.
        val rows = core(app) {
            assertEquals(account, establishedAccount(app))
            assertEquals(DeliveryState.DELIVERED, messageStatus(mid))
            exactHistory(peer, incoming, outgoing).also { rows ->
                val sent = rows.single { it.direction == MessageDirection.OUTGOING }
                assertTrue("original outgoing ID is retained", sent.messageIdHex == mid)
                assertEquals(DeliveryState.DELIVERED, sent.deliveryState)
            }
        }
        ActivityScenario.launch<ChatActivity>(chatIntent(app, peer)).use { scenario ->
            for (row in rows) awaitRow(scenario, row, if (row.direction == MessageDirection.INCOMING) R.string.message_incoming else R.string.delivery_delivered)
            scenario.recreate()
            for (row in rows) awaitRow(scenario, row, if (row.direction == MessageDirection.INCOMING) R.string.message_incoming else R.string.delivery_delivered)
        }
        val dialogPreview = core(app) { summary(peer)?.preview ?: throw AssertionError("real persisted dialog preview required") }
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            awaitMain(scenario, LaunchState.Dialogs)
            await(scenario, "real main dialog alias and outgoing preview visible") {
                val content = visibleTexts(it.findViewById(R.id.dialogs))
                PEER_ALIAS in content && dialogPreview in content
            }
            scenario.onActivity { assertDialogs(it) }
            scenario.recreate()
            awaitMain(scenario, LaunchState.Dialogs)
            scenario.onActivity { assertDialogs(it) }
        }
        val proof = core(app) {
            assertEquals(account, establishedAccount(app))
            assertEquals(DeliveryState.DELIVERED, messageStatus(mid))
            val durable = exactHistory(peer, incoming, outgoing)
            assertEquals(DeliveryState.DELIVERED, durable.single { it.direction == MessageDirection.OUTGOING }.deliveryState)
            evidence(account).put("peerContactId", peer).put("messageId", mid).put("coreSchema", 8)
                .put("historyCount", durable.size).put("incomingCount", 1).put("outgoingCount", 1)
                .put("plaintextEqual", true).put("status", "delivered").put("skipped", 0)
                .put("historyRowsVisible", true).put("recreated", true).put("mainDialogs", true).put("noStoreError", true)
        }
        assertNoService(app)
        writePrivate(app, "dev-main-delivered-proof.json", proof.toString())
    }

    /** Run only after the fresh native peer ACK and delivered main-history proof. */
    @Test fun editOwnDeliveredMainMessageThroughUi() {
        val app = mainApp()
        unusedOutputs(app, "dev-main-edit-proof.json")
        val peer = peerId(app)
        val mid = readPrivate(File(app.filesDir, "dev-main-message-id"), 64).trim()
        val replacement = syntheticText(app, "dev-edited.txt")
        val before = core(app) {
            establishedAccount(app); trustedPeer(peer)
            historyPage(peer, null, 100).rows.single { it.messageIdHex == mid }.also {
                assertEquals(DeliveryState.DELIVERED, it.deliveryState)
                assertEquals(0uL, it.revision)
            }
        }
        val cipher = mainCipherHash(app, mid)
        ActivityScenario.launch<ChatActivity>(chatIntent(app, peer)).use { scenario ->
            awaitRow(scenario, before, R.string.delivery_delivered)
            scenario.onActivity { it.findViewById<EditText>(R.id.composer).setText("Main normal draft") }
            wholeMessageAction(scenario, before.localId, R.id.action_edit)
            await(scenario, "whole main message enters edit composer") {
                it.findViewById<View>(R.id.edit_banner).isShown &&
                    it.findViewById<EditText>(R.id.composer).text.toString() == before.text
            }
            scenario.onActivity {
                it.findViewById<EditText>(R.id.composer).setText(replacement)
                assertTrue(it.findViewById<Button>(R.id.btn_save_edit).performClick())
            }
            await(scenario, "main edit saved and normal draft restored") {
                !chatMemory(it).pending && chatMemory(it).history.rows.any { row ->
                    row.localId == before.localId && row.revision == 1uL && row.text == replacement
                } && !it.findViewById<View>(R.id.edit_banner).isShown &&
                    it.findViewById<EditText>(R.id.composer).text.toString() == "Main normal draft"
            }
        }
        val after = core(app) { historyMessage(peer, before.localId) }
        unchangedOriginal(before, after)
        assertEquals(cipher, mainCipherHash(app, mid))
        assertEquals(1uL, after.revision); assertEquals(replacement, after.text)
        assertNotNull(after.changeDeliveryState)
        writePrivate(app, "dev-main-edit-proof.json", evidence(core(app) { establishedAccount(app) })
            .put("coreSchema", 8).put("localId", after.localId).put("revision", 1)
            .put("originalCipherAndChronologyUnchanged", true).put("actualUiSaved", true).toString())
    }

    /** Coordinator verifies the edited peer projection before selecting this method. */
    @Test fun deleteOwnMainMessageForEveryoneThroughUi() {
        val app = mainApp()
        unusedOutputs(app, "dev-main-delete-proof.json")
        val peer = peerId(app)
        val edit = json(readPrivate(File(app.filesDir, "dev-main-edit-proof.json"), 4096))
        val before = core(app) { historyMessage(peer, edit.getLong("localId")) }
        assertEquals(1uL, before.revision)
        assertFalse(before.hiddenSelf); assertFalse(before.deletedAll)
        val cipher = mainCipherHash(app, before.messageIdHex)
        ActivityScenario.launch<ChatActivity>(chatIntent(app, peer)).use { scenario ->
            awaitRow(scenario, before, R.string.delivery_delivered)
            wholeMessageAction(scenario, before.localId, R.id.action_delete)
            await(scenario, "actual main confirmation displayed") { chatPrompt(it)?.isShowing == true }
            scenario.onActivity {
                val prompt = chatPrompt(it)!!
                val choices = descendants(prompt.window!!.decorView).filterIsInstance<RadioButton>()
                assertTrue("self-only is the initial main selection", choices.single { v ->
                    v.text.toString() == it.getString(R.string.delete_self)
                }.isChecked)
                choices.single { v -> v.text.toString() == it.getString(R.string.delete_everyone) }.performClick()
                prompt.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
            }
            await(scenario, "main terminal tombstone is saved and has no bubble") {
                val list = it.findViewById<ListView>(R.id.messages)
                !chatMemory(it).pending && chatMemory(it).history.rows.any { row ->
                    row.localId == before.localId && row.deletedAll && row.text.isEmpty() && row.revision == 2uL
                } && (0 until list.adapter.count).none { pos -> list.adapter.getItemId(pos) == before.localId }
            }
        }
        val after = core(app) { historyMessage(peer, before.localId) }
        unchangedOriginal(before, after)
        assertEquals(cipher, mainCipherHash(app, before.messageIdHex))
        assertTrue(after.deletedAll); assertTrue(after.text.isEmpty()); assertEquals(2uL, after.revision)
        writePrivate(app, "dev-main-delete-proof.json", evidence(core(app) { establishedAccount(app) })
            .put("coreSchema", 8).put("localId", after.localId).put("revision", 2)
            .put("originalCipherAndChronologyUnchanged", true).put("actualUiDeleted", true).toString())
    }

    /** New process after the peer has durably received and ACKed the DELETE control. */
    @Test fun reopenedMainRetainsDeliveredDeletion() {
        val app = mainApp()
        unusedOutputs(app, "dev-main-actions-delivered-proof.json")
        val peer = peerId(app)
        val proof = json(readPrivate(File(app.filesDir, "dev-main-delete-proof.json"), 4096))
        val row = core(app) {
            establishedAccount(app); trustedPeer(peer)
            assertTrue(reconnect() > 0L)
            assertEquals(0L, retry()[3]); assertEmptyFetch(fetch())
            historyMessage(peer, proof.getLong("localId")).also {
                assertTrue(it.deletedAll); assertFalse(it.hiddenSelf); assertTrue(it.text.isEmpty())
                assertEquals(2uL, it.revision)
                assertEquals(DeliveryState.DELIVERED, it.deliveryState)
                assertEquals(DeliveryState.DELIVERED, it.changeDeliveryState)
                assertTrue(outbox(0, 100).first.isEmpty())
                assertEquals(2, historyPage(peer, null, 100).rows.size)
                assertEquals(1, inbox(0, 100).first.size)
            }
        }
        ActivityScenario.launch<ChatActivity>(chatIntent(app, peer)).use { scenario ->
            await(scenario, "reopened main filters deleted bubble") {
                val list = it.findViewById<ListView>(R.id.messages)
                chatMemory(it).history.initialized && list.adapter.count == 1 &&
                    (0 until list.adapter.count).none { pos -> list.adapter.getItemId(pos) == row.localId }
            }
        }
        writePrivate(app, "dev-main-actions-delivered-proof.json", evidence(core(app) { establishedAccount(app) })
            .put("coreSchema", 8).put("revision", 2).put("deletedAll", true)
            .put("baseDelivered", true).put("deleteDelivered", true).put("skipped", 0)
            .put("newProcess", true).put("noPlaceholder", true).toString())
    }

    private fun unchangedOriginal(before: HistoryMessage, after: HistoryMessage) {
        assertEquals(before.localId, after.localId); assertEquals(before.messageIdHex, after.messageIdHex)
        assertEquals(before.localTimestampMs, after.localTimestampMs)
        assertEquals(before.serverSeq, after.serverSeq); assertEquals(before.serverTimestampMs, after.serverTimestampMs)
        assertEquals(before.deliveryState, after.deliveryState)
    }

    private fun mainCipherHash(app: Context, mid: String): String = SQLiteDatabase.openDatabase(
        Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { db ->
        assertTrue(Regex("[0-9a-f]{32}").matches(mid))
        db.rawQuery("SELECT ciphertext FROM core_messages WHERE direction='outgoing' AND kind='text' AND lower(hex(message_id))=?",
            arrayOf(mid)).use { cursor ->
            assertTrue(cursor.moveToFirst())
            MessageDigest.getInstance("SHA-256").digest(cursor.getBlob(0)).joinToString("") { "%02x".format(it) }
        }
    }

    private fun descendants(view: View): List<View> = listOf(view) + if (view is ViewGroup)
        (0 until view.childCount).flatMap { descendants(view.getChildAt(it)) } else emptyList()
    private fun chatPrompt(activity: ChatActivity) = ChatActivity::class.java.getDeclaredField("prompt")
        .also { it.isAccessible = true }.get(activity) as? AlertDialog
    private fun wholeMessageAction(scenario: ActivityScenario<ChatActivity>, localId: Long, action: Int) {
        await(scenario, "actual stable-ID main accessibility action") { activity ->
            val memory = chatMemory(activity)
            val guard = ChatActivity::class.java.getDeclaredField("pageGuard").also { it.isAccessible = true }.get(activity) as UiGuard
            if (memory.pending || guard.pending || memory.uncertain != null || memory.uncertainAction != null) return@await false
            val list = activity.findViewById<ListView>(R.id.messages)
            val child = (0 until list.childCount).map { list.getChildAt(it) }.singleOrNull { it.tag == localId } ?: return@await false
            child.performAccessibilityAction(action, null)
        }
    }

    private fun mainApp(): Context {
        val app = ApplicationProvider.getApplicationContext<Context>()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val args = InstrumentationRegistry.getArguments()
        val selected = args.getString("class") == "${javaClass.name}#${testName.methodName}"
        // A main-only rollout is not part of the ordinary disposable .gate suite.
        // Explicit selection on the wrong target still fails before any state changes.
        if (app.packageName == "org.dmsg.client.gate" && !selected) {
            assumeTrue("manual main-development rollout is not applicable to the .gate suite", false)
        }
        assertEquals("main rollout requires the exact main target", "org.dmsg.client", instrumentation.targetContext.packageName)
        assertEquals("main rollout cannot run in .gate", "org.dmsg.client", app.packageName)
        assertTrue("explicit main-development consent required", args.getString("allowMainDevReset") == "true")
        assertTrue("select exactly this one method, never a routine suite",
            selected)
        assertNoService(app)
        return app
    }

    private fun assertNoService(app: Context) {
        assertFalse("main acceptance requires foreground service off", DmsgService.running(app))
        assertFalse("foreground service must not be enabled", DmsgService.connectionState().serviceEnabled)
    }

    private fun <T> core(app: Context, work: DmsgFacade.() -> T): T {
        val task = FutureTask(Callable {
            assertNoService(app)
            val f = Core.facade(app)
            assertTrue("actual native facade required", f is UniFfiFacade && f.isReady())
            f.work()
        })
        Core.dispatch { task.run() }
        return try { task.get(180, TimeUnit.SECONDS) }
        catch (e: ExecutionException) { throw (e.cause ?: e) }
    }

    private fun DmsgFacade.authenticatedAccount(app: Context): AccountInfo {
        val account = account()
        assertTrue("main account authenticated", account.authenticated && account.contactId != null)
        SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { db ->
            db.rawQuery("PRAGMA user_version", null).use { cursor ->
                assertTrue(cursor.moveToFirst()); assertEquals("actual core schema", 10, cursor.getInt(0))
            }
        }
        return account
    }

    private fun DmsgFacade.establishedAccount(app: Context): AccountInfo {
        val saved = json(readPrivate(File(app.filesDir, "dev-main-onboarding-proof.json"), 4096))
        assertTrue("successful onboarding proof required", saved.optBoolean("passed") && saved.optBoolean("authFixtureConsumed"))
        val account = authenticatedAccount(app)
        assertTrue("original main public identity is retained", account.contactId == string(saved, "contactId"))
        assertTrue("original main public contact QR is retained", myQr() == readPrivate(File(app.filesDir, "dev-main-contact.qr"), QrGate.URI_MAX))
        return account
    }

    private fun DmsgFacade.trustedPeer(peer: String) {
        val contact = get(peer)
        assertTrue("QR peer is accepted with pinned keys and no mismatch",
            contact != null && contact.state == "accepted" && contact.hasKeys && !contact.identityMismatch)
    }

    private fun DmsgFacade.exactHistory(peer: String, incoming: String, outgoing: String?): List<HistoryMessage> {
        val page = historyPage(peer, null, 100)
        assertNull("acceptance history fits one complete page", page.nextBeforeLocalId)
        assertEquals("exact durable history size", if (outgoing == null) 1 else 2, page.rows.size)
        assertEquals("unique local history IDs", page.rows.size, page.rows.map { it.localId }.toSet().size)
        assertEquals("unique message IDs", page.rows.size, page.rows.map { it.messageIdHex }.toSet().size)
        assertTrue("all history belongs to this peer", page.rows.all { it.contactId == peer && it.localId > 0 })
        val received = page.rows.filter { it.direction == MessageDirection.INCOMING }
        assertEquals("one incoming history row", 1, received.size)
        assertTrue("exact incoming plaintext", received.single().text == incoming)
        assertNull("incoming is not an outgoing delivery state", received.single().deliveryState)
        val sent = page.rows.filter { it.direction == MessageDirection.OUTGOING }
        assertEquals("exact outgoing history count", if (outgoing == null) 0 else 1, sent.size)
        if (outgoing != null) assertTrue("exact outgoing plaintext", sent.single().text == outgoing)
        return page.rows
    }

    private fun zeroSkipped(result: FetchRes) {
        assertEquals("all four native skip counters are present", 4, result.skipped.size)
        assertTrue("no unknown/blocked/undecryptable/mismatched messages skipped", result.skipped.all { it == 0L })
    }

    private fun assertEmptyFetch(result: FetchRes) { assertEquals(0, result.received.size); zeroSkipped(result) }

    private fun activeResolvers(app: Context): List<String> {
        val cm = app.getSystemService(ConnectivityManager::class.java)
        val network = cm.activeNetwork ?: throw AssertionError("actual active network required")
        val lp = cm.getLinkProperties(network) ?: throw AssertionError("actual LinkProperties required")
        val v4 = lp.dnsServers.filterIsInstance<Inet4Address>()
        val selected = if (v4.isNotEmpty()) v4 else lp.dnsServers
        val resolvers = selected.take(8).map { if (it is Inet4Address) "${it.hostAddress}:53" else "[${it.hostAddress}]:53" }
        assertTrue("active network supplies DNS resolvers", resolvers.isNotEmpty())
        assertTrue("production resolver selection matches current LinkProperties", resolvers == DnsNetwork.resolvers(app))
        return resolvers
    }

    private fun field(activity: MainActivity, name: String): Any? = MainActivity::class.java.getDeclaredField(name)
        .also { it.isAccessible = true }.get(activity)
    private fun prompt(activity: MainActivity) = field(activity, "prompt") as? AlertDialog
    private fun chatMemory(activity: ChatActivity) = ChatActivity::class.java.getDeclaredField("memory")
        .also { it.isAccessible = true }.get(activity) as ChatMemory

    private fun <A : Activity> await(scenario: ActivityScenario<A>, label: String, predicate: (A) -> Boolean) {
        val deadline = System.nanoTime() + 120_000_000_000L
        while (System.nanoTime() < deadline) {
            var ready = false
            scenario.onActivity { ready = predicate(it) }
            if (ready) return
            Thread.sleep(50)
        }
        fail(label)
    }

    private fun awaitMain(scenario: ActivityScenario<MainActivity>, state: LaunchState) = await(scenario, "real main launch state ready") {
        field(it, "state") == state && field(it, "busy") == false && it.hasWindowFocus()
    }

    private fun assertSecretsCleared(activity: MainActivity) {
        assertTrue("password field cleared", activity.findViewById<EditText>(R.id.auth_password).text.isEmpty())
        assertFalse("invitation memory cleared", (field(activity, "invitation") as InvitationMemory).hasInvitation)
        assertTrue("connection code field cleared", activity.findViewById<EditText>(R.id.connection_code).text.isEmpty())
    }

    private fun assertDialogs(activity: MainActivity) {
        assertTrue("MainActivity uses the actual facade", field(activity, "facade") is UniFfiFacade)
        assertEquals(LaunchState.Dialogs, field(activity, "state"))
        assertEquals(View.VISIBLE, activity.findViewById<View>(R.id.dialogs_panel).visibility)
        assertEquals(View.GONE, activity.findViewById<View>(R.id.auth_panel).visibility)
        assertEquals(View.GONE, activity.findViewById<View>(R.id.connection_panel).visibility)
        assertTrue("no Store error in actual visible main UI", visibleTexts(activity.window.decorView).none {
            it.contains("Store", ignoreCase = true) ||
                it.contains(humanError(activity.resources, DmsgError("synthetic gate error", ErrorKind.Store))) ||
                it.contains(humanError(activity.resources, ffiError(uniffi.dmsg_core.FfiException.Store("synthetic gate error"))))
        })
    }

    private fun visibleTexts(view: View): List<String> = when {
        !view.isShown -> emptyList()
        view is ViewGroup -> (0 until view.childCount).flatMap { visibleTexts(view.getChildAt(it)) }
        view is TextView && view.getGlobalVisibleRect(Rect()) -> listOf(view.text.toString())
        else -> emptyList()
    }

    private fun awaitRow(scenario: ActivityScenario<ChatActivity>, row: HistoryMessage, labelRes: Int) {
        await(scenario, "real history adapter contains expected local row") {
            val list = it.findViewById<ListView>(R.id.messages)
            (0 until (list.adapter?.count ?: 0)).any { pos -> list.adapter.getItemId(pos) == row.localId }
        }
        scenario.onActivity {
            val list = it.findViewById<ListView>(R.id.messages)
            list.setSelection((0 until list.adapter.count).first { pos -> list.adapter.getItemId(pos) == row.localId })
        }
        await(scenario, "exact plaintext and delivery/direction label visibly rendered in the same real row") {
            val list = it.findViewById<ListView>(R.id.messages)
            (0 until list.childCount).any { pos ->
                val child = list.getChildAt(pos)
                val content = visibleTexts(child)
                child.tag == row.localId && row.text in content && content.any { text -> text.contains(it.getString(labelRes)) }
            }
        }
    }

    private fun chatIntent(app: Context, peer: String) = Intent(app, ChatActivity::class.java).putExtra("peer", peer).putExtra("alias", PEER_ALIAS)
    private fun peerId(app: Context) = readPrivate(File(app.filesDir, "dev-native-id"), 64).trim().also {
        assertTrue("public contact ID required", Regex("[0-9A-Za-z]{12}").matches(it))
    }
    private fun syntheticText(app: Context, name: String) = readPrivate(File(app.filesDir, name), 4096)
    private fun evidence(account: AccountInfo) = JSONObject().put("passed", true).put("contactId", account.contactId)
        .put("authenticated", account.authenticated).put("foregroundService", false)

    private fun json(text: String): JSONObject = try { JSONObject(text) }
        catch (_: Exception) { throw AssertionError("valid private JSON fixture required") }
    private fun string(value: JSONObject, key: String): String = (value.opt(key) as? String)
        ?.takeIf { it.isNotEmpty() } ?: throw AssertionError("required private string field missing or empty")

    private fun privateFile(file: File, readOnly: Boolean = false) {
        val stat = try { Os.lstat(file.absolutePath) } catch (_: Exception) { throw AssertionError("app-private fixture required") }
        assertTrue("fixture must be an owned regular file, never a symlink", OsConstants.S_ISREG(stat.st_mode) && stat.st_uid == Process.myUid())
        val mode = stat.st_mode and 0xFFF
        assertTrue("fixture must be owner-only; auth fixture must be read-only", mode == 0x100 || (!readOnly && mode == 0x180))
    }

    private fun readPrivate(file: File, maxBytes: Int, readOnly: Boolean = false): String {
        privateFile(file, readOnly)
        assertTrue("bounded nonempty private fixture", file.length() in 1L..maxBytes.toLong())
        val bytes = file.readBytes()
        return try {
            assertTrue("bounded private fixture contents", bytes.size in 1..maxBytes)
            val text = bytes.toString(Charsets.UTF_8)
            assertTrue("fixture contains exact valid UTF-8", text.toByteArray(Charsets.UTF_8).contentEquals(bytes))
            text
        } finally { bytes.fill(0) }
    }

    private fun consumeAuth(input: File) {
        if (!input.exists()) return
        privateFile(input, readOnly = true)
        Os.chmod(input.absolutePath, 0x180)
        SecureStore.wipe(input)
        assertFalse("one-shot auth fixture consumed", input.exists())
    }

    private fun unusedOutputs(app: Context, vararg names: String) = names.forEach {
        assertFalse("refusing to overwrite rollout evidence; finish the existing stage first", File(app.filesDir, it).exists())
    }

    private fun writePrivate(app: Context, name: String, value: String) {
        val file = File(app.filesDir, name)
        val fd = Os.open(file.absolutePath, OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_EXCL or OsConstants.O_NOFOLLOW, 0x180)
        FileOutputStream(fd).use { out -> out.write(value.toByteArray(Charsets.UTF_8)); out.fd.sync() }
        privateFile(file)
        assertEquals("private output mode is exactly 0600", 0x180, Os.stat(file.absolutePath).st_mode and 0xFFF)
    }

    companion object { private const val PEER_ALIAS = "Проверка DNS" }
}

package org.dmsg.client

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.net.Uri
import android.view.View
import android.view.WindowManager
import android.content.ClipboardManager
import android.widget.AbsListView
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.RegistrationPolicy
import uniffi.dmsg_core.DialogSummary

class DialogMemory : ViewModel() {
    internal val rows = mutableListOf<DialogSummary>()
    internal var next: String? = null
    internal var anchor: String? = null
    internal var offset = 0
}

/** Launch router: public connection import -> account forms -> paginated dialogs. */
class MainActivity : DmsgActivity() {
    private lateinit var status: TextView
    private lateinit var list: ListView
    private lateinit var code: EditText
    private lateinit var login: EditText
    private lateinit var password: EditText
    private val invitation = InvitationMemory()
    private var scanTicket: Long? = null
    private var departingForScan = false
    private var pendingInvitationFile: Uri? = null
    private val invitationFilePicker = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        // Only a picker-issued URI, never an invitation/password in Intent extras.
        pendingInvitationFile = if (result.resultCode == RESULT_OK) result.data?.data else null
        if (pendingInvitationFile == null) { invitation.clear(); updateForm() }
    }
    private val invitationScanner = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val ticket = scanTicket
        scanTicket = null
        val value = ticket?.let { InvitationScanTransfer.take(it) }
        if (result.resultCode == RESULT_OK && value != null) acceptInvitation(value)
        else { value?.fill('\u0000'); invitation.clear(); updateForm() }
    }
    private lateinit var memory: DialogMemory
    private val rows get() = memory.rows
    private var nextCursor: String?
        get() = memory.next
        set(value) { memory.next = value }
    private var restoring = false
    private var token = 0L
    private var state: LaunchState? = null
    private var action = AuthAction.Login
    private var policy: RegistrationPolicy? = null
    private var flow: AuthFlow? = null
    private var facade: DmsgFacade? = null
    private var prompt: AlertDialog? = null
    private var busy = false
    private var activeSecrets: AuthSecrets? = null
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            if (state == LaunchState.Dialogs && !busy && list.firstVisiblePosition == 0) loadDialogs()
            if (state == LaunchState.Dialogs) findViewById<Button>(R.id.connection_strip).text = connectionLabel(resources, DmsgService.connectionState())
            handler.postDelayed(this, 5_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        memory = ViewModelProvider(this)[DialogMemory::class.java]
        status = findViewById(R.id.status)
        list = findViewById(R.id.dialogs)
        code = findViewById(R.id.connection_code)
        login = findViewById(R.id.auth_login)
        password = findViewById(R.id.auth_password)
        list.adapter = DialogAdapter(this, rows)
        list.setOnItemClickListener { _, _, pos, _ -> if (!busy) rows.getOrNull(pos)?.let { openChat(it.contactId) } }
        list.setOnScrollListener(object : AbsListView.OnScrollListener {
            override fun onScrollStateChanged(view: AbsListView?, scrollState: Int) {}
            override fun onScroll(view: AbsListView?, first: Int, visible: Int, total: Int) {
                if (visible > 0 && first + visible >= total - 3 && nextCursor != null && !busy && !restoring) loadDialogs(true)
            }
        })
        button(R.id.btn_preview) { previewCode() }
        button(R.id.btn_connection_scan) { scan(true) }
        button(R.id.btn_login) { selectAction(AuthAction.Login) }
        button(R.id.btn_signup) { selectAction(AuthAction.Signup) }
        button(R.id.btn_invitation_scan) {
            clearSecrets()
            val ticket = InvitationScanTransfer.begin()
            scanTicket = ticket
            departingForScan = true
            invitationScanner.launch(Intent(this, ScannerActivity::class.java)
                .putExtra(ScannerActivity.INVITATION_ONLY, true).putExtra(ScannerActivity.INVITATION_TICKET, ticket))
        }
        button(R.id.btn_invitation_file) {
            clearSecrets()
            invitationFilePicker.launch(Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            })
        }
        button(R.id.btn_invitation_cancel) { clearSecrets(); updateForm() }
        button(R.id.btn_auth_submit) { submit() }
        button(R.id.btn_policy_retry) { loadPolicy() }
        button(R.id.btn_scan) { addContact() }
        button(R.id.btn_menu) { menu() }
        button(R.id.btn_dialogs_retry) {
            if (state == LaunchState.Dialogs) {
                work(getString(R.string.refreshing_dns), { DmsgService.check(requireNotNull(facade)) }) { loadDialogs() }
            } else refresh()
        }
        button(R.id.connection_strip) { startActivity(Intent(this, DiagnosticsActivity::class.java)) }
        button(R.id.btn_paste) {
            val clip = getSystemService(ClipboardManager::class.java).primaryClip
            val text = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(this)
            if (text != null && text.length <= 8192) code.setText(text) else status.text = getString(R.string.clipboard_no_code)
        }
        button(R.id.btn_password_reveal) {
            password.transformationMethod = if (password.transformationMethod == null)
                android.text.method.PasswordTransformationMethod.getInstance() else null
            findViewById<Button>(R.id.btn_password_reveal).setText(if (password.transformationMethod == null) R.string.hide_password else R.string.show_password)
        }
    }

    private fun button(id: Int, click: () -> Unit) { findViewById<Button>(id).setOnClickListener { if (!busy) click() } }
    private fun scan(serverOnly: Boolean) {
        startActivity(Intent(this, ScannerActivity::class.java).putExtra(ScannerActivity.SERVER_ONLY, serverOnly))
    }

    override fun onResume() {
        super.onResume()
        refresh()
        handler.post(ticker)
    }

    override fun onPause() {
        memory.anchor = rows.getOrNull(list.firstVisiblePosition)?.contactId
        memory.offset = list.getChildAt(0)?.top ?: 0
        token++
        handler.removeCallbacks(ticker)
        flow?.cancel()
        activeSecrets?.clear(); activeSecrets = null
        clearSecrets()
        pendingInvitationFile = null
        if (!departingForScan) scanTicket?.let(InvitationScanTransfer::cancel)
        departingForScan = false
        code.text.clear()
        prompt?.dismiss(); prompt = null
        busy = false
        super.onPause()
    }

    private fun clearSecrets() {
        password.text.clear(); invitation.clear()
        password.transformationMethod = android.text.method.PasswordTransformationMethod.getInstance()
        findViewById<Button>(R.id.btn_password_reveal).setText(R.string.show_password)
        updateInvitationIndicator()
    }

    override fun onDestroy() {
        scanTicket?.let(InvitationScanTransfer::cancel)
        invitation.clear()
        super.onDestroy()
    }

    private fun updateInvitationIndicator() {
        findViewById<TextView>(R.id.invitation_status).text =
            getString(if (invitation.hasInvitation) R.string.invitation_imported else R.string.invitation_empty)
        findViewById<Button>(R.id.btn_invitation_cancel).isEnabled = !busy && invitation.hasInvitation
    }

    private fun acceptInvitation(value: CharArray) {
        if (state == LaunchState.Dialogs) { value.fill('\u0000'); return }
        password.text.clear()
        flow?.cancel()
        invitation.replace(value)
        action = AuthAction.Signup
        updateForm()
    }

    /** Picker and isolated file gates share this exact bounded stream-to-form path. */
    internal fun importInvitationFile(open: () -> java.io.InputStream) {
        if (busy || state != LaunchState.Authentication) return
        clearSecrets()
        val stamp = token
        busy = true
        status.text = getString(R.string.reading_invitation)
        updateForm()
        Core.dispatch {
            val result = runCatching { open().use(InvitationInput::read) }
            runOnUiThread {
                if (stamp != token || isFinishing || isDestroyed) {
                    result.getOrNull()?.fill('\u0000')
                    return@runOnUiThread
                }
                busy = false
                result.fold({
                    acceptInvitation(it)
                    status.text = getString(R.string.invitation_ready)
                    if (policy == null) loadPolicy()
                },
                    { status.text = getString(R.string.invitation_read_failed) })
                updateForm()
            }
        }
    }

    private fun render(next: LaunchState) {
        state = next
        findViewById<TextView>(R.id.main_title).setText(when (next) {
            LaunchState.Connection -> R.string.title_connection
            LaunchState.Authentication -> R.string.title_account
            LaunchState.Dialogs -> R.string.title_dialogs
        })
        findViewById<View>(R.id.btn_scan).visibility = if (next == LaunchState.Dialogs) View.VISIBLE else View.GONE
        findViewById<View>(R.id.connection_panel).visibility = if (next == LaunchState.Connection) View.VISIBLE else View.GONE
        findViewById<View>(R.id.auth_panel).visibility = if (next == LaunchState.Authentication) View.VISIBLE else View.GONE
        findViewById<View>(R.id.dialogs_panel).visibility = if (next == LaunchState.Dialogs) View.VISIBLE else View.GONE
        // Screenshots/recents must not capture credentials; dialogs contain no auth fields.
        if (next != LaunchState.Dialogs) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        else window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    private fun <T> work(message: String, task: () -> T, success: (T) -> Unit) {
        if (busy) return
        val stamp = token
        busy = true
        status.text = message
        updateForm()
        Core.dispatch {
            val result = runCatching(task)
            runOnUiThread {
                if (stamp != token || isFinishing || isDestroyed) return@runOnUiThread
                busy = false
                result.fold(success, { status.text = humanError(resources, it) })
                updateForm()
            }
        }
    }

    private fun refresh() {
        token++
        policy = null
        work(getString(R.string.opening_store), {
            val f = Core.facade(applicationContext)
            val configured = TrustedServerProfile.configureIfFresh(f, { DnsNetwork.resolvers(this) }) {
                if (TrustedServerProfile.ASSET in assets.list("").orEmpty()) assets.open(TrustedServerProfile.ASSET) else null
            }
            if (configured) DnsNetwork.mirrorProfile(this, f)
            val auth = AuthFlow(f)
            Triple(f, auth, auth.launchState())
        }) { (f, auth, next) ->
            facade = f; flow = auth
            render(next)
            when (next) {
                LaunchState.Connection -> status.text = getString(R.string.enter_server_code)
                LaunchState.Authentication -> loadPolicy()
                LaunchState.Dialogs -> { invitation.clear(); pendingInvitationFile = null; loadDialogs() }
            }
        }
    }

    private fun previewCode() {
        val input = code.text.toString()
        work(getString(R.string.checking_code), { QrGate.serverPreview(requireNotNull(facade), input) }) { p ->
            prompt = AlertDialog.Builder(this).setTitle(R.string.verify_server_title)
                .setMessage(getString(R.string.server_trust_preview, p.domain, p.fingerprint))
                .setNegativeButton(R.string.cancel, null)
                .setPositiveButton(R.string.accept_server) { _, _ ->
                    work(getString(R.string.saving_server), {
                        requireNotNull(facade).configureDns(p.code, DnsNetwork.resolvers(this))
                        DnsNetwork.mirrorProfile(this, requireNotNull(facade))
                    }) { code.text.clear(); render(LaunchState.Authentication); loadPolicy() }
                }.show()
        }
    }

    private fun loadPolicy() {
        val auth = flow ?: return
        val uri = pendingInvitationFile
        pendingInvitationFile = null
        if (uri != null) {
            importInvitationFile {
                contentResolver.openInputStream(uri) ?: throw java.io.IOException("Invitation file unavailable")
            }
            return
        }
        policy = null
        work(getString(R.string.checking_policy), { auth.policy() }) { result ->
            policy = result
            status.setText(if (result == RegistrationPolicy.OPEN) R.string.policy_open else R.string.policy_invitation)
        }
    }

    private fun selectAction(next: AuthAction) {
        clearSecrets()
        flow?.cancel()
        action = next
        updateForm()
    }

    private fun updateForm() {
        if (!::password.isInitialized) return
        login.hint = if (action == AuthAction.Signup) getString(R.string.signup_example) else ""
        val showInvitation = AuthForm.needsInvitation(action, policy) ||
            (action == AuthAction.Signup && invitation.hasInvitation)
        findViewById<View>(R.id.invitation_group).visibility = if (showInvitation) View.VISIBLE else View.GONE
        updateInvitationIndicator()
        findViewById<TextView>(R.id.auth_title).setText(if (action == AuthAction.Login) R.string.sign_in else R.string.sign_up)
        findViewById<Button>(R.id.btn_auth_submit).apply {
            setText(if (action == AuthAction.Login) R.string.sign_in else R.string.sign_up)
            isEnabled = !busy && (action == AuthAction.Login || policy != null)
        }
        findViewById<Button>(R.id.btn_policy_retry).isEnabled = !busy
        findViewById<Button>(R.id.btn_login).isEnabled = !busy
        findViewById<Button>(R.id.btn_signup).isEnabled = !busy
        login.isEnabled = !busy; password.isEnabled = !busy
        for (id in listOf(R.id.btn_preview, R.id.btn_connection_scan, R.id.btn_paste, R.id.btn_password_reveal,
            R.id.btn_menu, R.id.btn_scan, R.id.btn_dialogs_retry, R.id.btn_invitation_scan,
            R.id.btn_invitation_file)) findViewById<Button>(id).isEnabled = !busy
    }

    private fun submit() {
        val auth = flow ?: return
        val name = login.text.toString() // Core owns normalization.
        val secrets = AuthSecrets(password.text.toString().toCharArray(), invitation.take())
        activeSecrets = secrets
        val chosen = action
        val serverPolicy = policy
        clearSecrets()
        work(getString(R.string.checking_account), { auth.submit(chosen, serverPolicy, name, secrets) }, ::authOutcome)
    }

    private fun authOutcome(outcome: LoginOutcome) {
        when (outcome) {
            is LoginOutcome.Authenticated -> {
                clearSecrets()
                activeSecrets = null
                render(LaunchState.Dialogs)
                loadDialogs()
            }
            is LoginOutcome.ReplacementRequired -> {
                if (flow?.awaitingConfirmation != true) return
                status.setText(R.string.replacement_required)
                prompt = AlertDialog.Builder(this).setTitle(R.string.replacement_title)
                    .setMessage(R.string.replacement_warning)
                    .setNegativeButton(R.string.cancel) { _, _ -> cancelReplacement() }
                    .setOnCancelListener { cancelReplacement() }
                    .setPositiveButton(R.string.replace_device) { _, _ ->
                        work(getString(R.string.confirming_replacement), { requireNotNull(flow).confirm() }, ::authOutcome)
                    }.show()
            }
        }
    }

    private fun cancelReplacement() {
        flow?.cancel(); clearSecrets()
        activeSecrets?.clear(); activeSecrets = null
        status.setText(R.string.replacement_cancelled)
    }

    private fun loadDialogs(older: Boolean = false, restoreId: String? = null, restoreOffset: Int = 0) {
        val cursor = if (older) nextCursor ?: return else null
        val position = list.firstVisiblePosition
        val offset = list.getChildAt(0)?.top ?: 0
        val anchor = restoreId ?: if (!older) memory.anchor else null
        val anchorOffset = if (restoreId != null) restoreOffset else memory.offset
        restoring = anchor != null
        work(if (older) getString(R.string.loading_dialogs) else "", {
            val f = requireNotNull(facade)
            DnsNetwork.mirrorProfile(this, f)
            f.dialogsPage(cursor, 50)
        }) { page ->
            if (!older) rows.clear()
            val ids = rows.map { it.contactId }.toSet()
            rows.addAll(page.rows.filter { it.contactId !in ids })
            nextCursor = page.nextCursor
            (list.adapter as DialogAdapter).notifyDataSetChanged()
            val restoredPosition = anchor?.let { id -> rows.indexOfFirst { it.contactId == id } } ?: -1
            if (restoredPosition >= 0) {
                list.setSelectionFromTop(restoredPosition, anchorOffset)
                restoring = false
            } else if (anchor != null && nextCursor != null) {
                loadDialogs(true, anchor, anchorOffset)
            } else {
                restoring = false
                if (older) list.setSelectionFromTop(position, offset)
            }
            status.text = if (rows.isEmpty()) getString(R.string.no_dialogs) else ""
            findViewById<Button>(R.id.connection_strip).text = connectionLabel(resources, DmsgService.connectionState())
        }
    }

    private fun menu() {
        prompt = AlertDialog.Builder(this).setTitle(R.string.app_name)
            .setItems(arrayOf(getString(R.string.title_my_qr), getString(R.string.title_connectivity), getString(R.string.title_outbox), getString(R.string.title_storage))) { _, which ->
                val target = arrayOf(ProfileActivity::class.java, DiagnosticsActivity::class.java, OutboxActivity::class.java, StorageActivity::class.java)[which]
                startActivity(Intent(this, target).putExtra("mine", which == 0))
            }.setNegativeButton(R.string.close, null).show()
    }

    private fun addContact() {
        prompt = AlertDialog.Builder(this).setTitle(R.string.add_contact)
            .setItems(arrayOf(getString(R.string.scan_or_paste_qr), getString(R.string.enter_contact_id))) { _, which ->
                if (which == 0) scan(false) else startActivity(Intent(this, ProfileActivity::class.java))
            }.setNegativeButton(R.string.cancel, null).show()
    }

    private fun openChat(id: String) {
        val row = rows.find { it.contactId == id }
        val target = if (row?.state in setOf("incoming", "requested")) ProfileActivity::class.java else ChatActivity::class.java
        startActivity(Intent(this, target).putExtra("peer", id).putExtra("alias", row?.localAlias))
    }
}

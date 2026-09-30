package org.dmsg.client

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.WindowManager
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import androidx.core.app.ActivityCompat
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.RegistrationPolicy

/** Launch router: public connection import -> account forms -> paginated dialogs. */
class MainActivity : DmsgActivity() {
    private lateinit var status: TextView
    private lateinit var list: ListView
    private lateinit var code: EditText
    private lateinit var login: EditText
    private lateinit var password: EditText
    private lateinit var invitation: EditText
    private val rows = mutableListOf<String>()
    private val rowLabels = mutableListOf<String>()
    private var token = 0L
    private var state: LaunchState? = null
    private var action = AuthAction.Login
    private var policy: RegistrationPolicy? = null
    private var flow: AuthFlow? = null
    private var facade: DmsgFacade? = null
    private var prompt: AlertDialog? = null
    private var busy = false
    private var activeSecrets: AuthSecrets? = null
    private var accountLabel = ""
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            if (state == LaunchState.Dialogs && !busy) loadDialogs()
            handler.postDelayed(this, 5_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        status = findViewById(R.id.status)
        list = findViewById(R.id.dialogs)
        code = findViewById(R.id.connection_code)
        login = findViewById(R.id.auth_login)
        password = findViewById(R.id.auth_password)
        invitation = findViewById(R.id.auth_invitation)
        list.adapter = ArrayAdapter(this, android.R.layout.simple_list_item_1, rowLabels)
        list.setOnItemClickListener { _, _, pos, _ -> openChat(rows[pos]) }
        button(R.id.btn_chat) { rows.firstOrNull()?.let(::openChat) }
        button(R.id.btn_preview) { previewCode() }
        button(R.id.btn_connection_scan) { scan(true) }
        button(R.id.btn_login) { selectAction(AuthAction.Login) }
        button(R.id.btn_signup) { selectAction(AuthAction.Signup) }
        button(R.id.btn_auth_submit) { submit() }
        button(R.id.btn_policy_retry) { loadPolicy() }
        button(R.id.btn_profile) { startActivity(Intent(this, ProfileActivity::class.java)) }
        button(R.id.btn_scan) { scan(false) }
        button(R.id.btn_storage) { startActivity(Intent(this, StorageActivity::class.java)) }
        button(R.id.btn_diag) { startActivity(Intent(this, DiagnosticsActivity::class.java)) }
        button(R.id.btn_fgs) { toggleFgs() }
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
        token++
        handler.removeCallbacks(ticker)
        flow?.cancel()
        activeSecrets?.clear(); activeSecrets = null
        clearSecrets()
        code.text.clear()
        prompt?.dismiss(); prompt = null
        busy = false
        super.onPause()
    }

    private fun clearSecrets() { password.text.clear(); invitation.text.clear() }

    private fun render(next: LaunchState) {
        state = next
        findViewById<View>(R.id.connection_panel).visibility = if (next == LaunchState.Connection) View.VISIBLE else View.GONE
        findViewById<View>(R.id.auth_panel).visibility = if (next == LaunchState.Authentication) View.VISIBLE else View.GONE
        findViewById<View>(R.id.dialogs_panel).visibility = if (next == LaunchState.Dialogs) View.VISIBLE else View.GONE
        // Screenshots/recents must not capture credentials; dialogs contain no auth fields.
        if (next != LaunchState.Dialogs) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        else window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    private fun <T> work(message: String, task: () -> T, success: (T) -> Unit) {
        val stamp = token
        busy = true
        status.text = message
        updateForm()
        Core.dispatch {
            val result = runCatching(task)
            runOnUiThread {
                if (stamp != token || isFinishing || isDestroyed) return@runOnUiThread
                busy = false
                result.fold(success, { status.text = humanError(it) })
                updateForm()
            }
        }
    }

    private fun refresh() {
        token++
        policy = null
        work("Открываем защищённое хранилище…", {
            val f = Core.facade(applicationContext)
            val auth = AuthFlow(f)
            Triple(f, auth, auth.launchState())
        }) { (f, auth, next) ->
            facade = f; flow = auth
            render(next)
            when (next) {
                LaunchState.Connection -> status.text = "Вставьте публичный код сервера или сканируйте QR"
                LaunchState.Authentication -> loadPolicy()
                LaunchState.Dialogs -> loadDialogs()
            }
        }
    }

    private fun previewCode() {
        val input = code.text.toString()
        work("Проверяем код офлайн…", { QrGate.serverPreview(requireNotNull(facade), input) }) { p ->
            prompt = AlertDialog.Builder(this).setTitle("Проверьте сервер")
                .setMessage("Домен: ${p.domain}\nОтпечаток сертификата: ${p.fingerprint}\n\nСверьте данные с доверенным источником. Код публичный и не создаёт аккаунт.")
                .setNegativeButton("Отмена", null)
                .setPositiveButton("Принять сервер") { _, _ ->
                    work("Сохраняем сервер…", {
                        requireNotNull(facade).configureDns(p.code, DnsNetwork.resolvers(this))
                        DnsNetwork.mirrorProfile(this, requireNotNull(facade))
                    }) { code.text.clear(); render(LaunchState.Authentication); loadPolicy() }
                }.show()
        }
    }

    private fun loadPolicy() {
        val auth = flow ?: return
        invitation.text.clear()
        policy = null
        work("Проверяем режим регистрации через DNS…", { auth.policy() }) { result ->
            policy = result
            status.text = if (result == RegistrationPolicy.OPEN) "Сервер разрешает создание аккаунтов"
                else "Для создания аккаунта нужно приглашение. Для входа оно не требуется"
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
        val needsInvite = AuthForm.needsInvitation(action, policy)
        findViewById<View>(R.id.invitation_group).visibility = if (needsInvite) View.VISIBLE else View.GONE
        findViewById<TextView>(R.id.auth_title).text = if (action == AuthAction.Login) "Войти" else "Создать аккаунт"
        findViewById<Button>(R.id.btn_auth_submit).apply {
            text = if (action == AuthAction.Login) "Войти" else "Создать аккаунт"
            isEnabled = !busy && (action == AuthAction.Login || policy != null)
        }
        findViewById<Button>(R.id.btn_policy_retry).isEnabled = !busy
        findViewById<Button>(R.id.btn_login).isEnabled = !busy
        findViewById<Button>(R.id.btn_signup).isEnabled = !busy
        login.isEnabled = !busy; password.isEnabled = !busy; invitation.isEnabled = !busy
    }

    private fun submit() {
        val auth = flow ?: return
        val name = login.text.toString() // Core owns normalization.
        val secrets = AuthSecrets(password.text.toString().toCharArray(), invitation.text.toString().toCharArray())
        activeSecrets = secrets
        val chosen = action
        val serverPolicy = policy
        clearSecrets()
        work("Проверяем аккаунт через DNS…", { auth.submit(chosen, serverPolicy, name, secrets) }, ::authOutcome)
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
                status.text = "Нужно подтвердить замену устройства"
                prompt = AlertDialog.Builder(this).setTitle("Заменить прежнее устройство?")
                    .setMessage("Прежнее устройство потеряет доступ. Старая история на этом устройстве недоступна. Контактам потребуется подтвердить новый ключ.")
                    .setNegativeButton("Отмена") { _, _ -> cancelReplacement() }
                    .setOnCancelListener { cancelReplacement() }
                    .setPositiveButton("Заменить устройство") { _, _ ->
                        work("Подтверждаем замену через DNS…", { requireNotNull(flow).confirm() }, ::authOutcome)
                    }.show()
            }
        }
    }

    private fun cancelReplacement() {
        flow?.cancel(); clearSecrets()
        activeSecrets?.clear(); activeSecrets = null
        status.text = "Замена отменена. Прежнее устройство сохраняет доступ"
    }

    private fun loadDialogs() {
        work("Открываем диалоги…", {
            val f = requireNotNull(facade)
            DnsNetwork.mirrorProfile(this, f)
            val dialogs = mutableListOf<Dialog>()
            var cursor: String? = null
            do {
                val (page, next) = f.contacts(cursor, 50)
                dialogs.addAll(page)
                cursor = next
            } while (cursor != null)
            Pair(f.account().contactId, dialogs)
        }) { (id, dialogs) ->
            rows.clear(); rows.addAll(dialogs.map { it.contactId })
            rowLabels.clear(); rowLabels.addAll(dialogs.map {
                "${it.contactId} — ${it.state}" + (if (it.identityMismatch) "\nКлюч изменился: отправка СТОП" else "")
            })
            (list.adapter as ArrayAdapter<*>).notifyDataSetChanged()
            accountLabel = "Мой ID: $id"
            status.text = "$accountLabel\n${DmsgService.pollStatus()}"
        }
    }

    private fun openChat(id: String) { startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", id)) }
    private fun toggleFgs() {
        if (state != LaunchState.Dialogs) return
        if (DmsgService.running(this)) DmsgService.stop(this) else {
            if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED)
                ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
            DmsgService.start(this)
        }
        status.postDelayed({ if (!isFinishing && !isDestroyed) status.text = "$accountLabel\n${DmsgService.pollStatus()}" }, 500)
    }
}

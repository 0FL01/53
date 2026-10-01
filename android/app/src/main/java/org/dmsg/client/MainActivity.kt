package org.dmsg.client

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.WindowManager
import android.content.ClipboardManager
import android.widget.AbsListView
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
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
    private lateinit var invitation: EditText
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
            if (state == LaunchState.Dialogs) findViewById<Button>(R.id.connection_strip).text = connectionLabel(DmsgService.connectionState())
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
        invitation = findViewById(R.id.auth_invitation)
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
        button(R.id.btn_auth_submit) { submit() }
        button(R.id.btn_policy_retry) { loadPolicy() }
        button(R.id.btn_scan) { addContact() }
        button(R.id.btn_menu) { menu() }
        button(R.id.btn_dialogs_retry) {
            if (state == LaunchState.Dialogs) {
                work("Обновляем сообщения через DNS…", { DmsgService.check(requireNotNull(facade)) }) { loadDialogs() }
            } else refresh()
        }
        button(R.id.connection_strip) { startActivity(Intent(this, DiagnosticsActivity::class.java)) }
        button(R.id.btn_paste) {
            val clip = getSystemService(ClipboardManager::class.java).primaryClip
            val text = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(this)
            if (text != null && text.length <= 8192) code.setText(text) else status.text = "В буфере нет подходящего кода"
        }
        button(R.id.btn_password_reveal) {
            password.transformationMethod = if (password.transformationMethod == null)
                android.text.method.PasswordTransformationMethod.getInstance() else null
            findViewById<Button>(R.id.btn_password_reveal).text = if (password.transformationMethod == null) "Скрыть пароль" else "Показать пароль"
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
        code.text.clear()
        prompt?.dismiss(); prompt = null
        busy = false
        super.onPause()
    }

    private fun clearSecrets() {
        password.text.clear(); invitation.text.clear()
        password.transformationMethod = android.text.method.PasswordTransformationMethod.getInstance()
        findViewById<Button>(R.id.btn_password_reveal).text = "Показать пароль"
    }

    private fun render(next: LaunchState) {
        state = next
        findViewById<TextView>(R.id.main_title).text = when (next) {
            LaunchState.Connection -> "53 · Подключение"
            LaunchState.Authentication -> "Аккаунт"
            LaunchState.Dialogs -> "Диалоги"
        }
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
        for (id in listOf(R.id.btn_preview, R.id.btn_connection_scan, R.id.btn_paste, R.id.btn_password_reveal,
            R.id.btn_menu, R.id.btn_scan, R.id.btn_dialogs_retry)) findViewById<Button>(id).isEnabled = !busy
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

    private fun loadDialogs(older: Boolean = false, restoreId: String? = null, restoreOffset: Int = 0) {
        val cursor = if (older) nextCursor ?: return else null
        val position = list.firstVisiblePosition
        val offset = list.getChildAt(0)?.top ?: 0
        val anchor = restoreId ?: if (!older) memory.anchor else null
        val anchorOffset = if (restoreId != null) restoreOffset else memory.offset
        restoring = anchor != null
        work(if (older) "Загружаем ещё диалоги…" else "", {
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
            status.text = if (rows.isEmpty()) "Пока нет диалогов. Добавьте контакт по QR или ID." else ""
            findViewById<Button>(R.id.connection_strip).text = connectionLabel(DmsgService.connectionState())
        }
    }

    private fun menu() {
        prompt = AlertDialog.Builder(this).setTitle("53")
            .setItems(arrayOf("Мой QR", "Связь", "Очередь", "Хранилище")) { _, which ->
                val target = arrayOf(ProfileActivity::class.java, DiagnosticsActivity::class.java, OutboxActivity::class.java, StorageActivity::class.java)[which]
                startActivity(Intent(this, target).putExtra("mine", which == 0))
            }.setNegativeButton("Закрыть", null).show()
    }

    private fun addContact() {
        prompt = AlertDialog.Builder(this).setTitle("Добавить контакт")
            .setItems(arrayOf("Сканировать / вставить QR", "Ввести контактный ID")) { _, which ->
                if (which == 0) scan(false) else startActivity(Intent(this, ProfileActivity::class.java))
            }.setNegativeButton("Отмена", null).show()
    }

    private fun openChat(id: String) { startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", id).putExtra("alias", rows.find { it.contactId == id }?.localAlias)) }
}

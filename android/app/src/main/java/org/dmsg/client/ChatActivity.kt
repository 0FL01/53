package org.dmsg.client

import android.content.Intent
import android.content.res.Resources
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.AbsListView
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.appcompat.app.AlertDialog
import uniffi.dmsg_core.DeleteScope

/** Retained only in memory: Rust owns durable history; drafts never enter Prefs or saved-state. */
class ChatMemory : ViewModel() {
    internal val history = HistoryWindow()
    internal val composition = MessageComposer()
    internal val outgoing get() = composition.normal
    internal var draft: String
        get() = composition.text
        set(value) { composition.text = value }
    internal var anchor: HistoryAnchor? = null
    internal var pending = false
    // Render after recreation with current resources; capture no Activity or Context.
    internal var action: (Resources) -> String = { "" }
    internal var contact: Dialog? = null
    internal var readThrough = 0L
    internal var uncertain: TextSendOutcome.Uncertain? = null
    internal var uncertainAction: MessageActionAttempt? = null
    internal var rebaseEdit = false
}

class ChatActivity : DmsgActivity() {
    private lateinit var memory: ChatMemory
    private lateinit var list: ListView
    private lateinit var composer: EditText
    private lateinit var info: TextView
    private lateinit var adapter: HistoryAdapter
    private val pageGuard = UiGuard()
    @Volatile private var active = false
    @Volatile private var lifecycleStamp = 0L
    private var readPending = false
    private var pageError = ""
    private var prompt: AlertDialog? = null
    private val id get() = intent.getStringExtra("peer").orEmpty()
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            if (!active) return
            if (!pageGuard.pending && !memory.pending) {
                when {
                    memory.uncertain != null -> resolveUncertain()
                    memory.uncertainAction != null -> resolveAction()
                    else -> loadPage()
                }
            }
            render()
            handler.postDelayed(this, 3_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_chat)
        memory = ViewModelProvider(this)[ChatMemory::class.java]
        NativeUi.back(this, getString(R.string.title_chat))
        list = findViewById(R.id.messages)
        composer = findViewById(R.id.composer)
        info = findViewById(R.id.info)
        composer.setText(memory.draft)
        composer.isSaveEnabled = false
        composer.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) { memory.draft = s?.toString().orEmpty() }
            override fun afterTextChanged(s: android.text.Editable?) {}
        })
        adapter = HistoryAdapter(this, memory.history.rows,
            { canChangeMessage(it, memory.contact) }, ::startEdit, ::confirmDelete, ::messageMenu)
        list.adapter = adapter
        list.setOnScrollListener(object : AbsListView.OnScrollListener {
            override fun onScrollStateChanged(view: AbsListView?, state: Int) { if (state == AbsListView.OnScrollListener.SCROLL_STATE_IDLE) markViewed() }
            override fun onScroll(view: AbsListView?, first: Int, visible: Int, total: Int) {
                if (active && visible > 0) {
                    markViewed()
                    if (first <= 2 && memory.history.initialized && pageError.isEmpty()) loadPage(true)
                }
            }
        })
        findViewById<Button>(R.id.btn_send).setOnClickListener { send() }
        findViewById<Button>(R.id.btn_save_edit).setOnClickListener { saveEdit() }
        findViewById<Button>(R.id.btn_cancel_edit).setOnClickListener {
            if (!memory.pending && memory.uncertain == null && memory.uncertainAction == null) {
                memory.composition.cancel(); memory.rebaseEdit = false; render()
            }
        }
        findViewById<Button>(R.id.btn_retry).setOnClickListener { retry() }
        findViewById<Button>(R.id.btn_history_retry).setOnClickListener {
            refreshReceived()
        }
        findViewById<Button>(R.id.btn_contact).setOnClickListener {
            startActivity(Intent(this, ProfileActivity::class.java).putExtra("peer", id))
        }
    }

    override fun onResume() {
        super.onResume()
        active = true
        if (memory.uncertain == null) TextSendCoordinator.pendingFor(id)?.let {
            memory.uncertain = it
            if (memory.outgoing.text.isEmpty()) memory.outgoing.text = it.text
            memory.outgoing.begin()
        }
        if (memory.uncertainAction == null) TextSendCoordinator.pendingActionFor(id)?.let {
            memory.uncertainAction = it
            memory.composition.restore(it)
        }
        adapter.notifyDataSetChanged()
        restoreAnchor(memory.anchor)
        render()
        handler.post(ticker)
    }

    override fun onPause() {
        memory.draft = composer.text.toString()
        memory.anchor = captureAnchor()
        active = false; lifecycleStamp++
        pageGuard.stop(); readPending = false
        handler.removeCallbacks(ticker)
        prompt?.dismiss(); prompt = null
        super.onPause()
    }

    override fun onImeVisibilityChanged(visible: Boolean) {
        findViewById<View>(R.id.chat_actions)?.visibility = if (visible) View.GONE else View.VISIBLE
        // Large-font landscape leaves very little height above the keyboard.
        // Keep the draft/send target reachable; dismissing IME restores navigation.
        val compact = visible && resources.configuration.orientation == android.content.res.Configuration.ORIENTATION_LANDSCAPE
        findViewById<View>(R.id.chat_header)?.visibility = if (compact) View.GONE else View.VISIBLE
        findViewById<EditText>(R.id.composer)?.maxLines = if (compact) 1 else 3
    }

    private fun render() {
        if (!active) return
        if (adapter.visibleRows != memory.history.visibleRows) {
            val anchor = captureAnchor()
            adapter.notifyDataSetChanged()
            restoreAnchor(anchor)
        }
        if (composer.text.toString() != memory.draft) composer.setText(memory.draft)
        val allowed = contactCta(memory.contact) == ContactCta.Chat
        val editing = memory.composition.edit
        val unresolved = memory.uncertain != null || memory.uncertainAction != null
        findViewById<TextView>(R.id.peer).text = intent.getStringExtra("alias") ?: id
        findViewById<Button>(R.id.btn_contact).contentDescription = getString(if (allowed) R.string.contact_card else R.string.check_contact)
        findViewById<Button>(R.id.btn_send).visibility = if (editing == null) View.VISIBLE else View.GONE
        findViewById<View>(R.id.edit_banner).visibility = if (editing == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_save_edit).visibility = if (editing == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_cancel_edit).visibility = if (editing == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_save_edit).isEnabled = editing != null && !editing.unavailable && !memory.rebaseEdit && allowed && !memory.pending && !unresolved && !pageGuard.pending
        findViewById<Button>(R.id.btn_cancel_edit).isEnabled = !memory.pending && !unresolved
        findViewById<Button>(R.id.btn_send).isEnabled = allowed && editing == null && !memory.pending && !unresolved
        findViewById<Button>(R.id.btn_retry).isEnabled = allowed && !memory.pending
        composer.isEnabled = (allowed || editing != null) && !memory.pending && !unresolved
        findViewById<Button>(R.id.btn_history_retry).isEnabled = !pageGuard.pending && !memory.pending
        info.text = listOf(if (allowed) "" else trustLabel(resources, memory.contact), memory.action(resources), pageError).filter { it.isNotEmpty() }.joinToString("\n")
        findViewById<View>(R.id.chat_notice).visibility = if (info.text.isEmpty()) View.GONE else View.VISIBLE
        info.setBackgroundResource(if (memory.contact?.identityMismatch == true || memory.contact?.state == "blocked") R.color.error_surface
            else if (allowed && pageError.isEmpty()) R.color.surface else R.color.warning_surface)
    }

    private fun captureAnchor(): HistoryAnchor? = historyAnchor(adapter.visibleRows, list.firstVisiblePosition,
        list.getChildAt(0)?.top ?: 0, adapter.count > 0 && list.lastVisiblePosition >= adapter.count - 2)
        ?.also { memory.anchor = it } ?: memory.anchor
    private fun restoreAnchor(anchor: HistoryAnchor?) {
        if (anchor == null) return
        anchorPosition(adapter.visibleRows, anchor, memory.history.rows)?.let {
            if (anchor.followBottom) list.setSelection(it) else list.setSelectionFromTop(it, anchor.offset)
        }
    }

    private fun loadPage(older: Boolean = false) {
        if (!active || memory.pending) return
        val bridge = memory.history.gapBefore != null
        val continuing = !bridge && memory.history.hiddenBefore != null
        val before = memory.history.gapBefore ?: memory.history.hiddenBefore ?: if (older) memory.history.nextBefore ?: return else null
        val stamp = pageGuard.begin() ?: return
        val anchor = captureAnchor()
        val bottom = !memory.history.initialized || adapter.count == 0 || anchor?.followBottom == true
        val normalRefresh = !older && !bridge && !continuing
        val retainedIds = if (normalRefresh) memory.history.rows.map { it.localId }
            else if (memory.rebaseEdit) listOfNotNull(memory.composition.edit?.localId) else emptyList()
        val ingestionThrough = memory.history.latestLocalId
        Core.dispatch {
            val result = runCatching {
                synchronized(Core.storeLock) {
                    val f = Core.facade(applicationContext)
                    val contact = f.get(id)
                    // Three raw pages per worker turn; continuation survives hidden-only results.
                    val pages = timelineChunk(before) { f.timelinePage(id, it, 50) }
                    val refreshed = retainedIds.map { f.historyMessage(id, it) }.toMutableList()
                    val head = if (normalRefresh) f.historyPage(id, null, 1).rows.firstOrNull()?.localId else null
                    // Late receives can rank BELOW the newest server-order page.
                    // Discover them by append-only ingestion IDs, not timeline rank.
                    if (head != null && ingestionThrough != null && head > ingestionThrough) {
                        var cursor: Long? = null
                        do {
                            val appended = f.historyPage(id, cursor, 50)
                            refreshed.addAll(appended.rows.filter { it.localId > ingestionThrough })
                            cursor = if (appended.rows.lastOrNull()?.localId?.let { it > ingestionThrough } == true) appended.nextBeforeLocalId else null
                        } while (cursor != null)
                    }
                    Triple(contact, pages, refreshed to head)
                }
            }
            runOnUiThread {
                if (!active || !pageGuard.finish(stamp)) return@runOnUiThread
                result.fold({ (contact, pages, refresh) ->
                    memory.contact = contact
                    pageError = ""
                    pages.forEachIndexed { index, page ->
                        if ((index == 0 && (older || continuing) && !bridge) || (index > 0 && memory.history.gapBefore == null)) memory.history.older(page)
                        else memory.history.latest(page, if (index == 0) refresh.first else emptyList(), if (index == 0) refresh.second else null)
                    }
                    refresh.first.forEach(memory.history::replace)
                    memory.composition.edit?.let { draft ->
                        memory.history.rows.find { it.localId == draft.localId }?.let {
                            val exact = refresh.first.any { row -> row.localId == draft.localId } ||
                                pages.any { page -> page.rows.any { row -> row.localId == draft.localId } }
                            if (!memory.rebaseEdit || exact) {
                                memory.composition.refresh(it, memory.rebaseEdit)
                                memory.rebaseEdit = false
                            }
                        }
                    }
                    adapter.notifyDataSetChanged()
                    list.post {
                        if (!active) return@post
                        if (bottom && (!older || continuing) && !bridge) list.setSelection(adapter.count - 1)
                        else restoreAnchor(anchor)
                        markViewed()
                        if (memory.history.gapBefore != null) loadPage()
                    }
                }, { pageError = humanError(resources, it) })
                render()
            }
        }
    }

    private fun markViewed() {
        if (!active || readPending || pageGuard.pending || list.childCount == 0) return
        // Use only intersecting rendered rows, never a page's newest cursor or fetch report.
        val visible = (0 until list.childCount).filter {
            val child = list.getChildAt(it)
            child.bottom > list.paddingTop && child.top < list.height - list.paddingBottom
        }.mapNotNull { list.getChildAt(it).tag as? Long }
        val anchor = memory.history.viewedAnchor(visible) ?: return
        if (anchor <= memory.readThrough) return
        val stamp = lifecycleStamp
        readPending = true
        Core.dispatch {
            // A paused activity must not start a fresh read mutation.
            val result = if (active && stamp == lifecycleStamp) runCatching { Core.facade(applicationContext).markRead(id, anchor) } else null
            runOnUiThread {
                if (!active || stamp != lifecycleStamp) return@runOnUiThread
                readPending = false
                result?.onSuccess { memory.readThrough = maxOf(memory.readThrough, it) }
            }
        }
    }

    private fun actionsReady() = active && !memory.pending && !pageGuard.pending && memory.uncertain == null && memory.uncertainAction == null
    private fun ownRow(localId: Long) = memory.history.rows.find { it.localId == localId && canHideMessage(it) }

    private fun messageMenu(localId: Long) {
        if (!actionsReady()) return
        val row = ownRow(localId) ?: return
        val editable = canChangeMessage(row, memory.contact) && memory.composition.edit == null
        val labels = if (editable) arrayOf(getString(R.string.edit_whole_message), getString(R.string.delete_whole_message))
            else arrayOf(getString(R.string.delete_whole_message))
        prompt = AlertDialog.Builder(this).setTitle(R.string.message_actions).setItems(labels) { _, which ->
            if (editable && which == 0) startEdit(localId) else confirmDelete(localId)
        }.setNegativeButton(R.string.cancel, null).show()
    }

    private fun startEdit(localId: Long) {
        if (!actionsReady()) return
        val row = ownRow(localId) ?: return
        if (!canChangeMessage(row, memory.contact) || !memory.composition.start(row)) return
        memory.rebaseEdit = false
        render()
        composer.requestFocus()
        composer.setSelection(composer.text.length)
    }

    private fun saveEdit() {
        if (!actionsReady() || memory.rebaseEdit) return
        val draft = memory.composition.edit ?: return
        val row = ownRow(draft.localId)
        if (draft.unavailable || row == null || !canChangeMessage(row, memory.contact)) {
            memory.composition.unavailable()
            memory.action = { it.getString(R.string.error_message_unavailable) }; render(); return
        }
        memory.draft = composer.text.toString()
        if (draft.text.isEmpty() || draft.text.toByteArray(Charsets.UTF_8).size > 4096) {
            memory.action = { it.getString(R.string.message_bounds) }; render(); return
        }
        // Even unchanged text goes through native CAS validation.
        mutate(draft.localId, MessageActionCommand.Edit(draft.expectedRevision, draft.text))
    }

    private fun confirmDelete(localId: Long) {
        if (!actionsReady()) return
        val row = ownRow(localId) ?: return
        val scopes = deleteScopes(row, memory.contact)
        var scope = scopes.first()
        val content = android.widget.LinearLayout(this).apply {
            orientation = android.widget.LinearLayout.VERTICAL
            setPadding(NativeUi.dp(context, 20), NativeUi.dp(context, 8), NativeUi.dp(context, 20), 0)
        }
        content.addView(NativeUi.text(this, 14f).apply {
            text = getString(R.string.delete_warning) + if (row.deliveryState == uniffi.dmsg_core.DeliveryState.QUEUED) "\n" + getString(R.string.self_hide_warning) else ""
        })
        val choices = android.widget.RadioGroup(this)
        val selfId = View.generateViewId()
        val everyoneId = View.generateViewId()
        choices.addView(android.widget.RadioButton(this).apply {
            this.id = selfId; setText(R.string.delete_self); minHeight = NativeUi.dp(context, 48)
        })
        if (DeleteScope.EVERYONE in scopes) choices.addView(android.widget.RadioButton(this).apply {
            this.id = everyoneId; setText(R.string.delete_everyone); minHeight = NativeUi.dp(context, 48)
        })
        choices.check(selfId)
        choices.setOnCheckedChangeListener { _, checked -> scope = if (checked == everyoneId) DeleteScope.EVERYONE else DeleteScope.SELF_ONLY }
        content.addView(choices)
        val scroll = android.widget.ScrollView(this).apply { addView(content) }
        prompt = AlertDialog.Builder(this).setTitle(R.string.delete_title).setView(scroll)
            .setNegativeButton(R.string.cancel, null).setPositiveButton(R.string.delete_confirm) { _, _ ->
                if (!actionsReady()) return@setPositiveButton
                val current = ownRow(localId) ?: return@setPositiveButton
                if (scope == DeleteScope.EVERYONE && !canChangeMessage(current, memory.contact)) return@setPositiveButton
                mutate(localId, MessageActionCommand.Delete(scope))
            }.show()
    }

    private fun mutate(localId: Long, command: MessageActionCommand) {
        memory.pending = true
        memory.action = { it.getString(R.string.saving_change) }
        render()
        Core.dispatch {
            val outcome = try { TextSendCoordinator.mutate(Core.facade(applicationContext), id, localId, command) }
                catch (e: Exception) { MessageActionOutcome.NotSaved(e) } // facade acquisition failed before writer
            runOnUiThread {
                // Durable state belongs to the retained VM, even after pause/recreation.
                memory.pending = false
                completeAction(command, localId, outcome)
                if (active) { render(); loadPage() }
            }
        }
    }

    private fun completeAction(command: MessageActionCommand, localId: Long, outcome: MessageActionOutcome) {
        when (outcome) {
            is MessageActionOutcome.Saved -> {
                memory.uncertainAction = null
                memory.history.replace(outcome.row)
                if (command is MessageActionCommand.Edit) memory.composition.saved(localId)
                else memory.composition.refresh(outcome.row)
                memory.rebaseEdit = false
                val messageRes = actionSavedRes(command, outcome.row)
                val delivery = outcome.row.changeDeliveryState
                val pendingNetwork = outcome.networkError != null
                memory.action = { res ->
                    res.getString(messageRes, deliveryLabel(res, delivery)) + if (pendingNetwork) "\n" + res.getString(R.string.change_pending) else ""
                }
            }
            is MessageActionOutcome.NotSaved -> {
                memory.uncertainAction = null
                if ((outcome.error as? DmsgError)?.kind == ErrorKind.MessageChanged && command is MessageActionCommand.Edit) memory.rebaseEdit = true
                if ((outcome.error as? DmsgError)?.kind == ErrorKind.MessageUnavailable && memory.composition.edit?.localId == localId) memory.composition.unavailable()
                val messageRes = humanErrorRes(outcome.error)
                memory.action = { it.getString(R.string.change_not_saved, it.getString(messageRes)) }
            }
            is MessageActionOutcome.Uncertain -> {
                memory.uncertainAction = outcome.attempt
                memory.action = { it.getString(R.string.change_uncertain) }
            }
        }
    }

    private fun resolveAction() {
        val attempt = memory.uncertainAction ?: return
        val stamp = pageGuard.begin() ?: return
        Core.dispatch {
            val outcome = try { TextSendCoordinator.reconcileAction(Core.facade(applicationContext), attempt) }
                catch (_: Exception) { MessageActionOutcome.Uncertain(attempt) }
            runOnUiThread {
                if (memory.uncertainAction != attempt) return@runOnUiThread
                completeAction(attempt.command, attempt.before.localId, outcome)
                if (!active || !pageGuard.finish(stamp)) return@runOnUiThread
                render()
                if (memory.uncertainAction == null) loadPage()
            }
        }
    }

    private fun send() {
        if (memory.composition.edit != null || memory.pending || memory.uncertain != null || memory.uncertainAction != null || contactCta(memory.contact) != ContactCta.Chat) { render(); return }
        val text = composer.text.toString()
        if (text.isEmpty() || text.toByteArray(Charsets.UTF_8).size > 4096) {
            memory.action = { it.getString(R.string.message_bounds) }; render(); return
        }
        memory.draft = text
        memory.outgoing.begin() ?: return
        memory.pending = true
        memory.action = { it.getString(R.string.saving_sending) }
        render()
        Core.dispatch {
            val outcome = try { TextSendCoordinator.send(Core.facade(applicationContext), id, text) }
                catch (e: Exception) { TextSendOutcome.NotSaved(e) }
            val status = (outcome as? TextSendOutcome.Saved)?.let { saved -> runCatching { Core.facade(applicationContext).messageStatus(saved.messageId) }.getOrNull() }
            runOnUiThread {
                memory.pending = false
                completeSend(outcome, status)
                if (active) { render(); loadPage() }
            }
        }
    }

    private fun completeSend(outcome: TextSendOutcome, status: uniffi.dmsg_core.DeliveryState?) {
        when (outcome) {
            is TextSendOutcome.Saved -> {
                memory.uncertain = null
                memory.outgoing.finish(true)
                val recovered = outcome.recoveredAfterError
                memory.action = { res ->
                    res.getString(R.string.message_saved, deliveryLabel(res, status)) +
                        if (recovered) "\n" + res.getString(R.string.send_recovered) else ""
                }
            }
            is TextSendOutcome.NotSaved -> {
                memory.uncertain = null; memory.outgoing.finish(false)
                val errorRes = humanErrorRes(outcome.error)
                memory.action = { res -> res.getString(R.string.message_not_sent, res.getString(errorRes)) }
            }
            is TextSendOutcome.Uncertain -> {
                memory.uncertain = outcome
                memory.action = { it.getString(R.string.send_uncertain) }
            }
        }
    }

    private fun resolveUncertain() {
        val uncertain = memory.uncertain ?: return
        val stamp = pageGuard.begin() ?: return
        Core.dispatch {
            val outcome = try { TextSendCoordinator.reconcile(Core.facade(applicationContext), id, uncertain) }
                catch (_: Exception) { uncertain }
            val status = (outcome as? TextSendOutcome.Saved)?.let { runCatching { Core.facade(applicationContext).messageStatus(it.messageId) }.getOrNull() }
            runOnUiThread {
                if (memory.uncertain != uncertain) return@runOnUiThread
                completeSend(outcome, status)
                if (!active || !pageGuard.finish(stamp)) return@runOnUiThread
                render()
                if (memory.uncertain == null) loadPage()
            }
        }
    }

    private fun retry() {
        if (memory.pending || contactCta(memory.contact) != ContactCta.Chat) return
        memory.pending = true; memory.action = { it.getString(R.string.retrying_saved_queue) }; render()
        Core.dispatch {
            val result = runCatching { Core.facade(applicationContext).retry() }
            runOnUiThread {
                memory.pending = false
                val messageRes = result.fold({ R.string.retry_complete }, ::humanErrorRes)
                memory.action = { it.getString(messageRes) }
                if (active) { render(); loadPage() }
            }
        }
    }

    private fun refreshReceived() {
        if (memory.pending || pageGuard.pending) return
        memory.pending = true; memory.action = { it.getString(R.string.refreshing_dns) }; render()
        Core.dispatch {
            val result = runCatching { DmsgService.check(Core.facade(applicationContext)) }
            runOnUiThread {
                memory.pending = false
                memory.action = result.fold({ report ->
                    val count = report.received.size
                    val pending = report.skipped.getOrElse(0) { 0 }
                    val render: (Resources) -> String = { res ->
                        res.getString(R.string.received_local, count) +
                            if (pending > 0) "\n" + res.getString(R.string.pending_contacts, pending) else ""
                    }
                    render
                }, { error ->
                    val messageRes = humanErrorRes(error)
                    val render: (Resources) -> String = { res -> res.getString(messageRes) }
                    render
                })
                pageError = ""
                if (active) { render(); loadPage() }
            }
        }
    }
}

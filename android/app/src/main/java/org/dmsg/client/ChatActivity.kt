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
import uniffi.dmsg_core.MessageDirection

/** Retained only in memory: Rust owns durable history; drafts never enter Prefs or saved-state. */
class ChatMemory : ViewModel() {
    internal val history = HistoryWindow()
    internal val outgoing = OutgoingDraft()
    internal var draft: String
        get() = outgoing.text
        set(value) { outgoing.text = value }
    internal var position = 0
    internal var offset = 0
    internal var pending = false
    // Render after recreation with current resources; capture no Activity or Context.
    internal var action: (Resources) -> String = { "" }
    internal var contact: Dialog? = null
    internal var readThrough = 0L
    internal var uncertain: TextSendOutcome.Uncertain? = null
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
    private var gapBefore: Long? = null
    private var gapThrough: Long? = null
    private var pageError = ""
    private val id get() = intent.getStringExtra("peer").orEmpty()
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            if (!active) return
            if (!pageGuard.pending && !memory.pending) {
                if (memory.uncertain != null) resolveUncertain() else loadPage()
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
        adapter = HistoryAdapter(this, memory.history.rows)
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
            if (memory.draft.isEmpty()) memory.draft = it.text
            memory.outgoing.begin()
        }
        adapter.notifyDataSetChanged()
        list.setSelectionFromTop(memory.position, memory.offset)
        render()
        handler.post(ticker)
    }

    override fun onPause() {
        memory.draft = composer.text.toString()
        memory.position = list.firstVisiblePosition
        memory.offset = list.getChildAt(0)?.top ?: 0
        active = false; lifecycleStamp++
        pageGuard.stop(); readPending = false
        handler.removeCallbacks(ticker)
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
        if (composer.text.toString() != memory.draft) composer.setText(memory.draft)
        val allowed = contactCta(memory.contact) == ContactCta.Chat
        findViewById<TextView>(R.id.peer).text = intent.getStringExtra("alias") ?: id
        findViewById<Button>(R.id.btn_contact).contentDescription = getString(if (allowed) R.string.contact_card else R.string.check_contact)
        findViewById<Button>(R.id.btn_send).isEnabled = allowed && !memory.pending && memory.uncertain == null
        findViewById<Button>(R.id.btn_retry).isEnabled = allowed && !memory.pending
        composer.isEnabled = allowed && !memory.pending && memory.uncertain == null
        findViewById<Button>(R.id.btn_history_retry).isEnabled = !pageGuard.pending && !memory.pending
        info.text = listOf(if (allowed) "" else trustLabel(resources, memory.contact), memory.action(resources), pageError).filter { it.isNotEmpty() }.joinToString("\n")
        findViewById<View>(R.id.chat_notice).visibility = if (info.text.isEmpty()) View.GONE else View.VISIBLE
        info.setBackgroundResource(if (memory.contact?.identityMismatch == true || memory.contact?.state == "blocked") R.color.error_surface
            else if (allowed && pageError.isEmpty()) R.color.surface else R.color.warning_surface)
    }

    private fun loadPage(older: Boolean = false) {
        if (!active || memory.pending) return
        val before = if (gapBefore != null) gapBefore else if (older) memory.history.nextBefore ?: return else null
        val stamp = pageGuard.begin() ?: return
        val firstId = memory.history.rows.getOrNull(list.firstVisiblePosition)?.localId
        val offset = list.getChildAt(0)?.top ?: 0
        val bottom = !memory.history.initialized || list.lastVisiblePosition >= adapter.count - 2
        val previousNewest = memory.history.rows.lastOrNull()?.localId
        val bridge = gapBefore != null
        val outgoingIds = memory.history.rows.filter { it.direction == MessageDirection.OUTGOING && it.deliveryState != uniffi.dmsg_core.DeliveryState.DELIVERED }.map { it.messageIdHex }
        Core.dispatch {
            val result = runCatching {
                val f = Core.facade(applicationContext)
                val contact = f.get(id)
                val page = f.historyPage(id, before, 50)
                // Exact status is also refreshed for older loaded outgoing rows.
                val statuses = outgoingIds.map { it to f.messageStatus(it) }
                Triple(contact, page, statuses)
            }
            runOnUiThread {
                if (!active || !pageGuard.finish(stamp)) return@runOnUiThread
                result.fold({ (contact, page, statuses) ->
                    memory.contact = contact
                    pageError = ""
                    if (older && !bridge) memory.history.older(page) else memory.history.latest(page)
                    statuses.forEach { (mid, status) ->
                        // Unknown never upgrades a previously known state.
                        if (status != null) memory.history.rows.find { it.messageIdHex == mid }?.deliveryState = status
                    }
                    if (bridge) {
                        gapBefore = if (page.rows.any { it.localId <= (gapThrough ?: 0L) }) null else page.nextBeforeLocalId
                    } else if (!older && previousNewest != null && page.rows.isNotEmpty() && page.rows.last().localId > previousNewest) {
                        gapThrough = previousNewest; gapBefore = page.nextBeforeLocalId
                    }
                    adapter.notifyDataSetChanged()
                    list.post {
                        if (!active) return@post
                        if (!older && !bridge && bottom) list.setSelection(adapter.count - 1)
                        else firstId?.let { anchor ->
                            val index = memory.history.rows.indexOfFirst { it.localId == anchor }
                            if (index >= 0) list.setSelectionFromTop(index, offset)
                        }
                        markViewed()
                        if (gapBefore != null) loadPage()
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

    private fun send() {
        if (memory.pending || memory.uncertain != null || contactCta(memory.contact) != ContactCta.Chat) { render(); return }
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

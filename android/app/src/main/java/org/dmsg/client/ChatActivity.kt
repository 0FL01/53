package org.dmsg.client

import android.content.Intent
import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.animation.ValueAnimator
import android.content.res.Resources
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.MotionEvent
import android.widget.AbsListView
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.appcompat.app.AlertDialog
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import uniffi.dmsg_core.DeleteScope
import uniffi.dmsg_core.HistoryMessage

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
    internal val voice = VoiceUiState()
    internal var audio: VoiceNoteAudio? = null
    internal var uncertainVoice: VoiceSendOutcome.Uncertain? = null
    internal var voiceAttempt: VoiceSendAttempt? = null
    internal fun audio(context: Context): VoiceNoteAudio = audio ?: VoiceNoteAudio(context).also { engine ->
        audio = engine
        engine.onMeter = { samples, rms -> voice.samples = samples; voice.rms = rms; voice.changed() }
        engine.onNote = { bytes, samples, bars, paused ->
            if (voice.mode == VoiceMode.Idle) bytes.fill(0)
            else if (paused && voice.mode == VoiceMode.Finishing) { bytes.fill(0); engine.finishRecording() }
            else {
                voice.preview(bytes, samples, bars)
                if (paused) { voice.mode = VoiceMode.Paused; voice.changed() }
            }
        }
        engine.onError = {
            voice.sendOnFinish = false
            if (voice.bytes == null) voice.discard() else { voice.mode = VoiceMode.Preview; voice.changed() }
            action = { it.getString(R.string.voice_error) }; voice.changed()
        }
        engine.onInterrupted = { voice.background(); voice.changed() }
    }
    override fun onCleared() { voice.observer = null; audio?.close(); voice.discard(); super.onCleared() }
}

class ChatActivity : DmsgActivity() {
    private lateinit var memory: ChatMemory
    private lateinit var list: ListView
    private lateinit var composer: EditText
    private lateinit var info: TextView
    private lateinit var adapter: HistoryAdapter
    private val pageGuard = UiGuard()
    private var pageRequest = 0L
    private var replyRefreshPending = false
    private var replyRefreshRequest = 0L
    private lateinit var replyMarkdown: MessageMarkdown
    private data class QuoteJump(val targetLocalId: Long, val token: Long, val lifecycle: Long,
        var verified: Boolean = false, var refreshed: Boolean = false, var settling: Boolean = false)
    @Volatile private var navigationGeneration = 0L
    @Volatile private var quoteJump: QuoteJump? = null
    private var touchY = 0f
    @Volatile private var active = false
    @Volatile private var lifecycleStamp = 0L
    private var readPending = false
    private var pageError = ""
    private var prompt: AlertDialog? = null
    private val transferOwner = Any()
    private var lastVoiceMode = VoiceMode.Idle
    private var playerRequest = 0L
    private var micX = 0f
    private var micY = 0f
    private val microphonePermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        // Grant is never a continuation of the gesture that opened the dialog.
        memory.action = { it.getString(if (granted) R.string.voice_ready else R.string.voice_permission) }; render()
    }
    private val id get() = intent.getStringExtra("peer").orEmpty()
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            if (!active) return
            if (quoteJump == null && !pageGuard.pending && !memory.pending) {
                when {
                    memory.uncertain != null -> resolveUncertain()
                    memory.uncertainAction != null -> resolveAction()
                    memory.uncertainVoice != null -> resolveVoice()
                    else -> loadPage()
                }
            }
            refreshReplyTargets()
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
        replyMarkdown = MessageMarkdown(this)
        composer.setText(memory.draft)
        composer.isSaveEnabled = false
        composer.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {
                memory.draft = s?.toString().orEmpty()
                if (active) { updateComposer(); renderVoice() }
            }
            override fun afterTextChanged(s: android.text.Editable?) {}
        })
        adapter = HistoryAdapter(this, memory.history.rows,
            { canEditMessage(it, memory.contact) }, ::startEdit, ::confirmDelete, ::messageMenu, ::bindVoice,
            ::startReply, ::openQuote, { intent.getStringExtra("alias") ?: id })
        list.adapter = adapter
        list.setOnScrollListener(object : AbsListView.OnScrollListener {
            override fun onScrollStateChanged(view: AbsListView?, state: Int) {
                if (state == AbsListView.OnScrollListener.SCROLL_STATE_TOUCH_SCROLL) cancelQuoteJump()
                if (state == AbsListView.OnScrollListener.SCROLL_STATE_IDLE) markViewed()
            }
            override fun onScroll(view: AbsListView?, first: Int, visible: Int, total: Int) {
                if (active && visible > 0) {
                    markViewed()
                    if (quoteJump == null && first <= 2 && memory.history.initialized && pageError.isEmpty()) loadPage(true)
                }
            }
        })
        list.setOnTouchListener { _, event ->
            if (event.actionMasked == MotionEvent.ACTION_DOWN) touchY = event.y
            if (event.actionMasked == MotionEvent.ACTION_MOVE && kotlin.math.abs(event.y - touchY) > android.view.ViewConfiguration.get(this).scaledTouchSlop) cancelQuoteJump()
            false
        }
        findViewById<Button>(R.id.btn_send).setOnClickListener { send() }
        findViewById<Button>(R.id.btn_save_edit).setOnClickListener { saveEdit() }
        findViewById<Button>(R.id.btn_cancel_edit).setOnClickListener {
            if (!memory.pending && memory.uncertain == null && memory.uncertainAction == null && memory.uncertainVoice == null) {
                memory.composition.cancel(); memory.rebaseEdit = false; render()
            }
        }
        findViewById<Button>(R.id.btn_reply_cancel).setOnClickListener {
            if (canCancelReply()) { memory.outgoing.reply = null; render() }
        }
        findViewById<Button>(R.id.btn_retry).setOnClickListener { retry() }
        findViewById<Button>(R.id.btn_history_retry).setOnClickListener {
            refreshReceived()
        }
        findViewById<Button>(R.id.btn_contact).setOnClickListener {
            startActivity(Intent(this, ProfileActivity::class.java).putExtra("peer", id))
        }
        setupVoice()
    }

    override fun onResume() {
        super.onResume()
        active = true
        memory.uncertainVoice = memory.uncertainVoice ?: TextSendCoordinator.pendingVoiceFor(id)
        memory.uncertainVoice?.let {
            memory.outgoing.restorePendingReply(it.attempt.replyToLocalId)
            if (!memory.voice.busy) {
                memory.voiceAttempt = it.attempt
                memory.voice.restorePending(it.attempt)
            }
        }
        memory.voice.observer = {
            if (active) {
                if (lastVoiceMode != memory.voice.mode) { lastVoiceMode = memory.voice.mode; render() } else renderVoice()
                if (memory.voice.mode == VoiceMode.Preview && memory.voice.sendOnFinish) {
                    memory.voice.sendOnFinish = false; queueVoice()
                }
            }
        }
        memory.audio(applicationContext).onPlayback = { playback ->
            if (active) {
                playback.key?.let { key -> visibleVoice(key)?.playback(playback) }
                renderVoice()
            }
        }
        VoiceTransferCoordinator.foreground(this, transferOwner, true, ::transferChanged)
        if (memory.uncertain == null) TextSendCoordinator.pendingFor(id)?.let {
            memory.uncertain = it
            memory.outgoing.restoreSubmitted(it.text, it.replyToLocalId)
        }
        if (memory.uncertainAction == null) TextSendCoordinator.pendingActionFor(id)?.let {
            memory.uncertainAction = it
            memory.composition.restore(it)
        }
        adapter.notifyDataSetChanged()
        restoreAnchor(memory.anchor)
        render()
        refreshReplyTargets()
        handler.post(ticker)
    }

    override fun onPause() {
        memory.draft = composer.text.toString()
        memory.anchor = captureAnchor()
        active = false; lifecycleStamp++
        cancelQuoteJump()
        replyRefreshRequest++; replyRefreshPending = false
        playerRequest++
        memory.voice.observer = null
        memory.voice.background()
        memory.audio?.onPlayback = null
        memory.audio?.background()
        VoiceTransferCoordinator.foreground(this, transferOwner, false)
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
        // Counter + touch targets must fit above IME even with a scrollable notice.
        // Keep font scaling/minHeight; only reduce spare vertical padding.
        val controlPadding = NativeUi.dp(this, if (compact) 4 else 12)
        listOf(R.id.composer, R.id.btn_save_edit, R.id.btn_cancel_edit).forEach { id ->
            findViewById<View>(id)?.let { it.setPadding(it.paddingLeft, controlPadding, it.paddingRight, controlPadding) }
        }
        val rowPadding = NativeUi.dp(this, if (compact) 0 else 8)
        findViewById<View>(R.id.chat_composer_row)?.let { it.setPadding(it.paddingLeft, rowPadding, it.paddingRight, rowPadding) }
        if (compact) findViewById<android.widget.ScrollView>(R.id.chat_footer)?.let { footer ->
            // Insets can resize an already focused editor without another focus request.
            // Scroll, rather than move focus or hide the edit banner/notice.
            footer.post { footer.scrollTo(0, footer.getChildAt(0).height) }
        }
    }

    private fun render() {
        if (!active) return
        val visible = memory.history.visibleRows
        if (!sameHistoryRows(adapter.visibleRows, visible)) {
            if (adapter.replaceVoicePayloads(visible)) {
                visible.filter { it.kind == uniffi.dmsg_core.MessageKind.VOICE }.forEach { row -> visibleVoice(VoiceKey(row))?.let { bindVoice(it, row) } }
            } else {
                val anchor = captureAnchor()
                adapter.notifyDataSetChanged()
                if (quoteJump == null) restoreAnchor(anchor)
            }
        }
        if (composer.text.toString() != memory.draft) composer.setText(memory.draft)
        val allowed = contactCta(memory.contact) == ContactCta.Chat
        val editing = memory.composition.edit
        val unresolved = memory.uncertain != null || memory.uncertainAction != null || memory.uncertainVoice != null
        findViewById<TextView>(R.id.peer).text = intent.getStringExtra("alias") ?: id
        findViewById<Button>(R.id.btn_contact).contentDescription = getString(if (allowed) R.string.contact_card else R.string.check_contact)
        findViewById<View>(R.id.edit_banner).visibility = if (editing == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_save_edit).visibility = if (editing == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_cancel_edit).visibility = if (editing == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_cancel_edit).isEnabled = !memory.pending && !unresolved
        renderReply()
        findViewById<Button>(R.id.btn_retry).isEnabled = allowed && !memory.pending && quoteJump == null
        composer.isEnabled = (allowed || editing != null) && !memory.pending && !unresolved && !memory.voice.busy
        findViewById<Button>(R.id.btn_history_retry).isEnabled = !pageGuard.pending && !memory.pending && quoteJump == null
        val reply = displayedReply()
        info.text = listOf(if (allowed) "" else trustLabel(resources, memory.contact), memory.action(resources), pageError,
            if (reply != null && !reply.available) getString(R.string.reply_target_unavailable) else "").filter { it.isNotEmpty() }.joinToString("\n")
        findViewById<View>(R.id.chat_notice).visibility = if (info.text.isEmpty()) View.GONE else View.VISIBLE
        info.setBackgroundResource(if (memory.contact?.identityMismatch == true || memory.contact?.state == "blocked") R.color.error_surface
            else if (allowed && pageError.isEmpty()) R.color.surface else R.color.warning_surface)
        updateComposer()
        renderVoice()
    }

    /** Keystrokes update only composer controls; history refresh belongs to render/loadPage. */
    private fun updateComposer() {
        val count = MessageTextPolicy.count(memory.draft)
        val withinLimit = count != null && count <= MessageTextPolicy.MAX_SCALARS
        val allowed = contactCta(memory.contact) == ContactCta.Chat
        val editing = memory.composition.edit
        val unresolved = memory.uncertain != null || memory.uncertainAction != null || memory.uncertainVoice != null
        // Hidden Send retains its old readiness for an empty normal draft.
        findViewById<Button>(R.id.btn_send).isEnabled = quoteJump == null && allowed && editing == null && !memory.pending && !unresolved && !memory.voice.busy && withinLimit && replyReady(memory.outgoing.reply)
        findViewById<Button>(R.id.btn_save_edit).isEnabled = editing != null && !editing.unavailable && !memory.rebaseEdit && allowed && !memory.pending && !unresolved && !pageGuard.pending && withinLimit && count != 0
        findViewById<TextView>(R.id.composer_counter).text = when {
            count == null -> getString(R.string.message_counter_invalid, MessageTextPolicy.MAX_SCALARS)
            count > MessageTextPolicy.MAX_SCALARS -> getString(R.string.message_counter_overflow, count, MessageTextPolicy.MAX_SCALARS)
            else -> getString(R.string.message_counter, count, MessageTextPolicy.MAX_SCALARS)
        }
    }

    private fun validateComposerText(text: String): Boolean {
        if (MessageTextPolicy.isValid(text)) return true
        val messageRes = if (MessageTextPolicy.count(text) == null) R.string.message_unicode_invalid else R.string.message_bounds
        memory.action = { it.getString(messageRes) }
        render()
        return false
    }

    private fun captureAnchor(): HistoryAnchor? = historyAnchor(adapter.visibleRows, list.firstVisiblePosition,
        list.getChildAt(0)?.top ?: 0, adapter.count > 0 && list.lastVisiblePosition >= adapter.count - 2)
        ?.also { memory.anchor = it } ?: memory.anchor
    private fun restoreAnchor(anchor: HistoryAnchor?) {
        if (anchor == null || quoteJump != null) return
        anchorPosition(adapter.visibleRows, anchor, memory.history.rows)?.let {
            if (anchor.followBottom) list.setSelection(it) else list.setSelectionFromTop(it, anchor.offset)
        }
    }

    private fun loadPage(older: Boolean = false, jumpToken: Long? = null) {
        if (!active || memory.pending) return
        if (quoteJump != null && jumpToken != quoteJump?.token) return
        val bridge = memory.history.gapBefore != null
        val continuing = !bridge && memory.history.hiddenBefore != null
        val before = memory.history.gapBefore ?: memory.history.hiddenBefore ?: if (older) memory.history.nextBefore ?: return else null
        val stamp = pageGuard.begin() ?: return
        val request = ++pageRequest
        val lifecycle = lifecycleStamp
        val navigation = navigationGeneration
        val anchor = captureAnchor()
        val bottom = quoteJump == null && (!memory.history.initialized || adapter.count == 0 || anchor?.followBottom == true)
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
                if (!finishPage(stamp, request) || !active || lifecycle != lifecycleStamp || navigation != navigationGeneration) return@runOnUiThread
                result.fold({ (contact, pages, refresh) ->
                    val contactChanged = memory.contact != contact
                    memory.contact = contact
                    pageError = ""
                    if (normalRefresh && jumpToken != null) currentJump(jumpToken)?.refreshed = true
                    pages.forEachIndexed { index, page ->
                        if ((index == 0 && (older || continuing) && !bridge) || (index > 0 && memory.history.gapBefore == null)) memory.history.older(page)
                        else memory.history.latest(page, if (index == 0) refresh.first else emptyList(), if (index == 0) refresh.second else null)
                    }
                    refresh.first.forEach(memory.history::replace)
                    stopDeletedVoice()
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
                    // A no-op ticker must not destroy the system text-selection action mode.
                    val visible = memory.history.visibleRows
                    val changed = !sameHistoryRows(adapter.visibleRows, visible)
                    val voiceOnly = changed && !contactChanged && adapter.replaceVoicePayloads(visible)
                    if (voiceOnly) visible.filter { it.kind == uniffi.dmsg_core.MessageKind.VOICE }.forEach { row ->
                        visibleVoice(VoiceKey(row))?.let { bindVoice(it, row) }
                    }
                    val rebind = contactChanged || (changed && !voiceOnly)
                    if (rebind) adapter.notifyDataSetChanged()
                    list.post {
                        if (!active || lifecycle != lifecycleStamp || navigation != navigationGeneration) return@post
                        if (jumpToken != null) {
                            if (currentJump(jumpToken) != null) advanceQuoteJump(jumpToken)
                            return@post
                        }
                        if (rebind) {
                            if (bottom && (!older || continuing) && !bridge) list.setSelection(adapter.count - 1)
                            else restoreAnchor(anchor)
                        }
                        markViewed()
                        if (memory.history.gapBefore != null) loadPage()
                    }
                }, {
                    pageError = humanError(resources, it)
                    if (jumpToken != null) finishQuoteJump(jumpToken, false)
                })
                render()
                refreshReplyTargets()
            }
        }
    }

    private fun markViewed() {
        if (!active || quoteJump != null || readPending || pageGuard.pending || list.childCount == 0) return
        // Use only intersecting rendered rows, never a page's newest cursor or fetch report.
        val visible = (0 until list.childCount).filter {
            val child = list.getChildAt(it)
            child.bottom > list.paddingTop && child.top < list.height - list.paddingBottom
        }.mapNotNull { list.getChildAt(it).tag as? Long }
        val anchor = memory.history.viewedAnchor(visible) ?: return
        if (anchor <= memory.readThrough) return
        val stamp = lifecycleStamp
        val navigation = navigationGeneration
        readPending = true
        Core.dispatch {
            // A paused activity must not start a fresh read mutation.
            val result = if (active && stamp == lifecycleStamp && navigation == navigationGeneration && quoteJump == null)
                runCatching {
                    synchronized(Core.storeLock) {
                        if (active && stamp == lifecycleStamp && navigation == navigationGeneration && quoteJump == null)
                            Core.facade(applicationContext).markRead(id, anchor) else null
                    }
                } else null
            runOnUiThread {
                if (!active || stamp != lifecycleStamp) return@runOnUiThread
                readPending = false
                result?.onSuccess { if (it != null) memory.readThrough = maxOf(memory.readThrough, it) }
            }
        }
    }

    private fun finishPage(stamp: Long, request: Long) = request == pageRequest && pageGuard.finish(stamp)
    private fun replyReady(reply: ReplyDraft?) = reply == null || reply.available
    private fun displayedReply(): ReplyDraft? = if (memory.composition.edit != null) null
        else if (memory.voice.busy) memory.voice.replyDraft else memory.outgoing.reply
    private fun canCancelReply() = active && memory.composition.edit == null && !memory.voice.busy && !memory.pending &&
        memory.uncertain == null && memory.uncertainAction == null && memory.uncertainVoice == null

    private fun renderReply() {
        val reply = displayedReply()
        findViewById<View>(R.id.reply_banner).visibility = if (reply == null) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_reply_cancel).isEnabled = canCancelReply()
        if (reply == null) return
        findViewById<TextView>(R.id.reply_title).text = getString(R.string.reply_banner_title,
            replyAuthor(this, reply.target?.direction, intent.getStringExtra("alias") ?: id))
        findViewById<TextView>(R.id.reply_preview).text = if (reply.available) replyPreview(this, replyInfoForTarget(reply.target!!), replyMarkdown)
            else getString(R.string.reply_message_unavailable)
    }

    private fun startReply(localId: Long) {
        // Selecting a local draft must not be silently lost to the read-only ticker.
        if (quoteJump != null || !canCancelReply()) return
        val row = memory.history.rows.firstOrNull { it.localId == localId && it.contactId == id && canReplyMessage(it) } ?: return
        memory.outgoing.reply = ReplyDraft(localId, row)
        render(); refreshReplyTargets()
        composer.requestFocus(); composer.setSelection(composer.text.length)
    }

    private fun refreshReplyTargets() {
        if (!active || replyRefreshPending) return
        val drafts = listOfNotNull(memory.outgoing.reply, memory.voice.replyDraft.takeIf { memory.voice.busy })
        if (drafts.isEmpty()) return
        replyRefreshPending = true
        val request = ++replyRefreshRequest
        val lifecycle = lifecycleStamp
        Core.dispatch {
            val results = drafts.map { draft -> draft to runCatching {
                val row = Core.facade(applicationContext).historyMessage(id, draft.targetLocalId)
                if (row.contactId != id) throw DmsgError(R.string.error_message_unavailable, ErrorKind.MessageUnavailable)
                row
            } }
            runOnUiThread {
                if (!active || lifecycle != lifecycleStamp || request != replyRefreshRequest) return@runOnUiThread
                replyRefreshPending = false
                results.forEach { (captured, result) ->
                    applyReplyRefresh(memory.outgoing.reply, captured, result)
                    if (memory.voice.busy) applyReplyRefresh(memory.voice.replyDraft, captured, result)
                }
                render()
                // A reselect while the read was pending needs its own canonical read.
                if (listOfNotNull(memory.outgoing.reply, memory.voice.replyDraft.takeIf { memory.voice.busy }).any { current -> drafts.none { it === current } }) refreshReplyTargets()
            }
        }
    }

    private fun currentJump(token: Long) = quoteJump?.takeIf { active && it.token == token && it.lifecycle == lifecycleStamp && navigationGeneration == token }
    private fun cancelQuoteJump() {
        if (quoteJump == null) return
        navigationGeneration++; quoteJump = null
        pageGuard.stop()
    }
    private fun finishQuoteJump(token: Long, success: Boolean) {
        if (currentJump(token) == null) return
        quoteJump = null
        if (success) { memory.anchor = captureAnchor()?.copy(followBottom = false); markViewed() }
        else { memory.action = { it.getString(R.string.reply_message_unavailable) }; render() }
    }

    private fun openQuote(localId: Long) {
        if (!active || memory.pending) return
        cancelQuoteJump()
        // Invalidate an ordinary refresh and all of its already-posted viewport work.
        pageGuard.stop()
        val token = ++navigationGeneration
        quoteJump = QuoteJump(localId, token, lifecycleStamp)
        render()
        val stamp = pageGuard.begin() ?: return
        val request = ++pageRequest
        Core.dispatch {
            val result = runCatching { Core.facade(applicationContext).historyMessage(id, localId) }
            runOnUiThread {
                if (!finishPage(stamp, request)) return@runOnUiThread
                val jump = currentJump(token) ?: return@runOnUiThread
                result.fold({ row ->
                    if (row.contactId != id || !canReplyMessage(row)) { finishQuoteJump(token, false); return@fold }
                    // Exact lookup proves availability; pagination alone adds rows to the window.
                    memory.history.replace(row)
                    jump.verified = true
                    render(); advanceQuoteJump(token)
                }, { finishQuoteJump(token, false) })
            }
        }
    }

    private fun advanceQuoteJump(token: Long) {
        val jump = currentJump(token) ?: return
        if (!jump.verified || jump.settling || pageGuard.pending || memory.pending) return
        val position = adapter.visibleRows.indexOfFirst { it.localId == jump.targetLocalId }
        if (position >= 0) {
            jump.settling = true
            settleQuoteJump(token, position)
            return
        }
        when {
            memory.history.gapBefore != null || memory.history.hiddenBefore != null -> loadPage(jumpToken = token)
            !jump.refreshed -> loadPage(jumpToken = token)
            memory.history.nextBefore != null -> loadPage(older = true, jumpToken = token)
            memory.history.pagingExhausted && !pageGuard.pending -> finishQuoteJump(token, false)
        }
    }

    private fun settleQuoteJump(token: Long, position: Int, turn: Int = 0, intersected: Boolean = false) {
        val jump = currentJump(token) ?: return
        if (turn == 0) list.setSelectionFromTop(position, list.paddingTop)
        list.postOnAnimation {
            if (currentJump(token) !== jump) return@postOnAnimation
            val actual = (0 until list.childCount).any {
                val child = list.getChildAt(it)
                child.tag == jump.targetLocalId && child.bottom > list.paddingTop && child.top < list.height - list.paddingBottom
            } && !list.isLayoutRequested
            if (actual && intersected) finishQuoteJump(token, true)
            else if (turn < 8) settleQuoteJump(token, position, turn + 1, actual)
            else finishQuoteJump(token, false)
        }
    }

    private fun actionsReady() = active && quoteJump == null && !memory.pending && !pageGuard.pending && memory.uncertain == null && memory.uncertainAction == null && memory.uncertainVoice == null
    private fun ownRow(localId: Long) = memory.history.rows.find { it.localId == localId && canHideMessage(it) }

    private fun messageMenu(localId: Long) {
        if (!active || quoteJump != null || memory.pending || memory.uncertain != null || memory.uncertainAction != null || memory.uncertainVoice != null) return
        val row = memory.history.rows.firstOrNull { it.localId == localId && messageVisible(it) } ?: return
        val editable = actionsReady() && canEditMessage(row, memory.contact) && memory.composition.edit == null && !memory.voice.busy
        val replyable = canReplyMessage(row) && canCancelReply()
        val choices = listOfNotNull(if (replyable) R.string.reply_whole_message else null,
            if (editable) R.string.edit_whole_message else null, if (actionsReady() && canHideMessage(row)) R.string.delete_whole_message else null)
        if (choices.isEmpty()) return
        val labels = choices.map { getString(it) }.toTypedArray()
        prompt = AlertDialog.Builder(this).setTitle(R.string.message_actions).setItems(labels) { _, which ->
            when (choices[which]) {
                R.string.reply_whole_message -> startReply(localId)
                R.string.edit_whole_message -> startEdit(localId)
                R.string.delete_whole_message -> confirmDelete(localId)
            }
        }.setNegativeButton(R.string.cancel, null).show()
    }

    private fun startEdit(localId: Long) {
        if (!actionsReady()) return
        val row = ownRow(localId) ?: return
        if (memory.voice.busy || !canEditMessage(row, memory.contact) || !memory.composition.start(row)) return
        memory.rebaseEdit = false
        render()
        composer.requestFocus()
        composer.setSelection(composer.text.length)
    }

    private fun saveEdit() {
        if (!actionsReady() || memory.rebaseEdit) return
        val draft = memory.composition.edit ?: return
        val row = ownRow(draft.localId)
        if (draft.unavailable || row == null || !canEditMessage(row, memory.contact)) {
            memory.composition.unavailable()
            memory.action = { it.getString(R.string.error_message_unavailable) }; render(); return
        }
        memory.draft = composer.text.toString()
        if (!validateComposerText(draft.text)) return
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
                if (command is MessageActionCommand.Delete) {
                    val key = VoiceKey(outcome.row)
                    VoiceTransferCoordinator.deleted(key)
                    if (memory.audio?.playback?.key == key) { playerRequest++; memory.audio?.stopPlayer() }
                }
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
        val request = ++pageRequest
        Core.dispatch {
            val outcome = try { TextSendCoordinator.reconcileAction(Core.facade(applicationContext), attempt) }
                catch (_: Exception) { MessageActionOutcome.Uncertain(attempt) }
            runOnUiThread {
                val released = finishPage(stamp, request)
                if (memory.uncertainAction != attempt) return@runOnUiThread
                completeAction(attempt.command, attempt.before.localId, outcome)
                if (!active || !released) return@runOnUiThread
                render()
                if (memory.uncertainAction == null) loadPage()
            }
        }
    }

    private fun send() {
        if (quoteJump != null || memory.voice.busy || memory.composition.edit != null || memory.pending || memory.uncertain != null || memory.uncertainAction != null || memory.uncertainVoice != null || contactCta(memory.contact) != ContactCta.Chat || !replyReady(memory.outgoing.reply)) { render(); return }
        val text = composer.text.toString()
        if (!validateComposerText(text)) return
        memory.draft = text
        memory.outgoing.begin() ?: return
        val replyToLocalId = memory.outgoing.submittedReplyLocalId
        memory.pending = true
        memory.action = { it.getString(R.string.saving_sending) }
        render()
        Core.dispatch {
            val outcome = try { TextSendCoordinator.send(Core.facade(applicationContext), id, text, replyToLocalId) }
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
        val request = ++pageRequest
        Core.dispatch {
            val outcome = try { TextSendCoordinator.reconcile(Core.facade(applicationContext), id, uncertain) }
                catch (_: Exception) { uncertain }
            val status = (outcome as? TextSendOutcome.Saved)?.let { runCatching { Core.facade(applicationContext).messageStatus(it.messageId) }.getOrNull() }
            runOnUiThread {
                val released = finishPage(stamp, request)
                if (memory.uncertain != uncertain) return@runOnUiThread
                memory.outgoing.restoreSubmitted(uncertain.text, uncertain.replyToLocalId)
                completeSend(outcome, status)
                if (!active || !released) return@runOnUiThread
                render()
                if (memory.uncertain == null) loadPage()
            }
        }
    }

    private fun retry() {
        if (quoteJump != null || memory.pending || contactCta(memory.contact) != ContactCta.Chat) return
        memory.pending = true; memory.action = { it.getString(R.string.retrying_saved_queue) }; render()
        VoiceTransferCoordinator.wake()
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
        if (quoteJump != null || memory.pending || pageGuard.pending) return
        memory.pending = true; memory.action = { it.getString(R.string.refreshing_dns) }; render()
        Core.dispatch {
            val result = runCatching { DmsgService.check(Core.facade(applicationContext)) }
            VoiceTransferCoordinator.wake()
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

    private fun setupVoice() {
        val mic = findViewById<VoiceMicView>(R.id.btn_mic)
        mic.setOnClickListener { startVoice(true) } // Accessibility/keyboard equivalent of lock.
        mic.setOnTouchListener { _, event ->
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> { micX = event.rawX; micY = event.rawY; startVoice(false); true }
                MotionEvent.ACTION_MOVE -> {
                    when (memory.voice.move(event.rawX - micX, event.rawY - micY, NativeUi.dp(this, 64).toFloat())) {
                        VoiceGesture.Cancel -> discardVoice()
                        VoiceGesture.Lock -> renderVoice()
                        VoiceGesture.None -> Unit
                    }; true
                }
                MotionEvent.ACTION_UP -> { if (memory.voice.mode == VoiceMode.Holding) finishVoice(true); true }
                MotionEvent.ACTION_CANCEL -> { if (memory.voice.mode == VoiceMode.Holding) finishVoice(false); true }
                else -> false
            }
        }
        findViewById<Button>(R.id.voice_discard).setOnClickListener { discardVoice() }
        findViewById<Button>(R.id.voice_pause).setOnClickListener {
            when (memory.voice.mode) {
                VoiceMode.Locked -> {
                    memory.voice.mode = VoiceMode.Pausing; memory.voice.changed(); memory.audio?.pauseRecording()
                }
                VoiceMode.Paused -> {
                    if (memory.audio(applicationContext).start(true)) {
                        memory.voice.bytes?.fill(0); memory.voice.bytes = null
                        memory.voice.mode = VoiceMode.Locked; memory.voice.changed()
                    }
                }
                else -> Unit
            }
        }
        findViewById<Button>(R.id.voice_preview).setOnClickListener {
            when (memory.voice.mode) {
                VoiceMode.Locked -> finishVoice(false)
                VoiceMode.Paused, VoiceMode.Preview -> previewVoice()
                else -> Unit
            }
        }
        findViewById<Button>(R.id.voice_send).setOnClickListener {
            when (memory.voice.mode) {
                VoiceMode.Locked, VoiceMode.Paused -> finishVoice(true)
                VoiceMode.Preview -> queueVoice()
                else -> Unit
            }
        }
        findViewById<VoiceWaveformView>(R.id.voice_waveform).onSeek = { sample ->
            memory.voice.bytes?.let { memory.audio?.seek(sample, it, preview = true) }
        }
    }
    private fun startVoice(locked: Boolean) {
        if (!actionsReady() || quoteJump != null || memory.composition.edit != null || memory.draft.isNotEmpty() || contactCta(memory.contact) != ContactCta.Chat || memory.voice.busy || !replyReady(memory.outgoing.reply)) return
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            memory.action = { it.getString(R.string.voice_permission) }; render(); microphonePermission.launch(Manifest.permission.RECORD_AUDIO); return
        }
        if (!memory.voice.begin(locked, memory.outgoing.reply?.targetLocalId)) return
        memory.voiceAttempt = null
        memory.outgoing.reply?.target?.let { memory.voice.replyDraft?.refresh(it) }
        refreshReplyTargets()
        playerRequest++
        if (!memory.audio(applicationContext).start()) {
            memory.voice.discard(); memory.action = { it.getString(R.string.voice_error) }; render()
        }
    }
    private fun finishVoice(send: Boolean) {
        memory.voice.finish(send); memory.audio?.finishRecording()
    }
    private fun discardVoice() {
        if (memory.pending || memory.uncertainVoice != null) return
        playerRequest++; memory.audio?.cancelRecording(); memory.audio?.stopPlayer()
        memory.voice.discard(); memory.voiceAttempt = null; render()
    }
    private fun previewVoice() {
        val bytes = memory.voice.bytes ?: return
        val audio = memory.audio(applicationContext)
        if (audio.playback.preview && audio.playback.playing) audio.stopPlayer()
        else audio.play(bytes, preview = true, from = if (audio.playback.preview && audio.playback.sample < memory.voice.samples) audio.playback.sample else 0)
    }
    private fun transition(view: View, visible: Boolean) {
        val visibility = if (visible) View.VISIBLE else View.GONE
        if (view.visibility == visibility) return
        view.animate().cancel(); view.visibility = visibility
        if (visible && ValueAnimator.areAnimatorsEnabled()) {
            view.alpha = 0f; view.scaleX = .8f; view.scaleY = .8f
            view.animate().alpha(1f).scaleX(1f).scaleY(1f).setDuration(200).start()
        } else { view.alpha = 1f; view.scaleX = 1f; view.scaleY = 1f }
    }
    private fun renderVoice() {
        if (!active) return
        val voice = memory.voice
        val mic = findViewById<VoiceMicView>(R.id.btn_mic)
        val editing = memory.composition.edit != null
        transition(findViewById(R.id.btn_send), !editing && !voice.busy && memory.draft.isNotEmpty())
        transition(mic, !editing && (memory.draft.isEmpty() || voice.busy))
        mic.isEnabled = quoteJump == null && contactCta(memory.contact) == ContactCta.Chat && !memory.pending && memory.uncertain == null && memory.uncertainAction == null && memory.uncertainVoice == null && replyReady(memory.outgoing.reply)
        mic.meter(voice.rms, voice.mode in setOf(VoiceMode.Holding, VoiceMode.Locked))
        findViewById<View>(R.id.voice_panel).visibility = if (voice.busy) View.VISIBLE else View.GONE
        findViewById<TextView>(R.id.voice_timer).text = voiceTime(voice.samples)
        findViewById<TextView>(R.id.voice_hint).setText(when (voice.mode) {
            VoiceMode.Holding -> R.string.voice_hold_hint
            VoiceMode.Locked -> R.string.voice_locked
            VoiceMode.Queueing -> R.string.voice_saving
            else -> R.string.voice_unsent
        })
        val waveform = findViewById<VoiceWaveformView>(R.id.voice_waveform)
        waveform.visibility = if (voice.bytes == null) View.GONE else View.VISIBLE
        waveform.bars = voice.waveform; waveform.totalSamples = voice.samples
        val playback = memory.audio?.playback
        waveform.sample = if (playback?.preview == true) playback.sample else 0
        waveform.isEnabled = voice.bytes != null && voice.mode != VoiceMode.Queueing
        val locked = voice.mode in setOf(VoiceMode.Locked, VoiceMode.Paused)
        findViewById<Button>(R.id.voice_pause).apply {
            visibility = if (locked) View.VISIBLE else View.GONE
            setText(if (voice.mode == VoiceMode.Paused) R.string.voice_resume else R.string.voice_pause)
        }
        findViewById<Button>(R.id.voice_preview).apply {
            visibility = if (voice.mode in setOf(VoiceMode.Locked, VoiceMode.Paused, VoiceMode.Preview)) View.VISIBLE else View.GONE
            setText(if (playback?.preview == true && playback.playing) R.string.voice_pause else R.string.voice_preview)
        }
        val mutable = !memory.pending && memory.uncertainVoice == null && voice.mode !in setOf(VoiceMode.Finishing, VoiceMode.Pausing)
        findViewById<Button>(R.id.voice_discard).isEnabled = mutable
        findViewById<Button>(R.id.voice_send).isEnabled = mutable && voice.mode in setOf(VoiceMode.Locked, VoiceMode.Paused, VoiceMode.Preview) && contactCta(memory.contact) == ContactCta.Chat && replyReady(voice.replyDraft)
    }
    private fun queueVoice() {
        if (!active || quoteJump != null || memory.pending || memory.uncertain != null || memory.uncertainAction != null || memory.uncertainVoice != null || contactCta(memory.contact) != ContactCta.Chat || !replyReady(memory.voice.replyDraft)) { render(); return }
        val bytes = memory.voice.bytes?.copyOf() ?: return
        val attempt = memory.voiceAttempt ?: VoiceSendAttempt(id, replyToLocalId = memory.voice.replyToLocalId).also { memory.voiceAttempt = it }
        memory.audio?.stopPlayer()
        memory.pending = true; memory.voice.mode = VoiceMode.Queueing
        memory.action = { it.getString(R.string.voice_saving) }; render()
        Core.dispatch {
            val outcome = try { TextSendCoordinator.queueVoice(Core.facade(applicationContext), attempt, bytes) }
                catch (e: Exception) { VoiceSendOutcome.NotSaved(e) }
                finally { bytes.fill(0) }
            runOnUiThread { completeVoice(outcome, attempt); if (active) { render(); loadPage() } }
        }
    }
    private fun completeVoice(outcome: VoiceSendOutcome, attempt: VoiceSendAttempt) {
        memory.pending = false
        val ownsVoiceDraft = memory.voiceAttempt == attempt
        when (outcome) {
            is VoiceSendOutcome.Saved -> {
                memory.outgoing.consumeReply(attempt.replyToLocalId)
                memory.uncertainVoice = null
                if (ownsVoiceDraft) { memory.voiceAttempt = null; memory.voice.discard() }
                memory.action = { it.getString(R.string.voice_saved) }; VoiceTransferCoordinator.wake()
            }
            is VoiceSendOutcome.NotSaved -> {
                memory.uncertainVoice = null
                if (ownsVoiceDraft) memory.voice.mode = if (memory.voice.bytes == null) VoiceMode.Idle else VoiceMode.Preview
                val error = humanErrorRes(outcome.error)
                memory.action = { it.getString(R.string.message_not_sent, it.getString(error)) }
            }
            is VoiceSendOutcome.Uncertain -> {
                memory.uncertainVoice = outcome
                if (ownsVoiceDraft) memory.voice.mode = VoiceMode.Queueing
                memory.action = { it.getString(R.string.voice_uncertain) }
            }
        }
    }
    private fun resolveVoice() {
        val attempt = memory.uncertainVoice ?: return
        val stamp = pageGuard.begin() ?: return
        val request = ++pageRequest
        Core.dispatch {
            val outcome = try { TextSendCoordinator.reconcileVoice(Core.facade(applicationContext), attempt) }
                catch (_: Exception) { attempt }
            runOnUiThread {
                val released = finishPage(stamp, request)
                if (memory.uncertainVoice != attempt) return@runOnUiThread
                completeVoice(outcome, attempt.attempt)
                if (!active || !released) return@runOnUiThread
                render(); if (memory.uncertainVoice == null) loadPage()
            }
        }
    }
    private fun bindVoice(view: VoiceBubbleView, row: uniffi.dmsg_core.HistoryMessage) {
        view.bind(row, VoiceTransferCoordinator.state(VoiceKey(row)), memory.audio?.playback ?: VoicePlayback(), ::voiceAction, ::seekVoice)
    }
    private fun visibleVoice(key: VoiceKey): VoiceBubbleView? {
        if (memory.history.rows.none(key::matches)) return null
        for (i in 0 until list.childCount) voiceView(list.getChildAt(i), key)?.let { return it }
        return null
    }
    private fun voiceAction(row: uniffi.dmsg_core.HistoryMessage) {
        if (!active || memory.voice.mode in setOf(VoiceMode.Holding, VoiceMode.Locked, VoiceMode.Pausing, VoiceMode.Finishing)) return
        val key = VoiceKey(row)
        val current = memory.history.rows.firstOrNull(key::matches) ?: return
        if (current.voice?.downloaded != true) { VoiceTransferCoordinator.download(current); return }
        val playback = memory.audio?.playback
        if (playback?.key == key && playback.playing) memory.audio?.stopPlayer()
        else playStored(key, if (playback?.key == key && playback.sample < (current.voice?.sampleCount?.toInt() ?: 0)) playback.sample else 0)
    }
    private fun seekVoice(row: uniffi.dmsg_core.HistoryMessage, sample: Int) {
        val key = VoiceKey(row)
        if (!active || memory.history.rows.none(key::matches)) return
        if (memory.audio?.playback?.playing == true && memory.audio?.playback?.key == key) playStored(key, sample)
        else memory.audio?.seek(sample, byteArrayOf(), key)
    }
    private fun playStored(key: VoiceKey, sample: Int) {
        val request = ++playerRequest
        val stamp = lifecycleStamp
        Core.dispatch {
            val result = runCatching {
                val f = Core.facade(applicationContext)
                val row = f.historyMessage(key.contactId, key.localId)
                check(key.matches(row)); f.voiceData(key.contactId, key.localId)
            }
            runOnUiThread {
                result.fold({ bytes ->
                    if (active && stamp == lifecycleStamp && request == playerRequest && memory.history.rows.any(key::matches)) memory.audio(applicationContext).play(bytes, key, from = sample)
                    bytes.fill(0)
                }, { if (active && stamp == lifecycleStamp && request == playerRequest) { memory.action = { it.getString(R.string.voice_error) }; render() } })
            }
        }
    }
    private fun transferChanged(key: VoiceKey) {
        if (!active || memory.history.rows.none(key::matches)) return
        val row = memory.history.rows.first(key::matches)
        visibleVoice(key)?.let { bindVoice(it, row) }
        val state = VoiceTransferCoordinator.state(key)
        if (state.complete || state.errorRes != null) {
            val stamp = lifecycleStamp
            Core.dispatch {
                val result = runCatching { Core.facade(applicationContext).historyMessage(key.contactId, key.localId) }
                runOnUiThread {
                    if (!active || stamp != lifecycleStamp || memory.history.rows.none(key::matches)) return@runOnUiThread
                    result.onSuccess { updated ->
                        memory.history.replace(updated); stopDeletedVoice()
                        if (key.matches(updated) && adapter.replaceVoicePayload(updated)) visibleVoice(key)?.let { bindVoice(it, updated) } else render()
                    }
                }
            }
        }
    }
    private fun stopDeletedVoice() {
        memory.history.rows.filter { it.kind == uniffi.dmsg_core.MessageKind.VOICE && !messageVisible(it) }.forEach {
            val key = VoiceKey(it); VoiceTransferCoordinator.deleted(key)
            if (memory.audio?.playback?.key == key) { playerRequest++; memory.audio?.stopPlayer() }
        }
    }
}

package org.dmsg.client

import android.content.res.Resources
import androidx.annotation.StringRes
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.HistoryPage
import uniffi.dmsg_core.MessageDirection

/** No transport/debug-string interpretation. These are outcomes of actual worker calls. */
data class ConnectionUiState(
    val serviceEnabled: Boolean = false,
    val pollInFlight: Boolean = false,
    val lastSuccessAt: Long? = null,
    val lastFailure: ErrorKind? = null,
    val revision: Long = 0
)

internal class ConnectionFacts {
    private var sequence = 0L
    private val activePolls = mutableSetOf<Long>()
    private var value = ConnectionUiState()
    @Synchronized fun snapshot() = value
    @Synchronized fun enabled(enabled: Boolean) {
        if (value.serviceEnabled != enabled) activePolls.clear()
        value = value.copy(serviceEnabled = enabled, pollInFlight = activePolls.isNotEmpty())
    }
    @Synchronized fun begin(): Long {
        value = value.copy(pollInFlight = true)
        return (++sequence).also { activePolls.add(it) }
    }
    @Synchronized fun success(stamp: Long, at: Long) {
        if (stamp !in activePolls) return
        value = value.copy(lastSuccessAt = at, lastFailure = null)
    }
    @Synchronized fun finish(stamp: Long, at: Long, failure: ErrorKind?) {
        if (!activePolls.remove(stamp)) return
        value = value.copy(pollInFlight = activePolls.isNotEmpty(), lastSuccessAt = if (failure == null) at else value.lastSuccessAt,
            lastFailure = failure, revision = value.revision + 1)
    }
}

internal fun deliveryLabel(resources: Resources, state: DeliveryState?): String = resources.getString(deliveryLabelRes(state))
@StringRes internal fun deliveryLabelRes(state: DeliveryState?): Int = when (state) {
    DeliveryState.QUEUED -> R.string.delivery_queued
    DeliveryState.ACCEPTED -> R.string.delivery_accepted
    DeliveryState.DELIVERED -> R.string.delivery_delivered
    null -> R.string.delivery_unknown
}

internal enum class ContactCta { ScanKeys, Accept, VerifyChanged, Blocked, Chat, Loading }
internal fun contactCta(contact: Dialog?): ContactCta = when {
    contact == null -> ContactCta.Loading
    contact.state == "blocked" -> ContactCta.Blocked
    contact.identityMismatch -> ContactCta.VerifyChanged
    !contact.hasKeys -> ContactCta.ScanKeys
    contact.state in setOf("requested", "incoming") -> ContactCta.Accept
    contact.state in setOf("accepted", "accepted_server", "inviting") -> ContactCta.Chat
    else -> ContactCta.Loading
}
internal fun trustLabel(resources: Resources, contact: Dialog?): String = resources.getString(trustLabelRes(contact))
@StringRes internal fun trustLabelRes(contact: Dialog?): Int = when {
    contactCta(contact) == ContactCta.Accept && contact?.state == "incoming" -> R.string.trust_incoming
    contactCta(contact) == ContactCta.Chat && contact?.state == "accepted_server" -> R.string.trust_server_keys
    contactCta(contact) == ContactCta.Chat && contact?.state == "inviting" -> R.string.trust_inviting
    else -> when (contactCta(contact)) {
    ContactCta.VerifyChanged -> R.string.trust_changed
    ContactCta.Blocked -> R.string.trust_blocked
    ContactCta.ScanKeys -> R.string.trust_no_keys
    ContactCta.Accept -> R.string.trust_requested
    ContactCta.Chat -> R.string.trust_pinned
    ContactCta.Loading -> R.string.trust_loading
    }
}

internal fun historyOrder(row: HistoryMessage): Pair<Int, Long> =
    row.serverSeq?.let { 1 to it } ?: if (row.deliveryState == DeliveryState.QUEUED) 2 to row.localId else 0 to row.localId
internal val historyComparator = compareBy<HistoryMessage>({ historyOrder(it).first }, { historyOrder(it).second })
internal fun compareHistoryOrder(a: Pair<Int, Long>, b: Pair<Int, Long>): Int =
    a.first.compareTo(b.first).takeIf { it != 0 } ?: a.second.compareTo(b.second)

internal fun messageVisible(row: HistoryMessage) = !row.hiddenSelf && !row.deletedAll
internal fun canHideMessage(row: HistoryMessage) = messageVisible(row) && row.direction == MessageDirection.OUTGOING
internal fun canChangeMessage(row: HistoryMessage, contact: Dialog?) = canHideMessage(row) &&
    row.deliveryState in setOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED) && contactCta(contact) == ContactCta.Chat
internal fun deleteScopes(row: HistoryMessage, contact: Dialog?): List<uniffi.dmsg_core.DeleteScope> =
    if (!canHideMessage(row)) emptyList() else listOf(uniffi.dmsg_core.DeleteScope.SELF_ONLY) +
        if (canChangeMessage(row, contact)) listOf(uniffi.dmsg_core.DeleteScope.EVERYONE) else emptyList()

/** Pixel offsets are relative to a stable visible TEXT ID, never the raw page position. */
internal data class HistoryAnchor(val localId: Long, val order: Pair<Int, Long>, val offset: Int, val followBottom: Boolean)
internal fun historyAnchor(visible: List<HistoryMessage>, position: Int, offset: Int, followBottom: Boolean): HistoryAnchor? =
    visible.getOrNull(position)?.let { HistoryAnchor(it.localId, historyOrder(it), offset, followBottom) }
internal fun anchorPosition(visible: List<HistoryMessage>, anchor: HistoryAnchor, canonical: List<HistoryMessage> = visible): Int? {
    if (visible.isEmpty()) return null
    if (anchor.followBottom) return visible.lastIndex
    val exact = visible.indexOfFirst { it.localId == anchor.localId }
    if (exact >= 0) return exact
    // The deleted anchor stays in the canonical timeline; choose its next visible neighbor.
    val order = canonical.find { it.localId == anchor.localId }?.let(::historyOrder) ?: anchor.order
    return visible.indexOfFirst { compareHistoryOrder(historyOrder(it), order) >= 0 }.takeIf { it >= 0 }
        ?: visible.lastIndex
}

/** A hidden run does not restart at newest; each worker turn has a fixed raw-page budget. */
internal fun timelineChunk(before: Long?, fetch: (Long?) -> HistoryPage): List<HistoryPage> {
    val pages = mutableListOf<HistoryPage>()
    var cursor = before
    do {
        val page = fetch(cursor)
        pages.add(page)
        cursor = page.nextBeforeLocalId
    } while (pages.size < 3 && pages.last().rows.none(::messageVisible) && cursor != null)
    return pages
}

/** Server-order window with stable local IDs; reads never move the local read cursor. */
internal class HistoryWindow {
    val rows = mutableListOf<HistoryMessage>()
    var nextBefore: Long? = null
        private set
    var initialized = false
        private set
    var gapBefore: Long? = null
        private set
    private var gapThrough: Pair<Int, Long>? = null
    var latestLocalId: Long? = null
        private set
    var hiddenBefore: Long? = null
        private set
    val visibleRows: List<HistoryMessage> get() = rows.filter(::messageVisible)
    private fun continuation(page: HistoryPage) {
        hiddenBefore = if (page.rows.none(::messageVisible)) page.nextBeforeLocalId else null
    }
    fun latest(page: HistoryPage, refreshed: List<HistoryMessage> = emptyList(), localHead: Long? = null) {
        val oldById = rows.associateBy { it.localId }
        val bridging = gapBefore != null
        val byId = rows.associateBy { it.localId }.toMutableMap()
        refreshed.forEach { byId[it.localId] = it }
        page.rows.forEach { byId[it.localId] = it }
        val metadataMoved = refreshed.any { oldById[it.localId]?.let { old -> historyOrder(old) != historyOrder(it) } == true }
        // A bridge may see a new high ID without seeing every earlier ingest.
        // Only a normal refresh/discovery may advance that watermark.
        val head = localHead ?: if (!bridging) page.rows.maxOfOrNull { it.localId } else null
        val appended = head?.let { it > (latestLocalId ?: 0L) } == true
        rows.clear(); rows.addAll(byId.values.sortedWith(historyComparator))
        if (bridging) {
            gapBefore = if (page.rows.any { compareHistoryOrder(historyOrder(it), gapThrough!!) <= 0 }) null else page.nextBeforeLocalId
        } else if (initialized && (appended || metadataMoved)) {
            // Keep the previous pagination boundary, not the lowest newly inserted
            // row: a late receive may rank far below an otherwise contiguous page.
            val retainedOrders = oldById.keys.mapNotNull { byId[it]?.let(::historyOrder) }
            gapThrough = if (metadataMoved) retainedOrders.minWithOrNull(::compareHistoryOrder) else retainedOrders.maxWithOrNull(::compareHistoryOrder)
            gapBefore = if (gapThrough == null || page.rows.any { compareHistoryOrder(historyOrder(it), gapThrough!!) <= 0 }) null else page.nextBeforeLocalId
        }
        if (!initialized) nextBefore = page.nextBeforeLocalId
        else if (page.nextBeforeLocalId == null) nextBefore = null
        latestLocalId = maxOf(latestLocalId ?: 0L, head ?: 0L).takeIf { it > 0 }
        initialized = true
        continuation(page)
    }
    fun older(page: HistoryPage) {
        val byId = rows.associateBy { it.localId }.toMutableMap()
        page.rows.forEach { byId[it.localId] = it }
        rows.clear(); rows.addAll(byId.values.sortedWith(historyComparator))
        nextBefore = page.nextBeforeLocalId
        continuation(page)
    }
    fun replace(row: HistoryMessage) {
        val index = rows.indexOfFirst { it.localId == row.localId }
        if (index >= 0) {
            val current = rows[index]
            // A retained completion proves the write, but must not resurrect an older projection.
            if (current.revision > row.revision) return
            fun stateRank(state: DeliveryState?) = when (state) {
                null -> 0
                DeliveryState.QUEUED -> 1
                DeliveryState.ACCEPTED -> 2
                DeliveryState.DELIVERED -> 3
            }
            rows[index] = row.copy(
                hiddenSelf = current.hiddenSelf || row.hiddenSelf,
                deletedAll = current.deletedAll || row.deletedAll,
                text = if (current.hiddenSelf || row.hiddenSelf || current.deletedAll || row.deletedAll) "" else row.text,
                deliveryState = if (stateRank(current.deliveryState) > stateRank(row.deliveryState)) current.deliveryState else row.deliveryState,
                changeDeliveryState = if (current.revision == row.revision && stateRank(current.changeDeliveryState) > stateRank(row.changeDeliveryState)) current.changeDeliveryState else row.changeDeliveryState,
                serverSeq = current.serverSeq ?: row.serverSeq,
                serverTimestampMs = current.serverTimestampMs ?: row.serverTimestampMs)
        }
        rows.sortWith(historyComparator)
    }
    fun viewedAnchor(renderedIds: Collection<Long>): Long? = renderedIds.filter { id ->
        rows.any { it.localId == id && messageVisible(it) }
    }.maxOrNull()
}

internal data class EditDraft(val localId: Long, val expectedRevision: ULong, val baseline: String, var text: String,
    val unavailable: Boolean = false)

/** Both drafts are memory-only. A missing/hidden edit target never changes composer mode to Send. */
internal class MessageComposer {
    val normal = OutgoingDraft()
    var edit: EditDraft? = null
        private set
    var text: String
        get() = edit?.text ?: normal.text
        set(value) { edit?.let { it.text = value } ?: run { normal.text = value } }
    fun start(row: HistoryMessage): Boolean {
        if (edit != null || !canHideMessage(row) || row.deliveryState !in setOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED)) return false
        edit = EditDraft(row.localId, row.revision, row.text, row.text)
        return true
    }
    fun restore(attempt: MessageActionAttempt) {
        if (edit != null) return
        (attempt.command as? MessageActionCommand.Edit)?.let {
            edit = EditDraft(attempt.before.localId, it.expectedRevision, attempt.before.text, it.text)
        }
    }
    fun cancel() { edit = null }
    fun saved(localId: Long) { if (edit?.localId == localId) edit = null }
    fun refresh(row: HistoryMessage, rebase: Boolean = false) {
        val current = edit ?: return
        if (current.localId != row.localId) return
        edit = current.copy(expectedRevision = if (rebase) row.revision else current.expectedRevision,
            baseline = if (rebase) row.text else current.baseline,
            unavailable = !canHideMessage(row) || row.deliveryState !in setOf(DeliveryState.ACCEPTED, DeliveryState.DELIVERED))
    }
    fun unavailable() { edit = edit?.copy(unavailable = true) }
}

@StringRes internal fun outboxKindRes(kind: String): Int = when (kind) {
    "text" -> R.string.outbox_text
    "edit" -> R.string.outbox_edit
    "delete" -> R.string.outbox_delete
    else -> R.string.outbox_event
}
@StringRes internal fun actionSavedRes(command: MessageActionCommand, row: HistoryMessage): Int = when (command) {
    is MessageActionCommand.Edit -> if (row.revision == command.expectedRevision) R.string.edit_unchanged else R.string.edit_saved
    is MessageActionCommand.Delete -> if (command.scope == uniffi.dmsg_core.DeleteScope.EVERYONE) R.string.delete_saved
        else if (row.deliveryState == DeliveryState.QUEUED) R.string.self_hidden_queued else R.string.self_hidden
}

/** A successful durable send consumes only its submitted draft, even if status lookup fails later. */
internal class OutgoingDraft {
    var text = ""
    private var submitted: String? = null
    fun begin(): String? {
        if (submitted != null || text.isEmpty()) return null
        submitted = text
        return submitted
    }
    fun finish(durablySaved: Boolean) {
        if (durablySaved && text == submitted) text = ""
        submitted = null
    }
}

/** Guards are main-thread owned. Closing a screen invalidates callbacks without cancelling a commit. */
internal class UiGuard {
    var generation = 0L; private set
    var pending = false; private set
    fun begin(): Long? = if (pending) null else generation.also { pending = true }
    fun accepts(stamp: Long) = generation == stamp
    fun finish(stamp: Long): Boolean {
        if (!accepts(stamp) || !pending) return false
        pending = false; return true
    }
    fun stop() { generation++; pending = false }
}

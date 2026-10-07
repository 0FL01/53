package org.dmsg.client

import android.content.res.Resources
import androidx.annotation.StringRes
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.HistoryPage

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
    }
    fun older(page: HistoryPage) {
        val byId = rows.associateBy { it.localId }.toMutableMap()
        page.rows.forEach { byId[it.localId] = it }
        rows.clear(); rows.addAll(byId.values.sortedWith(historyComparator))
        nextBefore = page.nextBeforeLocalId
    }
    fun viewedAnchor(renderedIds: Collection<Long>): Long? = renderedIds.filter { id -> rows.any { it.localId == id } }.maxOrNull()
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

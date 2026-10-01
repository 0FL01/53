package org.dmsg.client

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

internal fun deliveryLabel(state: DeliveryState?): String = when (state) {
    DeliveryState.QUEUED -> "В очереди"
    DeliveryState.ACCEPTED -> "На сервере"
    DeliveryState.DELIVERED -> "Доставка подтверждена сервером"
    null -> "Статус пока неизвестен"
}

internal enum class ContactCta { ScanKeys, Accept, VerifyChanged, Blocked, Chat, Loading }
internal fun contactCta(contact: Dialog?): ContactCta = when {
    contact == null -> ContactCta.Loading
    contact.state == "blocked" -> ContactCta.Blocked
    contact.identityMismatch -> ContactCta.VerifyChanged
    !contact.hasKeys -> ContactCta.ScanKeys
    contact.state == "requested" -> ContactCta.Accept
    contact.state == "accepted" -> ContactCta.Chat
    else -> ContactCta.Loading
}
internal fun trustLabel(contact: Dialog?): String = when (contactCta(contact)) {
    ContactCta.VerifyChanged -> "Ключ контакта изменился — отправка СТОП. Проверьте QR другим способом, затем подтвердите новый ключ."
    ContactCta.Blocked -> "Контакт заблокирован. Блокировка в этой версии необратима."
    ContactCta.ScanKeys -> "Нет ключей контакта. Сканируйте его QR."
    ContactCta.Accept -> "Контакт ожидает вашего согласия."
    ContactCta.Chat -> "Ключи контакта закреплены локально"
    ContactCta.Loading -> "Контакт ещё не загружен"
}

/** One local per-contact window; page reads themselves never move the read cursor. */
internal class HistoryWindow {
    val rows = mutableListOf<HistoryMessage>()
    var nextBefore: Long? = null
        private set
    var initialized = false
        private set
    var gapBefore: Long? = null
        private set
    private var gapThrough = 0L
    fun latest(page: HistoryPage) {
        val previouslyNewest = rows.lastOrNull()?.localId
        val bridging = gapBefore != null
        val byId = rows.associateBy { it.localId }.toMutableMap()
        page.rows.forEach { byId[it.localId] = it }
        rows.clear(); rows.addAll(byId.values.sortedBy { it.localId })
        if (bridging) {
            gapBefore = if (page.rows.any { it.localId <= gapThrough }) null else page.nextBeforeLocalId
        } else if (initialized && previouslyNewest != null && page.rows.isNotEmpty() && page.rows.last().localId > previouslyNewest) {
            gapThrough = previouslyNewest; gapBefore = page.nextBeforeLocalId
        }
        if (!initialized) nextBefore = page.nextBeforeLocalId
        initialized = true
    }
    fun older(page: HistoryPage) {
        val ids = rows.map { it.localId }.toSet()
        rows.addAll(0, page.rows.asReversed().filter { it.localId !in ids })
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

package org.dmsg.client

import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.RegistrationPolicy
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.DialogSummary
import uniffi.dmsg_core.DialogsPage
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.HistoryPage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.QrOutcome

/** In-memory fake for JVM unit tests and UI previews (no native lib). */
class FakeFacade : DmsgFacade {
    var authenticated = false
    var myId: String? = null
    var profile: DnsProfile? = null
    var policy = RegistrationPolicy.INVITE_ONLY
    var policyFailure: DmsgError? = null
    var authFailure: DmsgError? = null
    var stopCalls = 0
    var previewCalls = 0
    var signupCalls = 0
    val loginCalls = mutableListOf<Pair<String, String?>>()
    val outcomes = ArrayDeque<LoginOutcome>()
    var signupProbe: (String, String, String?) -> Unit = { _, _, _ -> }
    var loginProbe: (String, String, String?) -> Unit = { _, _, _ -> }
    val dialogs = mutableListOf<Dialog>()
    val messages = mutableListOf<Msg>()
    var lastSent: String? = null
    val history = mutableListOf<HistoryMessage>()
    val aliases = mutableMapOf<String, String>()
    val readCursors = mutableMapOf<String, Long>()
    var sendFailure: DmsgError? = null
    var failAfterInsert = false

    override fun isReady() = true
    override fun dnsProfile() = profile
    override fun configureDns(code: String, resolvers: List<String>) {
        profile = DnsProfile("test.example", ByteArray(32), "ab".repeat(32), resolvers)
    }
    override fun registrationPolicyDns(): RegistrationPolicy { policyFailure?.let { throw it }; return policy }
    override fun signupDns(login: String, password: String, invitation: String?): AccountInfo {
        signupCalls++
        authFailure?.let { throw it }
        signupProbe(login, password, invitation)
        authenticated = true; myId = "MOCK1234MOCK"
        return account()
    }
    override fun loginDns(login: String, password: String, expectedDevice: String?): LoginOutcome {
        loginCalls.add(Pair(login, expectedDevice))
        authFailure?.let { throw it }
        loginProbe(login, password, expectedDevice)
        val out = if (outcomes.isEmpty()) LoginOutcome.Authenticated("MOCK1234MOCK") else outcomes.removeFirst()
        if (out is LoginOutcome.Authenticated) { authenticated = true; myId = out.contactId }
        return out
    }
    override fun dnsNetworkChanged(resolvers: List<String>) {}
    override fun dnsStop() { stopCalls++ }
    override fun dnsStatus() = "stopped"
    override fun account() = AccountInfo(authenticated, myId)
    override fun profilePreview(code: String): Pair<String, String> { previewCalls++; return Pair("test.example", "ab".repeat(32)) }
    override fun myQr() = "dmsg://contact/MOCK"
    override fun addQr(uri: String) = QrOutcome.ADDED
    override fun contactQrId(uri: String) = "0123456789AB"
    override fun inviteQr(uri: String) = QrOutcome.ADDED
    override fun request(id: String): String {
        dialogs.add(Dialog(id, "requested"))
        return "requested"
    }
    override fun accept(id: String) {
        replace(id, "accepted")
    }
    override fun block(id: String) {
        replace(id, "blocked")
    }
    override fun confirm(id: String) {
        replace(id, "accepted")
    }
    override fun get(id: String) = dialogs.find { it.contactId == id }
    override fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?> {
        val lim = limit.coerceIn(1, 100)
        val after = cursor ?: ""
        val page = dialogs.filter { it.contactId > after }.take(lim)
        val next = if (dialogs.any { it.contactId > (page.lastOrNull()?.contactId ?: "~") }) {
            page.lastOrNull()?.contactId
        } else null
        return Pair(page, next)
    }
    override fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?> {
        val lim = limit.coerceIn(1, 100)
        val page = messages.filter { it.seq > cursor }.take(lim)
        return Pair(page, page.lastOrNull()?.seq)
    }
    override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage {
        val all = history.filter { it.contactId == contactId && (beforeLocalId == null || it.localId < beforeLocalId) }.sortedByDescending { it.localId }
        val page = all.take(limit.coerceIn(1, 100))
        return HistoryPage(page.map { it.copy() }, if (all.size > page.size) page.last().localId else null)
    }
    override fun messageStatus(mid: String) = history.find { it.messageIdHex == mid && it.direction == MessageDirection.OUTGOING }?.deliveryState
    override fun historyMessage(contactId: String, localId: Long) = history.single { it.contactId == contactId && it.localId == localId }.copy()
    override fun timelinePage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage {
        val anchor = beforeLocalId?.let { historyMessage(contactId, it) }
        val all = history.filter { it.contactId == contactId && (anchor == null || historyComparator.compare(it, anchor) < 0) }.sortedWith(historyComparator.reversed())
        val page = all.take(limit.coerceIn(1, 100))
        return HistoryPage(page.map { it.copy() }, if (all.size > page.size) page.last().localId else null)
    }
    override fun dialogsPage(cursor: String?, limit: Int): DialogsPage {
        val summaries = dialogs.map { dialog ->
            val last = history.filter { it.contactId == dialog.contactId }.maxByOrNull { it.localId }
            val read = readCursors[dialog.contactId] ?: 0L
            DialogSummary(dialog.contactId, aliases[dialog.contactId], last?.text, last?.localTimestampMs,
                history.count { it.contactId == dialog.contactId && it.direction == MessageDirection.INCOMING && it.localId > read }.toULong(),
                read, dialog.hasKeys, dialog.identityMismatch, dialog.state)
        }.sortedWith(compareByDescending<DialogSummary> { it.lastLocalTimestampMs ?: 0L }.thenBy { it.contactId })
        val start = cursor?.toInt() ?: 0
        val page = summaries.drop(start).take(limit.coerceIn(1, 100))
        return DialogsPage(page, if (start + page.size < summaries.size) (start + page.size).toString() else null)
    }
    override fun setContactAlias(id: String, alias: String?) { if (alias == null) aliases.remove(id) else aliases[id] = alias.trim() }
    override fun markRead(id: String, throughLocalId: Long): Long {
        if (history.none { it.contactId == id && it.localId == throughLocalId }) throw DmsgError("Invalid read anchor", ErrorKind.InvalidInput)
        val cursor = maxOf(readCursors[id] ?: 0L, throughLocalId)
        readCursors[id] = cursor
        return cursor
    }
    override fun outbox(cursor: Long, limit: Int) = Pair(emptyList<OutRow>(), null)
    override fun send(id: String, text: String): String {
        sendFailure?.let { throw it }
        lastSent = text
        val localId = (history.maxOfOrNull { it.localId } ?: 0L) + 1
        val mid = localId.toString(16).padStart(32, '0')
        history.add(HistoryMessage(localId, mid, id, MessageDirection.OUTGOING, text, localId, DeliveryState.QUEUED, null, null))
        if (failAfterInsert) throw DmsgError("fixture post-commit failure", ErrorKind.Transport)
        return mid
    }
    override fun retry() = longArrayOf(0, 0, 0, 0)
    override fun fetch() =
        FetchRes(emptyList(), longArrayOf(0, 0, 0, 0), 0)
    override fun reconnect() = 16L
    override fun qrKind(uri: String) = QrGate.route(uri).getOrThrow()
    override fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean) =
        if (hasWrapped) "ready" else if (hasLegacy) "migrate" else "fresh"

    private fun replace(id: String, state: String) {
        val i = dialogs.indexOfFirst { it.contactId == id }
        if (i >= 0) dialogs[i] = dialogs[i].copy(state = state, identityMismatch = false) else dialogs.add(Dialog(id, state))
    }
}

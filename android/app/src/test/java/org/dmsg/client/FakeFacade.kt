package org.dmsg.client

/** In-memory fake for JVM unit tests and UI previews (no native lib). */
class FakeFacade : DmsgFacade {
    var enrolled = false
    var myId: String? = null
    val dialogs = mutableListOf<Dialog>()
    val messages = mutableListOf<Msg>()
    var lastSent: String? = null

    override fun isReady() = true
    override fun account(): Pair<Boolean, String?> = Pair(enrolled, myId)
    override fun preview(qr: String) = Pair("test.example", "ab".repeat(32))
    override fun enrol(qr: String, addr: String, pinDer: ByteArray?): String {
        enrolled = true
        myId = "MOCK1234MOCK"
        return myId!!
    }
    override fun myQr() = "dmsg://contact/MOCK"
    override fun addQr(uri: String) = "added"
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
    override fun outbox(cursor: Long, limit: Int) = Pair(emptyList<OutRow>(), null)
    override fun send(addr: String, pub: ByteArray, domain: String, id: String, text: String): String {
        lastSent = text
        return "00".repeat(16)
    }
    override fun retry(addr: String, pub: ByteArray, domain: String) = longArrayOf(0, 0, 0, 0)
    override fun fetch(addr: String, pub: ByteArray, domain: String) =
        FetchRes(emptyList(), longArrayOf(0, 0, 0, 0), 0)
    override fun reconnect(addr: String, pub: ByteArray, domain: String) = 16L
    override fun qrKind(uri: String) = QrGate.route(uri).getOrThrow()
    override fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean) =
        if (hasWrapped) "ready" else if (hasLegacy) "migrate" else "fresh"

    private fun replace(id: String, state: String) {
        val i = dialogs.indexOfFirst { it.contactId == id }
        if (i >= 0) dialogs[i] = Dialog(id, state) else dialogs.add(Dialog(id, state))
    }
}

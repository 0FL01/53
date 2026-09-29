package org.dmsg.client

/** UI-level error: only static reasons, never key material / plaintext. */
class DmsgError(msg: String) : Exception(msg)

data class Dialog(val contactId: String, val state: String)
data class Msg(val seq: Long, val contactId: String, val text: String)
data class OutRow(val mid: String, val contactId: String, val status: String)
data class FetchRes(val received: List<Msg>, val skipped: LongArray, val cursor: Long)

/**
 * Commands/events boundary over the Rust core.
 * Lists are strictly paginated (cursor/limit); no raw DB cursors cross it.
 */
interface DmsgFacade {
    fun isReady(): Boolean
    fun account(): Pair<Boolean, String?>
    fun preview(qr: String): Pair<String, String>
    fun enrol(qr: String, addr: String, pinDer: ByteArray?): String
    fun myQr(): String
    fun addQr(uri: String): String
    fun request(id: String): String
    fun accept(id: String)
    fun block(id: String)
    fun confirm(id: String)
    fun get(id: String): Dialog?
    fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?>
    fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?>
    fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?>
    fun send(addr: String, pub: ByteArray, domain: String, id: String, text: String): String
    fun retry(addr: String, pub: ByteArray, domain: String): LongArray
    fun fetch(addr: String, pub: ByteArray, domain: String): FetchRes
    fun reconnect(addr: String, pub: ByteArray, domain: String): Long
    fun qrKind(uri: String): String
    fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean): String
}

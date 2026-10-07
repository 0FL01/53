package org.dmsg.client

import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.QrKind
import uniffi.dmsg_core.RegistrationPolicy
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.DialogsPage
import uniffi.dmsg_core.HistoryPage
import uniffi.dmsg_core.QrOutcome
import androidx.annotation.StringRes

/** UI-level error: only static reasons, never key material / plaintext. */
class DmsgError(msg: String, val kind: ErrorKind = ErrorKind.Other,
    @param:StringRes val uiMessageRes: Int? = null) : Exception(msg) {
    constructor(@StringRes uiMessageRes: Int, kind: ErrorKind = ErrorKind.Other) :
        this("UI error", kind, uiMessageRes)
}
enum class ErrorKind { Other, InvalidCredentials, LoginTaken, InviteRequired, InviteExpired, InviteRevoked,
    InviteUsed, AuthRateLimited, InvalidInput, Transport, PinMismatch, IdentityMismatch, NotAuthenticated,
    BadQr, UnknownContact, NotAccepted, Blocked, MissingKeys, Revoked, Busy, BadText, Store, Crypto, Protocol, NativeUnavailable,
    StorageKeyLost, SnapshotMissing, LiveDatabaseExists, LiveDatabaseMissing, SnapshotRestoreRequired, SnapshotInvalid }

data class Dialog(val contactId: String, val state: String, val identityMismatch: Boolean = false, val hasKeys: Boolean = false)
data class DnsProfile(val domain: String, val pub: ByteArray, val fingerprint: String, val resolvers: List<String>)
data class Msg(val seq: Long, val contactId: String, val text: String)
data class OutRow(val mid: String, val contactId: String, val status: String)
data class FetchRes(val received: List<Msg>, val skipped: LongArray, val cursor: Long)

/**
 * Commands/events boundary over the Rust core.
 * Lists are strictly paginated (cursor/limit); no raw DB cursors cross it.
 */
interface DmsgFacade {
    fun dnsProfile(): DnsProfile?
    fun configureDns(code: String, resolvers: List<String>)
    fun registrationPolicyDns(): RegistrationPolicy
    fun signupDns(login: String, password: String, invitation: String?): AccountInfo
    fun loginDns(login: String, password: String, expectedDevice: String?): LoginOutcome
    fun dnsNetworkChanged(resolvers: List<String>)
    fun dnsStop()
    fun dnsStatus(): String
    fun isReady(): Boolean
    fun account(): AccountInfo
    fun profilePreview(code: String): Pair<String, String>
    fun myQr(): String
    fun addQr(uri: String): QrOutcome
    fun contactQrId(uri: String): String
    fun inviteQr(uri: String): QrOutcome
    fun request(id: String): String
    fun accept(id: String)
    fun block(id: String)
    fun confirm(id: String)
    fun get(id: String): Dialog?
    fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?>
    fun dialogsPage(cursor: String?, limit: Int): DialogsPage
    fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage
    fun messageStatus(mid: String): DeliveryState?
    fun setContactAlias(id: String, alias: String?)
    fun markRead(id: String, throughLocalId: Long): Long
    fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?>
    fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?>
    fun send(id: String, text: String): String
    fun retry(): LongArray
    fun fetch(): FetchRes
    fun reconnect(): Long
    fun qrKind(uri: String): QrKind
    fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean): String
}

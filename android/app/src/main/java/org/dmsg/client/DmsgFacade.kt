package org.dmsg.client

import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.QrKind
import uniffi.dmsg_core.RegistrationPolicy
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.DialogsPage
import uniffi.dmsg_core.HistoryPage
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.DeleteScope
import uniffi.dmsg_core.MessageKind
import uniffi.dmsg_core.VoiceInfo
import uniffi.dmsg_core.VoiceTransferProgress
import uniffi.dmsg_core.QrOutcome
import androidx.annotation.StringRes

/** UI-level error: only static reasons, never key material / plaintext. */
class DmsgError(msg: String, val kind: ErrorKind = ErrorKind.Other,
    @param:StringRes val uiMessageRes: Int? = null) : Exception(msg) {
    constructor(@StringRes uiMessageRes: Int, kind: ErrorKind = ErrorKind.Other) :
        this("UI error", kind, uiMessageRes)
}
enum class ErrorKind { Other, InvalidCredentials, LoginTaken, InviteRequired, InviteExpired, InviteRevoked,
    InviteUsed, InviteLimit, AuthRateLimited, InvalidInput, Transport, PinMismatch, IdentityMismatch, NotAuthenticated,
    BadQr, UnknownContact, NotAccepted, Blocked, MissingKeys, Revoked, Busy, BadText, Store, Crypto, Protocol, NativeUnavailable,
    StorageKeyLost, SnapshotMissing, LiveDatabaseExists, LiveDatabaseMissing, SnapshotRestoreRequired, SnapshotInvalid,
    MessageChanged, MessageUnavailable, VoiceSessionRequired, BadVoice }

data class Dialog(val contactId: String, val state: String, val identityMismatch: Boolean = false, val hasKeys: Boolean = false)
data class DnsProfile(val domain: String, val pub: ByteArray, val fingerprint: String, val resolvers: List<String>)
data class Msg(val seq: Long, val contactId: String, val text: String,
    val kind: MessageKind = MessageKind.TEXT, val voice: VoiceInfo? = null)
data class OutRow(val mid: String, val contactId: String, val status: String, val kind: String = "text")
data class FetchRes(val received: List<Msg>, val skipped: LongArray, val cursor: Long)

/** Opaque bulk lane; advance never holds the app's store lock. */
interface VoiceTransferHandle {
    fun advance(): VoiceTransferProgress
    fun commit(): VoiceTransferProgress
    fun cancel()
}

/**
 * Commands/events boundary over the Rust core.
 * Lists are strictly paginated (cursor/limit); no raw DB cursors cross it.
 */
interface DmsgFacade {
    fun dnsProfile(): DnsProfile?
    fun configureDns(code: String, resolvers: List<String>)
    fun registrationPolicyDns(): RegistrationPolicy
    fun normalizeInvitation(input: String): String
    fun issueInvitation(id: ByteArray): InvitationGrant
    fun listInvitations(): InvitationList
    fun revokeInvitation(id: ByteArray)
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
    fun timelinePage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage
    fun historyMessage(contactId: String, localId: Long): uniffi.dmsg_core.HistoryMessage
    fun messageStatus(mid: String): DeliveryState?
    fun setContactAlias(id: String, alias: String?)
    fun markRead(id: String, throughLocalId: Long): Long
    fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?>
    fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?>
    fun send(id: String, text: String, replyToLocalId: Long? = null): String
    fun editMessage(contactId: String, localId: Long, expectedRevision: ULong, text: String): HistoryMessage
    fun deleteMessage(contactId: String, localId: Long, scope: DeleteScope): HistoryMessage
    fun voiceSessionReady(contactId: String): Boolean
    fun primeVoiceSession(contactId: String)
    fun queueVoice(contactId: String, midHex: String, encodedBytes: ByteArray, replyToLocalId: Long? = null): HistoryMessage
    fun historyMessageByMid(contactId: String, midHex: String): HistoryMessage?
    fun voiceData(contactId: String, localId: Long): ByteArray
    fun pendingVoiceUpload(): HistoryMessage?
    fun prepareVoiceTransfer(contactId: String, localId: Long, download: Boolean): VoiceTransferHandle
    fun clearVoiceCache()
    fun retry(): LongArray
    fun fetch(): FetchRes
    fun reconnect(): Long
    fun qrKind(uri: String): QrKind
}

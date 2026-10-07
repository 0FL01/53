package org.dmsg.client

import android.content.Context
import uniffi.dmsg_core.DmsgClient
import uniffi.dmsg_core.FfiException
import uniffi.dmsg_core.QrKind
import uniffi.dmsg_core.QrOutcome
import uniffi.dmsg_core.DeleteScope
import uniffi.dmsg_core.pageLimit
import uniffi.dmsg_core.qrKind
import java.io.File

/** Real facade over generated UniFFI bindings (commands/events only). */
class UniFfiFacade(private val dbPath: String, key: ByteArray, private val context: Context? = null) : DmsgFacade {
    private val core: DmsgClient = try { DmsgClient.openEncrypted(dbPath, key) }
        catch (e: FfiException) { throw ffiError(e) }
    private val networkState = DnsNetwork.runtime(dbPath)

    private inline fun <T> wrap(block: () -> T): T {
        try {
            return synchronized(Core.storeLock) {
                if (!File(dbPath).exists() && File("$dbPath.sealed").exists()) {
                    throw DmsgError("sealed identity: restore in Storage before use", ErrorKind.SnapshotRestoreRequired)
                }
                block()
            }
        } catch (e: FfiException) {
            // Generated error type carries only static reasons (no secrets).
            throw ffiError(e)
        }
    }

    override fun isReady() = true
    override fun dnsProfile(): DnsProfile? = wrap {
        core.dnsProfileInfo()?.let { DnsProfile(it.domain, it.noisePubkey, it.pinFingerprintHex, it.resolvers) }
    }
    override fun configureDns(code: String, resolvers: List<String>) = wrap { core.configureDns(code, resolvers) }
    override fun registrationPolicyDns() = dnsCommand { core.registrationPolicyDns() }
    override fun signupDns(login: String, password: String, invitation: String?) =
        dnsCommand { core.signupDns(login, password, invitation) }
    override fun loginDns(login: String, password: String, expectedDevice: String?) =
        dnsCommand { core.loginDns(login, password, expectedDevice) }
    override fun dnsNetworkChanged(resolvers: List<String>) = wrap { core.dnsNetworkChanged(resolvers) }
    // Cancellation has no DB access and must not wait for a blocked store call.
    override fun dnsStop() {
        try { core.dnsStop() } catch (e: FfiException) { throw ffiError(e) }
    }
    override fun dnsStatus(): String = wrap { core.dnsStatus() }

    override fun account() = wrap { core.accountInfo() }

    override fun profilePreview(code: String): Pair<String, String> = wrap {
        val p = core.profilePreview(code)
        Pair(p.domain, p.pinFingerprintHex)
    }

    override fun myQr(): String = wrap { core.myContactQr() }

    override fun addQr(uri: String) = wrap { core.addContactQr(uri) }
    override fun contactQrId(uri: String) = wrap { core.contactQrId(uri) }
    override fun inviteQr(uri: String) = wrap { core.inviteContactQr(uri) }

    override fun request(id: String): String = wrap { core.contactRequest(id) }
    override fun accept(id: String) = wrap { core.contactAccept(id) }
    override fun block(id: String) = wrap { core.contactBlock(id) }
    override fun confirm(id: String) = wrap { core.contactConfirm(id) }

    override fun get(id: String): Dialog? = wrap {
        try {
            val c = core.contactGet(id)
            Dialog(c.contactId, c.state, c.identityMismatch, c.hasKeys)
        } catch (e: FfiException) {
            // UnknownContact -> null card (not an error screen).
            if (e is FfiException.UnknownContact) null else throw ffiError(e)
        }
    }

    override fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?> = wrap {
        val p = core.contactsPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { val c = core.contactGet(it.contactId); Dialog(it.contactId, it.state, c.identityMismatch, c.hasKeys) }, p.nextCursor)
    }

    override fun dialogsPage(cursor: String?, limit: Int) = wrap { core.dialogsPage(cursor, limit.coerceIn(1, 100).toUInt()) }
    override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int) = wrap {
        core.historyPage(contactId, beforeLocalId, limit.coerceIn(1, 100).toUInt())
    }
    override fun messageStatus(mid: String) = wrap { core.messageStatus(mid) }
    override fun timelinePage(contactId: String, beforeLocalId: Long?, limit: Int) = wrap { core.timelinePage(contactId, beforeLocalId, limit.coerceIn(1, 100).toUInt()) }
    override fun historyMessage(contactId: String, localId: Long) = wrap { core.historyMessage(contactId, localId) }
    override fun setContactAlias(id: String, alias: String?) = wrap { core.setContactAlias(id, alias) }
    override fun markRead(id: String, throughLocalId: Long) = wrap { core.markRead(id, throughLocalId) }

    override fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?> = wrap {
        val p = core.inboxPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { Msg(it.seq, it.contactId, it.text, it.kind, it.voice) }, p.nextCursor)
    }

    override fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?> = wrap {
        val p = core.outboxPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { OutRow(it.messageIdHex, it.contactId, it.status, it.kind) }, p.nextCursor)
    }

    /** Cancellation precedes the store lock, for foreground commands as well as FGS events. */
    internal fun observeDnsNetwork() {
        val app = context ?: return
        networkState.observe({ DnsNetwork.snapshot(app) }, ::dnsStop)
    }

    private fun applyDns() {
        if (context != null) networkState.apply(core::dnsNetworkChanged, DmsgService::wakeAfterNetworkApplied)
    }

    /** Foreground commands detect a new Network even when its resolver IPs are unchanged. */
    private fun <T> dnsCommand(block: () -> T): T {
        observeDnsNetwork()
        return wrap {
            if (Thread.currentThread().isInterrupted) throw InterruptedException("DNS command stopped")
            core.dnsProfileInfo() ?: throw DmsgError(R.string.error_connection_code_required)
            applyDns()
            if (Thread.currentThread().isInterrupted) throw InterruptedException("DNS command stopped")
            block()
        }
    }

    internal fun refreshDnsNetwork(keepGoing: () -> Boolean) {
        if (!keepGoing()) return
        observeDnsNetwork()
        wrap {
            if (keepGoing() && core.dnsProfileInfo() != null) applyDns()
        }
    }

    override fun send(id: String, text: String): String = dnsCommand { core.sendDns(id, text) }
    // Local durable commands must work offline, before any DNS observation/application.
    override fun editMessage(contactId: String, localId: Long, expectedRevision: ULong, text: String) = wrap {
        core.editMessage(contactId, localId, expectedRevision, text)
    }
    override fun deleteMessage(contactId: String, localId: Long, scope: DeleteScope) = wrap {
        core.deleteMessage(contactId, localId, scope)
    }
    override fun voiceSessionReady(contactId: String) = wrap { core.voiceSessionReady(contactId) }
    override fun primeVoiceSession(contactId: String) = dnsCommand { core.primeVoiceSessionDns(contactId) }
    override fun queueVoice(contactId: String, midHex: String, encodedBytes: ByteArray) = wrap {
        core.queueVoice(contactId, midHex, encodedBytes)
    }
    override fun historyMessageByMid(contactId: String, midHex: String) = wrap { core.historyMessageByMid(contactId, midHex) }
    override fun voiceData(contactId: String, localId: Long) = wrap { core.voiceData(contactId, localId) }
    override fun pendingVoiceUpload() = wrap { core.pendingVoiceUpload() }
    override fun clearVoiceCache() = wrap { core.clearVoiceCache() }
    override fun prepareVoiceTransfer(contactId: String, localId: Long, download: Boolean): VoiceTransferHandle {
        observeDnsNetwork()
        val transfer = wrap { applyDns(); core.prepareVoiceTransfer(contactId, localId, download) }
        return object : VoiceTransferHandle {
            private val closed = java.util.concurrent.atomic.AtomicBoolean(false)
            override fun advance() = try { transfer.advance() } catch (e: FfiException) { throw ffiError(e) }
            override fun commit() = wrap { core.commitVoiceTransfer(transfer) }
            override fun cancel() {
                if (closed.compareAndSet(false, true)) try { transfer.cancel() } finally { transfer.close() }
            }
        }
    }

    override fun retry(): LongArray = dnsCommand {
        val r = core.retryDns()
        longArrayOf(r.resent.toLong(), r.accepted.toLong(), r.delivered.toLong(), r.skipped.toLong())
    }

    override fun fetch(): FetchRes = dnsCommand {
        val r = core.fetchDns()
        FetchRes(
            r.received.map { Msg(it.seq.toLong(), it.contactId, it.text, it.kind, it.voice) },
            longArrayOf(
                r.skippedUnknown.toLong(), r.skippedBlocked.toLong(),
                r.skippedUndecryptable.toLong(), r.skippedMismatch.toLong()
            ),
            r.cursor.toLong()
        )
    }

    override fun reconnect(): Long = dnsCommand { core.reconnectDns().toLong() }

    override fun qrKind(uri: String): QrKind = wrap { uniffi.dmsg_core.qrKind(uri) }

}

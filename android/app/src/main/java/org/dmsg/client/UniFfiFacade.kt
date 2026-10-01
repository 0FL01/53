package org.dmsg.client

import android.content.Context
import uniffi.dmsg_core.DmsgClient
import uniffi.dmsg_core.FfiException
import uniffi.dmsg_core.QrKind
import uniffi.dmsg_core.QrOutcome
import uniffi.dmsg_core.StoragePlan
import uniffi.dmsg_core.pageLimit
import uniffi.dmsg_core.qrKind
import uniffi.dmsg_core.storagePlan
import java.io.File

/** Real facade over generated UniFFI bindings (commands/events only). */
class UniFfiFacade(private val dbPath: String, key: ByteArray, private val context: Context? = null) : DmsgFacade {
    private val core: DmsgClient = try { DmsgClient.openEncrypted(dbPath, key) }
        catch (e: FfiException) { throw ffiError(e) }

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
    override fun registrationPolicyDns() = wrap { refreshDns(); core.registrationPolicyDns() }
    override fun signupDns(login: String, password: String, invitation: String?) =
        wrap { refreshDns(); core.signupDns(login, password, invitation) }
    override fun loginDns(login: String, password: String, expectedDevice: String?) =
        wrap { refreshDns(); core.loginDns(login, password, expectedDevice) }
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
    override fun setContactAlias(id: String, alias: String?) = wrap { core.setContactAlias(id, alias) }
    override fun markRead(id: String, throughLocalId: Long) = wrap { core.markRead(id, throughLocalId) }

    override fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?> = wrap {
        val p = core.inboxPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { Msg(it.seq, it.contactId, it.text) }, p.nextCursor)
    }

    override fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?> = wrap {
        val p = core.outboxPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { OutRow(it.messageIdHex, it.contactId, it.status) }, p.nextCursor)
    }

    /** Foreground commands also refresh DNS when the background service is disabled. */
    private fun refreshDns() {
        val profile = core.dnsProfileInfo() ?: throw DmsgError("Сначала добавьте код подключения")
        val app = context ?: return
        val resolvers = try { DnsNetwork.resolvers(app) } catch (_: DmsgError) {
            // Do not reuse a stale Ready carrier after radio/network loss. The Rust
            // supervisor decides whether an established session can queue offline.
            core.dnsStop()
            return
        }
        if (resolvers != profile.resolvers) core.dnsNetworkChanged(resolvers)
    }

    override fun send(id: String, text: String): String = wrap { refreshDns(); core.sendDns(id, text) }

    override fun retry(): LongArray = wrap {
        refreshDns()
        val r = core.retryDns()
        longArrayOf(r.resent.toLong(), r.accepted.toLong(), r.delivered.toLong(), r.skipped.toLong())
    }

    override fun fetch(): FetchRes = wrap {
        refreshDns()
        val r = core.fetchDns()
        FetchRes(
            r.received.map { Msg(it.seq.toLong(), it.contactId, it.text) },
            longArrayOf(
                r.skippedUnknown.toLong(), r.skippedBlocked.toLong(),
                r.skippedUndecryptable.toLong(), r.skippedMismatch.toLong()
            ),
            r.cursor.toLong()
        )
    }

    override fun reconnect(): Long = wrap { refreshDns(); core.reconnectDns().toLong() }

    override fun qrKind(uri: String): QrKind = wrap { uniffi.dmsg_core.qrKind(uri) }

    override fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean): String = wrap {
        when (uniffi.dmsg_core.storagePlan(hasLegacy, hasWrapped)) {
            StoragePlan.FRESH_INSTALL -> "fresh"
            StoragePlan.MIGRATE_LEGACY -> "migrate"
            StoragePlan.READY_WRAPPED -> "ready"
        }
    }
}

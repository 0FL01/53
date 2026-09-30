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
        catch (e: FfiException) { throw DmsgError(ffiErrorMessage(e)) }

    private inline fun <T> wrap(block: () -> T): T {
        try {
            return synchronized(Core.storeLock) {
                if (!File(dbPath).exists() && File("$dbPath.sealed").exists()) {
                    throw DmsgError("sealed identity: restore in Storage before use")
                }
                block()
            }
        } catch (e: FfiException) {
            // Generated error type carries only static reasons (no secrets).
            throw DmsgError(ffiErrorMessage(e))
        }
    }

    override fun isReady() = true
    override fun dnsProfile(): DnsProfile? = wrap {
        core.dnsProfileInfo()?.let { DnsProfile(it.domain, it.noisePubkey, it.pinFingerprintHex, it.resolvers) }
    }
    override fun configureDns(qr: String, resolvers: List<String>) = wrap { core.configureDns(qr, resolvers) }
    override fun enrolDns(qr: String, resolvers: List<String>): String = wrap { core.enrolDns(qr, resolvers).contactId }
    override fun dnsNetworkChanged(resolvers: List<String>) = wrap { core.dnsNetworkChanged(resolvers) }
    // Cancellation has no DB access and must not wait for a blocked store call.
    override fun stopDns() {
        try { core.stopDns() } catch (e: FfiException) { throw DmsgError(ffiErrorMessage(e)) }
    }
    override fun dnsStatus(): String = wrap { core.dnsStatus() }

    override fun account(): Pair<Boolean, String?> = wrap {
        val a = core.accountInfo()
        Pair(a.enrolled, a.contactId)
    }

    override fun preview(qr: String): Pair<String, String> = wrap {
        val p = core.enrolPreview(qr)
        Pair(p.domain, p.pinFingerprintHex)
    }

    override fun enrol(qr: String, addr: String, pinDer: ByteArray?): String = wrap {
        core.enrolFromQr(qr, addr, pinDer).contactId
    }

    override fun myQr(): String = wrap { core.myContactQr() }

    override fun addQr(uri: String): String = wrap {
        when (core.addContactQr(uri)) {
            QrOutcome.ADDED -> "added"
            QrOutcome.UNCHANGED -> "unchanged"
            QrOutcome.IDENTITY_CHANGED -> "identity_changed"
        }
    }

    override fun request(id: String): String = wrap { core.contactRequest(id) }
    override fun accept(id: String) = wrap { core.contactAccept(id) }
    override fun block(id: String) = wrap { core.contactBlock(id) }
    override fun confirm(id: String) = wrap { core.contactConfirm(id) }

    override fun get(id: String): Dialog? = wrap {
        try {
            val c = core.contactGet(id)
            Dialog(c.contactId, c.state, c.identityMismatch)
        } catch (e: FfiException) {
            // UnknownContact -> null card (not an error screen).
            if (e is FfiException.UnknownContact) null else throw DmsgError(ffiErrorMessage(e))
        }
    }

    override fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?> = wrap {
        val p = core.contactsPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { Dialog(it.contactId, it.state) }, p.nextCursor)
    }

    override fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?> = wrap {
        val p = core.inboxPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { Msg(it.seq, it.contactId, it.text) }, p.nextCursor)
    }

    override fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?> = wrap {
        val p = core.outboxPage(cursor, pageLimit(limit.toUInt()))
        Pair(p.rows.map { OutRow(it.messageIdHex, it.contactId, it.status) }, p.nextCursor)
    }

    /** Foreground commands also refresh DNS when the background service is disabled. */
    private fun usesDns(): Boolean {
        val profile = core.dnsProfileInfo() ?: return false
        val app = context ?: return true
        val resolvers = try { DnsNetwork.resolvers(app) } catch (_: DmsgError) {
            // Do not reuse a stale Ready carrier after radio/network loss. The Rust
            // supervisor decides whether an established session can queue offline.
            core.stopDns()
            return true
        }
        if (resolvers != profile.resolvers) core.dnsNetworkChanged(resolvers)
        return true
    }

    override fun send(addr: String, pub: ByteArray, domain: String, id: String, text: String): String =
        wrap { if (usesDns()) core.sendDns(id, text) else core.sendText(addr, pub, domain, id, text) }

    override fun retry(addr: String, pub: ByteArray, domain: String): LongArray = wrap {
        val r = if (usesDns()) core.retryDns() else core.retryQueued(addr, pub, domain)
        longArrayOf(r.resent.toLong(), r.accepted.toLong(), r.delivered.toLong(), r.skipped.toLong())
    }

    override fun fetch(addr: String, pub: ByteArray, domain: String): FetchRes = wrap {
        val r = if (usesDns()) core.fetchDns() else core.fetch(addr, pub, domain)
        FetchRes(
            r.received.map { Msg(it.seq.toLong(), it.contactId, it.text) },
            longArrayOf(
                r.skippedUnknown.toLong(), r.skippedBlocked.toLong(),
                r.skippedUndecryptable.toLong(), r.skippedMismatch.toLong()
            ),
            r.cursor.toLong()
        )
    }

    override fun reconnect(addr: String, pub: ByteArray, domain: String): Long =
        wrap { (if (usesDns()) core.reconnectDns() else core.reconnect(addr, pub, domain)).toLong() }

    override fun qrKind(uri: String): String = wrap {
        when (uniffi.dmsg_core.qrKind(uri)) {
            QrKind.JOIN -> "join"
            QrKind.CONTACT -> "contact"
        }
    }

    override fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean): String = wrap {
        when (uniffi.dmsg_core.storagePlan(hasLegacy, hasWrapped)) {
            StoragePlan.FRESH_INSTALL -> "fresh"
            StoragePlan.MIGRATE_LEGACY -> "migrate"
            StoragePlan.READY_WRAPPED -> "ready"
        }
    }
}

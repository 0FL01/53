package org.dmsg.client

import android.content.Context
import java.io.File
import java.util.concurrent.Executors
import uniffi.dmsg_core.AccountInfo
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.QrKind
import uniffi.dmsg_core.RegistrationPolicy
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.DialogsPage
import uniffi.dmsg_core.HistoryPage
import uniffi.dmsg_core.QrOutcome

/**
 * Owns the app-private DB path and the facade instance.
 * DB owner is Rust: Kotlin only passes the path string.
 * Missing native lib -> Unready facade (screens degrade, no crash).
 */
object Core {
    internal val storeLock = Any()
    private val commands = Executors.newSingleThreadExecutor { r -> Thread(r, "dmsg-ui-core").also { it.isDaemon = true } }
    fun dispatch(work: () -> Unit) { commands.execute(work) }
    fun dbFile(c: Context): File = File(c.filesDir, "core.db")

    fun facade(c: Context): DmsgFacade = synchronized(storeLock) {
        try {
            // System linker reads from the APK even when JNA cannot;
            // JNA reuses the already-loaded lib on first Native.load.
            System.loadLibrary("dmsg_core")
            val k = SecureStore.key(c)
            try { UniFfiFacade(dbFile(c).absolutePath, k, c.applicationContext) }
            finally { k.fill(0) }
        } catch (e: LinkageError) {
            Unready()
        }
    }

    private class Unready : DmsgFacade {
        private fun fail(): Nothing = throw DmsgError(R.string.error_native_unavailable, ErrorKind.NativeUnavailable)
        override fun isReady() = false
        override fun dnsProfile(): DnsProfile? = fail()
        override fun configureDns(code: String, resolvers: List<String>) = fail()
        override fun registrationPolicyDns(): RegistrationPolicy = fail()
        override fun signupDns(login: String, password: String, invitation: String?): AccountInfo = fail()
        override fun loginDns(login: String, password: String, expectedDevice: String?): LoginOutcome = fail()
        override fun dnsNetworkChanged(resolvers: List<String>) = fail()
        override fun dnsStop() {}
        override fun dnsStatus(): String = fail()
        override fun account(): AccountInfo = fail()
        override fun profilePreview(code: String): Pair<String, String> = fail()
        override fun myQr(): String = fail()
        override fun addQr(uri: String): QrOutcome = fail()
        override fun contactQrId(uri: String): String = fail()
        override fun inviteQr(uri: String): QrOutcome = fail()
        override fun request(id: String): String = fail()
        override fun accept(id: String) = fail()
        override fun block(id: String) = fail()
        override fun confirm(id: String) = fail()
        override fun get(id: String): Dialog? = fail()
        override fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?> = fail()
        override fun dialogsPage(cursor: String?, limit: Int): DialogsPage = fail()
        override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage = fail()
        override fun timelinePage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage = fail()
        override fun historyMessage(contactId: String, localId: Long): uniffi.dmsg_core.HistoryMessage = fail()
        override fun messageStatus(mid: String): DeliveryState? = fail()
        override fun setContactAlias(id: String, alias: String?) = fail()
        override fun markRead(id: String, throughLocalId: Long): Long = fail()
        override fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?> = fail()
        override fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?> = fail()
        override fun send(id: String, text: String): String = fail()
        override fun retry(): LongArray = fail()
        override fun fetch(): FetchRes = fail()
        override fun reconnect(): Long = fail()
        override fun qrKind(uri: String): QrKind = fail()
        override fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean): String =
            if (hasWrapped) "ready" else if (hasLegacy) "migrate" else "fresh"
    }
}

package org.dmsg.client

import android.content.Context
import java.io.File

/**
 * Owns the app-private DB path and the facade instance.
 * DB owner is Rust: Kotlin only passes the path string.
 * Missing native lib -> Unready facade (screens degrade, no crash).
 */
object Core {
    internal val storeLock = Any()
    fun dbFile(c: Context): File = File(c.filesDir, "core.db")

    fun facade(c: Context): DmsgFacade = synchronized(storeLock) {
        try {
            // System linker reads from the APK even when JNA cannot;
            // JNA reuses the already-loaded lib on first Native.load.
            System.loadLibrary("dmsg_core")
            val k = SecureStore.key(c)
            try { UniFfiFacade(dbFile(c).absolutePath, k, c.applicationContext) }
            finally { k.fill(0) }
        } catch (e: UnsatisfiedLinkError) {
            Unready(e.message ?: "no native lib")
        } catch (e: ExceptionInInitializerError) {
            Unready(e.message ?: "core init failed")
        }
    }

    private class Unready(val why: String) : DmsgFacade {
        private fun fail(): Nothing = throw DmsgError("core missing: $why")
        override fun isReady() = false
        override fun account(): Pair<Boolean, String?> = fail()
        override fun preview(qr: String): Pair<String, String> = fail()
        override fun enrol(qr: String, addr: String, pinDer: ByteArray?): String = fail()
        override fun myQr(): String = fail()
        override fun addQr(uri: String): String = fail()
        override fun request(id: String): String = fail()
        override fun accept(id: String) = fail()
        override fun block(id: String) = fail()
        override fun confirm(id: String) = fail()
        override fun get(id: String): Dialog? = fail()
        override fun contacts(cursor: String?, limit: Int): Pair<List<Dialog>, String?> = fail()
        override fun inbox(cursor: Long, limit: Int): Pair<List<Msg>, Long?> = fail()
        override fun outbox(cursor: Long, limit: Int): Pair<List<OutRow>, Long?> = fail()
        override fun send(addr: String, pub: ByteArray, domain: String, id: String, text: String): String = fail()
        override fun retry(addr: String, pub: ByteArray, domain: String): LongArray = fail()
        override fun fetch(addr: String, pub: ByteArray, domain: String): FetchRes = fail()
        override fun reconnect(addr: String, pub: ByteArray, domain: String): Long = fail()
        override fun qrKind(uri: String): String {
            // Pure routing works without the lib (mirror of qr_kind prefix step).
            return QrGate.route(uri).getOrThrow()
        }
        override fun storagePlan(hasLegacy: Boolean, hasWrapped: Boolean): String =
            if (hasWrapped) "ready" else if (hasLegacy) "migrate" else "fresh"
    }
}

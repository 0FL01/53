package org.dmsg.client

import android.content.Context
import android.os.Handler
import android.os.Looper
import java.util.concurrent.ConcurrentHashMap

internal data class VoiceTransferUi(val transferred: UInt = 0u, val total: UInt = 0u,
    val active: Boolean = false, val complete: Boolean = false, val errorRes: Int? = null)

/** App-scoped window-one bulk worker. UI/FGS only wake it; control dispatch is independent. */
internal object VoiceTransferCoordinator {
    private val lock = Any()
    private val foreground = mutableSetOf<Any>()
    private val listeners = mutableMapOf<Any, (VoiceKey) -> Unit>()
    private val downloads = linkedSetOf<VoiceKey>()
    private val states = ConcurrentHashMap<VoiceKey, VoiceTransferUi>()
    private val main = Handler(Looper.getMainLooper())
    private var app: Context? = null
    private var serviceEnabled = false
    @Volatile private var activeTransfer: VoiceTransferHandle? = null
    @Volatile private var activeKey: VoiceKey? = null
    @Volatile private var activeDownload = false
    private val worker = SingleWorker(::loop)
    fun state(key: VoiceKey) = states[key] ?: VoiceTransferUi()
    fun foreground(context: Context, owner: Any, visible: Boolean, listener: ((VoiceKey) -> Unit)? = null) {
        synchronized(lock) {
            app = context.applicationContext
            if (visible) { foreground.add(owner); if (listener != null) listeners[owner] = listener }
            else { foreground.remove(owner); listeners.remove(owner) }
            updateWanted()
        }
    }
    fun service(context: Context, enabled: Boolean) {
        synchronized(lock) { app = context.applicationContext; serviceEnabled = enabled; updateWanted() }
    }
    private fun updateWanted() {
        if (foreground.isNotEmpty() || serviceEnabled) { worker.start(); worker.wake() }
        else { worker.stop(); activeTransfer?.cancel() }
    }
    fun wake() = worker.wake()
    fun download(row: uniffi.dmsg_core.HistoryMessage) {
        if (!messageVisible(row) || row.kind != uniffi.dmsg_core.MessageKind.VOICE || row.voice?.downloaded == true) return
        val key = VoiceKey(row)
        synchronized(lock) { if (downloads.size < 16) downloads.add(key) }
        publish(key, state(key).copy(active = true, errorRes = null)); wake()
    }
    /** Local hiding a queued own note deliberately does not cancel its upload. */
    fun deleted(key: VoiceKey) {
        synchronized(lock) { downloads.remove(key) }
        if (activeDownload && activeKey == key) activeTransfer?.cancel()
        states.remove(key)
    }
    private fun publish(key: VoiceKey, value: VoiceTransferUi) {
        states[key] = value
        main.post { synchronized(lock) { listeners.values.toList() }.forEach { it(key) } }
    }
    /** FINISH uploads the blob, not its E2E message. Reuse the existing control lane. */
    private fun retrySaved(context: Context, key: VoiceKey? = null) {
        Core.dispatch {
            runCatching { Core.facade(context).retry() }.onFailure { error ->
                key?.let { publish(it, state(it).copy(active = false, errorRes = humanErrorRes(error))) }
            }
        }
    }
    private fun loop() {
        val context = synchronized(lock) { requireNotNull(app) }
        // A previous process may have committed FINISH before sending the manifest.
        retrySaved(context)
        var facade: DmsgFacade? = null
        while (worker.active) {
            var key: VoiceKey? = null
            var download = false
            try {
                val f = facade ?: Core.facade(context).also { facade = it }
                val requested = synchronized(lock) { downloads.firstOrNull() }
                download = requested != null
                val row = if (requested != null) f.historyMessage(requested.contactId, requested.localId)
                    else f.pendingVoiceUpload()
                if (row == null) { if (!worker.awaitNext(3_000)) break; continue }
                key = VoiceKey(row)
                if (requested != null && (!requested.matches(row) || row.voice?.downloaded == true)) {
                    synchronized(lock) { downloads.remove(requested) }; continue
                }
                if (!worker.active) break
                val handle = f.prepareVoiceTransfer(row.contactId, row.localId, download)
                activeDownload = download; activeKey = key; activeTransfer = handle
                try {
                    while (worker.active) {
                        if (download && !synchronized(lock) { key in downloads }) break
                        handle.advance() // Blocking network, explicitly OUTSIDE Core.storeLock.
                        if (!worker.active) break
                        if (download && (!synchronized(lock) { key in downloads } || !key.matches(f.historyMessage(key.contactId, key.localId)))) break
                        val progress = handle.commit() // Short durable commit.
                        publish(key, VoiceTransferUi(progress.transferred, progress.total, !progress.complete, progress.complete))
                        if (progress.complete) {
                            synchronized(lock) { downloads.remove(key) }
                            if (!download) retrySaved(context, key)
                            break
                        }
                    }
                } finally {
                    handle.cancel(); activeTransfer = null; activeKey = null; activeDownload = false
                }
            } catch (e: Exception) {
                key?.let { current ->
                    if (!download || synchronized(lock) { current in downloads })
                        publish(current, state(current).copy(active = false, errorRes = if (worker.active) humanErrorRes(e) else null))
                }
                if (!worker.awaitNext(5_000)) break
            }
            if (key != null && !worker.active && (!download || synchronized(lock) { key in downloads })) publish(key, state(key).copy(active = false))
            // One step at a time, no speculative multi-chunk window or control work here.
        }
    }
}

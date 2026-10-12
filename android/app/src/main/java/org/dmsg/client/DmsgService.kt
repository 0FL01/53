package org.dmsg.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.os.IBinder
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.app.Person
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import uniffi.dmsg_core.CallDirection
import uniffi.dmsg_core.CallPhase

/**
 * Single always-on foreground service (K4 audit).
 * No second FGS mode: "Economy" (Prefs.economy) only changes the poll
 * interval. Notifications are local only — no FCM anywhere in the app.
 * Doze/force-stop limits are disclosed, not masked (gate checklist).
 */
class DmsgService : Service() {
    private lateinit var calls: CallSessionOwner
    private val main = Handler(Looper.getMainLooper())
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var connectivityWanted = false
    private var stoppingLink = false
    private val networkEvents = java.util.concurrent.Executors.newSingleThreadExecutor()
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        private fun changed() {
            if (!networkEvents.isShutdown) {
                // Cancel before queuing DB work: an earlier event may be waiting on storeLock.
                Worker.observeNetwork()
                try { networkEvents.execute { Worker.networkChanged() } }
                catch (_: java.util.concurrent.RejectedExecutionException) { /* destroying */ }
            }
        }
        override fun onAvailable(network: Network) = changed()
        override fun onLost(network: Network) = changed()
        override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) = changed()
    }

    companion object {
        const val CH = "dmsg-link"
        const val ID = 53
        const val ACTION_STOP = "org.dmsg.client.STOP"
        const val ACTION_DIAL = "org.dmsg.client.CALL_DIAL"
        const val ACTION_END_CALL = "org.dmsg.client.CALL_END"
        const val ACTION_REJECT_CALL = "org.dmsg.client.CALL_REJECT"
        private var instance: DmsgService? = null
        internal fun callOwner(): CallSessionOwner? = instance?.let { if (it::calls.isInitialized) it.calls else null }
        internal fun modeChanged() { instance?.main?.post { instance?.syncListener() } }
        const val POLL_NORMAL_MS = 15_000L
        const val POLL_ECONOMY_MS = 300_000L
        private const val TAG = "DmsgService"
        private val facts = ConnectionFacts()
        fun connectionState(): ConnectionUiState = facts.snapshot()

        fun start(c: Context) {
            val app = c.applicationContext
            Core.dispatch {
                try {
                    val f = Core.facade(app)
                    if (f.account().authenticated && f.dnsProfile() != null) {
                        Worker.prepare(f)
                        app.startForegroundService(Intent(app, DmsgService::class.java))
                    } else recordFailure(ErrorKind.NotAuthenticated)
                } catch (e: Exception) { recordFailure((e as? DmsgError)?.kind ?: ErrorKind.Other) }
            }
        }

        fun stop(c: Context) {
            val service = instance
            if (service == null) { Worker.stop(); c.stopService(Intent(c, DmsgService::class.java)) }
            else service.main.post { service.stopLink() }
        }

        private fun recordFailure(kind: ErrorKind) = facts.finish(facts.begin(), System.currentTimeMillis(), kind)

        /** User-requested foreground check uses the same DNS-only operations and outcome facts. */
        fun check(f: DmsgFacade, keepGoing: () -> Boolean = { true }): FetchRes {
            val stamp = facts.begin()
            fun ensureActive() { if (!keepGoing()) throw InterruptedException("DNS poll stopped") }
            try {
                ensureActive()
                f.reconnect()
                facts.success(stamp, System.currentTimeMillis())
                ensureActive()
                val received = f.fetch()
                facts.success(stamp, System.currentTimeMillis())
                ensureActive()
                f.retry()
                facts.finish(stamp, System.currentTimeMillis(), null)
                return received
            } catch (e: Exception) {
                facts.finish(stamp, System.currentTimeMillis(), (e as? DmsgError)?.kind ?: ErrorKind.Other)
                throw e
            }
        }

        fun running(c: Context): Boolean = Worker.running
        internal fun wakeAfterNetworkApplied() = Worker.wake()

        private fun Context.updateChannel() {
            getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CH, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW)
            )
        }

        private fun Context.buildCountNotif(total: Int): Notification {
            updateChannel()
            val open = PendingIntent.getActivity(
                this, 0, Intent(this, MainActivity::class.java),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
            // No history / filenames in notifications (ARCH S14).
            return NotificationCompat.Builder(this, CH)
                .setContentTitle(getString(R.string.fgs_title))
                .setContentText(getString(R.string.fgs_body, total))
                .setSmallIcon(R.drawable.ic_message)
                .setContentIntent(open)
                .setOngoing(true)
                .build()
        }

    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        calls = CallSessionOwner(this, ::refreshForeground, Worker::pauseMessages) {
            syncListener()
            if (!connectivityWanted && !calls.busy && !stoppingLink) stopLink()
        }
        CallUi.mutable.value = calls.state
        getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(networkCallback)
        updateChannel()
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        updateChannel()
        refreshForeground()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopLink()
            return START_NOT_STICKY
        }
        when (intent?.action) {
            ACTION_DIAL -> {
                refreshForeground()
                calls.dial(intent.getStringExtra("peer").orEmpty())
            }
            ACTION_END_CALL -> {
                calls.end(intent.getStringExtra("callId").orEmpty(), intent.getLongExtra("epoch", -1))
                if (!connectivityWanted && !calls.busy) stopSelf()
            }
            ACTION_REJECT_CALL -> {
                calls.reject(intent.getStringExtra("callId").orEmpty(), intent.getLongExtra("epoch", -1))
                if (!connectivityWanted && !calls.busy) stopSelf()
            }
            else -> {
                connectivityWanted = true
                refreshForeground()
                Worker.start(this)
                syncListener()
            }
        }
        return if (connectivityWanted) START_STICKY else START_NOT_STICKY
    }

    override fun onDestroy() {
        getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(networkCallback)
        networkEvents.shutdownNow()
        if (instance === this) instance = null
        calls.destroy { Worker.stop() }
        super.onDestroy()
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        // Never rely on a service category to override an OS stop requirement.
        stopLink()
    }

    private fun stopLink() {
        if (stoppingLink) return
        stoppingLink = true; connectivityWanted = false
        serviceScope.launch {
            calls.stop()
            Worker.stop()
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
    }
    private fun syncListener() {
        Worker.facadeSnapshot()?.let { calls.listener(it, connectivityWanted && Worker.running &&
            !Prefs.economy(this) && connectionState().lastSuccessAt != null && !stoppingLink) }
        refreshForeground()
    }
    private fun refreshForeground() {
        if (!::calls.isInitialized || stoppingLink) return
        val state = calls.state
        val phone = state.call != null && (state.call.busy() || state.tearingDown)
        val microphone = state.telecomActive && (state.tearingDown || state.call?.phase in setOf(CallPhase.CONNECTING, CallPhase.ACTIVE))
        var types = 0
        if (Build.VERSION.SDK_INT >= 34 && (connectivityWanted || !phone)) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
        if (Build.VERSION.SDK_INT >= 29 && phone) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL
        if (Build.VERSION.SDK_INT >= 30 && microphone) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        try {
            ServiceCompat.startForeground(this, ID,
                if (phone) callNotification(state) else buildCountNotif(Worker.countSnapshot()), types)
        } catch (_: SecurityException) {
            calls.audioUnavailable()
        }
    }
    private fun callAction(state: CallUiState, action: String, activity: Boolean = false): PendingIntent {
        val call = requireNotNull(state.call)
        val intent = Intent(this, if (activity) CallActivity::class.java else DmsgService::class.java)
            .setAction(action).setData(android.net.Uri.Builder().scheme("dmsg-call").authority(packageName)
                .appendPath(call.callId).appendPath(state.epoch.toString()).appendPath(action).build())
            .putExtra("callId", call.callId).putExtra("epoch", state.epoch)
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        return if (activity) PendingIntent.getActivity(this, 0, intent, flags)
            else PendingIntent.getService(this, 0, intent, flags)
    }
    private fun callNotification(state: CallUiState): Notification {
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel("dmsg-call", getString(R.string.call_channel), NotificationManager.IMPORTANCE_HIGH)
                .apply { lockscreenVisibility = Notification.VISIBILITY_PRIVATE })
        val call = requireNotNull(state.call)
        val person = Person.Builder().setName(getString(R.string.call_private_caller)).setImportant(true).build()
        val incoming = call.direction == CallDirection.INCOMING && call.phase == CallPhase.RINGING && !state.tearingDown
        val open = callAction(state, "org.dmsg.client.CALL_OPEN", activity = true)
        val style = if (incoming) NotificationCompat.CallStyle.forIncomingCall(person,
            callAction(state, ACTION_REJECT_CALL), callAction(state, CallActivity.ACTION_ANSWER, activity = true))
        else NotificationCompat.CallStyle.forOngoingCall(person, callAction(state, ACTION_END_CALL))
        return NotificationCompat.Builder(this, "dmsg-call").setSmallIcon(R.drawable.ic_call)
            .setContentTitle(getString(R.string.app_name)).setContentText(getString(callPhaseRes(call)))
            .setStyle(style).setCategory(NotificationCompat.CATEGORY_CALL).setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setContentIntent(open).setOngoing(true).setOnlyAlertOnce(!incoming).build()
    }

    /** Poll loop: reconnect (refill) -> fetch -> retry. Backoff on failure. */
    private object Worker {
        private var app: Context? = null
        @Volatile private var facade: DmsgFacade? = null
        private val worker = SingleWorker { loop(requireNotNull(app)) }
        val running: Boolean get() = worker.running
        @Volatile private var total = 0
        @Volatile private var messagesPaused = false
        fun facadeSnapshot() = facade
        fun pauseMessages(value: Boolean) { messagesPaused = value; worker.wake() }
        fun countSnapshot(): Int = total

        fun prepare(f: DmsgFacade) { facade = f }

        fun start(c: Context) {
            app = c.applicationContext
            facts.enabled(true)
            VoiceTransferCoordinator.service(c, true)
            worker.start()
        }

        private fun loop(app: Context) {
            var backoff = 5_000L
            total = 0
            while (worker.active) {
                if (messagesPaused) { if (!worker.awaitNext(1_000)) break; continue }
                try {
                    val n = pollOnce(app)
                    total += n
                    backoff = 5_000L
                    instance?.main?.post { instance?.syncListener() }
                    // Gate-observable poll outcome (no secrets: counts only).
                    Log.d(TAG, "poll ok n=$n total=$total")
                } catch (e: Exception) {
                    // Fatal VM/Linkage errors are not transient network errors.
                    Log.w(TAG, "poll fail: ${e.javaClass.simpleName}")
                    if (worker.active) recordFailure((e as? DmsgError)?.kind ?: ErrorKind.Other)
                    backoff = minOf(backoff * 2, 120_000L)
                }
                val interval = if (Prefs.economy(app)) POLL_ECONOMY_MS else POLL_NORMAL_MS
                if (!worker.awaitNext(maxOf(interval, backoff))) break
            }
        }

        fun stop() {
            facts.enabled(false)
            app?.let { VoiceTransferCoordinator.service(it, false) }
            worker.stop()
            try { facade?.dnsStop() } catch (_: Exception) { /* no secret diagnostics */ }
        }

        fun observeNetwork() {
            if (!worker.running) return
            try { (facade as? UniFfiFacade)?.observeDnsNetwork() }
            catch (e: Exception) { Log.d(TAG, "network observation: ${e.javaClass.simpleName}") }
        }

        fun networkChanged() {
            val f = facade as? UniFfiFacade ?: return
            try {
                f.refreshDnsNetwork { worker.running }
            } catch (e: Exception) {
                Log.d(TAG, "network transition: ${e.javaClass.simpleName}")
            }
        }

        fun wake() { worker.wake(); VoiceTransferCoordinator.wake() }

        private fun pollOnce(app: Context): Int {
            val f = facade ?: Core.facade(app).also { facade = it }
            if (!f.isReady()) throw DmsgError(R.string.error_native_unavailable, ErrorKind.NativeUnavailable)
            if (!f.account().authenticated) throw DmsgError(R.string.error_sign_in_required, ErrorKind.NotAuthenticated)
            if (f.dnsProfile() == null) throw DmsgError(R.string.error_server_required, ErrorKind.InvalidInput)
            val rep = check(f) { worker.active && !messagesPaused }
            VoiceTransferCoordinator.wake()
            Log.d(TAG, "receive pending=${rep.skipped.getOrElse(0) { 0 }} blocked=${rep.skipped.getOrElse(1) { 0 }} undecryptable=${rep.skipped.getOrElse(2) { 0 }} mismatch=${rep.skipped.getOrElse(3) { 0 }}")
            return rep.received.size
        }
    }
}

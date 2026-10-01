package org.dmsg.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat

/**
 * Single always-on foreground service (K4 audit).
 * No second FGS mode: "Economy" (Prefs.economy) only changes the poll
 * interval. Notifications are local only — no FCM anywhere in the app.
 * Doze/force-stop limits are disclosed, not masked (gate checklist).
 */
class DmsgService : Service() {
    private val networkEvents = java.util.concurrent.Executors.newSingleThreadExecutor()
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        private fun changed() {
            if (!networkEvents.isShutdown) {
                try { networkEvents.execute { Worker.networkChanged(applicationContext) } }
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
            Worker.stop() // immediate native cancellation, independent of the UI command/store lock
            c.stopService(Intent(c, DmsgService::class.java))
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
            } finally {
                if (!keepGoing()) f.dnsStop()
            }
        }

        fun running(c: Context): Boolean = Worker.running
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(networkCallback)
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CH, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW)
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            Worker.stop()
            stopSelf()
            return START_NOT_STICKY
        }
        val notif = buildNotif(0)
        ServiceCompat.startForeground(
            this, ID, notif,
            if (android.os.Build.VERSION.SDK_INT >= 34)
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        )
        Worker.start(this)
        return START_STICKY
    }

    override fun onDestroy() {
        getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(networkCallback)
        networkEvents.shutdownNow()
        Worker.stop()
        super.onDestroy()
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        // Never rely on a service category to override an OS stop requirement.
        Worker.stop()
        stopSelf()
    }

    private fun buildNotif(newCount: Int): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        // No history / filenames in notifications (ARCH S14).
        return NotificationCompat.Builder(this, CH)
            .setContentTitle(getString(R.string.fgs_title))
            .setContentText(getString(R.string.fgs_body, newCount))
            .setSmallIcon(R.drawable.ic_message)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
    }

    /** Poll loop: reconnect (refill) -> fetch -> retry. Backoff on failure. */
    private object Worker {
        private var app: Context? = null
        @Volatile private var facade: DmsgFacade? = null
        private val worker = SingleWorker { loop(requireNotNull(app)) }
        val running: Boolean get() = worker.running

        fun prepare(f: DmsgFacade) { facade = f }

        fun start(c: Context) {
            app = c.applicationContext
            facts.enabled(true)
            worker.start()
        }

        private fun loop(app: Context) {
            var backoff = 5_000L
            var total = 0
            while (worker.running && !Thread.currentThread().isInterrupted) {
                try {
                    val n = pollOnce(app)
                    total += n
                    backoff = 5_000L
                    app.notify(app.buildCountNotif(total))
                    // Gate-observable poll outcome (no secrets: counts only).
                    Log.d(TAG, "poll ok n=$n total=$total")
                } catch (e: Exception) {
                    // Fatal VM/Linkage errors are not transient network errors.
                    Log.w(TAG, "poll fail: ${e.javaClass.simpleName}")
                    if (worker.running) recordFailure((e as? DmsgError)?.kind ?: ErrorKind.Other)
                    backoff = minOf(backoff * 2, 120_000L)
                }
                val interval = if (Prefs.economy(app)) POLL_ECONOMY_MS else POLL_NORMAL_MS
                try {
                    Thread.sleep(maxOf(interval, backoff))
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    break
                }
            }
        }

        fun stop() {
            facts.enabled(false)
            worker.stop()
            try { facade?.dnsStop() } catch (_: Exception) { /* no secret diagnostics */ }
        }

        fun networkChanged(app: Context) {
            val f = facade ?: return
            try {
                f.dnsStop() // cancellation does not wait for the store lock
                if (f.dnsProfile() != null) f.dnsNetworkChanged(DnsNetwork.resolvers(app))
            } catch (e: Exception) {
                Log.d(TAG, "network transition: ${e.javaClass.simpleName}")
            }
        }

        private fun pollOnce(app: Context): Int {
            val f = facade ?: Core.facade(app).also { facade = it }
            if (!f.isReady()) throw DmsgError("Ядро приложения недоступно", ErrorKind.NativeUnavailable)
            if (!f.account().authenticated) throw DmsgError("Сначала войдите в аккаунт", ErrorKind.NotAuthenticated)
            if (f.dnsProfile() == null) throw DmsgError("Сначала добавьте сервер", ErrorKind.InvalidInput)
            val rep = check(f) { worker.running && !Thread.currentThread().isInterrupted }
            return rep.received.size
        }

        private fun Context.buildCountNotif(total: Int): Notification {
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CH, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW)
            )
            return NotificationCompat.Builder(this, CH)
                .setContentTitle(getString(R.string.fgs_title))
                .setContentText(getString(R.string.fgs_body, total))
                .setSmallIcon(R.drawable.ic_message)
                .setOngoing(true)
                .build()
        }

        private fun Context.notify(n: Notification) {
            val nm = getSystemService(NotificationManager::class.java)
            try {
                nm.notify(ID, n)
            } catch (_: SecurityException) {
                // POST_NOTIFICATIONS denied: stay alive, stay silent.
            }
        }
    }
}

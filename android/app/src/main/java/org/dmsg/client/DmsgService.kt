package org.dmsg.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
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

    companion object {
        const val CH = "dmsg-link"
        const val ID = 53
        const val ACTION_STOP = "org.dmsg.client.STOP"
        const val POLL_NORMAL_MS = 15_000L
        const val POLL_ECONOMY_MS = 300_000L
        private const val TAG = "DmsgService"
        @Volatile private var lastPollMs = 0L
        @Volatile private var lastPollError = "ожидание опроса"

        fun pollStatus(): String = if (!Worker.running) "FGS выключен" else {
            val elapsed = (System.currentTimeMillis() - lastPollMs) / 1000
            if (lastPollMs == 0L) "FGS: $lastPollError (Doze может задерживать)"
            else "последний ответ ${elapsed}с назад; $lastPollError (Doze может задерживать)"
        }

        fun start(c: Context) {
            c.startForegroundService(Intent(c, DmsgService::class.java))
        }

        fun stop(c: Context) {
            c.stopService(Intent(c, DmsgService::class.java))
        }

        fun running(c: Context): Boolean = Worker.running
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CH, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW)
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        val notif = buildNotif(0)
        ServiceCompat.startForeground(
            this, ID, notif,
            if (android.os.Build.VERSION.SDK_INT >= 29)
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else 0
        )
        Worker.start(this)
        return START_STICKY
    }

    override fun onDestroy() {
        Worker.stop()
        super.onDestroy()
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
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
    }

    /** Poll loop: reconnect (refill) -> fetch -> retry. Backoff on failure. */
    private object Worker {
        private var app: Context? = null
        private val worker = SingleWorker { loop(requireNotNull(app)) }
        val running: Boolean get() = worker.running

        fun start(c: Context) {
            app = c.applicationContext
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
                    lastPollMs = System.currentTimeMillis()
                    lastPollError = "доступен"
                    app.notify(app.buildCountNotif(total))
                    // Gate-observable poll outcome (no secrets: counts only).
                    Log.d(TAG, "poll ok n=$n total=$total")
                } catch (e: Exception) {
                    // Fatal VM/Linkage errors are not transient network errors.
                    Log.w(TAG, "poll fail: ${e.javaClass.simpleName}: ${e.message}")
                    lastPollError = if (e is DmsgError) "ошибка: ${e.message}"
                        else "ошибка ${e.javaClass.simpleName}"
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
            worker.stop()
        }

        private fun pollOnce(app: Context): Int {
            val f = Core.facade(app)
            if (!f.isReady()) throw DmsgError("core is not ready")
            val addr = Prefs.addr(app)
            val pub = Prefs.serverPub(app)
            val domain = Prefs.domain(app)
            if (addr.isEmpty() || pub == null || domain.isEmpty()) {
                throw DmsgError("transport is not configured")
            }
            f.reconnect(addr, pub, domain)
            val rep = f.fetch(addr, pub, domain)
            f.retry(addr, pub, domain)
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
                .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
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

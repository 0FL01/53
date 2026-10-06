package org.dmsg.client

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Button
import android.widget.TextView
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationManagerCompat

/** Connection settings report actual DNS outcomes, independently of foreground-service ownership. */
class DiagnosticsActivity : DmsgActivity() {
    private val guard = UiGuard()
    private var active = false
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() { if (active) { showFacts(); handler.postDelayed(this, 1_000) } }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_diagnostics)
        NativeUi.back(this, getString(R.string.title_connectivity))
        findViewById<Button>(R.id.btn_economy).setOnClickListener {
            Prefs.setEconomy(this, !Prefs.economy(this)); showFacts()
        }
        findViewById<Button>(R.id.btn_fgs).setOnClickListener {
            if (DmsgService.running(this)) DmsgService.stop(this) else {
                if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED)
                    ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
                DmsgService.start(this)
            }
            showFacts()
        }
        findViewById<Button>(R.id.btn_reconnect).setOnClickListener {
            val stamp = guard.begin() ?: return@setOnClickListener
            showFacts()
            Core.dispatch {
                val result = runCatching { DmsgService.check(Core.facade(applicationContext)) }
                runOnUiThread {
                    if (!active || !guard.finish(stamp)) return@runOnUiThread
                    findViewById<TextView>(R.id.stats).text = result.fold({ getString(R.string.dns_check_complete, it.received.size) }, { humanError(resources, it) })
                    showFacts()
                }
            }
        }
        findViewById<Button>(R.id.btn_advanced).setOnClickListener { startActivity(Intent(this, AdvancedProfileActivity::class.java)) }
    }
    override fun onResume() {
        super.onResume(); active = true; handler.post(ticker)
        val stamp = guard.generation
        Core.dispatch {
            val result = runCatching { Core.facade(applicationContext).account() }
            runOnUiThread { if (active && guard.accepts(stamp)) findViewById<TextView>(R.id.account).text = result.fold({
                if (it.authenticated) getString(R.string.device_authenticated, it.contactId) else getString(R.string.connection_not_authenticated)
            }, { humanError(resources, it) }) }
        }
    }
    override fun onPause() { active = false; guard.stop(); handler.removeCallbacks(ticker); super.onPause() }
    private fun showFacts() {
        val facts = DmsgService.connectionState()
        findViewById<TextView>(R.id.fgs).text = listOf(
            getString(if (facts.serviceEnabled) R.string.connection_enabled else R.string.connection_disabled),
            connectionLabel(resources, facts),
            getString(R.string.last_response, facts.lastSuccessAt?.let(::localTime) ?: getString(R.string.no_response)),
            getString(if (NotificationManagerCompat.from(this).areNotificationsEnabled()) R.string.notifications_allowed else R.string.notifications_disabled)
        ).joinToString("\n")
        findViewById<Button>(R.id.btn_fgs).setText(if (facts.serviceEnabled) R.string.disable_connectivity else R.string.enable_connectivity)
        findViewById<Button>(R.id.btn_economy).setText(if (Prefs.economy(this)) R.string.economy_on else R.string.economy_off)
        findViewById<Button>(R.id.btn_reconnect).isEnabled = !guard.pending && !facts.pollInFlight
    }
}

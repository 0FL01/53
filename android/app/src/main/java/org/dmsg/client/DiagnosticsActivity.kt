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
        NativeUi.back(this, "Связь")
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
                    findViewById<TextView>(R.id.stats).text = result.fold({ "DNS-проверка завершена. Получено локально: ${it.received.size}. Очередь повторена прежними ID." }, ::humanError)
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
                if (it.authenticated) "Устройство авторизовано · ${it.contactId}" else "Сначала войдите в аккаунт"
            }, ::humanError) }
        }
    }
    override fun onPause() { active = false; guard.stop(); handler.removeCallbacks(ticker); super.onPause() }
    private fun showFacts() {
        val facts = DmsgService.connectionState()
        findViewById<TextView>(R.id.fgs).text = listOf(
            if (facts.serviceEnabled) "Фоновая связь включена" else "Фоновая связь выключена",
            connectionLabel(facts),
            "Последний успешный ответ: ${facts.lastSuccessAt?.let(::localTime) ?: "ещё не получен"}",
            if (NotificationManagerCompat.from(this).areNotificationsEnabled()) "Уведомления разрешены" else "Уведомления выключены в настройках Android"
        ).joinToString("\n")
        findViewById<Button>(R.id.btn_fgs).text = if (facts.serviceEnabled) "Выключить фоновую связь" else "Включить фоновую связь"
        findViewById<Button>(R.id.btn_economy).text = if (Prefs.economy(this)) "Экономия: включена" else "Экономия: выключена"
        findViewById<Button>(R.id.btn_reconnect).isEnabled = !guard.pending && !facts.pollInFlight
    }
}

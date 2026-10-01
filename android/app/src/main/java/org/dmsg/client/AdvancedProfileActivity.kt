package org.dmsg.client

import android.os.Bundle
import android.widget.TextView

/** Public pinned data only. Raw native debug state is never treated as health. */
class AdvancedProfileActivity : DmsgActivity() {
    private var active = false
    private val guard = UiGuard()
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); setContentView(R.layout.activity_advanced_profile)
        NativeUi.back(this, "Профиль сервера")
    }
    override fun onResume() {
        super.onResume(); active = true
        val stamp = guard.begin() ?: return
        Core.dispatch {
            val result = runCatching { Core.facade(applicationContext).dnsProfile() }
            runOnUiThread {
                if (!active || !guard.finish(stamp)) return@runOnUiThread
                findViewById<TextView>(R.id.profile_details).text = result.fold({ p ->
                    p?.let { "Домен: ${it.domain}\n\nОтпечаток полного сертификата:\n${it.fingerprint}\n\nПубличный Noise-ключ:\n${Prefs.bytesToHex(it.pub)}\n\nРезолверы текущей сети:\n${it.resolvers.joinToString("\n")}\n\nПрофиль закреплён ядром. Проверка сертификата обязательна; вход передаётся только после pinned handshake." }
                        ?: "Сервер пока не выбран. Вставьте публичный код на первом экране."
                }, ::humanError)
            }
        }
    }
    override fun onPause() { active = false; guard.stop(); super.onPause() }
}

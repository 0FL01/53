package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.TextView

/** Read-only trusted DNS profile and background availability. */
class DiagnosticsActivity : DmsgActivity() {
    private lateinit var account: TextView
    private lateinit var fgs: TextView
    private lateinit var stats: TextView
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_diagnostics)
        account = findViewById(R.id.account)
        fgs = findViewById(R.id.fgs)
        stats = findViewById(R.id.stats)
        findViewById<Button>(R.id.btn_economy).setOnClickListener {
            Prefs.setEconomy(this, !Prefs.economy(this)); show()
        }
        findViewById<Button>(R.id.btn_reconnect).setOnClickListener {
            Core.dispatch {
                val out = try { "Ключей загружено: ${Core.facade(this).reconnect()}" } catch (e: Exception) { humanError(e) }
                runOnUiThread { if (!isDestroyed) stats.text = out }
            }
        }
    }
    override fun onResume() { super.onResume(); show() }
    private fun show() {
        Core.dispatch {
            val result = runCatching {
                val f = Core.facade(this)
                val a = f.account()
                val p = f.dnsProfile()
                Pair("Вход: ${a.authenticated}; ID: ${a.contactId}",
                    "Экономия: ${Prefs.economy(this)}\nОчередь (страница): ${f.outbox(0, 100).first.size}\n" +
                        (p?.let { "Домен: ${it.domain}\nОтпечаток сертификата: ${it.fingerprint}\nNoise key: ${Prefs.bytesToHex(it.pub)}\nDNS: ${f.dnsStatus()}\nРезолверы сети: ${it.resolvers.joinToString()}" } ?: "Сервер ещё не добавлен"))
            }
            runOnUiThread { if (!isDestroyed) {
                result.fold({ (a, s) -> account.text = a; stats.text = s }, { stats.text = humanError(it) })
                fgs.text = DmsgService.pollStatus() + " (Doze/force-stop могут прервать связь)"
            } }
        }
    }
}

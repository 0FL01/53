package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/** Diagnostics: account, FGS state, economy flag, outbox stats, refill. */
class DiagnosticsActivity : DmsgActivity() {
    private lateinit var account: TextView
    private lateinit var fgs: TextView
    private lateinit var stats: TextView
    private lateinit var addr: EditText
    private lateinit var domain: EditText
    private lateinit var pub: EditText

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_diagnostics)
        account = findViewById(R.id.account)
        fgs = findViewById(R.id.fgs)
        stats = findViewById(R.id.stats)
        addr = findViewById(R.id.profile_addr)
        domain = findViewById(R.id.profile_domain)
        pub = findViewById(R.id.profile_pub)
        addr.setText(Prefs.addr(this))
        domain.setText(Prefs.domain(this))
        pub.setText(Prefs.serverPub(this)?.let { Prefs.bytesToHex(it) } ?: "")
        findViewById<Button>(R.id.btn_save_profile).setOnClickListener {
            stats.text = try {
                Prefs.setTransport(this, addr.text.toString(), domain.text.toString(), pub.text.toString())
                "профиль сохранён (проверка Noise только при подключении)"
            } catch (e: DmsgError) { "error: ${e.message}" }
        }
        findViewById<Button>(R.id.btn_economy).setOnClickListener {
            Prefs.setEconomy(this, !Prefs.economy(this))
            show()
        }
        findViewById<Button>(R.id.btn_reconnect).setOnClickListener { reconnect() }
    }

    override fun onResume() {
        super.onResume()
        show()
    }

    private fun show() {
        Thread {
            var a = ""
            var s = ""
            try {
                val f = Core.facade(this)
                val (enrolled, id) = f.account()
                a = "enrolled=$enrolled id=$id"
                val (rows, _) = f.outbox(0, 100)
                s = "outbox pending=${rows.size} economy=${Prefs.economy(this)}"
            } catch (e: Exception) {
                a = "error: ${e.message}"
            }
            runOnUiThread {
                account.text = a
                fgs.text = DmsgService.pollStatus() + " (Doze/force-stop limits apply)"
                stats.text = s
            }
        }.start()
    }

    private fun reconnect() {
        Thread {
            val out = try {
                val a = Prefs.addr(this)
                val p = Prefs.serverPub(this)
                val d = Prefs.domain(this)
                if (a.isEmpty() || p == null || d.isEmpty()) "заполните addr/domain/server_pub"
                else "prekeys=${Core.facade(this).reconnect(a, p, d)}"
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { stats.text = out }
        }.start()
    }
}

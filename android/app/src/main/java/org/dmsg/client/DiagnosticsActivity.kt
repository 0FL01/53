package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/** Diagnostics: account, FGS state, economy flag, outbox stats, refill. */
class DiagnosticsActivity : AppCompatActivity() {
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
                fgs.text = "fgs=${DmsgService.running(this)} (Doze/force-stop limits apply, see gate)"
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

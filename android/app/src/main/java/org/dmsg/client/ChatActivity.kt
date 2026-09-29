package org.dmsg.client

import android.os.Bundle
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/** Chat: paginated history + composer (send is a blocking facade call). */
class ChatActivity : AppCompatActivity() {
    private lateinit var peer: TextView
    private lateinit var list: ListView
    private lateinit var composer: EditText
    private lateinit var info: TextView
    private val rows = mutableListOf<String>()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_chat)
        val id = intent.getStringExtra("peer") ?: ""
        peer = findViewById(R.id.peer)
        list = findViewById(R.id.messages)
        composer = findViewById(R.id.composer)
        info = findViewById(R.id.info)
        peer.text = id
        list.adapter = ArrayAdapter(this, android.R.layout.simple_list_item_1, rows)
        findViewById<Button>(R.id.btn_send).setOnClickListener { send(id) }
        findViewById<Button>(R.id.btn_retry).setOnClickListener { retry() }
    }

    override fun onResume() {
        super.onResume()
        load(intent.getStringExtra("peer") ?: "")
    }

    private fun transport(): Triple<String, ByteArray, String>? {
        val a = Prefs.addr(this)
        val p = Prefs.serverPub(this)
        val d = Prefs.domain(this)
        if (a.isEmpty() || p == null || d.isEmpty()) return null
        return Triple(a, p, d)
    }

    private fun load(id: String) {
        Thread {
            val out = try {
                val f = Core.facade(this)
                rows.clear()
                var cursor = 0L
                repeat(10) {
                    val (page, next) = f.inbox(cursor, 50)
                    rows.addAll(page.filter { it.contactId == id }.map { format(it) })
                    cursor = next ?: return@repeat
                }
                "${rows.size} сообщений"
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread {
                info.text = out
                (list.adapter as ArrayAdapter<*>).notifyDataSetChanged()
            }
        }.start()
    }

    private fun format(m: Msg) = "[${m.seq}] ${m.contactId}: ${m.text}"

    private fun send(id: String) {
        val text = composer.text.toString()
        val t = transport()
        Thread {
            val out = try {
                val f = Core.facade(this)
                if (t == null) "заполните addr/domain/server_pub в диагностике"
                else {
                    val mid = f.send(t.first, t.second, t.third, id, text)
                    composer.text.clear()
                    "sent $mid"
                }
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { info.text = out; load(id) }
        }.start()
    }

    private fun retry() {
        val t = transport()
        Thread {
            val out = try {
                val f = Core.facade(this)
                if (t == null) "нет транспорта" else {
                    val r = f.retry(t.first, t.second, t.third)
                    "retry sent=${r[0]} accepted=${r[1]} delivered=${r[2]} skipped=${r[3]}"
                }
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { info.text = out }
        }.start()
    }
}

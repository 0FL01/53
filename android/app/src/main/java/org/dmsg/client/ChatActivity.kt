package org.dmsg.client

import android.os.Bundle
import android.widget.AbsListView
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.ListView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/** Chat: paginated history + composer (send is a blocking facade call). */
class ChatActivity : DmsgActivity() {
    private lateinit var peer: TextView
    private lateinit var list: ListView
    private lateinit var composer: EditText
    private lateinit var info: TextView
    private val rows = mutableListOf<String>()
    private val paging = InboxPaging()
    private var uiToken = 0L
    private var loadInfo = ""
    private var actionInfo = ""

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
        list.setOnScrollListener(object : AbsListView.OnScrollListener {
            override fun onScrollStateChanged(view: AbsListView?, scrollState: Int) {}

            override fun onScroll(
                view: AbsListView?, firstVisibleItem: Int, visibleItemCount: Int, totalItemCount: Int
            ) {
                if (visibleItemCount > 0 && firstVisibleItem + visibleItemCount >= totalItemCount - 5) {
                    loadNext(id)
                }
            }
        })
        findViewById<Button>(R.id.btn_send).setOnClickListener { send(id) }
        findViewById<Button>(R.id.btn_retry).setOnClickListener { retry() }
    }

    override fun onResume() {
        super.onResume()
        load(intent.getStringExtra("peer") ?: "")
    }

    override fun onPause() {
        uiToken++
        paging.stop()
        super.onPause()
    }

    private fun transport(): Triple<String, ByteArray, String>? {
        val a = Prefs.addr(this)
        val p = Prefs.serverPub(this)
        val d = Prefs.domain(this)
        if (a.isEmpty() || p == null || d.isEmpty()) return null
        return Triple(a, p, d)
    }

    private fun load(id: String) {
        paging.reset()
        rows.clear()
        (list.adapter as ArrayAdapter<*>).notifyDataSetChanged()
        loadInfo = "0 сообщений"
        updateInfo()
        loadNext(id)
    }

    private fun loadNext(id: String) {
        if (isFinishing || isDestroyed) return
        val request = paging.begin() ?: return
        Thread {
            val result: Result<Pair<List<String>, Long?>> = try {
                val f = Core.facade(applicationContext)
                val (page, next) = f.inbox(request.cursor, 50)
                Result.success(Pair(page.filter { it.contactId == id }.map { format(it) }, next))
            } catch (e: Exception) {
                Result.failure(e)
            }
            runOnUiThread {
                if (isFinishing || isDestroyed || !paging.complete(request, result.getOrNull()?.second)) {
                    return@runOnUiThread
                }
                result.fold(
                    onSuccess = { (page, _) ->
                        rows.addAll(page)
                        (list.adapter as ArrayAdapter<*>).notifyDataSetChanged()
                        loadInfo = "${rows.size} сообщений"
                        updateInfo()
                        // Empty peer-filtered pages must still advance the global inbox cursor.
                        list.post {
                            if (rows.isEmpty() || list.lastVisiblePosition >= rows.size - 5) loadNext(id)
                        }
                    },
                    onFailure = {
                        // Suspend automatic paging until the next reload, rather than spin on an error.
                        loadInfo = "error: ${it.message}"
                        updateInfo()
                    }
                )
            }
        }.start()
    }

    private fun updateInfo() {
        info.text = listOf(actionInfo, loadInfo).filter { it.isNotEmpty() }.joinToString("\n")
    }

    private fun format(m: Msg) = "[${m.seq}] ${m.contactId}: ${m.text}"

    private fun send(id: String) {
        val text = composer.text.toString()
        val t = transport()
        val token = uiToken
        Thread {
            var sent = false
            val out = try {
                val f = Core.facade(applicationContext)
                if (t == null) "заполните addr/domain/server_pub в диагностике"
                else {
                    val mid = f.send(t.first, t.second, t.third, id, text)
                    sent = true
                    var after = 0L
                    var state: String? = null
                    do {
                        val (page, next) = f.outbox(after, 50)
                        state = page.firstOrNull { it.mid == mid }?.status
                        after = next ?: break
                    } while (state == null)
                    "${state ?: "delivered"} $mid"
                }
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread {
                if (token != uiToken || isFinishing || isDestroyed) return@runOnUiThread
                if (sent && composer.text.toString() == text) composer.text.clear()
                actionInfo = out
                updateInfo()
                if (sent) load(id)
            }
        }.start()
    }

    private fun retry() {
        val t = transport()
        val token = uiToken
        Thread {
            val out = try {
                val f = Core.facade(applicationContext)
                if (t == null) "нет транспорта" else {
                    val r = f.retry(t.first, t.second, t.third)
                    "retry sent=${r[0]} accepted=${r[1]} delivered=${r[2]} skipped=${r[3]}"
                }
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread {
                if (token != uiToken || isFinishing || isDestroyed) return@runOnUiThread
                actionInfo = out
                updateInfo()
            }
        }.start()
    }
}

/** Main-thread pagination state: one request at a time, stale reload/lifecycle results ignored. */
internal class InboxPaging {
    data class Request(val generation: Long, val cursor: Long)

    private var generation = 0L
    private var cursor: Long? = null
    private var inFlight: Request? = null

    fun reset() {
        generation++
        cursor = 0L
        inFlight = null
    }

    fun stop() {
        generation++
        cursor = null
        inFlight = null
    }

    fun begin(): Request? {
        if (inFlight != null) return null
        val after = cursor ?: return null
        return Request(generation, after).also { inFlight = it }
    }

    fun complete(request: Request, next: Long?): Boolean {
        if (inFlight != request) return false
        cursor = next
        inFlight = null
        return true
    }
}

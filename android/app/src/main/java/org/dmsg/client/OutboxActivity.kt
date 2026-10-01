package org.dmsg.client

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.AbsListView
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.ListView
import android.widget.TextView

/** Retry means core.retryDns(): old ciphertext/message IDs, never a new plaintext send. */
class OutboxActivity : DmsgActivity() {
    private val rows = mutableListOf<OutRow>()
    private val labels = mutableListOf<String>()
    private val guard = UiGuard()
    private var next: Long? = null
    private var active = false
    private var retryable = false
    private lateinit var list: ListView
    private lateinit var adapter: ArrayAdapter<String>
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() { if (active) { if (!guard.pending && list.firstVisiblePosition == 0) load(); handler.postDelayed(this, 5_000) } }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); setContentView(R.layout.activity_outbox)
        NativeUi.back(this, "Очередь")
        list = findViewById(R.id.outbox_rows)
        adapter = ArrayAdapter(this, R.layout.row_outbox, R.id.row_label, labels)
        list.adapter = adapter
        list.setOnItemClickListener { _, _, pos, _ -> rows.getOrNull(pos)?.let {
            startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", it.contactId))
        } }
        list.setOnScrollListener(object : AbsListView.OnScrollListener {
            override fun onScrollStateChanged(view: AbsListView?, state: Int) {}
            override fun onScroll(view: AbsListView?, first: Int, visible: Int, total: Int) {
                if (visible > 0 && first + visible >= total - 3 && next != null) load(true)
            }
        })
        findViewById<Button>(R.id.btn_outbox_refresh).setOnClickListener { load() }
        findViewById<Button>(R.id.btn_outbox_retry).setOnClickListener {
            if (!retryable) return@setOnClickListener
            val stamp = guard.begin() ?: return@setOnClickListener
            controls()
            findViewById<TextView>(R.id.info).text = "Повторяем прежние сообщения…"
            Core.dispatch {
                val result = runCatching { Core.facade(applicationContext).retry() }
                runOnUiThread {
                    if (!active || !guard.finish(stamp)) return@runOnUiThread
                    findViewById<TextView>(R.id.info).text = result.fold({
                        "Повтор: отправлено ${it[0]}, принято ${it[1]}, доставка подтверждена ${it[2]}, пропущено ${it[3]}. Это результат попытки, не прочтение."
                    }, ::humanError)
                    controls(); load()
                }
            }
        }
    }
    override fun onResume() { super.onResume(); active = true; handler.post(ticker) }
    override fun onPause() { active = false; guard.stop(); handler.removeCallbacks(ticker); super.onPause() }
    private fun controls() {
        findViewById<Button>(R.id.btn_outbox_retry).isEnabled = !guard.pending && retryable
        findViewById<Button>(R.id.btn_outbox_refresh).isEnabled = !guard.pending
    }
    private fun load(older: Boolean = false) {
        if (!active) return
        val after = if (older) next ?: return else 0L
        val stamp = guard.begin() ?: return
        val position = list.firstVisiblePosition
        val offset = list.getChildAt(0)?.top ?: 0
        controls()
        Core.dispatch {
            val result = runCatching {
                val f = Core.facade(applicationContext)
                val page = f.outbox(after, 50)
                Triple(page, page.first.map { row ->
                    val contact = f.get(row.contactId)
                    "${row.contactId}\n${deliveryLabel(f.messageStatus(row.mid))}\nID: ${row.mid}" +
                        if (contactCta(contact) != ContactCta.Chat) "\n${trustLabel(contact)}" else ""
                }, page.first.any { contactCta(f.get(it.contactId)) == ContactCta.Chat })
            }
            runOnUiThread {
                if (!active || !guard.finish(stamp)) return@runOnUiThread
                result.fold({ (page, text, allowed) ->
                    if (!older) { rows.clear(); labels.clear(); retryable = false }
                    val ids = rows.map { it.mid }.toMutableSet()
                    page.first.zip(text).filter { ids.add(it.first.mid) }.forEach { (row, label) -> rows.add(row); labels.add(label) }
                    next = page.second
                    retryable = retryable || allowed
                    adapter.notifyDataSetChanged()
                    if (older) list.setSelectionFromTop(position, offset)
                    findViewById<TextView>(R.id.outbox_empty).text = if (rows.isEmpty()) "Сохранённая очередь пуста. Это не подтверждает доставку конкретного сообщения — его статус в истории." else "Показано ${rows.size} записей${if (next != null) " · есть ещё" else ""}"
                }, { findViewById<TextView>(R.id.info).text = humanError(it) })
                controls()
            }
        }
    }
}

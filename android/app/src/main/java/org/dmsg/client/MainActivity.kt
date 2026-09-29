package org.dmsg.client

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.ListView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat

/** Dialogs list: paginated contacts via facade (no full dumps). */
class MainActivity : AppCompatActivity() {
    private lateinit var status: TextView
    private lateinit var list: ListView
    private val rows = mutableListOf<String>()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        status = findViewById(R.id.status)
        list = findViewById(R.id.dialogs)
        list.adapter = ArrayAdapter(this, android.R.layout.simple_list_item_1, rows)
        list.setOnItemClickListener { _, _, pos, _ -> openChat(rows[pos]) }
        findViewById<Button>(R.id.btn_chat).setOnClickListener {
            if (rows.isNotEmpty()) openChat(rows[0])
        }
        findViewById<Button>(R.id.btn_profile).setOnClickListener {
            startActivity(Intent(this, ProfileActivity::class.java))
        }
        findViewById<Button>(R.id.btn_scan).setOnClickListener {
            startActivity(Intent(this, ScannerActivity::class.java))
        }
        findViewById<Button>(R.id.btn_storage).setOnClickListener {
            startActivity(Intent(this, StorageActivity::class.java))
        }
        findViewById<Button>(R.id.btn_diag).setOnClickListener {
            startActivity(Intent(this, DiagnosticsActivity::class.java))
        }
        findViewById<Button>(R.id.btn_fgs).setOnClickListener { toggleFgs() }
        askNotifPerm()
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    private fun refresh() {
        Thread {
            val f = Core.facade(this)
            val txt = try {
                if (!f.isReady()) getString(R.string.core_missing)
                else {
                    val (enrolled, myId) = f.account()
                    rows.clear()
                    var cursor: String? = null
                    repeat(10) {
                        val (page, next) = f.contacts(cursor, 50)
                        rows.addAll(page.map { it.contactId })
                        cursor = next
                        if (next == null) return@repeat
                    }
                    "me=$myId enrolled=$enrolled fgs=${DmsgService.running(this)}"
                }
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread {
                status.text = txt
                (list.adapter as ArrayAdapter<*>).notifyDataSetChanged()
            }
        }.start()
    }

    private fun openChat(id: String) {
        startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", id))
    }

    private fun toggleFgs() {
        if (DmsgService.running(this)) DmsgService.stop(this) else DmsgService.start(this)
        refresh()
    }

    private fun askNotifPerm() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            ActivityCompat.requestPermissions(
                this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1
            )
        }
    }
}

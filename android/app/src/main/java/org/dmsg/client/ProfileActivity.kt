package org.dmsg.client

import android.graphics.Bitmap
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.ImageView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter

/** Profile + contact-QR show + request/accept/block/confirm. */
class ProfileActivity : AppCompatActivity() {
    private lateinit var myId: TextView
    private lateinit var qr: ImageView
    private lateinit var peerId: EditText
    private lateinit var info: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_profile)
        myId = findViewById(R.id.my_id)
        qr = findViewById(R.id.qr)
        peerId = findViewById(R.id.peer_id)
        info = findViewById(R.id.info)
        findViewById<Button>(R.id.btn_request).setOnClickListener { act { request(peer()) } }
        findViewById<Button>(R.id.btn_accept).setOnClickListener { act { accept(peer()); "accepted" } }
        findViewById<Button>(R.id.btn_block).setOnClickListener { act { block(peer()); "blocked" } }
        findViewById<Button>(R.id.btn_confirm).setOnClickListener { act { confirm(peer()); "confirmed" } }
    }

    override fun onResume() {
        super.onResume()
        showMine()
    }

    private fun peer() = peerId.text.toString().trim()

    private fun act(block: DmsgFacade.() -> String) {
        Thread {
            val out = try {
                "ok: ${block(Core.facade(this))}"
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { info.text = out }
        }.start()
    }

    private fun showMine() {
        Thread {
            var uri: String? = null
            var label = ""
            try {
                val f = Core.facade(this)
                val (enrolled, id) = f.account()
                label = "me=$id enrolled=$enrolled"
                if (enrolled) uri = f.myQr()
            } catch (e: Exception) {
                label = "error: ${e.message}"
            }
            val bmp = uri?.let { renderQr(it) }
            runOnUiThread {
                myId.text = label
                if (bmp != null) qr.setImageBitmap(bmp)
            }
        }.start()
    }

    private fun renderQr(uri: String): Bitmap? {
        return try {
            val m = QRCodeWriter().encode(uri, BarcodeFormat.QR_CODE, 512, 512)
            val bmp = Bitmap.createBitmap(512, 512, Bitmap.Config.RGB_565)
            for (x in 0 until 512) for (y in 0 until 512) {
                bmp.setPixel(x, y, if (m.get(x, y)) 0xFF000000.toInt() else 0xFFFFFFFF.toInt())
            }
            bmp
        } catch (_: Exception) {
            null
        }
    }
}

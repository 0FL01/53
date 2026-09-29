package org.dmsg.client

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.camera.core.CameraSelector
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.google.zxing.BinaryBitmap
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import java.util.concurrent.Executors

/**
 * Offline QR scanner: both dmsg://join/ and dmsg://contact/.
 * Broken/oversized input -> explicit error string (never silent).
 * Join -> preview (offline) -> enrol; contact -> add + outcome.
 */
@ExperimentalGetImage
class ScannerActivity : AppCompatActivity() {
    private lateinit var preview: PreviewView
    private lateinit var result: TextView
    private val exec = Executors.newSingleThreadExecutor()
    private var done = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_scanner)
        preview = findViewById(R.id.preview)
        result = findViewById(R.id.result)
        findViewById<Button>(R.id.btn_close).setOnClickListener { finish() }
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.CAMERA), 2)
        } else {
            bind()
        }
    }

    override fun onRequestPermissionsResult(code: Int, perms: Array<String>, res: IntArray) {
        super.onRequestPermissionsResult(code, perms, res)
        if (res.firstOrNull() == PackageManager.PERMISSION_GRANTED) bind()
        else result.text = "камера запрещена: введите QR вручную нельзя — нужен доступ"
    }

    private fun bind() {
        val provider = ProcessCameraProvider.getInstance(this)
        provider.addListener({
            val pv = provider.get()
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            analysis.setAnalyzer(exec) { img ->
                if (!done) {
                    decode(img)?.let { onText(it) }
                }
                img.close()
            }
            pv.unbindAll()
            pv.bindToLifecycle(
                this, CameraSelector.DEFAULT_BACK_CAMERA,
                androidx.camera.core.Preview.Builder().build().also {
                    it.setSurfaceProvider(preview.surfaceProvider)
                },
                analysis
            )
        }, ContextCompat.getMainExecutor(this))
    }

    private fun decode(img: androidx.camera.core.ImageProxy): String? {
        return try {
            val yuv = img.planes[0].buffer
            val bytes = ByteArray(yuv.remaining())
            yuv.get(bytes)
            val src = PlanarYUVLuminanceSource(
                bytes, img.width, img.height, 0, 0, img.width, img.height, false
            )
            MultiFormatReader().decode(BinaryBitmap(HybridBinarizer(src))).text
        } catch (_: Exception) {
            null
        }
    }

    private fun onText(uri: String) {
        done = true
        runOnUiThread { result.text = "QR: ${uri.take(24)}… обработка…" }
        Thread {
            val out = handle(uri)
            runOnUiThread { result.text = out }
        }.start()
    }

    private fun handle(uri: String): String {
        val f = Core.facade(this)
        val route = try {
            QrGate.route(uri).getOrThrow()
        } catch (e: DmsgError) {
            // Explicit scanner error (incl. oversized) before touching core.
            return "битый QR: ${e.message}"
        }
        return try {
            when (f.qrKind(uri)) {
                "join" -> {
                    val (domain, fp) = f.preview(uri)
                    val addr = Prefs.addr(this)
                    if (addr.isEmpty()) "invite: $domain pin=$fp (укажите addr, затем сканируйте снова)"
                    else {
                        val id = f.enrol(uri, addr, null)
                        "enrolled: $id"
                    }
                }
                "contact" -> "контакт: ${f.addQr(uri)}"
                else -> "неизвестный тип"
            }
        } catch (e: Exception) {
            "битый QR: ${e.message}"
        }
    }

    override fun onDestroy() {
        exec.shutdownNow()
        super.onDestroy()
    }
}

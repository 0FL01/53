package org.dmsg.client

import android.Manifest
import androidx.appcompat.app.AlertDialog
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
 * Join -> offline preview and explicit approval -> enrol; contact -> add + outcome.
 */
@ExperimentalGetImage
class ScannerActivity : DmsgActivity() {
    private lateinit var preview: PreviewView
    private lateinit var result: TextView
    private val exec = Executors.newSingleThreadExecutor()
    @Volatile private var done = false

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
            val plane = img.planes[0]
            val yuv = plane.buffer
            val base = yuv.position()
            val bytes = ByteArray(img.width * img.height)
            for (row in 0 until img.height) {
                for (col in 0 until img.width) {
                    bytes[row * img.width + col] =
                        yuv.get(base + row * plane.rowStride + col * plane.pixelStride)
                }
            }
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
        runOnUiThread { result.text = "QR считан, проверка…" }
        Thread {
            try {
                QrGate.route(uri).getOrThrow()
                val f = Core.facade(this)
                when (f.qrKind(uri)) {
                    "join" -> {
                        val (domain, fp) = f.preview(uri)
                        runOnUiThread { if (!isFinishing && !isDestroyed) {
                            AlertDialog.Builder(this)
                                .setTitle("Подтвердите профиль сервера")
                                .setMessage("Домен: $domain\nОтпечаток pin: $fp\n\nПроверьте эти данные вне QR перед регистрацией.")
                                .setNegativeButton("Отмена") { _, _ -> done = false; result.text = "отменено, сканируйте снова" }
                                .setPositiveButton("Зарегистрировать") { _, _ -> enrol(uri, f) }
                                .show()
                            result.text = "ожидается подтверждение сервера"
                        } }
                    }
                    "contact" -> {
                        val out = try { "контакт: ${f.addQr(uri)}" }
                            catch (e: Exception) { "ошибка контакта: ${e.message}" }
                        runOnUiThread { result.text = out }
                    }
                }
            } catch (e: Exception) {
                runOnUiThread { result.text = "битый QR: ${e.message}" }
            }
        }.start()
    }

    private fun enrol(uri: String, f: DmsgFacade) {
        result.text = "регистрация…"
        Thread {
            val out = try {
                val id = f.enrolDns(uri, DnsNetwork.resolvers(this))
                DnsNetwork.mirrorProfile(this, f)
                "enrolled: $id (DNS)"
            }
                catch (e: Exception) { "ошибка регистрации: ${e.message}" }
            runOnUiThread { result.text = out }
        }.start()
    }

    override fun onDestroy() {
        exec.shutdownNow()
        super.onDestroy()
    }
}

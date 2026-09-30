package org.dmsg.client

import android.Manifest
import androidx.appcompat.app.AlertDialog
import android.content.pm.PackageManager
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
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
import uniffi.dmsg_core.QrKind

/**
 * Offline scanner and paste use the same parser. Server import never creates an account.
 */
@ExperimentalGetImage
class ScannerActivity : DmsgActivity() {
    companion object { const val SERVER_ONLY = "serverOnly" }
    private lateinit var preview: PreviewView
    private lateinit var result: TextView
    private val exec = Executors.newSingleThreadExecutor()
    @Volatile private var done = false
    private var active = true
    private var prompt: AlertDialog? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_scanner)
        preview = findViewById(R.id.preview)
        result = findViewById(R.id.result)
        findViewById<Button>(R.id.btn_close).setOnClickListener { finish() }
        findViewById<Button>(R.id.btn_scanner_paste).setOnClickListener {
            done = false
            val field = findViewById<EditText>(R.id.scanner_code)
            val input = field.text.toString()
            field.text.clear()
            onText(input)
        }
        findViewById<Button>(R.id.btn_scan_again).setOnClickListener { done = false; result.text = "Сканируйте снова или вставьте код" }
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.CAMERA), 2)
        } else {
            bind()
        }
    }

    override fun onRequestPermissionsResult(code: Int, perms: Array<String>, res: IntArray) {
        super.onRequestPermissionsResult(code, perms, res)
        if (code != 2) return
        if (res.firstOrNull() == PackageManager.PERMISSION_GRANTED) bind()
        else result.text = "Камера запрещена. Вставьте код в поле ниже"
    }

    private fun bind() {
        val provider = ProcessCameraProvider.getInstance(this)
        provider.addListener({
            if (!active) return@addListener
            try {
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
            } catch (_: Exception) { result.text = "Камера недоступна. Вставьте код в поле ниже" }
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
        Core.dispatch {
            try {
                val code = QrGate.normalize(uri)
                QrGate.route(code).getOrThrow()
                val f = Core.facade(this)
                val serverOnly = intent.getBooleanExtra(SERVER_ONLY, false)
                when (f.qrKind(code)) {
                    QrKind.SERVER -> {
                        if (!serverOnly) throw DmsgError("Для добавления контакта нужен QR контакта")
                        val p = QrGate.serverPreview(f, code)
                        runOnUiThread { if (active && !isFinishing && !isDestroyed) {
                            prompt = AlertDialog.Builder(this)
                                .setTitle("Подтвердите профиль сервера")
                                .setMessage("Домен: ${p.domain}\nОтпечаток сертификата: ${p.fingerprint}\n\nСверьте данные с доверенным источником. Код публичный и не создаёт аккаунт.")
                                .setNegativeButton("Отмена") { _, _ -> done = false; result.text = "отменено, сканируйте снова" }
                                .setOnCancelListener { done = false }
                                .setPositiveButton("Принять сервер") { _, _ -> importServer(p.code, f) }
                                .show()
                            result.text = "ожидается подтверждение сервера"
                        } }
                    }
                    QrKind.CONTACT -> {
                        if (serverOnly) throw DmsgError("Нужен публичный QR сервера, а не контакта")
                        if (!f.account().authenticated) throw DmsgError("Сначала войдите в аккаунт")
                        val out = "контакт: ${f.addQr(code)}"
                        runOnUiThread { if (active) result.text = out }
                    }
                }
            } catch (e: Exception) {
                runOnUiThread { if (active) result.text = "битый QR: ${humanError(e)}" }
            }
        }
    }

    private fun importServer(code: String, f: DmsgFacade) {
        result.text = "сохраняем сервер…"
        Core.dispatch {
            val outcome = runCatching {
                f.configureDns(code, DnsNetwork.resolvers(this))
                DnsNetwork.mirrorProfile(this, f)
            }
            runOnUiThread { if (active) outcome.fold(
                { setResult(RESULT_OK); finish() },
                { result.text = humanError(it) }
            ) }
        }
    }

    override fun onDestroy() {
        active = false
        prompt?.dismiss()
        exec.shutdownNow()
        super.onDestroy()
    }
}

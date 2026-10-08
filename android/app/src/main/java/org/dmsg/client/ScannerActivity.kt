package org.dmsg.client

import android.Manifest
import android.content.Intent
import androidx.appcompat.app.AlertDialog
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
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
import uniffi.dmsg_core.QrOutcome

/**
 * Offline scanner and paste use the same parser. Server import never creates an account.
 */
@ExperimentalGetImage
class ScannerActivity : DmsgActivity() {
    companion object {
        const val SERVER_ONLY = "serverOnly"
        const val INVITATION_ONLY = "invitationOnly"
        const val INVITATION_TICKET = "invitationTicket"
    }
    private val invitationOnly get() = intent.getBooleanExtra(INVITATION_ONLY, false)
    private val invitationTicket get() = intent.getLongExtra(INVITATION_TICKET, -1)
    private var returningInvitation = false
    private lateinit var preview: PreviewView
    private lateinit var result: TextView
    private val exec = Executors.newSingleThreadExecutor()
    @Volatile private var done = false
    private var active = true
    @Volatile private var generation = 0L
    private var prompt: AlertDialog? = null
    private val guard = UiGuard()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_scanner)
        NativeUi.back(this, getString(if (invitationOnly) R.string.title_invitation else if (intent.getBooleanExtra(SERVER_ONLY, false)) R.string.title_server_qr else R.string.title_contact_qr))
        if (invitationOnly) {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
            findViewById<View>(R.id.scanner_code).visibility = View.GONE
            findViewById<View>(R.id.scanner_code_label).visibility = View.GONE
            findViewById<View>(R.id.btn_scanner_paste).visibility = View.GONE
        }
        preview = findViewById(R.id.preview)
        result = findViewById(R.id.result)
        findViewById<Button>(R.id.btn_close).setOnClickListener { finish() }
        findViewById<Button>(R.id.btn_scanner_paste).setOnClickListener {
            if (guard.pending || prompt?.isShowing == true) return@setOnClickListener
            done = false
            val field = findViewById<EditText>(R.id.scanner_code)
            val input = field.text.toString()
            field.text.clear()
            onText(input)
        }
        findViewById<Button>(R.id.btn_scan_again).setOnClickListener { if (!guard.pending && prompt?.isShowing != true) { done = false; result.setText(R.string.scan_or_paste_again) } }
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
        else result.setText(if (invitationOnly) R.string.camera_denied_invitation else R.string.camera_denied_code)
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
                val stamp = generation
                try { if (!done) decode(img)?.let { onText(it, stamp) } }
                finally { img.close() }
            }
            pv.unbindAll()
            pv.bindToLifecycle(
                this, CameraSelector.DEFAULT_BACK_CAMERA,
                androidx.camera.core.Preview.Builder().build().also {
                    it.setSurfaceProvider(preview.surfaceProvider)
                },
                analysis
            )
            } catch (_: Exception) { result.setText(if (invitationOnly) R.string.camera_unavailable_invitation else R.string.camera_unavailable_code) }
        }, ContextCompat.getMainExecutor(this))
    }

    private fun decode(img: androidx.camera.core.ImageProxy): String? {
        var bytes: ByteArray? = null
        return try {
            val plane = img.planes[0]
            val yuv = plane.buffer
            val base = yuv.position()
            bytes = ByteArray(img.width * img.height)
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
        } finally { bytes?.fill(0) }
    }

    private fun onText(uri: String) {
        onText(uri, generation)
    }
    private fun onText(uri: String, scanGeneration: Long) {
        done = true
        runOnUiThread {
        if (!active || scanGeneration != generation || isFinishing || isDestroyed) return@runOnUiThread
        if (invitationOnly) {
            try {
                val value = InvitationInput.parse(uri)
                if (!InvitationScanTransfer.publish(invitationTicket, value)) {
                    result.setText(R.string.scan_cancelled)
                    return@runOnUiThread
                }
                returningInvitation = true
                // Expire an undelivered result; the Intent contains no payload.
                val ticket = invitationTicket
                Handler(Looper.getMainLooper()).postDelayed({ InvitationScanTransfer.cancel(ticket) }, 5_000)
                setResult(RESULT_OK)
                finish()
            } catch (_: Exception) {
                result.setText(R.string.invitation_invalid)
            }
            return@runOnUiThread
        }
        val stamp = guard.begin() ?: return@runOnUiThread
        if (!active || prompt?.isShowing == true) { guard.finish(stamp); return@runOnUiThread }
        result.setText(R.string.qr_checking)
        Core.dispatch {
            try {
                val code = QrGate.normalize(uri)
                QrGate.route(code).getOrThrow()
                val f = Core.facade(applicationContext)
                val serverOnly = intent.getBooleanExtra(SERVER_ONLY, false)
                when (f.qrKind(code)) {
                    QrKind.SERVER -> {
                         if (!serverOnly) throw DmsgError(R.string.error_contact_qr_required, ErrorKind.BadQr)
                        val p = QrGate.serverPreview(f, code)
                         runOnUiThread { if (active && guard.finish(stamp) && !isFinishing && !isDestroyed) {
                            prompt = AlertDialog.Builder(this)
                                .setTitle(R.string.confirm_server_profile)
                                .setMessage(getString(R.string.server_trust_preview, p.domain, p.fingerprint))
                                .setNegativeButton(R.string.cancel) { _, _ -> done = false; result.setText(R.string.server_cancelled) }
                                .setOnCancelListener { done = false }
                                .setPositiveButton(R.string.accept_server) { _, _ -> importServer(p.code, f) }
                                .show()
                            result.setText(R.string.server_awaiting_confirmation)
                        } }
                    }
                    QrKind.CONTACT -> {
                         if (serverOnly) throw DmsgError(R.string.error_server_qr_required, ErrorKind.BadQr)
                         if (!f.account().authenticated) throw DmsgError(R.string.error_sign_in_required, ErrorKind.NotAuthenticated)
                          val id = f.contactQrId(code)
                          runOnUiThread { if (active && guard.finish(stamp) && !isFinishing && !isDestroyed) {
                              prompt = AlertDialog.Builder(this)
                                  .setTitle(R.string.add_contact)
                                  .setMessage(getString(R.string.contact_add_preview, id))
                                  .setNegativeButton(R.string.cancel) { _, _ -> done = false; result.setText(R.string.contact_add_cancelled) }
                                  .setOnCancelListener { done = false }
                                  .setPositiveButton(R.string.contact_add_confirm) { _, _ -> inviteContact(code, id, f) }
                                  .show()
                              result.setText(R.string.contact_add_confirm)
                          } }
                    }
                }
            } catch (e: Exception) {
                runOnUiThread { if (active && guard.finish(stamp)) result.text = getString(R.string.qr_failed, humanError(resources, e)) }
            }
        }
        }
    }

    private fun importServer(code: String, f: DmsgFacade) {
        val stamp = guard.begin() ?: return
        result.setText(R.string.saving_server)
        Core.dispatch {
            val outcome = runCatching {
                f.configureDns(code, DnsNetwork.resolvers(this))
                DnsNetwork.mirrorProfile(this, f)
            }
            runOnUiThread { if (active && guard.finish(stamp)) outcome.fold(
                { setResult(RESULT_OK); finish() },
                { result.text = humanError(resources, it) }
            ) }
        }
    }

    private fun inviteContact(code: String, id: String, f: DmsgFacade) {
        val stamp = guard.begin() ?: return
        result.setText(R.string.adding_contact)
        Core.dispatch {
            val outcome = runCatching {
                val added = f.inviteQr(code) // durable before the network attempt
                if (added != QrOutcome.IDENTITY_CHANGED) {
                    // A failed connection does not undo the saved request; the
                    // existing DNS poll retries its durable state on reconnect.
                    try { f.reconnect() } catch (e: DmsgError) {
                        if (e.kind != ErrorKind.Transport && e.kind != ErrorKind.Busy) throw e
                    }
                }
                added
            }
            runOnUiThread { if (active && guard.finish(stamp)) outcome.fold(
                { added ->
                    if (added == QrOutcome.IDENTITY_CHANGED) result.setText(R.string.qr_identity_changed)
                    else { startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", id)); finish() }
                }, { result.text = humanError(resources, it) }
            ) }
        }
    }

    override fun onResume() {
        super.onResume(); active = true
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) bind()
    }
    override fun onPause() {
        generation++
        active = false; guard.stop(); prompt?.dismiss(); prompt = null
        findViewById<EditText>(R.id.scanner_code).text.clear()
        super.onPause()
    }

    override fun onDestroy() {
        active = false
        if (invitationOnly && !returningInvitation) InvitationScanTransfer.cancel(invitationTicket)
        prompt?.dismiss()
        exec.shutdownNow()
        super.onDestroy()
    }
}

package org.dmsg.client

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.graphics.Bitmap
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PersistableBundle
import android.os.SystemClock
import android.view.WindowManager
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.ImageView
import android.widget.ListView
import android.widget.TextView

class InvitationsActivity : DmsgActivity() {
    internal val flow = InvitationFlow()
    private lateinit var status: TextView
    private lateinit var phrase: TextView
    private lateinit var qr: ImageView
    private lateinit var list: ListView
    private var bitmap: Bitmap? = null
    private var active = false
    private var anchor = 0L
    private var clock = 0L
    private var imageGeneration = 0L
    private var exporting = false
    private var facade: DmsgFacade? = null
    private var opened = false
    private val handler = Handler(Looper.getMainLooper())
    private val tick = object : Runnable {
        override fun run() {
            if (!active) return
            val grant = flow.grant
            if (grant?.state == InvitationState.ACTIVE) {
                val remaining = grant.invitation.expiresAt - (clock + (SystemClock.elapsedRealtime() - anchor) / 1000)
                if (remaining <= 0) {
                    flow.expire(); wipe(); InvitationShare.cleanup(applicationContext, true)
                    status.setText(R.string.invitation_expired); controls()
                } else findViewById<TextView>(R.id.invitation_expiry).text = getString(R.string.invitation_remaining, remaining / 3600, remaining % 3600 / 60)
            }
            handler.postDelayed(this, 1_000)
        }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        setContentView(R.layout.activity_invitations)
        NativeUi.back(this, getString(R.string.title_invitations))
        status = findViewById(R.id.invitation_manage_status)
        phrase = findViewById(R.id.invitation_phrase)
        qr = findViewById(R.id.invitation_qr)
        list = findViewById(R.id.invitation_list)
        flow.restore(savedInstanceState?.getByteArray("pendingId"), savedInstanceState?.getByteArray("selectedId"), savedInstanceState?.getByteArray("revokeId"))
        findViewById<Button>(R.id.btn_invitation_refresh).setOnClickListener { run(flow.refresh()) }
        findViewById<Button>(R.id.btn_invitation_create).setOnClickListener {
            try { run(flow.create()) } catch (e: Exception) { status.text = humanError(resources, e) }
        }
        list.setOnItemClickListener { _, _, position, _ -> flow.rows.getOrNull(position)?.let { run(flow.select(it.issueId)) } }
        findViewById<Button>(R.id.btn_invitation_revoke).setOnClickListener {
            if (!exporting) { InvitationShare.cleanup(this, true); run(flow.revoke()) }
        }
        findViewById<Button>(R.id.btn_invitation_copy).setOnClickListener {
            if (!flow.mayShare || exporting) return@setOnClickListener
            val clip = ClipData.newPlainText(getString(R.string.title_invitation), flow.grant?.phrase() ?: return@setOnClickListener)
            if (Build.VERSION.SDK_INT >= 33) clip.description.extras = PersistableBundle().apply { putBoolean("android.content.extra.IS_SENSITIVE", true) }
            getSystemService(ClipboardManager::class.java).setPrimaryClip(clip)
            status.setText(R.string.invitation_phrase_copied)
        }
        findViewById<Button>(R.id.btn_invitation_share).setOnClickListener { share() }
    }
    override fun onSaveInstanceState(outState: Bundle) {
        outState.putByteArray("pendingId", flow.pending)
        outState.putByteArray("selectedId", flow.selected)
        outState.putByteArray("revokeId", flow.pendingRevoke)
        super.onSaveInstanceState(outState)
    }
    override fun onResume() {
        super.onResume(); active = true
        // Next open clears a process-death export too; no absolute deletion TTL claim.
        InvitationShare.cleanup(this, !opened)
        opened = true
        run(flow.refresh()); handler.post(tick)
    }
    override fun onPause() {
        active = false; imageGeneration++; exporting = false
        flow.pause(); handler.removeCallbacks(tick); wipe()
        super.onPause()
    }
    private fun wipe() {
        phrase.text = ""; qr.setImageDrawable(null)
        bitmap?.let { if (!it.isRecycled) { it.eraseColor(0); it.recycle() } }; bitmap = null
        findViewById<TextView>(R.id.invitation_expiry).text = ""
    }
    private fun run(job: InvitationFlow.Job?) {
        if (job == null || exporting) return
        wipe(); controls(); status.setText(R.string.invitation_working)
        Core.dispatch {
            val f = runCatching { Core.facade(applicationContext) }
            val result = runCatching { job.run(f.getOrThrow()) }
            runOnUiThread {
                if (!active || isFinishing || isDestroyed) { result.getOrNull()?.clear(); return@runOnUiThread }
                try {
                    if (!flow.complete(job, result)) return@runOnUiThread
                    facade = f.getOrNull()
                    result.fold({ render() }, { status.text = humanError(resources, it) })
                } catch (e: Exception) { status.text = humanError(resources, e) }
                controls()
            }
        }
    }
    private fun render() {
        list.adapter = ArrayAdapter(this, android.R.layout.simple_list_item_1, flow.rows.mapIndexed { i, row ->
            getString(R.string.invitation_list_item, i + 1, java.text.DateFormat.getDateTimeInstance().format(java.util.Date(row.expiresAt * 1000)))
        })
        val grant = flow.grant
        if (grant == null) { status.setText(R.string.invitation_list_ready); return }
        status.setText(when (grant.state) {
            InvitationState.ACTIVE -> R.string.invitation_active
            InvitationState.USED -> R.string.invitation_used
            InvitationState.REVOKED -> R.string.invitation_revoked
            InvitationState.EXPIRED -> R.string.invitation_expired
        })
        if (grant.state == InvitationState.ACTIVE && flow.pendingRevoke == null) {
            val value = grant.phrase() ?: return
            phrase.text = value
            clock = grant.serverNow; anchor = SystemClock.elapsedRealtime()
            val generation = ++imageGeneration
            val f = facade ?: return
            InvitationImages.dispatch {
                val result = runCatching { InvitationImages.qr(f.normalizeInvitation(value)) }
                runOnUiThread {
                    if (!active || generation != imageGeneration || flow.grant !== grant) {
                        result.getOrNull()?.let { it.eraseColor(0); it.recycle() }; return@runOnUiThread
                    }
                    result.fold({ bitmap = it; qr.setImageBitmap(it) }, { status.text = humanError(resources, it) })
                    controls()
                }
            }
        } else InvitationShare.cleanup(this, true)
    }
    private fun controls() {
        val available = !flow.busy && !exporting
        findViewById<Button>(R.id.btn_invitation_refresh).isEnabled = available
        findViewById<Button>(R.id.btn_invitation_create).apply {
            isEnabled = available && flow.pendingRevoke == null && (flow.pending != null || flow.rows.size < 8)
            setText(if (flow.pending != null) R.string.invitation_retry else R.string.invitation_create)
        }
        list.isEnabled = available && flow.pending == null && flow.pendingRevoke == null
        findViewById<Button>(R.id.btn_invitation_copy).isEnabled = flow.mayShare && !exporting
        findViewById<Button>(R.id.btn_invitation_share).isEnabled = flow.mayShare && bitmap != null && !exporting
        findViewById<Button>(R.id.btn_invitation_revoke).isEnabled = available && flow.selected != null &&
            (flow.grant?.state == InvitationState.ACTIVE || flow.pendingRevoke != null)
    }
    private fun share() {
        if (!flow.mayShare || exporting) return
        val source = bitmap ?: return
        val copy = source.copy(Bitmap.Config.ARGB_8888, true)
        val generation = imageGeneration
        exporting = true; controls()
        val app = applicationContext
        InvitationImages.dispatch {
            val result = try { runCatching { InvitationShare.export(app, copy) } }
                finally { copy.eraseColor(0); copy.recycle() }
            runOnUiThread {
                exporting = false
                if (!active || generation != imageGeneration) { InvitationShare.cleanup(app, true); return@runOnUiThread }
                result.fold({ uri ->
                    // Application-context cleanup survives activity pause/stop; best effort only.
                    handler.postDelayed({ InvitationShare.cleanup(app) }, InvitationShare.MAX_AGE_MS)
                    startActivity(Intent.createChooser(InvitationShare.intent(this, uri), getString(R.string.invitation_share_qr)))
                }, { status.text = humanError(resources, it) })
                controls()
            }
        }
    }
}

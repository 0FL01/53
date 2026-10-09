package org.dmsg.client

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.system.Os
import android.system.OsConstants
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.io.File

/** Visible target-UID debug host. No Application/coordinator/account or DB access. */
class CallProbeActivity : DmsgActivity() {
    companion object {
        const val EXTRA_AUTO_START = "probe_auto_start"
        const val EXTRA_FIXTURE_PATH = "probe_fixture_path"
        const val EXTRA_RATE_OFFSET_HZ = "probe_playback_rate_offset_hz"
    }

    var probeOwner: CallProbeAudio? = null
        private set
    @Volatile var probeFailure: String? = null
        private set
    private var foreground = false
    private var autoPending = false
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var status: TextView
    private lateinit var start: Button
    private lateinit var stop: Button
    private val refresh = object : Runnable {
        override fun run() {
            val state = probeOwner?.snapshot()
            status.text = probeFailure ?: state?.json()?.toString(2) ?: "Ready. Microphone permission is required."
            start.isEnabled = state == null || (state.cleanupComplete && !state.ownerAlive)
            stop.isEnabled = state != null && !state.cleanupComplete
            if (foreground) handler.postDelayed(this, 250)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // This guard precedes all native/audio work, including JNI class loading.
        if (packageName != "org.dmsg.client.gate") {
            probeFailure = "gate_package_required"
            finish()
            return
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val padding = (16 * resources.displayMetrics.density).toInt()
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(padding, padding, padding, padding)
        }
        column.addView(TextView(this).apply {
            text = if (intent.hasExtra(EXTRA_FIXTURE_PATH)) "Foreground DNS fixture audio probe"
                else "Foreground protected local memory-pair probe (no DNS evidence)"
            textSize = 18f
        })
        start = Button(this).apply {
            text = "Start probe"
            setOnClickListener {
                if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                    probeFailure = "microphone_permission_denied"
                    requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), 1)
                } else startProbe()
            }
        }
        stop = Button(this).apply { text = "Stop probe"; setOnClickListener { stopProbe() } }
        column.addView(start); column.addView(stop)
        status = TextView(this).apply { setTextIsSelectable(true); textSize = 12f }
        column.addView(status)
        setContentView(ScrollView(this).apply { addView(column) })
        autoPending = savedInstanceState == null && intent.getBooleanExtra(EXTRA_AUTO_START, false)
    }

    override fun onResume() {
        super.onResume()
        if (packageName != "org.dmsg.client.gate") return
        foreground = true
        handler.post(refresh)
        maybeAutoStart()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) maybeAutoStart()
    }

    private fun maybeAutoStart() {
        if (foreground && hasWindowFocus() && autoPending) {
            autoPending = false
            startProbe()
        }
    }

    /** UI/instrumentation entrypoint. The owner does all potentially blocking work. */
    fun startProbe(): Boolean {
        if (packageName != "org.dmsg.client.gate") { probeFailure = "gate_package_required"; return false }
        if (!foreground || !hasWindowFocus()) { probeFailure = "visible_foreground_required"; return false }
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            probeFailure = "microphone_permission_denied"
            return false
        }
        val old = probeOwner?.snapshot()
        if (old != null && (!old.cleanupComplete || old.ownerAlive)) return false
        val fixture = if (intent.hasExtra(EXTRA_FIXTURE_PATH)) {
            val path = intent.getStringExtra(EXTRA_FIXTURE_PATH)
            if (path.isNullOrBlank() || !privateReadOnlyFixture(path)) {
                probeFailure = "private_read_only_fixture_required"
                return false
            }
            path
        } else null
        val offset = intent.getIntExtra(EXTRA_RATE_OFFSET_HZ, 0)
        if (offset !in -8..8) { probeFailure = "playback_rate_offset_out_of_bounds"; return false }
        probeFailure = null
        probeOwner = CallProbeAudio(this, fixture).also { it.setPlaybackRateOffsetHz(offset); it.start() }
        return true
    }

    private fun privateReadOnlyFixture(path: String): Boolean = try {
        val file = File(path)
        val root = File(applicationInfo.dataDir).canonicalPath + File.separator
        val stat = Os.stat(file.canonicalPath)
        file.isAbsolute && file.canonicalPath.startsWith(root) && OsConstants.S_ISREG(stat.st_mode) &&
            stat.st_uid == Process.myUid() && (stat.st_mode and 0x3f) == 0 && // no group/other permissions
            (stat.st_mode and 0x92) == 0 && (stat.st_mode and 0x100) != 0 // no writes, owner-readable
    } catch (_: Exception) { false }

    fun stopProbe(reason: String = "explicit_stop") { probeOwner?.requestStop(reason) }

    override fun onPause() {
        foreground = false
        autoPending = false
        handler.removeCallbacks(refresh)
        stopProbe("activity_paused")
        super.onPause()
    }

    override fun onDestroy() {
        handler.removeCallbacks(refresh)
        stopProbe("activity_destroyed")
        super.onDestroy()
    }
}

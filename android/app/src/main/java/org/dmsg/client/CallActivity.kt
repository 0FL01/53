package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.View
import android.widget.Button
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.core.content.ContextCompat
import androidx.core.telecom.CallEndpointCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import uniffi.dmsg_core.CallEndReason
import uniffi.dmsg_core.CallPhase
import uniffi.dmsg_core.CallSnapshot

/** Visible permission/actions and observation; service owns Telecom, media and screen-off lifetime. */
class CallActivity : DmsgActivity() {
    companion object {
        const val ACTION_ANSWER = "org.dmsg.client.CALL_ANSWER"
        internal fun open(context: Context, id: String, epoch: Long, answer: Boolean = false) {
            context.startActivity(Intent(context, CallActivity::class.java)
                .setAction(if (answer) ACTION_ANSWER else "org.dmsg.client.CALL_OPEN")
                .putExtra("callId", id).putExtra("epoch", epoch)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP))
        }
    }
    private enum class PermissionAction { DIAL, ANSWER }
    private var pending: PermissionAction? = null
    private var visible = false
    private var dialSubmitted = false
    private var permissionRequested = false
    private var permissionDenied = false
    private var observer: CoroutineScope? = null
    private var endpointDialog: AlertDialog? = null
    private val handler = Handler(Looper.getMainLooper())
    private val microphone = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        permissionRequested = false
        if (!granted) {
            permissionDenied = true
            pending = null
            findViewById<TextView>(R.id.call_status).setText(R.string.call_microphone_permission)
        } else continueAction()
    }
    private val ticker = object : Runnable {
        override fun run() { if (visible) { render(CallUi.state.value); handler.postDelayed(this, 1_000) } }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_call)
        NativeUi.back(this, getString(R.string.title_call))
        dialSubmitted = savedInstanceState?.getBoolean("dialSubmitted") ?: false
        pending = savedInstanceState?.getString("permissionAction")?.let { value -> PermissionAction.entries.firstOrNull { it.name == value } }
        permissionRequested = savedInstanceState?.getBoolean("permissionRequested") ?: false
        savedInstanceState?.getString("boundCallId")?.let { id ->
            intent.putExtra("callId", id).putExtra("epoch", savedInstanceState.getLong("boundEpoch"))
        }
        findViewById<Button>(R.id.call_accept).setOnClickListener { request(PermissionAction.ANSWER) }
        findViewById<Button>(R.id.call_decline).setOnClickListener { currentAction { id, epoch -> DmsgService.callOwner()?.reject(id, epoch) } }
        findViewById<Button>(R.id.call_hangup).setOnClickListener {
            if (CallUi.state.value.preparing) DmsgService.callOwner()?.cancelPreparing(intent.getStringExtra("peer").orEmpty())
            else currentAction { id, epoch -> DmsgService.callOwner()?.end(id, epoch) }
        }
        findViewById<Button>(R.id.call_mute).setOnClickListener { currentAction { id, epoch -> DmsgService.callOwner()?.mute(id, epoch) } }
        findViewById<Button>(R.id.call_speaker).setOnClickListener {
            val state = CallUi.state.value
            val target = if (state.endpoint?.type == CallEndpointCompat.TYPE_SPEAKER) CallEndpointCompat.TYPE_EARPIECE else CallEndpointCompat.TYPE_SPEAKER
            state.endpoints.firstOrNull { it.type == target }?.let { endpoint ->
                currentAction { id, epoch -> DmsgService.callOwner()?.endpoint(id, epoch, endpoint) }
            }
        }
        findViewById<Button>(R.id.call_endpoint).setOnClickListener { endpoints() }
        findViewById<Button>(R.id.call_dial).setOnClickListener { request(PermissionAction.DIAL) }
    }
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent); setIntent(intent); pending = null
        if (intent.action == ACTION_ANSWER) request(PermissionAction.ANSWER)
        render(CallUi.state.value)
    }
    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("dialSubmitted", dialSubmitted)
        outState.putString("permissionAction", pending?.name)
        outState.putBoolean("permissionRequested", permissionRequested)
        outState.putString("boundCallId", intent.getStringExtra("callId"))
        outState.putLong("boundEpoch", intent.getLongExtra("epoch", -1))
        super.onSaveInstanceState(outState)
    }
    override fun onResume() {
        super.onResume(); visible = true
        observer = CoroutineScope(Dispatchers.Main.immediate + Job()).also { scope ->
            scope.launch { CallUi.state.collect { render(it); continueAction() } }
        }
        handler.post(ticker)
        when {
            pending != null -> continueAction()
            intent.action == ACTION_ANSWER -> request(PermissionAction.ANSWER)
            !dialSubmitted && intent.hasExtra("peer") && !CallUi.state.value.busy -> request(PermissionAction.DIAL)
        }
    }
    override fun onPause() {
        visible = false; observer?.cancel(); observer = null; handler.removeCallbacks(ticker)
        endpointDialog?.dismiss(); endpointDialog = null
        super.onPause()
    }
    private fun request(action: PermissionAction) {
        if (!visible || permissionRequested) return
        permissionDenied = false
        pending = action
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) continueAction()
        else { permissionRequested = true; microphone.launch(Manifest.permission.RECORD_AUDIO) }
    }
    private fun continueAction() {
        val action = pending ?: return
        if (!visible || checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) return
        val state = CallUi.state.value
        when (action) {
            PermissionAction.DIAL -> {
                pending = null
                if (state.busy || dialSubmitted) return
                dialSubmitted = true
                ContextCompat.startForegroundService(this, Intent(this, DmsgService::class.java)
                    .setAction(DmsgService.ACTION_DIAL).putExtra("peer", intent.getStringExtra("peer").orEmpty()))
            }
            PermissionAction.ANSWER -> {
                if (!matches(state) || state.call?.phase != CallPhase.RINGING) { pending = null; return }
                if (!state.telecomReady) return
                pending = null
                currentAction { id, epoch -> DmsgService.callOwner()?.accept(id, epoch) }
            }
        }
    }
    private fun matches(state: CallUiState): Boolean {
        val id = intent.getStringExtra("callId") ?: return intent.getStringExtra("peer") == state.call?.contactId
        return state.call?.callId == id && state.epoch == intent.getLongExtra("epoch", -1)
    }
    private fun currentAction(action: (String, Long) -> Unit) {
        val state = CallUi.state.value
        if (!visible || !matches(state) || state.tearingDown) return
        state.call?.let { action(it.callId, state.epoch) }
    }
    private fun render(state: CallUiState) {
        if (!visible) return
        if (!intent.hasExtra("callId") && dialSubmitted && !state.preparing && state.call?.busy() == true &&
            intent.getStringExtra("peer") == state.call.contactId) {
            intent.putExtra("callId", state.call.callId).putExtra("epoch", state.epoch)
        }
        val matches = matches(state)
        val call = if (matches) state.call else null
        findViewById<TextView>(R.id.call_peer).text = intent.getStringExtra("alias") ?: call?.contactId ?: intent.getStringExtra("peer") ?: getString(R.string.call_private_caller)
        findViewById<TextView>(R.id.call_status).text = getString(when {
            state.tearingDown && matches -> R.string.call_ending
            state.preparing -> R.string.call_connecting
            permissionDenied -> R.string.call_microphone_permission
            (matches || intent.hasExtra("peer")) && state.errorRes != null -> state.errorRes
            call != null -> callPhaseRes(call)
            intent.hasExtra("callId") -> R.string.call_expired
            else -> R.string.call_ready
        })
        findViewById<TextView>(R.id.call_duration).text = if (matches && state.activeSince != null) {
            val seconds = ((state.endedAt ?: SystemClock.elapsedRealtime()) - state.activeSince).coerceAtLeast(0) / 1_000
            getString(R.string.call_duration, seconds / 60, seconds % 60)
        } else ""
        findViewById<TextView>(R.id.call_availability).setText(if (state.incomingAvailable) R.string.call_incoming_available else R.string.call_incoming_unavailable)
        val ringing = call?.phase == CallPhase.RINGING && call.direction == uniffi.dmsg_core.CallDirection.INCOMING && !state.tearingDown
        findViewById<Button>(R.id.call_accept).apply { visibility = if (ringing) View.VISIBLE else View.GONE; isEnabled = !permissionRequested }
        findViewById<Button>(R.id.call_decline).visibility = if (ringing) View.VISIBLE else View.GONE
        findViewById<Button>(R.id.call_hangup).apply {
            visibility = if (state.preparing || call?.busy() == true && !ringing) View.VISIBLE else View.GONE
            isEnabled = !state.tearingDown
        }
        val active = matches && state.telecomActive && call?.phase in setOf(CallPhase.CONNECTING, CallPhase.ACTIVE) && !state.tearingDown
        findViewById<View>(R.id.call_audio_controls).visibility = if (active) View.VISIBLE else View.GONE
        findViewById<Button>(R.id.call_mute).apply { setText(if (state.muted) R.string.call_unmute else R.string.call_mute); isSelected = state.muted }
        findViewById<Button>(R.id.call_speaker).apply { isSelected = state.endpoint?.type == CallEndpointCompat.TYPE_SPEAKER; isEnabled = state.endpoints.any { it.type == CallEndpointCompat.TYPE_SPEAKER } }
        findViewById<Button>(R.id.call_endpoint).text = state.endpoint?.name ?: getString(R.string.call_endpoint)
        findViewById<Button>(R.id.call_dial).visibility = if (intent.hasExtra("peer") && !dialSubmitted && !state.busy) View.VISIBLE else View.GONE
    }
    private fun endpoints() {
        val state = CallUi.state.value
        if (!matches(state)) return
        val endpoints = state.endpoints.filter { it.type in setOf(CallEndpointCompat.TYPE_EARPIECE, CallEndpointCompat.TYPE_SPEAKER,
            CallEndpointCompat.TYPE_BLUETOOTH, CallEndpointCompat.TYPE_WIRED_HEADSET) }
        endpointDialog = AlertDialog.Builder(this).setTitle(R.string.call_endpoint)
            .setItems(endpoints.map { it.name.toString() }.toTypedArray()) { _, which ->
                currentAction { id, epoch -> DmsgService.callOwner()?.endpoint(id, epoch, endpoints[which]) }
            }.setNegativeButton(R.string.cancel, null).show()
    }
}

internal fun callPhaseRes(call: CallSnapshot): Int = when (call.phase) {
    CallPhase.STOPPED, CallPhase.LISTENING -> R.string.call_ready
    CallPhase.DIALING -> R.string.call_dialing
    CallPhase.RINGING -> R.string.call_ringing
    CallPhase.CONNECTING -> R.string.call_connecting
    CallPhase.ACTIVE -> R.string.call_active
    CallPhase.ENDED -> when (call.reason) {
        CallEndReason.NONE, CallEndReason.LOCAL_ENDED -> R.string.call_ended
        CallEndReason.REMOTE_ENDED -> R.string.call_remote_ended
        CallEndReason.UNAVAILABLE -> R.string.call_unavailable
        CallEndReason.BUSY -> R.string.call_busy
        CallEndReason.EXPIRED -> R.string.call_expired
        CallEndReason.REJECTED -> R.string.call_rejected
        CallEndReason.CONNECTION_LOST -> R.string.call_connection_lost
        CallEndReason.INVALID -> R.string.call_invalid
        CallEndReason.REVOKED -> R.string.call_revoked
        CallEndReason.AUDIO_UNAVAILABLE -> R.string.call_audio_unavailable
    }
}

package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.PowerManager
import android.os.SystemClock
import android.telecom.DisconnectCause
import androidx.core.telecom.CallAttributesCompat
import androidx.core.telecom.CallControlResult
import androidx.core.telecom.CallControlScope
import androidx.core.telecom.CallEndpointCompat
import androidx.core.telecom.CallsManager
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.dmsg_core.CallDirection
import uniffi.dmsg_core.CallEndReason
import uniffi.dmsg_core.CallObserver
import uniffi.dmsg_core.CallPhase
import uniffi.dmsg_core.CallSnapshot

internal data class CallUiState(val call: CallSnapshot? = null, val epoch: Long = 0,
    val incomingAvailable: Boolean = false, val preparing: Boolean = false, val tearingDown: Boolean = false,
    val telecomActive: Boolean = false, val activeSince: Long? = null, val muted: Boolean = false,
    val telecomReady: Boolean = false, val endedAt: Long? = null,
    val endpoints: List<CallEndpointCompat> = emptyList(), val endpoint: CallEndpointCompat? = null,
    val errorRes: Int? = null, val audio: CallAudio.Snapshot? = null) {
    val busy get() = preparing || tearingDown || call?.busy() == true
}

/** Observation only. The instance and all mutation belong to DmsgService. */
internal object CallUi {
    val mutable = MutableStateFlow(CallUiState())
    val state: StateFlow<CallUiState> get() = mutable
}

internal class CallSessionOwner(private val context: Context,
    private val changed: () -> Unit, private val pauseMessages: (Boolean) -> Unit,
    private val idle: () -> Unit) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate +
        CoroutineExceptionHandler { _, _ -> finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true) })
    private val guard = CallStateGuard()
    private var manager: CallsManager? = null
    private var commands: CallCommands? = null
    private var actorAvailable = false
    private var creating = false
    private var afterActor: (() -> Unit)? = null
    private var dialingPeer: String? = null
    private var generation = 0L
    private var wantedListener = false
    private var facade: DmsgFacade? = null
    private var telecom: CallControlScope? = null
    private var telecomJob: Job? = null
    private var telecomAdding = false
    private var activationPending = false
    private var accepted = false
    private var systemAnswered = false
    private var localMute = false
    private var platformMute = false
    private var audio: CallAudio? = null
    private var negotiatedHandle = 0L
    private var platform: TelecomAudioPlatform? = null
    private var note: VoiceNoteAudio? = null
    private var notesJoined = false
    private var noteJoin: Job? = null
    private var teardown: Job? = null
    @Volatile var teardownStage = "idle"; private set
    private var proximity: PowerManager.WakeLock? = null
    private var closed = false
    private var uiState = CallUiState()
    val state get() = uiState
    val busy get() = state.busy

    private fun update(value: CallUiState = state) {
        uiState = value.copy(call = guard.current, epoch = guard.epoch, tearingDown = guard.tearingDown)
        if (DmsgService.callOwner() === this || DmsgService.callOwner() == null) CallUi.mutable.value = uiState
        proximity()
        changed()
    }
    fun listener(f: DmsgFacade, wanted: Boolean) {
        if (closed) return
        facade = f; wantedListener = wanted
        if (wanted) createActor()
        else if (!busy) shutdownActor()
    }
    private fun createActor(after: (() -> Unit)? = null) {
        if (after != null) afterActor = after
        if (commands != null) { runAfterActor(); return }
        if (creating || closed) return
        creating = true
        val token = ++generation
        scope.launch {
            try {
                val f = facade ?: withContext(Dispatchers.IO) { Core.facade(context) }.also { facade = it }
                val created = withContext(Dispatchers.IO) {
                    f.startCalls(object : CallObserver {
                        override fun changed(state: CallSnapshot) {
                            scope.launch { if (!closed && token == generation) receive(state) }
                        }
                    })
                }
                if (closed || token != generation) { withContext(Dispatchers.IO) { created.shutdown() }; return@launch }
                commands = created
                runAfterActor()
            } catch (_: Exception) {
                val wasPreparing = state.preparing
                update(state.copy(preparing = false, incomingAvailable = false, errorRes = R.string.call_unavailable))
                if (wasPreparing) finish(CallEndReason.UNAVAILABLE, signal = false)
            } finally { creating = false }
        }
    }
    private fun runAfterActor() {
        if (commands == null || !actorAvailable || guard.tearingDown) return
        val action = afterActor; afterActor = null
        action?.invoke()
    }
    private fun shutdownActor(): Job? {
        generation++
        afterActor = null
        val actor = commands; commands = null
        actorAvailable = false
        update(state.copy(incomingAvailable = false))
        return if (actor != null) scope.launch(Dispatchers.IO) { runCatching { actor.shutdown() } } else null
    }
    fun dial(contactId: String) {
        if (busy || closed) return
        dialingPeer = contactId
        reserve()
        update(state.copy(preparing = true, errorRes = null, activeSince = null, endedAt = null, audio = null))
        scope.launch {
            try {
                val f = facade ?: withContext(Dispatchers.IO) { Core.facade(context) }.also { facade = it }
                withContext(Dispatchers.IO) {
                    check(callContactAllowed(f.get(contactId)))
                    f.reconnect()
                }
                if (closed || !state.preparing) return@launch
                createActor {
                    if (!command { it.dial(contactId) } && guard.current?.busy() != true)
                        finish(CallEndReason.UNAVAILABLE, signal = false)
                }
            } catch (_: Exception) {
                update(state.copy(preparing = false, errorRes = R.string.call_unavailable))
                finish(CallEndReason.UNAVAILABLE, signal = false)
            }
        }
    }
    private fun command(block: (CallCommands) -> Unit): Boolean {
        return try {
            val actor = commands ?: return false
            block(actor); true
        } catch (_: Exception) {
            update(state.copy(errorRes = R.string.call_unavailable)); false
        }
    }
    private fun reserve() {
        pauseMessages(true); VoiceTransferCoordinator.callPaused(true)
        if (VoiceNoteAudio.callClaim.callBusy) return
        note = VoiceNoteAudio.interruptForCall()
        notesJoined = false
        noteJoin = scope.launch {
            try {
                withContext(Dispatchers.IO) { note?.joinForCall() }
                note?.releaseForCall(); notesJoined = true
                addTelecom(); maybeAudio()
            } catch (_: Exception) { finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true) }
        }
    }
    private fun receive(next: CallSnapshot) {
        if (next.phase in setOf(CallPhase.STOPPED, CallPhase.LISTENING)) {
            actorAvailable = next.available
            if (next.phase == CallPhase.STOPPED && (state.preparing || guard.current?.busy() == true)) {
                finish(if (next.reason != CallEndReason.NONE) next.reason else
                    if (state.preparing) CallEndReason.UNAVAILABLE else CallEndReason.CONNECTION_LOST, signal = false)
            }
            update(state.copy(incomingAvailable = wantedListener && next.available && !busy))
            runAfterActor()
            return
        }
        val newCall = guard.current?.callId != next.callId
        if (!guard.receive(next)) {
            if (next.mediaHandle != 0L && next.mediaHandle != audio?.snapshot()?.activeHandle &&
                next.mediaHandle != guard.current?.mediaHandle)
                scope.launch(Dispatchers.IO) { runCatching { CallMediaJni.stop(next.mediaHandle) } }
            return
        }
        if (next.mediaHandle != 0L) {
            if (negotiatedHandle != 0L && negotiatedHandle != next.mediaHandle) {
                scope.launch(Dispatchers.IO) { runCatching { CallMediaJni.stop(next.mediaHandle) } }
                finish(CallEndReason.INVALID, signal = true)
                return
            }
            negotiatedHandle = next.mediaHandle
        }
        if (next.busy()) {
            reserve()
            update(if (newCall) state.copy(preparing = false, incomingAvailable = false, errorRes = null,
                activeSince = null, endedAt = null, audio = null) else
                state.copy(preparing = false, incomingAvailable = false, errorRes = null))
            addTelecom()
            if (next.phase in setOf(CallPhase.CONNECTING, CallPhase.ACTIVE)) activateOutgoing()
            maybeAudio()
            if (next.phase == CallPhase.ACTIVE && state.activeSince == null)
                update(state.copy(activeSince = SystemClock.elapsedRealtime()))
        } else if (next.phase == CallPhase.ENDED) {
            update(state.copy(preparing = false))
            finish(next.reason, signal = false)
        }
    }
    private fun addTelecom() {
        val call = guard.current ?: return
        if (!call.busy() || !notesJoined || telecomAdding || telecom != null || guard.tearingDown) return
        telecomAdding = true
        val id = call.callId; val token = guard.epoch
        telecomJob = scope.launch {
            try {
                val calls = manager ?: CallsManager(context).also {
                    it.registerAppWithTelecom(CallsManager.CAPABILITY_BASELINE); manager = it
                }
                calls.addCall(CallAttributesCompat(context.getString(R.string.call_private_caller),
                    Uri.fromParts("sip", id, null),
                    if (call.direction == CallDirection.INCOMING) CallAttributesCompat.DIRECTION_INCOMING
                    else CallAttributesCompat.DIRECTION_OUTGOING,
                    callType = CallAttributesCompat.CALL_TYPE_AUDIO_CALL, callCapabilities = 0),
                    onAnswer = {
                        // Wearable/system answer opens the visible permission/answer UI. No microphone here.
                        withContext(Dispatchers.Main.immediate) {
                            if (guard.matches(id, token)) {
                                systemAnswered = true
                                CallActivity.open(context, id, token, answer = true)
                            }
                        }
                    },
                    onDisconnect = { withContext(Dispatchers.Main.immediate) { if (guard.matches(id, token)) end(id, token) } },
                    onSetActive = {
                        withContext(Dispatchers.Main.immediate) {
                            if (guard.matches(id, token) && accepted) {
                                update(state.copy(telecomActive = true)); maybeAudio()
                            }
                        }
                    },
                    onSetInactive = {
                        // No hold support: stop audio immediately, complete teardown off UI.
                        withContext(Dispatchers.Main.immediate) {
                            audio?.requestStop("telecom_interruption")
                            if (guard.matches(id, token)) finish(CallEndReason.LOCAL_ENDED, signal = true)
                        }
                    }) {
                    val control = this
                    scope.launch {
                        if (!guard.matches(id, token)) { control.disconnect(DisconnectCause(DisconnectCause.LOCAL)); return@launch }
                        telecom = control
                        update(state.copy(telecomReady = true))
                        control.launch(Dispatchers.Main.immediate) { control.availableEndpoints.collect { if (guard.matches(id, token)) update(state.copy(endpoints = it)) } }
                        control.launch(Dispatchers.Main.immediate) { control.currentCallEndpoint.collect { endpoint ->
                            if (guard.matches(id, token)) {
                                if (endpoint.type !in setOf(CallEndpointCompat.TYPE_EARPIECE, CallEndpointCompat.TYPE_SPEAKER,
                                        CallEndpointCompat.TYPE_BLUETOOTH, CallEndpointCompat.TYPE_WIRED_HEADSET)) {
                                    finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true)
                                } else {
                                    if (state.endpoint?.identifier != endpoint.identifier) {
                                        platform?.routeChangeExpected(); audio?.hardwareRouteChanged()
                                    }
                                    update(state.copy(endpoint = endpoint))
                                }
                            }
                        } }
                        control.launch(Dispatchers.Main.immediate) { control.isMuted.collect { if (guard.matches(id, token)) { platformMute = it; muteAudio() } } }
                        changed()
                        activateOutgoing()
                    }
                }
                if (guard.matches(id, token)) finish(CallEndReason.LOCAL_ENDED, signal = true)
            } catch (_: Exception) {
                if (guard.matches(id, token)) finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true)
            } finally { telecomAdding = false }
        }
    }
    fun accept(id: String, token: Long) {
        if (!guard.matches(id, token) || !guard.incomingRinging() || accepted || !permission()) return
        val control = telecom ?: run { update(state.copy(errorRes = R.string.call_connecting)); return }
        accepted = true
        scope.launch {
            if ((systemAnswered || control.answer(CallAttributesCompat.CALL_TYPE_AUDIO_CALL) is CallControlResult.Success) && guard.matches(id, token)) {
                update(state.copy(telecomActive = true, errorRes = null))
                if (!guard.matches(id, token)) return@launch
                if (!command { it.accept(id) }) finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true)
            } else if (guard.matches(id, token)) finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true)
        }
    }
    private fun activateOutgoing() {
        val call = guard.current ?: return
        val control = telecom ?: return
        if (call.direction != CallDirection.OUTGOING || call.phase !in setOf(CallPhase.CONNECTING, CallPhase.ACTIVE) ||
            activationPending || state.telecomActive || guard.tearingDown) return
        accepted = true; activationPending = true
        val id = call.callId; val token = guard.epoch
        scope.launch {
            try {
                if (control.setActive() is CallControlResult.Success && guard.matches(id, token)) {
                    update(state.copy(telecomActive = true)); maybeAudio()
                } else if (guard.matches(id, token)) finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true)
            } finally { activationPending = false }
        }
    }
    private fun permission() = context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
    private fun maybeAudio() {
        if (audio != null || !notesJoined || !guard.mayCapture(state.telecomActive, permission())) return
        val call = guard.current ?: return
        val token = guard.epoch
        val routing = TelecomAudioPlatform().also { it.routeChangeExpected() }; platform = routing
        // The microphone FGS type is established while the permission-bearing UI is visible.
        changed()
        if (!guard.mayCapture(state.telecomActive, permission())) return
        audio = CallAudio(context, NativeCallAudioPort(call.mediaHandle), routing, finished = { result ->
            scope.launch {
                if (guard.epoch == token) {
                    update(state.copy(audio = result))
                    guard.audioCompletionReason(token, result)?.let { finish(it, signal = true) }
                }
            }
        }).also { it.setMuted(localMute || platformMute); it.start() }
        scope.launch {
            while (guard.epoch == token && !guard.tearingDown && audio != null) {
                update(state.copy(audio = audio?.snapshot())); delay(500)
            }
        }
    }
    fun reject(id: String, token: Long) {
        if (!guard.matches(id, token)) return
        command { it.reject(id) }; finish(CallEndReason.REJECTED, signal = false)
    }
    fun end(id: String, token: Long) {
        if (guard.matches(id, token)) finish(CallEndReason.LOCAL_ENDED, signal = true)
    }
    fun cancelPreparing(contactId: String) {
        if (state.preparing && dialingPeer == contactId) {
            update(state.copy(errorRes = R.string.call_ended))
            finish(CallEndReason.LOCAL_ENDED, signal = false)
        }
    }
    fun audioUnavailable() { if (busy) finish(CallEndReason.AUDIO_UNAVAILABLE, signal = true) }
    fun mute(id: String, token: Long) {
        if (!guard.matches(id, token)) return
        localMute = !localMute; muteAudio()
    }
    private fun muteAudio() { audio?.setMuted(localMute || platformMute); update(state.copy(muted = localMute || platformMute)) }
    fun endpoint(id: String, token: Long, endpoint: CallEndpointCompat) {
        if (!guard.matches(id, token) || endpoint !in state.endpoints) return
        val control = telecom ?: return
        platform?.routeChangeExpected()
        scope.launch {
            if (control.requestEndpointChange(endpoint) is CallControlResult.Error && guard.matches(id, token))
                update(state.copy(errorRes = R.string.call_route_failed))
        }
    }
    private fun finish(reason: CallEndReason, signal: Boolean) {
        if (teardown?.isActive == true || guard.tearingDown) return
        val call = guard.current
        val cancelPreparing = state.preparing && call?.busy() != true
        val handleToRetire = negotiatedHandle
        guard.beginTeardown()
        teardownStage = "guard_set"
        afterActor = null
        audio?.requestStop("call_ended")
        // Dial can be admitted before its first callback. Cancel its generation
        // even when the UI has no call ID yet; never replay the previous ID.
        if (cancelPreparing) command { it.end("") }
        else if (signal && call != null) command { it.end(call.callId) }
        if (call != null && call.phase != CallPhase.ENDED) guard.receive(call.copy(phase = CallPhase.ENDED, reason = reason))
        update(state.copy(preparing = false, incomingAvailable = false, endedAt = SystemClock.elapsedRealtime()))
        teardown = scope.launch {
            try {
                teardownStage = "audio_join_enter"
                noteJoin?.join()
                val physical = audio
                withContext(Dispatchers.IO) {
                    physical?.join()
                    // A negotiated handle can exist before Telecom activation/permission. It has no audio workers yet.
                    if (physical == null && handleToRetire != 0L) CallMediaJni.stop(handleToRetire)
                }
                update(state.copy(audio = physical?.snapshot() ?: state.audio))
                val cause = when (reason) {
                    CallEndReason.REJECTED -> DisconnectCause.REJECTED
                    CallEndReason.EXPIRED -> DisconnectCause.MISSED
                    CallEndReason.REMOTE_ENDED -> DisconnectCause.REMOTE
                    else -> DisconnectCause.LOCAL
                }
                teardownStage = "disconnect_enter"
                telecom?.disconnect(DisconnectCause(cause))
                teardownStage = "disconnect_return"
                // Core-Telecom 1.0.1 leaves its endpoint/mute channels open on
                // normal disconnect. Stop collectors without cancelling addCall's cleanup.
                telecom?.coroutineContext?.cancelChildren()
                teardownStage = "telecom_join_enter"
                telecomJob?.join()
                teardownStage = "telecom_join_return"
            } finally {
                telecom = null; telecomJob = null; audio = null; platform = null; negotiatedHandle = 0L; dialingPeer = null
                localMute = false; platformMute = false; accepted = false; systemAnswered = false; activationPending = false
                guard.finishTeardown()
                update(state.copy(incomingAvailable = wantedListener && actorAvailable && !closed,
                    telecomActive = false, telecomReady = false, muted = false, endpoint = null, endpoints = emptyList()))
                releaseReservation()
                if (!wantedListener || closed) shutdownActor()
                teardownStage = "idle"
                idle()
            }
        }
    }
    private fun releaseReservation() {
        VoiceNoteAudio.callClaim.releaseCall(); note = null; notesJoined = false
        pauseMessages(false); VoiceTransferCoordinator.callPaused(false); VoiceTransferCoordinator.wake()
    }
    private fun proximity() {
        val wanted = state.telecomActive && state.call?.phase == CallPhase.ACTIVE && !state.tearingDown &&
            state.endpoint?.type == CallEndpointCompat.TYPE_EARPIECE
        if (wanted) {
            val power = context.getSystemService(PowerManager::class.java)
            if (power.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) {
                val lock = proximity ?: power.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK,
                    "dmsg:call-proximity").also { it.setReferenceCounted(false); proximity = it }
                if (!lock.isHeld) lock.acquire()
            }
        } else proximity?.let { if (it.isHeld) it.release() }
    }
    /** Explicit link stop awaits audio retirement before the service cancels DNS. */
    suspend fun stop() {
        wantedListener = false
        if (busy) finish(CallEndReason.LOCAL_ENDED, signal = true)
        teardown?.join()
        shutdownActor()?.join()
        withContext(Dispatchers.IO) { runCatching { facade?.dnsStop() } }
    }
    fun destroy(after: () -> Unit) {
        closed = true
        scope.launch {
            stop(); proximity?.let { if (it.isHeld) it.release() }
            after(); scope.cancel()
        }
    }
}

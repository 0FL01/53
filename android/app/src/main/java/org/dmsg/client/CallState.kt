package org.dmsg.client

import uniffi.dmsg_core.CallDirection
import uniffi.dmsg_core.CallEndReason
import uniffi.dmsg_core.CallPhase
import uniffi.dmsg_core.CallSnapshot

internal fun CallSnapshot.busy() = phase in setOf(CallPhase.DIALING, CallPhase.RINGING,
    CallPhase.CONNECTING, CallPhase.ACTIVE, CallPhase.RECONNECTING)

internal fun callContactAllowed(contact: Dialog?) = contactCta(contact) == ContactCta.Chat &&
    contact?.state in setOf("accepted", "accepted_server")

/** The action token belongs to the logical call, never to a replaceable media epoch. */
internal class CallStateGuard {
    var epoch = System.nanoTime(); private set
    var current: CallSnapshot? = null; private set
    var tearingDown = false; private set
    private var interruptedMediaEpoch = 0uL
    fun receive(next: CallSnapshot): Boolean {
        if (next.callId.isEmpty()) return false
        val before = current
        if (before != null && before.callId != next.callId && (before.busy() || tearingDown)) return false
        if (tearingDown && next.busy()) return false
        if (before?.callId == next.callId && before.phase == CallPhase.ENDED && next.busy()) return false
        if (before?.callId == next.callId && next.phase != CallPhase.ENDED) {
            if (next.mediaEpoch < before.mediaEpoch) return false
            if (next.mediaEpoch == before.mediaEpoch &&
                (before.phase == CallPhase.RECONNECTING && next.phase != CallPhase.RECONNECTING &&
                    next.mediaEpoch <= interruptedMediaEpoch ||
                    before.phase == CallPhase.ACTIVE && next.phase == CallPhase.CONNECTING)) return false
        }
        if (before?.callId != next.callId) {
            epoch++; interruptedMediaEpoch = 0uL
        } else if (next.phase == CallPhase.RECONNECTING && before.phase != CallPhase.RECONNECTING) {
            // The server publishes the reserved epoch before its handle. Only
            // callbacks belonging to the interrupted generation remain stale.
            interruptedMediaEpoch = maxOf(interruptedMediaEpoch, before.mediaEpoch)
        }
        current = next
        return true
    }
    fun matches(id: String, token: Long) = current?.callId == id && token == epoch &&
        current?.busy() == true && !tearingDown
    fun matchesMedia(id: String, token: Long, mediaEpoch: ULong) =
        matches(id, token) && current?.mediaEpoch == mediaEpoch
    fun audioCompletionReason(token: Long, result: CallAudio.Snapshot, expectedStop: Boolean = false): CallEndReason? {
        if (token != epoch || tearingDown || current?.busy() != true) return null
        if (!result.cleanupComplete) return CallEndReason.AUDIO_UNAVAILABLE
        if (expectedStop && result.failure == null) return null
        // Native joins/retirement precede Ended, whose callback may still be queued
        // on Main. Only these native-owned failures defer to the actor's reason;
        // final failed/stopped stats never excuse a physical/Java audio failure.
        return when (result.failure) {
            "native_probe_failed", "native_stopped_early" -> null
            else -> CallEndReason.AUDIO_UNAVAILABLE
        }
    }
    fun beginTeardown() { tearingDown = true }
    fun finishTeardown() { tearingDown = false }
    fun mayCapture(telecomActive: Boolean, permission: Boolean): Boolean {
        val call = current ?: return false
        return !tearingDown && telecomActive && permission && call.mediaHandle != 0L &&
            call.phase in setOf(CallPhase.CONNECTING, CallPhase.ACTIVE)
    }
    fun incomingRinging() = current?.let { it.phase == CallPhase.RINGING && it.direction == CallDirection.INCOMING } == true
}

/** Reserve before interrupting notes; release only after all call workers have joined. */
internal class CallAudioClaim {
    @Volatile var callBusy = false; private set
    @Synchronized fun reserveCall(): Boolean {
        if (callBusy) return false
        callBusy = true; return true
    }
    @Synchronized fun allowNote() = !callBusy
    @Synchronized fun releaseCall() { callBusy = false }
}

package org.dmsg.client

import uniffi.dmsg_core.CallDirection
import uniffi.dmsg_core.CallEndReason
import uniffi.dmsg_core.CallPhase
import uniffi.dmsg_core.CallSnapshot

internal fun CallSnapshot.busy() = phase in setOf(CallPhase.DIALING, CallPhase.RINGING,
    CallPhase.CONNECTING, CallPhase.ACTIVE)

internal fun callContactAllowed(contact: Dialog?) = contactCta(contact) == ContactCta.Chat &&
    contact?.state in setOf("accepted", "accepted_server")

/** Tokens bind every UI/notification action to this service's current call generation. */
internal class CallStateGuard {
    var epoch = System.nanoTime(); private set
    var current: CallSnapshot? = null; private set
    var tearingDown = false; private set
    fun receive(next: CallSnapshot): Boolean {
        if (next.callId.isEmpty()) return false
        val before = current
        if (before != null && before.callId != next.callId && (before.busy() || tearingDown)) return false
        if (before?.callId == next.callId && before.phase == CallPhase.ENDED && next.busy()) return false
        if (before?.callId != next.callId) epoch++
        current = next
        return true
    }
    fun matches(id: String, token: Long) = current?.callId == id && token == epoch &&
        current?.busy() == true && !tearingDown
    fun audioCompletionReason(token: Long, result: CallAudio.Snapshot): CallEndReason? {
        if (token != epoch || tearingDown || current?.busy() != true) return null
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

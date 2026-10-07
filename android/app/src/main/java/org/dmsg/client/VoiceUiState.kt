package org.dmsg.client

import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageKind
import java.security.SecureRandom

internal enum class VoiceMode { Idle, Holding, Locked, Pausing, Paused, Finishing, Preview, Queueing }
internal enum class VoiceGesture { None, Cancel, Lock }

/** Main-thread, memory-only draft. No Bundle, files, preferences or process-death restoration. */
internal class VoiceUiState {
    var mode = VoiceMode.Idle
    var samples = 0
    var rms = 0f
    var bytes: ByteArray? = null
    var waveform = byteArrayOf()
    var sendOnFinish = false
    var generation = 0L
    var observer: (() -> Unit)? = null
    val busy get() = mode != VoiceMode.Idle
    fun changed() { observer?.invoke() }
    fun begin(locked: Boolean = false): Boolean {
        if (busy) return false
        generation++; samples = 0; rms = 0f; sendOnFinish = false
        mode = if (locked) VoiceMode.Locked else VoiceMode.Holding
        changed(); return true
    }
    fun move(dx: Float, dy: Float, threshold: Float): VoiceGesture {
        if (mode != VoiceMode.Holding) return VoiceGesture.None
        return when {
            dx <= -threshold -> VoiceGesture.Cancel
            dy <= -threshold -> { mode = VoiceMode.Locked; changed(); VoiceGesture.Lock }
            else -> VoiceGesture.None
        }
    }
    fun finish(send: Boolean) {
        if (mode !in setOf(VoiceMode.Holding, VoiceMode.Locked, VoiceMode.Pausing, VoiceMode.Paused)) return
        sendOnFinish = send; mode = VoiceMode.Finishing; changed()
    }
    fun background() {
        sendOnFinish = false
        if (mode in setOf(VoiceMode.Holding, VoiceMode.Locked, VoiceMode.Pausing, VoiceMode.Paused)) finish(false)
    }
    fun preview(encoded: ByteArray, count: Int, bars: ByteArray) {
        bytes?.fill(0); bytes = encoded; samples = count; waveform = bars
        mode = VoiceMode.Preview; rms = 0f; changed()
    }
    fun discard() {
        generation++; bytes?.fill(0); bytes = null; waveform = byteArrayOf()
        samples = 0; rms = 0f; sendOnFinish = false; mode = VoiceMode.Idle; changed()
    }
}

internal fun voiceTime(samples: Int): String {
    val seconds = samples.coerceAtLeast(0) / 16_000
    return "%d:%02d".format(java.util.Locale.ROOT, seconds / 60, seconds % 60)
}
internal fun sameHistoryRows(left: List<HistoryMessage>, right: List<HistoryMessage>): Boolean =
    left.size == right.size && left.indices.all { index ->
        val a = left[index]; val b = right[index]
        val av = a.voice; val bv = b.voice
        a.copy(voice = null) == b.copy(voice = null) &&
            (if (av == null || bv == null) av == null && bv == null else av.sampleCount == bv.sampleCount &&
                av.byteLen == bv.byteLen && av.downloaded == bv.downloaded && av.waveform.contentEquals(bv.waveform))
    }
internal data class VoiceKey(val contactId: String, val localId: Long, val mid: String) {
    constructor(row: HistoryMessage) : this(row.contactId, row.localId, row.messageIdHex)
    fun matches(row: HistoryMessage) = contactId == row.contactId && localId == row.localId && mid == row.messageIdHex &&
        row.kind == MessageKind.VOICE && messageVisible(row)
}
internal data class VoiceSendAttempt(val contactId: String, val mid: String = newVoiceMid())
private fun newVoiceMid() = ByteArray(16).also { SecureRandom().nextBytes(it) }
    .joinToString("") { "%02x".format(java.util.Locale.ROOT, it.toInt() and 255) }
internal sealed interface VoiceSendOutcome {
    data class Saved(val row: HistoryMessage, val recovered: Boolean = false) : VoiceSendOutcome
    data class NotSaved(val error: Throwable) : VoiceSendOutcome
    data class Uncertain(val attempt: VoiceSendAttempt, val error: Throwable) : VoiceSendOutcome
}

/** Exact MID proof, including hidden queued rows. Never infer durability from upload success. */
internal fun reconcileVoiceSend(f: DmsgFacade, attempt: VoiceSendAttempt, error: Throwable): VoiceSendOutcome = try {
    val row = f.historyMessageByMid(attempt.contactId, attempt.mid)
    if (row == null) VoiceSendOutcome.NotSaved(error)
    else if (row.messageIdHex == attempt.mid && row.contactId == attempt.contactId && row.kind == MessageKind.VOICE &&
        row.direction == uniffi.dmsg_core.MessageDirection.OUTGOING) VoiceSendOutcome.Saved(row, true)
    else VoiceSendOutcome.Uncertain(attempt, error)
} catch (_: Exception) { VoiceSendOutcome.Uncertain(attempt, error) }

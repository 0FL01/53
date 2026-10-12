package org.dmsg.client

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper

/** Standalone gate-only routing/focus/fixture boundary; production uses Telecom instead. */
class CallProbeAudio(context: Context, fixturePath: String? = null, outputMuted: Boolean = false,
    loudspeaker: Boolean = false) {
    companion object {
        const val SAMPLE_RATE = CallAudio.SAMPLE_RATE
        const val BATCH_SAMPLES = CallAudio.BATCH_SAMPLES
        const val QUEUE_BOUND_SAMPLES = CallAudio.QUEUE_BOUND_SAMPLES
    }
    private val audio = CallAudio(context, ProbePort(fixturePath), ProbePlatform(context, loudspeaker), outputMuted)
    fun start() = audio.start()
    fun requestStop(reason: String = "explicit_stop") = audio.requestStop(reason)
    fun setPlaybackRateOffsetHz(offset: Int) = audio.setPlaybackRateOffsetHz(offset)
    fun snapshot() = audio.snapshot()

    private class ProbePort(private val fixture: String?) : CallAudioPort {
        override val dns get() = fixture != null
        override fun acquire() = if (fixture == null) CallProbeJni.startLocal() else CallProbeJni.startDns(fixture)
        override fun push(handle: Long, pcm: ShortArray, valid: Int, readPosition: Long, frame: Long, ns: Long) =
            CallProbeJni.push(handle, pcm, valid, readPosition, frame, ns)
        override fun pull(handle: Long, pcm: ShortArray, valid: Int) = CallProbeJni.pull(handle, pcm, valid)
        override fun sinkQueued(handle: Long, samples: Int) = CallProbeJni.sinkQueued(handle, samples)
        override fun sinkRate(handle: Long, ppb: Long) = CallProbeJni.sinkRate(handle, ppb)
        override fun renderExpiredSamples(handle: Long) = CallProbeJni.renderExpiredSamples(handle)
        override fun stats(handle: Long) = CallProbeJni.stats(handle)
        override fun stop(handle: Long) = CallProbeJni.stop(handle)
    }
    private class ProbePlatform(private val context: Context, private val speaker: Boolean) : CallAudioPlatform {
        override val probeMode = true
        override var evidence = CallPlatformEvidence(); private set
        override val cleanupComplete get() = (!modeOwned || evidence.restoredMode == evidence.previousMode) &&
            (!evidence.focusGranted || evidence.focusAbandoned) && evidence.outputRouteRestored
        private var manager: AudioManager? = null
        private var focus: AudioFocusRequest? = null
        private var modeOwned = false
        private var speakerOwned = false
        private var previousSpeaker = false
        private fun gate(condition: Boolean, diagnostic: String) { if (!condition) throw CallPlatformFailure(diagnostic) }
        override fun prepare(manager: AudioManager, fail: (String) -> Unit) {
            gate(context.packageName == "org.dmsg.client.gate", "gate_package_required")
            this.manager = manager
            evidence = evidence.copy(previousMode = manager.mode)
            if (speaker && Build.VERSION.SDK_INT < 31) previousSpeaker = manager.isSpeakerphoneOn
            val attributes = AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build()
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attributes).setAcceptsDelayedFocusGain(false).setWillPauseWhenDucked(true)
                .setOnAudioFocusChangeListener({ change -> if (change < 0) fail("audio_focus_lost") }, Handler(Looper.getMainLooper()))
                .build()
            focus = request
            evidence = evidence.copy(focusGranted = manager.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED)
            gate(evidence.focusGranted, "foreground_audio_focus_denied")
            modeOwned = true; manager.mode = AudioManager.MODE_IN_COMMUNICATION
            evidence = evidence.copy(communicationMode = manager.mode)
            gate(evidence.communicationMode == AudioManager.MODE_IN_COMMUNICATION, "communication_mode_failed")
            if (speaker) {
                speakerOwned = true; evidence = evidence.copy(outputRouteRestored = false)
                if (Build.VERSION.SDK_INT >= 31) {
                    val device = manager.availableCommunicationDevices.firstOrNull { it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
                    gate(device != null && manager.setCommunicationDevice(device), "speaker_selection_failed")
                } else { manager.isSpeakerphoneOn = true; gate(manager.isSpeakerphoneOn, "speaker_selection_failed") }
            }
        }
        override fun checkMicrophone(muted: Boolean) { gate(!muted, "microphone_muted") }
        override fun checkRoute(input: Boolean, previousId: Int, route: AudioDeviceInfo) {
            gate(input || !speaker || route.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER, "speaker_route_not_selected")
            gate(previousId == 0 || previousId == route.id, if (input) "input_route_changed" else "output_route_changed")
        }
        override fun release() {
            val audio = manager ?: return
            var failed = false
            if (speakerOwned) runCatching {
                if (Build.VERSION.SDK_INT >= 31) audio.clearCommunicationDevice()
                else { audio.isSpeakerphoneOn = previousSpeaker; check(audio.isSpeakerphoneOn == previousSpeaker) }
                evidence = evidence.copy(outputRouteRestored = true)
            }.onFailure { failed = true }
            focus?.let { request -> runCatching {
                evidence = evidence.copy(focusAbandoned = audio.abandonAudioFocusRequest(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED)
                check(evidence.focusAbandoned)
            }.onFailure { failed = true } }
            if (modeOwned) runCatching {
                audio.mode = evidence.previousMode; evidence = evidence.copy(restoredMode = audio.mode)
                check(evidence.restoredMode == evidence.previousMode)
            }.onFailure { failed = true }
            check(!failed)
        }
    }
}

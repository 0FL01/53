package org.dmsg.client

import android.media.AudioDeviceInfo
import android.media.AudioManager

internal data class CallPlatformEvidence(val previousMode: Int = -1, val communicationMode: Int = -1,
    val restoredMode: Int = -1, val focusGranted: Boolean = false, val focusAbandoned: Boolean = false,
    val outputRouteRestored: Boolean = true)

internal class CallPlatformFailure(val diagnostic: String) : RuntimeException()

/** Exactly two consumers: Telecom production owner and standalone debug probe owner. */
internal interface CallAudioPlatform {
    val probeMode: Boolean
    val evidence: CallPlatformEvidence
    val cleanupComplete: Boolean
    fun prepare(manager: AudioManager, fail: (String) -> Unit)
    fun checkMicrophone(muted: Boolean)
    fun checkRoute(input: Boolean, previousId: Int, route: AudioDeviceInfo)
    fun release()
}

internal class TelecomAudioPlatform : CallAudioPlatform {
    override val probeMode = false
    override val evidence get() = CallPlatformEvidence(communicationMode = AudioManager.MODE_IN_COMMUNICATION)
    override val cleanupComplete = true // Telecom disconnect is owned by the session, after audio joins.
    @Volatile private var routeChangeUntil = 0L
    fun routeChangeExpected() { routeChangeUntil = System.nanoTime() + 3_000_000_000L }
    override fun prepare(manager: AudioManager, fail: (String) -> Unit) {
        check(manager.mode == AudioManager.MODE_IN_COMMUNICATION) { "Telecom audio not activated" }
    }
    override fun checkMicrophone(muted: Boolean) { /* Telecom/local mute zeros PCM, never changes global mute. */ }
    override fun checkRoute(input: Boolean, previousId: Int, route: AudioDeviceInfo) {
        val supported = if (input) route.type in setOf(AudioDeviceInfo.TYPE_BUILTIN_MIC,
            AudioDeviceInfo.TYPE_WIRED_HEADSET, AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
            AudioDeviceInfo.TYPE_USB_HEADSET, AudioDeviceInfo.TYPE_BLE_HEADSET)
        else route.type in setOf(AudioDeviceInfo.TYPE_BUILTIN_EARPIECE, AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
            AudioDeviceInfo.TYPE_WIRED_HEADSET, AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
            AudioDeviceInfo.TYPE_BLUETOOTH_SCO, AudioDeviceInfo.TYPE_USB_HEADSET,
            AudioDeviceInfo.TYPE_BLE_HEADSET, AudioDeviceInfo.TYPE_BLE_SPEAKER)
        check(supported) { "Unsupported call route" }
        check(previousId == 0 || previousId == route.id || System.nanoTime() <= routeChangeUntil) {
            "Unexpected call route change"
        }
    }
    override fun release() = Unit
}

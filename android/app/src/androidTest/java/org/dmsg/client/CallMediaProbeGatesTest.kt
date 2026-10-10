package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.AudioEffect
import android.media.audiofx.NoiseSuppressor
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructStat
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.abs

/** Exact-method .gate probes. Local memory-pair progress is not DNS or call-quality acceptance.
 * Operator grants RECORD_AUDIO before the run; changing/revoking it here can kill the UID.
 */
@RunWith(AndroidJUnit4::class)
class CallMediaProbeGatesTest {
    @get:Rule val testName = TestName()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val arguments get() = InstrumentationRegistry.getArguments()

    private fun gate(): Context {
        val app = ApplicationProvider.getApplicationContext<Context>()
        assertEquals("org.dmsg.client.gate", app.packageName)
        assertEquals("select one exact probe method", "${javaClass.name}#${testName.methodName}", arguments.getString("class"))
        assertFalse("isolated foreground owner requires stopped DNS service", DmsgService.running(app))
        return app
    }

    private fun await(label: String, timeoutMs: Long = 10_000, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeoutMs
        while (!condition()) {
            assertTrue(label, SystemClock.elapsedRealtime() < deadline)
            Thread.sleep(50)
        }
    }

    /** Sanitized counters/timestamps only, collected by am instrument's result channel. */
    private fun evidence(name: String, value: JSONObject) {
        instrumentation.sendStatus(0, Bundle().apply { putString(name, value.toString()) })
    }

    private fun finishMarkerStat(marker: File): StructStat? = try {
        Os.lstat(marker.absolutePath)
    } catch (error: ErrnoException) {
        if (error.errno == OsConstants.ENOENT) null else throw error
    }

    private fun finishMarker(app: Context, path: String): File {
        val marker = File(path)
        val root = File(app.filesDir.canonicalFile, "voice-probe")
        assertTrue("probe_finish_marker_path must be absolute/canonical inside filesDir/voice-probe/<fixture-run>/",
            marker.isAbsolute && marker.absolutePath == marker.canonicalPath && marker.parentFile?.parentFile == root)
        listOf(root, marker.parentFile!!).forEach { directory ->
            val stat = Os.lstat(directory.absolutePath)
            assertTrue("finish marker directories must be owned directories, never symlinks",
                OsConstants.S_ISDIR(stat.st_mode) && stat.st_uid == Process.myUid())
        }
        assertNull("finish marker must be new for this interval", finishMarkerStat(marker))
        return marker
    }

    /** Controller atomically publishes byte 0x01, owner0400, only after both frozen intervals. */
    private fun awaitFinishMarker(marker: File) {
        val deadline = SystemClock.elapsedRealtime() + 15_000
        while (SystemClock.elapsedRealtime() < deadline) {
            assertEquals("finish marker path must remain canonical/non-symlink", marker.absolutePath, marker.canonicalPath)
            val stat = finishMarkerStat(marker)
            if (stat != null) {
                assertTrue("finish marker must be a UID-private regular owner0400 one-byte file",
                    OsConstants.S_ISREG(stat.st_mode) && stat.st_uid == Process.myUid() &&
                        (stat.st_mode and 0xfff) == 0x100 && stat.st_size == 1L)
                val fd = Os.open(marker.absolutePath,
                    OsConstants.O_RDONLY or OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW or OsConstants.O_NONBLOCK, 0)
                try {
                    val opened = Os.fstat(fd)
                    assertTrue("finish marker must remain the validated regular owner0400 file",
                        OsConstants.S_ISREG(opened.st_mode) && opened.st_uid == Process.myUid() &&
                            (opened.st_mode and 0xfff) == 0x100 && opened.st_size == 1L &&
                            opened.st_dev == stat.st_dev && opened.st_ino == stat.st_ino)
                    val bytes = ByteArray(2)
                    assertEquals("finish marker has exactly one byte", 1, Os.read(fd, bytes, 0, bytes.size))
                    assertEquals("finish marker release byte is 0x01", 1, bytes[0].toInt())
                    assertEquals("finish marker has no trailing bytes", 0, Os.read(fd, bytes, 0, bytes.size))
                    assertTrue("finish marker must arrive within 15s", SystemClock.elapsedRealtime() <= deadline)
                } finally {
                    Os.close(fd)
                }
                return
            }
            val remaining = deadline - SystemClock.elapsedRealtime()
            if (remaining > 0) Thread.sleep(minOf(50L, remaining))
        }
        fail("paired finish marker did not arrive in 15s")
    }

    @Test(timeout = 30_000) fun packagedLiveCodecActivationOnlyInGatePackage() {
        gate()
        val value = JSONObject(CallProbeJni.codecEvidence())
        val clean = JSONObject()
        listOf("nolace_changed_samples", "deep_plc_changed_samples", "wideband_packets", "lookahead_samples")
            .forEach { clean.put(it, value.getLong(it)) }
        evidence("call_probe_codec", clean)
        assertTrue("actual packaged WB40 NoLACE changes decoded PCM", value.getLong("nolace_changed_samples") > 0)
        assertTrue("actual isolated Deep PLC changes decoded PCM", value.getLong("deep_plc_changed_samples") > 0)
        assertTrue("WB40 packet fixture", value.getLong("wideband_packets") > 50)
        assertEquals(104, value.getInt("lookahead_samples"))
    }

    @Test(timeout = 90_000) fun foregroundMicrophoneProtectedLoopbackAndCleanupOnlyInGatePackage() {
        val seconds = arguments.getString("probe_local_duration_seconds")?.toInt() ?: 8
        assertTrue("local actuator/expiry diagnostic interval is 8..16 seconds", seconds in 8..16)
        runAudio(gate(), null, seconds * 1_000L, exerciseRate = true)
    }

    /** Sequential input counters only. The operator's external speech stimulus is not generated here. */
    @Test(timeout = 60_000) fun foregroundMicrophoneSourceDiagnosticOnlyInGatePackage() {
        val app = gate()
        assertEquals("capture worker must run in the target UID", app.applicationInfo.uid, Process.myUid())
        assertEquals("operator must externally grant RECORD_AUDIO before instrumentation; denial is not PASS",
            PackageManager.PERMISSION_GRANTED, app.checkSelfPermission(Manifest.permission.RECORD_AUDIO))
        val intent = Intent(app, CallProbeActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val sources = listOf("MIC" to MediaRecorder.AudioSource.MIC,
            "VOICE_COMMUNICATION" to MediaRecorder.AudioSource.VOICE_COMMUNICATION)
        val results = sources.mapIndexed { index, (name, id) ->
            val initial = JSONObject().put("source_name", name).put("source_id", id).put("effects_requested", index == 1)
                .put("failure", "source_not_finished").put("cleanup_complete", false)
            listOf("duration_ms", "captured_samples", "zero_samples", "nonzero_samples", "peak", "clipped_samples",
                "sample_rate", "channel_count", "channel_mask", "encoding", "input_route_id", "input_route_type",
                "timestamp_available", "first_frame", "first_monotonic_ns", "last_frame", "last_monotonic_ns",
                "frame_monotonic", "record_stopped", "record_released", "effects_released").forEach { initial.put(it, JSONObject.NULL) }
            listOf("aec", "ns").forEach { effect ->
                initial.put(effect, JSONObject().apply {
                    listOf("available", "created", "enable_result", "control", "enabled").forEach { put(it, JSONObject.NULL) }
                })
            }
            AtomicReference(initial)
        }
        val stopping = AtomicBoolean(false)
        val failure = AtomicReference<String?>(null)
        val manager = app.getSystemService(AudioManager::class.java)
        var worker: Thread? = null
        var joined = false
        var focus: AudioFocusRequest? = null
        var focusGranted = false
        var focusAbandoned = false
        var modeOwned = false
        var previousMode = -1
        var restoredMode = -1
        try {
            ActivityScenario.launch<CallProbeActivity>(intent).use { scenario ->
                fun foreground(): Boolean {
                    var visible = false
                    scenario.onActivity {
                        assertNull("diagnostic must not start the protected audio/native owner", it.probeOwner)
                        assertNull(it.probeFailure)
                        visible = it.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED) &&
                            it.hasWindowFocus() && it.window.decorView.isShown
                    }
                    return visible
                }
                await("visible resumed target window required") { foreground() }
                previousMode = manager.mode
                focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                    .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build())
                    .setAcceptsDelayedFocusGain(false).setWillPauseWhenDucked(true)
                    .setOnAudioFocusChangeListener({ change ->
                        if (change < 0 && !stopping.get()) {
                            failure.compareAndSet(null, "audio_focus_lost"); stopping.set(true)
                        }
                    }, Handler(Looper.getMainLooper())).build()
                focusGranted = manager.requestAudioFocus(focus!!) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
                assertTrue("foreground audio focus required", focusGranted)
                modeOwned = true
                manager.mode = AudioManager.MODE_IN_COMMUNICATION
                assertEquals(AudioManager.MODE_IN_COMMUNICATION, manager.mode)
                assertTrue("target must remain foreground before capture", foreground())
                worker = Thread({
                    val pcm = ShortArray(160)
                    try {
                        Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO)
                        sources.forEachIndexed { index, (_, id) ->
                            if (!stopping.get()) captureMicrophoneSource(app, manager, stopping, failure,
                                id, index == 1, pcm, results[index])
                        }
                    } catch (_: Throwable) {
                        failure.compareAndSet(null, "microphone_source_worker_failed")
                    } finally { pcm.fill(0) }
                }, "call-probe-microphone-sources").also { it.start() }
                // This observer owns the stop deadline independently of capture/platform calls.
                try {
                    await("microphone source worker exceeded 20s", 20_000) {
                        assertTrue("capture requires the visible resumed target window", foreground())
                        assertEquals("communication mode changed", AudioManager.MODE_IN_COMMUNICATION, manager.mode)
                        assertFalse("DNS service must remain stopped", DmsgService.running(app))
                        !worker!!.isAlive
                    }
                } finally { stopping.set(true) }
            }
        } catch (error: Throwable) {
            failure.compareAndSet(null, "source_diagnostic_foreground_or_observer_failed")
            throw error
        } finally {
            stopping.set(true)
            try {
                worker?.let { capture ->
                    try { await("source cleanup incomplete in 10s", 10_000) { !capture.isAlive } }
                    finally { capture.join(1_000); joined = !capture.isAlive }
                }
            } finally {
                if (worker != null && !joined) failure.compareAndSet(null, "worker_cleanup_timeout")
                try {
                    focus?.let { focusAbandoned = manager.abandonAudioFocusRequest(it) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED }
                    if (focusGranted && !focusAbandoned) failure.compareAndSet(null, "audio_focus_abandon_failed")
                } catch (_: Throwable) { failure.compareAndSet(null, "audio_focus_abandon_failed") }
                try {
                    if (modeOwned) { manager.mode = previousMode; restoredMode = manager.mode }
                    if (modeOwned && restoredMode != previousMode) failure.compareAndSet(null, "audio_mode_restore_failed")
                } catch (_: Throwable) { failure.compareAndSet(null, "audio_mode_restore_failed") }
                val raw = results[0].get(); val processed = results[1].get()
                val complete = raw.optBoolean("cleanup_complete") && processed.optBoolean("cleanup_complete") &&
                    joined && (!focusGranted || focusAbandoned) && modeOwned && restoredMode == previousMode
                val finding = when {
                    !complete || failure.get() != null -> "source_diagnostic_incomplete"
                    raw.getInt("peak") == 0 && processed.getInt("peak") == 0 -> "all_zero_source_diagnostic_not_voice_acceptance"
                    raw.getInt("peak") > 0 && processed.getInt("peak") == 0 -> "mic_nonzero_voice_communication_zero_counter_difference_only"
                    raw.getInt("peak") == 0 -> "mic_zero_voice_communication_nonzero_counter_difference_only"
                    else -> "both_sources_nonzero_counters_only"
                }
                evidence("call_probe_microphone_sources", JSONObject().put("evidence_scope", "foreground_sequential_source_counters_only")
                    .put("voice_acceptance", false).put("finding", finding).put("mic", raw).put("voice_communication", processed)
                    .put("failure", failure.get() ?: JSONObject.NULL).put("worker_joined", joined)
                    .put("worker_alive", worker?.isAlive == true).put("cleanup_complete", complete)
                    .put("focus_granted", focusGranted).put("focus_abandoned", focusAbandoned)
                    .put("previous_mode", previousMode).put("restored_mode", restoredMode)
                    .put("restored_mode_ok", modeOwned && restoredMode == previousMode))
            }
        }
        assertNull("source diagnostic failed", failure.get())
        assertTrue("capture worker joined", joined)
        assertTrue("foreground audio focus abandoned", focusAbandoned)
        assertEquals("audio mode restored", previousMode, restoredMode)
        results.forEach { assertTrue("source resources released", it.get().getBoolean("cleanup_complete")) }
        // Both frozen source findings have already been emitted, including a raw/processed difference.
        assertTrue("all_zero_source_diagnostic_not_voice_acceptance",
            results.any { it.get().getInt("peak") > 0 })
    }

    private fun captureMicrophoneSource(app: Context, manager: AudioManager, stopping: AtomicBoolean,
        failure: AtomicReference<String?>, source: Int, withEffects: Boolean, pcm: ShortArray,
        result: AtomicReference<JSONObject>) {
        val report = JSONObject(result.get().toString()).put("capture_uid", Process.myUid())
            .put("worker_priority", Process.getThreadPriority(Process.myTid()))
        var record: AudioRecord? = null
        var aec: AcousticEchoCanceler? = null
        var ns: NoiseSuppressor? = null
        var captured = 0L; var zeros = 0L; var peak = 0; var clipped = 0L
        var firstFrame = -1L; var firstTime = -1L; var lastFrame = -1L; var lastTime = -1L
        var unavailable = 0L; var monotonic = true
        var routeId = 0; var routeType = 0
        var micMuted = false; var silenced = false; var silencingObservable = false
        var started = 0L; var duration = 0L
        var sourceFailure: String? = null
        var stopped = false; var recordReleased = false; var effectsReleased = false
        fun requireSource(ok: Boolean, code: String) {
            if (!ok) { sourceFailure = code; throw AssertionError(code) }
        }
        fun effectState(available: Boolean, effect: AudioEffect?, enable: Boolean): JSONObject {
            val enabledResult = if (enable && effect != null) effect.setEnabled(true) else null
            return JSONObject().put("available", available).put("created", effect != null)
                .put("enable_result", enabledResult ?: JSONObject.NULL).put("control", effect?.hasControl() == true)
                .put("enabled", effect?.enabled == true)
        }
        try {
            requireSource(app.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED,
                "microphone_permission_denied")
            val minimum = AudioRecord.getMinBufferSize(16_000, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            requireSource(minimum > 0, "unsupported_16k_audio_path")
            val bufferBytes = maxOf(minimum, 4 * 160 * 2)
            report.put("record_min_bytes", minimum).put("requested_buffer_bytes", bufferBytes)
            record = AudioRecord.Builder().setAudioSource(source).setAudioFormat(AudioFormat.Builder()
                .setSampleRate(16_000).setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build()).setBufferSizeInBytes(bufferBytes).build()
            requireSource(record.state == AudioRecord.STATE_INITIALIZED, "record_initialization_failed")
            report.put("sample_rate", record.sampleRate).put("channel_count", record.channelCount)
                .put("channel_mask", record.channelConfiguration).put("encoding", record.audioFormat)
                .put("record_buffer_frames", record.bufferSizeInFrames).put("record_source_id", record.audioSource)
            requireSource(record.sampleRate == 16_000 && record.channelCount == 1 &&
                record.audioFormat == AudioFormat.ENCODING_PCM_16BIT, "record_format_mismatch")
            requireSource(record.audioSource == source, "record_source_mismatch")
            val aecAvailable = AcousticEchoCanceler.isAvailable(); val nsAvailable = NoiseSuppressor.isAvailable()
            if (withEffects && aecAvailable) aec = AcousticEchoCanceler.create(record.audioSessionId)
            if (withEffects && nsAvailable) ns = NoiseSuppressor.create(record.audioSessionId)
            val aecState = effectState(aecAvailable, aec, withEffects)
            val nsState = effectState(nsAvailable, ns, withEffects)
            report.put("aec", aecState).put("ns", nsState)
            if (withEffects) listOf(aecState, nsState).filter { it.getBoolean("available") }.forEach {
                requireSource(it.getBoolean("created") && it.optInt("enable_result", -1) == AudioEffect.SUCCESS &&
                    it.getBoolean("control") && it.getBoolean("enabled"), "available_effect_enable_failed")
            }
            requireSource(!stopping.get(), "source_stopped_before_start")
            record.startRecording()
            requireSource(record.recordingState == AudioRecord.RECORDSTATE_RECORDING, "microphone_start_failed")
            started = SystemClock.elapsedRealtime()
            val deadline = started + 6_000
            val timestamp = AudioTimestamp()
            var nextObservation = 0L
            while (!stopping.get() && SystemClock.elapsedRealtime() < deadline) {
                // Nonblocking read plus two independent deadlines avoids an unbounded recording read thread.
                val count = record.read(pcm, 0, pcm.size, AudioRecord.READ_NON_BLOCKING)
                requireSource(count in 0..pcm.size, if (count < 0) "capture_read_error_$count" else "capture_read_invalid")
                // Every returned partial read is counted exactly once; there is no PCM FIFO or padded batch.
                for (index in 0 until count) {
                    val magnitude = abs(pcm[index].toInt())
                    peak = maxOf(peak, magnitude)
                    if (magnitude == 0) zeros++
                    if (magnitude >= 32767) clipped++
                }
                captured += count
                pcm.fill(0)
                val now = SystemClock.elapsedRealtime()
                if (now >= nextObservation) {
                    micMuted = micMuted || manager.isMicrophoneMute
                    if (Build.VERSION.SDK_INT >= 29) {
                        val config = manager.activeRecordingConfigurations.firstOrNull { it.clientAudioSessionId == record.audioSessionId }
                        silencingObservable = silencingObservable || config != null
                        silenced = silenced || config?.isClientSilenced == true
                    }
                    requireSource(!micMuted && !silenced, "microphone_muted_or_system_silenced")
                    requireSource(record.recordingState == AudioRecord.RECORDSTATE_RECORDING, "microphone_stopped_early")
                    record.routedDevice?.let { route ->
                        requireSource(routeId == 0 || routeId == route.id, "input_route_changed")
                        routeId = route.id; routeType = route.type
                    } ?: requireSource(routeId == 0, "input_route_lost")
                    listOf(aecState to aec, nsState to ns).forEach { (state, effect) ->
                        if (effect != null) {
                            state.put("control", effect.hasControl()).put("enabled", effect.enabled)
                            requireSource(state.getBoolean("control") && state.getBoolean("enabled"), "effect_control_lost")
                        }
                    }
                    if (record.getTimestamp(timestamp, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS &&
                        timestamp.framePosition >= 0 && timestamp.nanoTime > 0) {
                        monotonic = monotonic && (lastFrame < 0 ||
                            (timestamp.framePosition >= lastFrame && timestamp.nanoTime >= lastTime))
                        if (firstFrame < 0) { firstFrame = timestamp.framePosition; firstTime = timestamp.nanoTime }
                        lastFrame = timestamp.framePosition; lastTime = timestamp.nanoTime
                    } else unavailable++
                    nextObservation = now + 100
                }
                if (count == 0) Thread.sleep(2)
            }
            duration = SystemClock.elapsedRealtime() - started
            requireSource(!stopping.get() && duration >= 6_000, "source_interval_stopped_early")
            requireSource(captured >= 16_000, "microphone_no_progress")
            requireSource(routeId > 0 && routeType > 0, "input_route_unobserved")
            requireSource(firstFrame < 0 || (monotonic && lastFrame > firstFrame && lastTime > firstTime),
                "available_capture_timestamp_did_not_advance")
        } catch (_: Throwable) {
            if (sourceFailure == null) sourceFailure = "microphone_source_capture_failed"
            failure.compareAndSet(null, sourceFailure); stopping.set(true)
        } finally {
            pcm.fill(0)
            if (started != 0L && duration == 0L) duration = SystemClock.elapsedRealtime() - started
            try {
                record?.let { if (it.recordingState == AudioRecord.RECORDSTATE_RECORDING) it.stop() }
                stopped = record == null || record.recordingState != AudioRecord.RECORDSTATE_RECORDING
            } catch (_: Throwable) { failure.compareAndSet(null, "record_stop_failed") }
            try { aec?.release(); aec = null } catch (_: Throwable) { failure.compareAndSet(null, "aec_release_failed") }
            try { ns?.release(); ns = null } catch (_: Throwable) { failure.compareAndSet(null, "ns_release_failed") }
            effectsReleased = aec == null && ns == null
            try { record?.release(); recordReleased = true } catch (_: Throwable) { failure.compareAndSet(null, "record_release_failed") }
            result.set(report.put("duration_ms", duration).put("captured_samples", captured).put("zero_samples", zeros)
                .put("nonzero_samples", captured - zeros).put("peak", peak).put("clipped_samples", clipped)
                .put("input_route_id", routeId).put("input_route_type", routeType)
                .put("mic_muted", micMuted).put("system_silenced", silenced).put("silencing_observable", silencingObservable)
                .put("timestamp_available", firstFrame >= 0).put("timestamp_unavailable_observations", unavailable)
                .put("first_frame", if (firstFrame >= 0) firstFrame else JSONObject.NULL)
                .put("first_monotonic_ns", if (firstTime > 0) firstTime else JSONObject.NULL)
                .put("last_frame", if (lastFrame >= 0) lastFrame else JSONObject.NULL)
                .put("last_monotonic_ns", if (lastTime > 0) lastTime else JSONObject.NULL)
                .put("frame_monotonic", if (firstFrame >= 0) monotonic && lastFrame > firstFrame && lastTime > firstTime else JSONObject.NULL)
                .put("failure", sourceFailure ?: JSONObject.NULL).put("record_stopped", stopped)
                .put("record_released", recordReleased).put("effects_released", effectsReleased)
                .put("cleanup_complete", stopped && recordReleased && effectsReleased))
            if (failure.get() != null) stopping.set(true)
        }
    }

    /** Required instrumentation arguments: probe_fixture_path (private owner0400 file),
     * probe_duration_seconds (6..3600). Missing/invalid inputs fail, never skip.
     * Optional probe_finish_marker_path: new canonical filesDir/voice-probe/<fixture-run>/ marker.
     * Paired controller releases owner0400 byte 0x01 after both call_probe_interval_complete statuses.
     * Peer/relay topology and actual DNS service rate must be independently documented.
     */
    @Test(timeout = 3_840_000) fun foregroundMicrophoneProtectedDnsAndCleanupOnlyInGatePackage() {
        val app = gate()
        val path = arguments.getString(CallProbeActivity.EXTRA_FIXTURE_PATH)
        assertFalse("required explicit probe_fixture_path", path.isNullOrBlank())
        val duration = arguments.getString("probe_duration_seconds")?.toLongOrNull()
        assertTrue("required probe_duration_seconds in 6..3600", duration != null && duration in 6..3600)
        val finishMarker = arguments.getString("probe_finish_marker_path")?.let { finishMarker(app, it) }
        runAudio(app, path!!, duration!! * 1_000, exerciseRate = false, finishMarker = finishMarker)
    }

    private fun assertHealthy(activity: CallProbeActivity, owner: CallProbeAudio): CallProbeAudio.Snapshot {
        assertNull("Activity precondition failed", activity.probeFailure)
        return owner.snapshot().also { assertNull("audio/native failure: ${it.failure}", it.failure) }
    }

    private fun runAudio(app: Context, fixture: String?, durationMs: Long, exerciseRate: Boolean, finishMarker: File? = null) {
        assertEquals("operator must externally grant RECORD_AUDIO before instrumentation; denial is not PASS",
            PackageManager.PERMISSION_GRANTED, app.checkSelfPermission(Manifest.permission.RECORD_AUDIO))
        val intent = Intent(app, CallProbeActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            .putExtra(CallProbeActivity.EXTRA_AUTO_START, true)
        if (fixture != null) intent.putExtra(CallProbeActivity.EXTRA_FIXTURE_PATH, fixture)
        var owner: CallProbeAudio? = null
        var activity: CallProbeActivity? = null
        var report: JSONObject? = null
        try {
            ActivityScenario.launch<CallProbeActivity>(intent).use { scenario ->
                await("visible foreground owner did not start", if (fixture == null) 10_000 else 30_000) {
                    scenario.onActivity {
                        activity = it
                        assertNull(it.probeFailure)
                        assertTrue("probe target must remain resumed", it.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))
                        owner = it.probeOwner
                        owner?.let { audio -> assertHealthy(it, audio) }
                    }
                    owner?.snapshot()?.let { state ->
                        val native = JSONObject(state.nativeStats)
                        state.phase == "running" && native.optBoolean("ready") &&
                            native.optBoolean("capture_clock_calibrated") &&
                            native.optLong("encoded_packets") > 0 && native.optLong("decoded_packets") > 0 &&
                            (fixture == null || native.optBoolean("dns_carrier"))
                    } == true
                }
                val audio = owner!!
                val host = activity!!
                val readyNative = JSONObject(audio.snapshot().nativeStats)
                evidence("call_probe_ready", JSONObject().put("native", readyNative))
                val started = SystemClock.elapsedRealtime()
                var initialFrames: Long? = null
                var initialCapture: Long? = null
                var initialPlaybackTimestamp: CallProbeAudio.Clock? = null
                var rateRaised = false
                var rateRestored = false
                var rateEvidence: JSONObject? = null
                var nextProgressMs = 10_000L
                while (SystemClock.elapsedRealtime() - started < durationMs) {
                    val elapsed = SystemClock.elapsedRealtime() - started
                    val state = assertHealthy(host, audio)
                    if (initialPlaybackTimestamp == null) initialPlaybackTimestamp = state.playbackClock.latest
                    assertTrue("foreground microphone must be active", state.microphoneActive)
                    assertTrue("independent capture worker", state.captureAlive)
                    assertTrue("independent render worker", state.renderAlive)
                    assertTrue("bounded actual AudioTrack queue", state.maxQueueSamples <= CallProbeAudio.QUEUE_BOUND_SAMPLES)
                    if (elapsed >= nextProgressMs) {
                        evidence("call_probe_progress", JSONObject().put("elapsed_ms", elapsed).put("running", state.json()))
                        nextProgressMs = elapsed + 10_000 // one current snapshot, never catch-up reports
                    }
                    if (elapsed >= 2_000 && initialFrames == null) {
                        initialFrames = state.hardwareFrames; initialCapture = state.capturedSamples
                    }
                    if (exerciseRate && elapsed >= 2_500 && !rateRaised) {
                        audio.setPlaybackRateOffsetHz(8); rateRaised = true
                    }
                    if (exerciseRate && elapsed >= 5_500 && !rateRestored) {
                        assertEquals("platform actuator readback", 16_008, state.playbackRate)
                        rateEvidence = JSONObject().put("requested_rate", state.requestedRate)
                            .put("readback_rate", state.playbackRate).put("hardware_clock", state.playbackClock.json())
                            .put("head_clock", state.playbackHeadClock.json())
                        audio.setPlaybackRateOffsetHz(0); rateRestored = true
                    }
                    Thread.sleep(50)
                }
                val state = assertHealthy(host, audio)
                // Snapshot and duration are immutable measurement-end evidence. The wait keeps
                // producers alive but contributes no counters/time to the measured interval.
                val interval = JSONObject().put("duration_ms", SystemClock.elapsedRealtime() - started).put("running", state.json())
                // Preserve frozen active evidence on a subsequent assertion failure,
                // rather than exposing only teardown loss in the final snapshot.
                report = JSONObject().put("duration_ms", interval.getLong("duration_ms"))
                    .put("ready_native", readyNative).put("running", interval.getJSONObject("running"))
                    .put("rate_experiment", rateEvidence ?: JSONObject.NULL)
                    .put("playback_timestamp_available", state.playbackClock.latest != null)
                    .put("capture_timestamp_available", state.captureClock.latest != null)
                    .put("evidence_scope", if (fixture == null) "one_phone_local_protected_memory_pair" else "one_phone_explicit_dns_fixture")
                if (finishMarker != null) {
                    assertNull("finish marker must not precede interval_complete", finishMarkerStat(finishMarker))
                    evidence("call_probe_interval_complete", interval)
                    awaitFinishMarker(finishMarker)
                }
                if (exerciseRate) assertEquals("explicit actuator restoration", 16_000, state.playbackRate)
                else assertTrue("automatic platform correction remains within the supported +/-500ppm",
                    state.playbackRate in 15_992..16_008)
                assertTrue("real microphone sample count", state.capturedSamples >= 16_000)
                assertTrue("continued real microphone progress", state.capturedSamples > (initialCapture ?: 0) + 16_000)
                assertTrue("nonzero microphone signal (all-zero/muted input is not evidence)", state.capturePeak > 0)
                val native = JSONObject(state.nativeStats)
                assertTrue("authenticated peer readiness", native.getBoolean("ready"))
                assertTrue("frozen recording-clock observation is required before measurement",
                    native.getBoolean("capture_clock_calibrated"))
                assertEquals("explicit topology must match the native carrier", fixture != null, native.getBoolean("dns_carrier"))
                assertTrue("real live encoder packets", native.getLong("encoded_packets") > 0)
                assertTrue("real protected decoder packets", native.getLong("decoded_packets") > 0)
                assertTrue("protected outbound bytes", native.getLong("tx_bytes") > 0)
                assertTrue("protected inbound bytes", native.getLong("rx_bytes") > 0)
                assertTrue("authenticated terminal feedback", native.getLong("terminal_feedback") > 0)
                assertFalse(native.getBoolean("failed"))
                if (fixture != null && durationMs >= 60_000) {
                    assertTrue("normal long probe must establish authenticated source-clock rate",
                        native.getBoolean("remote_clock_calibrated") && native.getBoolean("remote_clock_valid"))
                    val compensation = JSONObject(state.playbackCompensation)
                    assertTrue("actual AudioTrack clock must support the platform compensator",
                        compensation.getBoolean("hardware_calibrated") && compensation.getBoolean("healthy"))
                    assertTrue("actual sink-rate report stays within its validated range",
                        compensation.getLong("sink_ppb") in -1_000_000L..1_000_000L)
                }
                if (fixture == null) {
                    assertEquals("synthetic capture must not lose source slots merely because its callback was late",
                        0L, native.getLong("fixture_peer_capture_gap_batches") - readyNative.getLong("fixture_peer_capture_gap_batches"))
                    assertEquals("live native PCM must not overwrite a still-waiting render frame",
                        0L, native.getLong("dropped_render") - readyNative.getLong("dropped_render"))
                }
                assertTrue("actual submitted playback PCM", state.submittedSamples > 0)
                assertTrue("actual hardware playback frame-position progress", state.hardwareFrames > (initialFrames ?: 0))
                assertTrue("actual hardware playback frames", state.hardwareFrames > 0)
                if (state.playbackClock.latest != null) {
                    // The current-rate telemetry segment resets on a real
                    // platform rate change. Check physical timestamp progress
                    // across measurement, not whether that latest segment has
                    // happened to accumulate two readbacks at the final instant.
                    val before = initialPlaybackTimestamp
                    assertTrue("available hardware AudioTrack timestamp advances",
                        before != null && state.playbackClock.latest.frame > before.frame &&
                            state.playbackClock.latest.nanoTime > before.nanoTime)
                }
                if (state.captureClock.latest != null) {
                    assertTrue("available monotonic AudioRecord timestamp advances",
                        state.captureClock.latest.frame > state.captureClock.first!!.frame &&
                            state.captureClock.latest.nanoTime > state.captureClock.first.nanoTime)
                }
                assertTrue("nonzero decoded playback PCM", state.renderPeak > 0)
                assertTrue("capture route observed", state.inputRouteId > 0 && state.inputRouteType > 0)
                assertTrue("playback route observed", state.outputRouteId > 0 && state.outputRouteType > 0)
                assertFalse(state.micMuted); assertFalse(state.silenced)
                assertTrue(state.focusGranted)
                listOf(state.aec, state.ns).filter { it.available }.forEach {
                    assertTrue(it.created); assertEquals(0, it.enableResult); assertTrue(it.enabled); assertTrue(it.control)
                }
                // Real lifecycle pause, not only a Stop callback returning/null Activity.
                scenario.moveToState(Lifecycle.State.CREATED)
                await("pause cleanup did not complete in 10s; do not start a second owner") {
                    val stopped = audio.snapshot()
                    stopped.cleanupComplete && !stopped.ownerAlive
                }
                assertStopped(audio.snapshot())
                report!!.put("stopped", audio.snapshot().json())
            }
        } finally {
            // Also runs on assertion/setup failure. No permission revocation or account IO.
            owner?.requestStop("instrumentation_finally")
            owner?.let { audio ->
                try {
                    await("final cleanup incomplete in 10s") { val s = audio.snapshot(); s.cleanupComplete && !s.ownerAlive }
                } finally {
                    evidence("call_probe_audio", (report ?: JSONObject()).put("final", audio.snapshot().json()))
                }
            }
        }
    }

    private fun assertStopped(state: CallProbeAudio.Snapshot) {
        assertNull(state.failure)
        assertEquals("stopped", state.phase)
        assertTrue(state.cleanupComplete)
        assertFalse(state.ownerAlive); assertFalse(state.captureAlive); assertFalse(state.renderAlive)
        assertFalse(state.microphoneActive); assertFalse(state.recordOpen); assertFalse(state.trackOpen)
        assertFalse(state.effectsOpen); assertEquals(0L, state.activeHandle)
        assertTrue("foreground audio focus abandoned", state.focusAbandoned)
        assertEquals("audio mode restored", state.previousMode, state.restoredMode)
        assertNotNull("observable native stop result", state.stopResult)
        assertTrue(JSONObject(state.stopResult!!).getBoolean("stopped"))
        assertFalse(JSONObject(state.stopResult).getBoolean("failed"))
    }
}

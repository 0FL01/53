package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.SystemClock
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
        runAudio(gate(), null, 8_000, exerciseRate = true)
    }

    /** Required instrumentation arguments: probe_fixture_path (private owner0400 file),
     * probe_duration_seconds (6..3600). Missing/invalid inputs fail, never skip.
     * Peer/relay topology and actual DNS service rate must be independently documented.
     */
    @Test(timeout = 3_840_000) fun foregroundMicrophoneProtectedDnsAndCleanupOnlyInGatePackage() {
        val app = gate()
        val path = arguments.getString(CallProbeActivity.EXTRA_FIXTURE_PATH)
        assertFalse("required explicit probe_fixture_path", path.isNullOrBlank())
        val duration = arguments.getString("probe_duration_seconds")?.toLongOrNull()
        assertTrue("required probe_duration_seconds in 6..3600", duration != null && duration in 6..3600)
        runAudio(app, path!!, duration!! * 1_000, exerciseRate = false)
    }

    private fun assertHealthy(activity: CallProbeActivity, owner: CallProbeAudio): CallProbeAudio.Snapshot {
        assertNull("Activity precondition failed", activity.probeFailure)
        return owner.snapshot().also { assertNull("audio/native failure: ${it.failure}", it.failure) }
    }

    private fun runAudio(app: Context, fixture: String?, durationMs: Long, exerciseRate: Boolean) {
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
                var rateRaised = false
                var rateRestored = false
                var rateEvidence: JSONObject? = null
                var nextProgressMs = 10_000L
                while (SystemClock.elapsedRealtime() - started < durationMs) {
                    val elapsed = SystemClock.elapsedRealtime() - started
                    val state = assertHealthy(host, audio)
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
                    assertTrue("available hardware AudioTrack timestamp advances",
                        state.playbackClock.latest.frame > state.playbackClock.first!!.frame &&
                            state.playbackClock.latest.nanoTime > state.playbackClock.first.nanoTime)
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
                report = JSONObject().put("duration_ms", SystemClock.elapsedRealtime() - started)
                    .put("ready_native", readyNative)
                    .put("running", state.json()).put("rate_experiment", rateEvidence ?: JSONObject.NULL)
                    .put("playback_timestamp_available", state.playbackClock.latest != null)
                    .put("capture_timestamp_available", state.captureClock.latest != null)
                    .put("evidence_scope", if (fixture == null) "one_phone_local_protected_memory_pair" else "one_phone_explicit_dns_fixture")
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

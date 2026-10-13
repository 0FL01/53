package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Process
import android.os.SystemClock
import android.system.Os
import android.system.OsConstants
import android.widget.Button
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import uniffi.dmsg_core.CallEndReason
import uniffi.dmsg_core.CallPhase
import java.io.File

/**
 * Operator selects ONE method on .gate with a fresh authenticated core10/server7 account and an
 * accepted real peer on another genuine client. Input owner-only gate-call-client.json: {peerCid}.
 * The saved DNS profile/resolver must already be configured; this suite never resets account data.
 * Grant microphone externally before media methods; never revoke it from the running runner.
 * No fixture echo/JNI startLocal/startDns. Remote peer explicitly dials/answers/speaks/ends as named.
 */
@RunWith(AndroidJUnit4::class)
class CallClientGatesTest {
    @get:Rule val testName = TestName()
    private val app get() = ApplicationProvider.getApplicationContext<Context>()
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private lateinit var peer: String
    private lateinit var facade: DmsgFacade
    private var resolverApplied = false
    private var cloudInput = false
    private var speakerInput = false
    private var testResolver: List<String>? = null
    @Before fun isolatedRealAccount() {
        assertTrue("isolated package required", app.packageName.endsWith(".gate"))
        assertEquals("select exactly one method", "${javaClass.name}#${testName.methodName}",
            InstrumentationRegistry.getArguments().getString("class"))
        assertFalse("connectivity must initially be off", DmsgService.running(app))
        assertFalse("previous call must have fully retired", CallUi.state.value.busy)
        val cloud = when (InstrumentationRegistry.getArguments().getString("call_cloud_resolver")) {
            null, "false" -> false
            "true" -> true
            else -> error("Invalid call_cloud_resolver")
        }
        val explicit = InstrumentationRegistry.getArguments().getString("call_test_resolver")
        require(!cloud || explicit == null) { "Conflicting resolver inputs" }
        if (explicit != null) {
            val parts = explicit.split(':')
            require(parts.size == 2 && parts[1] == "53") { "Invalid test resolver" }
            val octets = parts[0].split('.')
            require(octets.size == 4 && octets.all { value ->
                value.isNotEmpty() && value.all { it in '0'..'9' } && value.toIntOrNull() in 0..255
            }) { "Invalid test resolver" }
        }
        cloudInput = cloud
        speakerInput = when (InstrumentationRegistry.getArguments().getString("call_speaker")) {
            null, "false" -> false
            "true" -> true
            else -> error("Invalid call_speaker")
        }
        testResolver = if (cloud) listOf("1.1.1.1:53") else explicit?.let { listOf(it) }
        val fixture = File(app.filesDir, "gate-call-client.json")
        val stat = Os.lstat(fixture.absolutePath)
        assertTrue("owner-only bounded regular fixture required", OsConstants.S_ISREG(stat.st_mode) &&
            stat.st_uid == Process.myUid() && (stat.st_mode and 511) in listOf(256, 384) && stat.st_size in 1..4096)
        peer = try { JSONObject(fixture.readText()).getString("peerCid") }
            catch (_: Exception) { throw AssertionError("Invalid client-call fixture") }
        assertTrue(peer.length in 1..128)
        assertTrue("existing encrypted account required", Core.dbFile(app).isFile)
        facade = Core.facade(app)
        assertTrue("real facade required", facade is UniFfiFacade)
        assertTrue("authenticate fresh gate account first", facade.account().authenticated)
        assertNotNull(facade.dnsProfile())
        assertTrue("real accepted peer required", callContactAllowed(facade.get(peer)))
        if (testResolver != null) {
            // Warm the platform observer before changing this test's saved resolver input.
            // A later network change can still override it; this is not a persistent mode.
            facade.reconnect()
            resolverApplied = true
            facade.dnsNetworkChanged(requireNotNull(testResolver))
            assertResolverInput()
        }
        reportDns("before_reconnect")
    }
    @After fun retire() {
        try {
            instrumentation.runOnMainSync {
                val state = CallUi.state.value
                state.call?.let { DmsgService.callOwner()?.end(it.callId, state.epoch) }
                DmsgService.stop(app)
            }
            waitFor("call audio/service did not retire") { !CallUi.state.value.busy && !DmsgService.running(app) }
        } finally {
            if (resolverApplied) {
                val platform = DnsNetwork.resolvers(app)
                facade.dnsNetworkChanged(platform)
                assertEquals("platform resolver input must be restored", platform, facade.dnsProfile()?.resolvers)
                resolverApplied = false
                reportDns("platform_restored")
            }
        }
    }
    private fun waitFor(label: String, timeout: Long = 90_000, condition: () -> Boolean) {
        val deadline = SystemClock.uptimeMillis() + timeout
        var nextReport = SystemClock.uptimeMillis() + 10_000
        while (SystemClock.uptimeMillis() < deadline) {
            if (condition()) return
            if (SystemClock.uptimeMillis() >= nextReport) {
                report(label)
                nextReport += 10_000
            }
            SystemClock.sleep(75)
        }
        fail(label)
    }
    private fun report(label: String) {
        reportDns(label)
        val state = CallUi.state.value
        val native = state.audio?.nativeStats?.let { JSONObject(it) }
        val value = JSONObject().put("waiting", label).put("phase", state.call?.phase?.name)
            .put("reason", state.call?.reason?.name).put("preparing", state.preparing)
            .put("tearing_down", state.tearingDown).put("listener", state.incomingAvailable)
            .put("telecom_ready", state.telecomReady).put("telecom_active", state.telecomActive)
            .put("audio_failure", state.audio?.failure).put("error_resource", state.errorRes)
            .put("endpoint_type", state.endpoint?.type).put("output_route_type", state.audio?.outputRouteType)
            .put("playback_rate", state.audio?.playbackRate).put("underruns", state.audio?.underruns)
            .put("capture_clock", state.audio?.captureClock?.json())
            .put("captured_samples", state.audio?.capturedSamples)
            .put("submitted_samples", state.audio?.submittedSamples)
            .put("last_render_loop_trace", state.audio?.lastRenderLoopTrace?.json())
            .put("track_configuration", state.audio?.let { audio -> JSONObject()
                .put("buffer_frames", audio.trackBufferFrames).put("capacity_frames", audio.trackCapacityFrames)
                .put("start_threshold_frames", audio.startThresholdFrames).put("performance_mode", audio.trackPerformanceMode) })
            .put("hardware_clock", state.audio?.let { audio ->
                val clock = JSONObject(audio.playbackCompensation)
                JSONObject().put("calibrated", clock.optBoolean("hardware_calibrated"))
                    .put("healthy", clock.optBoolean("healthy"))
            })
            .put("hardware_clock_failure", state.audio?.let {
                JSONObject(it.playbackCompensation).optJSONObject("last_backward_hardware_sample")
            })
            .put("dns_status", runCatching { facade.dnsStatus() }.getOrNull())
            .put("native_failed", native?.optBoolean("failed"))
            .put("last_progress_failure", native?.opt("last_progress_failure"))
        if (native != null) {
            val progress = JSONObject()
            for (key in listOf("encoded_packets", "decoded_packets", "received_rtp_packets",
                "last_validated_peer_media_index", "last_prepared_feedback_terminal",
                "last_committed_feedback_terminal", "terminal_feedback", "tx_bytes", "rx_bytes",
                "last_prepared_feedback_srtcp_index", "last_committed_feedback_srtcp_index",
                "last_authenticated_feedback_srtcp_index",
                "dropped_capture", "source_route_gap_samples", "dropped_render", "expired_render_samples", "late_packets", "plc_slots",
                "future_rejected_packets", "skipped_playout_slots", "max_future_lead_ms",
                "max_receiver_phase_advance_us", "max_sender_phase_advance_us",
                "max_capture_age_us", "max_additional_capture_age_us", "max_source_ready_to_commit_us",
                "arrival_after_nominal_due_packets", "late_before_nominal_due_packets",
                "max_playout_tick_lateness_ms", "max_decode_us", "encode_duration_bins", "last_render_expiry",
                "capture_age_unavailable_batches", "max_feedback_cycle_ms", "error")) {
                progress.put(key, native.opt(key))
            }
            value.put("media_progress", progress)
        }
        instrumentation.sendStatus(0, android.os.Bundle().apply { putString("client_call_state", value.toString()) })
    }
    private fun reportDns(label: String) {
        val platform = DnsNetwork.resolvers(app)
        val configured = requireNotNull(facade.dnsProfile()).resolvers
        val keyguard = app.getSystemService(android.app.KeyguardManager::class.java)
        val value = JSONObject().put("stage", label)
            .put("profile_matches_platform", configured.toSet() == platform.toSet())
            .put("platform_count", platform.size).put("configured_count", configured.size)
            .put("explicit_cloud_input", resolverApplied && cloudInput)
            .put("explicit_test_input", resolverApplied)
            .put("explicit_input_matches", !resolverApplied || configured == testResolver)
            .put("device_secure", keyguard.isDeviceSecure).put("device_locked", keyguard.isDeviceLocked)
        val connectivity = app.getSystemService(android.net.ConnectivityManager::class.java)
        value.put("platform_wifi", connectivity.getNetworkCapabilities(connectivity.activeNetwork)
            ?.hasTransport(android.net.NetworkCapabilities.TRANSPORT_WIFI) == true)
        instrumentation.sendStatus(0, android.os.Bundle().apply { putString("client_call_dns", value.toString()) })
    }
    private fun microphonePermission() = assertEquals("operator grants microphone before this method",
        PackageManager.PERMISSION_GRANTED, app.checkSelfPermission(Manifest.permission.RECORD_AUDIO))
    private fun screenKey(code: Int) {
        require(code == 223 || code == 224) // KEYCODE_SLEEP / KEYCODE_WAKEUP only.
        android.os.ParcelFileDescriptor.AutoCloseInputStream(
            instrumentation.uiAutomation.executeShellCommand("input keyevent $code")
        ).use { it.readBytes().fill(0) }
    }
    private fun assertResolverInput() {
        if (resolverApplied) assertEquals("explicit resolver input changed", testResolver, facade.dnsProfile()?.resolvers)
    }
    private fun activeSeconds(): Long {
        val seconds = InstrumentationRegistry.getArguments().getString("call_active_seconds")?.let {
            requireNotNull(it.toLongOrNull())
        } ?: 3L
        require(seconds in 3L..300L)
        return seconds
    }
    private fun listen(): ActivityScenario<DiagnosticsActivity> {
        val scenario = ActivityScenario.launch<DiagnosticsActivity>(Intent(app, DiagnosticsActivity::class.java))
        scenario.onActivity { Prefs.setEconomy(it, false); DmsgService.start(it) }
        waitFor("real call listener unavailable") { CallUi.state.value.incomingAvailable }
        assertResolverInput()
        reportDns("listener_ready")
        instrumentation.sendStatus(0, android.os.Bundle().apply { putString("client_call_listener_ready", "true") })
        return scenario
    }
    private fun incoming() {
        waitFor("real remote client did not ring") { CallUi.state.value.call?.phase == CallPhase.RINGING }
        assertEquals(peer, CallUi.state.value.call?.contactId)
        assertNull("ringing must not create microphone/native audio workers", CallUi.state.value.audio)
        assertFalse(CallUi.state.value.telecomActive)
        assertTrue("ringing must not activate an app microphone", app.getSystemService(android.media.AudioManager::class.java)
            .activeRecordingConfigurations.isEmpty())
    }
    private fun activity() = CallUi.state.value.let { state -> ActivityScenario.launch<CallActivity>(
        Intent(app, CallActivity::class.java).putExtra("callId", state.call!!.callId).putExtra("epoch", state.epoch)) }
    private fun healthy() {
        assertResolverInput()
        waitFor("real media engine not ready") {
            if (CallUi.state.value.call?.phase == CallPhase.ENDED) {
                report("media_ended"); fail("real media ended before the required interval")
            }
            CallUi.state.value.call?.phase == CallPhase.ACTIVE &&
                CallUi.state.value.audio?.let { audio -> audio.microphoneActive && audio.capturedSamples > 0 &&
                    runCatching { JSONObject(audio.nativeStats).getLong("decoded_packets") > 0 }.getOrDefault(false) } == true
        }
        val audio = requireNotNull(CallUi.state.value.audio)
        assertNull(audio.failure)
        assertTrue(audio.activeHandle != 0L)
        assertEquals(16_000, CallAudio.SAMPLE_RATE)
        assertTrue(audio.maxQueueSamples <= 640)
        val stats = JSONObject(requireNotNull(audio.nativeStats))
        assertTrue(stats.getBoolean("ready")); assertFalse(stats.getBoolean("failed"))
        assertTrue("real authenticated incoming RTP required", stats.getLong("received_rtp_packets") > 0)
        assertEquals("no standalone fixture sender in genuine client", 0L, stats.getLong("fixture_peer_encoded_packets"))
    }
    private fun selectSpeaker(ui: ActivityScenario<CallActivity>) {
        if (!speakerInput) return
        report("before_speaker")
        waitFor("Telecom speaker endpoint unavailable") {
            CallUi.state.value.endpoints.any { it.type == androidx.core.telecom.CallEndpointCompat.TYPE_SPEAKER }
        }
        if (CallUi.state.value.endpoint?.type != androidx.core.telecom.CallEndpointCompat.TYPE_SPEAKER) {
            ui.onActivity { activity ->
                val button = activity.findViewById<Button>(R.id.call_speaker)
                assertTrue("production speaker action must be enabled", button.isEnabled)
                button.performClick()
            }
        }
        waitFor("Telecom speaker request did not select actual track route", 5_000) {
            if (CallUi.state.value.call?.phase == CallPhase.ENDED) {
                report("speaker_ended"); fail("call ended during speaker selection")
            }
            CallUi.state.value.endpoint?.type == androidx.core.telecom.CallEndpointCompat.TYPE_SPEAKER &&
                CallUi.state.value.audio?.outputRouteType == android.media.AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
        }
        report("speaker_selected")
        healthy()
    }
    private fun retired(reason: CallEndReason, timeout: Long = 90_000) {
        waitFor("call teardown incomplete", timeout) { CallUi.state.value.call?.phase == CallPhase.ENDED && !CallUi.state.value.tearingDown }
        report("retired")
        assertEquals(reason, CallUi.state.value.call?.reason)
        CallUi.state.value.audio?.let { audio ->
            assertTrue(audio.cleanupComplete); assertFalse(audio.microphoneActive); assertFalse(audio.recordOpen)
            assertFalse(audio.trackOpen); assertEquals(0L, audio.activeHandle)
            assertFalse(audio.effectsOpen); assertFalse(audio.captureAlive); assertFalse(audio.renderAlive)
        }
        assertTrue("notes can claim only after joins", VoiceNoteAudio.callClaim.allowNote())
    }
    @Test fun client01IncomingRingRejectNoMicrophone() {
        listen().use {
            incoming()
            activity().use { ui -> ui.onActivity { it.findViewById<Button>(R.id.call_decline).performClick() } }
            retired(CallEndReason.REJECTED)
            assertNull(CallUi.state.value.audio)
        }
    }
    @Test fun client02OutgoingRealAnswerMuteRecreateHangup() {
        val seconds = activeSeconds()
        microphonePermission()
        ActivityScenario.launch<CallActivity>(Intent(app, CallActivity::class.java).putExtra("peer", peer)).use { ui ->
            healthy() // Operator answers and speaks on the other genuine client.
            report("caller_active")
            val before = requireNotNull(CallUi.state.value.audio).capturedSamples
            ui.onActivity { it.findViewById<Button>(R.id.call_mute).performClick() }
            waitFor("local mute stopped the capture timeline") {
                if (CallUi.state.value.call?.phase == CallPhase.ENDED) {
                    report("mute_ended"); fail("call ended during mute timeline check")
                }
                CallUi.state.value.muted && (CallUi.state.value.audio?.capturedSamples ?: 0) > before
            }
            ui.onActivity { it.findViewById<Button>(R.id.call_mute).performClick() }
            waitFor("outgoing PCM must unmute") {
                if (CallUi.state.value.call?.phase == CallPhase.ENDED) {
                    report("unmute_ended"); fail("call ended during unmute check")
                }
                !CallUi.state.value.muted
            }
            report("caller_unmuted")
            val id = CallUi.state.value.call!!.callId
            ui.recreate()
            report("caller_recreated")
            assertEquals(id, CallUi.state.value.call!!.callId)
            assertEquals(CallPhase.ACTIVE, CallUi.state.value.call!!.phase)
            val deadline = SystemClock.elapsedRealtime() + seconds * 1_000
            var lastCapture = requireNotNull(CallUi.state.value.audio).capturedSamples
            var lastDecoded = JSONObject(requireNotNull(CallUi.state.value.audio).nativeStats).getLong("decoded_packets")
            var nextReport = SystemClock.elapsedRealtime() + 10_000
            while (SystemClock.elapsedRealtime() < deadline) {
                SystemClock.sleep(1_000)
                healthy()
                assertEquals(id, CallUi.state.value.call!!.callId)
                val audio = requireNotNull(CallUi.state.value.audio)
                val decoded = JSONObject(audio.nativeStats).getLong("decoded_packets")
                if (audio.capturedSamples <= lastCapture || decoded <= lastDecoded) {
                    report("media_progress_stalled")
                    fail("active media must progress, not retain an earlier nonzero counter")
                }
                lastCapture = audio.capturedSamples; lastDecoded = decoded
                if (SystemClock.elapsedRealtime() >= nextReport) {
                    report("active_interval")
                    nextReport += 10_000
                }
            }
            if (seconds >= 60) {
                report("hardware_interval_complete")
                val clock = JSONObject(requireNotNull(CallUi.state.value.audio).playbackCompensation)
                assertTrue("sustained interval requires qualified hardware clock", clock.getBoolean("hardware_calibrated"))
                assertTrue("sustained interval requires healthy hardware clock", clock.getBoolean("healthy"))
            }
            ui.onActivity { it.findViewById<Button>(R.id.call_hangup).performClick() }
            report("caller_hangup")
            retired(CallEndReason.LOCAL_ENDED)
        }
    }
    @Test fun client03IncomingVisibleAnswerBackgroundRemoteEnd() {
        val seconds = activeSeconds()
        microphonePermission()
        listen().use {
            incoming()
            activity().use { ui ->
                ui.onActivity { it.findViewById<Button>(R.id.call_accept).performClick() }
                healthy()
                selectSpeaker(ui)
                ui.moveToState(androidx.lifecycle.Lifecycle.State.CREATED)
                SystemClock.sleep(3_000)
                report("background_observed")
                assertEquals("UI pause must not end a service-owned call", CallPhase.ACTIVE, CallUi.state.value.call?.phase)
                assertTrue(CallUi.state.value.audio?.microphoneActive == true)
                retired(CallEndReason.REMOTE_ENDED, (seconds + 90) * 1_000) // Actual remote client hangs up.
            }
        }
    }
    @Test fun client04OutgoingRealRejectionNoMicrophone() {
        ActivityScenario.launch<CallActivity>(Intent(app, CallActivity::class.java).putExtra("peer", peer)).use {
            retired(CallEndReason.REJECTED) // The genuine peer selects Decline.
            assertNull("unanswered outgoing call must not capture", CallUi.state.value.audio)
        }
    }
    @Test fun client07IncomingNotificationAnswerWhileLockedRemoteEnd() {
        val seconds = activeSeconds()
        microphonePermission()
        val keyguard = app.getSystemService(android.app.KeyguardManager::class.java)
        assertTrue("this gate requires an existing secure keyguard", keyguard.isDeviceSecure)
        Prefs.setEconomy(app, false)
        DmsgService.start(app)
        waitFor("real call listener unavailable") { CallUi.state.value.incomingAvailable }
        screenKey(223)
        waitFor("secure device did not lock", 15_000) { keyguard.isDeviceLocked }
        assertResolverInput()
        reportDns("locked_listener_ready")
        instrumentation.sendStatus(0, android.os.Bundle().apply { putString("client_call_listener_ready", "true") })
        incoming()
        assertTrue("ringing must not unlock the device", keyguard.isDeviceLocked)
        waitFor("incoming Telecom registration unavailable") { CallUi.state.value.telecomReady }
        val manager = app.getSystemService(android.app.NotificationManager::class.java)
        val notification = manager.activeNotifications.single { it.id == DmsgService.ID }.notification
        assertEquals(android.app.Notification.CATEGORY_CALL, notification.category)
        val answerTitle = app.getString(androidx.core.R.string.call_notification_answer_action)
        val answer = requireNotNull(notification.actions).single { it.title.toString() == answerTitle }
        // Exercise the production notification PendingIntent; never dismiss keyguard or enter a PIN.
        answer.actionIntent.send()
        healthy()
        assertTrue("answer must not remove the secure keyguard", keyguard.isDeviceSecure && keyguard.isDeviceLocked)
        report("notification_answer_while_locked")
        screenKey(223)
        SystemClock.sleep(3_000)
        assertEquals("screen-off must preserve the service-owned call", CallPhase.ACTIVE, CallUi.state.value.call?.phase)
        assertTrue(CallUi.state.value.audio?.microphoneActive == true)
        assertTrue(keyguard.isDeviceLocked)
        report("locked_screen_off_observed")
        screenKey(224)
        assertTrue("waking must not bypass keyguard", keyguard.isDeviceLocked)
        retired(CallEndReason.REMOTE_ENDED, (seconds + 90) * 1_000)
    }
    @Test fun client05SendTextAfterCall() {
        facade.reconnect()
        val mid = facade.send(peer, "53 client post-call text smoke")
        assertTrue(facade.messageStatus(mid) in listOf(
            uniffi.dmsg_core.DeliveryState.ACCEPTED, uniffi.dmsg_core.DeliveryState.DELIVERED))
        waitFor("post-call text was not delivered") {
            facade.retry()
            facade.messageStatus(mid) == uniffi.dmsg_core.DeliveryState.DELIVERED
        }
    }
    @Test fun client06ReceiveTextAfterCall() {
        facade.reconnect()
        waitFor("post-call text was not received") {
            facade.fetch().received.any { it.contactId == peer && it.text == "53 client post-call text smoke" }
        }
    }
}

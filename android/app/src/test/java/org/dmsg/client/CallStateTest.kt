package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.CallDirection
import uniffi.dmsg_core.CallEndReason
import uniffi.dmsg_core.CallPhase
import uniffi.dmsg_core.CallSnapshot

class CallStateTest {
    private fun snapshot(id: String = "first", phase: CallPhase = CallPhase.RINGING, handle: Long = 0) =
        CallSnapshot(id, "peer", phase, CallDirection.INCOMING, CallEndReason.NONE, handle, true)

    private fun completedAudio(failure: String?, finalNative: String = "{\"failed\":true,\"stopped\":true}") = CallAudio.Snapshot(
        phase = "failed", failure = failure, stopReason = failure, dns = true, cleanupComplete = true,
        ownerAlive = false, captureAlive = false, renderAlive = false, microphoneActive = false,
        recordOpen = false, trackOpen = false, effectsOpen = false, activeHandle = 0,
        capturedSamples = 0, submittedSamples = 0, hardwareFrames = 0, queueSamples = 0, maxQueueSamples = 0,
        rejectedCaptureBatches = 0, capturePeak = 0, captureClipped = 0, captureZeroSamples = 0,
        renderPeak = 0, renderClipped = 0, recordMinBytes = 0, trackMinBytes = 0, recordBufferFrames = 0,
        trackBufferFrames = 0, trackCapacityFrames = 0, startThresholdFrames = -1,
        inputRouteId = 0, inputRouteType = 0, outputRouteId = 0, outputRouteType = 0,
        micMuted = false, silenced = false, silencingObservable = false, focusGranted = false, focusAbandoned = true,
        previousMode = 0, communicationMode = 0, restoredMode = 0,
        aec = CallAudio.Effect(), ns = CallAudio.Effect(), playbackRate = 16_000,
        requestedRate = 16_000, underruns = 2, captureClock = CallAudio.ClockPair(),
        playbackClock = CallAudio.ClockPair(), playbackHeadClock = CallAudio.ClockPair(),
        nativeStats = finalNative, stopResult = finalNative, playbackCompensation = "{}")

    @Test fun nativeAudioCompletionBeforeEndedWaitsForAuthoritativeRemoteReason() {
        for (failure in listOf("native_probe_failed", "native_stopped_early")) {
            val guard = CallStateGuard()
            val claim = CallAudioClaim()
            guard.receive(snapshot(phase = CallPhase.ACTIVE, handle = 42))
            claim.reserveCall()
            val token = guard.epoch
            // All PCM/native retirement finished while the UI still has ACTIVE;
            // the actor's already joined terminal callback is consumed afterwards.
            val reason = guard.audioCompletionReason(token, completedAudio(failure))
            assertNull("completion must not synthesize AUDIO_UNAVAILABLE", reason)
            assertEquals(CallPhase.ACTIVE, guard.current!!.phase)
            assertFalse(guard.tearingDown)
            assertFalse(claim.allowNote())
            assertTrue(guard.receive(snapshot(phase = CallPhase.ENDED).copy(reason = CallEndReason.REMOTE_ENDED)))
            assertEquals(CallEndReason.REMOTE_ENDED, guard.current!!.reason)
            assertNull(guard.audioCompletionReason(token, completedAudio(failure)))
        }
    }

    @Test fun realPlaybackReversalStillEndsAudioDespiteFinalNativeFailure() {
        val clock = CallPlaybackClock()
        val before = 10_000_000_000L
        val after = before + 300_024_218L
        clock.observe(81_920, before, before, 16_000, 2, 182_880)
        clock.observe(1_121, after, after, 16_000, 2, 187_520)
        val command = clock.choose(after, 0, 0, false)
        assertTrue(clock.backwards)
        assertEquals(81_920L, command.backwardSample!!.beforeFrames)
        assertEquals(1_121L, command.backwardSample!!.frames)
        val guard = CallStateGuard()
        guard.receive(snapshot(phase = CallPhase.ACTIVE, handle = 42))
        val result = completedAudio(if (clock.backwards) "playback_clock_backwards" else null)
        val reason = guard.audioCompletionReason(guard.epoch, result)
        assertEquals(CallEndReason.AUDIO_UNAVAILABLE, reason)
        guard.beginTeardown()
        guard.receive(guard.current!!.copy(phase = CallPhase.ENDED, reason = reason!!))
        assertEquals(CallEndReason.AUDIO_UNAVAILABLE, guard.current!!.reason)
    }

    @Test fun physicalAndUnknownCompletionsAreNeverMaskedByNativeStopStats() {
        val guard = CallStateGuard()
        guard.receive(snapshot(phase = CallPhase.ACTIVE, handle = 42))
        for (failure in listOf("capture_clock_backwards", "input_route_lost", "output_route_lost",
            "communication_mode_changed", "sink_clock_rate_rejected", "native_stop_failed", null)) {
            for (stats in listOf("{\"failed\":true,\"stopped\":true}", "{\"failed\":false,\"stopped\":false}")) {
                assertEquals("failure=$failure; stats=$stats", CallEndReason.AUDIO_UNAVAILABLE,
                    guard.audioCompletionReason(guard.epoch, completedAudio(failure, stats)))
            }
        }
        assertNull(guard.audioCompletionReason(guard.epoch - 1, completedAudio("capture_clock_backwards")))
        guard.beginTeardown()
        assertNull(guard.audioCompletionReason(guard.epoch, completedAudio("capture_clock_backwards")))
    }

    @Test fun ringingAndMissingTelecomNeverPermitCapture() {
        val guard = CallStateGuard()
        guard.receive(snapshot(handle = 42))
        assertFalse(guard.mayCapture(telecomActive = true, permission = true))
        guard.receive(snapshot(phase = CallPhase.CONNECTING, handle = 42))
        assertFalse(guard.mayCapture(telecomActive = false, permission = true))
        assertFalse(guard.mayCapture(telecomActive = true, permission = false))
        assertTrue(guard.mayCapture(telecomActive = true, permission = true))
        guard.beginTeardown()
        assertFalse(guard.mayCapture(telecomActive = true, permission = true))
    }
    @Test fun oldNotificationAndLateReadyCannotResurrectRetiredCall() {
        val guard = CallStateGuard()
        guard.receive(snapshot())
        val old = guard.epoch
        assertFalse(guard.matches("wrong", old))
        guard.receive(snapshot(phase = CallPhase.ENDED))
        assertFalse(guard.receive(snapshot(phase = CallPhase.CONNECTING, handle = 42)))
        guard.beginTeardown()
        assertFalse(guard.receive(snapshot(id = "second")))
        guard.finishTeardown()
        assertTrue(guard.receive(snapshot(id = "second")))
        assertFalse(guard.matches("second", old))
        assertTrue(guard.matches("second", guard.epoch))
    }
    @Test fun noteClaimBlockedThroughTeardownReservation() {
        val claim = CallAudioClaim()
        assertTrue(claim.allowNote())
        assertTrue(claim.reserveCall())
        assertFalse(claim.reserveCall())
        assertFalse(claim.allowNote())
        claim.releaseCall()
        assertTrue(claim.allowNote())
    }
    @Test fun onlyAcceptedPinnedContactsHaveCallAction() {
        assertTrue(callContactAllowed(Dialog("peer", "accepted", hasKeys = true)))
        assertTrue(callContactAllowed(Dialog("peer", "accepted_server", hasKeys = true)))
        for (state in listOf("inviting", "incoming", "requested", "blocked"))
            assertFalse(callContactAllowed(Dialog("peer", state, hasKeys = true)))
        assertFalse(callContactAllowed(Dialog("peer", "accepted", identityMismatch = true, hasKeys = true)))
        assertFalse(callContactAllowed(Dialog("peer", "accepted", hasKeys = false)))
    }
}

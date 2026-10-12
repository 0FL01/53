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

package org.dmsg.client

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.QRCodeWriter
import org.junit.Assert.*
import org.junit.Test

class InvitationFlowTest {
    private val id = ByteArray(16) { it.toByte() }
    private fun complete(flow: InvitationFlow, job: InvitationFlow.Job, f: DmsgFacade) =
        flow.complete(job, runCatching { job.run(f) })

    @Test fun openingListsWithoutIssuingAndCreateIsSingleFlight() {
        val f = FakeFacade()
        var issued = 0
        f.issueProbe = { issued++; InvitationGrant(100, InvitationInfo(it, 100, 86500), InvitationState.ACTIVE, "six test words only for unit".toCharArray()) }
        val flow = InvitationFlow { id }
        complete(flow, flow.refresh()!!, f)
        assertEquals(0, issued)
        val job = flow.create()!!
        assertNull(flow.create()); assertNull(flow.refresh())
        complete(flow, job, f)
        assertEquals(1, issued); assertTrue(flow.mayShare)
        assertNull(flow.pending); assertArrayEquals(id, flow.selected)
        assertEquals(0, f.stopCalls)
    }
    @Test fun unknownCreateAndRecreationRecoverSameIdNeverRenew() {
        val f = FakeFacade(); val observed = mutableListOf<ByteArray>()
        f.issueProbe = { observed.add(it.copyOf()); throw DmsgError(R.string.error_transport, ErrorKind.Transport) }
        var randomCalls = 0
        val flow = InvitationFlow { randomCalls++; id }
        complete(flow, flow.create()!!, f)
        complete(flow, flow.create()!!, f)
        val recreated = InvitationFlow { fail("must not generate another id"); id }
        recreated.restore(flow.pending, flow.selected, flow.pendingRevoke)
        f.issueProbe = { observed.add(it.copyOf()); InvitationGrant(200, InvitationInfo(it, 100, 86500), InvitationState.ACTIVE, "original phrase".toCharArray()) }
        complete(recreated, recreated.refresh()!!, f)
        assertEquals(1, randomCalls); assertEquals(3, observed.size)
        assertTrue(observed.all { it.contentEquals(id) }); assertTrue(recreated.mayShare)
        assertEquals(100, recreated.grant!!.invitation.createdAt)
    }
    @Test fun staleCreateResultIsWipedAndPendingIdSurvivesPause() {
        val f = FakeFacade(); val secret = "one two three four five six".toCharArray()
        f.issueProbe = { InvitationGrant(100, InvitationInfo(it, 100, 86500), InvitationState.ACTIVE, secret) }
        val flow = InvitationFlow { id }; val job = flow.create()!!
        val result = job.run(f)
        flow.pause()
        assertFalse(flow.complete(job, kotlin.Result.success(result)))
        assertTrue(secret.all { it == '\u0000' }); assertFalse(flow.mayShare)
        assertArrayEquals(id, flow.pending); assertNull(flow.grant)
    }
    @Test fun unknownRevokeDisablesExportUntilAcknowledgedReconciliation() {
        val f = FakeFacade(); val flow = InvitationFlow { id }
        complete(flow, flow.create()!!, f)
        f.revokeProbe = { throw DmsgError(R.string.error_transport, ErrorKind.Transport) }
        complete(flow, flow.revoke()!!, f)
        assertFalse(flow.mayShare); assertNotNull(flow.pendingRevoke); assertNull(flow.grant)
        assertNull(flow.create()); assertNull(flow.select(id))
        val recreated = InvitationFlow()
        recreated.restore(flow.pending, flow.selected, flow.pendingRevoke)
        var revokes = 0
        f.revokeProbe = { assertArrayEquals(id, it); revokes++ }
        f.issueProbe = { InvitationGrant(100, InvitationInfo(it, 100, 86500), InvitationState.REVOKED, null) }
        complete(recreated, recreated.refresh()!!, f)
        assertEquals(1, revokes); assertNull(recreated.pendingRevoke)
        assertEquals(InvitationState.REVOKED, recreated.grant!!.state); assertFalse(recreated.mayShare)
    }
    @Test fun terminalRecoveryAndPauseWipeMutablePhrase() {
        for (state in InvitationState.entries) {
            val f = FakeFacade(); val secret = "synthetic foreground grant".toCharArray()
            f.issueProbe = { InvitationGrant(100, InvitationInfo(it, 100, 86500), state, secret) }
            val flow = InvitationFlow { id }
            complete(flow, flow.create()!!, f)
            assertEquals(state == InvitationState.ACTIVE, flow.mayShare)
            if (state != InvitationState.ACTIVE) assertTrue(secret.all { it == '\u0000' })
            flow.pause(); assertTrue(secret.all { it == '\u0000' }); assertFalse(flow.mayShare)
        }
    }
    @Test fun activeLimitAndMetadataOnlyList() {
        val f = FakeFacade(); val flow = InvitationFlow { id }
        f.listProbe = { InvitationList(100, (0..7).map { InvitationInfo(ByteArray(16) { _ -> it.toByte() }, 100, 86500) }) }
        complete(flow, flow.refresh()!!, f)
        val error = runCatching { flow.create() }.exceptionOrNull() as DmsgError
        assertEquals(ErrorKind.InviteLimit, error.kind); assertNull(flow.pending)
        assertFalse(flow.mayShare)
    }
    @Test fun knownLimitReleasesAttemptSoExistingInvitationCanBeRevoked() {
        val f = FakeFacade(); val flow = InvitationFlow { id }
        f.listProbe = { InvitationList(100, listOf(InvitationInfo(ByteArray(16) { 42 }, 100, 86500))) }
        complete(flow, flow.refresh()!!, f)
        f.issueProbe = { throw DmsgError(R.string.error_invite_limit, ErrorKind.InviteLimit) }
        complete(flow, flow.create()!!, f)
        assertNull(flow.pending); assertNull(flow.selected)
        assertNotNull(flow.select(ByteArray(16) { 42 }))
    }
    @Test fun listRejectionCannotResolveUnknownEarlierIssue() {
        val f = FakeFacade(); val flow = InvitationFlow { id }
        f.issueProbe = { throw DmsgError(R.string.error_transport, ErrorKind.Transport) }
        complete(flow, flow.create()!!, f)
        f.listProbe = { throw DmsgError(R.string.error_auth_rate, ErrorKind.AuthRateLimited) }
        complete(flow, flow.refresh()!!, f)
        assertArrayEquals(id, flow.pending)
        assertArrayEquals(id, flow.selected)
    }
    @Test fun qrRasterRoundTripPreservesStrictRawTokenNotContactRoute() {
        val token = "A".repeat(43)
        val matrix = QRCodeWriter().encode(token, BarcodeFormat.QR_CODE, 512, 512)
        val pixels = IntArray(512 * 512) { if (matrix[it % 512, it / 512]) -0x1000000 else -1 }
        val decoded = QRCodeReader().decode(BinaryBitmap(HybridBinarizer(RGBLuminanceSource(512, 512, pixels)))).text
        assertEquals(token, decoded); assertTrue(InvitationInput.isCanonical(decoded)); assertTrue(QrGate.route(decoded).isFailure)
        pixels.fill(0)
    }
}

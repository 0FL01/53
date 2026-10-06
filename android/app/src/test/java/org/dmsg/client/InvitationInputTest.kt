package org.dmsg.client

import java.io.ByteArrayInputStream
import java.io.InputStream
import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.RegistrationPolicy

class InvitationInputTest {
    private val token = "A".repeat(42) + "8"

    @Test fun qrAndPrivateFileUseCanonicalParser() {
        for (ending in listOf("", "\n", "\r\n")) {
            assertArrayEquals(InvitationInput.parse(token), InvitationInput.read(ByteArrayInputStream((token + ending).toByteArray())))
        }
        for (last in "AEIMQUYcgkosw048") assertTrue(InvitationInput.isCanonical("_".repeat(42) + last))
    }

    @Test fun rejectsPaddingNoncanonicalBitsWhitespaceUrisAndOversize() {
        for (value in listOf("", "A".repeat(42), token + "=", "A".repeat(42) + "B", " " + token,
            token + " ", "dmsg://invite/$token", "dmsg://server/AAAA", "é".repeat(43), token.take(20) + "\n" + token.drop(20))) {
            assertTrue("raw QR rejection", runCatching { InvitationInput.parse(value) }.isFailure)
            assertTrue("private file rejection", runCatching { InvitationInput.read(ByteArrayInputStream(value.toByteArray())) }.isFailure)
        }
        for (value in listOf(token + "\n\n", token + "\r", "\uFEFF" + token, token + "\u0000", token + "x".repeat(100)))
            assertTrue(runCatching { InvitationInput.read(ByteArrayInputStream(value.toByteArray())) }.isFailure)
        assertTrue(runCatching { AuthForm.validate(AuthAction.Signup, RegistrationPolicy.INVITE_ONLY,
            "user", "password", "A".repeat(42) + "B") }.isFailure)
    }

    @Test fun fileReadStopsAtBoundAndDoesNotEchoInputErrors() {
        var reads = 0
        val infinite = object : InputStream() { override fun read(): Int { reads++; return 'A'.code } }
        val error = runCatching { InvitationInput.read(infinite) }.exceptionOrNull()!!
        assertEquals(46, reads)
        assertFalse(error.message.orEmpty().contains("A".repeat(43)))
    }

    @Test fun takingForExplicitSignupTransfersOwnershipAndAuthClearsBuffer() {
        val memory = InvitationMemory()
        val first = InvitationInput.parse(token)
        memory.replace(first)
        memory.replace(InvitationInput.parse(token))
        assertTrue(first.all { it == '\u0000' })
        val value = memory.take()
        assertFalse(memory.hasInvitation)
        val f = FakeFacade()
        f.signupProbe = { _, _, invite -> assertTrue(invite == token) }
        AuthFlow(f).submit(AuthAction.Signup, RegistrationPolicy.INVITE_ONLY, "user", AuthSecrets("password".toCharArray(), value))
        assertEquals(1, f.signupCalls)
        assertTrue(value.all { it == '\u0000' })
    }

    @Test fun cancelActionChangeAndScannerTicketLossWipeMemory() {
        val memory = InvitationMemory()
        val value = InvitationInput.parse(token)
        memory.replace(value); memory.clear()
        assertFalse(memory.hasInvitation)
        assertTrue(value.all { it == '\u0000' })
        val first = InvitationScanTransfer.begin()
        val scanned = InvitationInput.parse(token)
        assertTrue(InvitationScanTransfer.publish(first, scanned))
        val next = InvitationScanTransfer.begin()
        assertTrue(scanned.all { it == '\u0000' })
        assertNull(InvitationScanTransfer.take(first))
        val rejected = InvitationInput.parse(token)
        assertFalse(InvitationScanTransfer.publish(first, rejected))
        assertTrue(rejected.all { it == '\u0000' })
        InvitationScanTransfer.cancel(next)
        assertNull(InvitationScanTransfer.take(next))
        val success = InvitationScanTransfer.begin()
        assertTrue(InvitationScanTransfer.publish(success, InvitationInput.parse(token)))
        val taken = InvitationScanTransfer.take(success)!!
        assertArrayEquals(token.toCharArray(), taken)
        assertNull(InvitationScanTransfer.take(success))
        taken.fill('\u0000')
    }
}

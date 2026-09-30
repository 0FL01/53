package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.QrKind

class QrGateTest {
    @Test fun routesPublicServerAndContact() {
        assertEquals(QrKind.SERVER, QrGate.route("dmsg://server/AAAA").getOrThrow())
        assertEquals(QrKind.CONTACT, QrGate.route("dmsg://contact/AAAA").getOrThrow())
        assertTrue(QrGate.route("dmsg://join/AAAA").isFailure)
        assertTrue(QrGate.route("A".repeat(43)).isFailure) // invitation is never a profile
    }
    @Test fun oversizedAndBadPrefixesFailBeforeCorePreview() {
        val f = FakeFacade()
        for (input in listOf("https://x/", "dmsg://server/" + "A".repeat(QrGate.SERVER_MAX),
                "dmsg://contact/" + "A".repeat(QrGate.URI_MAX))) {
            assertTrue(runCatching { QrGate.serverPreview(f, input) }.isFailure)
        }
        assertEquals(0, f.previewCalls)
    }
    @Test fun pasteAndScannerUseSameOfflineParserAndContactIsRejectedForConnection() {
        val f = FakeFacade()
        assertEquals(QrGate.serverPreview(f, "dmsg://server/AAAA"),
            QrGate.serverPreview(f, " \ndmsg://server/AA\nAA\r\n"))
        assertTrue(runCatching { QrGate.serverPreview(f, "dmsg://contact/AAAA") }.isFailure)
        assertEquals(2, f.previewCalls)
        assertFalse(f.authenticated)
        assertNull(f.profile)
        assertEquals(0, f.signupCalls)
    }
}

package org.dmsg.client

import java.io.ByteArrayInputStream
import org.junit.Assert.*
import org.junit.Test

class TrustedServerProfileTest {
    private fun stream(value: String) = ByteArrayInputStream(value.toByteArray())

    @Test fun freshPackagedProfileUsesExistingPreviewConfigureWithoutSignup() {
        val f = FakeFacade()
        var resolverCalls = 0
        assertTrue(TrustedServerProfile.configureIfFresh(f, { resolverCalls++; listOf("127.0.0.1:53") }) {
            stream(" dmsg://server/AA\nAA\r\n")
        })
        assertEquals(1, resolverCalls)
        assertEquals(1, f.previewCalls)
        assertNotNull(f.profile)
        assertFalse(f.authenticated)
        assertEquals(0, f.signupCalls)
    }

    @Test fun existingAccountOrProfileNeverOpensAssetOrResolves() {
        val f = FakeFacade()
        f.configureDns("dmsg://server/AAAA", listOf("127.0.0.1:53"))
        val before = f.profile
        assertFalse(TrustedServerProfile.configureIfFresh(f, { error("must not resolve") }) { error("must not open") })
        assertSame(before, f.profile)
        f.profile = null; f.authenticated = true
        assertFalse(TrustedServerProfile.configureIfFresh(f, { error("must not resolve") }) { error("must not open") })
    }

    @Test fun missingAssetRetainsManualFlowAndBadAssetFailsWithoutMutation() {
        val f = FakeFacade()
        assertFalse(TrustedServerProfile.configureIfFresh(f, { error("must not resolve") }) { null })
        assertEquals(LaunchState.Connection, AuthFlow(f).launchState())
        for (value in listOf("A".repeat(43), "dmsg://contact/AAAA", "dmsg://server/" + "A".repeat(QrGate.SERVER_MAX),
            "é", "A".repeat(QrGate.URI_MAX + 1))) {
            assertTrue(runCatching { TrustedServerProfile.configureIfFresh(f, { error("must not resolve") }) { stream(value) } }.isFailure)
            assertNull(f.profile)
            assertFalse(f.authenticated)
        }
        assertEquals(0, f.previewCalls)
    }
}

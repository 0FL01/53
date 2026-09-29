package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

/** QrGate routing without the native lib (oversized/bad prefix explicit). */
class QrGateTest {
    @Test fun routesBothFormats() {
        assertEquals("join", QrGate.route("dmsg://join/AAAA").getOrThrow())
        assertEquals("contact", QrGate.route("dmsg://contact/AAAA").getOrThrow())
    }

    @Test fun badPrefixIsExplicit() {
        val e = QrGate.route("https://x/").exceptionOrNull()
        assertEquals("bad prefix", e?.message)
    }

    @Test fun oversizedIsExplicit() {
        val bigJoin = "dmsg://join/" + "A".repeat(QrGate.URI_MAX)
        assertEquals("oversized", QrGate.route(bigJoin).exceptionOrNull()?.message)
        val bigContact = "dmsg://contact/" + "A".repeat(QrGate.URI_MAX)
        assertEquals("oversized", QrGate.route(bigContact).exceptionOrNull()?.message)
    }
}

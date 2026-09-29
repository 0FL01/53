package org.dmsg.client

/**
 * Pure pre-check routing for scanned QR (mirror of Rust qr_kind caps).
 * Unit-tested without the native lib. The core re-validates everything;
 * this only decides which screen/error to show first.
 */
object QrGate {
    const val URI_MAX = 8192
    const val JOIN_PREFIX = "dmsg://join/"
    const val CONTACT_PREFIX = "dmsg://contact/"

    /** "join" | "contact" | error string ("bad prefix" | "oversized"). */
    fun route(uri: String): Result<String> {
        if (uri.length > URI_MAX) return Result.failure(DmsgError("oversized"))
        return when {
            uri.startsWith(JOIN_PREFIX) -> Result.success("join")
            uri.startsWith(CONTACT_PREFIX) -> Result.success("contact")
            else -> Result.failure(DmsgError("bad prefix"))
        }
    }
}

package org.dmsg.client

import uniffi.dmsg_core.QrKind

/** Shared paste/scan input; full bounded/versioned parsing belongs to Rust. */
object QrGate {
    const val URI_MAX = 8192
    const val SERVER_MAX = 13 + (4096 * 4 + 2) / 3 // Rust's 4 KiB raw profile + prefix.
    fun normalize(input: String): String {
        if (input.toByteArray(Charsets.UTF_8).size > URI_MAX) throw DmsgError("Код слишком большой")
        return input.filterNot { it == ' ' || it == '\t' || it == '\r' || it == '\n' }
    }
    fun route(input: String): Result<QrKind> = runCatching {
        val uri = normalize(input)
        when {
            uri.startsWith("dmsg://server/") && uri.length <= SERVER_MAX -> QrKind.SERVER
            uri.startsWith("dmsg://contact/") -> QrKind.CONTACT
            else -> throw DmsgError("Неверный тип или размер кода")
        }
    }
    fun serverPreview(f: DmsgFacade, input: String): ServerPreview {
        val code = normalize(input)
        if (route(code).getOrThrow() != QrKind.SERVER || f.qrKind(code) != QrKind.SERVER)
            throw DmsgError("Нужен публичный код сервера, а не QR контакта")
        val (domain, fingerprint) = f.profilePreview(code)
        return ServerPreview(code, domain, fingerprint)
    }
}

data class ServerPreview(val code: String, val domain: String, val fingerprint: String)

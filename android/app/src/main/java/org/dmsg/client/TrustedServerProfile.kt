package org.dmsg.client

import java.io.InputStream

/** The APK distributor supplies this public trust anchor; invitations cannot supply pins. */
internal object TrustedServerProfile {
    const val ASSET = "server-profile.txt"

    fun read(stream: InputStream): String {
        val bytes = ByteArray(QrGate.URI_MAX + 1)
        try {
            var count = 0
            while (count < bytes.size) {
                val next = stream.read()
                if (next == -1) break
                bytes[count++] = next.toByte()
            }
            if (count == bytes.size || (0 until count).any { bytes[it].toInt() !in 0..127 })
                throw DmsgError("Неверный встроенный профиль сервера", ErrorKind.BadQr)
            val code = QrGate.normalize(String(bytes, 0, count, Charsets.US_ASCII))
            if (QrGate.route(code).getOrThrow() != uniffi.dmsg_core.QrKind.SERVER)
                throw DmsgError("Неверный встроенный профиль сервера", ErrorKind.BadQr)
            return code
        } finally { bytes.fill(0) }
    }

    /** Check persisted state before even opening the packaged asset. Full validation stays native. */
    fun configureIfFresh(f: DmsgFacade, resolvers: () -> List<String>, open: () -> InputStream?): Boolean {
        if (f.account().authenticated || f.dnsProfile() != null) return false
        val code = open()?.use(::read) ?: return false
        val preview = QrGate.serverPreview(f, code)
        f.configureDns(preview.code, resolvers())
        return true
    }
}

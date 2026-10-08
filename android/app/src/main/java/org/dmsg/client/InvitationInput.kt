package org.dmsg.client

import java.io.InputStream

/** Raw 32-byte base64url invitation only. Never normalize a URI or arbitrary whitespace. */
internal object InvitationInput {
    const val TOKEN_LENGTH = 43
    const val FILE_MAX = 45 // An optional final LF or CRLF from the administrator's file.
    private const val alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"

    fun isCanonical(value: String): Boolean = value.length == TOKEN_LENGTH &&
        value.all { it in alphabet } && alphabet.indexOf(value.last()) % 4 == 0

    fun parse(value: String): CharArray {
        if (!isCanonical(value)) throw DmsgError(R.string.error_invite_format, ErrorKind.InvalidInput)
        return value.toCharArray()
    }

    fun read(stream: InputStream): CharArray {
        val bytes = ByteArray(FILE_MAX + 1)
        try {
            var count = 0
            while (count < bytes.size) {
                val next = stream.read()
                if (next == -1) break
                bytes[count++] = next.toByte()
            }
            val tokenSize = when {
                count == TOKEN_LENGTH -> count
                count == TOKEN_LENGTH + 1 && bytes[count - 1] == 10.toByte() -> count - 1
                count == FILE_MAX && bytes[count - 2] == 13.toByte() && bytes[count - 1] == 10.toByte() -> count - 2
                else -> throw DmsgError(R.string.error_invite_file_format, ErrorKind.InvalidInput)
            }
            if ((0 until tokenSize).any { bytes[it].toInt() !in 0..127 })
                throw DmsgError(R.string.error_invite_format, ErrorKind.InvalidInput)
            return parse(String(bytes, 0, tokenSize, Charsets.US_ASCII))
        } finally { bytes.fill(0) }
    }
}

/** Activity-local ownership. Taking transfers the buffer to AuthSecrets, clearing wipes it. */
internal class InvitationMemory {
    private var value: CharArray? = null
    val hasInvitation: Boolean get() = value != null
    fun replace(next: CharArray) { clear(); value = next }
    fun take(): CharArray = value?.also { value = null } ?: charArrayOf()
    fun clear() { value?.fill('\u0000'); value = null }
    fun peek(): String? = value?.concatToString()
}

/** One-shot process-local scanner handoff; only its non-secret ticket enters an Intent. */
internal object InvitationScanTransfer {
    private var sequence = 0L
    private var ticket: Long? = null
    private val memory = InvitationMemory()
    @Synchronized fun begin(): Long { memory.clear(); return (++sequence).also { ticket = it } }
    @Synchronized fun publish(owner: Long, value: CharArray): Boolean {
        if (ticket != owner) { value.fill('\u0000'); return false }
        memory.replace(value)
        return true
    }
    @Synchronized fun take(owner: Long): CharArray? {
        if (ticket != owner) return null
        ticket = null
        return if (memory.hasInvitation) memory.take() else null
    }
    @Synchronized fun cancel(owner: Long) {
        if (ticket == owner) { memory.clear(); ticket = null }
    }
}

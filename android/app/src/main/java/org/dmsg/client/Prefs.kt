package org.dmsg.client

import android.content.Context

/** Transport + mode settings. Secrets (keys) live only in the Rust DB file. */
object Prefs {
    private const val F = "dmsg"
    private const val ECONOMY = "economy"

    fun addr(c: Context): String =
        c.getSharedPreferences(F, Context.MODE_PRIVATE).getString("addr", "") ?: ""

    fun setAddr(c: Context, v: String) {
        c.getSharedPreferences(F, Context.MODE_PRIVATE).edit().putString("addr", v).apply()
    }

    fun domain(c: Context): String =
        c.getSharedPreferences(F, Context.MODE_PRIVATE).getString("domain", "") ?: ""

    fun setDomain(c: Context, v: String) {
        c.getSharedPreferences(F, Context.MODE_PRIVATE).edit().putString("domain", v).apply()
    }

    /** Server Noise static pubkey, hex (32 bytes). Not a secret. */
    fun serverPub(c: Context): ByteArray? {
        val h = c.getSharedPreferences(F, Context.MODE_PRIVATE).getString("server_pub", null)
            ?: return null
        return hexToBytes(h)?.takeIf { it.size == 32 }
    }

    fun setServerPub(c: Context, hex: String) {
        c.getSharedPreferences(F, Context.MODE_PRIVATE).edit().putString("server_pub", hex).apply()
    }

    /** "Economy" is only a poll-interval flag (no second service impl). */
    fun economy(c: Context): Boolean =
        c.getSharedPreferences(F, Context.MODE_PRIVATE).getBoolean(ECONOMY, false)

    fun setEconomy(c: Context, v: Boolean) {
        c.getSharedPreferences(F, Context.MODE_PRIVATE).edit().putBoolean(ECONOMY, v).apply()
    }

    fun hexToBytes(h: String): ByteArray? {
        if (h.length % 2 != 0) return null
        return try {
            ByteArray(h.length / 2) { i ->
                h.substring(i * 2, i * 2 + 2).toInt(16).toByte()
            }
        } catch (_: Exception) {
            null
        }
    }

    fun bytesToHex(b: ByteArray): String {
        val h = "0123456789abcdef"
        val s = StringBuilder(b.size * 2)
        for (x in b) {
            s.append(h[(x.toInt() ushr 4) and 0xF]).append(h[x.toInt() and 0xF])
        }
        return s.toString()
    }
}

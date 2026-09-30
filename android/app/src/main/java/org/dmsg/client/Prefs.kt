package org.dmsg.client

import android.content.Context

/** Public DNS metadata only; core is the source of trust. No credentials here. */
object Prefs {
    private const val F = "dmsg"
    fun mirrorDns(c: Context, p: DnsProfile) {
        val ok = c.getSharedPreferences(F, Context.MODE_PRIVATE).edit()
            .remove("addr").remove("domain").remove("server_pub")
            .putString("dns_domain", p.domain).putString("dns_pin", p.fingerprint)
            .putString("dns_noise_pub", bytesToHex(p.pub)).commit()
        if (!ok) throw DmsgError("Не удалось сохранить сведения о сервере")
    }
    fun economy(c: Context): Boolean = c.getSharedPreferences(F, Context.MODE_PRIVATE).getBoolean("economy", false)
    fun setEconomy(c: Context, v: Boolean) { c.getSharedPreferences(F, Context.MODE_PRIVATE).edit().putBoolean("economy", v).apply() }
    fun bytesToHex(b: ByteArray): String = b.joinToString("") { "%02x".format(it.toInt() and 255) }
}

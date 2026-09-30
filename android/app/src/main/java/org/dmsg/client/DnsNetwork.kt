package org.dmsg.client

import android.content.Context
import android.net.ConnectivityManager
import java.net.Inet4Address

/** Numeric UDP resolvers supplied by the active network; no guessed fallback. */
internal object DnsNetwork {
    fun resolvers(c: Context): List<String> {
        val cm = c.getSystemService(ConnectivityManager::class.java)
        val network = cm.activeNetwork ?: throw DmsgError("нет активной сети")
        val addresses = cm.getLinkProperties(network)?.dnsServers.orEmpty()
        val v4 = addresses.filterIsInstance<Inet4Address>()
        val selected = if (v4.isNotEmpty()) v4 else addresses
        if (selected.isEmpty()) throw DmsgError("сеть не предоставила DNS resolver")
        return selected.take(8).map {
            val ip = it.hostAddress ?: throw DmsgError("нет числового DNS адреса")
            if (it is Inet4Address) "$ip:53" else "[$ip]:53"
        }
    }

    fun mirrorProfile(c: Context, f: DmsgFacade) {
        val p = f.dnsProfile() ?: throw DmsgError("DNS профиль не сохранён")
        val hex = p.pub.joinToString("") { "%02x".format(it.toInt() and 255) }
        Prefs.setTransport(c, "dns", p.domain, hex)
    }
}

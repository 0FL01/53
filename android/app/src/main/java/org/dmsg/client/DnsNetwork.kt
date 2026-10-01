package org.dmsg.client

import android.content.Context
import android.net.ConnectivityManager
import java.net.Inet4Address

/** Numeric UDP resolvers supplied by the active network; no guessed fallback. */
internal object DnsNetwork {
    fun resolvers(c: Context): List<String> {
        val cm = c.getSystemService(ConnectivityManager::class.java)
        val network = cm.activeNetwork ?: throw DmsgError("Нет активной сети", ErrorKind.Transport)
        val addresses = cm.getLinkProperties(network)?.dnsServers.orEmpty()
        val v4 = addresses.filterIsInstance<Inet4Address>()
        val selected = if (v4.isNotEmpty()) v4 else addresses
        if (selected.isEmpty()) throw DmsgError("Сеть не предоставила DNS-резолвер", ErrorKind.Transport)
        return selected.take(8).map {
            val ip = it.hostAddress ?: throw DmsgError("Нет числового DNS-адреса", ErrorKind.Transport)
            if (it is Inet4Address) "$ip:53" else "[$ip]:53"
        }
    }

    fun mirrorProfile(c: Context, f: DmsgFacade) {
        val p = f.dnsProfile() ?: throw DmsgError("DNS профиль не сохранён")
        Prefs.mirrorDns(c, p)
    }
}

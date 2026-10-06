package org.dmsg.client

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import java.net.Inet4Address

internal data class DnsSnapshot<N>(val network: N, val resolvers: List<String>)

/** Runtime-only state, shared by all facades for one DB. Never takes the store lock. */
internal class DnsRuntimeState<N> {
    private var observed = false
    private var desired: DnsSnapshot<N>? = null
    private var revision = 0L
    private var applied = 0L

    // Read the current default network here, rather than trusting a queued callback's Network.
    @Synchronized fun observe(read: () -> DnsSnapshot<N>?, cancel: () -> Unit) {
        val next = read()
        if (observed && next == desired) return
        cancel() // no DB access; a blocked command must release before we can apply DNS
        observed = true
        desired = next
        revision++
    }

    /** Caller serializes resolver application with commands using Core.storeLock. */
    fun apply(change: (List<String>) -> Unit, wake: () -> Unit) {
        while (true) {
            val (version, snapshot) = synchronized(this) {
                if (applied == revision) return
                Pair(revision, desired)
            }
            val resolvers = snapshot?.resolvers.orEmpty()
            if (resolvers.isNotEmpty()) change(resolvers)
            synchronized(this) {
                // A newer observation may cancel while change is waiting on native/store work.
                if (revision == version) {
                    applied = version
                    if (resolvers.isNotEmpty()) wake()
                    return
                }
            }
        }
    }
}

/** Numeric UDP resolvers supplied by the active network; no guessed fallback. */
internal object DnsNetwork {
    private val states = mutableMapOf<String, DnsRuntimeState<Network>>()
    @Synchronized fun runtime(dbPath: String): DnsRuntimeState<Network> =
        states.getOrPut(dbPath) { DnsRuntimeState() }

    fun snapshot(c: Context): DnsSnapshot<Network>? {
        val cm = c.getSystemService(ConnectivityManager::class.java)
        val network = cm.activeNetwork ?: return null
        val addresses = cm.getLinkProperties(network)?.dnsServers.orEmpty()
        val v4 = addresses.filterIsInstance<Inet4Address>()
        val selected = if (v4.isNotEmpty()) v4 else addresses
        val resolvers = selected.take(8).map {
            val ip = it.hostAddress ?: throw DmsgError(R.string.error_dns_numeric, ErrorKind.Transport)
            if (it is Inet4Address) "$ip:53" else "[$ip]:53"
        }
        return DnsSnapshot(network, resolvers)
    }

    fun resolvers(c: Context): List<String> {
        val snapshot = snapshot(c) ?: throw DmsgError(R.string.error_no_network, ErrorKind.Transport)
        if (snapshot.resolvers.isEmpty()) throw DmsgError(R.string.error_no_dns, ErrorKind.Transport)
        return snapshot.resolvers
    }

    fun mirrorProfile(c: Context, f: DmsgFacade) {
        val p = f.dnsProfile() ?: throw DmsgError(R.string.error_profile_unsaved)
        Prefs.mirrorDns(c, p)
    }
}

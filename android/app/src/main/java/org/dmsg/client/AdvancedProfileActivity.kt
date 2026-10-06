package org.dmsg.client

import android.os.Bundle
import android.widget.TextView

/** Public pinned data only. Raw native debug state is never treated as health. */
class AdvancedProfileActivity : DmsgActivity() {
    private var active = false
    private val guard = UiGuard()
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); setContentView(R.layout.activity_advanced_profile)
        NativeUi.back(this, getString(R.string.title_server_profile))
    }
    override fun onResume() {
        super.onResume(); active = true
        val stamp = guard.begin() ?: return
        Core.dispatch {
            val result = runCatching { Core.facade(applicationContext).dnsProfile() }
            runOnUiThread {
                if (!active || !guard.finish(stamp)) return@runOnUiThread
                findViewById<TextView>(R.id.profile_details).text = result.fold({ p ->
                    p?.let { getString(R.string.profile_details, it.domain, it.fingerprint, Prefs.bytesToHex(it.pub), it.resolvers.joinToString("\n")) }
                        ?: getString(R.string.profile_missing)
                }, { humanError(resources, it) })
            }
        }
    }
    override fun onPause() { active = false; guard.stop(); super.onPause() }
}

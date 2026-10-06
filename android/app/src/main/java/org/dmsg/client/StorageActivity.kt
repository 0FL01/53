package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AlertDialog

/** Same-install snapshots and cache are distinct; no history deletion or key export CTA. */
class StorageActivity : DmsgActivity() {
    private val guard = UiGuard()
    private var active = false
    private var prompt: AlertDialog? = null
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); setContentView(R.layout.activity_storage)
        NativeUi.back(this, getString(R.string.title_storage))
        findViewById<Button>(R.id.btn_migrate).setOnClickListener {
            work(getString(R.string.saving_copy), { SecureStore.seal(applicationContext) }) { getString(R.string.copy_saved) }
        }
        findViewById<Button>(R.id.btn_unseal).setOnClickListener {
            prompt = AlertDialog.Builder(this).setTitle(R.string.restore_title)
                .setMessage(R.string.restore_warning)
                .setNegativeButton(R.string.cancel, null).setPositiveButton(R.string.restore_confirm) { _, _ ->
                    work(getString(R.string.restoring_copy), { SecureStore.unseal(applicationContext) }) { getString(R.string.copy_restored) }
                }.show()
        }
        findViewById<Button>(R.id.btn_wipe_cache).setOnClickListener {
            work(getString(R.string.clearing_cache), { cacheDir.deleteRecursively() }) {
                getString(if (it) R.string.cache_cleared else R.string.cache_partial)
            }
        }
    }
    override fun onResume() { super.onResume(); active = true; showPlan() }
    override fun onPause() { active = false; guard.stop(); prompt?.dismiss(); prompt = null; super.onPause() }
    private fun controls() {
        for (id in listOf(R.id.btn_migrate, R.id.btn_unseal, R.id.btn_wipe_cache)) findViewById<Button>(id).isEnabled = !guard.pending
    }
    private fun <T> work(message: String, task: () -> T, success: (T) -> String) {
        val stamp = guard.begin() ?: return
        findViewById<TextView>(R.id.info).text = message; controls()
        Core.dispatch {
            val result = runCatching(task)
            runOnUiThread {
                if (!active || !guard.finish(stamp)) return@runOnUiThread
                findViewById<TextView>(R.id.info).text = result.fold(success, { humanError(resources, it) })
                controls(); showPlan()
            }
        }
    }
    private fun showPlan() {
        val stamp = guard.begin() ?: return
        controls()
        Core.dispatch {
            val result = runCatching { Triple(SecureStore.plan(applicationContext), Core.dbFile(applicationContext).exists(), SecureStore.sealedDb(applicationContext).exists()) }
            runOnUiThread {
            if (!active || !guard.finish(stamp)) return@runOnUiThread
            result.fold({ (plan, live, copy) ->
            findViewById<TextView>(R.id.plan).text = getString(when (plan) {
                "ready" -> R.string.store_ready
                "reinstall_loss" -> R.string.store_key_lost
                "migrate" -> R.string.store_unwrapped
                else -> R.string.store_new
            }) + getString(R.string.store_files, getString(if (live) R.string.file_present else R.string.file_absent), getString(if (copy) R.string.file_present else R.string.file_absent))
            }, { findViewById<TextView>(R.id.plan).text = humanError(resources, it) })
            controls()
            }
        }
    }
}

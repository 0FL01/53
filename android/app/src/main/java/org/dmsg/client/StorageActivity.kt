package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/** Storage settings: Keystore plan, migrate, wipe cache. */
class StorageActivity : AppCompatActivity() {
    private lateinit var plan: TextView
    private lateinit var info: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_storage)
        plan = findViewById(R.id.plan)
        info = findViewById(R.id.info)
        findViewById<Button>(R.id.btn_migrate).setOnClickListener { migrate() }
        findViewById<Button>(R.id.btn_wipe_cache).setOnClickListener { wipeCache() }
    }

    override fun onResume() {
        super.onResume()
        showPlan()
    }

    private fun showPlan() {
        Thread {
            val p = try {
                SecureStore.plan(this)
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { plan.text = "plan=$p" }
        }.start()
    }

    private fun migrate() {
        Thread {
            val out = try {
                SecureStore.seal(this)
                "migrated: plaintext wiped, sealed copy kept"
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { info.text = out; showPlan() }
        }.start()
    }

    private fun wipeCache() {
        Thread {
            val out = try {
                cacheDir.deleteRecursively()
                "cache wiped (identity kept)"
            } catch (e: Exception) {
                "error: ${e.message}"
            }
            runOnUiThread { info.text = out }
        }.start()
    }
}

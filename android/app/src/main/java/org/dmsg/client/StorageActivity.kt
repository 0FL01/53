package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/** Storage settings: Keystore plan, migrate, wipe cache. */
class StorageActivity : DmsgActivity() {
    private lateinit var plan: TextView
    private lateinit var info: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_storage)
        plan = findViewById(R.id.plan)
        info = findViewById(R.id.info)
        findViewById<Button>(R.id.btn_migrate).setOnClickListener { migrate() }
        findViewById<Button>(R.id.btn_unseal).setOnClickListener { restore() }
        findViewById<Button>(R.id.btn_wipe_cache).setOnClickListener { wipeCache() }
    }

    override fun onResume() {
        super.onResume()
        showPlan()
    }

    private fun showPlan() {
        Core.dispatch {
            val p = try {
                SecureStore.plan(this)
            } catch (e: Exception) {
                humanError(e)
            }
            runOnUiThread { plan.text = "plan=$p" }
        }
    }

    private fun migrate() {
        Core.dispatch {
            val out = try {
                SecureStore.seal(this)
                "encrypted migration + same-install sealed snapshot ready"
            } catch (e: Exception) {
                humanError(e)
            }
            runOnUiThread { info.text = out; showPlan() }
        }
    }

    private fun restore() {
        Core.dispatch {
            val out = try {
                SecureStore.unseal(this)
                "restored: identity verified"
            } catch (e: Exception) {
                humanError(e)
            }
            runOnUiThread { info.text = out; showPlan() }
        }
    }

    private fun wipeCache() {
        Core.dispatch {
            val out = try {
                cacheDir.deleteRecursively()
                "cache wiped (identity kept)"
            } catch (e: Exception) {
                humanError(e)
            }
            runOnUiThread { info.text = out }
        }
    }
}

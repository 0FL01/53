package org.dmsg.client

import android.os.Bundle
import android.view.View
import android.view.WindowManager
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowCompat
import androidx.core.view.updatePadding

/** Keep controls readable above system bars on edge-to-edge Android versions. */
abstract class DmsgActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
    }
    protected open fun onImeVisibilityChanged(visible: Boolean) {}
    override fun onPostCreate(savedInstanceState: Bundle?) {
        super.onPostCreate(savedInstanceState)
        val content = findViewById<View>(android.R.id.content)
        val left = content.paddingLeft
        val top = content.paddingTop
        val right = content.paddingRight
        val bottom = content.paddingBottom
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.ime())
            view.updatePadding(left + bars.left, top + bars.top, right + bars.right, bottom + bars.bottom)
            onImeVisibilityChanged(insets.isVisible(WindowInsetsCompat.Type.ime()))
            insets
        }
        ViewCompat.requestApplyInsets(content)
    }
}

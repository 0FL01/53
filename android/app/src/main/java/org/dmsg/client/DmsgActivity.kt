package org.dmsg.client

import android.os.Bundle
import android.view.View
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.updatePadding

/** Keep controls readable above system bars on edge-to-edge Android versions. */
abstract class DmsgActivity : AppCompatActivity() {
    override fun onPostCreate(savedInstanceState: Bundle?) {
        super.onPostCreate(savedInstanceState)
        val content = findViewById<View>(android.R.id.content)
        val left = content.paddingLeft
        val top = content.paddingTop
        val right = content.paddingRight
        val bottom = content.paddingBottom
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            view.updatePadding(left + bars.left, top + bars.top, right + bars.right, bottom + bars.bottom)
            insets
        }
        ViewCompat.requestApplyInsets(content)
    }
}

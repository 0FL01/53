package org.dmsg.client

import android.content.Context
import android.util.AttributeSet
import android.view.View
import android.widget.ScrollView

/** Long safety/errors stay fully scrollable without pushing the composer off-screen at 200%. */
class NoticeScrollView(context: Context, attrs: AttributeSet?) : ScrollView(context, attrs) {
    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val mode = View.MeasureSpec.getMode(heightMeasureSpec)
        val size = View.MeasureSpec.getSize(heightMeasureSpec)
        val bounded = if (mode == View.MeasureSpec.UNSPECIFIED) heightMeasureSpec else
            View.MeasureSpec.makeMeasureSpec(maxOf(NativeUi.dp(context, 48), size / 4), View.MeasureSpec.AT_MOST)
        super.onMeasure(widthMeasureSpec, bounded)
    }
}

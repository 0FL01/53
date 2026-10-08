package org.dmsg.client

import android.content.Context
import android.util.AttributeSet
import android.view.View
import android.widget.LinearLayout
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

/** LinearLayout otherwise measures a footer after weighted history against the full height,
 * even when the preceding notice/header already consumed space: no internal scroll range. */
class ChatFooterScrollView(context: Context, attrs: AttributeSet?) : ScrollView(context, attrs) {
    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val column = parent as? LinearLayout
        val mode = View.MeasureSpec.getMode(heightMeasureSpec)
        val bounded = if (column == null || mode == View.MeasureSpec.UNSPECIFIED) heightMeasureSpec else {
            val used = (0 until column.childCount).sumOf { index ->
                val sibling = column.getChildAt(index)
                val params = sibling.layoutParams as LinearLayout.LayoutParams
                if (sibling === this || sibling.visibility == View.GONE || params.weight > 0f) 0
                else sibling.measuredHeight + params.topMargin + params.bottomMargin
            }
            View.MeasureSpec.makeMeasureSpec(maxOf(0, View.MeasureSpec.getSize(heightMeasureSpec) - used), View.MeasureSpec.AT_MOST)
        }
        super.onMeasure(widthMeasureSpec, bounded)
    }
}

package org.dmsg.client

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.os.Bundle
import android.util.AttributeSet
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.Button
import android.widget.LinearLayout
import androidx.appcompat.widget.AppCompatImageButton
import androidx.core.content.ContextCompat
import kotlin.math.max

/** Sampled RMS halo: 200 ms interpolation, respecting the system animation-off setting. */
class VoiceMicView @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : AppCompatImageButton(context, attrs) {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = ContextCompat.getColor(context, R.color.accent_soft) }
    private var amplitude = 0f
    private var animator: ValueAnimator? = null
    private var target = -1f
    init {
        setImageResource(R.drawable.ic_mic)
        contentDescription = context.getString(R.string.voice_record)
        minimumHeight = NativeUi.dp(context, 48); minimumWidth = NativeUi.dp(context, 48)
        setBackgroundColor(ContextCompat.getColor(context, R.color.surface))
    }
    fun meter(rms: Float, recording: Boolean) {
        val next = if (recording) .3f + (rms * 4f).coerceIn(0f, .7f) else 0f
        if (next == target) return
        target = next; animator?.cancel()
        if (!ValueAnimator.areAnimatorsEnabled()) { amplitude = next; invalidate(); return }
        animator = ValueAnimator.ofFloat(amplitude, next).apply {
            duration = 200
            addUpdateListener { amplitude = it.animatedValue as Float; invalidate() }
            start()
        }
    }
    override fun onDraw(canvas: Canvas) {
        if (amplitude > 0f) canvas.drawCircle(width / 2f, height / 2f, minOf(width, height) * (.28f + .2f * amplitude), paint)
        super.onDraw(canvas)
    }
    override fun onDetachedFromWindow() { animator?.cancel(); super.onDetachedFromWindow() }
}

/** Native waveform with a real seek range and a 48 dp touch/accessibility target. */
class VoiceWaveformView @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : View(context, attrs) {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    var bars: ByteArray = byteArrayOf(); set(value) { field = value; invalidate() }
    var totalSamples: Int = 0; set(value) { field = value; invalidate() }
    var sample: Int = 0; set(value) { field = value.coerceIn(0, totalSamples); invalidate() }
    var onSeek: ((Int) -> Unit)? = null
    init { minimumHeight = NativeUi.dp(context, 48); isFocusable = true; contentDescription = context.getString(R.string.voice_seek) }
    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val count = max(1, bars.size.coerceAtMost(96))
        val step = width.toFloat() / count
        val fraction = if (totalSamples > 0) sample.toFloat() / totalSamples else 0f
        paint.strokeWidth = max(1f, step * .45f); paint.strokeCap = Paint.Cap.ROUND
        for (i in 0 until count) {
            paint.color = ContextCompat.getColor(context, if ((i + .5f) / count <= fraction) R.color.accent else R.color.text_secondary)
            val value = bars.getOrNull(i)?.toInt()?.and(255) ?: 0
            val size = max(NativeUi.dp(context, 3).toFloat(), height * .68f * value / 255f)
            val x = (i + .5f) * step
            canvas.drawLine(x, (height - size) / 2, x, (height + size) / 2, paint)
        }
    }
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!isEnabled || onSeek == null || totalSamples <= 0) return false
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> { parent?.requestDisallowInterceptTouchEvent(true); return true }
            MotionEvent.ACTION_MOVE -> return true
            MotionEvent.ACTION_UP -> {
                onSeek?.invoke((event.x / width.coerceAtLeast(1) * totalSamples).toInt().coerceIn(0, totalSamples))
                performClick(); parent?.requestDisallowInterceptTouchEvent(false); return true
            }
            MotionEvent.ACTION_CANCEL -> { parent?.requestDisallowInterceptTouchEvent(false); return true }
        }
        return super.onTouchEvent(event)
    }
    override fun performClick(): Boolean { super.performClick(); return true }
    override fun onInitializeAccessibilityNodeInfo(info: AccessibilityNodeInfo) {
        super.onInitializeAccessibilityNodeInfo(info); info.className = "android.widget.SeekBar"
        info.rangeInfo = AccessibilityNodeInfo.RangeInfo.obtain(AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_INT, 0f, totalSamples.toFloat(), sample.toFloat())
        if (isEnabled && onSeek != null) {
            info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_PROGRESS)
            info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_FORWARD)
            info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_BACKWARD)
        }
    }
    override fun performAccessibilityAction(action: Int, arguments: Bundle?): Boolean {
        if (!isEnabled || onSeek == null) return super.performAccessibilityAction(action, arguments)
        val value = when (action) {
            AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_PROGRESS.id -> arguments?.getFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE)?.toInt() ?: return false
            AccessibilityNodeInfo.ACTION_SCROLL_FORWARD -> sample + 80_000
            AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD -> sample - 80_000
            else -> return super.performAccessibilityAction(action, arguments)
        }
        onSeek?.invoke(value.coerceIn(0, totalSamples)); return true
    }
}

internal class VoiceBubbleView(context: Context) : LinearLayout(context) {
    private val control = Button(context).apply { minWidth = NativeUi.dp(context, 48); minHeight = NativeUi.dp(context, 48) }
    private val waveform = VoiceWaveformView(context)
    private val label = NativeUi.text(context, 12f, true)
    var key: VoiceKey? = null; private set
    init {
        orientation = VERTICAL
        val top = LinearLayout(context).apply { gravity = Gravity.CENTER_VERTICAL }
        top.addView(control, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT))
        top.addView(waveform, LayoutParams(0, NativeUi.dp(context, 48), 1f))
        addView(top); addView(label)
    }
    fun bind(row: uniffi.dmsg_core.HistoryMessage, transfer: VoiceTransferUi, playback: VoicePlayback,
        onAction: (uniffi.dmsg_core.HistoryMessage) -> Unit, onSeek: (uniffi.dmsg_core.HistoryMessage, Int) -> Unit) {
        key = VoiceKey(row)
        val voice = row.voice ?: return
        val playing = playback.key == key && playback.playing
        control.setText(if (!voice.downloaded) R.string.voice_download else if (playing) R.string.voice_pause else R.string.voice_play)
        control.contentDescription = control.text; control.isEnabled = voice.downloaded || !transfer.active
        control.setOnClickListener { onAction(row) }
        waveform.bars = voice.waveform; waveform.totalSamples = voice.sampleCount.toInt()
        waveform.sample = if (playback.key == key) playback.sample else 0
        waveform.isEnabled = voice.downloaded
        waveform.onSeek = if (voice.downloaded) { value -> onSeek(row, value) } else null
        label.text = when {
            transfer.errorRes != null -> context.getString(transfer.errorRes) + " · " + context.getString(R.string.voice_byte_progress, transfer.transferred.toString(), transfer.total.toString())
            transfer.active || transfer.transferred > 0u && !transfer.complete -> context.getString(R.string.voice_byte_progress, transfer.transferred.toString(), transfer.total.toString())
            playback.key == key -> voiceTime(playback.sample) + " / " + voiceTime(voice.sampleCount.toInt())
            else -> context.getString(R.string.voice_duration_bytes, voiceTime(voice.sampleCount.toInt()), voice.byteLen.toString())
        }
    }
    fun playback(value: VoicePlayback) {
        if (value.key != key) return
        waveform.sample = value.sample
        control.setText(if (value.playing) R.string.voice_pause else R.string.voice_play)
        label.text = voiceTime(value.sample) + " / " + voiceTime(waveform.totalSamples)
    }
}

internal fun voiceView(root: View, key: VoiceKey): VoiceBubbleView? {
    if (root is VoiceBubbleView && root.key == key) return root
    if (root is android.view.ViewGroup) for (i in 0 until root.childCount) voiceView(root.getChildAt(i), key)?.let { return it }
    return null
}

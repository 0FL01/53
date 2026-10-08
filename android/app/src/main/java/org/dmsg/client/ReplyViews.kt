package org.dmsg.client

import android.content.Context
import android.graphics.Typeface
import android.text.TextUtils
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat.AccessibilityActionCompat
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.MessageKind
import uniffi.dmsg_core.ReplyInfo
import uniffi.dmsg_core.ReplyTargetState

/** The author belongs to the target, not the message carrying the quote. */
internal fun replyAuthor(context: Context, direction: MessageDirection?, peerName: String): String = when (direction) {
    MessageDirection.OUTGOING -> context.getString(R.string.reply_author_you)
    MessageDirection.INCOMING -> context.getString(R.string.reply_author_peer, peerName)
    null -> context.getString(R.string.reply_message_unavailable)
}

/** Strip rendering spans so quotes and composer previews have no selection or active links. */
internal fun replyPreview(context: Context, info: ReplyInfo, markdown: MessageMarkdown): String {
    if (info.state != ReplyTargetState.AVAILABLE) return context.getString(R.string.reply_message_unavailable)
    return when (info.kind) {
        MessageKind.TEXT -> markdown.render(replyTextPreview(info.preview)).toString()
        MessageKind.VOICE -> context.getString(R.string.reply_voice_preview,
            voiceTime(((info.voiceDurationMs ?: 0u).toLong() * 16).toInt()))
        null -> context.getString(R.string.reply_message_unavailable)
    }
}

/** Shallow preview of this row's own body; never borrow its nested reply or terminal content. */
internal fun replyInfoForTarget(row: HistoryMessage): ReplyInfo {
    val state = when {
        row.deletedAll -> ReplyTargetState.DELETED
        row.hiddenSelf -> ReplyTargetState.HIDDEN
        else -> ReplyTargetState.AVAILABLE
    }
    val available = state == ReplyTargetState.AVAILABLE
    return ReplyInfo(targetLocalId = row.localId, state = state, targetRevision = row.revision,
        direction = row.direction, kind = row.kind,
        preview = if (available && row.kind == MessageKind.TEXT) replyTextPreview(row.text) else "",
        voiceDurationMs = if (available && row.kind == MessageKind.VOICE) row.voice?.sampleCount?.let { (it + 15u) / 16u } else null)
}

private fun replyTextPreview(source: String): String {
    val count = source.codePointCount(0, source.length)
    return if (count <= 160) source else source.substring(0, source.offsetByCodePoints(0, 160))
}

/** Separate from the selectable message body: Copy can never include this header. */
internal fun replyQuoteView(context: Context, info: ReplyInfo, peerName: String, markdown: MessageMarkdown,
    onQuote: (Long) -> Unit, onHold: () -> Unit): View {
    val available = info.state == ReplyTargetState.AVAILABLE
    val author = if (available) replyAuthor(context, info.direction, peerName) else null
    val preview = replyPreview(context, info, markdown)
    return LinearLayout(context).apply {
        id = R.id.reply_quote
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        minimumHeight = NativeUi.dp(context, 48)
        setPadding(0, NativeUi.dp(context, 8), NativeUi.dp(context, 8), NativeUi.dp(context, 8))
        setBackgroundColor(ContextCompat.getColor(context, R.color.surface))
        contentDescription = listOfNotNull(author, preview).joinToString("\n")
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_YES
        addView(View(context).apply {
            setBackgroundColor(ContextCompat.getColor(context, R.color.accent))
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }, LinearLayout.LayoutParams(NativeUi.dp(context, 3), ViewGroup.LayoutParams.MATCH_PARENT))
        addView(LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
            if (author != null) addView(NativeUi.text(context, 14f).apply {
                text = author
                typeface = Typeface.DEFAULT_BOLD
                setTextColor(ContextCompat.getColor(context, R.color.accent))
                maxLines = 2
                ellipsize = TextUtils.TruncateAt.END
            })
            addView(NativeUi.text(context, 14f, true).apply {
                text = preview
                setTextIsSelectable(false)
                autoLinkMask = 0
                movementMethod = null
                maxLines = 2
                ellipsize = TextUtils.TruncateAt.END
            })
        }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
            marginStart = NativeUi.dp(context, 8)
        })
        if (available) info.targetLocalId?.let { targetId ->
            setOnClickListener { onQuote(targetId) }
            ViewCompat.replaceAccessibilityAction(this, AccessibilityActionCompat.ACTION_CLICK,
                context.getString(R.string.reply_open_original)) { _, _ -> onQuote(targetId); true }
        }
        setOnLongClickListener { onHold(); true }
    }
}

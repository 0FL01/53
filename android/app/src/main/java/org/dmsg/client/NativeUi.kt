package org.dmsg.client

import android.app.Activity
import android.content.Context
import android.content.res.Resources
import androidx.annotation.StringRes
import android.graphics.Typeface
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.content.ContextCompat
import uniffi.dmsg_core.DialogSummary
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import java.text.DateFormat
import java.util.Date

internal fun localTime(at: Long?): String = at?.let {
    DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(it))
}.orEmpty()

internal fun connectionLabel(resources: Resources, state: ConnectionUiState): String = when {
    state.pollInFlight -> resources.getString(R.string.connection_checking)
    state.lastFailure != null -> resources.getString(connectionFailureResource(state.lastFailure))
    !state.serviceEnabled -> resources.getString(R.string.connection_disabled)
    state.lastSuccessAt != null -> resources.getString(R.string.connection_last_success, localTime(state.lastSuccessAt))
    else -> resources.getString(R.string.connection_waiting)
}
@StringRes internal fun connectionFailureResource(kind: ErrorKind): Int = when (kind) {
    ErrorKind.PinMismatch -> R.string.connection_pin_mismatch
    ErrorKind.Revoked -> R.string.connection_revoked
    ErrorKind.NotAuthenticated -> R.string.connection_not_authenticated
    ErrorKind.NativeUnavailable -> R.string.connection_native_unavailable
    ErrorKind.Store -> R.string.connection_store
    ErrorKind.Crypto -> R.string.connection_crypto
    ErrorKind.Protocol -> R.string.connection_protocol
    ErrorKind.Busy -> R.string.connection_busy
    ErrorKind.Transport -> R.string.connection_transport
    else -> R.string.connection_failed
}

internal object NativeUi {
    fun dp(c: Context, value: Int) = (value * c.resources.displayMetrics.density).toInt()
    fun text(c: Context, size: Float = 16f, secondary: Boolean = false) = TextView(c).apply {
        textSize = size
        setTextColor(ContextCompat.getColor(c, if (secondary) R.color.text_secondary else R.color.text))
        setLineSpacing(dp(c, 3).toFloat(), 1f)
    }
    fun back(activity: Activity, title: String) {
        activity.findViewById<TextView>(R.id.screen_title)?.text = title
        activity.findViewById<Button>(R.id.btn_back)?.setOnClickListener { activity.finish() }
    }
    fun divider(c: Context) = View(c).apply { setBackgroundColor(ContextCompat.getColor(c, R.color.divider)) }
}

/** Stable IDs and natural-height text make large fonts/long messages scroll instead of clipping. */
internal class HistoryAdapter(private val context: Context, private val rows: List<HistoryMessage>) : BaseAdapter() {
    override fun getCount() = rows.size
    override fun getItem(position: Int) = rows[position]
    override fun getItemId(position: Int) = rows[position].localId
    override fun hasStableIds() = true
    override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
        val c = context
        val message = rows[position]
        val outgoing = message.direction == MessageDirection.OUTGOING
        val root = LinearLayout(c).apply {
            tag = message.localId
            gravity = if (outgoing) Gravity.END else Gravity.START
            setPadding(NativeUi.dp(c, 16), NativeUi.dp(c, 4), NativeUi.dp(c, 16), NativeUi.dp(c, 4))
        }
        val bubble = LinearLayout(c).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(NativeUi.dp(c, 12), NativeUi.dp(c, 10), NativeUi.dp(c, 12), NativeUi.dp(c, 10))
            setBackgroundColor(ContextCompat.getColor(c, if (outgoing) R.color.accent_soft else R.color.incoming))
        }
        val width = ((parent.width.takeIf { it > 0 } ?: c.resources.displayMetrics.widthPixels) - NativeUi.dp(c, 32)) * .86
        bubble.addView(NativeUi.text(c).apply { text = message.text; setTextIsSelectable(true) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        bubble.addView(NativeUi.text(c, 12f, true).apply {
            val time = when {
                message.serverTimestampMs != null -> c.getString(R.string.server_timestamp, localTime(message.serverTimestampMs))
                message.deliveryState == uniffi.dmsg_core.DeliveryState.QUEUED -> c.getString(R.string.pending_timestamp, localTime(message.localTimestampMs))
                else -> c.getString(R.string.legacy_timestamp, localTime(message.localTimestampMs))
            }
            text = listOf(if (outgoing) deliveryLabel(c.resources, message.deliveryState) else c.getString(R.string.message_incoming), time).joinToString(" · ")
        })
        root.addView(bubble, LinearLayout.LayoutParams(width.toInt(), ViewGroup.LayoutParams.WRAP_CONTENT))
        return root
    }
}

internal class DialogAdapter(private val context: Context, private val rows: List<DialogSummary>) : BaseAdapter() {
    override fun getCount() = rows.size
    override fun getItem(position: Int) = rows[position]
    override fun getItemId(position: Int) = position.toLong()
    override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
        val c = context
        val row = rows[position]
        val root = LinearLayout(c).apply {
            gravity = Gravity.TOP
            minimumHeight = NativeUi.dp(c, 76)
            setPadding(NativeUi.dp(c, 16), NativeUi.dp(c, 12), NativeUi.dp(c, 16), NativeUi.dp(c, 12))
        }
        root.addView(NativeUi.text(c, 16f).apply {
            text = row.contactId.take(2); gravity = Gravity.CENTER
            setBackgroundColor(ContextCompat.getColor(c, R.color.accent_soft))
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }, LinearLayout.LayoutParams(NativeUi.dp(c, 44), NativeUi.dp(c, 44)).apply { marginEnd = NativeUi.dp(c, 12) })
        val body = LinearLayout(c).apply { orientation = LinearLayout.VERTICAL }
        body.addView(NativeUi.text(c).apply { text = row.localAlias ?: row.contactId; typeface = Typeface.DEFAULT_BOLD })
        if (row.localAlias != null) body.addView(NativeUi.text(c, 12f, true).apply { text = row.contactId })
        body.addView(NativeUi.text(c, 14f, true).apply { text = row.preview ?: c.getString(R.string.no_messages); maxLines = 2 })
        body.addView(NativeUi.text(c, 12f, true).apply {
            text = listOfNotNull(row.lastLocalTimestampMs?.let { c.getString(R.string.local_timestamp, localTime(it)) },
                if (row.localUnread > 0uL) c.getString(R.string.local_unread, row.localUnread.toString()) else null).joinToString(" · ")
        })
        val contact = Dialog(row.contactId, row.state, row.identityMismatch, row.hasKeys)
        if (contactCta(contact) != ContactCta.Chat) body.addView(NativeUi.text(c, 12f).apply {
            text = trustLabel(c.resources, contact)
            setTextColor(ContextCompat.getColor(c, if (row.identityMismatch || row.state == "blocked") R.color.error else R.color.warning))
        })
        root.addView(body, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        return root
    }
}

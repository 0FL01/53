package org.dmsg.client

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.view.textclassifier.TextClassifier
import android.widget.TextView
import io.noties.markwon.AbstractMarkwonPlugin
import io.noties.markwon.LinkResolver
import io.noties.markwon.Markwon
import io.noties.markwon.MarkwonConfiguration
import io.noties.markwon.MarkwonSpansFactory
import io.noties.markwon.MarkwonVisitor
import io.noties.markwon.RenderProps
import io.noties.markwon.SoftBreakAddsNewLinePlugin
import io.noties.markwon.SpanFactory
import io.noties.markwon.core.CorePlugin
import io.noties.markwon.core.CoreProps
import io.noties.markwon.core.MarkwonTheme
import io.noties.markwon.core.spans.LinkSpan
import io.noties.markwon.ext.strikethrough.StrikethroughPlugin
import org.commonmark.node.HtmlBlock
import org.commonmark.node.HtmlInline
import org.commonmark.node.Image
import org.commonmark.node.Link
import org.commonmark.node.Node
import java.net.URI
import java.net.URISyntaxException

/** Pure policy: no Android spans, input normalization, or new-message validation. */
internal object MessageMarkdownPolicy {
    const val MAX_AST_DEPTH = 64

    /** Root counts as depth 1. Walk child/sibling/parent pointers without recursion. */
    fun withinDepthLimit(root: Node): Boolean {
        var node = root
        var depth = 1
        while (true) {
            if (depth > MAX_AST_DEPTH) return false
            val child = node.firstChild
            if (child != null) {
                node = child
                depth++
                continue
            }
            while (node !== root && node.next == null) {
                node = node.parent ?: return false
                depth--
            }
            if (node === root) return true
            node = node.next ?: return false
        }
    }

    /** Strict URI host parsing rejects relative/opaque URLs and malformed authorities. */
    fun allowsLink(destination: String): Boolean {
        val uri = try { URI(destination) } catch (_: URISyntaxException) { return false }
        return uri.isAbsolute && !uri.isOpaque &&
            (uri.scheme.equals("https", ignoreCase = true) || uri.scheme.equals("http", ignoreCase = true)) &&
            !uri.host.isNullOrEmpty()
    }
}

/** Only this application's filtered Markdown spans participate in Open link. */
internal class MessageMarkdownLinkSpan(theme: MarkwonTheme, destination: String, resolver: LinkResolver) :
    LinkSpan(theme, destination, resolver)

private class FilteredMarkdownLinkSpanFactory : SpanFactory {
    override fun getSpans(configuration: MarkwonConfiguration, props: RenderProps): Any? {
        val destination = CoreProps.LINK_DESTINATION.require(props)
        return if (MessageMarkdownPolicy.allowsLink(destination))
            MessageMarkdownLinkSpan(configuration.theme(), destination, configuration.linkResolver()) else null
    }
}

/** One renderer per HistoryAdapter; source text remains exclusively in the history model. */
internal class MessageMarkdown(context: Context) {
    private val markwon = Markwon.builderNoCore(context)
        .usePlugin(CorePlugin.create().hasExplicitMovementMethod(true))
        .usePlugin(SoftBreakAddsNewLinePlugin.create())
        .usePlugin(StrikethroughPlugin.create())
        .usePlugin(object : AbstractMarkwonPlugin() {
            override fun configureConfiguration(builder: MarkwonConfiguration.Builder) {
                // Span clicks (including accessibility clicks) cannot bypass selection actions.
                // Replaces LinkResolverDef, which accepts other schemes and logs destinations.
                builder.linkResolver(LinkResolver { _, _ -> })
            }

            override fun configureSpansFactory(builder: MarkwonSpansFactory.Builder) {
                builder.setFactory(Link::class.java, FilteredMarkdownLinkSpanFactory())
                // Core's default image visitor renders alt children when no factory exists.
                builder.setFactory(Image::class.java, null)
            }

            override fun configureVisitor(builder: MarkwonVisitor.Builder) {
                builder.on(HtmlInline::class.java) { visitor, html -> visitor.builder().append(html.literal) }
                builder.on(HtmlBlock::class.java) { visitor, html ->
                    visitor.blockStart(html)
                    visitor.builder().append(html.literal)
                    visitor.blockEnd(html)
                }
            }
        })
        .build()

    /** Parse once, reject excessive AST depth before recursive render, preserve full raw fallback. */
    fun render(source: String): Spanned {
        val rendered = try {
            val root = markwon.parse(source)
            if (MessageMarkdownPolicy.withinDepthLimit(root)) markwon.render(root) else null
        } catch (_: StackOverflowError) {
            null
        }
        return if (rendered == null || (rendered.isEmpty() && source.isNotEmpty()))
            SpannableStringBuilder(source) else rendered
    }

    fun setMarkdown(view: TextView, source: String) {
        view.autoLinkMask = 0
        // A displayed link label is not its destination; no classifier-generated URL actions.
        view.setTextClassifier(TextClassifier.NO_OP)
        view.setTextIsSelectable(true)
        // Preserve Markwon's ordered-list measurement/TextViewSpan hooks without reparsing.
        markwon.setParsedMarkdown(view, render(source))
    }

    companion object {
        /** Current nonempty selection must lie wholly inside exactly one of our link spans. */
        fun selectedLink(view: TextView): String? {
            val text = view.text as? Spanned ?: return null
            val start = minOf(view.selectionStart, view.selectionEnd)
            val end = maxOf(view.selectionStart, view.selectionEnd)
            if (start < 0 || start == end || end > text.length) return null
            val span = text.getSpans(start, end, MessageMarkdownLinkSpan::class.java)
                .filter { text.getSpanStart(it) < end && start < text.getSpanEnd(it) }
                .singleOrNull() ?: return null
            if (start < text.getSpanStart(span) || end > text.getSpanEnd(span)) return null
            return span.link.takeIf(MessageMarkdownPolicy::allowsLink)
        }

        /** Called only by the explicit selection action after rereading its span/destination. */
        fun openLink(context: Context, destination: String): Boolean {
            if (!MessageMarkdownPolicy.allowsLink(destination)) return false
            return try {
                // Android intent filters match schemes case-sensitively; the source stays untouched.
                val uri = Uri.parse(destination).normalizeScheme()
                context.startActivity(Intent(Intent.ACTION_VIEW, uri).addCategory(Intent.CATEGORY_BROWSABLE))
                true
            } catch (_: ActivityNotFoundException) {
                false
            } catch (_: SecurityException) {
                false
            }
        }
    }
}

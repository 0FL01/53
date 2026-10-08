package org.dmsg.client

import org.commonmark.node.BlockQuote
import org.commonmark.node.Document
import org.commonmark.node.Node
import org.commonmark.node.Text
import org.junit.Assert.*
import org.junit.Test

class MessageMarkdownTest {
    @Test fun onlyAbsoluteHttpAndHttpsWithAParsedHostAreAllowed() {
        for (link in listOf("https://example.test/path?q=1#part", "http://example.test", "HTTPS://EXAMPLE.TEST",
            "https://example.test:8443/a%20b", "https://127.0.0.1/", "http://[::1]/")) {
            assertTrue(link, MessageMarkdownPolicy.allowsLink(link))
        }
        for (link in listOf("", "example.test", "/path", "//example.test/path", "#part", "https:example.test",
            "https:///path", "https://", "https://bad host/", "https://example.test/%zz", "https://example.test:bad/",
            " https://example.test/", "https://example.test/\n", "https://example.test\\evil/",
            "javascript:alert(1)", "data:text/html,test", "file:///tmp/test", "content://example.test/path",
            "intent://example.test/", "mailto:user@example.test", "ftp://example.test/")) {
            assertFalse(link, MessageMarkdownPolicy.allowsLink(link))
        }
    }

    private fun chain(depth: Int): Node {
        val root = Document()
        var parent: Node = root
        repeat(depth - 1) { parent = BlockQuote().also { parent.appendChild(it) } }
        return root
    }

    @Test fun depth64IsAcceptedAnd65IsRejectedWithoutRecursiveVisitors() {
        assertTrue(MessageMarkdownPolicy.withinDepthLimit(chain(1)))
        assertTrue(MessageMarkdownPolicy.withinDepthLimit(chain(MessageMarkdownPolicy.MAX_AST_DEPTH)))
        assertFalse(MessageMarkdownPolicy.withinDepthLimit(chain(MessageMarkdownPolicy.MAX_AST_DEPTH + 1)))
        assertFalse(MessageMarkdownPolicy.withinDepthLimit(chain(4096)))
    }

    @Test fun depthWalkChecksLaterSiblingsAndDoesNotConfuseWidthWithDepth() {
        val wide = Document().apply { repeat(4096) { appendChild(Text("text")) } }
        assertTrue(MessageMarkdownPolicy.withinDepthLimit(wide))
        val mixed = Document().apply {
            appendChild(Text("first"))
            appendChild(chain(MessageMarkdownPolicy.MAX_AST_DEPTH))
        }
        assertFalse(MessageMarkdownPolicy.withinDepthLimit(mixed))
        val allowed = Document().apply {
            appendChild(chain(MessageMarkdownPolicy.MAX_AST_DEPTH - 1))
            appendChild(Text("last"))
        }
        assertTrue(MessageMarkdownPolicy.withinDepthLimit(allowed))
    }
}

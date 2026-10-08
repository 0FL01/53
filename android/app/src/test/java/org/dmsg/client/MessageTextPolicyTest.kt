package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class MessageTextPolicyTest {
    @Test fun asciiCyrillicAndFourByteScalarsShareTheSameBoundary() {
        for (scalar in listOf("a", "Я", "😀")) {
            assertEquals(1, MessageTextPolicy.count(scalar))
            assertTrue(MessageTextPolicy.isValid(scalar))
            val source = scalar.repeat(4000)
            assertEquals(4000, MessageTextPolicy.count(source))
            assertTrue(MessageTextPolicy.isValid(source))
            assertEquals(4001, MessageTextPolicy.count(source + scalar))
            assertFalse(MessageTextPolicy.isValid(source + scalar))
        }
    }

    @Test fun combiningMarksAndZwjAreScalarsRatherThanGraphemeClusters() {
        assertEquals(2, MessageTextPolicy.count("e\u0301"))
        val combining = "e\u0301".repeat(2000)
        assertTrue(MessageTextPolicy.isValid(combining))
        assertFalse(MessageTextPolicy.isValid(combining + " "))
        assertEquals(3, MessageTextPolicy.count("👩‍💻"))
        val zwj = "👩‍💻".repeat(1333) + " "
        assertEquals(4000, MessageTextPolicy.count(zwj))
        assertTrue(MessageTextPolicy.isValid(zwj))
        assertFalse(MessageTextPolicy.isValid(zwj + "\n"))
        assertEquals(1, MessageTextPolicy.count("é"))
    }

    @Test fun rawWhitespaceAndMarkdownMarkersCountWithoutTrimmingOrNormalization() {
        val source = " \n**Ж** e\u0301 👩‍💻\n "
        assertEquals(16, MessageTextPolicy.count(source))
        assertTrue(MessageTextPolicy.isValid(source))
        assertEquals(0, MessageTextPolicy.count(""))
        assertFalse(MessageTextPolicy.isValid(""))
        assertEquals(3, MessageTextPolicy.count(" \n\t"))
        assertTrue(MessageTextPolicy.isValid(" \n\t"))
    }

    @Test fun onlyPairedSurrogatesHaveScalarValues() {
        assertEquals(5, MessageTextPolicy.count("\uD7FF\uD800\uDC00\uDBFF\uDFFF\uE000\uFFFF"))
        for (source in listOf("\uD800", "\uDC00", "\uD800a", "a\uDC00", "\uDC00\uD800",
            "\uD800\uD800\uDC00", "\uD800\uDC00\uDC00", "a".repeat(4000) + "\uD800")) {
            assertNull(MessageTextPolicy.count(source))
            assertFalse(MessageTextPolicy.isValid(source))
        }
    }
}

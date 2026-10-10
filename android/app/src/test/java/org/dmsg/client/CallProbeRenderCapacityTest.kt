package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class CallProbeRenderCapacityTest {
    private data class Write(val offset: Int, val requested: Int, val accepted: Int)

    // Fixed head and scripted nonblocking writes isolate capacity/PCM ownership.
    // This models neither native source deadlines nor physical tail expiry.
    private class PartialWriteFixture(initialDepth: Long = 480) {
        private val pcm = ShortArray(CallProbeAudio.BATCH_SAMPLES)
        private val head = 0L
        var submitted = initialDepth
            private set
        var valid = 0
            private set
        var offset = 0
            private set
        var pullCalls = 0
            private set
        private var pulledSamples = 0
        val writes = mutableListOf<Write>()
        val acceptedSamples = mutableListOf<Short>()
        val pending get() = valid - offset
        val depth get() = submitted - head

        fun step(accepted: Int, pullSamples: Int = pcm.size): Boolean {
            assertTrue(depth in 0..CallProbeAudio.QUEUE_BOUND_SAMPLES.toLong())
            assertTrue(offset in 0..valid && valid <= pcm.size)
            if (!CallProbeAudio.renderHasCapacity(depth, pending)) return false
            if (valid == offset) {
                valid = pullSamples; offset = 0
                assertTrue(valid in 0..pcm.size)
                pullCalls++
                for (index in 0 until valid) pcm[index] = (pulledSamples + index).toShort()
                pulledSamples += valid
                if (valid == 0) return false
            }
            assertTrue(accepted in 0..pending)
            writes += Write(offset, pending, accepted)
            for (index in offset until offset + accepted) acceptedSamples += pcm[index]
            offset += accepted; submitted += accepted
            assertTrue("no write may exceed the unchanged sink bound",
                depth in 0..CallProbeAudio.QUEUE_BOUND_SAMPLES.toLong())
            assertEquals("pulled PCM is accepted exactly once or remains pending",
                pulledSamples, acceptedSamples.size + pending)
            return true
        }
    }

    @Test fun twoEightySampleWritesFinishAt640WithoutHeadProgressOrAnotherPull() {
        val fixture = PartialWriteFixture()
        assertTrue(fixture.step(80))
        assertEquals(560L, fixture.submitted)
        assertEquals(80, fixture.pending)
        assertEquals(80, fixture.offset)
        assertEquals(1, fixture.pullCalls)

        assertTrue("the second 80-sample write fits at depth 560 without waiting",
            fixture.step(80))
        assertEquals(640L, fixture.depth)
        assertEquals(0, fixture.pending)
        assertEquals(160, fixture.offset)
        assertEquals(1, fixture.pullCalls)
        assertEquals(listOf(Write(0, 160, 80), Write(80, 80, 80)), fixture.writes)
        assertEquals((0 until 160).map(Int::toShort), fixture.acceptedSamples)
        assertFalse(fixture.step(0))
        assertEquals(1, fixture.pullCalls)
        assertEquals(2, fixture.writes.size)
    }

    @Test fun fortyRemainingSamplesFitAtDepth600WithoutAnotherPull() {
        val fixture = PartialWriteFixture()
        assertTrue(fixture.step(120))
        assertEquals(600L, fixture.depth)
        assertEquals(40, fixture.pending)
        assertTrue("only the remaining 40 samples need capacity", fixture.step(40))
        assertEquals(640L, fixture.depth)
        assertEquals(0, fixture.pending)
        assertEquals(1, fixture.pullCalls)
        assertEquals(listOf(Write(0, 160, 120), Write(120, 40, 40)), fixture.writes)
        assertEquals((0 until 160).map(Int::toShort), fixture.acceptedSamples)
        assertFalse(CallProbeAudio.renderHasCapacity(601, 40))
        assertFalse(CallProbeAudio.renderHasCapacity(561, 80))
    }

    @Test fun zeroWritesKeepTheSamePendingPcmAndOffset() {
        val fixture = PartialWriteFixture()
        assertTrue(fixture.step(0))
        assertEquals(480L, fixture.depth)
        assertEquals(160, fixture.pending)
        assertEquals(0, fixture.offset)
        assertTrue(fixture.step(80))
        repeat(2) {
            assertTrue("a zero write retains the existing 80-sample remainder", fixture.step(0))
            assertEquals(560L, fixture.depth)
            assertEquals(80, fixture.pending)
            assertEquals(80, fixture.offset)
            assertEquals(1, fixture.pullCalls)
            assertEquals((0 until 80).map(Int::toShort), fixture.acceptedSamples)
        }
        assertTrue(fixture.step(80))
        assertEquals(640L, fixture.depth)
        assertEquals(0, fixture.pending)
        assertEquals(1, fixture.pullCalls)
        assertEquals(listOf(Write(0, 160, 0), Write(0, 160, 80), Write(80, 80, 0),
            Write(80, 80, 0), Write(80, 80, 80)), fixture.writes)
        assertEquals((0 until 160).map(Int::toShort), fixture.acceptedSamples)
    }

    @Test fun partialOffsetsConservePcmIncludingAShortNativePull() {
        val fixture = PartialWriteFixture(160)
        for (accepted in listOf(13, 0, 47, 60, 0, 40)) {
            assertTrue(fixture.step(accepted))
            assertEquals(1, fixture.pullCalls)
        }
        assertEquals(160, fixture.offset)
        assertEquals(0, fixture.pending)
        assertEquals(320L, fixture.depth)
        assertEquals((0 until 160).map(Int::toShort), fixture.acceptedSamples)

        assertTrue(fixture.step(40, pullSamples = 100))
        assertEquals(100, fixture.valid)
        assertEquals(40, fixture.offset)
        assertEquals(60, fixture.pending)
        assertEquals(2, fixture.pullCalls)
        assertTrue(fixture.step(60))
        assertEquals(100, fixture.offset)
        assertEquals(0, fixture.pending)
        assertEquals(420L, fixture.depth)
        assertEquals(2, fixture.pullCalls)
        assertEquals(listOf(Write(0, 160, 13), Write(13, 147, 0), Write(13, 147, 47),
            Write(60, 100, 60), Write(120, 40, 0), Write(120, 40, 40),
            Write(0, 100, 40), Write(40, 60, 60)), fixture.writes)
        assertEquals((0 until 260).map(Int::toShort), fixture.acceptedSamples)
    }

    @Test fun freshPullStillRequires160SamplesOfCapacity() {
        for (depth in 0L..640L) {
            val fixture = PartialWriteFixture(depth)
            if (depth <= 480) {
                assertTrue("fresh pull must fit at depth $depth", fixture.step(160))
                assertEquals(depth + 160, fixture.depth)
                assertEquals(1, fixture.pullCalls)
                assertEquals(0, fixture.pending)
            } else {
                assertFalse("fresh pull must be blocked at depth $depth", fixture.step(0))
                assertEquals(depth, fixture.depth)
                assertEquals(0, fixture.pullCalls)
                assertTrue(fixture.writes.isEmpty())
                assertEquals(0, fixture.valid)
                assertEquals(0, fixture.offset)
            }
        }
    }
}

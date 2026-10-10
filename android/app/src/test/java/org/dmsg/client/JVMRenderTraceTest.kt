package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class JVMRenderTraceTest {
    private fun expiry(discarded: Long = 160) = CallProbeAudio.RenderExpiry("queued", -250L,
        9_579L, 36_056L, 321L, 3, 640, 480, discarded, 40_000, 481, 13, 0)

    private fun observe(recorder: CallProbeAudio.RenderLoopRecorder, counter: Long, at: Long,
        native: CallProbeAudio.RenderExpiry? = expiry(), start: Long = at - 30, cachedCounter: Long = counter) {
        val delta = recorder.counterIncrease(counter)
        if (delta > 0) recorder.freezeExpiry(counter, delta, cachedCounter, native, at, start, at - 20)
    }

    private fun record(recorder: CallProbeAudio.RenderLoopRecorder, start: Long, end: Long,
        depth: Long = 480, request: Int = 160, pulled: Int = 160, validBefore: Int = 0,
        offsetBefore: Int = 0, validAfter: Int = 160, offsetAfter: Int = 80,
        writeRequested: Int = 160, accepted: Int = 80,
        reason: CallProbeAudio.RenderParkReason = CallProbeAudio.RenderParkReason.NONE,
        stats: Long = 11, clock: Long = 12, head: Long = 13, sink: Long = 14,
        timestamp: Long = 15, pull: Long = 16, write: Long = 17, park: Long = -1,
        completed: Boolean = true, counterRead: Long = 10) {
        recorder.record(start, end, completed, counterRead, stats, clock, head, sink, timestamp, pull, write,
            park, 1_234, depth, request, pulled, validBefore, offsetBefore, validAfter, offsetAfter,
            writeRequested, accepted, reason)
    }

    private fun assertConservation(cycle: CallProbeAudio.RenderCycle) {
        assertTrue(cycle.observed)
        assertEquals(cycle.endVsObservationNs!! - cycle.startVsObservationNs!!, cycle.durationNs)
        val stages = listOf(cycle.expiryCounterReadNs, cycle.statsReadParseNs, cycle.clockWorkNs, cycle.headReadNs, cycle.sinkQueuedNs,
            cycle.timestampWorkNs, cycle.pullNs, cycle.writeNs, cycle.parkNs).filter { it >= 0 }
        assertTrue(cycle.otherWorkNs >= 0)
        assertEquals(cycle.durationNs, stages.sum() + cycle.otherWorkNs)
    }

    @Test fun firstKnownAndUnchangedCounterInventNoEventEvenWhenAlreadyNonzero() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, -1, 0, native = null)
        observe(recorder, 95, 10, native = null)
        repeat(100) {
            record(recorder, 100L + it * 200, 250L + it * 200)
            observe(recorder, 95, 300L + it * 200)
        }
        assertNull(recorder.latest)
        observe(recorder, 255, 30_000)
        assertEquals(160L, recorder.latest!!.counterDelta)
        assertFalse(recorder.latest!!.ambiguousMultipleExpiries)
    }

    @Test fun unknownFreshJniMinusOneAndCachedCounterChangesCannotTriggerOrChangeBaseline() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, -1, 0, cachedCounter = 160)
        assertNull(recorder.latest)
        observe(recorder, 95, 100, cachedCounter = 255) // First fresh sample only initializes.
        observe(recorder, -1, 200, cachedCounter = 255)
        observe(recorder, 95, 300, cachedCounter = 415)
        assertNull(recorder.latest)
        observe(recorder, 255, 400, cachedCounter = 95)
        val first = recorder.latest!!
        assertEquals(160L, first.counterDelta)
        assertNull(first.nativeExpiry)
        assertFalse(first.cachedDetailMatch)
        assertTrue(first.ambiguousMultipleExpiries)
        // A later cache catch-up cannot replace/enrich the frozen observation.
        observe(recorder, 255, 500, cachedCounter = 255)
        observe(recorder, -1, 600, cachedCounter = 415)
        assertSame(first, recorder.latest)
        observe(recorder, 415, 700, cachedCounter = 415)
        assertEquals(160L, recorder.latest!!.counterDelta)
        assertTrue(recorder.latest!!.cachedDetailMatch)
        assertFalse(recorder.latest!!.ambiguousMultipleExpiries)
    }

    @Test fun cachedExpiryDetailRequiresExactFreshCounterMatchAndAnExistingTypedObject() {
        for (cachedCounter in listOf(-1L, 0L, 95L, 254L, 256L, 255L)) {
            val recorder = CallProbeAudio.RenderLoopRecorder()
            observe(recorder, 95, 0)
            val native = expiry()
            observe(recorder, 255, 1_000, native, cachedCounter = cachedCounter)
            val trace = recorder.latest!!
            assertEquals(255L, trace.expiredRenderSamples)
            assertEquals(160L, trace.counterDelta)
            assertEquals(cachedCounter, trace.cachedCounter)
            assertEquals(cachedCounter == 255L, trace.cachedDetailMatch)
            assertEquals(cachedCounter != 255L, trace.ambiguousMultipleExpiries)
            if (cachedCounter == 255L) assertSame(native, trace.nativeExpiry) else assertNull(trace.nativeExpiry)
        }
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 95, 0)
        observe(recorder, 255, 1_000, native = null, cachedCounter = 255)
        assertNull(recorder.latest!!.nativeExpiry)
        assertFalse(recorder.latest!!.cachedDetailMatch)
        assertTrue(recorder.latest!!.ambiguousMultipleExpiries)
    }

    @Test fun observationFreezesExactlyTwoPrecedingCyclesInChronologicalOrder() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, -10_000)
        record(recorder, -9_000, -8_800)
        record(recorder, -8_700, -8_500)
        record(recorder, -8_000, -7_800)
        observe(recorder, 160, -7_000, start = -7_030)
        val trace = recorder.latest!!
        assertEquals(-1_700L, trace.cycleMinus2.startVsObservationNs)
        assertEquals(-1_500L, trace.cycleMinus2.endVsObservationNs)
        assertEquals(-1_000L, trace.cycleMinus1.startVsObservationNs)
        assertEquals(-800L, trace.cycleMinus1.endVsObservationNs)
        assertEquals(-30L, trace.observationIterationStartVsObservationNs)
        assertEquals(-20L, trace.observationCounterReadStartVsObservationNs)
        assertEquals(20L, trace.observationCounterReadDurationNs)
        assertEquals(0L, trace.observationCounterReadStartVsObservationNs + trace.observationCounterReadDurationNs)
        assertEquals(770L, trace.observationPrecedingLoopGapNs)
        assertConservation(trace.cycleMinus2)
        assertConservation(trace.cycleMinus1)
        // Finishing the current observation cycle cannot retroactively replace either frozen cycle.
        record(recorder, -7_030, -6_000)
        assertSame(trace, recorder.latest)
        assertEquals(-1_000L, trace.cycleMinus1.startVsObservationNs)
    }

    @Test fun incompleteCurrentCycleCannotDisplaceEitherPriorCompletedCycle() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, 0)
        record(recorder, 100, 300)
        record(recorder, 400, 600)
        record(recorder, 700, 750, completed = false, stats = -1, clock = -1,
            head = -1, sink = -1, timestamp = -1, pull = -1, write = -1)
        observe(recorder, 160, 900, start = 850)
        val trace = recorder.latest!!
        assertEquals(-800L, trace.cycleMinus2.startVsObservationNs)
        assertEquals(-300L, trace.cycleMinus1.endVsObservationNs)
        assertTrue(trace.cycleMinus2.completed && trace.cycleMinus1.completed)
        assertEquals(250L, trace.observationPrecedingLoopGapNs)
        assertConservation(trace.cycleMinus2)
        assertConservation(trace.cycleMinus1)
    }

    @Test fun signedRelativeOffsetsAreTranslationInvariantAndDurationsConserveAllWork() {
        fun trace(epoch: Long): CallProbeAudio.RenderLoopTrace {
            val recorder = CallProbeAudio.RenderLoopRecorder()
            observe(recorder, 0, epoch)
            record(recorder, epoch + 1_000, epoch + 1_200)
            record(recorder, epoch + 1_500, epoch + 4_000, park = 2_000,
                reason = CallProbeAudio.RenderParkReason.ZERO_WRITE, accepted = 0, offsetAfter = 0)
            observe(recorder, 160, epoch + 5_000)
            return recorder.latest!!
        }
        val negativeEpoch = trace(-1_000_000_000L)
        assertEquals(negativeEpoch, trace(9_000_000_000L))
        assertConservation(negativeEpoch.cycleMinus2)
        assertConservation(negativeEpoch.cycleMinus1)
        assertEquals(10L, negativeEpoch.cycleMinus1.expiryCounterReadNs)
        assertEquals(11L, negativeEpoch.cycleMinus1.statsReadParseNs)
        assertEquals(12L, negativeEpoch.cycleMinus1.clockWorkNs)
        assertEquals(13L, negativeEpoch.cycleMinus1.headReadNs)
        assertEquals(14L, negativeEpoch.cycleMinus1.sinkQueuedNs)
        assertEquals(15L, negativeEpoch.cycleMinus1.timestampWorkNs)
        assertEquals(16L, negativeEpoch.cycleMinus1.pullNs)
        assertEquals(17L, negativeEpoch.cycleMinus1.writeNs)
        assertEquals(2_000L, negativeEpoch.cycleMinus1.parkNs)
    }

    @Test fun unknownActionsRemainDistinctFromAnEmptyPullAndAnAcceptedZeroWrite() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, 0)
        record(recorder, 100, 400, depth = 640, request = -1, pulled = -1,
            validAfter = 0, offsetAfter = 0, writeRequested = -1, accepted = -1,
            timestamp = -1, pull = -1, write = -1, park = 200,
            reason = CallProbeAudio.RenderParkReason.CAPACITY)
        record(recorder, 500, 800, depth = 639, request = 1, pulled = 0,
            validAfter = 0, offsetAfter = 0, writeRequested = -1, accepted = -1,
            timestamp = -1, write = -1, park = 200, reason = CallProbeAudio.RenderParkReason.EMPTY_PULL)
        observe(recorder, 160, 900)
        val capacity = recorder.latest!!.cycleMinus2
        val empty = recorder.latest!!.cycleMinus1
        assertEquals(-1, capacity.freshRequest)
        assertEquals(-1, capacity.pullValid)
        assertEquals(-1L, capacity.pullNs)
        assertEquals(-1, capacity.writeAccepted)
        assertEquals(1, empty.freshRequest)
        assertEquals(0, empty.pullValid)
        assertEquals(0, empty.pendingAfter)
        assertEquals(-1, empty.writeRequested)
        assertEquals(-1L, empty.writeNs)
        assertConservation(capacity)
        assertConservation(empty)

        record(recorder, 1_000, 1_400, request = 159, pulled = 159, depth = 481,
            validAfter = 159, offsetAfter = 0, writeRequested = 159, accepted = 0,
            park = 200, reason = CallProbeAudio.RenderParkReason.ZERO_WRITE)
        record(recorder, 1_500, 1_700, request = -1, pulled = -1, pull = -1,
            depth = 481, validBefore = 159, validAfter = 159, offsetAfter = 80,
            writeRequested = 159, accepted = 80)
        observe(recorder, 320, 1_800)
        val zero = recorder.latest!!.cycleMinus2
        val partial = recorder.latest!!.cycleMinus1
        assertEquals(0, zero.writeAccepted)
        assertEquals(159, zero.pendingAfter)
        assertEquals(CallProbeAudio.RenderParkReason.ZERO_WRITE, zero.parkReason)
        assertEquals(-1, partial.freshRequest)
        assertEquals(-1, partial.pullValid)
        assertEquals(159, partial.validBefore)
        assertEquals(159, partial.pendingBefore)
        assertEquals(80, partial.writeAccepted)
        assertEquals(79, partial.pendingAfter)
        assertEquals(CallProbeAudio.RenderParkReason.NONE, partial.parkReason)
    }

    @Test fun precedingLoopGapAndMeasuredParkingRemainSeparateObservations() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, 0)
        record(recorder, 1_000, 3_000, park = 1_500, reason = CallProbeAudio.RenderParkReason.ZERO_WRITE)
        record(recorder, 20_000, 35_000, park = 14_000, reason = CallProbeAudio.RenderParkReason.CAPACITY)
        observe(recorder, 160, 50_000, start = 49_000)
        val trace = recorder.latest!!
        assertEquals(-1L, trace.cycleMinus2.precedingLoopGapNs)
        assertEquals(17_000L, trace.cycleMinus1.precedingLoopGapNs)
        assertEquals(14_000L, trace.cycleMinus1.parkNs)
        assertEquals(14_000L, trace.observationPrecedingLoopGapNs)
        assertEquals(CallProbeAudio.RenderParkReason.CAPACITY, trace.cycleMinus1.parkReason)
        assertConservation(trace.cycleMinus1)
    }

    @Test fun firstExpiryWithInsufficientHistoryDoesNotFabricateCyclesOrActions() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, 0)
        observe(recorder, 160, 1_000)
        val empty = recorder.latest!!.cycleMinus1
        assertEquals(recorder.latest!!.cycleMinus2, empty)
        assertFalse(empty.observed)
        assertFalse(empty.completed)
        assertNull(empty.startVsObservationNs)
        assertNull(empty.endVsObservationNs)
        assertEquals(-1L, empty.durationNs)
        assertEquals(-1L, empty.expiryCounterReadNs)
        assertEquals(-1L, empty.head)
        assertEquals(-1, empty.validBefore)
        assertEquals(-1, empty.pendingAfter)
        assertEquals(CallProbeAudio.RenderParkReason.UNOBSERVED, empty.parkReason)
        record(recorder, 2_000, 2_200)
        observe(recorder, 320, 3_000)
        assertFalse(recorder.latest!!.cycleMinus2.observed)
        assertTrue(recorder.latest!!.cycleMinus1.observed)
    }

    @Test fun multiExpiryDeltaIsExplicitlyAmbiguousAndAbsentNativeRecordIsNotInvented() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, 0)
        observe(recorder, 255, 1_000)
        assertEquals(255L, recorder.latest!!.counterDelta)
        assertEquals(160L, recorder.latest!!.nativeExpiry!!.discardedSamples)
        assertTrue(recorder.latest!!.ambiguousMultipleExpiries)
        observe(recorder, 350, 2_000, native = null)
        assertEquals(95L, recorder.latest!!.counterDelta)
        assertNull(recorder.latest!!.nativeExpiry)
        assertTrue(recorder.latest!!.ambiguousMultipleExpiries)
        observe(recorder, 510, 3_000)
        assertFalse(recorder.latest!!.ambiguousMultipleExpiries)
    }

    private fun snapshot(trace: CallProbeAudio.RenderLoopTrace?) = CallProbeAudio.Snapshot(
        phase = "running", failure = null, stopReason = null, dns = false, cleanupComplete = false,
        ownerAlive = true, captureAlive = true, renderAlive = true, microphoneActive = true,
        recordOpen = true, trackOpen = true, effectsOpen = true, activeHandle = 1,
        capturedSamples = 0, submittedSamples = 0, hardwareFrames = 0, queueSamples = 0, maxQueueSamples = 0,
        rejectedCaptureBatches = 0, capturePeak = 0, captureClipped = 0, captureZeroSamples = 0,
        renderPeak = 0, renderClipped = 0, recordMinBytes = 0, trackMinBytes = 0, recordBufferFrames = 0,
        trackBufferFrames = 0, trackCapacityFrames = 0, startThresholdFrames = -1,
        inputRouteId = 0, inputRouteType = 0, outputRouteId = 0, outputRouteType = 0,
        micMuted = false, silenced = false, silencingObservable = false, focusGranted = true, focusAbandoned = false,
        previousMode = 0, communicationMode = 0, restoredMode = 0,
        aec = CallProbeAudio.Effect(), ns = CallProbeAudio.Effect(), playbackRate = 16_000,
        requestedRate = 16_000, underruns = 0, captureClock = CallProbeAudio.ClockPair(),
        playbackClock = CallProbeAudio.ClockPair(), playbackHeadClock = CallProbeAudio.ClockPair(),
        nativeStats = "{}", stopResult = null, playbackCompensation = "{}", lastRenderLoopTrace = trace)

    @Test fun publishedSnapshotAndItsNativeCopyStayFrozenAcrossLoopsAndReplacement() {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        observe(recorder, 0, 0)
        record(recorder, 100, 300)
        record(recorder, 400, 600)
        val native = expiry().copy(firstPullVsStartUs = null, codecDurationUs = null)
        observe(recorder, 160, 700, native)
        val first = recorder.latest!!
        val frozen = snapshot(first)
        val firstCycle = first.cycleMinus1
        repeat(1_000) {
            record(recorder, 1_000L + it * 500, 1_200L + it * 500, accepted = 0)
            observe(recorder, 160, 1_300L + it * 500)
            assertSame(first, recorder.latest)
        }
        observe(recorder, -1, 600_000, native = null)
        assertSame(first, recorder.latest)
        // A counter reset is a new baseline, not a positive event.
        observe(recorder, 0, 600_100)
        assertSame(first, recorder.latest)
        observe(recorder, 160, 600_200)
        val replacement = recorder.latest!!
        assertNotSame(first, replacement)
        assertSame(first, frozen.lastRenderLoopTrace)
        assertSame(firstCycle, frozen.lastRenderLoopTrace!!.cycleMinus1)
        assertSame(native, frozen.lastRenderLoopTrace!!.nativeExpiry)
        assertNull(frozen.lastRenderLoopTrace!!.nativeExpiry!!.firstPullVsStartUs)
        assertEquals(100L, -firstCycle.endVsObservationNs!!)
        // Ready/running/failure snapshots carry the same immutable public field.
        assertSame(first, frozen.copy(phase = "ready").lastRenderLoopTrace)
        assertSame(first, frozen.copy(phase = "failed", failure = "local_gate_failed").lastRenderLoopTrace)
        assertEquals(160L, replacement.counterDelta)
        assertEquals(-99_500L, replacement.cycleMinus1.endVsObservationNs)
    }

    private inner class MediaFixture(private val instrumented: Boolean, var depth: Long) {
        val recorder = CallProbeAudio.RenderLoopRecorder()
        var valid = 0; var offset = 0
        var supplied = 0
        val acceptedPcm = mutableListOf<Int>()
        val operations = mutableListOf<String>()
        private var base = 0
        private var time = 0L
        val pending get() = valid - offset

        init { observe(recorder, 0, 0) }

        fun step(returned: Int, accepted: Int, advance: Int = 0,
            freshCounter: Long = 0, cachedCounter: Long = freshCounter) {
            if (instrumented) observe(recorder, freshCounter, time + 30, cachedCounter = cachedCounter)
            require(advance.toLong() in 0..depth)
            depth -= advance
            val beforeValid = valid; val beforeOffset = offset; val beforeDepth = depth
            var request = -1; var pulled = -1; var writeRequest = -1; var written = -1
            var reason = CallProbeAudio.RenderParkReason.NONE
            val hasCapacity = if (instrumented) CallProbeAudio.renderHasCapacity(depth, pending)
                else if (pending > 0) depth + pending <= 640 else depth < 640
            if (!hasCapacity) {
                operations += "park:capacity"
                reason = CallProbeAudio.RenderParkReason.CAPACITY
            } else {
                if (valid == offset) {
                    request = if (instrumented) CallProbeAudio.freshPullLimit(depth) else minOf(160, (640 - depth).toInt())
                    require(returned in 0..request)
                    operations += "pull:$request:$returned"
                    base = supplied; supplied += returned
                    valid = returned; offset = 0; pulled = valid
                }
                if (valid == 0) {
                    operations += "park:emptyPull"
                    reason = CallProbeAudio.RenderParkReason.EMPTY_PULL
                } else {
                    writeRequest = pending
                    require(accepted in 0..pending)
                    operations += "write:$offset:$pending:$accepted"
                    for (index in offset until offset + accepted) acceptedPcm += base + index
                    offset += accepted; depth += accepted; written = accepted
                    if (accepted == 0) {
                        operations += "park:zeroWrite"
                        reason = CallProbeAudio.RenderParkReason.ZERO_WRITE
                    }
                }
            }
            if (instrumented) {
                record(recorder, time, time + 400, depth = beforeDepth, request = request, pulled = pulled,
                    validBefore = beforeValid, offsetBefore = beforeOffset, validAfter = valid, offsetAfter = offset,
                    writeRequested = writeRequest, accepted = written,
                    pull = if (pulled == -1) -1 else 16, write = if (written == -1) -1 else 17,
                    park = if (reason == CallProbeAudio.RenderParkReason.NONE) -1 else 200, reason = reason)
            }
            time += 500
            assertEquals(supplied, acceptedPcm.size + pending)
            assertTrue(depth in 0..640L)
        }
    }

    @Test fun instrumentationPreservesFreshCapacityPartialOwnershipAndEveryMediaOperationDecision() {
        for (initialDepth in 0L..640L) {
            val original = MediaFixture(false, initialDepth)
            val traced = MediaFixture(true, initialDepth)
            val fresh = minOf(160, (640 - initialDepth).toInt())
            // Empty native prefix, then zero/partial/remainder writes, then capacity parking and a 1-sample prefix.
            for (fixture in listOf(original, traced)) {
                fixture.step(0, 0, freshCounter = -1, cachedCounter = 255)
                fixture.step(fresh, 0, freshCounter = 0, cachedCounter = 255)
                fixture.step(fresh, fresh / 2, freshCounter = 160, cachedCounter = 0)
                fixture.step(0, 0, freshCounter = 160, cachedCounter = 160)
                fixture.step(0, fresh - fresh / 2, freshCounter = -1, cachedCounter = 320)
                fixture.step(0, 0, freshCounter = 160, cachedCounter = 320)
                fixture.step(1, 1, advance = 1, freshCounter = 320, cachedCounter = 320)
            }
            assertEquals("operations at depth $initialDepth", original.operations, traced.operations)
            assertEquals(original.depth, traced.depth)
            assertEquals(original.valid, traced.valid)
            assertEquals(original.offset, traced.offset)
            assertEquals(original.acceptedPcm, traced.acceptedPcm)
            observe(traced.recorder, 480, 4_000)
            val trace = traced.recorder.latest!!
            assertEquals(0, trace.cycleMinus1.pendingAfter)
            assertEquals(1, trace.cycleMinus1.writeAccepted)
            assertConservation(trace.cycleMinus1)
        }
    }
}

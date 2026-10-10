package org.dmsg.client

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import java.util.IdentityHashMap
import java.util.concurrent.atomic.AtomicReference

class JvmRenderMetadataCacheTest {
    private data class Fields(val ticks: Long = 0, val ns: Long = 0, val valid: Boolean = false,
        val expired: Long = -1)

    private val empty = "{}"
    private val valid = """{"remote_clock_ticks":480012,"remote_clock_ns":10000000000,"remote_clock_valid":true,"expired_render_samples":160}"""
    private val invalid = """{"remote_clock_ticks":480012,"remote_clock_ns":10000000000,"remote_clock_valid":false,"expired_render_samples":0}"""
    private val optDefaults = """{"remote_clock_ticks":"unknown","remote_clock_ns":null,"remote_clock_valid":{},"expired_render_samples":0}"""
    private val definitions = mapOf(empty to Fields(), valid to Fields(480_012, 10_000_000_000, true, 160),
        invalid to Fields(480_012, 10_000_000_000, false, 0), optDefaults to Fields(expired = 0))

    private fun newReference(raw: String) = String(raw.toCharArray())

    /** Android's JVM JSONObject is a throwing stub. Opaque tokens test the real cache with a
     * deterministic fixture parser; no JSONObject method or Android parser runs in these tests.
     */
    private class Parser(private val definitions: Map<String, Fields>) {
        private val unsafeClass = Class.forName("sun.misc.Unsafe")
        private val unsafe = unsafeClass.getDeclaredField("theUnsafe").let {
            it.isAccessible = true; it.get(null)
        }
        private val allocate = unsafeClass.getMethod("allocateInstance", Class::class.java)
        private val values = IdentityHashMap<JSONObject, Fields>()
        var calls = 0
            private set

        fun parse(raw: String): JSONObject {
            calls++
            val fields = definitions[raw] ?: throw IllegalArgumentException("malformed fixture metadata")
            val token = allocate.invoke(unsafe, JSONObject::class.java) as JSONObject
            values[token] = fields
            return token
        }

        fun fields(token: JSONObject) = values.getValue(token)
    }

    // Exact legacy diagnostic fields, with a pure JVM encoder injected at the serialization seam.
    private fun encode(command: CallProbePlaybackClock.Output) =
        """{"hardware_calibrated":${command.calibrated},"healthy":${command.healthy},"selected_rate":${command.rate},"sink_ppb":${command.sinkPpb},"hardware_rejection_mask":${command.rejectionMask},"relative_ppm":${command.relativePpm}}"""

    @Test fun twoHundredReadsOfTheSameReferenceParseAndSerializeOnceInsteadOfTwoHundredTimes() {
        val oldParser = Parser(definitions)
        val parser = Parser(definitions)
        val cache = CallProbeAudio.RenderMetadataCache(parser::parse)
        val raw = AtomicReference(newReference(valid))
        val published = AtomicReference("{}")
        val command = CallProbePlaybackClock.Output(16_000, 0, true, true, 25.0, 0)
        var oldSerialized = 0; var serialized = 0
        val diagnostics = CallProbeAudio.RenderCompensationCache { serialized++; encode(it) }
        var reads = 0
        var first: JSONObject? = null
        repeat(200) {
            val source = raw.get(); reads++ // Same atomic read on every iteration.
            val old = oldParser.parse(source)
            val parsed = cache.read(source)
            if (it == 0) first = parsed else assertSame(first, parsed)
            assertEquals(oldParser.fields(old), parser.fields(parsed))
            diagnostics.changedJson(command.copy())?.let { json -> published.set(json) }
            oldSerialized++
            assertEquals(encode(command), published.get())
        }
        assertEquals(200, reads)
        assertEquals(200, oldParser.calls)
        assertEquals(1, parser.calls)
        assertEquals(200, oldSerialized)
        assertEquals(1, serialized)
    }

    @Test fun equalContentNewReferenceAndReturningToAnOldReferenceBothRequireParsing() {
        val parser = Parser(definitions)
        val cache = CallProbeAudio.RenderMetadataCache(parser::parse)
        val firstRaw = newReference(empty)
        val equalRaw = newReference(empty)
        assertEquals(firstRaw, equalRaw)
        assertNotSame(firstRaw, equalRaw)
        val first = cache.read(firstRaw)
        assertSame(first, cache.read(firstRaw))
        assertEquals(Fields(), parser.fields(first)) // Initial {} retains the opt defaults.
        val second = cache.read(equalRaw)
        assertNotSame(first, second)
        assertSame(second, cache.read(equalRaw))
        val returned = cache.read(firstRaw)
        assertNotSame(first, returned) // One slot, not an identity-keyed history.
        assertEquals(3, parser.calls)
        val changed = cache.read(newReference(valid))
        assertEquals(definitions.getValue(valid), parser.fields(changed))
        assertEquals(4, parser.calls)
    }

    private fun expectMalformed(action: () -> Unit) {
        try { action(); fail("malformed replacement must throw") }
        catch (e: IllegalArgumentException) { assertEquals("malformed fixture metadata", e.message) }
    }

    @Test fun malformedReplacementNeverPublishesCacheStateOrFallsBackToOldMetadata() {
        val parser = Parser(definitions)
        val cache = CallProbeAudio.RenderMetadataCache(parser::parse)
        val goodRaw = newReference(valid)
        val good = cache.read(goodRaw)
        val bad = newReference("{")
        repeat(2) { expectMalformed { cache.read(bad) } }
        assertEquals(3, parser.calls) // Even the same malformed reference retries parsing, never a hit.
        assertSame(good, cache.read(goodRaw))
        assertEquals(3, parser.calls) // Neither field was replaced by either failed parse.

        val old = RenderModel(false, 480)
        val optimized = RenderModel(true, 480)
        for (model in listOf(old, optimized)) {
            model.begin(goodRaw, EPOCH, 0, 0)
            model.media(EPOCH, 160, 80)
            val operationsBefore = model.operations.toList()
            val chosenBefore = model.commands.size
            expectMalformed { model.begin(bad, EPOCH + 2_000_000, 0, 160) }
            assertEquals(chosenBefore, model.commands.size)
            assertEquals(operationsBefore + listOf("freshCounter:160", "atomicRead"), model.operations)
        }
        assertEquals(old.operations, optimized.operations)
        assertEquals(old.diagnostics, optimized.diagnostics)
        assertEquals(old.pending, optimized.pending)
    }

    @Test fun separateRenderersNeverShareTheParsedSlotEvenForTheSameRawReference() {
        val parser = Parser(definitions)
        val first = CallProbeAudio.RenderMetadataCache(parser::parse)
        val second = CallProbeAudio.RenderMetadataCache(parser::parse)
        val raw = newReference(valid)
        val a = first.read(raw)
        val b = second.read(raw)
        assertNotSame(a, b)
        assertEquals(2, parser.calls)
        assertSame(a, first.read(raw))
        assertSame(b, second.read(raw))
        first.read(newReference(empty))
        assertSame(b, second.read(raw))
        assertEquals(3, parser.calls)
    }

    @Test fun everyExactOutputFieldIncludingNullsSignedZeroAndOneDoubleUlpUpdatesPublication() {
        val base = CallProbePlaybackClock.Output(16_000, 0, true, true, 0.0, 0)
        val changes = listOf(base.copy(rate = 16_001), base.copy(sinkPpb = 1), base.copy(sinkPpb = null),
            base.copy(calibrated = false), base.copy(healthy = false), base.copy(rejectionMask = 1),
            base.copy(rejectionMask = 2), base.copy(relativePpm = null), base.copy(relativePpm = -0.0),
            base.copy(relativePpm = Math.nextUp(0.0)))
        for (changed in changes) {
            assertNotEquals(base, changed)
            var calls = 0
            val cache = CallProbeAudio.RenderCompensationCache { calls++; encode(it) }
            val published = AtomicReference("{}")
            published.set(cache.changedJson(base)!!)
            val frozenSnapshotValue = published.get()
            assertNull(cache.changedJson(base.copy()))
            published.set(cache.changedJson(changed)!!)
            assertEquals(encode(changed), published.get())
            assertEquals(encode(base), frozenSnapshotValue)
            assertNull(cache.changedJson(changed.copy()))
            published.set(cache.changedJson(base)!!)
            assertEquals(encode(base), published.get())
            assertEquals(3, calls)
        }
    }

    @Test fun failedDiagnosticSerializationDoesNotSuppressTheNextAttempt() {
        val base = CallProbePlaybackClock.Output(16_000, null, false, false, null, 0)
        var calls = 0
        val cache = CallProbeAudio.RenderCompensationCache {
            calls++
            if (it.rejectionMask != 0L) throw IllegalArgumentException("malformed fixture metadata")
            encode(it)
        }
        assertEquals(encode(base), cache.changedJson(base))
        repeat(2) { expectMalformed { cache.changedJson(base.copy(rejectionMask = 1)) } }
        assertEquals(3, calls)
        assertNull(cache.changedJson(base.copy()))
    }

    private fun calibrated() = CallProbePlaybackClock().also {
        it.observe(0, 1, 1, 16_000, 0)
        it.observe(160_000, 10_000_000_001, 10_000_000_001, 16_000, 0)
        it.observe(320_000, EPOCH, EPOCH, 16_000, 0)
    }

    private inner class RenderModel(private val optimized: Boolean, initialDepth: Long) {
        val parser = Parser(definitions)
        private val metadata = AtomicReference(empty)
        private val metadataCache = CallProbeAudio.RenderMetadataCache(parser::parse)
        private val diagnosticCache = CallProbeAudio.RenderCompensationCache { serialized++; encode(it) }
        private val published = AtomicReference("{}")
        val clock = calibrated()
        private val counter = CallProbeAudio.RenderLoopRecorder()
        val operations = mutableListOf<String>()
        val commands = mutableListOf<CallProbePlaybackClock.Output>()
        val diagnostics = mutableListOf<String>()
        var reads = 0; var freshReads = 0; var serialized = 0
        private var actualRate = 16_000
        private var head = 320_000L
        private var submitted = head + initialDepth
        private var nextTimestamp = 0L
        private var valid = 0; private var offset = 0
        private var sourceBase = 0
        var supplied = 0
            private set
        val acceptedPcm = mutableListOf<Int>()
        val pending get() = valid - offset
        val depth get() = submitted - head

        fun begin(raw: String, now: Long, manual: Int, freshCounter: Long): CallProbePlaybackClock.Output {
            metadata.set(raw)
            operations += "freshCounter:$freshCounter"; freshReads++
            val delta = counter.counterIncrease(freshCounter)
            operations += "atomicRead"; reads++
            val observedRaw = metadata.get()
            val source = if (optimized) metadataCache.read(observedRaw) else parser.parse(observedRaw)
            val fields = parser.fields(source)
            if (delta > 0) operations += "expiry:$freshCounter:$delta:cached:${fields.expired}"
            operations += "choose:$now:${fields.ticks}:${fields.ns}:${fields.valid}:$manual"
            val command = clock.choose(now, fields.ticks, fields.ns, fields.valid, manual)
            commands += command
            if (optimized) diagnosticCache.changedJson(command)?.let { published.set(it) }
            else { serialized++; published.set(encode(command)) }
            diagnostics += published.get()
            operations += "requestedRate:${command.rate}"
            command.sinkPpb?.let { operations += "sinkRate:$it" }
            if (actualRate != command.rate) {
                operations += "setPlaybackRate:${command.rate}"
                actualRate = command.rate
                operations += "readback:$actualRate:resetClockPairs"
            }
            return command
        }

        fun media(now: Long, returned: Int, accepted: Int, advance: Int = 0) {
            require(advance.toLong() in 0..depth)
            head += advance
            operations += "head:$head:depth:$depth:sinkQueued:$depth"
            require(depth in 0..640L)
            if (now >= nextTimestamp) {
                operations += "timestamp:$now"
                nextTimestamp = now + 100_000_000
            }
            val capacity = if (optimized) CallProbeAudio.renderHasCapacity(depth, pending)
                else if (pending > 0) depth + pending <= 640 else depth < 640
            if (!capacity) { operations += "park:capacity:2000000"; return }
            if (valid == offset) {
                val requested = if (optimized) CallProbeAudio.freshPullLimit(depth)
                    else minOf(160L, 640 - depth).toInt()
                require(returned in 0..requested)
                operations += "pull:$requested:$returned"
                sourceBase = supplied; supplied += returned
                valid = returned; offset = 0
                if (valid == 0) { operations += "park:emptyPull:2000000"; return }
            }
            require(accepted in 0..pending)
            operations += "write:$offset:$pending:$accepted"
            for (index in offset until offset + accepted) acceptedPcm += sourceBase + index
            offset += accepted; submitted += accepted
            operations += "afterWriteDepth:$depth"
            if (accepted == 0) operations += "park:zeroWrite:2000000"
            assertEquals(supplied, acceptedPcm.size + pending)
            assertTrue(depth in 0..640L)
        }
    }

    @Test fun unchangedMetadataStillChoosesEveryCycleAcrossFreshnessManualOverrideRejectionAndDither() {
        val old = RenderModel(false, 640)
        val optimized = RenderModel(true, 640)
        var raw = newReference(empty)
        var frames = 320_000.0
        var actualRate = 16_000
        repeat(2_000) { step ->
            if (step > 0) frames += actualRate / 500.0
            val now = EPOCH + step * 2_000_000L
            raw = when (step) {
                1, 200 -> valid
                50 -> newReference(valid)
                100 -> invalid
                150 -> optDefaults
                else -> raw
            }
            val manual = if (step in 250 until 300) 8 else 0
            val fresh = when { step == 0 -> -1L; step < 100 -> 0L; else -> 160L }
            val before = old.begin(raw, now, manual, fresh)
            val after = optimized.begin(raw, now, manual, fresh)
            assertEquals("command at step $step", before, after)
            assertEquals(old.diagnostics.last(), optimized.diagnostics.last())
            actualRate = before.rate
            for (model in listOf(old, optimized)) {
                if (step == 1_500) model.clock.observe(-1, -1, now, actualRate, 0)
                else if (step % 50 == 0 && step !in 500 until 850)
                    model.clock.observe(kotlin.math.floor(frames).toLong(), now, now, actualRate, 0)
            }
        }
        assertEquals(old.operations, optimized.operations)
        assertEquals(old.commands, optimized.commands)
        assertEquals(old.diagnostics, optimized.diagnostics)
        assertEquals(2_000, optimized.reads)
        assertEquals(2_000, optimized.freshReads)
        assertEquals(2_000, optimized.commands.size)
        assertEquals(2_000, old.parser.calls)
        assertEquals(6, optimized.parser.calls)
        assertEquals(2_000, old.serialized)
        val changes = 1 + optimized.commands.zipWithNext().count { (before, after) -> before != after }
        assertEquals(changes, optimized.serialized)
        assertTrue(optimized.serialized < old.serialized)
        assertTrue(optimized.commands.any { it.rate == 16_000 })
        assertTrue(optimized.commands.any { it.rate == 16_001 })
        assertTrue(optimized.commands.any { it.rate == 16_008 })
        assertNull(optimized.commands.first().relativePpm)
        assertTrue(optimized.commands[450].healthy)
        assertFalse(optimized.commands[800].healthy) // Same raw reference, stale hardware timestamp.
        assertTrue(optimized.commands[851].healthy) // Same raw reference, fresh hardware observation.
        assertEquals(1L, optimized.commands[1_501].rejectionMask)
        assertFalse(optimized.commands[1_501].healthy)
    }

    @Test fun cachedAndUncachedModelsPreserveHeadTimeCapacityEveryRequestPartialWriteAndCounterDecision() {
        for (initialDepth in 0L..640L) {
            val old = RenderModel(false, initialDepth)
            val optimized = RenderModel(true, initialDepth)
            val prefix = minOf(160L, 640 - initialDepth).toInt()
            val sameRaw = newReference(valid)
            val raws = listOf(sameRaw, sameRaw, newReference(valid), invalid, invalid, optDefaults, sameRaw)
            val fresh = listOf(-1L, 0L, 160L, 160L, -1L, 160L, 320L)
            val returns = listOf(0, prefix, prefix, 0, 0, 0, 1)
            val accepts = listOf(0, 0, prefix / 2, 0, prefix - prefix / 2, 0, 1)
            for (step in raws.indices) {
                val now = EPOCH + step * 2_000_000L
                val manual = if (step in 2..3) -8 else 0
                for (model in listOf(old, optimized)) {
                    model.begin(raws[step], now, manual, fresh[step])
                    model.media(now, returns[step], accepts[step], if (step == 6) 1 else 0)
                }
                assertEquals("operations at depth=$initialDepth step=$step", old.operations, optimized.operations)
                assertEquals(old.diagnostics, optimized.diagnostics)
                assertEquals(old.commands, optimized.commands)
                assertEquals(old.pending, optimized.pending)
                assertEquals(old.depth, optimized.depth)
                assertEquals(old.acceptedPcm, optimized.acceptedPcm)
            }
            assertEquals(7, old.parser.calls)
            assertEquals(5, optimized.parser.calls)
            assertEquals(7, optimized.reads)
            assertEquals(7, optimized.freshReads)
            assertEquals(old.supplied, optimized.supplied)
            assertEquals(0, optimized.pending)
        }
    }

    private companion object {
        const val EPOCH = 20_000_000_001L
    }
}

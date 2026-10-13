package org.dmsg.client
import org.dmsg.client.CallAudio as CallProbeAudio
import org.dmsg.client.CallPlaybackClock as CallProbePlaybackClock

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

    private fun transportFixture(): MutableMap<String, Any?> {
        val capture = linkedMapOf<String, Any?>("schema_version" to 1L, "connection_state" to 0xffff_ffffL,
            "multipath" to false, "path_count" to 0xffff_ffffL, "captured_path_count" to 8L)
        for (key in listOf("now_native_us", "packets_received", "data_sent", "data_received",
            "latest_receive_age_us", "latest_progress_age_us")) capture[key] = Long.MAX_VALUE
        capture["paths"] = MutableList(8) { slot ->
            val path = linkedMapOf<String, Any?>("slot" to slot.toLong(), "validated" to false, "demoted" to false,
                "pending_count" to 64L, "pending_truncated" to false, "tail_ack_eliciting" to false)
            for (key in listOf("smoothed_rtt_us", "base_retransmit_timer_us", "consecutive_pto", "bytes_in_transit",
                "cwin", "pacing_bytes_per_second", "send_mtu", "oldest_pending_age_us", "tail_pending_age_us",
                "highest_ack_age_us", "last_packet_receive_age_us")) path[key] = Long.MAX_VALUE
            path
        }
        return linkedMapOf("age_since_observed_ms" to Long.MAX_VALUE, "publication_age_us" to null,
            "native_capture" to capture)
    }

    @Suppress("UNCHECKED_CAST")
    private fun transportCapture(input: Map<String, Any?>) = input.getValue("native_capture") as MutableMap<String, Any?>

    @Suppress("UNCHECKED_CAST")
    private fun transportPaths(input: Map<String, Any?>) = transportCapture(input).getValue("paths") as MutableList<Any?>

    @Suppress("UNCHECKED_CAST")
    private fun transportPath(input: Map<String, Any?>) = transportPaths(input)[0] as MutableMap<String, Any?>

    // Only this diagnostic's numeric/bool/null tree; no Android JSONObject stub or new dependency.
    private fun encodeTransport(value: Any?): String = when (value) {
        null -> "null"
        is Long, is Boolean -> value.toString()
        is Map<*, *> -> value.entries.joinToString(",", "{", "}") { "\"${it.key}\":${encodeTransport(it.value)}" }
        is List<*> -> value.joinToString(",", "[", "]") { encodeTransport(it) }
        else -> throw AssertionError("unexpected transport diagnostic type")
    }

    @Test fun nativeTransportIsAnAtomicAllowlistWithExactCountersAndUnknownFreshness() {
        val input = transportFixture()
        input["identity"] = "not forwarded"
        transportCapture(input)["address"] = "not forwarded"
        transportPath(input)["error"] = "not forwarded"
        transportCapture(input)["captured_path_count"] = 1L
        transportCapture(input)["latest_progress_age_us"] = null
        transportPath(input)["oldest_pending_age_us"] = null
        transportPath(input)["tail_ack_eliciting"] = null
        for (slot in 1..7) transportPaths(input)[slot] = null
        val clean = requireNotNull(CallProbeAudio.NativeTransportEvidence.sanitize(input))
        assertEquals(setOf("age_since_observed_ms", "publication_age_us", "native_capture"), clean.keys)
        assertEquals(Long.MAX_VALUE, clean["age_since_observed_ms"])
        assertNull(clean["publication_age_us"])
        val capture = clean.getValue("native_capture") as Map<*, *>
        assertEquals(Long.MAX_VALUE, capture["now_native_us"])
        assertEquals(0xffff_ffffL, capture["connection_state"])
        assertFalse(capture.containsKey("address"))
        assertTrue(capture.containsKey("latest_progress_age_us")); assertNull(capture["latest_progress_age_us"])
        val paths = capture["paths"] as List<*>
        assertEquals(8, paths.size)
        assertTrue(paths.drop(1).all { it == null })
        val path = paths[0] as Map<*, *>
        assertEquals(64L, path["pending_count"])
        assertNull(path["tail_ack_eliciting"])
        assertFalse(path.containsKey("error"))
        // The sanitized copy cannot be changed by a later native/fixture publication.
        transportPath(input)["pending_count"] = 0L
        assertEquals(64L, path["pending_count"])
    }

    @Test fun invalidOptionalNativeTransportYieldsNullIncludingRoundedUnsignedOverflow() {
        assertNull(CallProbeAudio.NativeTransportEvidence.sanitize(null))
        val mutations: List<(MutableMap<String, Any?>) -> Unit> = listOf(
            { it["age_since_observed_ms"] = "123" },
            { it["age_since_observed_ms"] = -1L },
            { it["age_since_observed_ms"] = Long.MAX_VALUE.toDouble() }, // Parsed 2^63 is already lossy.
            { it["age_since_observed_ms"] = java.math.BigInteger("18446744073709551615").toDouble() },
            { it["publication_age_us"] = 0L },
            { it.remove("publication_age_us") },
            { transportCapture(it)["schema_version"] = 2L },
            { transportCapture(it)["connection_state"] = 0x1_0000_0000L },
            { transportCapture(it)["captured_path_count"] = 9L },
            { transportCapture(it)["multipath"] = "true" },
            { transportCapture(it).remove("latest_receive_age_us") },
            { transportPaths(it).add(null) },
            { transportPath(it)["slot"] = 1L },
            { transportPath(it)["pending_count"] = 65L },
            { transportPath(it)["smoothed_rtt_us"] = 1.5 },
            { transportPath(it)["validated"] = 1L },
            { transportPath(it)["tail_ack_eliciting"] = "false" },
            { transportPath(it)["tail_pending_age_us"] = -1L })
        for (mutate in mutations) {
            val input = transportFixture()
            mutate(input)
            assertNull(CallProbeAudio.NativeTransportEvidence.sanitize(input))
            assertNotNull(CallProbeAudio.NativeTransportEvidence.sanitize(transportFixture()))
        }
    }

    @Test fun maximumNativeTransportContributionFitsAnEightKiBStructuredStatusBudget() {
        val clean = requireNotNull(CallProbeAudio.NativeTransportEvidence.sanitize(transportFixture()))
        val serialized = encodeTransport(mapOf("native_transport" to clean))
        assertTrue(serialized.contains(Long.MAX_VALUE.toString()))
        assertEquals(8, (clean.getValue("native_capture") as Map<*, *>)["paths"].let { (it as List<*>).size })
        // All maximum accepted integer widths, eight fully populated paths, longest bool tokens.
        // This measures the added member, not the rest of client_call_state or Android's parser.
        assertTrue("native_transport bytes=${serialized.toByteArray(Charsets.UTF_8).size}",
            serialized.toByteArray(Charsets.UTF_8).size < 8 * 1024)
    }

    private fun deliveryFixture(): MutableMap<String, Any?> {
        val socket = linkedMapOf<String, Any?>("tx_base" to 200L, "rx_base" to 180L, "tx_socket_written" to 1220L, "tx_record_start" to 1100L,
            "tx_record_end" to 1220L, "tx_noise_end" to 1220L, "tx_records_committed" to 8L,
            "tx_records_written" to 8L, "rx_socket_read" to 1530L, "rx_record_start" to 1400L,
            "rx_record_end" to 1500L, "rx_authenticated_end" to 1500L, "rx_records_authenticated" to 9L,
            "tx_noise_srtcp_index" to 8L, "tx_record_srtcp_index" to 8L, "rx_noise_srtcp_index" to null)
        val native = linkedMapOf<String, Any?>("schema_version" to 2L, "publication_age_us" to null,
            "age_since_observed_ms" to 25L, "now_native_us" to 50L, "tx_submitted" to 1100L,
            "rx_contiguous" to 1600L, "rx_flushed" to 1540L, "rx_pending" to 60L,
            "rx_highest" to 1700L,
            "remote_stream_credit" to 65536L, "stream_active" to true, "send_queue" to false,
            "failed" to false, "local_fin" to false, "remote_fin" to false)
        return linkedMapOf("socket" to socket, "native" to native)
    }

    @Suppress("UNCHECKED_CAST")
    private fun deliveryPart(input: Map<String, Any?>, key: String) = input.getValue(key) as MutableMap<String, Any?>

    @Test fun controlDeliveryOnlyForwardsLosslessByteFrontiersAndUnknownPublicationAge() {
        val input = deliveryFixture()
        deliveryPart(input, "native")["port"] = 45678L
        deliveryPart(input, "native")["generation"] = 21L
        deliveryPart(input, "socket")["payload"] = "never forwarded"
        val clean = requireNotNull(CallProbeAudio.ControlDeliveryEvidence.sanitize(input))
        assertEquals(setOf("socket", "native"), clean.keys)
        val native = clean.getValue("native") as Map<*, *>
        assertFalse(native.containsKey("port")); assertFalse(native.containsKey("generation"))
        assertNull(native["publication_age_us"])
        assertEquals(1100L, native["tx_submitted"])
        assertEquals(1700L, native["rx_highest"])
        assertFalse((clean.getValue("socket") as Map<*, *>).containsKey("payload"))
        input["native"] = null
        assertNull(requireNotNull(CallProbeAudio.ControlDeliveryEvidence.sanitize(input))["native"])
        assertTrue(encodeTransport(clean).toByteArray(Charsets.UTF_8).size < 2 * 1024)
    }

    @Test fun malformedControlDeliveryIsOptionalNullNotACoercedCounter() {
        val mutations: List<(MutableMap<String, Any?>) -> Unit> = listOf(
            { deliveryPart(it, "socket")["tx_socket_written"] = Long.MAX_VALUE.toDouble() },
            { deliveryPart(it, "socket")["tx_record_end"] = 1221L },
            { deliveryPart(it, "socket")["tx_noise_end"] = 1219L },
            { deliveryPart(it, "socket")["tx_records_committed"] = 10L },
            { deliveryPart(it, "socket")["rx_authenticated_end"] = 1501L },
            { deliveryPart(it, "socket")["rx_socket_read"] = "1530" },
            { deliveryPart(it, "socket")["tx_noise_srtcp_index"] = 0x8000_0000L },
            { deliveryPart(it, "socket")["rx_base"] = 1401L },
            { deliveryPart(it, "native")["rx_pending"] = 59L },
            { deliveryPart(it, "native")["publication_age_us"] = 1L },
            { deliveryPart(it, "native")["schema_version"] = 1L },
            { deliveryPart(it, "native")["rx_highest"] = 1599L },
            { deliveryPart(it, "native")["stream_active"] = 1L },
            { deliveryPart(it, "native")["tx_submitted"] = -1L },
            { it.remove("native") })
        assertNull(CallProbeAudio.ControlDeliveryEvidence.sanitize(null))
        for (mutate in mutations) {
            val input = deliveryFixture(); mutate(input)
            assertNull(CallProbeAudio.ControlDeliveryEvidence.sanitize(input))
        }
    }

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

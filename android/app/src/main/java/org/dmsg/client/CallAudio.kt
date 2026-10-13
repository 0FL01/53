package org.dmsg.client

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.AudioEffect
import android.media.audiofx.NoiseSuppressor
import android.os.Build
import android.os.Process
import org.json.JSONObject
import java.io.RandomAccessFile
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.locks.LockSupport
import kotlin.math.abs
import kotlin.math.max

/** One foreground run, no core/store, no PCM persistence, no restart or handle reuse.
 * The owner handles setup/telemetry/teardown. Capture and render never encode, join,
 * or do network IO: push/pull must be nonblocking bounded native queue operations.
 */
class CallAudio internal constructor(context: Context, private val port: CallAudioPort,
    private val platform: CallAudioPlatform, private val outputMuted: Boolean = false,
    private val finished: (Snapshot) -> Unit = {}) {
    companion object {
        const val SAMPLE_RATE = 16_000
        const val BATCH_SAMPLES = 160
        const val QUEUE_BOUND_SAMPLES = 640 // 40 ms of submitted, not yet rendered PCM.
        internal fun freshPullLimit(depth: Long): Int =
            (QUEUE_BOUND_SAMPLES - depth).coerceIn(0L, BATCH_SAMPLES.toLong()).toInt()

        internal fun renderHasCapacity(depth: Long, pending: Int): Boolean {
            // Finish all owned PCM before requesting another bounded prefix.
            return if (pending > 0) depth + pending <= QUEUE_BOUND_SAMPLES
                else freshPullLimit(depth) > 0
        }

        private val nativeCounters = listOf("encoded_packets", "decoded_packets", "plc_slots",
            "tx_bytes", "rx_bytes", "dropped_capture", "dropped_render", "terminal_feedback",
            "late_packets", "max_unconfirmed_bytes", "max_feedback_cycle_ms", "sink_queue_samples",
            "tx_soft_deadline_packets", "stop_ms", "native_stop_ms", "source_route_gap_samples")
    }

    /** Optional, all-or-null copy of one native capture; never a media validation failure.
     * Observed age is only a lower bound: publication freshness and app/native clock mapping
     * remain unknown. Android's JSON parser rounds u64 values above signed Long into Double;
     * reject those rather than publishing a wrapped, clamped or rounded counter.
     */
    internal object NativeTransportEvidence {
        private val captureCounters = listOf("now_native_us", "packets_received", "data_sent", "data_received")
        private val captureAges = listOf("latest_receive_age_us", "latest_progress_age_us")
        private val pathCounters = listOf("smoothed_rtt_us", "base_retransmit_timer_us", "consecutive_pto",
            "bytes_in_transit", "cwin", "pacing_bytes_per_second", "send_mtu")
        private val pathAges = listOf("oldest_pending_age_us", "tail_pending_age_us", "highest_ack_age_us",
            "last_packet_receive_age_us")
        private val transportFields = listOf("age_since_observed_ms", "publication_age_us", "native_capture")
        private val captureFields = listOf("schema_version", "connection_state", "multipath", "path_count",
            "captured_path_count", "paths") + captureCounters + captureAges
        private val pathFields = listOf("slot", "validated", "demoted", "pending_count", "pending_truncated",
            "tail_ack_eliciting") + pathCounters + pathAges

        internal fun fields(source: JSONObject, names: List<String>): MutableMap<String, Any?> =
            names.associateWith { name -> source.get(name).let { if (it === JSONObject.NULL) null else it } }.toMutableMap()

        fun fromNativeStats(source: JSONObject): JSONObject? {
            return try {
                val transport = source.optJSONObject("native_transport") ?: return null
                val capture = transport.getJSONObject("native_capture")
                val paths = capture.getJSONArray("paths")
                require(paths.length() <= 8)
                val input = fields(transport, transportFields)
                val captured = fields(capture, captureFields)
                captured["paths"] = List(paths.length()) { index ->
                    if (paths.isNull(index)) null else fields(paths.getJSONObject(index), pathFields)
                }
                input["native_capture"] = captured
                sanitize(input)?.let { JSONObject(it) }
            } catch (_: Exception) { null }
        }

        internal fun number(source: Map<*, *>, field: String, max: Long = Long.MAX_VALUE,
            nullable: Boolean = false): Long? {
            require(source.containsKey(field))
            val value = source[field]
            if (value == null && nullable) return null
            // No coercion of strings, booleans, floating point or lossy unsigned JSON numbers.
            require(value is Byte || value is Short || value is Int || value is Long)
            return (value as Number).toLong().also { require(it in 0L..max) }
        }

        internal fun boolean(source: Map<*, *>, field: String, nullable: Boolean = false): Boolean? {
            require(source.containsKey(field))
            val value = source[field]
            if (value == null && nullable) return null
            require(value is Boolean)
            return value
        }

        /** Pure seam for schema/bounds checks; output has only allowlisted numbers, bools and nulls. */
        fun sanitize(source: Map<*, *>?): Map<String, Any?>? {
            return try {
                if (source == null) return null
                require(source.containsKey("publication_age_us") && source["publication_age_us"] == null)
                val capture = source["native_capture"] as? Map<*, *> ?: return null
                require(number(capture, "schema_version", 1) == 1L)
                val clean = linkedMapOf<String, Any?>("age_since_observed_ms" to number(source, "age_since_observed_ms"),
                    "publication_age_us" to null)
                val captured = linkedMapOf<String, Any?>("schema_version" to 1L,
                    "connection_state" to number(capture, "connection_state", 0xffff_ffffL),
                    "multipath" to boolean(capture, "multipath"),
                    "path_count" to number(capture, "path_count", 0xffff_ffffL),
                    "captured_path_count" to number(capture, "captured_path_count", 8))
                captureCounters.forEach { captured[it] = number(capture, it) }
                captureAges.forEach { captured[it] = number(capture, it, nullable = true) }
                val paths = capture["paths"] as? List<*> ?: return null
                require(paths.size <= 8)
                // Preserve null slots and their ordinals, including a padded eight-slot array.
                captured["paths"] = paths.mapIndexed { index, value ->
                    if (value == null) null else {
                        val path = value as? Map<*, *> ?: return null
                        require(number(path, "slot", 7) == index.toLong())
                        val copied = linkedMapOf<String, Any?>("slot" to index.toLong(),
                            "validated" to boolean(path, "validated"), "demoted" to boolean(path, "demoted"))
                        pathCounters.forEach { copied[it] = number(path, it) }
                        copied["pending_count"] = number(path, "pending_count", 64)
                        copied["pending_truncated"] = boolean(path, "pending_truncated")
                        pathAges.forEach { copied[it] = number(path, it, nullable = true) }
                        copied["tail_ack_eliciting"] = boolean(path, "tail_ack_eliciting", nullable = true)
                        copied
                    }
                }
                clean["native_capture"] = captured
                clean
            } catch (_: Exception) { null }
        }
    }

    /** Only the selected control lane's byte frontiers, not delivery or credit. */
    internal object ControlDeliveryEvidence {
        private val socketFields = listOf("tx_base", "rx_base", "tx_socket_written", "tx_record_start", "tx_record_end", "tx_noise_end",
            "tx_records_committed", "tx_records_written", "rx_socket_read", "rx_record_start", "rx_record_end",
            "rx_authenticated_end", "rx_records_authenticated")
        private val socketIndices = listOf("tx_noise_srtcp_index", "tx_record_srtcp_index", "rx_noise_srtcp_index")
        private val nativeCounters = listOf("age_since_observed_ms", "now_native_us", "tx_submitted",
            "rx_contiguous", "rx_flushed", "rx_pending", "remote_stream_credit")
        private val nativeFlags = listOf("stream_active", "send_queue", "failed", "local_fin", "remote_fin")
        fun fromNativeStats(source: JSONObject): JSONObject? { return try {
            val delivery = source.optJSONObject("control_delivery") ?: return null
            val socket = NativeTransportEvidence.fields(delivery.getJSONObject("socket"), socketFields + socketIndices)
            val native = if (delivery.isNull("native")) null else NativeTransportEvidence.fields(
                delivery.getJSONObject("native"), nativeCounters + nativeFlags + listOf("schema_version", "publication_age_us"))
            sanitize(mapOf("socket" to socket, "native" to native))?.let { JSONObject(it) }
        } catch (_: Exception) { null } }

        fun sanitize(source: Map<*, *>?): Map<String, Any?>? { return try {
            if (source == null) return null
            val socket = source["socket"] as? Map<*, *> ?: return null
            val copied = socketFields.associateWith { requireNotNull(NativeTransportEvidence.number(socket, it)) }
            require(copied.getValue("tx_base") <= copied.getValue("tx_record_start"))
            require(copied.getValue("rx_base") <= copied.getValue("rx_record_start"))
            require(copied.getValue("tx_record_start") <= copied.getValue("tx_record_end"))
            require(copied.getValue("tx_record_end") <= copied.getValue("tx_socket_written"))
            require(copied.getValue("tx_socket_written") <= copied.getValue("tx_noise_end"))
            require(copied.getValue("tx_records_written") <= copied.getValue("tx_records_committed"))
            require(copied.getValue("tx_records_committed") - copied.getValue("tx_records_written") <= 1)
            require(copied.getValue("rx_record_start") <= copied.getValue("rx_authenticated_end"))
            require(copied.getValue("rx_authenticated_end") <= copied.getValue("rx_record_end"))
            require(copied.getValue("rx_record_end") <= copied.getValue("rx_socket_read"))
            val cleanSocket = linkedMapOf<String, Any?>().apply { putAll(copied) }
            socketIndices.forEach { cleanSocket[it] = NativeTransportEvidence.number(socket, it, 0x7fff_ffffL, nullable = true) }
            require(source.containsKey("native"))
            val native = source["native"]?.let { value ->
                val capture = value as? Map<*, *> ?: return null
                require(NativeTransportEvidence.number(capture, "schema_version", 1) == 1L)
                require(capture.containsKey("publication_age_us") && capture["publication_age_us"] == null)
                val result = linkedMapOf<String, Any?>("schema_version" to 1L, "publication_age_us" to null)
                nativeCounters.forEach { result[it] = NativeTransportEvidence.number(capture, it) }
                nativeFlags.forEach { result[it] = NativeTransportEvidence.boolean(capture, it) }
                val contiguous = result.getValue("rx_contiguous") as Long
                val flushed = result.getValue("rx_flushed") as Long
                val pending = result.getValue("rx_pending") as Long
                require(flushed <= contiguous && pending == contiguous - flushed)
                result
            }
            linkedMapOf("socket" to cleanSocket, "native" to native)
        } catch (_: Exception) { null } }
    }

    /** One renderer's read-only parse, keyed by the immutable owner's string identity. */
    internal class RenderMetadataCache(private val parse: (String) -> JSONObject = ::JSONObject) {
        private var lastRaw: String? = null
        private var lastParsed: JSONObject? = null

        fun read(raw: String): JSONObject {
            if (raw === lastRaw) return lastParsed!!
            val parsed = parse(raw) // A malformed replacement throws before either cache field changes.
            lastParsed = parsed
            lastRaw = raw
            return parsed
        }
    }

    /** Only diagnostic serialization is skipped; the immutable output still drives every media call. */
    internal class RenderCompensationCache(private val encode: (CallPlaybackClock.Output) -> String = { command ->
        JSONObject().put("hardware_calibrated", command.calibrated)
            .put("healthy", command.healthy).put("selected_rate", command.rate)
            .put("sink_ppb", command.sinkPpb ?: JSONObject.NULL)
            .put("hardware_rejection_mask", command.rejectionMask)
            .put("last_rejected_hardware_window", command.rejectedWindow?.let { window ->
                JSONObject().put("frames", window.frames).put("ns", window.ns).put("rate", window.rate)
                    .put("before_frames", window.beforeFrames ?: JSONObject.NULL).put("before_ns", window.beforeNs ?: JSONObject.NULL)
                    .put("after_frames", window.afterFrames ?: JSONObject.NULL).put("after_ns", window.afterNs ?: JSONObject.NULL)
            } ?: JSONObject.NULL)
            .put("last_backward_hardware_sample", command.backwardSample?.let { sample ->
                JSONObject().put("before_frames", sample.beforeFrames).put("frames", sample.frames)
                    .put("timestamp_delta_ns", sample.timestampDeltaNs)
                    .put("before_head_frames", sample.beforeHead ?: JSONObject.NULL)
                    .put("head_frames", sample.head ?: JSONObject.NULL)
                    .put("rate", sample.rate).put("underruns", sample.underruns)
            } ?: JSONObject.NULL)
            .put("relative_ppm", command.relativePpm ?: JSONObject.NULL).toString()
    }) {
        private var lastCommand: CallPlaybackClock.Output? = null

        fun changedJson(command: CallPlaybackClock.Output): String? {
            if (command == lastCommand) return null
            val json = encode(command)
            lastCommand = command
            return json
        }
    }

    data class Clock(val frame: Long, val nanoTime: Long) {
        fun json() = JSONObject().put("frame", frame).put("monotonic_ns", nanoTime)
    }
    data class ClockPair(val first: Clock? = null, val latest: Clock? = null, val unavailable: Long = 0) {
        // Observed slope includes stalls; it is not a remote clock/skew estimator.
        fun json(): JSONObject {
            val a = first; val b = latest
            val slope = if (a != null && b != null && b.nanoTime > a.nanoTime && b.frame >= a.frame)
                (b.frame - a.frame) * 1e9 / (b.nanoTime - a.nanoTime) else null
            return JSONObject().put("first", a?.json() ?: JSONObject.NULL)
                .put("latest", b?.json() ?: JSONObject.NULL).put("unavailable_observations", unavailable)
                .put("observed_frames_per_second", slope ?: JSONObject.NULL)
        }
    }
    data class Effect(val available: Boolean = false, val created: Boolean = false,
        val enableResult: Int? = null, val enabled: Boolean = false, val control: Boolean = false) {
        fun json() = JSONObject().put("available", available).put("created", created)
            .put("enable_result", enableResult ?: JSONObject.NULL).put("enabled", enabled).put("control", control)
    }

    enum class RenderParkReason(val label: String) {
        UNOBSERVED("unobserved"), NONE("none"), CAPACITY("capacity"),
        EMPTY_PULL("emptyPull"), ZERO_WRITE("zeroWrite")
    }

    /** Immutable typed copy of the already sanitized native last-expiry object. */
    data class RenderExpiry(
        val kind: String, val publicationVsStartUs: Long?, val firstPullVsStartUs: Long?,
        val lastPullVsStartUs: Long?, val codecDurationUs: Long?, val successfulPullCalls: Long,
        val initialSamples: Long, val transferredSamples: Long, val discardedSamples: Long,
        val sourceFrameDurationUs: Long, val sinkQueueSamples: Long, val expiryVsEndUs: Long,
        val sinkRatePpb: Long
    ) {
        companion object {
            internal fun fromJson(source: JSONObject): RenderExpiry {
                fun optional(field: String) = if (source.isNull(field)) null else source.getLong(field)
                return RenderExpiry(source.getString("kind"), optional("publication_vs_start_us"),
                    optional("first_pull_vs_start_us"), optional("last_pull_vs_start_us"), optional("codec_duration_us"),
                    source.getLong("successful_pull_calls"), source.getLong("initial_samples"),
                    source.getLong("transferred_samples"), source.getLong("discarded_samples"),
                    source.getLong("source_frame_duration_us"), source.getLong("sink_queue_samples"),
                    source.getLong("expiry_vs_end_us"), source.getLong("sink_rate_ppb"))
            }
        }

        fun json() = JSONObject().put("kind", kind)
            .put("publication_vs_start_us", publicationVsStartUs ?: JSONObject.NULL)
            .put("first_pull_vs_start_us", firstPullVsStartUs ?: JSONObject.NULL)
            .put("last_pull_vs_start_us", lastPullVsStartUs ?: JSONObject.NULL)
            .put("codec_duration_us", codecDurationUs ?: JSONObject.NULL)
            .put("successful_pull_calls", successfulPullCalls).put("initial_samples", initialSamples)
            .put("transferred_samples", transferredSamples).put("discarded_samples", discardedSamples)
            .put("source_frame_duration_us", sourceFrameDurationUs).put("sink_queue_samples", sinkQueueSamples)
            .put("expiry_vs_end_us", expiryVsEndUs).put("sink_rate_ppb", sinkRatePpb)
    }

    /** No absolute Java monotonic timestamp survives publication. -1 means action not observed.
     * expiryCounterReadNs is the fresh JNI getter; statsReadParseNs is the cache read and conditional parse.
     */
    data class RenderCycle(
        val observed: Boolean, val completed: Boolean, val startVsObservationNs: Long?, val endVsObservationNs: Long?,
        val durationNs: Long, val precedingLoopGapNs: Long, val expiryCounterReadNs: Long,
        val statsReadParseNs: Long, val clockWorkNs: Long,
        val headReadNs: Long, val sinkQueuedNs: Long, val timestampWorkNs: Long, val pullNs: Long, val writeNs: Long,
        val parkNs: Long, val otherWorkNs: Long, val head: Long, val depth: Long, val freshRequest: Int,
        val pullValid: Int, val validBefore: Int, val offsetBefore: Int, val pendingBefore: Int,
        val validAfter: Int, val offsetAfter: Int, val pendingAfter: Int,
        val writeRequested: Int, val writeAccepted: Int, val parkReason: RenderParkReason
    ) {
        fun json(): JSONObject {
            fun us(value: Long): Any = if (value == -1L) JSONObject.NULL else value / 1000.0
            fun count(value: Long): Any = if (value == -1L) JSONObject.NULL else value
            return JSONObject().put("observed", observed).put("completed", completed)
                .put("start_vs_observation_us", startVsObservationNs?.let { it / 1000.0 } ?: JSONObject.NULL)
                .put("end_vs_observation_us", endVsObservationNs?.let { it / 1000.0 } ?: JSONObject.NULL)
                .put("duration_us", us(durationNs)).put("preceding_loop_gap_us", us(precedingLoopGapNs))
                .put("expiry_counter_read_us", us(expiryCounterReadNs))
                .put("stats_read_parse_us", us(statsReadParseNs)).put("clock_work_us", us(clockWorkNs))
                .put("head_read_us", us(headReadNs)).put("sink_queued_us", us(sinkQueuedNs))
                .put("timestamp_work_us", us(timestampWorkNs)).put("pull_us", us(pullNs)).put("write_us", us(writeNs))
                .put("park_us", us(parkNs)).put("other_work_us", us(otherWorkNs))
                .put("head_frames", count(head)).put("depth_samples", count(depth))
                .put("fresh_request_samples", count(freshRequest.toLong())).put("pull_valid_samples", count(pullValid.toLong()))
                .put("valid_before", count(validBefore.toLong())).put("offset_before", count(offsetBefore.toLong()))
                .put("pending_before", count(pendingBefore.toLong())).put("valid_after", count(validAfter.toLong()))
                .put("offset_after", count(offsetAfter.toLong())).put("pending_after", count(pendingAfter.toLong()))
                .put("write_requested_samples", count(writeRequested.toLong())).put("write_accepted_samples", count(writeAccepted.toLong()))
                .put("parking_reason", parkReason.label)
        }
    }

    /** Observation is local getter completion, not native expiry time. Cached detail is parsed afterwards. */
    data class RenderLoopTrace(
        val expiredRenderSamples: Long, val counterDelta: Long, val cachedCounter: Long, val nativeExpiry: RenderExpiry?,
        val observationCounterReadStartVsObservationNs: Long, val observationCounterReadDurationNs: Long,
        val observationIterationStartVsObservationNs: Long,
        val observationPrecedingLoopGapNs: Long, val cycleMinus2: RenderCycle, val cycleMinus1: RenderCycle
    ) {
        val cachedDetailMatch: Boolean get() = cachedCounter == expiredRenderSamples && nativeExpiry != null
        val ambiguousMultipleExpiries: Boolean get() = nativeExpiry == null || counterDelta != nativeExpiry.discardedSamples

        fun json() = JSONObject().put("association", "OBSERVATION_ASSOCIATION_ONLY")
            .put("time_basis", "local_monotonic_diagnostic")
            .put("timing_scope", "not_physical_arrival_or_acoustic")
            .put("stats_source", "fresh_native_expiry_counter_with_cached_detail")
            .put("cycle_scope", "two_completed_cycles_before_fresh_counter_observation_not_expiry_bracket")
            .put("expired_render_samples", expiredRenderSamples).put("counter_delta", counterDelta)
            .put("cached_counter", if (cachedCounter < 0) JSONObject.NULL else cachedCounter)
            .put("cached_detail_match", cachedDetailMatch)
            .put("ambiguous_multiple_expiries", ambiguousMultipleExpiries)
            .put("native_last_render_expiry", nativeExpiry?.json() ?: JSONObject.NULL)
            .put("observation_counter_read_start_vs_observation_us", observationCounterReadStartVsObservationNs / 1000.0)
            .put("observation_counter_read_duration_us", observationCounterReadDurationNs / 1000.0)
            .put("observation_iteration_start_vs_observation_us", observationIterationStartVsObservationNs / 1000.0)
            .put("observation_preceding_loop_gap_us",
                if (observationPrecedingLoopGapNs == -1L) JSONObject.NULL else observationPrecedingLoopGapNs / 1000.0)
            .put("cycle_minus_2", cycleMinus2.json()).put("cycle_minus_1", cycleMinus1.json())
    }

    /** Render-thread-only primitives; precisely two reusable records, no per-cycle allocation. */
    internal class RenderLoopRecorder {
        internal class RecentCycle {
            var observed = false; var completed = false
            var start = 0L; var end = 0L; var gap = -1L
            var counterRead = -1L; var stats = -1L; var clock = -1L; var headRead = -1L; var sink = -1L; var timestamp = -1L
            var pull = -1L; var write = -1L; var park = -1L
            var head = -1L; var depth = -1L
            var freshRequest = -1; var pullValid = -1
            var validBefore = -1; var offsetBefore = -1; var pendingBefore = -1
            var validAfter = -1; var offsetAfter = -1; var pendingAfter = -1
            var writeRequested = -1; var writeAccepted = -1
            var parkReason = RenderParkReason.UNOBSERVED.ordinal

            fun freeze(observation: Long): RenderCycle {
                val duration = if (observed) end - start else -1L
                val measured = max(0L, counterRead) + max(0L, stats) + max(0L, clock) + max(0L, headRead) + max(0L, sink) +
                    max(0L, timestamp) + max(0L, pull) + max(0L, write) + max(0L, park)
                return RenderCycle(observed, completed, if (observed) start - observation else null,
                    if (observed) end - observation else null, duration, gap, counterRead, stats, clock, headRead, sink,
                    timestamp, pull, write, park, if (observed) duration - measured else -1L, head, depth,
                    freshRequest, pullValid, validBefore, offsetBefore, pendingBefore, validAfter, offsetAfter,
                    pendingAfter, writeRequested, writeAccepted, RenderParkReason.entries[parkReason])
            }
        }

        private val first = RecentCycle()
        private val second = RecentCycle()
        // next is always the older slot; neither slot is touched until after observation.
        private var next = 0
        private var previousEnd = 0L
        private var hasPreviousEnd = false
        private var previousCounter = -1L
        @Volatile var latest: RenderLoopTrace? = null
            private set

        fun precedingGap(start: Long): Long = if (hasPreviousEnd) start - previousEnd else -1L

        /** Unknown observations leave the baseline alone; first known value invents no event. */
        fun counterIncrease(counter: Long): Long {
            if (counter < 0) return -1L
            val delta = if (previousCounter < 0) 0L else counter - previousCounter
            previousCounter = counter
            return delta
        }

        fun freezeExpiry(counter: Long, delta: Long, cachedCounter: Long, expiry: RenderExpiry?, observation: Long,
            iterationStart: Long, counterReadStart: Long) {
            if (counter < 0 || delta <= 0) return
            val older = if (next == 0) first else second
            val newer = if (next == 0) second else first
            val matchedExpiry = if (cachedCounter == counter) expiry else null
            latest = RenderLoopTrace(counter, delta, cachedCounter, matchedExpiry,
                counterReadStart - observation, observation - counterReadStart, iterationStart - observation,
                precedingGap(iterationStart), older.freeze(observation), newer.freeze(observation))
        }

        fun record(start: Long, end: Long, completed: Boolean, counterRead: Long, stats: Long, clock: Long, headRead: Long,
            sink: Long, timestamp: Long, pull: Long, write: Long, park: Long, head: Long, depth: Long,
            freshRequest: Int, pullValid: Int, validBefore: Int, offsetBefore: Int, validAfter: Int,
            offsetAfter: Int, writeRequested: Int, writeAccepted: Int, parkReason: RenderParkReason) {
            if (!completed) return
            val cycle = if (next == 0) first else second
            cycle.observed = true; cycle.completed = completed
            cycle.start = start; cycle.end = end; cycle.gap = precedingGap(start)
            cycle.counterRead = counterRead; cycle.stats = stats; cycle.clock = clock; cycle.headRead = headRead; cycle.sink = sink
            cycle.timestamp = timestamp; cycle.pull = pull; cycle.write = write; cycle.park = park
            cycle.head = head; cycle.depth = depth; cycle.freshRequest = freshRequest; cycle.pullValid = pullValid
            cycle.validBefore = validBefore; cycle.offsetBefore = offsetBefore; cycle.pendingBefore = validBefore - offsetBefore
            cycle.validAfter = validAfter; cycle.offsetAfter = offsetAfter; cycle.pendingAfter = validAfter - offsetAfter
            cycle.writeRequested = writeRequested; cycle.writeAccepted = writeAccepted; cycle.parkReason = parkReason.ordinal
            previousEnd = end; hasPreviousEnd = true; next = 1 - next
        }
    }

    /** Probe schedstat bracket includes read overhead; these deltas are not a write-only CPU budget. */
    data class SlowWriteDiagnostic(val writeWallNs: Long, val requestedSamples: Int, val acceptedSamples: Int,
        val cpuDeltaNs: Long?, val runnableDelayDeltaNs: Long?, val schedstatKnown: Boolean,
        val timeSlicesDelta: Long?, val schedstatReadOverheadNs: Long) {
        fun json() = JSONObject().put("write_wall_ns", writeWallNs).put("write_requested_samples", requestedSamples)
            .put("write_accepted_samples", acceptedSamples).put("cpu_delta_ns", cpuDeltaNs ?: JSONObject.NULL)
            .put("runnable_delay_delta_ns", runnableDelayDeltaNs ?: JSONObject.NULL).put("schedstat_known", schedstatKnown)
            .put("time_slices_delta", timeSlicesDelta ?: JSONObject.NULL).put("schedstat_read_overhead_ns", schedstatReadOverheadNs)
    }

    /** Render-thread-only reader/clock seams; one wiped byte buffer and no per-write sample objects. */
    internal class SlowWriteProbe(private val read: (ByteArray) -> Int, private val nanoTime: (() -> Long)? = null) {
        private val bytes = ByteArray(64)
        private val fields = LongArray(3); private val before = LongArray(3)
        private var beforeKnown = false; private var readOverheadNs = 0L
        @Volatile var latest: SlowWriteDiagnostic? = null
            private set
        private fun now(): Long = nanoTime?.invoke() ?: System.nanoTime() // No boxed clock result in the probe loop.

        private fun sample(): Boolean {
            fun space(value: Byte) = value == 32.toByte() || value == 9.toByte() || value == 10.toByte() || value == 13.toByte()
            try {
                val length = read(bytes)
                if (length !in 1 until bytes.size) return false // Includes full/truncated input.
                var cursor = 0
                for (field in 0..2) {
                    while (cursor < length && space(bytes[cursor])) cursor++
                    val start = cursor; var value = 0L
                    while (cursor < length && bytes[cursor].toInt() in 48..57) {
                        val digit = bytes[cursor].toInt() - 48
                        if (value > (Long.MAX_VALUE - digit) / 10) return false
                        value = value * 10 + digit; cursor++
                    }
                    if (cursor == start || (cursor < length && !space(bytes[cursor]))) return false
                    fields[field] = value
                }
                while (cursor < length && space(bytes[cursor])) cursor++
                return cursor == length
            } catch (_: Exception) { return false }
            finally { bytes.fill(0) }
        }

        fun beforeWrite() {
            val start = now()
            beforeKnown = sample(); fields.copyInto(before)
            readOverheadNs = now() - start
        }

        fun afterWrite(wallNs: Long, requested: Int, accepted: Int) {
            val start = now(); val afterKnown = sample()
            val overhead = readOverheadNs + (now() - start)
            if (wallNs < 40_000_000L) return
            val known = beforeKnown && afterKnown && fields[0] >= before[0] && fields[1] >= before[1] && fields[2] >= before[2]
            latest = SlowWriteDiagnostic(wallNs, requested, accepted, if (known) fields[0] - before[0] else null,
                if (known) fields[1] - before[1] else null, known, if (known) fields[2] - before[2] else null, overhead)
        }
    }

    data class Snapshot(
        val phase: String, val failure: String?, val stopReason: String?, val dns: Boolean,
        val cleanupComplete: Boolean, val ownerAlive: Boolean, val captureAlive: Boolean, val renderAlive: Boolean,
        val microphoneActive: Boolean, val recordOpen: Boolean, val trackOpen: Boolean, val effectsOpen: Boolean, val activeHandle: Long,
        val capturedSamples: Long, val submittedSamples: Long, val hardwareFrames: Long,
        val queueSamples: Long, val maxQueueSamples: Long, val rejectedCaptureBatches: Long,
        val capturePeak: Int, val captureClipped: Long, val captureZeroSamples: Long,
        val renderPeak: Int, val renderClipped: Long,
        val recordMinBytes: Int, val trackMinBytes: Int, val recordBufferFrames: Int,
        val trackBufferFrames: Int, val trackCapacityFrames: Int, val startThresholdFrames: Int,
        val inputRouteId: Int, val inputRouteType: Int, val outputRouteId: Int, val outputRouteType: Int,
        val micMuted: Boolean, val silenced: Boolean, val silencingObservable: Boolean,
        val focusGranted: Boolean, val focusAbandoned: Boolean, val previousMode: Int,
        val communicationMode: Int, val restoredMode: Int, val aec: Effect, val ns: Effect,
        val playbackRate: Int, val requestedRate: Int, val underruns: Int,
        val captureClock: ClockPair, val playbackClock: ClockPair, val playbackHeadClock: ClockPair,
        val nativeStats: String, val stopResult: String?, val playbackCompensation: String,
        val lastRenderLoopTrace: RenderLoopTrace? = null, val outputMuted: Boolean = false,
        val outputRouteRestored: Boolean = true, val trackPerformanceMode: Int = -1,
        val lastSlowWriteDiagnostic: SlowWriteDiagnostic? = null
    ) {
        fun json() = JSONObject().put("phase", phase).put("failure", failure ?: JSONObject.NULL)
            .put("stop_reason", stopReason ?: JSONObject.NULL).put("path", if (dns) "dns_fixture" else "local_memory_pair")
            .put("cleanup_complete", cleanupComplete).put("owner_alive", ownerAlive)
            .put("capture_alive", captureAlive).put("render_alive", renderAlive)
            .put("microphone_active", microphoneActive).put("record_open", recordOpen).put("track_open", trackOpen)
            .put("effects_open", effectsOpen)
            .put("native_handle_active", activeHandle != 0L)
            .put("captured_samples", capturedSamples).put("submitted_samples", submittedSamples)
            .put("hardware_frames", hardwareFrames).put("queued_samples", queueSamples)
            .put("max_queued_samples", maxQueueSamples).put("queue_bound_samples", QUEUE_BOUND_SAMPLES)
            .put("rejected_capture_batches", rejectedCaptureBatches)
            .put("capture_peak", capturePeak).put("capture_clipped", captureClipped).put("capture_zero_samples", captureZeroSamples)
            .put("render_peak", renderPeak).put("render_clipped", renderClipped)
            .put("fixture_output_muted", outputMuted)
            .put("fixture_output_route_restored", outputRouteRestored)
            .put("record_min_bytes", recordMinBytes).put("track_min_bytes", trackMinBytes)
            .put("record_buffer_frames", recordBufferFrames).put("track_buffer_frames", trackBufferFrames)
            .put("track_capacity_frames", trackCapacityFrames).put("start_threshold_frames", startThresholdFrames)
            .put("track_performance_mode", trackPerformanceMode)
            .put("input_route_id", inputRouteId).put("input_route_type", inputRouteType)
            .put("output_route_id", outputRouteId).put("output_route_type", outputRouteType)
            .put("mic_muted", micMuted).put("system_silenced", silenced).put("silencing_observable", silencingObservable)
            .put("focus_granted", focusGranted).put("focus_abandoned", focusAbandoned)
            .put("previous_mode", previousMode).put("communication_mode", communicationMode).put("restored_mode", restoredMode)
            .put("aec", aec.json()).put("ns", ns.json()).put("playback_rate", playbackRate).put("requested_rate", requestedRate)
            .put("underruns", underruns).put("capture_clock", captureClock.json())
            .put("playback_clock", playbackClock.json()).put("playback_head_clock", playbackHeadClock.json())
            .put("native", JSONObject(nativeStats)).put("native_stop", stopResult?.let(::JSONObject) ?: JSONObject.NULL)
            .put("playback_compensation", JSONObject(playbackCompensation))
            .put("last_render_loop_trace", lastRenderLoopTrace?.json() ?: JSONObject.NULL)
            .put("last_slow_write_diagnostic", lastSlowWriteDiagnostic?.json() ?: JSONObject.NULL)
    }

    private val app = context.applicationContext
    private val stopping = AtomicBoolean(false)
    private val complete = AtomicBoolean(false)
    private val phase = AtomicReference("starting")
    private val failure = AtomicReference<String?>(null)
    private val stopReason = AtomicReference<String?>(null)
    private val handle = AtomicLong(0)
    private val micActive = AtomicBoolean(false)
    private val recordOpen = AtomicBoolean(false)
    private val trackOpen = AtomicBoolean(false)
    private val captured = AtomicLong(0)
    private val submitted = AtomicLong(0)
    private val rendered = AtomicLong(0)
    private val queued = AtomicLong(0)
    private val maxQueued = AtomicLong(0)
    private val rejected = AtomicLong(0)
    private val capturePeak = AtomicInteger(0)
    private val captureClipped = AtomicLong(0)
    private val captureZero = AtomicLong(0)
    private val renderPeak = AtomicInteger(0)
    private val renderClipped = AtomicLong(0)
    private val requestedRate = AtomicInteger(SAMPLE_RATE)
    private val manualRateOffset = AtomicInteger(0)
    private val playbackRate = AtomicInteger(0)
    private val captureClock = AtomicReference(ClockPair())
    private val playbackClock = AtomicReference(ClockPair())
    private val headClock = AtomicReference(ClockPair())
    private val nativeStats = AtomicReference("{}")
    private val playbackCompensation = AtomicReference("{}")
    private val renderLoopRecorder = RenderLoopRecorder()
    @Volatile private var slowWriteProbe: SlowWriteProbe? = null
    private val stopResult = AtomicReference<String?>(null)
    @Volatile private var captureThread: Thread? = null
    @Volatile private var renderThread: Thread? = null
    @Volatile private var recordMin = 0
    @Volatile private var trackMin = 0
    @Volatile private var recordFrames = 0
    @Volatile private var trackFrames = 0
    @Volatile private var trackCapacity = 0
    @Volatile private var startThreshold = -1
    @Volatile private var trackPerformanceMode = -1
    @Volatile private var inputId = 0
    @Volatile private var inputType = 0
    @Volatile private var outputId = 0
    @Volatile private var outputType = 0
    @Volatile private var micMuted = false
    @Volatile private var silenced = false
    @Volatile private var silencingObservable = false
    @Volatile private var focusGranted = false
    @Volatile private var focusAbandoned = false
    @Volatile private var previousMode = -1
    @Volatile private var communicationMode = -1
    @Volatile private var restoredMode = -1
    @Volatile private var aecState = Effect()
    @Volatile private var nsState = Effect()
    @Volatile private var aecOpen = false
    @Volatile private var nsOpen = false
    @Volatile private var underruns = 0
    @Volatile private var outputRouteRestored = true
    private val owner = Thread(::run, "dmsg-call-audio")
    private val localMuted = AtomicBoolean(false)
    private val hardwareGeneration = AtomicLong(0)
    private val captureRoutePublishedNs = AtomicLong(0)

    fun start() { owner.start() }
    fun setMuted(value: Boolean) { localMuted.set(value) }
    internal fun hardwareRouteChanged() {
        captureRoutePublishedNs.set(System.nanoTime())
        hardwareGeneration.incrementAndGet()
    }
    /** Only the service cleanup coroutine / debug gate may wait, never UI callbacks. */
    internal fun join() { owner.join() }

    /** Safe from UI callbacks: only signals the dedicated owner, never joins. */
    fun requestStop(reason: String = "explicit_stop") {
        stopReason.compareAndSet(null, reason)
        stopping.set(true)
        LockSupport.unpark(owner)
    }

    /** Explicit fixture actuator override; automatic correction resumes at zero. */
    fun setPlaybackRateOffsetHz(offset: Int) {
        require(offset in -8..8) { "Playback rate offset must be within +/-8 Hz" }
        manualRateOffset.set(offset)
        requestedRate.set(SAMPLE_RATE + offset)
    }

    fun snapshot() = Snapshot(phase.get(), failure.get(), stopReason.get(), port.dns,
        complete.get(), owner.isAlive, captureThread?.isAlive == true, renderThread?.isAlive == true,
        micActive.get(), recordOpen.get(), trackOpen.get(), aecOpen || nsOpen, handle.get(), captured.get(), submitted.get(), rendered.get(),
        queued.get(), maxQueued.get(), rejected.get(), capturePeak.get(), captureClipped.get(), captureZero.get(),
        renderPeak.get(), renderClipped.get(), recordMin, trackMin, recordFrames, trackFrames, trackCapacity, startThreshold,
        inputId, inputType, outputId, outputType, micMuted, silenced, silencingObservable, focusGranted, focusAbandoned,
        previousMode, communicationMode, restoredMode, aecState, nsState, playbackRate.get(), requestedRate.get(), underruns,
        captureClock.get(), playbackClock.get(), headClock.get(), nativeStats.get(), stopResult.get(), playbackCompensation.get(),
        renderLoopRecorder.latest, outputMuted, outputRouteRestored, trackPerformanceMode, slowWriteProbe?.latest)

    private class ProbeFailure(val diagnostic: String) : RuntimeException()
    private fun check(condition: Boolean, diagnostic: String) { if (!condition) throw ProbeFailure(diagnostic) }
    private fun fail(diagnostic: String) { failure.compareAndSet(null, diagnostic); requestStop("failure") }

    private fun nativeEvidence(raw: String): String {
        val source = JSONObject(raw)
        val clean = JSONObject()
        nativeCounters.forEach { field ->
            val value = source.getLong(field)
            check(value >= 0, "invalid_native_counter")
            clean.put(field, value)
        }
        val routeGap = source.getLong("source_route_gap_samples")
        check(routeGap % BATCH_SAMPLES == 0L && routeGap <= source.getLong("dropped_capture"),
            "invalid_native_route_gap")
        listOf("received_rtp_packets", "capture_gap_batches", "fixture_peer_encoded_packets",
            "fixture_peer_dropped_capture", "fixture_peer_capture_gap_batches", "tiny_non_dtx_packets",
            "arrival_after_nominal_due_packets", "late_before_nominal_due_packets", "plc_before_nominal_due_slots",
            "skipped_playout_slots", "expired_render_samples", "decode_after_nominal_due_slots",
            "max_decode_us", "max_playout_tick_lateness_ms", "max_capture_age_us",
            "initial_capture_age_floor_us", "max_additional_capture_age_us", "max_capture_clock_observation_gap_us",
            "capture_age_unavailable_batches", "capture_age_rejected_batches", "max_sender_phase_advance_us",
            "max_receiver_phase_advance_us", "future_rejected_packets", "max_future_lead_ms",
            "remote_clock_ticks", "remote_clock_ns", "remote_clock_rejected_reports", "remote_clock_rejection_mask",
            "max_rx_noise_auth_us", "max_rx_post_noise_wait_us", "max_rx_validation_us",
            "noise_after_nominal_due_packets", "noise_before_due_admitted_after_due_packets",
            "max_source_ready_to_commit_us")
            .forEach { field ->
                val value = source.getLong(field)
                check(value >= 0, "invalid_native_counter")
                clean.put(field, value)
            }
        listOf("encode_duration_bins", "decode_duration_bins", "plc_duration_bins",
            "rx_post_noise_wait_bins", "rx_validation_duration_bins", "source_ready_to_commit_bins")
            .forEach { field ->
                val values = source.getJSONArray(field)
                check(values.length() == 8, "invalid_native_timing_bins")
                val bins = org.json.JSONArray()
                for (index in 0 until 8) {
                    val value = values.getLong(index)
                    check(value >= 0, "invalid_native_timing_bins")
                    bins.put(value)
                }
                clean.put(field, bins)
            }
        if (source.isNull("last_render_expiry")) {
            clean.put("last_render_expiry", JSONObject.NULL)
        } else {
            val trace = source.getJSONObject("last_render_expiry")
            val expiry = JSONObject()
            val kind = trace.getString("kind")
            check(kind == "queued" || kind == "completion", "invalid_native_render_expiry")
            expiry.put("kind", kind)
            listOf("publication_vs_start_us", "first_pull_vs_start_us", "last_pull_vs_start_us")
                .forEach { field ->
                    check(trace.has(field), "invalid_native_render_expiry")
                    expiry.put(field, if (trace.isNull(field)) JSONObject.NULL else trace.getLong(field))
                }
            val codecDuration = if (trace.isNull("codec_duration_us")) null else trace.getLong("codec_duration_us")
            check(trace.has("codec_duration_us") && (codecDuration == null || codecDuration >= 0),
                "invalid_native_render_expiry")
            expiry.put("codec_duration_us", codecDuration ?: JSONObject.NULL)
            listOf("successful_pull_calls", "initial_samples", "transferred_samples", "discarded_samples",
                "source_frame_duration_us", "sink_queue_samples").forEach { field ->
                val value = trace.getLong(field)
                check(value >= 0, "invalid_native_render_expiry")
                expiry.put(field, value)
            }
            expiry.put("expiry_vs_end_us", trace.getLong("expiry_vs_end_us"))
            val sinkRate = trace.getLong("sink_rate_ppb")
            check(sinkRate in -1_000_000L..1_000_000L, "invalid_native_render_expiry")
            expiry.put("sink_rate_ppb", sinkRate)
            clean.put("last_render_expiry", expiry)
        }
        check(source.has("last_progress_failure"), "invalid_native_progress_failure")
        if (source.isNull("last_progress_failure")) {
            clean.put("last_progress_failure", JSONObject.NULL)
        } else {
            val trace = source.getJSONObject("last_progress_failure")
            val progress = JSONObject()
            val reason = trace.getString("reason")
            check(reason == "expired" || reason == "window_exhausted", "invalid_native_progress_failure")
            progress.put("reason", reason)
            val site = trace.getString("check_site")
            check(site == "top_turn" || site == "media_admission" || site == "receipt_commit",
                "invalid_native_progress_failure")
            progress.put("check_site", site)
            listOf("ledger_bytes", "window_bytes", "entry_count", "max_age_us", "pending_bytes",
                "pending_receipt_count", "committed_receipt_count", "check_extra_bytes", "total_check_bytes")
                .forEach { field ->
                    val value = trace.getLong(field)
                    check(value >= 0, "invalid_native_progress_failure")
                    progress.put(field, value)
                }
            listOf("attempted_index", "frame_bytes", "oldest_index", "oldest_age_us",
                "highest_committed_index", "terminal_frontier", "waiting_bytes").forEach { field ->
                check(trace.has(field), "invalid_native_progress_failure")
                val value = if (trace.isNull(field)) null else trace.getLong(field)
                check(value == null || value >= 0, "invalid_native_progress_failure")
                progress.put(field, value ?: JSONObject.NULL)
            }
            check(trace.has("last_authenticated_control"), "invalid_native_progress_failure")
            if (trace.isNull("last_authenticated_control")) {
                progress.put("last_authenticated_control", JSONObject.NULL)
            } else {
                val control = trace.getJSONObject("last_authenticated_control")
                val observation = JSONObject()
                observation.put("sender_clock_present", control.getBoolean("sender_clock_present"))
                listOf("terminal", "body_to_noise_us", "noise_to_processed_us", "since_processed_us",
                    "since_noise_us", "since_body_us").forEach { field ->
                    check(control.has(field), "invalid_native_progress_failure")
                    val value = if (control.isNull(field)) null else control.getLong(field)
                    check(value == null || value >= 0, "invalid_native_progress_failure")
                    observation.put(field, value ?: JSONObject.NULL)
                }
                progress.put("last_authenticated_control", observation)
            }
            clean.put("last_progress_failure", progress)
        }
        listOf("last_validated_peer_media_index", "last_prepared_feedback_terminal",
            "last_committed_feedback_terminal").forEach { field ->
            check(source.has(field), "invalid_native_feedback_frontier")
            val value = if (source.isNull(field)) null else source.getLong(field)
            check(value == null || value >= 0, "invalid_native_feedback_frontier")
            clean.put(field, value ?: JSONObject.NULL)
        }
        listOf("last_prepared_feedback_srtcp_index", "last_committed_feedback_srtcp_index",
            "last_authenticated_feedback_srtcp_index").forEach { field ->
            val value = if (!source.has(field) || source.isNull(field)) null else source.getLong(field)
            check(value == null || value in 0L..0x7fff_ffffL, "invalid_native_feedback_frontier")
            clean.put(field, value ?: JSONObject.NULL)
        }
        clean.put("ready", source.getBoolean("ready")).put("dns_carrier", source.getBoolean("dns_carrier"))
        clean.put("failed", source.getBoolean("failed")).put("stopped", source.getBoolean("stopped"))
        clean.put("capture_clock_calibrated", source.getBoolean("capture_clock_calibrated"))
        clean.put("remote_clock_calibrated", source.getBoolean("remote_clock_calibrated"))
        clean.put("remote_clock_valid", source.getBoolean("remote_clock_valid"))
        val error = source.optString("error")
        // Never forward arbitrary exceptions, fixture/config content, or path strings.
        val safeError = when (error) {
            "remote media window exhausted; retire generation" -> "remote_media_window_exhausted"
            "remote media progress expired; retire generation" -> "remote_media_progress_expired"
            "control submission expired; retire generation" -> "control_submission_expired"
            else -> if (error.matches(Regex("[A-Za-z0-9_ -]{0,120}"))) error else "native_diagnostic_redacted"
        }
        clean.put("error", safeError)
        clean.put("native_transport", NativeTransportEvidence.fromNativeStats(source) ?: JSONObject.NULL)
        clean.put("control_delivery", ControlDeliveryEvidence.fromNativeStats(source) ?: JSONObject.NULL)
        return clean.toString()
    }

    private fun run() {
        var manager: AudioManager? = null
        var record: AudioRecord? = null
        var track: AudioTrack? = null
        var aec: AcousticEchoCanceler? = null
        var ns: NoiseSuppressor? = null
        try {
            handle.set(port.initialHandle)
            check(app.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED,
                "microphone_permission_denied")
            if (stopping.get()) return
            manager = app.getSystemService(AudioManager::class.java)
            val attributes = AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build()
            platform.prepare(manager, ::fail)
            updatePlatformEvidence()
            if (stopping.get()) return
            micMuted = manager.isMicrophoneMute
            platform.checkMicrophone(micMuted)
            recordMin = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            trackMin = AudioTrack.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_16BIT)
            check(recordMin > 0 && trackMin > 0, "unsupported_16k_audio_path")
            record = AudioRecord.Builder().setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
                .setAudioFormat(AudioFormat.Builder().setSampleRate(SAMPLE_RATE).setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                .setBufferSizeInBytes(max(recordMin, BATCH_SAMPLES * 2 * 4)).build()
            recordOpen.set(true)
            check(record.state == AudioRecord.STATE_INITIALIZED, "record_initialization_failed")
            check(record.sampleRate == SAMPLE_RATE && record.channelCount == 1 && record.audioFormat == AudioFormat.ENCODING_PCM_16BIT,
                "record_format_mismatch")
            recordFrames = record.bufferSizeInFrames
            track = AudioTrack.Builder().setAudioAttributes(attributes)
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setAudioFormat(AudioFormat.Builder().setSampleRate(SAMPLE_RATE).setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                .setTransferMode(AudioTrack.MODE_STREAM).setBufferSizeInBytes(max(trackMin, QUEUE_BOUND_SAMPLES * 2)).build()
            trackOpen.set(true)
            check(track.state == AudioTrack.STATE_INITIALIZED, "track_initialization_failed")
            check(track.sampleRate == SAMPLE_RATE && track.channelCount == 1 && track.audioFormat == AudioFormat.ENCODING_PCM_16BIT,
                "track_format_mismatch")
            // Acoustic attribution only: consume the same PCM without changing the system volume.
            if (outputMuted) check(track.setVolume(0f) == AudioTrack.SUCCESS, "track_volume_failed")
            check(track.setBufferSizeInFrames(QUEUE_BOUND_SAMPLES) > 0, "track_buffer_size_failed")
            trackFrames = track.bufferSizeInFrames
            trackCapacity = track.bufferCapacityInFrames
            check(trackFrames >= BATCH_SAMPLES, "track_buffer_too_small")
            if (Build.VERSION.SDK_INT >= 31) {
                check(track.setStartThresholdInFrames(BATCH_SAMPLES) > 0, "track_start_threshold_failed")
                startThreshold = track.startThresholdInFrames
                check(startThreshold <= QUEUE_BOUND_SAMPLES, "track_start_threshold_exceeds_queue_bound")
            }
            val aecAvailable = AcousticEchoCanceler.isAvailable()
            aecState = Effect(available = aecAvailable)
            if (aecAvailable) aec = AcousticEchoCanceler.create(record.audioSessionId)
            aecOpen = aec != null
            aecState = enableEffect(aecAvailable, aec, "aec")
            val nsAvailable = NoiseSuppressor.isAvailable()
            nsState = Effect(available = nsAvailable)
            if (nsAvailable) ns = NoiseSuppressor.create(record.audioSessionId)
            nsOpen = ns != null
            nsState = enableEffect(nsAvailable, ns, "ns")
            if (stopping.get()) return
            val nativeHandle = if (handle.get() != 0L) handle.get() else port.acquire()
            check(nativeHandle != 0L, "native_start_failed")
            handle.set(nativeHandle)
            if (stopping.get()) return
            track.play()
            check(track.playState == AudioTrack.PLAYSTATE_PLAYING, "track_play_failed")
            record.startRecording()
            micActive.set(record.recordingState == AudioRecord.RECORDSTATE_RECORDING)
            check(micActive.get(), "microphone_start_failed")
            if (stopping.get()) return
            val activeRecord = record; val activeTrack = track
            captureThread = Thread({ capture(activeRecord, nativeHandle) }, "dmsg-call-capture").also { it.start() }
            renderThread = Thread({ render(activeTrack, nativeHandle) }, "dmsg-call-render").also { it.start() }
            phase.set("running")
            val audioStarted = System.nanoTime()
            while (!stopping.get()) {
                val evidence = nativeEvidence(port.stats(nativeHandle))
                nativeStats.set(evidence)
                if (JSONObject(evidence).getBoolean("failed")) fail("native_probe_failed")
                if (JSONObject(evidence).getBoolean("stopped")) fail("native_stopped_early")
                micMuted = manager.isMicrophoneMute
                platform.checkMicrophone(micMuted)
                if (Build.VERSION.SDK_INT >= 29) {
                    val config = manager.activeRecordingConfigurations.firstOrNull { it.clientAudioSessionId == activeRecord.audioSessionId }
                    silencingObservable = config != null
                    silenced = config?.isClientSilenced == true
                    if (silenced) fail("microphone_system_silenced")
                }
                check(manager.mode == AudioManager.MODE_IN_COMMUNICATION, "communication_mode_changed")
                check(activeRecord.recordingState == AudioRecord.RECORDSTATE_RECORDING, "microphone_stopped_early")
                check(activeTrack.playState == AudioTrack.PLAYSTATE_PLAYING, "playback_stopped_early")
                observeRoutes(activeRecord, activeTrack)
                // Native restoration may replace the startup proxy. Observe its
                // current configuration; requested LOW_LATENCY is not a grant.
                trackFrames = activeTrack.bufferSizeInFrames
                trackCapacity = activeTrack.bufferCapacityInFrames
                trackPerformanceMode = activeTrack.performanceMode
                if (Build.VERSION.SDK_INT >= 31) startThreshold = activeTrack.startThresholdInFrames
                aecState = observeEffect(aecState, aec, "aec")
                nsState = observeEffect(nsState, ns, "ns")
                check((captureThread?.isAlive == true && renderThread?.isAlive == true) || stopping.get(), "audio_worker_ended")
                if (System.nanoTime() - audioStarted > 3_000_000_000L) {
                    check(captured.get() > 0, "microphone_no_progress")
                    check(submitted.get() == 0L || rendered.get() > 0, "hardware_playback_no_progress")
                }
                LockSupport.parkNanos(100_000_000)
            }
        } catch (e: ProbeFailure) { if (!stopping.get()) fail(e.diagnostic) }
        catch (e: CallPlatformFailure) { if (!stopping.get()) fail(e.diagnostic) }
        catch (_: Throwable) { if (!stopping.get()) fail("native_or_audio_setup_failed") }
        finally {
            stopping.set(true)
            phase.set("stopping")
            // Off UI. stop() unblocks a blocking AudioRecord read. Native stop is
            // after both PCM workers join, so no JNI call races handle retirement.
            if (record != null) {
                try {
                    if (record.recordingState == AudioRecord.RECORDSTATE_RECORDING) record.stop()
                    micActive.set(record.recordingState == AudioRecord.RECORDSTATE_RECORDING)
                }
                catch (_: Throwable) {
                    failure.compareAndSet(null, "record_stop_failed")
                    try { record.release(); recordOpen.set(false); micActive.set(false) }
                    catch (_: Throwable) { failure.compareAndSet(null, "record_release_failed") }
                }
            }
            try { track?.pause(); track?.flush() } catch (_: Throwable) { failure.compareAndSet(null, "track_stop_failed") }
            captureThread?.join()
            renderThread?.join()
            try { aec?.release(); aecOpen = false } catch (_: Throwable) { failure.compareAndSet(null, "aec_release_failed") }
            try { ns?.release(); nsOpen = false } catch (_: Throwable) { failure.compareAndSet(null, "ns_release_failed") }
            try { if (recordOpen.get()) record?.release(); recordOpen.set(false); micActive.set(false) }
            catch (_: Throwable) { failure.compareAndSet(null, "record_release_failed") }
            try { track?.release(); trackOpen.set(false); queued.set(0) }
            catch (_: Throwable) { failure.compareAndSet(null, "track_release_failed") }
            val retired = handle.get()
            if (retired != 0L) {
                try {
                    val result = nativeEvidence(port.stop(retired))
                    stopResult.set(result); nativeStats.set(result)
                    val json = JSONObject(result)
                    if (json.getBoolean("stopped")) handle.set(0) else failure.compareAndSet(null, "native_stop_incomplete")
                    if (json.getBoolean("failed")) failure.compareAndSet(null, "native_probe_failed")
                } catch (_: Throwable) { failure.compareAndSet(null, "native_stop_failed") }
            }
            try { platform.release() } catch (_: Throwable) { failure.compareAndSet(null, "audio_platform_release_failed") }
            updatePlatformEvidence()
            complete.set(!micActive.get() && !recordOpen.get() && !trackOpen.get() && !aecOpen && !nsOpen && handle.get() == 0L &&
                (!focusGranted || focusAbandoned) && platform.cleanupComplete && outputRouteRestored)
            phase.set(if (failure.get() == null) "stopped" else "failed")
            finished(snapshot())
        }
    }

    private fun enableEffect(available: Boolean, effect: AudioEffect?, name: String): Effect {
        if (!available) return Effect()
        check(effect != null, "${name}_create_failed")
        val result = effect!!.setEnabled(true)
        val state = Effect(true, true, result, effect.enabled, effect.hasControl())
        if (name == "aec") aecState = state else nsState = state
        check(result == AudioEffect.SUCCESS && state.enabled && state.control, "${name}_enable_failed")
        return state
    }

    private fun observeEffect(before: Effect, effect: AudioEffect?, name: String): Effect {
        if (effect == null) return before
        val state = before.copy(enabled = effect.enabled, control = effect.hasControl())
        if (name == "aec") aecState = state else nsState = state
        check(state.enabled && state.control, "${name}_control_lost")
        return state
    }

    private fun observeRoutes(record: AudioRecord, track: AudioTrack) {
        val input = record.routedDevice
        val output = track.routedDevice
        check(inputId == 0 || input != null, "input_route_lost")
        check(outputId == 0 || output != null, "output_route_lost")
        input?.let { route ->
            platform.checkRoute(true, inputId, route)
            inputId = route.id; inputType = route.type
        }
        output?.let { route ->
            platform.checkRoute(false, outputId, route)
            outputId = route.id; outputType = route.type
        }
    }

    private fun updatePlatformEvidence() {
        val state = platform.evidence
        previousMode = state.previousMode; communicationMode = state.communicationMode; restoredMode = state.restoredMode
        focusGranted = state.focusGranted; focusAbandoned = state.focusAbandoned
        outputRouteRestored = state.outputRouteRestored
    }

    private fun observeClock(ref: AtomicReference<ClockPair>, clock: Clock?) {
        val before = ref.get()
        ref.set(if (clock == null) before.copy(unavailable = before.unavailable + 1)
            else ClockPair(before.first ?: clock, clock, before.unavailable))
    }

    private fun capture(record: AudioRecord, nativeHandle: Long) {
        val pcm = ShortArray(BATCH_SAMPLES)
        val timestamp = AudioTimestamp()
        var valid = 0
        var readPosition = 0L
        var previousClock: Clock? = null
        var nextTimestamp = 0L
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO)
            while (!stopping.get()) {
                val count = record.read(pcm, valid, pcm.size - valid, AudioRecord.READ_BLOCKING)
                if (stopping.get()) {
                    // stop() may return the last partial read. It still owns
                    // source positions and is counted as loss below, not played.
                    if (count > 0 && count <= pcm.size - valid) {
                        captured.addAndGet(count.toLong()); readPosition += count; valid += count
                    }
                    break
                }
                check(count >= 0, "capture_read_failed")
                check(count <= pcm.size - valid, "capture_read_invalid")
                if (count == 0) { LockSupport.parkNanos(2_000_000); continue }
                var peak = 0; var clipped = 0L; var zeros = 0L
                for (i in valid until valid + count) {
                    val sample = abs(pcm[i].toInt()); peak = max(peak, sample)
                    if (sample >= 32767) clipped++
                    if (sample == 0) zeros++
                }
                capturePeak.accumulateAndGet(peak, ::max)
                captureClipped.addAndGet(clipped); captureZero.addAndGet(zeros)
                captured.addAndGet(count.toLong()); readPosition += count; valid += count
                if (valid == pcm.size) {
                    // Only the existing service's Telecom endpoint publication
                    // sets this marker. Serialize the JNI command with pushes;
                    // preserve publication time even when read() was blocked.
                    val publishedNs = captureRoutePublishedNs.getAndSet(0)
                    if (publishedNs != 0L) port.sourceRouteChanged(nativeHandle, publishedNs)
                    // The documented frame epoch resets at startRecording, not
                    // at the first successful timestamp or complete JNI batch.
                    // Poll before each handoff: old/unavailable anchors cannot
                    // certify buffered PCM as fresh. Native validates age using
                    // System.nanoTime and the oldest sample across partial reads.
                    val ok = record.getTimestamp(timestamp, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS
                    if (ok && !platform.probeMode) {
                        val before = previousClock
                        if (before != null) check(timestamp.framePosition >= before.frame && timestamp.nanoTime >= before.nanoTime &&
                            ((timestamp.framePosition == before.frame) == (timestamp.nanoTime == before.nanoTime)),
                            "capture_clock_backwards")
                        previousClock = Clock(timestamp.framePosition, timestamp.nanoTime)
                    }
                    val clock = if (ok && timestamp.nanoTime > 0 && timestamp.framePosition >= 0)
                        Clock(timestamp.framePosition, timestamp.nanoTime) else null
                    val now = System.nanoTime()
                    if (now >= nextTimestamp) {
                        observeClock(captureClock, clock)
                        nextTimestamp = now + 100_000_000
                    }
                    // A rejected batch is dropped, never retained/retried in a PCM FIFO.
                    val batch = valid
                    valid = 0 // Ownership transfers once, even if JNI throws.
                    if (localMuted.get() || !platform.probeMode && micMuted) pcm.fill(0)
                    if (!port.push(nativeHandle, pcm, batch, readPosition - batch,
                            clock?.frame ?: -1L, clock?.nanoTime ?: -1L)) rejected.incrementAndGet()
                    pcm.fill(0)
                }
            }
        } catch (e: ProbeFailure) { if (!stopping.get()) fail(e.diagnostic) }
        catch (_: Throwable) { if (!stopping.get()) fail("capture_worker_failed") }
        finally {
            if (valid > 0) {
                // The owner joins capture before retiring the handle. An
                // incomplete batch has no certified age and cannot be sent.
                try {
                    if (localMuted.get() || !platform.probeMode && micMuted) pcm.fill(0)
                    port.push(nativeHandle, pcm, valid, readPosition - valid, -1L, -1L)
                    rejected.incrementAndGet()
                } catch (_: Throwable) { failure.compareAndSet(null, "capture_discard_failed") }
            }
            pcm.fill(0)
        }
    }

    private fun render(track: AudioTrack, nativeHandle: Long) {
        val pcm = ShortArray(BATCH_SAMPLES)
        val timestamp = AudioTimestamp()
        var valid = 0; var offset = 0
        var wrap = 0L; var previousHead = 0L
        var nextTimestamp = 0L
        val compensation = CallPlaybackClock()
        var routeGeneration = hardwareGeneration.get()
        val metadataCache = RenderMetadataCache()
        val compensationCache = RenderCompensationCache()
        // Open on this existing render thread only. Unsupported/denied proc access stays unknown, without retry.
        val schedstat = if (platform.probeMode) try { RandomAccessFile("/proc/thread-self/schedstat", "r") }
            catch (_: Exception) { null } else null
        val writeProbe = if (platform.probeMode) SlowWriteProbe(read = read@{ bytes ->
            if (schedstat == null) return@read -1
            schedstat.seek(0)
            var length = 0
            while (length < bytes.size) {
                val count = schedstat.read(bytes, length, bytes.size - length)
                if (count < 0) break
                if (count == 0) return@read -1
                length += count
            }
            length
        }) else null
        slowWriteProbe = writeProbe
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO)
            while (!stopping.get()) {
                val currentRoute = hardwareGeneration.get()
                if (currentRoute != routeGeneration) {
                    compensation.resetHardwareCalibration()
                    check(port.sinkRate(nativeHandle, 0), "sink_clock_rate_rejected")
                    playbackClock.set(ClockPair()); headClock.set(ClockPair())
                    routeGeneration = currentRoute
                }
                val iterationStart = System.nanoTime()
                val validBefore = valid; val offsetBefore = offset
                var cycleCompleted = false
                var counterReadNs = -1L; var statsNs = -1L; var clockNs = -1L; var headNs = -1L; var sinkNs = -1L; var timestampNs = -1L
                var pullNs = -1L; var writeNs = -1L; var parkNs = -1L
                var observedHead = -1L; var observedDepth = -1L
                var freshRequest = -1; var pullValid = -1; var writeRequested = -1; var writeAccepted = -1
                var parkReason = RenderParkReason.NONE
                try {
                    val counterReadStart = System.nanoTime()
                    val expired = port.renderExpiredSamples(nativeHandle)
                    val observation = System.nanoTime() // Getter completion, before the existing cache parse.
                    counterReadNs = observation - counterReadStart
                    val expiryDelta = renderLoopRecorder.counterIncrease(expired)
                    val statsStart = System.nanoTime()
                    val source = metadataCache.read(nativeStats.get())
                    val cachedCounter = if (source.has("expired_render_samples") && !source.isNull("expired_render_samples"))
                        source.getLong("expired_render_samples") else -1L
                    statsNs = System.nanoTime() - statsStart
                    if (expiryDelta > 0) {
                        val expiry = if (cachedCounter == expired && !source.isNull("last_render_expiry"))
                            RenderExpiry.fromJson(source.getJSONObject("last_render_expiry")) else null
                        renderLoopRecorder.freezeExpiry(expired, expiryDelta, cachedCounter, expiry,
                            observation, iterationStart, counterReadStart)
                    }
                    val clockStart = System.nanoTime()
                    val command = compensation.choose(System.nanoTime(), source.optLong("remote_clock_ticks"),
                        source.optLong("remote_clock_ns"), source.optBoolean("remote_clock_valid"), manualRateOffset.get())
                    compensationCache.changedJson(command)?.let { playbackCompensation.set(it) }
                    requestedRate.set(command.rate)
                    command.sinkPpb?.let { check(port.sinkRate(nativeHandle, it), "sink_clock_rate_rejected") }
                    if (!platform.probeMode) check(!compensation.backwards, "playback_clock_backwards")
                    if (!platform.probeMode) check(command.relativePpm?.let { abs(it) <= 500.0 } != false,
                        "unsupported_relative_audio_clock")
                    val rate = requestedRate.get()
                    if (playbackRate.get() != rate) {
                        check(track.setPlaybackRate(rate) == AudioTrack.SUCCESS, "playback_rate_failed")
                        playbackRate.set(track.playbackRate)
                        check(playbackRate.get() == rate, "playback_rate_readback_failed")
                        playbackClock.set(ClockPair()); headClock.set(ClockPair())
                    }
                    clockNs = System.nanoTime() - clockStart
                    val headStart = System.nanoTime()
                    val head = track.playbackHeadPosition.toLong() and 0xffffffffL
                    headNs = System.nanoTime() - headStart
                    if (head < previousHead) {
                        check(previousHead - head > 0x80000000L, "playback_head_discontinuity")
                        wrap += 0x100000000L
                    }
                    previousHead = head
                    val hardware = wrap + head
                    observedHead = hardware
                    rendered.set(hardware)
                    val depth = submitted.get() - hardware
                    observedDepth = depth
                    check(depth >= 0 && depth <= QUEUE_BOUND_SAMPLES, "playback_queue_invalid")
                    queued.set(depth); maxQueued.accumulateAndGet(depth, ::max)
                    val sinkStart = System.nanoTime()
                    port.sinkQueued(nativeHandle, depth.toInt())
                    sinkNs = System.nanoTime() - sinkStart
                    val now = System.nanoTime()
                    if (now >= nextTimestamp) {
                        val timestampStart = System.nanoTime()
                        observeClock(headClock, Clock(hardware, now))
                        val ok = track.getTimestamp(timestamp)
                        val observed = System.nanoTime() // timestamp retrieval can advance beyond pre-call now
                        if (ok) compensation.observe(timestamp.framePosition, timestamp.nanoTime, observed,
                            track.playbackRate, track.underrunCount, hardware)
                        else compensation.observe(-1, -1, observed, track.playbackRate, track.underrunCount)
                        observeClock(playbackClock, if (ok && timestamp.nanoTime > 0 && timestamp.framePosition >= 0)
                            Clock(timestamp.framePosition, timestamp.nanoTime) else null)
                        underruns = track.underrunCount
                        nextTimestamp = now + 100_000_000
                        timestampNs = System.nanoTime() - timestampStart
                    }
                    // Account the *actual* sink queue, irrespective of its minBuffer/capacity.
                    if (!renderHasCapacity(depth, valid - offset)) {
                        parkReason = RenderParkReason.CAPACITY
                        val parkStart = System.nanoTime()
                        LockSupport.parkNanos(2_000_000)
                        parkNs = System.nanoTime() - parkStart
                        cycleCompleted = true
                        continue
                    }
                    if (valid == offset) {
                        val requested = freshPullLimit(depth)
                        freshRequest = requested
                        val pullStart = System.nanoTime()
                        valid = port.pull(nativeHandle, pcm, requested); offset = 0
                        pullNs = System.nanoTime() - pullStart
                        pullValid = valid
                        check(valid in 0..requested, "native_pull_invalid")
                        if (valid == 0) {
                            parkReason = RenderParkReason.EMPTY_PULL
                            val parkStart = System.nanoTime()
                            LockSupport.parkNanos(2_000_000)
                            parkNs = System.nanoTime() - parkStart
                            cycleCompleted = true
                            continue
                        }
                    }
                    writeRequested = valid - offset
                    writeProbe?.beforeWrite()
                    val writeStart = System.nanoTime()
                    val count = track.write(pcm, offset, valid - offset, AudioTrack.WRITE_NON_BLOCKING)
                    writeNs = System.nanoTime() - writeStart
                    writeProbe?.afterWrite(writeNs, writeRequested, count)
                    writeAccepted = count
                    check(count >= 0, "render_write_failed")
                    var peak = 0; var clipped = 0L
                    for (i in offset until offset + count) {
                        val sample = abs(pcm[i].toInt()); peak = max(peak, sample)
                        if (sample >= 32767) clipped++
                    }
                    renderPeak.accumulateAndGet(peak, ::max); renderClipped.addAndGet(clipped)
                    offset += count; submitted.addAndGet(count.toLong())
                    val afterWriteDepth = submitted.get() - hardware
                    queued.set(afterWriteDepth); maxQueued.accumulateAndGet(afterWriteDepth, ::max)
                    if (count == 0) {
                        parkReason = RenderParkReason.ZERO_WRITE
                        val parkStart = System.nanoTime()
                        LockSupport.parkNanos(2_000_000)
                        parkNs = System.nanoTime() - parkStart
                    }
                    cycleCompleted = true
                } finally {
                    renderLoopRecorder.record(iterationStart, System.nanoTime(), cycleCompleted, counterReadNs, statsNs, clockNs, headNs,
                        sinkNs, timestampNs, pullNs, writeNs, parkNs, observedHead, observedDepth, freshRequest, pullValid,
                        validBefore, offsetBefore, valid, offset, writeRequested, writeAccepted, parkReason)
                }
            }
        } catch (e: ProbeFailure) { if (!stopping.get()) fail(e.diagnostic) }
        catch (_: Throwable) { if (!stopping.get()) fail("render_worker_failed") }
        finally {
            pcm.fill(0)
            try { schedstat?.close() } catch (_: Exception) { /* Diagnostic only. */ }
        }
    }
}

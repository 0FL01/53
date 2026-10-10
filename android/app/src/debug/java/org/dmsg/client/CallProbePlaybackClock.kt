package org.dmsg.client

import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.roundToLong

/** Fixture-only hardware-rate calibration and fractional-Hz platform actuator.
 * Network arrival/queue delay is deliberately absent. Route changes retire the owner.
 */
internal class CallProbePlaybackClock {
    data class Output(val rate: Int, val sinkPpb: Long?, val calibrated: Boolean,
        val healthy: Boolean, val relativePpm: Double?, val rejectionMask: Long)
    private data class Point(val frame: Long, val ns: Long, val rate: Int, val underruns: Int)
    private var first: Point? = null
    private var middle: Point? = null
    private var latest: Point? = null
    private var neutralRatio: Double? = null
    private var healthy = false
    private var targetHz = 16_000.0
    private var fraction = 0.0
    private var nextActuationNs = 0L
    private var selected = 16_000
    private var relativePpm: Double? = null
    private var rejectionMask = 0L

    fun observe(frame: Long, timestampNs: Long, nowNs: Long, actualRate: Int, underruns: Int) {
        if (frame < 0 || timestampNs <= 0 || nowNs < timestampNs) {
            rejectionMask = rejectionMask or 1L
            first = null; middle = null; healthy = false; return
        }
        if (nowNs - timestampNs > 500_000_000L || actualRate !in 15_992..16_008) {
            rejectionMask = rejectionMask or 2L
            first = null; middle = null; healthy = false; return
        }
        val previous = latest
        if (previous != null && (frame < previous.frame || timestampNs < previous.ns ||
                ((frame == previous.frame) != (timestampNs == previous.ns)))) {
            rejectionMask = rejectionMask or 4L
            first = null; middle = null; latest = null; healthy = false; return
        }
        if (previous != null && timestampNs == previous.ns) return
        val point = Point(frame, timestampNs, actualRate, underruns)
        latest = point
        var anchor = first
        // A rate change/underrun cannot certify a hardware-frequency sample.
        if (anchor == null || anchor.rate != actualRate || anchor.underruns != underruns ||
            timestampNs - anchor.ns > 60_000_000_000L) {
            first = point; middle = null; anchor = point
        }
        if (neutralRatio == null && middle == null && timestampNs - anchor.ns >= 10_000_000_000L) {
            middle = point
        }
        if (timestampNs - anchor.ns >= 20_000_000_000L && frame > anchor.frame) {
            val ratio = (frame - anchor.frame).toDouble() * 1e9 / (timestampNs - anchor.ns) / actualRate
            var qualified = ratio.isFinite() && abs(ratio - 1.0) <= 0.001
            if (qualified && neutralRatio == null) {
                val midpoint = middle ?: return
                val beforeNs = midpoint.ns - anchor.ns
                val afterNs = timestampNs - midpoint.ns
                if (afterNs < 10_000_000_000L) return
                // A short unreported startup pause can pass the whole-window
                // +/-1000ppm guard. Certify the first neutral only when two
                // independent >=10s spans agree, allowing one frame of timestamp
                // position uncertainty at each endpoint. No arrival/PCM slope.
                val beforeFrames = midpoint.frame - anchor.frame
                val afterFrames = frame - midpoint.frame
                // Cross products are bounded by the <=60s, +/-1000ppm span
                // above; integer comparison keeps the exact tick boundary.
                qualified = abs(beforeFrames * afterNs - afterFrames * beforeNs) <= 2L * (beforeNs + afterNs)
            }
            if (qualified) {
                // Freeze the first steady hardware ratio for this recording epoch.
                // Later stalls must not grow a rate estimate or move a timeline.
                if (neutralRatio == null) neutralRatio = ratio
                healthy = true
            } else {
                rejectionMask = rejectionMask or 8L
                healthy = false
                // Reject this entire hardware sample. Start a new observation
                // window; never retain a bad startup span as a frequency anchor.
                // The established neutral ratio and media timeline do not move.
                first = point; middle = null
            }
        }
    }

    fun choose(nowNs: Long, remoteTicks: Long, remoteNs: Long, remoteValid: Boolean,
        manualOffset: Int = 0): Output {
        require(manualOffset in -8..8)
        val ratio = neutralRatio
        val reference = latest
        val hardwareFresh = healthy && reference != null && nowNs >= reference.ns && nowNs - reference.ns <= 500_000_000L
        val valid = remoteValid && remoteTicks > 0 && remoteNs > 0 && ratio != null && hardwareFresh
        if (valid) {
            val desired = remoteTicks.toDouble() * 1e9 / remoteNs / 3.0 / ratio!!
            relativePpm = (desired / 16_000.0 - 1.0) * 1e6
            // Unsupported relative skew is observable, never silently clamped.
            healthy = desired.isFinite() && desired in 15_992.0..16_008.0
            if (healthy) targetHz = desired
        }
        if (manualOffset != 0) {
            selected = 16_000 + manualOffset
            nextActuationNs = 0L
        } else if (nowNs >= nextActuationNs) {
            val lower = floor(targetHz).toInt()
            fraction += targetHz - lower
            val extra = if (fraction >= 1.0) { fraction -= 1.0; 1 } else 0
            selected = lower + extra
            // One current command, not catch-up commands after a thread stall.
            nextActuationNs = nowNs + 1_000_000_000L
        }
        val sink = ratio?.let { ((it * selected / 16_000.0 - 1.0) * 1e9).roundToLong() }
        return Output(selected, sink?.takeIf { it in -1_000_000L..1_000_000L }, ratio != null,
            valid && healthy, relativePpm, rejectionMask)
    }
}

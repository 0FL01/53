package org.dmsg.client

import androidx.annotation.Keep

/** Uses the core actor's per-call registry handle, never creates a transport/client. */
@Keep
object CallMediaJni {
    init { System.loadLibrary("dmsg_core") }
    external fun push(handle: Long, pcm: ShortArray, valid: Int, readPosition: Long,
        timestampFrame: Long, timestampNs: Long): Boolean
    external fun sourceRouteChanged(handle: Long, publishedNs: Long): Boolean
    external fun pull(handle: Long, pcm: ShortArray, valid: Int): Int
    external fun sinkQueued(handle: Long, samples: Int)
    external fun sinkRate(handle: Long, ppb: Long): Boolean
    external fun renderExpiredSamples(handle: Long): Long
    external fun stats(handle: Long): String
    /** Caller has joined both audio workers before registry retirement. */
    external fun stop(handle: Long): String
}

internal interface CallAudioPort {
    val dns: Boolean
    val initialHandle: Long get() = 0
    fun acquire(): Long
    fun push(handle: Long, pcm: ShortArray, valid: Int, readPosition: Long, frame: Long, ns: Long): Boolean
    /** Probe input has no production route-source clock API. */
    fun sourceRouteChanged(handle: Long, publishedNs: Long): Boolean = true
    fun pull(handle: Long, pcm: ShortArray, valid: Int): Int
    fun sinkQueued(handle: Long, samples: Int)
    fun sinkRate(handle: Long, ppb: Long): Boolean
    fun renderExpiredSamples(handle: Long): Long
    fun stats(handle: Long): String
    fun stop(handle: Long): String
}

internal class NativeCallAudioPort(override val initialHandle: Long) : CallAudioPort {
    init { require(initialHandle != 0L) }
    override val dns = true
    override fun acquire() = initialHandle
    override fun push(handle: Long, pcm: ShortArray, valid: Int, readPosition: Long, frame: Long, ns: Long) =
        CallMediaJni.push(handle, pcm, valid, readPosition, frame, ns)
    override fun sourceRouteChanged(handle: Long, publishedNs: Long) =
        CallMediaJni.sourceRouteChanged(handle, publishedNs)
    override fun pull(handle: Long, pcm: ShortArray, valid: Int) = CallMediaJni.pull(handle, pcm, valid)
    override fun sinkQueued(handle: Long, samples: Int) = CallMediaJni.sinkQueued(handle, samples)
    override fun sinkRate(handle: Long, ppb: Long) = CallMediaJni.sinkRate(handle, ppb)
    override fun renderExpiredSamples(handle: Long) = CallMediaJni.renderExpiredSamples(handle)
    override fun stats(handle: Long) = CallMediaJni.stats(handle)
    override fun stop(handle: Long) = CallMediaJni.stop(handle)
}

package org.dmsg.client

/** Debug probe ABI. PCM calls only exchange bounded batches with native workers. */
object CallProbeJni {
    init { System.loadLibrary("dmsg_core") }

    external fun codecEvidence(): String
    external fun startLocal(): Long
    external fun startDns(fixturePath: String): Long
    external fun push(handle: Long, pcm: ShortArray, valid: Int): Boolean
    external fun pull(handle: Long, pcm: ShortArray, valid: Int): Int
    external fun sinkQueued(handle: Long, samples: Int)
    external fun stats(handle: Long): String
    external fun stop(handle: Long): String
}

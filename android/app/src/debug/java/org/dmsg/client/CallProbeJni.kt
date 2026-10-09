package org.dmsg.client

/** Debug probe ABI. PCM calls only exchange bounded batches with native workers. */
object CallProbeJni {
    init { System.loadLibrary("dmsg_core") }

    external fun codecEvidence(): String
    external fun startLocal(): Long
    external fun startDns(fixturePath: String): Long
    /** readPosition is the oldest mono sample since the single startRecording.
     * Timestamp fields are TIMEBASE_MONOTONIC; -1/-1 explicitly means unavailable.
     * Native drops 20 valid full batches to freeze the observed initial age floor,
     * then enforces an additional 40ms backlog budget. This is not acoustic calibration.
     * Rejected batches consume their source positions and must never be retried.
     */
    external fun push(handle: Long, pcm: ShortArray, valid: Int, readPosition: Long,
        timestampFrame: Long, timestampNs: Long): Boolean
    external fun pull(handle: Long, pcm: ShortArray, valid: Int): Int
    external fun sinkQueued(handle: Long, samples: Int)
    /** Actual sink frequency relative to nominal16k in parts per billion. */
    external fun sinkRate(handle: Long, ppb: Long): Boolean
    external fun stats(handle: Long): String
    external fun stop(handle: Long): String
}

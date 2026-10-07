package org.dmsg.client

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaRecorder
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.annotation.Keep
import androidx.core.content.ContextCompat
import java.util.concurrent.Executors
import kotlin.math.sqrt

/** JNI only: bounded PCM batches never cross UniFFI and never touch a file. */
@Keep
object VoiceCodecJni {
    @JvmStatic external fun nativeEncoderCreate(): Long
    @JvmStatic external fun nativeEncoderPush(handle: Long, pcm: ShortArray): Int
    @JvmStatic external fun nativeEncoderAtLimit(handle: Long): Boolean
    @JvmStatic external fun nativeEncoderSnapshot(handle: Long): ByteArray
    @JvmStatic external fun nativeEncoderFinish(handle: Long): ByteArray
    @JvmStatic external fun nativeEncoderCancel(handle: Long)
    @JvmStatic external fun nativeNoteSamples(bytes: ByteArray): Int
    @JvmStatic external fun nativeNoteWaveform(bytes: ByteArray): ByteArray
    @JvmStatic external fun nativeDecoderCreate(bytes: ByteArray): Long
    @JvmStatic external fun nativeDecoderRead(handle: Long, maxSamples: Int): ShortArray
    @JvmStatic external fun nativeDecoderSeek(handle: Long, sample: Int)
    @JvmStatic external fun nativeDecoderClose(handle: Long)
}

internal data class VoicePlayback(val key: VoiceKey? = null, val preview: Boolean = false,
    val playing: Boolean = false, val sample: Int = 0, val total: Int = 0)

/** Dedicated recorder and player threads. Main thread owns requests; native handles have one owner. */
internal class VoiceNoteAudio(context: Context) {
    companion object {
        const val RATE = 16_000
        const val BATCH = 1_600 // 100 ms maximum across JNI
        private var owner: VoiceNoteAudio? = null
        @Synchronized private fun claim(next: VoiceNoteAudio) {
            if (owner !== next) owner?.interruptAudio()
            owner = next
        }
    }
    private val app = context.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val recorderThread = Executors.newSingleThreadExecutor { Thread(it, "dmsg-voice-record").apply { isDaemon = true } }
    private val playerThread = Executors.newSingleThreadExecutor { Thread(it, "dmsg-voice-play").apply { isDaemon = true } }
    private val manager = app.getSystemService(AudioManager::class.java)
    private val attributes = AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).apply {
            if (Build.VERSION.SDK_INT >= 29) setAllowedCapturePolicy(AudioAttributes.ALLOW_CAPTURE_BY_NONE)
        }.build()
    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
        .setAudioAttributes(attributes).setOnAudioFocusChangeListener({ change ->
            if (change != AudioManager.AUDIOFOCUS_GAIN) interruptAudio()
        }, main).build()
    private val noisy = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) { interruptAudio() }
    }
    private var registered = false
    private var focusHeld = false
    @Volatile private var record: AudioRecord? = null
    @Volatile private var track: AudioTrack? = null
    @Volatile private var recording = false
    @Volatile private var recordAction = EndRecord.None
    @Volatile private var playerGeneration = 0L
    private var encoder = 0L // recorderThread only
    private var recordGeneration = 0L
    private var playBytes: ByteArray? = null
    var playback = VoicePlayback(); private set
    var onPlayback: ((VoicePlayback) -> Unit)? = null
    var onMeter: ((Int, Float) -> Unit)? = null
    var onNote: ((ByteArray, Int, ByteArray, Boolean) -> Unit)? = null
    var onError: (() -> Unit)? = null
    var onInterrupted: (() -> Unit)? = null
    private enum class EndRecord { None, Pause, Finish, Cancel }

    private fun acquire(): Boolean {
        claim(this)
        if (Build.VERSION.SDK_INT >= 29) manager.setAllowedCapturePolicy(AudioAttributes.ALLOW_CAPTURE_BY_NONE)
        if (manager.requestAudioFocus(focus) != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) return false
        focusHeld = true
        if (!registered) {
            if (Build.VERSION.SDK_INT >= 33) app.registerReceiver(noisy, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY), Context.RECEIVER_NOT_EXPORTED)
            else app.registerReceiver(noisy, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
            registered = true
        }
        return true
    }
    private fun release() {
        if (recording || playback.playing) return
        if (registered) { app.unregisterReceiver(noisy); registered = false }
        if (focusHeld) { manager.abandonAudioFocusRequest(focus); focusHeld = false }
    }
    fun start(resume: Boolean = false): Boolean {
        if (recording || ContextCompat.checkSelfPermission(app, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) return false
        stopPlayer()
        if (!acquire()) return false
        recording = true; recordAction = EndRecord.None
        val generation = ++recordGeneration
        recorderThread.execute {
            var failed = false
            try {
                System.loadLibrary("dmsg_core")
                if (!resume) { if (encoder != 0L) VoiceCodecJni.nativeEncoderCancel(encoder); encoder = VoiceCodecJni.nativeEncoderCreate() }
                check(encoder != 0L)
                val minimum = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
                check(minimum > 0)
                val input = AudioRecord(MediaRecorder.AudioSource.MIC, RATE, AudioFormat.CHANNEL_IN_MONO,
                    AudioFormat.ENCODING_PCM_16BIT, maxOf(minimum, BATCH * 4))
                record = input
                check(input.state == AudioRecord.STATE_INITIALIZED)
                input.startRecording()
                check(input.recordingState == AudioRecord.RECORDSTATE_RECORDING)
                val pcm = ShortArray(BATCH)
                try {
                    while (recordAction == EndRecord.None && generation == recordGeneration) {
                        val count = input.read(pcm, 0, BATCH, AudioRecord.READ_BLOCKING)
                        if (recordAction != EndRecord.None) break
                        check(count > 0 && count <= BATCH)
                        val batch = if (count == BATCH) pcm else pcm.copyOf(count)
                        val samples = try { VoiceCodecJni.nativeEncoderPush(encoder, batch) }
                            finally { if (batch !== pcm) batch.fill(0) }
                        val rms = sqrt((0 until count).sumOf { val v = pcm[it].toDouble() / 32768.0; v * v } / count).toFloat()
                        main.post { if (generation == recordGeneration) onMeter?.invoke(samples, rms) }
                        pcm.fill(0)
                        if (VoiceCodecJni.nativeEncoderAtLimit(encoder)) { recordAction = EndRecord.Finish; break }
                    }
                } finally { pcm.fill(0) }
            } catch (_: Exception) { failed = true; recordAction = EndRecord.Cancel }
              catch (_: LinkageError) { failed = true; recordAction = EndRecord.Cancel }
            finally {
                record?.let { runCatching { it.stop() }; it.release() }; record = null
                finishEncoder(recordAction, generation)
                main.post {
                    if (generation == recordGeneration) {
                        recording = false; release()
                        if (failed) onError?.invoke()
                    }
                }
            }
        }
        return true
    }
    private fun finishEncoder(action: EndRecord, generation: Long) {
        if (encoder == 0L) return
        try {
            if (action == EndRecord.Cancel) { VoiceCodecJni.nativeEncoderCancel(encoder); encoder = 0; return }
            val paused = action == EndRecord.Pause
            val bytes = if (paused) VoiceCodecJni.nativeEncoderSnapshot(encoder) else {
                val handle = encoder; encoder = 0; VoiceCodecJni.nativeEncoderFinish(handle)
            }
            check(bytes.size in 1..131_072)
            val samples = VoiceCodecJni.nativeNoteSamples(bytes)
            check(samples in 1..960_000)
            val waveform = VoiceCodecJni.nativeNoteWaveform(bytes)
            main.post { if (generation == recordGeneration) onNote?.invoke(bytes, samples, waveform, paused) else bytes.fill(0) }
        } catch (_: Exception) {
            if (encoder != 0L) { runCatching { VoiceCodecJni.nativeEncoderCancel(encoder) }; encoder = 0 }
            main.post { if (generation == recordGeneration) onError?.invoke() }
        }
    }
    fun pauseRecording() = endRecording(EndRecord.Pause)
    fun finishRecording() = endRecording(EndRecord.Finish)
    fun cancelRecording() = endRecording(EndRecord.Cancel)
    private fun endRecording(action: EndRecord) {
        recordAction = action
        if (recording) runCatching { record?.stop() }
        else {
            val generation = recordGeneration
            recorderThread.execute { finishEncoder(action, generation); main.post { release() } }
        }
    }
    private fun interruptAudio() {
        onInterrupted?.invoke()
        finishRecording(); stopPlayer()
    }
    fun play(bytes: ByteArray, key: VoiceKey? = null, preview: Boolean = false, from: Int = 0) {
        if (recording) return
        stopPlayer()
        if (!acquire()) { onError?.invoke(); return }
        playBytes?.fill(0); playBytes = bytes.copyOf()
        val note = requireNotNull(playBytes)
        val generation = ++playerGeneration
        playback = VoicePlayback(key, preview, true, from)
        onPlayback?.invoke(playback)
        playerThread.execute {
            var decoder = 0L
            var output: AudioTrack? = null
            var failed = false
            var total = 0
            var position = from
            try {
                System.loadLibrary("dmsg_core")
                check(note.size in 1..131_072)
                total = VoiceCodecJni.nativeNoteSamples(note)
                check(total in 1..960_000)
                position = from.coerceIn(0, total)
                decoder = VoiceCodecJni.nativeDecoderCreate(note); check(decoder != 0L)
                VoiceCodecJni.nativeDecoderSeek(decoder, position)
                val start = position
                val format = AudioFormat.Builder().setSampleRate(RATE).setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_MONO).build()
                val minimum = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_16BIT)
                check(minimum > 0)
                output = AudioTrack.Builder().setAudioAttributes(attributes).setAudioFormat(format)
                    .setTransferMode(AudioTrack.MODE_STREAM).setBufferSizeInBytes(maxOf(minimum, BATCH * 4)).build()
                val out = output
                check(out.state == AudioTrack.STATE_INITIALIZED)
                if (generation != playerGeneration) return@execute
                track = out; out.play()
                var written = 0
                var lastMeter = 0L
                while (generation == playerGeneration) {
                    val pcm = VoiceCodecJni.nativeDecoderRead(decoder, BATCH)
                    check(pcm.size <= BATCH)
                    if (pcm.isEmpty()) break
                    try {
                        var offset = 0
                        while (offset < pcm.size && generation == playerGeneration) {
                            val count = out.write(pcm, offset, pcm.size - offset, AudioTrack.WRITE_BLOCKING)
                            if (generation != playerGeneration) break
                            check(count > 0); offset += count; written += count
                        }
                    } finally { pcm.fill(0) }
                    position = (start + out.playbackHeadPosition).coerceIn(0, total)
                    val now = android.os.SystemClock.uptimeMillis()
                    if (now - lastMeter >= 100) {
                        lastMeter = now
                        postPlayback(generation, VoicePlayback(key, preview, true, position, total))
                    }
                }
                while (generation == playerGeneration && out.playbackHeadPosition < written) {
                    position = (start + out.playbackHeadPosition).coerceIn(0, total)
                    postPlayback(generation, VoicePlayback(key, preview, true, position, total)); Thread.sleep(100)
                }
                if (generation == playerGeneration) position = total
            } catch (_: Exception) { failed = generation == playerGeneration }
              catch (_: LinkageError) { failed = generation == playerGeneration }
            finally {
                output?.let { runCatching { it.stop() }; it.release() }
                if (generation == playerGeneration) track = null
                if (decoder != 0L) runCatching { VoiceCodecJni.nativeDecoderClose(decoder) }
                // A stopped worker owns its own copy, never erase a replacement worker's note.
                note.fill(0)
                main.post {
                    if (generation == playerGeneration) {
                        playback = VoicePlayback(key, preview, false, position, total); onPlayback?.invoke(playback)
                        release(); if (failed) onError?.invoke()
                    }
                }
            }
        }
    }
    private fun postPlayback(generation: Long, value: VoicePlayback) = main.post {
        if (generation == playerGeneration) { playback = value; onPlayback?.invoke(value) }
    }
    fun stopPlayer() {
        playerGeneration++; runCatching { track?.pause(); track?.flush() }; track = null
        playBytes?.fill(0); playBytes = null
        playback = playback.copy(playing = false); onPlayback?.invoke(playback)
        release()
    }
    fun seek(sample: Int, bytes: ByteArray, key: VoiceKey? = null, preview: Boolean = false) {
        if (playback.playing) play(bytes, key, preview, sample)
        else { playback = VoicePlayback(key, preview, false, sample, playback.total); onPlayback?.invoke(playback) }
    }
    fun background() { finishRecording(); stopPlayer() }
    fun close() { cancelRecording(); stopPlayer(); playBytes?.fill(0); playBytes = null; recorderThread.shutdown(); playerThread.shutdown() }
}

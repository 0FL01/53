package org.dmsg.client

import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec
import android.database.sqlite.SQLiteDatabase
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Test SQL writes use the current at-rest format; fixtures never require conversion. */
internal fun sealedFixtureValue(key: ByteArray, field: String, plain: ByteArray): ByteArray {
    require(key.size == 32)
    val nonce = ByteArray(12).also { SecureRandom().nextBytes(it) }
    val cipher = Cipher.getInstance("ChaCha20/Poly1305/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(nonce))
    cipher.updateAAD(field.toByteArray(Charsets.UTF_8))
    return "DMSG-S1".toByteArray(Charsets.US_ASCII) + nonce + cipher.doFinal(plain)
}

/** Strict schema9 incoming metadata fixture, not downloaded media or a network acceptance substitute. */
internal fun seedIncomingVoiceFixture(db: SQLiteDatabase, key: ByteArray, contactId: String,
    mid: ByteArray, sampleCount: Int, plainLen: Int, waveform: ByteArray, seq: Long = 1) {
    require(mid.size == 16 && waveform.size == 64 && sampleCount in 1..960_000 && plainLen in 1..122_880)
    val chunks = (plainLen + 8175) / 8176
    val bytes = plainLen + chunks * 16
    require(bytes <= 131_072)
    val blob = ByteArray(16) { 11 }
    val recipient = ByteArray(32) { 12 }
    // Core seals the complete strict E2E event, not just the 166-byte manifest body.
    val manifest = ByteBuffer.allocate(50 + 166).order(ByteOrder.BIG_ENDIAN)
        .put(1).put(4).put(mid).put(ByteArray(32) { 4 })
        .put(1).put(1).put(blob).put(ByteArray(32) { 13 }).put(ByteArray(8) { 14 }).put(recipient)
        .putInt(plainLen).putInt(bytes).putInt(sampleCount).put(waveform).array()
    val sealed = sealedFixtureValue(key, "voice_manifest", manifest)
    try {
        db.execSQL("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,text,media_manifest,local_timestamp_ms,server_seq,server_timestamp_ms) VALUES(?,?,?,'incoming','voice',NULL,?,?,?,?)",
            arrayOf(mid, ByteArray(32) { 2 }, contactId, sealed, seq, seq, seq))
        db.execSQL("INSERT INTO core_blob_transfers(local_id,blob_id,recipient_device,byte_len,chunk_count,upload_complete,downloaded,last_used_ms) VALUES((SELECT local_id FROM core_messages WHERE message_id=?),?,?,?,?,0,0,0)",
            arrayOf(mid, blob, recipient, bytes, chunks))
    } finally { manifest.fill(0); sealed.fill(0) }
}

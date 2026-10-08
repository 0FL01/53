package org.dmsg.client

import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.core.content.FileProvider
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.QRCodeWriter
import java.io.File
import java.io.InputStream
import java.util.UUID
import java.util.concurrent.Executors

internal object InvitationImages {
    // Separate from UI and the serialized DNS command queue. No secret persistence.
    private val worker = Executors.newSingleThreadExecutor { r -> Thread(r, "dmsg-invitation-image").also { it.isDaemon = true } }
    fun dispatch(work: () -> Unit) = worker.execute(work)
    const val MAX_BYTES = 8 * 1024 * 1024
    fun qr(token: String): Bitmap {
        check(InvitationInput.isCanonical(token))
        val matrix = QRCodeWriter().encode(token, BarcodeFormat.QR_CODE, 512, 512)
        val pixels = IntArray(512 * 512) { i -> if (matrix[i % 512, i / 512]) android.graphics.Color.BLACK else android.graphics.Color.WHITE }
        return try { Bitmap.createBitmap(512, 512, Bitmap.Config.ARGB_8888).apply {
            setPixels(pixels, 0, 512, 0, 0, 512, 512)
        } }
        finally { pixels.fill(0) }
    }
    fun decode(stream: InputStream): String {
        val encoded = ByteArray(MAX_BYTES + 1)
        var bitmap: Bitmap? = null
        var pixels: IntArray? = null
        try {
            var count = 0
            while (count < encoded.size) {
                val read = stream.read(encoded, count, encoded.size - count)
                if (read < 0) break
                if (read == 0) {
                    val next = stream.read(); if (next < 0) break
                    encoded[count++] = next.toByte()
                } else count += read
            }
            if (count !in 1..MAX_BYTES) throw DmsgError(R.string.invitation_image_invalid, ErrorKind.InvalidInput)
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(encoded, 0, count, bounds)
            val w = bounds.outWidth; val h = bounds.outHeight
            if (bounds.outMimeType !in setOf("image/png", "image/jpeg", "image/webp") ||
                w !in 1..8192 || h !in 1..8192 || w.toLong() * h > 32_000_000)
                throw DmsgError(R.string.invitation_image_invalid, ErrorKind.InvalidInput)
            var sample = 1
            while ((maxOf(w, h) + sample - 1) / sample > 2048 ||
                ((w + sample - 1) / sample).toLong() * ((h + sample - 1) / sample) > 4_000_000) sample *= 2
            bitmap = BitmapFactory.decodeByteArray(encoded, 0, count, BitmapFactory.Options().apply {
                inSampleSize = sample; inPreferredConfig = Bitmap.Config.ARGB_8888; inMutable = true
            }) ?: throw DmsgError(R.string.invitation_image_invalid, ErrorKind.InvalidInput)
            if (maxOf(bitmap.width, bitmap.height) > 2048 || bitmap.width.toLong() * bitmap.height > 4_000_000)
                throw DmsgError(R.string.invitation_image_invalid, ErrorKind.InvalidInput)
            pixels = IntArray(bitmap.width * bitmap.height)
            bitmap.getPixels(pixels, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
            val source = RGBLuminanceSource(bitmap.width, bitmap.height, pixels)
            return try { QRCodeReader().decode(BinaryBitmap(HybridBinarizer(source)),
                mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE))).text }
            finally { source.matrix.fill(0) }
        } finally {
            encoded.fill(0); pixels?.fill(0)
            bitmap?.let { if (!it.isRecycled) { it.eraseColor(0); it.recycle() } }
        }
    }
}

/** One narrow cache export. Pause does not remove the recipient's temporary read grant. */
internal object InvitationShare {
    const val MAX_AGE_MS = 15 * 60 * 1000L
    private fun directory(c: Context) = File(c.cacheDir, "invitation-share")
    fun cleanup(c: Context, all: Boolean = false) {
        val now = System.currentTimeMillis()
        directory(c).listFiles()?.forEach { if (all || now - it.lastModified() >= MAX_AGE_MS) it.delete() }
    }
    fun export(c: Context, bitmap: Bitmap): Uri {
        cleanup(c, true)
        val dir = directory(c); check(dir.isDirectory || dir.mkdirs())
        val file = File(dir, "${UUID.randomUUID()}.png")
        try {
            file.outputStream().use { check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)) }
            return FileProvider.getUriForFile(c, "${c.packageName}.invitation-share", file)
        } catch (e: Exception) { file.delete(); throw e }
    }
    fun intent(c: Context, uri: Uri): Intent = Intent(Intent.ACTION_SEND).apply {
        type = "image/png"
        putExtra(Intent.EXTRA_STREAM, uri)
        clipData = ClipData.newUri(c.contentResolver, c.getString(R.string.invitation_share_qr), uri)
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
}

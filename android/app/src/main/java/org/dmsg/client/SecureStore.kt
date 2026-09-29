package org.dmsg.client

import android.content.Context
import androidx.security.crypto.EncryptedFile
import androidx.security.crypto.MasterKeys
import java.io.File

/**
 * Keystore-wrap master key + EncryptedFile sealing (security-crypto 1.0.0:
 * MasterKeys.getOrCreate + Builder(File, Context, alias, Scheme)).
 *
 * Model (K4 audit):
 * - live DB stays in app-private storage (sandbox + 0600 semantics);
 *   the Keystore-held master key seals a copy (EncryptedFile) as the
 *   transferable backup and as the migration target;
 * - migration plaintext -> Keystore-wrap + wipe of the source bytes;
 * - reinstall without the sealed copy = identity loss (shown in UI, see
 *   strings.reinstall_loss). No silent recovery, no cloud backup of history.
 */
object SecureStore {
    fun legacyDb(c: Context): File = File(c.filesDir, "core.db")
    fun sealedDb(c: Context): File = File(c.filesDir, "core.db.sealed")

    private fun alias(c: Context): String =
        MasterKeys.getOrCreate(MasterKeys.AES256_GCM_SPEC)

    private fun sealed(c: Context, f: File): EncryptedFile =
        EncryptedFile.Builder(
            f, c, alias(c),
            EncryptedFile.FileEncryptionScheme.AES256_GCM_HKDF_4KB
        ).build()

    /** Seal current DB bytes into the wrapped file (migration / backup step). */
    @Throws(DmsgError::class)
    fun seal(c: Context) {
        val src = legacyDb(c)
        if (!src.exists()) throw DmsgError("no legacy db")
        val dst = sealedDb(c)
        try {
            src.inputStream().use { inp ->
                sealed(c, dst).openFileOutput().use { out -> inp.copyTo(out) }
            }
        } catch (e: Exception) {
            dst.delete()
            throw DmsgError("seal failed: ${e.message}")
        }
        wipe(src)
    }

    /** Restore sealed copy back to the live path (same device only). */
    @Throws(DmsgError::class)
    fun unseal(c: Context) {
        val src = sealedDb(c)
        if (!src.exists()) throw DmsgError("no sealed copy")
        try {
            sealed(c, src).openFileInput().use { inp ->
                legacyDb(c).outputStream().use { out -> inp.copyTo(out) }
            }
        } catch (e: Exception) {
            throw DmsgError("unseal failed: ${e.message}")
        }
    }

    /** Overwrite + delete (migration wipe of the plaintext source). */
    fun wipe(f: File) {
        try {
            if (f.exists()) {
                val n = f.length()
                f.outputStream().use { out ->
                    var left = n
                    val zero = ByteArray(8192)
                    while (left > 0) {
                        val w = minOf(left, zero.size.toLong()).toInt()
                        out.write(zero, 0, w)
                        left -= w
                    }
                }
            }
        } catch (_: Exception) {
        } finally {
            f.delete()
        }
    }

    /** Storage plan from file presence (facade mirrors Rust storage_plan). */
    fun plan(c: Context): String =
        Core.facade(c).storagePlan(legacyDb(c).exists(), sealedDb(c).exists())
}

package org.dmsg.client

import android.content.Context
import android.database.sqlite.SQLiteDatabase
import androidx.security.crypto.EncryptedFile
import androidx.security.crypto.MasterKeys
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.SecureRandom

/** Keystore-sealed data key; Rust encrypts secrets, ratchets and inbox in the live DB. */
object SecureStore {
    fun legacyDb(c: Context): File = File(c.filesDir, "core.db")
    /** Optional same-install snapshot. Not a transferable backup: Keystore key is device-bound. */
    fun sealedDb(c: Context): File = File(c.filesDir, "core.db.sealed")
    private fun wrappedKey(c: Context): File = File(c.filesDir, "core.key.sealed")

    private fun sealed(c: Context, f: File): EncryptedFile = EncryptedFile.Builder(
        f, c, MasterKeys.getOrCreate(MasterKeys.AES256_GCM_SPEC),
        EncryptedFile.FileEncryptionScheme.AES256_GCM_HKDF_4KB
    ).build()

    private fun checkpoint(f: File) {
        SQLiteDatabase.openDatabase(f.absolutePath, null, SQLiteDatabase.OPEN_READWRITE).use { db ->
            db.rawQuery("PRAGMA wal_checkpoint(TRUNCATE)", null).use { cur ->
                if (!cur.moveToFirst() || cur.getInt(0) != 0) throw DmsgError("storage checkpoint busy", ErrorKind.Store)
            }
        }
    }

    /** Never generate a replacement key for an encrypted DB or orphaned snapshot. */
    fun key(c: Context): ByteArray = synchronized(Core.storeLock) {
        val file = wrappedKey(c)
        if (file.exists()) {
            try {
                sealed(c, file).openFileInput().use { inp ->
                    val bytes = inp.readBytes()
                    if (bytes.size != 32) throw DmsgError("wrapped key length invalid", ErrorKind.StorageKeyLost)
                    return@synchronized bytes
                }
            } catch (_: Exception) {
                throw DmsgError("wrapped key unavailable: identity cannot be restored on this install", ErrorKind.StorageKeyLost)
            }
        }
        if (sealedDb(c).exists()) throw DmsgError("sealed copy without Keystore key: reinstall_loss", ErrorKind.StorageKeyLost)
        val db = legacyDb(c)
        if (db.exists()) {
            try {
                SQLiteDatabase.openDatabase(db.absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { sql ->
                    sql.rawQuery("SELECT name FROM sqlite_master WHERE type='table' AND name='core_storage'", null).use {
                        if (it.moveToFirst()) throw DmsgError("Keystore key missing: reinstall_loss", ErrorKind.StorageKeyLost)
                    }
                }
            } catch (e: DmsgError) {
                throw e
            } catch (_: Exception) {
                throw DmsgError("storage cannot be inspected safely", ErrorKind.Store)
            }
        }
        val fresh = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val staging = File(c.filesDir, "key-staging").also { it.mkdirs() }
        val tmp = File(staging, file.name) // same basename: EncryptedFile authenticates filename
        try {
            tmp.delete()
            sealed(c, tmp).openFileOutput().use { it.write(fresh) }
            Files.move(tmp.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE)
        } catch (_: Exception) {
            fresh.fill(0)
            throw DmsgError("could not create wrapped key", ErrorKind.Store)
        } finally {
            tmp.delete()
            staging.delete()
        }
        fresh
    }

    /** Migrate legacy columns first, checkpoint WAL, then keep an encrypted same-install snapshot. */
    fun seal(c: Context) = synchronized(Core.storeLock) {
        val db = legacyDb(c)
        if (!db.exists()) throw DmsgError("no live db", ErrorKind.LiveDatabaseMissing)
        val k = key(c)
        try {
            // Opens/migrates the DB and verifies the key; never copy an unencrypted legacy file.
            UniFfiFacade(db.absolutePath, k).account()
            checkpoint(db)
            val out = sealedDb(c)
            val staging = File(c.filesDir, "backup-staging").also { it.mkdirs() }
            val tmp = File(staging, out.name)
            try {
                tmp.delete()
                db.inputStream().use { input ->
                    sealed(c, tmp).openFileOutput().use { encrypted -> input.copyTo(encrypted) }
                }
                Files.move(tmp.toPath(), out.toPath(),
                    StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
            } catch (_: Exception) {
                throw DmsgError("storage backup failed; existing backup unchanged", ErrorKind.Store)
            } finally {
                tmp.delete()
                staging.delete()
            }
        } finally { k.fill(0) }
    }

    /** Restore only into an empty live path, verify under the original device-bound key. */
    fun unseal(c: Context) = synchronized(Core.storeLock) {
        val src = sealedDb(c)
        if (!src.exists()) throw DmsgError("no sealed copy", ErrorKind.SnapshotMissing)
        val live = legacyDb(c)
        if (live.exists()) throw DmsgError("live db already exists; refusing to overwrite identity", ErrorKind.LiveDatabaseExists)
        val k = key(c)
        val staging = File(c.filesDir, "restore-staging").also { it.mkdirs() }
        val tmp = File(staging, live.name)
        try {
            tmp.delete()
            sealed(c, src).openFileInput().use { encrypted ->
                tmp.outputStream().use { output -> encrypted.copyTo(output); output.fd.sync() }
            }
            UniFfiFacade(tmp.absolutePath, k).account()
            checkpoint(tmp)
            Files.move(tmp.toPath(), live.toPath(), StandardCopyOption.ATOMIC_MOVE)
        } catch (_: Exception) {
            throw DmsgError("unseal failed: sealed copy or Keystore key unavailable", ErrorKind.SnapshotInvalid)
        } finally {
            k.fill(0)
            wipe(tmp)
            staging.delete()
        }
    }

    /** Best effort on cache copies; filesystem wear-leveling prevents physical erase claims. */
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
        } finally { f.delete() }
    }

    fun plan(c: Context): String = when {
        wrappedKey(c).exists() -> "ready"
        sealedDb(c).exists() -> "reinstall_loss"
        legacyDb(c).exists() -> "migrate"
        else -> "fresh"
    }
}

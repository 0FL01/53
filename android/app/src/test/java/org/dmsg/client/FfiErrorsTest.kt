package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.FfiException

class FfiErrorsTest {
    @Test fun accountErrorsAreTypedAndHumanReadable() {
        val errors = listOf(
            FfiException.InvalidCredentials() to ErrorKind.InvalidCredentials,
            FfiException.LoginTaken() to ErrorKind.LoginTaken,
            FfiException.InviteRequired() to ErrorKind.InviteRequired,
            FfiException.InviteExpired() to ErrorKind.InviteExpired,
            FfiException.InviteRevoked() to ErrorKind.InviteRevoked,
            FfiException.InviteUsed() to ErrorKind.InviteUsed,
            FfiException.InviteLimit() to ErrorKind.InviteLimit,
            FfiException.AuthRateLimited() to ErrorKind.AuthRateLimited,
            FfiException.InvalidInput() to ErrorKind.InvalidInput
        )
        errors.forEach { (error, kind) ->
            assertEquals(kind, ffiError(error).kind)
            assertNotNull(ffiError(error).uiMessageRes)
        }
        assertEquals(errors.size, errors.map { humanErrorRes(ffiError(it.first)) }.distinct().size)
        assertEquals(R.string.error_identity_changed, humanErrorRes(ffiError(FfiException.IdentityMismatch())))
    }
    @Test fun nativeAndUnexpectedPayloadsNeverReachUi() {
        val secret = "example-password-invitation"
        for (e in listOf(FfiException.Transport(secret), FfiException.BadQr(secret), FfiException.Store(secret),
            FfiException.Protocol(secret), FfiException.Server(secret))) {
            val error = ffiError(e)
            assertFalse(error.message.orEmpty().contains(secret))
            assertNotNull(error.uiMessageRes)
        }
        assertEquals(R.string.error_operation, humanErrorRes(IllegalStateException(secret)))
        assertEquals(R.string.error_operation, humanErrorRes(DmsgError(secret)))
    }
    @Test fun storageOutcomesUseTypedSafeMessages() {
        val fixture = "untrusted diagnostic payload"
        listOf(ErrorKind.StorageKeyLost to R.string.error_storage_key_lost,
            ErrorKind.Store to R.string.error_store,
            ErrorKind.LiveDatabaseExists to R.string.error_live_db_exists,
            ErrorKind.LiveDatabaseMissing to R.string.error_live_db_missing,
            ErrorKind.SnapshotMissing to R.string.error_snapshot_missing,
            ErrorKind.SnapshotRestoreRequired to R.string.error_restore_required,
            ErrorKind.SnapshotInvalid to R.string.error_snapshot_invalid).forEach { (kind, resource) ->
            assertEquals(resource, humanErrorRes(DmsgError(fixture, kind)))
        }
    }
    @Test fun messageActionErrorsAreStaticAndDistinct() {
        val changed = ffiError(FfiException.MessageChanged())
        val unavailable = ffiError(FfiException.MessageUnavailable())
        assertEquals(ErrorKind.MessageChanged, changed.kind)
        assertEquals(ErrorKind.MessageUnavailable, unavailable.kind)
        assertEquals(R.string.error_message_changed, humanErrorRes(changed))
        assertEquals(R.string.error_message_unavailable, humanErrorRes(unavailable))
        assertNotEquals(changed.uiMessageRes, unavailable.uiMessageRes)
    }
    @Test fun voiceErrorsKeepPreviewAndCodecFailuresTypedAndStatic() {
        val session = ffiError(FfiException.VoiceSessionRequired())
        val invalid = ffiError(FfiException.BadVoice())
        assertEquals(ErrorKind.VoiceSessionRequired, session.kind)
        assertEquals(ErrorKind.BadVoice, invalid.kind)
        assertEquals(R.string.voice_session_required, humanErrorRes(session))
        assertEquals(R.string.voice_error, humanErrorRes(invalid))
        assertEquals(R.string.error_operation, humanErrorRes(IllegalArgumentException("private PCM or codec diagnostics")))
    }
}

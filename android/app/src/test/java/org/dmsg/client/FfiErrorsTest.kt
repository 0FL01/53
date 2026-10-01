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
            FfiException.AuthRateLimited() to ErrorKind.AuthRateLimited,
            FfiException.InvalidInput() to ErrorKind.InvalidInput
        )
        errors.forEach { (error, kind) ->
            assertEquals(kind, ffiError(error).kind)
            assertTrue(ffiErrorMessage(error).isNotBlank())
        }
        assertEquals(errors.size, errors.map { ffiErrorMessage(it.first) }.distinct().size)
        assertTrue(ffiErrorMessage(FfiException.IdentityMismatch()).contains("СТОП"))
    }
    @Test fun nativeAndUnexpectedPayloadsNeverReachUi() {
        val secret = "example-password-invitation"
        for (e in listOf(FfiException.Transport(secret), FfiException.BadQr(secret), FfiException.Store(secret),
            FfiException.Protocol(secret), FfiException.Server(secret))) assertFalse(ffiErrorMessage(e).contains(secret))
        assertFalse(humanError(IllegalStateException(secret)).contains(secret))
    }
    @Test fun storageOutcomesUseTypedSafeMessages() {
        val fixture = "untrusted diagnostic payload"
        listOf(ErrorKind.StorageKeyLost, ErrorKind.Store, ErrorKind.LiveDatabaseExists,
            ErrorKind.LiveDatabaseMissing, ErrorKind.SnapshotMissing, ErrorKind.SnapshotRestoreRequired, ErrorKind.SnapshotInvalid).forEach { kind ->
            assertFalse(humanError(DmsgError(fixture, kind)).contains(fixture))
        }
        assertTrue(humanError(DmsgError(fixture, ErrorKind.StorageKeyLost)).contains("Keystore"))
    }
}

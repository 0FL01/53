package org.dmsg.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.dmsg_core.FfiException

class FfiErrorsTest {
    @Test fun payloadFreeErrorsAreExplicit() {
        val errors = listOf(
            FfiException.PinMismatch(), FfiException.NotEnrolled(),
            FfiException.UnknownContact(), FfiException.NotAccepted(),
            FfiException.Blocked(), FfiException.IdentityMismatch(),
            FfiException.NothingToConfirm(), FfiException.MissingKeys(),
            FfiException.NoPeerPrekeys(), FfiException.UploadRejected(),
            FfiException.Quota(), FfiException.Revoked(), FfiException.BadToken(),
            FfiException.Expired(), FfiException.BoundOther(), FfiException.Busy(),
            FfiException.BadText()
        )
        assertTrue(errors.all { it.message == "" })
        assertTrue(errors.all { ffiErrorMessage(it).isNotBlank() })
        assertEquals(errors.size, errors.map(::ffiErrorMessage).distinct().size)
        assertEquals("identity changed, sending stopped until confirm",
            ffiErrorMessage(FfiException.IdentityMismatch()))
        assertEquals("invite expired", ffiErrorMessage(FfiException.Expired()))
    }

    @Test fun payloadErrorsKeepCategoryAndReason() {
        assertEquals("transport: closed", ffiErrorMessage(FfiException.Transport("closed")))
        assertEquals("bad qr: truncated", ffiErrorMessage(FfiException.BadQr("truncated")))
        assertEquals("store: load olm", ffiErrorMessage(FfiException.Store("load olm")))
    }
}

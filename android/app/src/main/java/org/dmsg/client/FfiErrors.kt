package org.dmsg.client

import uniffi.dmsg_core.FfiException

/** UniFFI's payload-free exceptions have an empty message; map typed variants. */
internal fun ffiErrorMessage(error: FfiException): String = when (error) {
    is FfiException.BadArgs -> "bad args: ${error.v1}"
    is FfiException.BadQr -> "bad qr: ${error.v1}"
    is FfiException.PinMismatch -> "transport pin mismatch"
    is FfiException.NotEnrolled -> "not enrolled"
    is FfiException.UnknownContact -> "unknown contact"
    is FfiException.NotAccepted -> "contact not accepted"
    is FfiException.Blocked -> "contact blocked"
    is FfiException.IdentityMismatch -> "identity changed, sending stopped until confirm"
    is FfiException.NothingToConfirm -> "nothing to confirm"
    is FfiException.MissingKeys -> "contact has no keys (scan QR first)"
    is FfiException.NoPeerPrekeys -> "peer has no one-time keys"
    is FfiException.UploadRejected -> "server rejected prekey upload"
    is FfiException.Quota -> "server quota"
    is FfiException.Revoked -> "device revoked"
    is FfiException.BadToken -> "bad token"
    is FfiException.Expired -> "invite expired"
    is FfiException.BoundOther -> "token bound to other key"
    is FfiException.Busy -> "server busy, retry later"
    is FfiException.BadText -> "bad text (empty or over limit)"
    is FfiException.Transport -> "transport: ${error.v1}"
    is FfiException.Store -> "store: ${error.v1}"
    is FfiException.Crypto -> "crypto: ${error.v1}"
    is FfiException.Protocol -> "protocol: ${error.v1}"
    is FfiException.Server -> "server: ${error.v1}"
}

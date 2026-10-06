package org.dmsg.client

import uniffi.dmsg_core.FfiException
import android.content.res.Resources
import androidx.annotation.StringRes

/** Typed errors, static human messages: never reflect native payloads or credentials. */
internal fun ffiError(error: FfiException): DmsgError = when (error) {
    is FfiException.InvalidCredentials -> DmsgError(R.string.error_credentials, ErrorKind.InvalidCredentials)
    is FfiException.LoginTaken -> DmsgError(R.string.error_login_taken, ErrorKind.LoginTaken)
    is FfiException.InviteRequired -> DmsgError(R.string.error_invite_required, ErrorKind.InviteRequired)
    is FfiException.InviteExpired -> DmsgError(R.string.error_invite_expired, ErrorKind.InviteExpired)
    is FfiException.InviteRevoked -> DmsgError(R.string.error_invite_revoked, ErrorKind.InviteRevoked)
    is FfiException.InviteUsed -> DmsgError(R.string.error_invite_used, ErrorKind.InviteUsed)
    is FfiException.AuthRateLimited -> DmsgError(R.string.error_auth_rate, ErrorKind.AuthRateLimited)
    is FfiException.InvalidInput -> DmsgError(R.string.error_input, ErrorKind.InvalidInput)
    is FfiException.BadArgs -> DmsgError(R.string.error_arguments, ErrorKind.InvalidInput)
    is FfiException.BadQr -> DmsgError(R.string.error_bad_qr, ErrorKind.BadQr)
    is FfiException.PinMismatch -> DmsgError(R.string.error_pin, ErrorKind.PinMismatch)
    is FfiException.NotEnrolled -> DmsgError(R.string.error_sign_in_required, ErrorKind.NotAuthenticated)
    is FfiException.UnknownContact -> DmsgError(R.string.error_unknown_contact, ErrorKind.UnknownContact)
    is FfiException.NotAccepted -> DmsgError(R.string.error_not_accepted, ErrorKind.NotAccepted)
    is FfiException.Blocked -> DmsgError(R.string.error_blocked, ErrorKind.Blocked)
    is FfiException.IdentityMismatch -> DmsgError(R.string.error_identity_changed, ErrorKind.IdentityMismatch)
    is FfiException.NothingToConfirm -> DmsgError(R.string.error_no_key_to_confirm)
    is FfiException.MissingKeys -> DmsgError(R.string.error_missing_keys, ErrorKind.MissingKeys)
    is FfiException.NoPeerPrekeys -> DmsgError(R.string.error_no_prekeys)
    is FfiException.UploadRejected -> DmsgError(R.string.error_key_upload)
    is FfiException.Quota -> DmsgError(R.string.error_quota)
    is FfiException.Revoked -> DmsgError(R.string.error_revoked, ErrorKind.Revoked)
    is FfiException.Busy -> DmsgError(R.string.error_busy, ErrorKind.Busy)
    is FfiException.BadText -> DmsgError(R.string.error_bad_text, ErrorKind.BadText)
    is FfiException.Transport -> DmsgError(R.string.error_transport, ErrorKind.Transport)
    is FfiException.Store -> DmsgError(R.string.error_store, ErrorKind.Store)
    is FfiException.Crypto -> DmsgError(R.string.error_crypto, ErrorKind.Crypto)
    is FfiException.Protocol -> DmsgError(R.string.error_protocol, ErrorKind.Protocol)
    is FfiException.Server -> DmsgError(R.string.error_server_rejected)
}

/** Resolve at rendering time; internal/native diagnostic strings are never UI text. */
@StringRes internal fun humanErrorRes(error: Throwable): Int {
    if (error !is DmsgError) return R.string.error_operation
    return when (error.kind) {
        ErrorKind.StorageKeyLost -> R.string.error_storage_key_lost
        ErrorKind.SnapshotMissing -> R.string.error_snapshot_missing
        ErrorKind.LiveDatabaseExists -> R.string.error_live_db_exists
        ErrorKind.LiveDatabaseMissing -> R.string.error_live_db_missing
        ErrorKind.SnapshotRestoreRequired -> R.string.error_restore_required
        ErrorKind.SnapshotInvalid -> R.string.error_snapshot_invalid
        ErrorKind.Store -> R.string.error_store
        else -> error.uiMessageRes ?: R.string.error_operation
    }
}

internal fun humanError(resources: Resources, error: Throwable): String = resources.getString(humanErrorRes(error))

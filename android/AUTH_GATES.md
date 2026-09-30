# Unified account-auth Android gates

Compile only:

```sh
ANDROID_HOME="$HOME/Android/Sdk" ANDROID_SDK_ROOT="$HOME/Android/Sdk" \
  ./gradlew testDebugUnitTest assembleDebug assembleRelease
ANDROID_HOME="$HOME/Android/Sdk" ANDROID_SDK_ROOT="$HOME/Android/Sdk" \
  ./gradlew assembleDebug assembleDebugAndroidTest -PgateInstall=true
```

These commands do not install or run the APK. Native libraries must be rebuilt
against the current generated bindings before runtime testing. Host/JVM success
is not Android recursive-DNS acceptance.

## Operator fixtures

Runtime gates require the separate `org.dmsg.client.gate` package and explicit
selection of one `DeviceGatesTest#method`. Never clear/reinstall the main package.
Before any install, check both APK application IDs with `apkanalyzer manifest
application-id`: exactly `org.dmsg.client.gate` and `org.dmsg.client.gate.test`.
Prepare fresh disposable server accounts/invitations for signup and replacement.
Keep fixture files in the gate app's private `filesDir`, owner-only (0600), and
transfer their contents as files rather than instrumentation/ADB arguments,
clipboard contents or deep links. Tests consume/wipe private auth fixtures in
`finally`. Do not commit actual codes, passwords, invitations, domains or keys.

| File | Method | Properties |
|---|---|---|
| `gate-auth.json` | `signupPrivateInviteOnce` | `serverCode`, `login`, `password`, `invitation`; server must be `invite_only` |
| `gate-auth.json` | `signupPrivateDnsAccountOnlyInGatePackage` | `serverCode`, `login`, `password`, `expectedPolicy` (`open` or `invite_only`); `invitation` required only in `invite_only` |
| `gate-login.json` | `loginPrivateAccountReplacementOnlyInGatePackage` | `serverCode`, `login`, `password`, `expectedDevice` (64 lowercase hex), `confirmReplacement` (boolean), `contactId` (required for confirmation) |
| `gate-auth-error.json` | `rejectPrivateCredentialsOnlyInGatePackage` | `serverCode`, `action` (`login` or `signup`), `login`, `password`, `expectedError` (typed enum name), optional `invitation` |
| `gate-reopen.json` | `reopenPrivateDnsAccountOnlyInGatePackage` | `serverCode`; no password/invitation. Run after signup in another instrumentation process; consumes the private `gate-account.json` contact-ID record emitted by signup |
| `gate-ui.json` | `unifiedAuthUiOnlyInGatePackage` | `serverCode`, `login`, `password`, `contactId`, local `resolvers`; fresh `.gate`, fresh old-device fixture on the same disposable invite-only server |
| `gate-carrier.json` (optional) | `rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate` | Local `resolvers`; accompanies the two public negative profiles |
| `gate-dns-profile.qr` | `configureAndProbeDnsOnlyOnExistingAccount` | Public `dmsg://server/…` profile, same trusted profile as the already authenticated disposable account |
| `gate-wrong-pin.qr`, `gate-wrong-noise.qr` | `rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate` | Public malformed-trust profiles: wrong complete certificate, or wrong Noise key with valid carrier certificate |

Passwords are exact JSON string values: do not trim them. Invitations are
standalone canonical 43-character base64url strings, never a profile/QR URI.
`expectedError` names include `InvalidCredentials`, `LoginTaken`, `InviteRequired`,
`InviteExpired`, `InviteRevoked`, `InviteUsed`, `AuthRateLimited`, `InvalidInput`,
`Transport`, and `PinMismatch`. Form validation may reject malformed input before
the server; expired/revoked/used/conflicting fixtures must otherwise be valid.

For an isolated **local authoritative DNS** carrier, the signup, login, negative
auth and reopen JSON fixtures may contain `resolvers`: 1–8 canonical numeric
private/loopback IPv4 `address:port` strings, with ports 1024–65535. This option
is implemented only in `androidTest` and requires `.gate`. These gates use the
real encrypted UniFFI facade without active-network resolver refresh so the
synthetic fixture suffix reaches its local C carrier. Without this field they
use the normal active-network resolvers. There is no production resolver
fallback, pin bypass or credential argument. Local authoritative DNS/QUIC +
pin + Noise account-auth evidence is not recursive-DNS/deployment acceptance.

`unifiedAuthUiOnlyInGatePackage` uses `ActivityScenario`, actual view clicks and
the real native facade. Its test-only injected facade changes only the resolver
argument for local profile import; all trust/auth/storage operations remain
native. It checks multiline paste, offline preview/cancel/accept, invite-only
form visibility, secret-field clearing on action switch/submit, replacement
cancel and explicit confirm, dialog routing and authenticated Activity restart.
It uses the editor's real clipboard-paste action, restoring the previous clip
immediately. It does not simulate optical QR or recursive DNS.

With `confirmReplacement=false`, the login gate cancels and leaves the new
identity unauthenticated. With `true`, it confirms only the exact fixture old-key
challenge, verifies the contact ID, empty old history, and key-only reconnect.
A changed CAS challenge fails this gate; it must not be auto-confirmed. Verify
old-device revocation and peer STOP/confirm separately with the paired fixtures.

Existing contact, history, screen-off, process-death/ciphertext retry, permission,
FGS and Keystore-loss gates remain. Their facade is DNS-only. Synthetic storage
fixtures use the current schema, not an old-auth compatibility path.

## UI/runtime checklist

- No profile: paste multiline code or scan server QR; offline domain/certificate
  fingerprint preview, explicit accept, no account created by import.
- Profile without account: short DNS policy operation closes transport before
  typing. Login has no invitation. Signup shows invitation only in `invite_only`.
- Login/password validation: ASCII login 3–32; password 8–128 UTF-8 bytes,
  including leading/trailing spaces. Credentials are not in Android saved state,
  prefs, logs, clipboard output, intent extras, or URIs.
- Replacement warning names loss of old-device access and absence of old history.
  Cancel/back/lifecycle loss clears secret values; confirmation sends a second
  login with `expectedDevice`. Another challenge needs another prompt.
- Authenticated restart opens dialogs and uses saved device keys. FGS is
  user-enabled after authentication. DNS comes from the active network.
- Camera denial is visible and paste remains usable. Contact QR stays in the
  authenticated contact flow; onboarding rejects it. Dialog cards refresh key
  warnings; send remains STOP until explicit contact-key confirmation.

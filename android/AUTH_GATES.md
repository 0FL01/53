# Unified account-auth Android gates

Compile only:

```sh
env -i HOME="$HOME" PATH="$JAVA21_HOME/bin:/usr/local/bin:/usr/bin:/bin" \
  ANDROID_HOME="$HOME/Android/Sdk" ANDROID_SDK_ROOT="$HOME/Android/Sdk" \
  ./gradlew --no-daemon testDebugUnitTest assembleDebug assembleRelease \
  assembleDebugAndroidTest -PgateInstall=true
```

These commands do not install or run the APK. Native libraries must be rebuilt
against the current generated bindings before runtime testing. Host/JVM success
is not Android recursive-DNS acceptance.
`JAVA21_HOME` is an operator-selected JDK 21 path; children receive only the
listed environment. Do not print inherited environment or provider credentials.

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
| `gate-ui.json` | `unifiedAuthUiOnlyInGatePackage` | `serverCode`, `login`, `password`, `contactId`; optional local `resolvers`; fresh `.gate`, fresh old-device fixture on the same disposable invite-only server |
| `gate-carrier.json` (optional) | `rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate` | Local `resolvers`; accompanies the two public negative profiles |
| `gate-dns-profile.qr` | `configureAndProbeDnsOnlyOnExistingAccount` | Public `dmsg://server/…` profile, same trusted profile as the already authenticated disposable account |
| `gate-wrong-pin.qr`, `gate-wrong-noise.qr` | `rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate` | Public malformed-trust profiles: wrong complete certificate, or wrong Noise key with valid carrier certificate |
| `gate-peer-contact.qr` | `acceptPeerFromPrivateFile` | Disposable native peer's public contact QR; emits private `gate-peer-contact-id` selection |
| `gate-incoming.json` | `pairedIncomingHistoryUiOnlyInGatePackage` | `text`, exact native-peer message expected once; real DNS fetch then dedup, unread summary, local time, rendered incoming history/recreation |
| `gate-send.json` | `pairedQueuedSendUiOnlyInGatePackage` | `text`; real chat double-submit guard, cancellation of only this app's pending native carrier, durable queued history/draft proof; emits private `gate-queued-record` |
| `gate-queued-record` | `retryAndVerifyPreservedCiphertextForGate` | Generated message/account/inbox/hash record; run after actual gate process death; same ID/ciphertext becomes accepted |
| `gate-send.json`, `gate-delivered.json` | `pairedAcceptedHistoryUiOnlyInGatePackage`, `pairedDeliveredHistoryReopenUiOnlyInGatePackage` | Copy queued record to delivered fixture before retry; accepted UI **before** real peer fetch; delivered UI **after** peer fetch and repeat-zero; empty queue never implies delivery |
| `gate-trust.qr` | `requestedAndMissingKeysUiOnlyInGatePackage` | Another disposable peer's public QR, not already in contacts; real requested-with-keys and local missing-keys states render disabled send |
| `gate-layout.json` (optional) | `frontendCardsConnectionAndLargeTextOnlyInGatePackage` | `contactId` of another disposable contact if paired contact was irreversibly blocked; otherwise uses paired selection. Actual system font scale must be 2.0 |

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
the real native facade. Without `resolvers`, it uses production active-network
DNS. Its optional test-only local facade changes only the resolver argument for
local profile import; all trust/auth/storage operations remain native.
It checks multiline paste, offline preview/cancel/accept, invite-only
form visibility, secret-field clearing on action switch/submit, replacement
cancel and explicit confirm, dialog routing and authenticated Activity restart.
It uses the editor's real clipboard-paste action, restoring the previous clip
immediately. It does not simulate optical QR. Public recursive-DNS evidence
requires omitting every local resolver fixture and verifying the actual path.

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

## Actual device R20/R21 run — 2026-10-01

**27 distinct selected methods reached a final pass; 38 successful one-test
executions, zero skips.** Seven diagnostic attempts were corrected and rerun.
The current installed gated APK was read back and hash-matched to the build;
its ARM64 native library matched `app/build/nativeLibs`. Device API 35, core
schema 6, wire2, fresh server schema 5; 35 JVM tests and debug/release/test-APK
builds passed after the device-reproduced fixes.

Private sanitized evidence: `.local/frontend-gates/evidence.md`, with exact
method names/timings, artifact hashes, final server counts and cleanup results.
Private screenshots remain beside it; no actual profile/domain/resolver/ID or
screenshot is tracked. Each method used:

```sh
env -i HOME="$HOME" PATH="$ANDROID_SDK_ROOT/platform-tools:/usr/local/bin:/usr/bin:/bin" \
  adb -s "$GATE_SERIAL" shell am instrument -w -r -e class \
  "org.dmsg.client.DeviceGatesTest#$METHOD" \
  org.dmsg.client.gate.test/androidx.test.runner.AndroidJUnitRunner
```

The serial is selected privately by the operator. Preserve main-package UID,
version, first-install and last-update metadata before/after. Runtime fixtures
must already be private files; method selection contains no secret arguments.

### Ordered live pair

1. `signupPrivateDnsAccountOnlyInGatePackage` and
   `reopenPrivateDnsAccountOnlyInGatePackage`: invite-only signup, fresh schema,
   separate-process key resume, empty fetch/dedup.
2. `exportActiveNetworkResolversOnlyInGatePackage`: compare production selection
   with current `LinkProperties`. The host verified a reachable endpoint with
   RD/RA, positive public answer and no authoritative flag, then used it for the
   native C DNS/QUIC peer. No Android test resolver override was present.
3. `exportMyContactQrForGate`, `acceptPeerFromPrivateFile`, then
   `pairedIncomingHistoryUiOnlyInGatePackage`: actual native send, phone receives
   one then zero, exact plaintext, no skipped categories, summary/unread/local
   timestamp, actual incoming bubble and reopened history.
4. `pairedQueuedSendUiOnlyInGatePackage`, force-stop only `.gate`,
   `retryAndVerifyPreservedCiphertextForGate`,
   `pairedAcceptedHistoryUiOnlyInGatePackage`, real native fetch one then zero,
   `pairedDeliveredHistoryReopenUiOnlyInGatePackage`: exact rendered
   Queued → Accepted → Delivered, one durable row despite double click, cleared
   draft only after storage proof, same ciphertext through process death,
   delivered history retained after outbox removal. Native DB reopen also proved
   incoming/outgoing history and its own delivered state.
5. Disposable native password replacement/new key resume and old-device typed
   `Revoked`, then `pairedChangedIdentityStopConfirmUiOnlyInGatePackage`: actual
   mismatched fetch, STOP/send disabled, cancel preserves STOP, explicit confirm,
   receive one then zero. `pairedBlockConfirmationUiOnlyInGatePackage` verifies
   the irreversible positive path after the earlier cancellation check.

Both directions used public recursive DNS, certificate pin, Noise, wire2 and
Olm. ADB carried instrumentation/fixtures only; SSH carried administration only.
No USB/radio toggle or TCP message bridge was used.

### Frontend and isolated supplemental gates

- `frontendCardsConnectionAndLargeTextOnlyInGatePackage`: persisted alias/card,
  real my QR, economy toggles, actual manual DNS success with FGS off, immediate
  FGS start/stop with native stopped, dialogs, 200% system font, real IME,
  settled landscape frame, send target physically above keyboard, retained draft
  after rotation/background return. Device reproduction fixed a late-poll
  restart race in `DmsgService`; landscape IME overlap was fixed with
  `flagNoExtractUi` and compact typing layout. Dismissing IME restores navigation.
- `requestedAndMissingKeysUiOnlyInGatePackage`: actual local trust warnings and
  disabled send, with no demo API.
- `unifiedAuthUiOnlyInGatePackage`,
  `rejectPrivateCredentialsOnlyInGatePackage`,
  `rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate`: real recursive-DNS
  auth forms/replacement/secret clearing and negative password/carrier/Noise
  checks. Old disposable host device was revoked after form confirmation.
- Fresh `.gate`: `seedLargeChatOnlyInGatePackage`, then
  `textHistoryPagingAndDraftUiOnlyInGatePackage` validates 551 synthetic schema6
  incoming rows, chronological older paging, no duplicates, retained anchor/draft
  and rendered-only read anchors. This is local paging evidence.
- `migrateEncryptedReopenAndRestoreOnlyWithOriginalKey`,
  `sealSyntheticAccountBeforeResetOnlyInGatePackage`,
  `verifyFreshAfterClearDataOnlyInGatePackage`,
  `verifySealedSnapshotCannotRestoreAfterClearDataOnlyInGatePackage`,
  `verifyLostKeyUiOnlyInGatePackage`: only disposable storage. Clear `.gate`
  again after the fresh-key gate **before** injecting the original sealed DB for
  the lost-key check; otherwise the fixture no longer represents a missing key.
- `scannerErrorsAndProfileConfirmUiOnlyInGatePackage` uses denied CAMERA and
  actual paste controls; malformed codes, key-change confirm and retained draft
  on real transport failure pass. The operator must revoke CAMERA and set
  `user-set user-fixed` flags only on `.gate` before this gate. Also passed
  `invalidQrIsRejectedWithoutAddingContact` and
  `changedIdentityStopsSendUntilExplicitConfirm`.

Cleanup force-stopped/cleared only `.gate`, removed local credential/message/
invite/QR/key/DB/raw-log fixtures, restored font scale and preserved main-package
metadata exactly. No owned native-peer process remained. Final read-only server
snapshot: healthy, schema 5, `invite_only`, three owned disposable accounts,
five device records (two retired), mailbox/send failures zero; deployed server
and carrier image hashes matched the rollout record.

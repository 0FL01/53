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

### Confirmed server chronology (2026-10-07)

Use the current metadata-capable backend, generated bindings and arm64 native.
Run the seven one-QR executions below on two fresh `.gate` accounts first, then
select exactly one `org.dmsg.client.ChronologyGatesTest#METHOD` per process:

| Phone | Method | Evidence |
|---|---|---|
| B | `receiverSendsEarlierBeforeSenderFetch` | Earlier server acceptance while absent on A |
| A | `senderSendsLaterThenFetchesEarlierMessageAndReopensUi` | Later own send, then delayed peer fetch; opposite local-ingest order, server timeline correct; actual Chat adapter/time/stable IDs and recreation |
| B | `receiverFetchesLaterTextAndRetainsOrderThroughRetryAndReopen` | Identical four MID/seq/server-time tuples; retry/dedup/recreation unchanged |
| A and B | `reopenConfirmedTimelineOnFinalApk` | Final APK install-r without fixture reset; local confirmed history and real UI/recreation still correct |

Local clock corruption is SQL **only in the disposable history fixture**, not a
phone clock/system setting. Actual DNS text verifies the first three methods;
the final two reopen checks are local UI verification, not additional network
acceptance. Twelve executions including the seven one-QR gates PASS/zero skips
on API35 USB and API36 Wi-Fi ADB. Host live integration additionally covers
offline queued→confirmed, byte-identical retry/reopen and foreign/missing metadata
indistinguishability. Rust/JVM cover >600 rows, late inserts below the newest page,
pending movement and receive-between-bridge-pages ingestion watermarks.

After this pair sequence, on a **fresh `.gate` only**, select
`ChronologyGatesTest#exactRowAndRetainedRefreshOnlyInFreshGatePackage`.
It seeds synthetic current-schema history, seals it via the real store, checks
601 exact records and contact boundaries, then verifies the retained last row
changes Queued→Delivered/server order without duplication and survives recreation.
This final-native gate passed on the remaining API35 Moto, zero skips; it is local
native/UI evidence, not a server registration or another DNS delivery claim.

Main `install-r` on both phones verifies authenticated core6→7 with original
sealed history/local IDs/times, contacts/read cursors, account/identity, Olm/session
pickles, wrapped key and UID/first-install retained. Actual DNS saved-key resume
restores metadata for the five shared original events35–39: equal sequence/time,
unchanged history rows/statuses, actual before/after chat screenshots. Main FGS
restored. No old ACK/drop body restoration or TTL-deleted metadata fabrication.
Never downgrade to core6 after upgrade or reset data to make an install pass.
The final exact-row patch/APK was installed-r and DNS/UI-verified on Moto after
the user removed Pacman. Pacman retains the preceding tested chronology/schema7
build; installation of the final artifact there is not claimed. Both original
authenticated-upgrade and two-phone chronology proofs remain valid and preserved.

JVM57/debug/release/native and targeted localization lint PASS; EN/RU286-key
parity. The two previously ignored explicit native-host gates/full lint debt are
not reclassified as passed. Backend schema5/data/pins/volumes/carrier preserved;
wire2/SEND_ACK/FETCH layouts unchanged. Private evidence stays ignored in
`.local/chronology/`; contract: `docs/goals/2026-10-07-message-chronology.md`.

### One contact QR / incoming consent (2026-10-07)

Build with current generated bindings/arm64 native and a trusted public profile,
`-PgateInstall=true`. Two **disposable** `.gate` packages; FGS off, camera permission
granted to `.gate` only. Never run `connectedDebugAndroidTest` against Main.
Select one `org.dmsg.client.OneQrContactGatesTest#METHOD` with the exact-method
`am instrument -w -r -e class` command below, changing only class/method/serial.

Run in order, each as a new instrumentation process:

| Phone | Method | Fixture/evidence |
|---|---|---|
| A and B | `bootstrapDisposableAccountThroughDns` | Owner0600 `files/one-qr-auth.json`: login/password/invitation. Explicit DNS signup/prekey publish; wipes auth, emits own public `one-qr-public.txt` |
| A | `senderOneQrPreviewAddAndFirstText` | Copy **only B's public QR** to A `files/one-qr-peer.txt`. Real ScannerActivity decoder injection; preview/cancel no mutation; Add opens Chat without second Accept; first text accepted |
| B | `receiverRequestAndFirstTextRemainPendingAcrossProcess` | Incoming/server provenance, no reverse QR; received0/pending1/cursor0, no history before consent |
| B | `receiverAcceptFromDialogsAndReplyWithoutReverseQr` | Main incoming dialog → visible Profile Accept → Chat; deferred text once, server-sourced trust label; reply via composer/button |
| A | `senderReceivesReplyAndOriginalCiphertextBecomesDelivered` | Exactly one stored reply/new receive or durable replay after host interruption, original MID/cipher hash unchanged, Delivered, next fetch0 |
| B | `receiverReplyDeliveredAndHistorySurvivesReopen` | Original reply cipher unchanged, Delivered, history2/reopen/fetch0 |

**Verified:** seven physical executions PASS/zero skips on API35 USB and API36
Wi-Fi ADB using actual recursive DNS. This covers the real scanner Add dialog,
request/consent Activities, composer and native E2E path, **not optical camera
recognition**. Optical scanning remains user-owned. Live host tests additionally
cover request before recipient prekey publication, no plaintext first-send queue,
simultaneous initial Olm sessions/normal-message convergence/reopen, integrity
rollback, and terminal pre-block without QR.

Final JVM53/debug/release/test/native and targeted localization lint PASS; EN/RU
283-key parity retained. Full lint's previously documented unrelated debt is not
claimed fixed. Backend recreated alone with schema/data/pins/volumes and carrier
ID/PID preserved. Main install-r on both phones retained encrypted identity,
account, contacts/history, wrapped key, UID/first-install; installed APK hashes
match `53.apk`, actual DNS saved-key resume PASS, prior FGS restored. Five existing
pending messages recovered (not new test messages) and outgoing statuses became
Delivered. An older already-ACKed/drop event is not backfilled. Aggregate private
proofs: ignored `.local/one-qr/`; closure: goal document below. Own fixture apps,
invitations and device access are removed/revoked after proof capture.

`docs/goals/2026-10-07-one-qr-contacts.md` is the rollout contract.

### Invitation onboarding and trusted APK profile (2026-10-06)

Package one trusted **public** server profile (full carrier DER pin + Noise key +
domain) using an absolute file path, not its contents in argv/environment:

```sh
# With the clean SDK/JDK environment above:
./gradlew --no-daemon testDebugUnitTest assembleDebug assembleRelease \
  assembleDebugAndroidTest -PgateInstall=true \
  -PserverProfileFile=/absolute/path/to/trusted-public-server-code.txt

# Main, identity-preserving build/install, no reset-data:
python3 android/dev-install.py --serial "$MAIN_SERIAL" \
  --server-profile /absolute/path/to/trusted-public-server-code.txt

# Container build with the same public input:
sh deploy/build-apk.sh /absolute/path/to/trusted-public-server-code.txt
```

Runtime validates the asset with the existing native parser. Existing accounts/
profiles are never overwritten. Without this input the generated stale asset is
removed and manual server-preview/accept remains available. No deployed profile/
domain is committed. Invitations never establish trust in a server.

Camera/private-file imports accept only the canonical raw 43-character token;
files may end in LF/CRLF. No secret URI/deep link. Import selects Signup and shows
«Приглашение считано», not server validation. Login/password and explicit creation
remain required; TTL/revocation/one-use are server-side. SAF uses temporary read
access only. Buffers clear on cancellation/background/recreation/submit; scanner
handoff is process-local and one-shot, not secret Intent/state/preferences data.

Run one exact method against `.gate` only, FGS off:

```sh
adb -s "$GATE_SERIAL" shell am instrument -w -r -e class \
  'org.dmsg.client.InvitationOnboardingGatesTest#METHOD' \
  org.dmsg.client.gate.test/androidx.test.runner.AndroidJUnitRunner
```

| Method | Input / order | Evidence |
|---|---|---|
| `canonicalParserOnlyInGatePackage` | Synthetic data | Canonical token and bounded file parser |
| `scannerResultLifecycleOnlyInGatePackage` | Fresh gate with trusted profile | Real ScannerActivity launch/result using **injected synthetic decoder text**, Signup/memory/cancel; not optics |
| `privateFileImportSignupThroughRecursiveDns` | Fresh gate; owner0600 `files/gate-invite-auth.json`, exactly `login/password`, and `files/gate-invitation.txt` | Exact importer downstream of SAF, malformed/cancel/background/recreate clearing, explicit DNS signup/dialogs; consumes secrets and emits private account proof |
| `reopenImportedAccountWithSavedDeviceKey` | New process, previous proof; auth/token fixtures absent | Same profile/account, actual recursive-DNS key resume/fetch; consumes proof |

**Verified 2026-10-06:** all four selected USB methods PASS, no skips, physical
API35/ARM64. Signup used the production `53-1` PTY invitation, reopen no password.
Independent decoder verifies actual terminal glyphs; Docker re-render matches QR,
non-TTY exits2 before issuance. **Camera optics and system DocumentsUI picker are
not claimed tested:** optical scanning is user-owned. JVM51/Rust170, host debug/
release/test and container APK builds PASS. Main install retained encrypted
device/account/history/contacts and wrapped-key digests, UID and first-install
identity; installed APK/native/public asset verified, main actual DNS key resume
PASS. Own secret fixtures/QR captures and disposable gate apps removed. Ignored
aggregate proofs: `.local/invite-onboarding/`; goal:
`docs/goals/2026-10-06-invite-onboarding.md`.

### Simple authentication copy

Current auth-copy check (2026-10-06): select
`InvitationOnboardingGatesTest#simpleAuthLabelsAndSignupExampleOnlyInGatePackage`
using the same exact-method `.gate` command above, fresh unauthenticated gate.
Physical USB PASS/no skips: labels exactly «Логин»/«Пароль», no login hint on
Login, «Например, marina53» on Signup, cleared again on Login; local invalid
input shows an ordinary-language error and clears password, no account created.
JVM auth tests retain the existing accepted punctuation and exact UTF-8 byte
bounds; presentation does not change validation. That copy-only change preceded
the separate bilingual scope below; current checks use resources, not RU literals.

Clean JDK21 JVM52/debug/release/test builds and main export PASS. Final USB method
PASS/no skips after cancelling its synthetic Autofill context (no password-manager
save prompt left behind). Main install-r/no-reset startup Dialogs and actual DNS
key resume PASS; installed APK/native/public asset exact, before/after encrypted
identity/account/history/contacts/wrapped-key digests plus UID/first-install
unchanged. Scoped .gate apps and private diagnostic captures removed. No backend,
native, schema or pin changes; aggregate ignored proofs `.local/auth-copy/`.

### Automatic English/Russian UI

Current UI follows Android preferred locales: first supported EN/RU, English
fallback if neither is available. Full English defaults in `values/`, Russian
in `values-ru/` (276 matching translatable keys); IDs, domains, user text/aliases,
QR contents and logs are not translated. EN/RU resource packaging also filters
dependency translations: without this, AppCompat French assets caused `fr,ru`
to fall back to English instead of choosing RU. No app picker, language prefs,
`localeConfig`, schema/auth/transport changes or locale resolver.

From `android/`, with the usual clean JDK21/SDK allowlist environment:

```sh
./gradlew --no-daemon testDebugUnitTest assembleDebug assembleRelease assembleDebugAndroidTest -PgateInstall=true -PserverProfileFile="$PUBLIC_PROFILE_FILE"
./gradlew --no-daemon lintDebug --init-script localization-lint.init.gradle -PgateInstall=true -PserverProfileFile="$PUBLIC_PROFILE_FILE"
```

`PUBLIC_PROFILE_FILE` is the existing absolute **public** trusted profile input,
not an invitation/password. Scoped lint checks translations, positional formats,
hardcoded text and plural candidates, without altering the normal lint config.
Full `lintDebug` still fails on 14 pre-existing NewApi/camera-opt-in errors in
generated UniFFI/the unchanged theme/existing scanner callers; it is **not PASS**.
Those unrelated APIs/bindings are not edited or suppressed in this scope.

Select these exact single methods with the normal `.gate` instrumentation command:

| `LocaleGatesTest#…` | Prerequisite / primary evidence |
| --- | --- |
| `preferredLocalesAndSafeErrorsOnlyInGatePackage` | Configuration contexts: en/ru/fr/en,ru/ru,en/fr,ru; specific safe errors, password bounds, pin/Keystore/status/trust disclosure, no payload echo |
| `retainedSendStateRendersCurrentResourcesOnlyInGatePackage` | Retained send-state fixture renders RU/EN at display time; draft/uncertain and late Saved/Delivered transitions unchanged; not a network send gate |
| `authConfigurationClearsSecretsAndUpdatesLabelsOnlyInGatePackage` | API33+ disposable unauthenticated account; actual `.gate` OS app-locale RU→EN recreation clears password/invitation, no automatic submit; simple labels/signup-only example |
| `englishLargeFontDisclosuresOnlyInGatePackage` | Real compiled English Storage layout, Configuration font-scale200%; complete Keystore/password/history warnings wrap and fit; not a claim of the entire display/orientation matrix |
| `notificationLocaleRefreshKeepsWorkerAndCountOnlyInGatePackage` | API33+ authenticated `.gate`, POST_NOTIFICATIONS granted, FGS initially off; successful actual DNS poll, Economy, known **synthetic** runtime count7; OS locale EN→RU updates same channel/notification, same Thread/facts revision/Ready/account/importance |

Only disposable package app locales are changed, restored in finally and removed
by uninstall; device system/main app language is never overridden. Do not run
the whole suite against main. For the notification prerequisite use the existing
`InvitationOnboardingGatesTest#privateFileImportSignupThroughRecursiveDns`, then
`#reopenImportedAccountWithSavedDeviceKey` in separate processes with a fresh
owner-only invitation/login/password fixture. The stream input after SAF is
tested; DocumentsUI optics remain the prior user-owned scope.

**Verified 2026-10-06:** JVM52 and debug/release/test/main-export builds PASS;
scoped localization lint PASS. All five Locale methods and the two existing
actual recursive-DNS signup/key-resume methods PASS/no skips on USB API35.
Main APK installed-r without reset, automatic English Dialogs on the existing
English system, actual DNS key resume/localized diagnostics PASS. Exact installed
APK/native/public asset and preserved encrypted identity/account/history/contacts,
wrapped key, UID/first-install, system/main locale state checked. Installer's
first readiness wait timed out after successful install; subsequent actual
resource-ID/translated-title Dialogs and DNS checks passed without reinstall/reset.
Own invitation revoked/file removed, gate apps/overrides and raw secret/diagnostic
fixtures removed. Aggregate ignored proofs: `.local/english-locale/`; durable goal:
`docs/goals/2026-10-06-english-locale.md`.

### Explicit main development rollout (R22)

Main data loss is allowed only with explicit user consent. `adb install -r`
preserves an incompatible DB; a correct icon/label is not application acceptance.
The current APK rejects unsupported schemas without migration or automatic wipe.
From the repository root, with the normal clean SDK environment:

```sh
python3 android/dev-install.py --serial "$MAIN_SERIAL" --reset-data
```

This builds `53.apk`, verifies its exact main package/label, updates it, explicitly
clears only `org.dmsg.client` DB/history/Keystore identity, removes the three known
test duplicates, and launches/verifies Connection/Authentication/Dialogs. Without
`--reset-data` it retains data and refuses a Store startup error rather than
declaring label-only success. `--no-build` installs the existing root artifact.
Startup readiness alone does not prove DNS signup/messages.

`MainDevRolloutTest` is a separate, non-resetting manual main acceptance. Require
main target, `allowMainDevReset=true` and one exact class/method; wrong explicit
targets/missing consent fail before mutation. It is not part of the ordinary
`.gate` suite. Compile with `assembleDebugAndroidTest` without `gateInstall`,
install only the main test APK, never run connected tests against main:

```sh
adb -s "$MAIN_SERIAL" shell am instrument -w -r \
  -e allowMainDevReset true -e class \
  'org.dmsg.client.MainDevRolloutTest#METHOD' \
  org.dmsg.client.test/androidx.test.runner.AndroidJUnitRunner
```

Sequential methods (each requires its predecessor's private outputs):

| Method | App-private input | Acceptance |
|---|---|---|
| `freshMainOnboardingAndDnsSignup` | `files/dev-main-auth.json`, exactly `serverCode/login/password/invitation`, owner0400 | Real UI preview/accept/policy/signup/dialogs, schema6, cleared secrets, recreate/resume/key-only DNS; exports own contact QR/resolvers/proof |
| `acceptNativePeerAndReceive` | `dev-native-contact.qr`, `dev-incoming.txt`, owner0400/0600 | Actual peer QR/accept/alias, recursive receive1 then0/all skips0, exact protected history |
| `sendAndVerifyMainHistory` | `dev-outgoing.txt` plus previous peer/history fixtures | Actual chat send/double-submit→1 row/retry, exact Accepted before native ACK |
| `reopenedMainHasDeliveredHistory` | Previous outputs after native receive/ACK | New process/key resume, persistent Delivered, incoming1/outgoing1 rendered/recreated, real main dialogs/no Store |

Do not put fixture contents in arguments, logs or Git. Auth input is consumed;
other `dev-*` proof/QR/message files are removed after acceptance, preserving main
account/profile/history. Remove only `.test`, not the accepted main installation.

Verified 2026-10-01 on actual main API35/ARM64: **4 distinct methods, 5 successful
one-test executions, 0 skips** (final reopen repeated after test guard change).
Both native DNS directions received1 then0, skipped0/plaintext equality, exact
Accepted/Delivered and durable history. Actual production LinkProperties DNS and
reachable recursive endpoint verified; ADB/SSH were control only. Old main schema0
was deliberately lost; no core/schema/security validation changed. Final normal
installer without reset verified Dialogs; one `53`, main account retained, current
root APK/installed bytes matched. Evidence/screenshot privately in
`.local/main-dev-rollout/`; owner0400 `account.json` retains the dev login/password
for the user, not in APK/Git/diagnostics. JVM35/release/test/export builds green.

Ordinary destructive gates require the separate `org.dmsg.client.gate` package and
explicit selection of one `DeviceGatesTest#method`. Main reset is permitted only
for an explicitly authorised development rollout; see the main-dev section above.
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
use the normal active-network resolvers. Production now has a sequential Yandex
backup group after primary carrier failure (see the DNS section below); there is
no pin bypass or credential argument. Local authoritative DNS/QUIC +
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
  user-enabled after authentication. Primary DNS comes from the active network;
  backup is Yandex only after primary bootstrap failure.
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

## Physical Android pair (R8) — 2026-10-05

Two phones, both `.gate` + `.gate.test`, each its own serial; ADB controls
instrumentation/fixtures only. Every step is one `am instrument -e class
org.dmsg.client.DeviceGatesTest#METHOD` on the named phone (`A`/`B`). Public
contact QRs move phone→phone as app-private files (`run-as` pipe), never printed:

1. Both: `exportActiveNetworkResolversOnlyInGatePackage`, then
   `signupPrivateDnsAccountOnlyInGatePackage` (`gate-auth.json`, one fresh
   invitation per phone, no `resolvers` field).
2. Both `exportMyContactQrForGate`; A `gate-my-contact.qr` → B
   `gate-peer-contact.qr` and back; both `acceptPeerFromPrivateFile`.
3. A→B: A `sendSessionProbeForGate` (marker `gate-offline-request`, removed
   afterwards); B `pairedIncomingHistoryUiOnlyInGatePackage`.
4. B→A: B `pairedQueuedSendUiOnlyInGatePackage` → `gate-delivered.json`{mid}
   from `gate-queued-record` → new process
   `retryAndVerifyPreservedCiphertextForGate` →
   `pairedAcceptedHistoryUiOnlyInGatePackage`; A
   `pairedIncomingHistoryUiOnlyInGatePackage`; B
   `pairedDeliveredHistoryReopenUiOnlyInGatePackage`.
5. A `recordInboxBaselineForGate`, B probe, A `verifyNextDeliveryAndDedupForGate`.

Result: moto g54 (API35) ↔ A142P (API36), 18 one-test executions, 0 failures,
0 skips. Both directions received one then zero, all skip counters zero, exact
text; one row despite double click; Queued → Accepted → Delivered with the same
ciphertext hash across a new process. Both phones used the production
LinkProperties resolver of the shared Wi-Fi (recursive RD/RA, non-authoritative
answer verified), `adb reverse` empty, no TCP bridge or radio toggle. The B
process had already exited with instrumentation, so no live-PID SIGKILL is
claimed. Cleanup uninstalled `.gate`/`.gate.test` on both phones, removed both
consumed remote invitation files and local fixtures; main-package metadata
unchanged. Wi-Fi↔mobile, restricted egress and server restart remain separate.

## Physical pair process/server restart (R8) — 2026-10-06

Scope: only live-process death/queued retry and scoped messenger-server restart.
R9 long screen-off/Doze/Standby/background measurements are **deferred by user**.
Moto API35 (`A`) and A142P API36 (`B`), production LinkProperties DNS, no resolver
fixture override, radio toggle, TCP bridge or main-package mutation. Install only
the `.gate`/`.gate.test` APKs and select each method explicitly as above.

1. Provision both disposable accounts, exchange contact QRs privately and establish
   an actual A→B session using the physical-pair sequence above.
2. B `pairedQueuedSendUiOnlyInGatePackage`; save `gate-queued-record` privately and
   select its `mid` in `gate-delivered.json`. Start
   `holdQueuedProcessForSigkillGate` asynchronously. Wait for app-private
   `gate-kill-ready` (the actual PID); require equality with `pidof` **before**
   `run-as org.dmsg.client.gate kill -9 PID`. Require no live PID afterward and
   `stopped=false`. This deliberately aborts instrumentation and is **not** a
   passing test. No FGS is allowed to consume the queued row during the hold.
3. B `retryAndVerifyPreservedCiphertextForGate`, then
   `pairedAcceptedHistoryUiOnlyInGatePackage`; A
   `pairedIncomingHistoryUiOnlyInGatePackage`; B
   `pairedDeliveredHistoryReopenUiOnlyInGatePackage`. Result: account/inbox/mid/
   ciphertext hash retained, one durable outgoing row, received1 then0, skips0,
   exact Queued→Accepted→Delivered.
4. Both queue a distinct message with `pairedQueuedSendUiOnlyInGatePackage` and
   prepare their delivered/incoming fixtures. Start each
   `reconnectQueuedAfterServerRestartForGate` asynchronously; wait for both
   private `gate-restart-ready` PID markers, emitted only after native ready.
   Snapshot scoped images/mounts/ports, protected configuration/key fingerprints
   and other-container identities. After a backup, jointly recreate only dmsg53
   as described in `docs/deploy.md`; require healthy and a shared network namespace.
   Only then write `gate-restart-resume` on both phones. Collect both full
   instrumentation results, never discard their ADB readers prematurely.
5. The restart gate preserves account/pins/inbox/queued ciphertext across the
   handshake and uses normal reconnect commands for bounded eventual recovery
   (90 s), not `dnsStop`/resolver override/clear-data. Only typed Transport permits
   another attempt; each failure checks unchanged queued state and ciphertext.
   Count is private `gate-restart-recovery.json`. Both final gates passed; each
   observed **2 transient transport errors**, then ready/retry/Accepted.
6. Both `pairedAcceptedHistoryUiOnlyInGatePackage` before peer fetch; both
   `pairedIncomingHistoryUiOnlyInGatePackage` with the opposite message, then
   both `pairedDeliveredHistoryReopenUiOnlyInGatePackage`: one then zero received,
   all skipped0, one durable row per ID, persistent Delivered in a new process.

Evidence: **38 recorded successful one-test executions, 21 device/method pairs,
0 skips**; final selected results all green. Three recorded diagnostic test
failures were resolved (two initial signup Transport failures and one first-call
restart expectation). One earlier restart lost its host ADB result readers after
an SSH control failure and was repeated, not counted as PASS. SIGKILL hold is
also excluded. Private sanitized records: `.local/r8-reliability/acceptance.json`,
`runs.jsonl`, `kill-proof.json`, selected test logs/artifact hashes.

Build: clean JDK21 `testDebugUnitTest assembleDebug assembleRelease
assembleDebugAndroidTest -PgateInstall=true` green; JVM35/0 failures/errors/skips
(existing results up-to-date); final changed test APK rebuilt and installed on
both. APK/native bytes matched the build. No Rust/native/binding changes.

Initial signup failure diagnosis: a pre-existing carrier-only restart left msgd
in the old network namespace. Backup `snap-1791284239` then joint recreate restored
the real DNS path; profile/images unchanged. Carrier logs showed an earlier
SPCDNS `decode_rr_opt` assertion, not reproduced by this run and not repaired here.
Final acceptance joint recreate at 11:08:25 UTC: images, canonical mounts/ports,
configuration/key fingerprints and other-container identities unchanged. Healthy,
schema5/invite_only; final restart counters send_fail0/mbox_err0.

Cleanup verified: both `.gate`/`.gate.test` uninstalled, no gate PIDs, both consumed
invitation files and local credential/resolver/message/queue/raw diagnostic files
removed; main UID/version/install/update metadata unchanged on both. No R9, mobile,
restricted-egress, automatic carrier-crash healing or power-loss acceptance claim.

## Independent crash/recovery gates (physical pair, 2026-10-06)

Server topology: independent network namespaces, static private `DMSG_MSGD_IPV4`
backend (no published TCP7000), EDNS OPT patch in carrier image. Private harness:
`.local/r8b-independent/run.py` (control-master SSH wrapper, fish-safe `sh -c`).

Per cycle (c1 carrier crash, c2 msgd crash, c3 msgd-only recreate):
1. `cycle-fixtures <id>` pushes unique `gate-send.json`/`gate-incoming.json`
   texts to both phones; `cycle-queue` runs `pairedQueuedSendUiOnlyInGatePackage`
   on both and captures `gate-queued-record` (the reconnect gate deletes it).
2. Start `reconnectQueuedAfterServerRestartForGate` on both via detached ADB
   readers; wait for `gate-restart-ready` markers.
3. Fault, one service only: `kill -9 <carrier PID>` / `kill -9 <msgd PID>` /
   `docker compose ... up -d --force-recreate --no-deps msgd`. Verify Docker
   auto-restart (new PID, RestartCount+1, msgd healthy on the same static IP)
   and the peer container PID unchanged; namespaces must differ.
4. Write `gate-restart-resume` on both; each gate reconnects (bounded transient
   Transport errors ≤90 s, ciphertext/queue rechecked per failure) and retries
   to Accepted.
5. `cycle-verify A B` / `B A`: sender `pairedAcceptedHistoryUiOnlyInGatePackage`
   + receiver `pairedIncomingHistoryUiOnlyInGatePackage` (received1 then0,
   skips0) + sender `pairedDeliveredHistoryReopenUiOnlyInGatePackage`.

Evidence: three fault cycles, **17/17 device-method PASS, 0 skips**; peer
container untouched in every cycle (PID proof); both directions delivered exactly
once with persistent Delivered. Backup `snap-1791287385` before rollout;
healthy/schema5 after; secrets/volumes/pins/nft/tunnel unchanged. Cleanup: gate
packages uninstalled, remote invitation files removed, local credentials/DBs
deleted, main metadata unchanged. R9 remains deferred; mobile/restricted egress
not covered here.

## DNS fallback / network wake gates (2026-10-06)

Goal/status: `docs/goals/2026-10-06-dns-fallback-handoff.md`. Production policy:
current Network + primary LinkProperties DNS; separate native backup run with
`77.88.8.8:53`, `77.88.8.1:53` only after transient primary bootstrap exhaustion.
Ready is QUIC readiness, not Listening/ordinary hostname resolution. Pin/config
failures are terminal. Ready path stays until network change/loss; both groups
fail → backoff1/2/4/8/16/32/60s → primary again. Primary profile is never replaced
by the runtime backup choice. Worst bootstrap budget is 3s per resolver (8+2);
core endpoint wait35s includes setup/join margin.

Build gated app/test APK as above; verify IDs and native bytes, `adb install -r`
preserves an existing disposable account. Select exactly one method of
`org.dmsg.client.DnsNetworkGatesTest`; missing prerequisites fail, not PASS/skips.

| Method | Prerequisites / evidence |
|---|---|
| `actualYandexFallbackPreservesPrimaryAndAccount` | Existing `.gate` account/profile, FGS off. Test-only context-free real facade sets a local UDP sink as primary, production backup reaches actual Yandex DNS/QUIC + Noise key resume; repeated commands retain backup/profile/account. Restores primary in finally. Emits owner-only `gate-yandex-proof.json`: sink packet/payload count and **aggregate UID** byte deltas, not isolated handshake/cellular billing. |
| `retryQueuedPreservesCiphertextWithoutRadioChanges` | FGS off, real-peer `gate-queued-record` with mid/account/ciphertextHash. Local sink forces production Yandex fallback; retry twice retains exact mid/ciphertext, Accepted. Restores primary, emits `gate-retry-proof.json`. |
| `economyWakeAndStopWithoutRadioChanges` | Existing account, FGS off at start. Real economy worker; invokes the network-applied wake entrypoint without changing radio; new successful poll then Stop/late-wake→no late poll. Restores economy/FGS, emits `gate-wake-proof.json`. **Not a physical handoff claim.** |
| `wifiCellularHandoffAndEconomyWakePreserveQueuedCiphertext` | Independent **USB ADB verified on host**, cellular data already enabled, Wi-Fi initially active, real queued record, FGS off. Requires `-e radioControl usb` before mutation. Exercises foreground and economy Wi-Fi↔cellular transitions; account/mid/ciphertext retained, emits `gate-handoff-proof.json`. Wi-Fi/economy/FGS restored in finally. Argument is operator attestation, not automatic USB detection. |

**Never launch the radio method through the sole Wi-Fi ADB channel**, even though
finally restores Wi-Fi: the debugging listener may disappear/change port. The
initial interrupted attempt lost its only Wi-Fi control channel. USB was later
provided and verified on host (`get-devpath` begins `usb:`); the guarded attempt
kept control and restored Wi-Fi, but initially timed out45s waiting for cellular
default because per-SIM data was disabled. Host preflight must verify effective/per-SIM data enablement: global
`mobile_data=1` alone is insufficient (`mobile_data1=0`, `mobile_data2=0`, both
telephony `mIsDataEnabled=false`). Do not enable data/change APN/roaming without
permission. User later explicitly authorized LTE/Wi-Fi; selected-SIM data was
enabled and effective state verified. Current guarded method status: **PASS**,
foreground/economy both directions. VPN binding, route overrides or an added
control service are outside this goal.

Confirmed on Moto API35 + A142P API36: actual Yandex fallback/key resume on both;
Moto queued retry over Yandex → A142P receive1 then0/skips0 → persistent Delivered
on Moto in a new process, exact mid/ciphertext/double-submit1 row. Moto economy
wake poll completed in4729ms; no late poll after Stop. Fresh accounts/invites are
disposable; main account not reset. Main `53.apk` compatible update on Moto kept
schema6, wrapped key/device/account/history2/contacts bytes and opened Dialogs;
real UI DNS check used the same saved device key. Backend healthy5/invite_only,
no backend/pin/tunnel rollout. Private evidence: `.local/dns-handoff/`.
USB follow-up with only one phone used a disposable native peer over actual
recursive DNS, isolated `CARGO_TARGET_DIR`, not DirectTCP. The same queued
ciphertext from the aborted cellular-prerequisite gate subsequently passed actual
Yandex retry/repeat→Accepted, peer receive1 then0/all skips0/exact plaintext once,
phone persistent Delivered. Main identity/history/wrapped key/install metadata
unchanged; disposable gate packages, peer credentials/keys and owned invite files
cleaned. This is delivery evidence, **not LTE acceptance**.
Final authorized USB run **does establish Wi-Fi/LTE acceptance**: fresh queued
real-peer ciphertext, foreground off-FGS Wi-Fi→LTE→Wi-Fi, economy Wi-Fi→LTE→Wi-Fi
successful new polls2713ms/4932ms after new default-network availability (not
radio transition duration). Account/mid/ciphertext unchanged. Physical networks
had different DNS; same-DNS/new-Network and duplicate/late callback cases remain
JVM evidence. Native peer then received1/0 with all skips0 and exact plaintext
once in history; phone persistent Delivered/outbox removal/one history row passed.
Main installed APK/native equal build, identity/history/wrapped key/metadata
unchanged after cleanup AND real UI DNS saved-key check. Wi-Fi restored, selected
data remains enabled as authorized; APN/roaming/VPN/second-SIM unchanged. Private
final proof/logs: `.local/dns-handoff/usb/lte/`. Goal complete; R9 remains deferred.

Local native counter gate (all UDP endpoints local; real pinned C carrier):

```sh
env -i HOME="$HOME" PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
  DMSG_TEST_CARRIER="<pinned-meson-slipstream-server>" \
  cargo test -p dmsg-core --lib dns::native_tests -- --ignored --nocapture
```

Passed explicitly, alongside `slipstream-sys` native loopback gate. Primary Ready
backup0 packets; silent attempt10/2720 bytes TX,0 RX; failed cycle (1+1)
20/5440 TX,0 RX (DNS payload, no UDP/IP overhead). On production suffix the
phone's silent primary was10/2740 bytes. UID totals/refill/Noise are distinct and
documented in the goal; numbers are a single scenario, not a universal quota.
Clean workspace, generated host bindings, NDK r28c, JVM43, debug/release/test/main
export gates green. Physical USB Wi-Fi/LTE handoff passed; long Doze R9 deferred.

# Self-service invitation runtime gates

All methods are in `org.dmsg.client.SelfServiceInvitationGatesTest`. Build/install the
isolated `.gate` application and its instrumentation APK with `-PgateInstall=true`,
the current native library and generated bindings. Select **one exact method** using
`am instrument -w -e class org.dmsg.client.SelfServiceInvitationGatesTest#METHOD org.dmsg.client.gate.test/androidx.test.runner.AndroidJUnitRunner`.
Do not place secrets or DNS overrides in instrumentation arguments. FGS must be off.

For a controlled isolated LAN DNS carrier, an optional owner-only
`files/gate-self-network.json` contains only `{"resolvers":["numeric-address:port"]}`.
The gate configures trust solely from the packaged public asset, observes the real
Network, then applies this resolver override. This is actual UDP DNS transport,
not evidence of public delegation/default-network recursive resolution.

## Sender (authenticated `.gate`, current pinned DNS profile)

1. `senderIssueRecoverAndPrivateGrantThroughDns`: opens LIST without creating,
   double taps explicit Create, recreates, verifies the same id/phrase and own LIST.
   Writes owner-only `files/gate-self-issued.json`:
   `{"issueIdHex":"32 lowercase hex digits","phrase":"six canonical words"}`.
   Also writes owner-only `files/gate-self-invitation.png` by reading the actual
   narrow FileProvider PNG export and verifying its strict QR roundtrip.
   This is a private bearer fixture: transfer only through owned private files.
2. `senderPngProviderReadAfterPauseAndRevokeThroughDns`: requires that file and
    the same sender account. Presses the real Share QR button and selects the
    separate test APK's `53 invitation PNG gate` target in the system chooser.
    The independent UID reads/decodes the PNG after sender pause. The target's
    private `files/invitation-share-read-proof.json`
   contains only `{"readPngAfterPause":true}`. Then resumes, revokes through DNS,
   verifies terminal recovery, wipes the fixture. This method revokes its grant;
   obtain independent fresh grants for recipient methods first.

## Recipient (fresh `.gate` account, packaged trusted profile)

Provide owner-only bounded files under target `files/`:

- `gate-self-auth.json`: exactly `{"login":"fresh unique login","password":"valid password"}`.
- For `phraseSignupThroughDns`: `gate-self-phrase.txt`, canonical six-word phrase,
  at most 256 bytes. A final ASCII newline is accepted by the Rust normalizer.
- For `privatePngImportSignupThroughDns` / `systemPickerPngSignupThroughDns`: `gate-self-invitation.png`, actual QR PNG
  obtained from the sender export/host fixture, at most 8 MiB.

Each method checks Signup default, imports through the production handler, checks
no autosubmit and local-error foreground retention, explicitly submits through DNS,
then writes owner-only `gate-self-account.json` with public `contactId` and wipes
input files. Each signup consumes its invitation; use independent fresh recipients
and grants. Do not reset Main identity. The private PNG method exercises the bounded
stream decoder directly; it is not system-picker evidence.
`systemPickerPngSignupThroughDns` presses that production button and selects the
owned `Download/dmsg-self-invite-gate.png` through the real system picker, then
explicitly signs up. Supply the matching private PNG fixture and remove only that
owned Downloads copy after the method; no media/storage permission is granted.
Publish an ADB-supplied fixture with MediaStore `scan_file` before selection:
the owned row must have its actual size and `is_pending=0`. A pending MediaStore
row is not a selectable recipient image. Gesture injection uses a FINGER pointer,
not just SOURCE_TOUCHSCREEN; DocumentsUI distinguishes the tool type.

`reopenAccountAndOwnInvitationsThroughDns` runs in a new instrumentation process
after successful signup, with no credential fixture. The UI reaches dialogs by
saved device key, LIST resumes the same account and the immutable account remains
unchanged.

## Local isolated gates

- `signupDefaultLoginGuardsAndForegroundValidation`: fresh unauthenticated `.gate`
  with trusted profile/DNS policy; validates malformed phrase editing, Login input
  guards, selection retention over pause/refresh/recreation and secret wipe.
- `rasterBoundsRoundTripAndStrictInvitationRoute`: `.gate` with native parser;
  real Android PNG encode/decode, encoded bound, non-raster/contact/URL rejection.

No method claims optical camera acceptance. Use the actual camera and external
display for that evidence. Synthetic scanner-result lifecycle remains covered by
`InvitationOnboardingGatesTest#scannerResultLifecycleOnlyInGatePackage`.

PNG exports use only private `cache/invitation-share/` through the non-exported
`${applicationId}.invitation-share` provider. Pause wipes screen/bitmap but leaves
the explicit read grant intact. Cleanup is best effort after 15 minutes, expiry,
revoke, next Share or next open. Process death can leave a file until next open;
there is no absolute deletion deadline and no background worker.

## Acceptance, 2026-10-08

Moto g54 `ZY22JFJ5LP`, isolated `.gate`, current ARM64 native and trusted packaged
profile: PASS sender issue/double-tap/recreate recovery, production Share QR button
and independent-UID PNG read after pause, revoke/terminal recovery, uppercase phrase
signup, actual system image-picker PNG signup, new-process saved-key reopen,
Signup/Login/local-preflight guards and Android raster bounds/roundtrip.
`InvitationOnboardingGatesTest#scannerResultLifecycleOnlyInGatePackage` also PASS
with synthetic text and real activity-result lifecycle; this is not optical evidence.
Fresh native host signup from a phone-issued invitation PASS. Both directions used
an isolated LAN UDP authoritative carrier from the pinned C revision, not an
application TCP forward. Public delegation/default-network recursive acceptance,
a third-party messenger target, two physical phones and camera optics are not claimed.
Main UID/version/install timestamps and private DB/preferences hashes are checked
unchanged; the live deployment, transport pins and unrelated tunnel are not changed.

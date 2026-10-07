# Goal: one-QR contact requests

Status: active
Source: user approved «один QR → входящий запрос → принять → переписка в обе стороны», implementation, commit, deploy and APK update.
Last updated: 2026-10-07

## Objective / execution directive
Complete the frozen outcomes with the smallest compatible change. No reset, schema migration, transport rewrite or unrelated work.

## Required outcomes
- R1: Scan one contact QR, preview and press Add; persist/retry a real contact request without a second local approval. Receiver sees an incoming request without scanning back. Status: verified. Evidence: live `one_qr_contacts`, physical Scanner preview/cancel/Add and receiver pending gate; request also works before receiver prekey publication.
- R2: Explicit receiver acceptance enables bidirectional E2E text. Server-sourced keys are not described as QR-verified; established pins never silently change; block remains terminal. Status: verified. Evidence: real Main→Profile Accept→Chat/reply without reverse QR; existing pin/integrity tests, simultaneous sends/reopen and pre-block live tests.
- R3: First messages stay pending before acceptance, not ACK/drop as unknown; receive/retry is deduplicated and ciphertext unchanged. Status: verified. Evidence: pending received0/cursor0 before consent, exactly one receive afterward; original MIDs/cipher hashes unchanged, Delivered/reopen/fetch0 on both phones.
- R4: Compatible backend deploy and main APK update on both phones, identity/history retained, intended commit. Status: in_progress (only commit remains). Evidence: schema5/all-table/pin/volume/carrier ID+PID equality at backend recreate; final Main install-r exact encrypted data/key/UID/first-install preservation, installed hashes equal root APK, actual DNS saved-key resume and restored FGS both. No reset.

## Change envelope / constraints
Protocol additive authenticated bounded contact requests, existing server schema5 `contact_permissions`, existing core schema6 contact state strings; no new tables/migration or wire-version bump. Core/UniFFI/Android scanner/contact state/resources and targeted gates. EN/RU resource parity. Reuse existing DNS poll worker and pinned Noise transport. No new dependencies/services. Disposable `.gate` only for instrumentation; main databases/Keystore are preserved. Existing other tunnel and carrier unchanged.

## Decisions
- Receiver request keys originate from the pinned server directory, not an out-of-band QR. Record this provenance in contact state.
- Sender queue uses a durable contact state until request ACK. Server duplicates never reopen an accepted/blocked request. SEND also creates a missing request atomically for existing senders.
- Unknown/unaccepted text is not ACKed; it stays in the existing bounded/TTL mailbox. Blocked drop policy stays explicit. Previously dropped/ACKed messages are not silently replayed by resetting cursors.

## Current checkpoint
All behavior/build/runtime/deployment checks are green; own disposable apps removed and fixture invitations revoked/device access blocked. Review/stage only intended source/tests/docs and create the requested commit. No implementation blocker remains.

## Evidence-driven envelope adjustment
- R2: both real phone contacts are now accepted; server has five pending events. Live reproduction `simultaneous_first_sends_and_reopen_preserve_both_ratchets` fails at received0. Include `olm.rs` pickle helpers and receive/session persistence so bidirectional first sends do not deadlock. Use vodozemac session IDs/decrypt, bounded two sessions, disposable ratchet copies on failed authentication.

## Checkpoint evidence
- Crossed-init reproduction now passes, including normal-message convergence, bounded pickle/reopen, unchanged original ciphertext and integrity rollback. Existing schema6/legacy primary pickle retained; no migration/dependency/transport changes.
- Public-ID pre-block with no QR preserves terminal block/drop by associating only authenticated routing, not approving identity pins. Live test passes.
- Fresh receiver before first prekey publication reproduced `NoPeerPrekeys` on request. Requests now allow that public-metadata-only condition; text retains strict binding/claim and no plaintext first-send queue. Live test passes.
- `cargo build -p msgd -p dmsg-core`, regenerated UniFFI via `DMSG_GEN_BINDINGS=1 cargo test -p dmsg-core --test gen_bindings`, `cargo test --workspace`: PASS. Two pre-existing explicit native-carrier tests remain ignored; no new ignored cases.
- NDK r28c arm64, JVM53 (zero failures/errors/skips), debug/release/test APK, targeted localization lint: PASS. EN/RU parity283; unrelated full-lint debt unchanged.
- Seven physical exact-method executions (API35 USB/API36 Wi-Fi ADB) PASS/no skips; two final receipt/reopen methods also re-run on final native. Decoded QR injection covers real scanner UI, **not optics**. Full method order/fixtures: `android/AUTH_GATES.md`. Host shell timeout recovered through durable exact-once receipt verification, not skipping assertions.
- Both Main APKs match final `53.apk`, packaged native equals fresh build, public profile matches normalized trusted build input. Initial and final install snapshots preserve encrypted identity/account/contacts/history, wrapped key and app identity; actual recursive-DNS key resume/FGS both PASS. Private aggregate proof retained in ignored `.local/one-qr/`.
- Five previously pending real-phone messages delivered with original queues/keys retained: Moto incoming3 and Pacman incoming2, outgoing Delivered3 each. Earlier event already ACKed/drop is not backfilled. Server healthy, deployed intended sources match; other tunnel/carrier unchanged. Only scoped fixture devices/invitations/apps cleaned; no main credentials/private keys inspected or logged.

## Completion
Behavior, verification, compatible rollout and cleanup complete. Awaiting intended commit only.

# Goal: self-service one-time registration invitations

Status: active
Source: user requests of 2026-10-08: client-issued QR/code-phrase invitations, friendly UX; audited plan; "Делай копию плана в цель и итеративно реализовать и коммит пуш и тесты на телефоне". Fresh DB explicitly permitted to avoid migrations.
Last updated: 2026-10-08

## Objective
An authenticated Android user can invite a new account without an administrator, by showing/sharing a QR or copying a one-time phrase. A recipient can scan the QR, import the QR image on the same phone, or enter the phrase and explicitly sign up through the existing pinned DNS transport.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Record any proven external blocker and the smallest unlock; do not substitute synthetic evidence for physical evidence.

## Frozen Contract

### Required Outcomes
- R1: Preserve the audited, corrected plan as a durable repository goal.
  - Source: latest user instruction to copy the plan into a goal.
  - Acceptance: this document contains the finish line, boundaries, implementation steps and execution state.
  - Primary evidence: review of this document and the plan commit.
  - Status: verified
  - Evidence: corrected contract reviewed and recorded in this file; initial goal commit is the plan artifact.
- R2: Authenticated clients issue/manage single-use invitations; QR and phrase authorize the same signup.
  - Source: original request to replace routine administrator issuance with client invitations; approved audited plan.
  - Acceptance: issue, same-ID recovery, own active list and revoke work; signup consumes one invitation atomically; existing account/password/device/replacement behavior remains intact.
  - Primary evidence: protocol/server/core tests against a fresh server DB.
  - Status: pending
  - Evidence: pending.
- R3: Implement the agreed native sender/recipient UX, including actual QR image sharing and same-phone image import.
  - Source: QR sharing/registration UX discussion and latest instruction to implement after audit.
  - Acceptance: separate invitation screen, QR PNG Share, phrase Copy, revoke, camera/image/phrase signup inputs; no contact-QR confusion or automatic signup/server trust.
  - Primary evidence: Android JVM tests, APK/native builds and isolated device gates.
  - Status: pending
  - Evidence: pending.
- R4: Test the feature on Moto g54 serial ZY22JFJ5LP, distinguishing Android DNS, image transfer/import and optical camera evidence.
  - Source: phone provided for testing and latest explicit request for phone tests.
  - Acceptance: isolated .gate sender/recipient scenarios with real DNS and host peer; functional PNG transfer/import; camera optics only marked verified if physically observed.
  - Primary evidence: exact-method instrumentation/device observations recorded without secrets.
  - Status: pending
  - Evidence: pending.
- R5: Commit and push the plan, verified implementation and acceptance state.
  - Source: latest explicit "коммит пуш" instruction.
  - Acceptance: intended files committed in repository style and pushed to the configured remote; no secrets, generated binaries or unrelated work included.
  - Primary evidence: git status/diff/log and successful push with matching remote revision.
  - Status: pending
  - Evidence: pending.

### Constraints
- Preserve wire2 SIGNUP optional32, password/login validation, accepted immutable local account, pending-key retry, one active device, replacement CAS, strict E2Ev2 and existing contact QR meaning.
- Fresh server7 replaces migration work. Local core10 is unchanged. Older DBs are rejected without mutation/autowipe; fresh local DB does not clean server mailboxes.
- Keep separate messenger carrier/subdomain/pins/read-only key files and deployment topology. Do not touch the working unrelated tunnel.
- No invitation secret in logs, public profile/URI, navigation Intent, preferences or saved state. Explicit Copy/Share is an intentional export; QR Share may use a narrowly granted private-cache content URI.
- Build/diagnostic commands use clean allowlist environments without credentials. Bindings are generated, not edited manually.
- Phone acceptance uses .gate. No Main clear/uninstall or unrelated identity/Keystore reset is implied by fresh DB permission. No connectedDebugAndroidTest against Main.
- Preserve the pre-existing untracked design-source image; do not stage it.

### Non-goals
- Passwordless, account recovery, invite-based login/device rebind, automatic contact addition, multi-device/groups/federation.
- Secret deep links, public auth HTTP, QR/media SDK, custom gallery/crop editor, polling/background-worker framework, invite outbox or local secret database.
- Legacy migrations, mass deployment/phone reset, broad historical gate reruns or unrelated transport/crypto refactoring.

## Audited Minimal Plan

### 1. Protocol/server: one secret, not root + alias
- Six independent uniformly chosen words from pinned EFF Long Wordlist (7776 words), with source/license attribution. Fixed Latin words; repeated words permitted. Implementation pins diceware_wordlists1.2.3 and verifies its complete EFF dictionary fingerprint; no runtime fetch or copied incomplete list.
- `T = SHA256(b"dmsg signup phrase v1\0" || canonical_phrase)`. QR is existing canonical base64url-no-pad encoding of T; phrase input yields the same T. Self-service entropy is about 77.55 bits, not 256. Existing administrative random32 invitations remain unchanged.
- Common Rust parser: raw43 OR six dictionary words; bound raw input to 256 bytes before normalization; ASCII lowercase/ASCII whitespace only; no URI, Unicode folding, transliteration, fuzzy correction or autocorrection.
- Authenticated op pairs 46/47 ISSUE, 48/49 REVOKE, 50/51 LIST. ISSUE/REVOKE carry exact issue_id16; LIST is empty.
- ISSUE response: server_now:i64BE, issue_id16, created_at:i64BE, expires_at:i64BE, state:u8 (Active0/Used1/Revoked2/Expired3); only Active adds phrase_len:u8 + canonical phrase. QR token is derived, not redundantly returned.
- LIST response: now8 + count1 + at most8 metadata records (id16, created8, expires8); no secrets. REVOKE ACK is exactly empty. Strict opcode/payload bounds, returned ID consistency and no terminal secret suffix.
- Fresh schema7 adds nullable owner_user_id16 FK, issue_id16 and phrase_ascii to invites, all three NULL for operator rows or all present for client rows; UNIQUE(owner,id), active-owner index. No alias column/OR lookup/cross-namespace collision logic.
- Policy constants: TTL24h, maximum8 active/account. Separate issuance budget8/account and32/global per60s; charged only after a successful new commit, retries free; owner counter state bounded by the successful global budget rather than a generic2048-entry registry.
- Every management operation checks the current exact device/user association and active/unblocked status under DB lock. Mutation uses BEGIN IMMEDIATE; retry lookup precedes quota/rate/RNG; no await under DB locks.
- Same owner/id returns the original record, even terminal, without renewal or another issue. Management state priority used/revoked/expired/active; existing signup error/retry ordering remains unchanged. Terminal own revoke is no-op ACK; unknown/foreign ID is uniformly BAD.
- Account-owned invitations survive issuing device replacement/block. That device cannot make new commands; existing bearers expire, are used or explicitly revoked. Terminal records retained for durable idempotency; no GC framework.
- Use existing single-token signup lookup and atomic user/device/cursor/invite consume. Keep operator bootstrap for the first fresh invite_only account.

### 2. Core/UniFFI
- Add pure invitation_token(String)->raw43 and typed issue_invitation_dns(id16), list_invitations_dns(), revoke_invitation_dns(id16), using the existing Core/RESUME and pinned connection setup.
- Update signup_direct, signup_dns and existing client example parsers to the common input parser; strict admin/file parsers remain strict.
- Validate local accepted account before network; one RESUME per command; no on_reconnect/contact/prekey synchronization.
- Existing DNS bootstrap retains its separately bounded readiness wait; one30s deadline covers subsequent Noise connect + RESUME + command. Do not claim a30s end-to-end wall-clock bound over the synchronous DNS setup.
- Capture result and close the application stream on success/error/timeout; never stop the shared carrier for invitation-screen lifecycle.
- Use existing zeroize crate for owned secret buffers on error/cancellation and redacted Debug; no bespoke wipe/transport framework. Strict single application frame consumption where affected. JVM/UniFFI immutable copies cannot be promised perfect heap erasure.
- Invitation quota maps to a dedicated InviteLimit error, not mailbox quota. Generate Kotlin bindings normally.

### 3. Android sender and recipient
- Separate non-exported InvitationsActivity and small InvitationFlow, native Light/Square XML. Menu entry is distinct from My QR/contact inviteQr.
- LIST on open/resume/explicit refresh, no automatic issue. Explicit Create uses SecureRandom id16; single flight, same-ID reconciliation after uncertain result; only pending/selected IDs survive recreation.
- Show QR + six words + single-use/expiry, active metadata list, Share QR, Copy phrase and Revoke. Countdown uses server_now + elapsedRealtime. Unknown revoke disables retransmission until reconciliation; terminal clears secrets.
- FLAG_SECURE; wipe on-screen secret holders/text/bitmap on pause/stale callback. Do not call global dnsStop.
- Share PNG through a narrow FileProvider cache path with read-only temporary URI grant, no token/phrase in filename/extras/caption. Export survives sharesheet pause, has short best-effort cleanup, and may remain until next open after process death. No promise of erasing recipients' copies.
- Recipient fresh screen defaults once to Signup; Login is secondary. Signup invitation block precedes credentials: camera Scan QR, system image picker, Enter/Paste phrase; strict raw-file import remains tertiary.
- Decode selected raster image with existing BitmapFactory/ZXing, bounded stream/dimensions/sampled pixels, off UI/network command thread; no storage permission/persistable grant/new media dependency. Route only to invitation parser.
- Imported secret stays process-local via existing memory/ticket pattern, no autosubmit. Packaged trusted profile is reused; generic profile trust remains a separate explicit action.
- Prevalidate full login/password/policy before taking/clearing secrets; local editing errors preserve foreground invitation. Actual submit/background retains existing wiping. Signup passes any present invitation in Open or InviteOnly, requires it only in InviteOnly; Login never passes it.

### 4. Minimum evidence and delivery
- Host: parser/codec vectors, one-use QR/phrase equivalence, owner access, replay/restart, revoke/signup races, quota/rate accounting, schema rejection, operator compatibility and backup state retention; core lifecycle/error/account tests.
- Android JVM: flow single flight/reconciliation/fences, input/policy validation, QR raster round-trip, typed safe errors and trust/contact routing regressions. Build native arm64 then JVM/debug/release/.gate APKs.
- Reuse voice_gate_peer for invitation commands and private fixtures; no second harness. Exact-method .gate tests on ZY22JFJ5LP, real DNS in both directions with fresh host stores.
- PNG chooser target must actually read the file after sender pause; system picker imports it and signup succeeds. Synthetic injection is not camera optics. Physical optical test needs an available display/phone placement; record that dependency honestly if unavailable.
- Update only current protocol/server/auth-gate/runbook references. Historical PASS remains historical. Any dev backend cutover requires actual old DB/blob backup and matching old image; preserve pins/carrier. Main destructive cutover is separate from isolated feature acceptance.
- Iterate with meaningful plan/implementation/evidence commits and push; do not commit private fixtures or build outputs.

## Change Envelope
- Protocol/server: crates/protocol/src/{lib,auth,invitation}.rs, pinned wordlist attribution, manifests/lock; crates/server/src/{auth,db,main}.rs and directly affected tests.
- Core: crates/core/src/{auth,chat,ffi,transport}.rs, direct zeroize dependency, client examples/tests; generated Kotlin bindings.
- Android: facade/error consumers, Main/AuthFlow/InvitationInput/Scanner, new InvitationsActivity/InvitationFlow, bounded QR share/image helpers, manifest/FileProvider paths, native XML and EN/RU resources; directly related JVM/device tests.
- Documentation: this goal, current protocol/server/auth-gate/deploy/version references only. No secrets, runtime dumps, databases, APKs or PNG grants in Git.

## Current Checkpoint
- Closes: R1; starts R2/R3.
- Smallest next action: commit the corrected goal, then implement independent server/protocol, Core and Android slices against the frozen contract.
- Expected evidence: intended-only plan commit; targeted compile/tests and compatible generated binding surface.
- Stop or replan if: a required external permission/resource is observably unavailable; record the exact dependency, not a speculative blocker.

## Current State
- Resolved: four independent read-only audits completed; single-secret simplification and QR Share/gallery contradiction resolved.
- Last relevant evidence: workspace master at e7e2a3a; only pre-existing unrelated untracked design image; implementation/tests not started.
- Blocker: none established. Physical optical preparation and current dev runtime inventory not yet observed.
- Next: plan commit, parallel implementation, host checks, native/APK checks, isolated phone acceptance, intended-only commits/push.

## Material Decisions
- 2026-10-08: user explicitly permits fresh DB to avoid migrations and requests implementation, phone tests, commits and push.
- 2026-10-08: audit removes root+alias; QR/phrase encode one derived token. Keep routine operator bootstrap only for the first account.
- 2026-10-08: latest UX requires real QR-PNG Share and same-phone image import; text-only Share is insufficient.
- 2026-10-08: preserve existing DNS setup and bound new application operation, rather than adding a global DNS deadline redesign.
- 2026-10-08: dictionary supplied by pinned diceware_wordlists1.2.3 with upstream verification/attribution, rather than an unnecessary duplicate data file. Dedicated ERR_INVITE_LIMIT14 avoids mailbox quota ambiguity.

## Checkpoint History
- 2026-10-08: frozen corrected plan; no implementation or device actions yet.

## Completion
- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.

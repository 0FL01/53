# Goal: fresh-store message editing and deletion

Status: active
Source: user approved the audited message-actions plan and requested a goal copy, iterative implementation and commits; latest instruction permits wipe/install on Moto `ZY22JFJ5LP` only, expressly forbids touching Pacman.
Last updated: 2026-10-07

## Objective
Own sent text can be edited in place, hidden locally, or deleted for both through a confirmation modal. Deliver schema8 fresh-only without migration chains, one message/event table, and verified UI/native/DNS on the sole authorized Moto.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: direct-fresh encrypted schema8 with unified `core_messages` and no old-schema/plain-to-sealed conversions.
  - Source: user's fresh DB/refactoring instruction and approved plan step1.
  - Acceptance: no history/inbox/outbox tables or conversion paths; unsupported stores fail without mutation/reset; marker/guards protect sensitive values from first write; current-key/current-schema snapshot remains.
  - Primary evidence: storage unit tests and fresh encrypted integration fixtures.
  - Status: verified
  - Evidence: direct schema8/fresh/encryption/unsupported-store proofs PASS; conversion/old-table fixtures ported, not disabled. Moto `freshEncryptedReopenAndRestoreOnlyWithOriginalKey` PASS/zero skips: same-version reopen/snapshot/restore, live-path refusal and missing original key fail closed.
- R2: authenticated TEXT/EDIT/DELETE and transactional mutations using existing delivery.
  - Source: initial feature request; approved plan steps2–3.
  - Acceptance: own Accepted/Delivered targets only for remote operations; revision CAS, terminal delete, durable dedup/pending controls, unchanged original MID/local ID/seq/time/ciphertext, byte-identical retry; self-hide sends nothing. Own event MID is inside E2E; recipient binding frozen; controls need no chronology. No superseded/deleted plaintext through projections/reports.
  - Primary evidence: core mutation/crypto tests and two fresh cores through live msgd.
  - Status: verified
  - Evidence: protocol53 and core55 PASS; original two explicit native-host ignores unchanged. New crypto proofs cover own/CAS/no-op/queued eligibility, local hide without cancel, control-before-base/reorder, authenticated MID, terminal delete/reopen, TX rollback before ACK, final batch projection, byte-identical retry and recipient replacement STOP. Fresh encrypted live-msgd text/edit/self-hide/everyone-delete/reopen integration PASS.
- R3: Android message actions and correct visible history/state.
  - Source: approved plan step4 and approved schematic UI.
  - Acceptance: Edit/Save/Cancel with separate drafts; SelfOnly/Everyone modal defaults SelfOnly; no deleted placeholder; original and edit delivery distinct; static EN/RU errors; Copy/accessibility preserved; stable visible anchors, hidden-page continuation, filtered viewed/read anchors and existing ingestion watermark/reconciliation.
  - Primary evidence: JVM state/paging tests and exact isolated Moto UI gate.
  - Status: verified
  - Evidence: generated bindings/JVM74 PASS/zero skips, native exact601-row retained-refresh gate PASS. Moto actual touchscreen Copy/whole-message Edit, accessibility actions, Cancel/Save draft preservation/recreation, 200% font with landscape IME, self-default/cancel/no-placeholder and footer-menu offline everyone-delete PASS/zero skips. Reproduced no-op ticker destroying selection; rebind only changed rows/contact, then native selection-across-ticker and JVM74 PASS. Real peer confirms edited projection, self-only copy unchanged and terminal delete after new-process retry; controls received as zero new texts, no skips.
- R4: final APK and fresh authorized Moto rollout with real recursive DNS evidence.
  - Source: approved plan steps5–6, superseded pair-device scope by latest Moto-only instruction.
  - Acceptance: Rust/build/codegen/NDK/JVM/debug/release gates green; Moto UI→native→DNS→isolated peer text/edit/self-delete/everyone-delete/offline/reopen verified. Final main installed/reset only on `ZY22JFJ5LP`. No access to Pacman or its accounts/messages/identity. Startup alone is not acceptance.
  - Primary evidence: aggregate one-Moto+fresh isolated peer gates and final installed APK/native readback.
  - Status: in_progress
  - Evidence: Moto isolated fresh signup and three texts through actual active-network recursive DNS; real fresh host peer receives/decrypts3. Edit/self-hide/everyone-delete/offline/new-process retry and exact peer projections PASS. Main final fresh install/onboarding/readback remains.
- R5: scoped commits and closed current documentation/evidence.
  - Source: latest user instruction: «делай копию плана в цель и итеративно реализовать и коммит».
  - Acceptance: intended implementation/doc commits, current contracts/checklist updated, goal complete only after R1–R4; pre-existing user PNG unchanged; no push.
  - Primary evidence: Git diffs/status/log plus closure against this contract.
  - Status: in_progress
  - Evidence: `9d00f00` frozen goal, `0ecef99` strict codec, `d6b0904` unified fresh core/Android actions and fixture ports. Native action gate and selection fix checkpoint follow; user PNG unchanged, no push.

### Constraints
- C1: only Moto `ZY22JFJ5LP`; all adb/install/instrument/reset calls explicitly scoped with `-s`. Never target/discover/connect/operate Pacman. Use a fresh isolated host peer (or isolated package on Moto when sufficient), not a second physical phone.
- C2: preserve existing slipstream/carrier, pins, server secrets, auth/wire2/server5 and consent/identity STOP; no backend wipe required/authorized by inference.
- C3: secrets read-only files, never Git/argv/logs; clean allowlist build/diagnostic environment; no user plaintext in evidence. External disposable harness has separate CARGO_TARGET_DIR.
- C4: Kotlin UniFFI generated from host cdylib; destructive instrumentation only isolated `.gate` and explicitly selected methods. Finish gates before main installer removes gate packages.
- C5: SQLite transactions save ratchet/event/projection before ACK; protocol/store failures not suppressed; current outbox/worker reused.
- C6: own remote actions only Accepted/Delivered; self-hide queued does not cancel send and UI says so. Backend data/policy/accounts remain unless an independently necessary explicit operation is recorded.

### Non-goals
- Incoming Delete UI, queued editing/cancellation, age limits, deleted placeholders, undo/history/bulk actions, groups/media/multi-device/read receipts.
- Legacy decoder, CRDT/event framework, new queue/service/coordinator, capability negotiation, indefinite delivery, forensic erase or snapshot rollback protection.
- Repair of ordinary TEXT initial-chronology recovery after server TTL/GC, or unrelated global-vs-per-account GC cap. Preserve known original order; do not promise lost-original recovery or fabricate metadata.

## Approved Plan Copy

### 1. One fresh schema
Create encrypted schema8 directly on empty DB; merge `core_history/core_inbox/core_outbox` into `core_messages`; remove authenticated6→7, plaintext conversion and StoragePlan/MigrateLegacy mirrors. Keep same-current-schema/key snapshots. Missing key/unsupported DB fails closed, no automatic reset. Cleared sealed body is NULL and never passed to unseal. Do not remove server fresh initializer merely because its name is migrate.

### 2. Unified E2E pipeline
Strict envelope `version|kind|event_mid|sender_ed|body`; generate MID before encryption and verify inner/outer MID. TEXT is UTF8 1..4096; EDIT contains targetMID/positive revision/text; DELETE targetMID/positive revision, exact length; revision fits i64. No legacy decode. Freeze full recipient binding in one outgoing field; target binding/session must still match current pins.

One IMMEDIATE transaction validates target/revision, updates base, saves ratchet/control ciphertext/queued event. Original IDs/order/time/ciphertext not rewritten; original delivery still progresses. Incoming targets scoped by contact/sender/MID/kind=text. Higher edit wins, delete terminal, self-hide persists. Unknown original controls are dedup/pending journal in same table; only winning edit body retained, superseded/applied bodies cleared. Commit before ACK; final batch report excludes deleted text. Replay checked before optional metadata; only unseen TEXT needs correct chronology, controls use current FETCH seq only to ACK. Bad protocol is not missing metadata.

### 3. API and statuses
Local durable `edit_message(contact,local_id,expected_revision,text)` and `delete_message(contact,local_id,scope)`; no network/fallible postcommit row-read in success path. Preserve draft on conflict/unavailable target/session. Existing retry sends ciphertext afterwards. HistoryMessage adds revision/visibility/delete/latest-edit status; message_status exact for any outgoing event MID. Existing Outbox shows typed event and own state, including pending everyone-delete; no new status screen. Regenerate Kotlin.

### 4. Android actions/history
Own-message actions only. Edit eligible visible Accepted/Delivered TEXT via composer banner/Save/Cancel; normal and edit drafts separate in watcher/pause/render/recreation, normal restored after Save/Cancel. No-op validated against current revision. Conflict refresh retains edit draft, next Save explicit; unavailable/hidden target never becomes new Send. Worker-side unresolved write guard reused; durable completion separate from Activity rendering.

Delete modal SelfOnly/Everyone defaults SelfOnly, Cancel no-op; queued self-hide explicitly not cancellation. Bubble disappears without placeholder. Everyone-delete local removal separate from remote queued/delivered notice. Copy/system selection remains; selection/bubble/accessibility actions use stable whole-message ID; Save/Cancel reachable under IME/large font.

Raw TEXT rows/tombstones retained for paging/exact refresh/ingestion; adapter visible rows only. Save/restore visible-ID + pixel anchors, removed anchor fallback in timeline order. Hidden-only loading retains continuation and progresses in bounded chunks. Hidden/control rows excluded from previews/unread/viewed read anchors; tombstones still valid paging anchors; bridge cannot skip unseen ingest rows.

### 5. Fixtures and minimum gates
Port all direct SQL and plain→encrypted fixture setups to fresh encrypted, not merely migration-named tests. Preserve trust/crypto/snapshot/chronology assertions. Test storage, mutation ownership/CAS/atomicity/replay/order, history/drafts/hidden paging, live-msgd integration, Moto native/UI/recursive DNS and offline/reopen. Existing accounts/e2e/oneQR/chronology/JVM regressions retained. Targeted tests during development; final build msgd then workspace, explicit host codegen, NDK, JVM/debug/release/gated-test APK and scoped lint. Do not duplicate unrelated full Doze/network matrices.

### 6. Authorized cutover
Complete isolated gates before main installer removes fixtures; stop old participating Moto/host clients, never Pacman. Reset/install only Moto main, fresh onboarding/contact with fresh isolated peer, actual DNS text/edit/delete acceptance. Server data remains; ordinary signup or explicit LOGIN replacement if used. Update active schema/body/API/checklists; historical PASS not relabelled. Old snapshot restore is rollback, backend backup cannot restore lost client Keystore/history.

## Change Envelope
- Core/protocol store/secure/history/chat/codec/FFI and direct consumers/fixtures; Android facade/state/UI/storage/errors/resources/unit/instrumentation tests/generated bindings; active AGENTS/architecture/work/protocol/R18/auth/checklist docs.
- No carrier/vendor/.local slipstream edits, new dependencies/services/queues or protocol endpoints; private runtime proof/helpers in ignored .local, no tracked secrets/data/APKs.
- Commits explicitly requested; no push. Original user `53-opendesign/Image Sep 30, 2026, 09_24_12 AM.png` untouched.

## Current Checkpoint
- Closes: R4–R5
- Smallest next action: commit verified native action/selection checkpoint, build final main debug/release/artifact, then explicitly fresh-install/onboard only Moto with fresh isolated peer.
- Expected evidence: final main artifact/readback and actual native/DNS/actions evidence; current contracts/checklist and closure. No Pacman or old account/contact traffic.
- Stop or replan if: actual unresolved outcome cannot be satisfied in envelope; record concrete cause, not speculative hardening.

## Current State
- Resolved: R1–R3; sole Moto plus own fresh peer proves native UI/DNS and pending control delivery. No main reset/install yet; backend/schema/policy/old accounts untouched.
- Last relevant evidence: `cargo build -p msgd -p dmsg-core && cargo test --workspace` PASS (core55/protocol53; two pre-existing explicit native ignores), explicit host bindings generation PASS, NDKr28c arm64 PASS, JVM74/zero skips and gated Android test APK PASS. JDK17 is the existing ignored `.local/frontend-gates/jdk`; system Java25 is incompatible with this Gradle/Kotlin toolchain.
- Blocker: none.
- Next: final fresh main rollout and documentation closure.

## Material Decisions
- 2026-10-07: latest explicit device restriction overrides physical pair evidence; substitute fresh isolated peer while keeping actual Moto recursive-DNS acceptance. Do not interact with existing Pacman contact/account.
- 2026-10-07: fresh reset is explicit rollout, not error handling. Remote DB wipe unnecessary; current wire2/server5 remain.
- 2026-10-07: native selection test uses explicit touchscreen event source per Android Editor contract; pointer-class-only synthetic events select text but cannot open its toolbar. One real no-op refresh defect fixed without disabling the ticker or changing chronology/paging.

## Checkpoint History
- 2026-10-07: plan copied/frozen, R5 started; implementation not yet performed.
- 2026-10-07: strict codec checkpoint complete (53 protocol tests); core and Android source work underway. Native bindings/UI/device gates still pending.
- 2026-10-07: core/storage/actions and generated Android consumers implemented; mandatory Rust suite, JVM74 and test-APK compilation green. Physical snapshot/UI/DNS and final artifact remain unclaimed.
- 2026-10-07: R1–R3 native Moto gates and fresh recursive-DNS peer projections PASS/zero skips, including offline deletion/new process and persistent system selection. Temporary font/network settings restored; main rollout is the remaining runtime checkpoint.

## Completion
- Pending R1–R4 implementation/evidence and R5 closure; no new PASS claimed.

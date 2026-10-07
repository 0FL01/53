# Goal: Telegram-like confirmed message chronology

Status: blocked
Source: user: «Нужно сделать такую же логику как в телеграмм», then «реализовать, проверить на устройства и коммит»
Last updated: 2026-10-07

## Objective
Both phones render shared confirmed messages in the same server acceptance order,
independent of delayed fetch, local insertion IDs and phone clocks. Pending sends
remain visible locally. Preserve existing accounts, keys, ciphertext and history.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract
### Required Outcomes
- R1: confirmed timeline uses original durable server seq/time on both sides;
  pending rows become confirmed without a new bubble or re-encryption.
  - Source: approved Telegram-like model.
  - Acceptance: delayed/opposite-order receives and retry/reopen retain identical shared message order/time.
  - Primary evidence: live Rust integration plus two-phone recursive DNS gate.
  - Status: verified
  - Evidence: live `chronology` integration; delayed opposite-order receives on both recursive-DNS phones give identical four MID/seq/time tuples, retry/reopen and actual Chat labels stable. Existing five shared events35–39 restored to equal order/time.
- R2: preserve current installation data; explicitly scoped core schema6→7 upgrade.
  - Source: approved plan followed by implementation instruction.
  - Acceptance: transactional authenticated upgrade retains identity/account/ratchet/history/read cursor; old/future unsupported schemas and wrong keys still fail closed.
  - Primary evidence: migration unit tests and before/after main metadata/ciphertext digests.
  - Status: verified
  - Evidence: authenticated/unauthenticated/wrong-key/unsupported-schema unit gates; Main6→7 on both phones retains original sealed history, local IDs/time/status, contacts/read cursor, identity/account, Olm/session pickles, wrapped key, UID/first-install before network resume.
- R3: paginate/render confirmed order without breaking stable IDs, send reconciliation,
  unread/read cursor or scroll position; bounded restoration of retained legacy metadata.
  - Source: approved fix plan, existing R18/native UI invariants.
  - Acceptance: >one page, delayed insert and pending→confirmed transitions lose/duplicate no rows; local ingest API remains available for reconciliation.
  - Primary evidence: Rust/JVM targeted tests and physical current-chat observation.
   - Status: verified
   - Evidence: >600-row Rust/JVM pages, late insert, pending movement, whole-history burst and receive-between-bridge-pages tests; local-ingest API/TextSend unchanged. Final exact-row regression fixed without weakening cursor validation. Final-native Moto gate verifies all601 records/contact boundaries and retained last-row Queued→Delivered refresh/recreation, zero skips. Main original rows/seq/time remain unchanged.
- R4: deploy required compatible backend, update both main APKs without reset,
  verify actual DNS and create intended commit.
  - Source: latest implementation/device/commit request and approved model.
  - Primary evidence: preserved backend schema/data/pins/carrier, APK readback and physical gates, commit.
   - Status: blocked
   - Evidence: backend schema5/data/pins/volumes/carrier preserved, deployed source hashes match intended files; seven one-QR plus five chronology executions PASS/zero skips. Both Main6→7/actual DNS/original shared-history proofs PASS. Final exact-row/native/APK installed-r and DNS/UI-verified on Moto with keys/history preserved; owned fixtures cleaned. User removed Pacman before this final artifact, so its final installation remains externally unavailable; the requested implementation/evidence commit can be delivered independently.

### Constraints
- Wire2/server schema5, auth and E2E payload unchanged; add bounded authenticated metadata lookup rather than changing existing SEND_ACK/FETCH layouts.
- Core schema7 adds only history ordering metadata. The sole allowed old-schema upgrade is authenticated transactional 6→7, no generic migrations/wipe.
- Main packages/Keystore must not be cleared/uninstalled. Fixtures only in `.gate`.
- Existing ingestion IDs/local timestamps/read cursors remain unchanged; unknown expired legacy metadata is not fabricated.
- No transport/radio/system locale/pin rotation/dependencies or unrelated feature work.

## Change Envelope
- protocol bounded metadata opcode pair; server read-only scoped lookup of existing mailbox rows.
- core store/history/chat/FFI; generated Kotlin bindings; native UI timeline API/window.
- scoped tests, docs, ignored private device/deploy helpers; no tracked secrets/screenshots/raw user messages.

## Current Checkpoint
- Closes: R4
- External dependency: access to Pacman after user removal; `adb devices -l` lists only Moto.
- Independent work delivered: final native/APK, Moto readback/preservation/DNS/UI, full host checks, fixture cleanup and intended commit.
- Smallest unlock: Pacman accessible again for the same final `install-r` and saved-key/UI readback checks; no reset or new migration required.

## Current State
- Baseline: clean tracked tree ea1b417; prior one-QR objective complete.
- Cause: history insert uses device save/decrypt time; core and UI order by local ID.
- Implemented: bounded scoped opcodes35/36, immutable original seq/time, authenticated upgrade, separate timeline vs ingestion and retained-row/gap refresh.
- Relevant gates: full Rust workspace PASS (58 core unit passes; two pre-existing explicit native gates ignored), JVM57 and physical chronology/one-QR PASS. No Main reset or new cipher/ratchet caused by ordering.
- Original defect: both current Main chats now interleave the five shared events in equal seq35–39 order/time; screenshots remain private/ignored. Pacman's initial ready timeout was an observed lock screen, not an app/database failure; resumed UI/DNS checks passed after unlock.
- Final regression: fabricated local_id+1 was not a valid pagination anchor for the newest row or the next contact. Exact scoped SELECT shares the existing validated row decoder; host and final-native Moto gates are green.
- Final artifact: `53.apk` SHA256 `08309ee79fff41f0c6c648d001a723c17395db01b4c155e319a690cce7cb8970`, native SHA256 `17e2a95e664789fbcee72a9fa768749b9bb4697cfca3d7a8d681ae8e1f7f6c58`; Main Moto readback matches. Pacman retains its preceding schema7 chronology build, not this final artifact.

## Material Decisions
- Use existing global mailbox seq as confirmed order, created_at as server time. No new server counter/table or server schema upgrade.
- Add one bounded metadata request/response; normal legacy op layouts remain exact. Lookup is authenticated and scoped to the requester's own incoming/outgoing events.
- The approved implementation is the explicit exception permitting core6→7 upgrade; all other old versions still rejected. Already TTL-deleted or old ACK/drop message bodies are not recovered.
- Reconciliation uses append-only local history, not canonical rank. A reproduced receive-between-pages regression is fixed by keeping bridge pages from advancing the ingestion watermark past unseen rows.
- No unauthenticated plain6 upgrade/sealing is permitted; current-schema plain harness support is unchanged.
- User removed Pacman after the two-phone chronology/upgrade checks. Final exact-row patch is validated on the remaining Moto; do not touch the removed phone or claim its final-artifact installation.

## Completion
- R1–R3 verified; code, backend deployment, final Moto installation and requested commit delivered. Full rollout remains blocked solely by the unavailable Pacman final installation, not by a code/test failure.
- Final checks: `cargo build -p msgd -p dmsg-core`, generated Kotlin bindings, `cargo test --workspace`, NDKr28c arm64, JVM57/debug/release/test APK and targeted localization lint PASS. The two pre-existing explicit native-host ignores/full lint debt are unchanged.
- Evidence: preserved original two-phone/6→7 proofs and private final-active proof; final local native/UI gate PASS/zero skips. No main reset, secret/user-text in Git, pin/carrier change or extra dependency. Owned disposable fixture packages/access and private raw logs removed.

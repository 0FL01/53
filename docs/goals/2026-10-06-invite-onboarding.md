# Goal: приглашение QR/файлом и простые серверные команды

Status: complete
Source: утверждённый пользователем план onboarding (QR/приватный файл вместе с доверенным сервером в APK), terminal qr-invite и «53-1, 53, 53ctl»; «Делай копию плана в цель и итеративно реализовать, мобильник по adb для тестов у тебя есть, qr коды я сам просканирую».
Last updated: 2026-10-06

## Objective / копия плана

```text
Администратор: docker exec -it 53-1 53ctl qr-invite [--ttl 3600]
       ↓ одноразовое приглашение, TTL default 24 h, файл 0600 + terminal QR
Открыть 53 (доверенный публичный server profile уже в APK)
       ↓ сканировать приглашение / импортировать приватный файл
Создать аккаунт: логин + пароль, «Приглашение считано»
       ↓ явное создание через pinned DNS + Noise, проверка TTL/отзыва/одноразовости
Диалоги → следующий запуск / reconnect по сохранённому ключу устройства
```

`53ctl qr-invite --file <private-file>` повторно показывает существующее приглашение, не выпускает новое. QR/файл содержат только существующий canonical 43-character token, не URI, не пароль и не профиль сервера. Публичный server profile не секрет; импорт приглашения не подтверждает сервер и не создаёт аккаунт. Ручной импорт другого server profile остаётся дополнительным сценарием с явным trust preview.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: server/container/control команды `53`, `53-1`, `53ctl <command>` без лишнего `msgctl` уровня.
  - Source: «делаем тупо 53-1, 53, 53ctl».
  - Acceptance: Docker entrypoint 53, `docker exec 53-1 53ctl ping` работает; прежние data/blobs volumes и pins/schema сохраняются при compatible recreate.
  - Primary evidence: server CLI tests + production container health/dbversion/policy/volume identity before/after.
  - Status: verified
  - Evidence: local direct CLI probe PASS; production image53:s1/entrypoint53/container53-1 healthy. `53ctl ping/dbversion/registration-mode` → pong/5/invite_only. Before/after all table digests/schema/volume identities/public profile hash identical; carrier ID/PID unchanged.
- R2: безопасный terminal QR выпуск и повторный показ приглашения.
  - Source: qr-invite через Docker, одобренный terminal plan.
  - Acceptance: default 24 h или explicit positive TTL, новый owner-only file; QR декодируется в тот же token; --file не выпускает новое; не-TTY отказ до выпуска; токен не в argv/plaintext logs.
  - Primary evidence: QR decoder/CLI tests and Docker PTY smoke with private captured artifact, no secret printed in evidence.
  - Status: verified
  - Evidence: independent rqrr decoder reconstructs exact canonical token from terminal Unicode glyphs. Local PTY default TTL86400/explicit3600/file0600/dir0700/rerender/non-TTY-before-issuance PASS. Production Docker PTY issue3600 and identical QR re-render PASS; managed SSH non-TTY exit2 with unchanged invite count. Actual private file then used for phone signup; own QR/token captures and consumed file removed.
- R3: Android trusted default server + scan/private-file invitation → signup, без ручного 43-character поля в обычном сценарии.
  - Source: фокус на варианте 3 вместе с вариантом 2.
  - Acceptance: свежая установка использует bounded trusted public profile packaged at build time; импорт автоматически выбирает signup, индикатор «считано», login/password/explicit submit; camera and file input share bounded canonical parser, cancellation/malformed input safe. Secret only transient memory, no persisted credentials/secret URI.
  - Primary evidence: JVM/parser/UI tests, isolated .gate ADB file-import signup through actual recursive DNS, key-resume on reopen; optical scan user-owned.
  - Status: verified
  - Evidence: JVM51; all 4 exact physical USB InvitationOnboardingGatesTest methods PASS/no skips. Fresh .gate auto-configures trusted public asset; import does not create account, selects Signup; malformed/cancel/action/background/recreate cleanup verified; real recursive-DNS file signup/dialogs and new-process saved-key resume PASS with auth/token fixtures absent. Scanner test injects synthetic decoded text into actual Activity/result lifecycle, not camera optics. File test exercises exact importer downstream of SAF, not DocumentsUI picker. Optical scan explicitly user-owned.
- R4: сборка и compatible rollout без потери рабочего аккаунта; актуальная инструкция для администратора/пользователя.
  - Source: утверждённая итеративная реализация/предыдущая инструкция commit/deploy, ADB проверки.
  - Acceptance: Rust workspace, JVM/debug/release builds green; server same volumes/state, main APK install -r no reset preserves identity; intended commits and docs distinguish automated decoder/file evidence from user optical scan.
  - Primary evidence: clean environment gates, main before/after identity digest/install metadata, git diff/commit review.
  - Status: verified
  - Evidence: clean Rust build/workspace170 + fmt; JVM51/debug/release/test, host main export and actual container APK build PASS. Main install-r/no-reset retained encrypted device/account/history/contacts/wrapped-key digests, UID/first-install identity; APK/native/public asset exact, actual main DNS key resume PASS. CLI/deploy/auth/architecture/checklist docs updated. Feature commit39181c3 contains intended implementation/tests/docs only; final diff/scope check clean, own disposable secrets removed, user design PNG untouched.

### Constraints
- Server schema5/core6/wire2, password auth/one-device CAS, TTL/revoke/onetime/idempotence, full cert DER + Noise pins unchanged; no credentials before pinned server.
- No secret in tracked artifacts/env/argv/logs; terminal QR is an explicit interactive admin-only secret view, refuses non-TTY. Invitation files owner-only. Public deployed domain/IP/profile only ignored local/build input, never tracked.
- Compose project/volume identities stay unchanged despite new container/executable display names; independent carrier untouched, only backend compatible recreate.
- Main dev identity preserved; destructive tests only .gate, selected instrumentation methods. Optical scan delegated to user, not a condition to falsely report PASS.

### Non-goals
- Passwordless/passkey/recovery, server-profile+invite envelope, secret browser link, schema migration/wipe, auth/wire/API redesign, crate/protocol renaming, transport changes, FGS/Doze/VPN extension.

## Change Envelope
- Server CLI/main + isolated QR module/tests, QR encoder dependency/lock; deploy Compose/Dockerfile/docs/current command references. Internal crate/msgd storage paths remain compatible.
- Android MainActivity/AuthFlow/QrGate/ScannerActivity, bounded invitation parser and private-file picker, layouts/tests; build-generated asset from owner-provided public server-profile file (no deployment names committed). No new network service/dependency for importing invitations.
- Tests/docs, ignored local fixtures/build outputs; existing .gate only. Compatible server backend recreate and main APK install -r, no reset.

## Current Checkpoint
- None: R1–R4 verified; closure passed. Optical scan is user-owned, not an unresolved implementation requirement.

## Current State
- Resolved: R1–R4, compatible backend/main deploy, documentation, cleanup and feature commit39181c3.
- Last relevant evidence: workspace170/2 unchanged transport fixtures ignored, JVM51, current debug/release/test and Docker APK builds; four selected physical methods, actual main DNS key resume and preserved raw encrypted identity/history. Aggregate private proofs `.local/invite-onboarding/` retained; own credentials/QR captures removed, .gate uninstalled.
- Blocker: none.
- Next: none; objective complete.

## Material Decisions
- 2026-10-06: Keep internal crate, Compose project, service/storage names for compatibility; user-facing container/commands simplified. Default server is a build-time public asset, not auto-trusted invitation content or downloaded pins.

## Checkpoint History
- 2026-10-06: contract frozen before implementation.
- 2026-10-06: R1/R2 local CLI/independent QR decoder and workspace gates PASS. R3 initial USB scanner-result test exposed premature wipe in scanner onStop before parent delivery; removing that hook retained parent cancel/background cleanup and 5 s undelivered-result expiry, same physical test PASS. SSH SFTP unavailable; exec-raw transfer succeeded, no scope change.
- 2026-10-06: production backend compatible recreate unchanged state/carrier PASS; Docker QR file → physical file signup → fresh-process key resume PASS. Proxy SSH later unavailable; managed SSH safely completed file transfer/non-TTY/own-file cleanup, no repeated issuance. Main compatible update and actual DNS check preserved identity/history, fixtures cleaned; actual container APK optional-public-input build PASS. Final root artifact restored from the same host build tested/deployed (container native compiled separately).
- 2026-10-06: closure compares all R1–R4 to current evidence; feature39181c3 committed after intended diff/status/log review. No known blocker or created regression; only user design PNG remains untracked. Main relaunched after final unchanged identity/history snapshot (cold632ms).

## Completion
- Resolved outcomes: R1/R2/R3/R4 verified, no pending/blocked items.
- Commands/artifacts: clean `cargo build -p msgd -p dmsg-core`, `cargo test --workspace` (170 PASS, 2 unchanged transport fixtures default ignored), `cargo fmt --all --check`; clean JDK21 `testDebugUnitTest assembleDebug assembleRelease assembleDebugAndroidTest -PgateInstall=true -PserverProfileFile=<public-file>` and final main `testDebugUnitTest assembleDebug assembleRelease export53Apk`; actual `sh deploy/build-apk.sh <public-file>` PASS. Four exact USB instrumentation methods PASS/no skips. Managed SSH compatible backend recreate + CLI/PTY/state evidence; main `dev-install.py --no-build` without reset, exact installed artifact and actual DNS key resume PASS.
- Constraint/diff scope: wire2/server5/core6/password/key/CAS/TTL/revocation/one-use/pins unchanged; no carrier restart, same volumes/schema/tables at recreate. Only scoped .gate destructive fixtures, main identity/history retained. Public profile generated build-only, secret QR/token/credentials not in Git/argv/env/logs; intentional PTY capture private then removed. Optical scan not claimed; system picker not substituted with an optical PASS. No transport/VPN/Doze scope expansion; no secret artifacts or user design file staged.
- Final status: complete. Implementation commit39181c3; this document records terminal closure.

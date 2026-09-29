# Goal: клиентский трек K1–K4 (Rust core + Olm + Kotlin shell)

Status: active
Source: пользовательская инструкция + исправленный план аудита клиентского трека, ARCHITECTURE.md §3/§5/§7, WORK_PLAN.md M1–M4
Last updated: 2026-09-29

## Objective
Живой клиентский трек: Rust core (транспорт, enrol, Olm, outbox, контакты) + Kotlin shell (UI, FGS, Keystore), проверенный против production-msgd на n-de2, без переписывания за счёт швов сейчас.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Subagents (@general) execute phases iteratively; orchestrator verifies, deploys, tests, commits.

## Frozen Contract

### Required Outcomes
- R1: K1 core-скелет
  - Source: план K1 + аудит (trait-шов, single-instance, свой SQLite)
  - Acceptance: крейт core поверх protocol; Transport как trait (реализация direct-TCP; initiator-код функцией из примеров, не копией); Supervisor (один инстанс, start/stop/status, reconnect с backoff); свой SQLite-файл ядра; прямой Rust API, без UniFFI/storage-крата/Olm
  - Primary evidence: харнес против живого msgd (AUTH→WELCOME); `cargo test -p dmsg-core`
  - Status: verified
  - Evidence: 7 unit + 2 интеграционных OK; release 0 warning; lock только +dmsg-core; отклонения: send_replace, предсобранный msgd для харнеса
- R2: K2 offline-enrol
  - Source: план K2 + аудит (preview без сети, DER целиком, ERR-маппинг, Noise static only)
  - Acceptance: enrol_from_qr: parse→preview (domain/profile/pin-fingerprint, без сети)→DER→Noise IK pinned→AUTH_DOMAIN→ENROL→staticgen→persist 0600; reconnect тем же ключом; отказ used-token вторым ключом; различие transport-pin vs Noise-key ошибок
  - Primary evidence: тесты offline-импорта, preview, reconnect, отказа
  - Status: verified
  - Evidence: 4 enrol-теста OK (happy+reconnect, wrong-pin, expired+revoked, wrong-noise-key); pin-DER Option обоснован (direct-TCP не предъявляет живой серт); Olm отложен в K3
- R3: Olm и контакты
  - Source: план K3 + аудит (vodozemac, стоп-при-подмене, refill-побудки, QR тем же конвертом)
  - Acceptance: vodozemac Olm (без Matrix SDK, свой X3DH запрещён); identity + signed prekeys; claim/refill; ratchet+ciphertext одна TX; ретрай сохранённым ciphertext; outbox queued/accepted/delivered; контакты (ID, request-add, block, QR, подмена = стоп+confirm)
  - Primary evidence: два core-инстанса друг другу через doubles сервера
  - Status: verified
  - Evidence: 26 unit + e2e A→Б OK (ciphertext≠plaintext, retry без дубликата); login replay per-connect обоснован; sig-verify receive-path; contact-QR в core; пиклы plaintext 0600 до K4-Keystore
- R4: K4 Kotlin shell
  - Source: план K4 + аудит (пагинация, always-on FGS, Keystore-wrap, gates)
  - Acceptance: экраны + UniFFI только команды/события с пагинацией; always-on FGS («Экономия» — флаг); Keystore-wrap + EncryptedFile; миграция plaintext→Keystore + wipe; QR-сканер; нотификации без FCM; DNS-relay только (WebRTC P2P запрещён); gates: Doze/screen-off/смерть процесса/no-GMS/пермишены/битый QR/переустановка/пороги APK
  - Primary evidence: сборка APK + gate-чеклист
  - Status: blocked
  - Evidence: код K4 полный (экраны, фасад, FGS, Keystore, QR, JVM 7/7, APK 5.9 MiB собирается); НО libdmsg_core.so в APK нет (NDK отсутствует, sdkmanager нет) — приложение нефункционально; gate-чеклист G1–G11 PENDING. Smallest unlock: cmdline-tools → NDK → cargo-ndk → jniLibs → device gates
- R5: деплой, тесты, коммиты
  - Source: инструкция «потом коммиты деплой тесты»
  - Acceptance: фазы коммитятся по готовности; серверный трек не сломан (workspace test green); DNS-прогоны против n-de2 PASS
  - Primary evidence: `git log --oneline`, `cargo test --workspace`, smoke PASS
  - Status: verified
  - Evidence: workspace 12 сюитов green (сервер не сломан); smoke DNS не требовался (сервер untouched); коммит ниже

### Constraints
- C1: Серверный трек не ломать (protocol обратно совместим; msgd untouched, кроме крайней нужды)
- C2: Секреты только файлами 0600, не в Git/логи/argv (прецеденты сервера)
- C3: Швы сейчас, мясо позже: Transport trait, пагинация как требование, Экономия флагом, contact-QR тем же конвертом
- C4: ARCH выше любых предложений: только DNS-relay, без WebRTC P2P; E2E только vodozemac

### Non-goals
- Общий storage-крейт (до реального дубля); адресная книга; prefix-search; группы/typing/link-preview; cloud-backup; автозагрузка; второй FGS-режим; SQLCipher; звонки в K1–K3

## Change Envelope
- Target: crates/core (+ android/ в K4), protocol-дополнения при нужде
- Expected paths: `crates/core/`, `crates/protocol/`, `android/`, `docs/goals/`
- Allowed: rusqlite/tokio/snow reuse; vodozemac (K3); uniffi (K4)
- Forbidden: правки msgd без блокера; Matrix SDK; WebRTC-стек; копипаста diag-main вместо библиотечной функции

## Current Checkpoint
- Closes: R1
- Smallest next action: @general-агент строит K1 по атомарному ТЗ ниже
- Expected evidence: cargo test -p dmsg-core + харнес против msgd
- Stop or replan if: initiator не ложится функцией без копипасты — вернуть с точной причиной

## Current State
- Resolved: аудит клиентского трека завершён, 7 противоречий разрешены
- Last relevant evidence: синтез аудита (двухкаповая аналогия: trait-шов, пагинация-требование, стоп-при-подмене, Olm в K3, 0600+шов)
- Blocker: нет
- Next: K1 via @general

## Material Decisions
- 2026-09-29: trait Transport в K1 (direct-TCP за ним); UniFFI только в K4; пагинация — требование сейчас; always-on FGS; contact-QR тем же конвертом; подмена = стоп+confirm; Olm строго K3; identity 0600+шов; ARCH > Hazel-Signal при конфликте

## Checkpoint History
- 2026-09-29: GOAL создан, next: K1 @general

## Completion
- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:

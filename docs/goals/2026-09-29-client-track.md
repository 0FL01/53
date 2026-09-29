# Goal: клиентский трек K1–K4 (Rust core + Olm + Kotlin shell)

Status: blocked
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
   - Evidence: `docs/goals/k4-gate-checklist.md`: arm64 native/JNA/camera и enrol на moto API35; 119 workspace tests + 13 JVM green, routine device 3/3. G2 32:21 screen-off с доставкой/dedup, G3 persisted byte-identical retry после SIGKILL, G4 no resurrection, G6 permissions, G9 release ~2.78 MiB / active idle PSS ~81.2 MiB / 551-row scroll verified. G7/G10 input/UI verified, optical fixtures не заявлены. G11 diagnostic TCP/core+UI verified. G5 требует отсутствующий no-GMS аппарат; API36–37/16KB runtime недоступны. G8 sealed-only reinstall UNMET под v1 Keystore/no-key-export. DirectTCP bridge не DNS acceptance; самостоятельный Android DNS embedding по-прежнему software gap, не внешний blocker.
- R5: деплой, тесты, коммиты
  - Source: инструкция «потом коммиты деплой тесты»
  - Acceptance: фазы коммитятся по готовности; серверный трек не сломан (workspace test green); DNS-прогоны против n-de2 PASS
  - Primary evidence: `git log --oneline`, `cargo test --workspace`, smoke PASS
  - Status: verified
   - Evidence: 119 workspace tests green; server fix `995b50f` deployed после backup, msgd healthy/pong и recursive DNS Noise smoke PASS (8.2 с). Core fix `a0c97bb`; Android runtime-fix commit завершает текущий аппаратный checkpoint, полная R4 acceptance не объявляется.

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
- R4/G3/G11 minimal server expansion: paired physical-phone retry exposed a stuck mailbox cursor after another recipient's global seq. Correct `crates/server/src/mbox.rs::ack` to advance over that recipient's delivered events (never over an undelivered event); add interleaved-recipient and replay tests. No schema/wire/deployment topology change. Core must check durable inbox dedup before advancing an Olm ratchet again.
- R4 storage correction: additive encrypted-open API + standard AEAD for sensitive SQLite values; Android supplies a random local key sealed by Keystore/EncryptedFile. Legacy Rust open remains supported. No SQLCipher, key export, cloud recovery or new service. This is necessary because the current live DB is plaintext and seal/wipe can silently replace the identity with an empty DB.
- Allowed: rusqlite/tokio/snow reuse; vodozemac (K3); uniffi (K4)
- Forbidden: правки msgd без блокера; Matrix SDK; WebRTC-стек; копипаста diag-main вместо библиотечной функции

## Current Checkpoint
- Closes: R4
- Result: доступный diagnostic-TCP checkpoint на moto завершён; исходный gate-контракт не ослаблен, полная R4 не закрыта
- Required external state: no-GMS аппарат, API36–37/16KB runtime и физическая оптическая постановка QR fixtures; для G8 требуется изменение противоречивого sealed-only требования, не экспорт ключей
- Boundary: Android C/DNS embedding отсутствует; это отдельный неисполненный software путь, не недостаток доступа и не доказанная невозможность

## Current State
- Resolved: R1–R3 и R5; доступные аппаратные/UniFFI/UI проверки перечислены в checklist
- Last relevant evidence: 32:21 OFF с живым FGS и новым message без дублей; финальные Rust/JVM/routine device проверки green. Финальный test-signed release установлен без reset, enrolled=true; bridges/fixtures/test packages удалены, FGS выключен
- Blocker: GMS moto не доказывает G5; нет API36–37/16KB runtime. Clear data реально удаляет Keystore key: одной sealed-копией G8 восстановление невозможно без запрещённого key export/history transfer
- Remaining software gap: самостоятельный Android DNS transport ещё не реализован; текущий checkpoint принимал только явно оговорённый diagnostic DirectTCP seam
- Next checkpoint requires new hardware/source state; optical QR без постановки камеры не засчитывать. Не стирать основной package

## Material Decisions
- 2026-09-29: G8's sealed-only restore after Clear data conflicts with device-bound Keystore and the v1 prohibition on history transfer: the key is deleted too. Do not export the wrapping key or weaken security to manufacture PASS. Verify same-install restore and explicit loss after a real reset separately; sealed-only reinstall remains unmet unless the source contract is changed.
- 2026-09-29: trait Transport в K1 (direct-TCP за ним); UniFFI только в K4; пагинация — требование сейчас; always-on FGS; contact-QR тем же конвертом; подмена = стоп+confirm; Olm строго K3; identity 0600+шов; ARCH > Hazel-Signal при конфликте

## Checkpoint History
- 2026-09-29: GOAL создан, next: K1 @general
- 2026-09-29: аппаратный enrol обнаружил отсутствие CameraX camera2 и INTERNET; исправлено. После enrol FGS выявил неверный индекс SQLite в load_olm и новый static key в каждом FFI-коннекте. Регрессионные тесты воспроизвели оба отказа; исправления зелёные, FGS действительно опрашивает живой msgd. Сервер не менялся; backend наружу не открыт.
- 2026-09-29: paired gate выявил global seq gaps в mailbox ACK и повторный decrypt durable inbox; RED→GREEN, server `995b50f` deployed с backup/DNS smoke, core `a0c97bb`. Android native/Keystore/offline/UI fixes проверены на moto; screen-off 32:21 PASS diagnostic TCP. Connected-test инцидент уничтожил прежнюю identity: новая enrol identity не является восстановлением старой. Теперь graph-stage guard запрещает connected tests без отдельного `.gate`, stateful gates manual-only. G8 sealed-only UNMET подтверждён reset только `.gate`; cleanup завершён.

## Completion
- Resolved outcomes: R1–R3, R5; доступная часть R4 не заменяет полную acceptance
- Commands and artifacts: workspace tests 119, JVM 13, routine device 3; final debug/release/native builds, four ELF 16KB alignment + zipalign; hardware measurements в checklist
- Constraint and diff-scope check: no public TCP, original tunnel untouched, secrets/fixtures не в Git, no key export/cloud/history transfer; исходные ожидания G1–G11 сохранены
- Final status: blocked для полной аппаратной приёмки; G8 sealed-only unmet, Android DNS software gap явно сохранён. Не complete

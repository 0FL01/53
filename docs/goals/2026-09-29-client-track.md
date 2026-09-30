# Goal: клиентский трек K1–K4 (Rust core + Olm + Kotlin shell)

Status: active
Source: пользовательские инструкции по клиентскому треку и выбранная 2026-09-30 авторизация «один готовый код/QR», ARCHITECTURE.md §3/§5/§6/§7, WORK_PLAN.md M1–M4
Last updated: 2026-09-30

## Objective
Живой DNS-клиент: один публичный код/QR подключения к серверу → логин/пароль → диалоги; регистрация открытая либо по приглашению, один активный аппарат, автоматический reconnect сохранённым ключом. Единая новая auth-логика без ENROL/token-replay, совместимости старого wire и переноса старых аккаунтов. Сохранить Native/Noise/Olm/outbox/Keystore гарантии. Remote-БД пользователь вайпнет позже: сейчас не стирать, не развёртывать несовместимую schema поверх работающих данных; проверить новую логику на свежих изолированных БД.

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
- R2: K2 legacy enrol — историческое evidence, не текущий UX
  - Source: прежний K2; новая инструкция 2026-09-30 заменяет прямой вход по invite требованиями R12–R16
  - Acceptance: прежний сценарий и его runtime-реализация удаляются; trust/pin/device-key регрессии переводятся на новый auth, без compatibility aliases/fallback
  - Primary evidence: прежние 4 enrol-теста; новая приёмка — R12–R16
  - Status: superseded
  - Evidence: happy/reconnect, wrong-pin, expired/revoked, wrong-noise-key были проверены. Это не реализация логина/пароля; текущий legacy wire отдельно описан в `docs/protocol.md`
- R3: Olm и контакты
  - Source: план K3 + аудит (vodozemac, стоп-при-подмене, refill-побудки, QR тем же конвертом)
  - Acceptance: vodozemac Olm (без Matrix SDK, свой X3DH запрещён); identity + signed prekeys; claim/refill; ratchet+ciphertext одна TX; ретрай сохранённым ciphertext; outbox queued/accepted/delivered; контакты (ID, request-add, block, QR, подмена = стоп+confirm)
  - Primary evidence: два core-инстанса друг другу через doubles сервера
  - Status: verified
  - Evidence: 26 unit + e2e A→Б OK (ciphertext≠plaintext, retry без дубликата); device auth per-connect и sig-verify receive-path; contact-QR в core. Парольный вход этим не реализован; прежние plaintext pickle впоследствии защищены K4-Keystore
- R4: K4 Kotlin shell
  - Source: план K4 + аудит (пагинация, always-on FGS, Keystore-wrap, gates)
  - Acceptance: экраны + UniFFI только команды/события с пагинацией; always-on FGS («Экономия» — флаг); Keystore-wrap + EncryptedFile; миграция plaintext→Keystore + wipe; QR-сканер; нотификации без FCM; DNS-relay только (WebRTC P2P запрещён); gates: Doze/screen-off/смерть процесса/no-GMS/пермишены/битый QR/переустановка/пороги APK
  - Primary evidence: сборка APK + gate-чеклист
  - Status: in_progress
  - Evidence: `docs/goals/k4-gate-checklist.md`: arm64 native/JNA/camera и legacy enrol на moto API35; исторические 119 workspace tests + 13 JVM green, routine device 3/3. G2 32:21 screen-off с доставкой/dedup, G3 persisted byte-identical retry после SIGKILL, G4 no resurrection, G6 permissions, G9 прежний TCP release ~2.78 MiB / active idle PSS ~81.2 MiB / 551-row scroll verified. G7/G10 input/UI verified, optical fixtures не заявлены. G11 diagnostic TCP/core+UI verified. Самостоятельный DNS transport — R6/R7, новая авторизация — R12–R16, оставшаяся runtime-матрица — R8–R10. G8 sealed-only обещание superseded новой политикой v1, не объявлено восстановленным.
- R5: деплой, тесты, коммиты
  - Source: инструкция «потом коммиты деплой тесты»
  - Acceptance: фазы коммитятся по готовности; серверный трек не сломан (workspace test green); DNS-прогоны против n-de2 PASS
  - Primary evidence: `git log --oneline`, `cargo test --workspace`, smoke PASS
  - Status: verified
  - Evidence: 119 workspace tests green; server fix `995b50f` deployed после backup, msgd healthy/pong и recursive DNS Noise smoke PASS (8.2 с). Core `a0c97bb`, Android `1a569b4`; новые фазы коммитить по мере проверки.
- R6: управляемый C Slipstream на Android
  - Source: утверждённый DNS-план 2026-09-29 (`b784328`), транспортная часть; ARCH §5 / WORK_PLAN M1
  - Acceptance: одна закреплённая ревизия C transport; native arm64 library с start/stop/status, без process signals; повторные циклы освобождают sockets/threads/handles
  - Primary evidence: native lifecycle tests + повторный start/stop на moto
  - Status: verified
  - Evidence: `crates/slipstream-sys` строит pinned Git sources только в target с условным embedding.patch. Host lifecycle и штатные protocol/path/pin/runtime 4/4 green; PEM/DER Ready, неверный pin rejected, 8 raw streams, loss terminal ~30 с. На moto API35 arm64 native executable: 64 start/stop cycles, FD/task stable, max stop 54.11 мс; signal handlers unchanged. APK/core integration относится к R7, recursive DNS не заявлен этим тестом
- R7: DNS transport в core и QR-профиль
  - Source: DNS-план 2026-09-29 (`b784328`), profile/pinning/device-auth; ARCH §3/§6/§7. Новый пользовательский login/register — R12–R16
  - Acceptance: сохранённые domain/full DER/Noise pub/resolver; live carrier pinning до enrol; один transport, Rust reconnect/backoff; expired invite не запрещает вход уже зарегистрированного ключа, revoked запрещает
  - Primary evidence: pin/TTL/reconnect regression tests и реальный Android enrol/reopen через recursive DNS
  - Status: verified
  - Evidence: authenticated encrypted `core_dns_profile` (schema v4), immutable domain/full DER/Noise pub, numeric current-network resolvers; Rust single-owner supervisor/backoff/cancel. Android APK independently reached recursive DNS. Fresh `.gate` enrol/reopen/refill/fetch passed on moto via Wi-Fi ADB (actual result code 0, not a skipped fixture); основной account сохранён. Live wrong carrier certificate→typed PinMismatch, wrong Noise key→Transport before enrol, no account created. Explicit same-resolver restart preserved identity/pins/inbox/outbox. Concurrent32-caller initial import RED accepted two different pins→GREEN one winner after BEGIN IMMEDIATE compare/save. TTL fix `93fe771` deployed after backup `snap-1790713278`, healthy/pong and DNS Noise smoke PASS 8.1 s. Workspace129 tests green; JVM13/builds green; generated bindings regenerated from host cdylib.
- R8: самостоятельная DNS-only доставка
  - Source: DNS-план 2026-09-29 (`b784328`), paired transport; WORK_PLAN «Первая контрольная точка» / M1–M3
  - Acceptance: moto + второй DNS native peer, затем два Android аппарата; E2E текст без USB/SSH, только разрешённый DNS-path; Wi-Fi/mobile, offline queued и byte-identical retry, process/server restart, без дублей
  - Primary evidence: paired live DNS gate с ограниченным egress; аппаратные результаты отдельно от Linux/emulator
  - Status: in_progress
  - Evidence: moto↔Rust native peer through actual recursive DNS, not adb reverse/SSH. After user disconnected USB (only Wi-Fi ADB listed), SIGKILL/manual retry preserved exact ciphertext, queued→accepted; DNS peer received1 then0, all skip counters0. Native-client saved cursor bug reproduced in live gate and host e2e, fixed by existing zero-count ACK querying the durable server cursor; repeat fetch cursor no longer resets0. During schema4 deployment the same phone process automatically resumed successful DNS polls after server/carrier restart; transient closed handshakes remained visible. Two physical Android devices, restricted-egress gate and Wi-Fi/mobile still pending; Wi-Fi ADB is control only and must not be disrupted blindly.
- R9: фоновая DNS-связь в рамках Android
  - Source: DNS-план 2026-09-29 (`b784328`), фоновые gates; ARCH §5 / WORK_PLAN M4
  - Acceptance: screen-off >30 мин, Doze/Standby, смена сети/DNS, оба режима; корректный FGS type/timeout/stop; реальные status, PSS, DNS queries и battery measurements
  - Primary evidence: runtime DNS background gates и ускоренный системный timeout тест
  - Status: in_progress
  - Evidence: production service now declares justified `specialUse` subtype for user-enabled continuous DNS/QUIC link (not finite data transfer); moto API35 accepted real foreground type0x40000000 and repeated successful DNS polls. Stop/timeout cancels native transport without waiting for the SQLite lock. No Play review or long-duration DNS/Doze/timeout/metrics PASS claimed yet.
- R10: дополнительная runtime-матрица и оптические QR
  - Source: DNS-план 2026-09-29 (`b784328`), compatibility/optical; WORK_PLAN M4 / minimal matrix
  - Acceptance: 16KB runtime, доступные Android16/17 images, AOSP no-GMS emulator; отдельно физический no-GMS аппарат и camera malformed/contact/changed-identity fixtures. Emulator не заменяет modem/OEM/battery evidence
  - Primary evidence: native loading/functional emulator tests и отдельные physical gate records
  - Status: in_progress
  - Evidence: official SDK images installed and complete archive SHA1 verified: API36 `default;x86_64` AOSP/no Google APIs, API37.0 `google_apis_ps16k;x86_64` separate 16KB/GMS compatibility fixture; emulator37.1.11 and tools23 installed. Prepared AVDs `dmsg-api36-aosp` / `dmsg-api37-16k`, neither launched. Native build boundary now supports Android x86_64; clean-env `cargo ndk -t x86_64 -P 26 build -p slipstream-sys --tests` passed, linked Android lifecycle ELF has all LOAD segments aligned0x4000. This is compilation/linking, not guest runtime or page-size evidence; APK test ABI remains to be wired. API37 default/no-GMS image not available in current catalogue. KVM usable. Physical no-GMS/second handset/optical gates remain distinct.
- R11: честная политика потери identity и пилотный APK
  - Source: DNS-план 2026-09-29 (`b784328`), loss/rebind/signing; ARCH §6/§10. Password-authorized replacement UX уточнён R16
  - Acceptance: same-install backup restore только с исходным Keystore; Clear data/uninstall→явная loss, no replacement silently; rebind revokes old device/new E2E/peer warning/no old history. Итоговая arm64 pilot сборка подписана постоянным защищённым release key; все intended правки закоммичены
  - Primary evidence: existing .gate reset/restore tests, UI disclosure, rebind/identity gates, APK signature и git status
  - Status: in_progress
  - Evidence: same-install restore и explicit fail-closed после real .gate clear-data уже проверены; старые ключи после инцидента не восстановлены. Server schema4 implements local file-only `invite-rebind`: atomic old-device/token revocation, fresh-device claim with same user/contact ID, one active device, no old-history delivery or quota reset. Server69/workspace146 tests green, including races, rollback, expiry/revocation, output failure and backup restore. Deployed after backup `snap-1790762585`; healthy/pong, dbversion4, recursive DNS Noise smoke PASS8.2s, 16 active devices and zero single-active violations. No production device was rebound/revoked for this checkpoint. Disposable live rebind + client peer warning and permanent release signing remain pending.
- R12: один публичный код/QR подключения
  - Source: выбранный пользователем вариант и шаг 1 плана ниже; ARCH §6 / WORK_PLAN M2
  - Acceptance: вставка кода и QR импортируют один bounded versioned профиль (domain/full DER/Noise pub), без invitation token/пароля/приватных ключей. Импорт только выбирает сервер; offline preview и live pins обязательны, резолвер текущей сети автоматический. Обычный экран не требует ручных crypto-полей
  - Primary evidence: parser roundtrip/invalid/oversized/no-secrets tests и одинаковый Android результат paste/scan без создания аккаунта
  - Status: pending
  - Evidence: выбран и документирован UX; нынешний `dmsg://join` содержит bearer и не является новым публичным кодом
- R13: аккаунты и два серверных режима регистрации
  - Source: утверждённый план, шаг 2; ARCH §4/§6
  - Acceptance: уникальный в пределах сервера логин, Argon2id hash без plaintext password, bounded auth/ограничение попыток; persisted `open` / `invite_only` (default) с msgctl. Invite нужен только для нового аккаунта в invite_only; созданные по новой схеме пользователи входят в обоих режимах. ENROL/token auth отсутствуют; создание/привязка/consumption invite атомарны и replay-safe
  - Primary evidence: backend login/register/policy/TTL/revocation/race tests, fresh-schema backup/restore; несовместимый старый schema/wire явно отклоняется
  - Status: pending
  - Evidence: в текущем backend нет password credentials и переключателя регистрации; новые команды/opcodes пока не назначены
- R14: простой Android вход и запомненное устройство
  - Source: утверждённый план, шаг 3; ARCH §5/§6 / WORK_PLAN M4
  - Acceptance: проверенный сервер → Войти / Создать аккаунт → диалоги. Логин/пароль, invitation field только когда нужен; понятные ошибки. Credentials только через DNS после pinned handshake, не в URI/logs. После входа device key сохранён, restart/reconnect не спрашивает пароль; Native/Noise/Olm не переписываются
  - Primary evidence: core auth integration + Android form/error/permission/restart gates на реальном DNS
  - Status: pending
  - Evidence: работающий transport/device auth остаётся основой; существующий Scanner enrol-код не считается новым login UI
- R15: перенос старых аккаунтов исключён пользователем
  - Source: новая инструкция «Выкинуть ... легаси логики ... на обратную совместимость похуй ... бд вайпнем потом на remote»
  - Acceptance: нет credential-attach/legacy migrations/auth fallback; новая server/core schema только для новой логики. Старые БД отклоняются без мутации, remote wipe не выполняется этим шагом
  - Primary evidence: отсутствие старых auth API и fail-closed schema tests
  - Status: superseded
  - Evidence: пользователь явно отменил compatibility/migration требование; рабочие remote/основной Android данные пока не стирать
- R16: подтверждённая замена устройства после password login
  - Source: утверждённый план, шаг 5; ARCH §6, R11 loss policy
  - Acceptance: успешная проверка credentials и явное подтверждение замены предшествуют revocation старого доступа. Отмена не меняет аккаунт. Прежние user/contact IDs нового аккаунта, один active device, свежие локальные Noise/Olm keys, peer identity-change STOP/confirm; пароль не восстанавливает удалённые ключи/историю. CAS защищает от concurrent replace; нет admin invite-rebind обхода password-login
  - Primary evidence: disposable-account login/confirm/cancel/concurrent-replace/old-access/peer-warning gates; запрет main clear-data/revoke
  - Status: pending
  - Evidence: atomic server rebind уже есть, password-authorized UX и полный peer gate пока отсутствуют

### Constraints
- C1: Обратная совместимость auth/wire/schema отменена пользователем. Рабочий remote и основной Android package не мутировать/не стирать; несовместимый rollout только отдельным шагом после remote wipe
- C2: Секреты только файлами 0600, не в Git/логи/argv (прецеденты сервера)
- C3: Швы сейчас, мясо позже: Transport trait, пагинация как требование, Экономия флагом, contact-QR тем же конвертом
- C4: ARCH выше любых предложений: только DNS-relay, без WebRTC P2P; E2E только vodozemac
- C5: публичный профиль отделён от секретного invitation/пароля; общий access key не добавлять. Credentials не заменяют device/E2E keys и не обещают восстановления истории; один active device, no key export/history transfer

### Non-goals
- Общий storage-крейт (до реального дубля); адресная книга; prefix-search; группы/typing/link-preview; cloud-backup; автозагрузка; второй FGS-режим; SQLCipher; звонки в K1–K3
- В auth-плане не добавлять общий секрет сервера, публичный поиск по логину, веб-админку, внешнюю авторизацию или дополнительные обязательные поля регистрации. Не добавлять legacy compatibility/credential attach; remote wipe/deploy и reset основной identity сейчас вне шага

## Change Envelope
- Target: crates/core (+ android/ в K4), protocol-дополнения при нужде
- Expected paths: `crates/core/`, `crates/protocol/`, `android/`, `docs/goals/`
- Approved plan expansion: `crates/slipstream-sys/` для C FFI/native build boundary; узкий tracked embedding/platform patch поверх `vendor/slipstream` pinned Git revision (не копия `.local/slipstream` и не rewrite DNS/QUIC/scheduler). Android build scripts/UI/profile/FGS/emulator tests; server enrol/auth tests + deploy только после backup, без public TCP/topology changes. Постоянный signing key только gitignored private files, не env/argv/logs.
- R11 historical rebind (`e3e8aa6`) больше не сохраняется как runtime API: R16 реализует ту же one-active/no-history гарантию через проверенный password login и подтверждение, без invite-rebind. Не выполнять replacement рабочего аккаунта ради теста.
- R12–R16 auth expansion: новый versioned public profile/auth wire в `crates/protocol/`; fresh credentials/policy/auth/msgctl schema/tests в `crates/server/`; core account-auth/profile/fresh schema/UniFFI в `crates/core/`; Android forms/import/errors/tests в `android/`, bindings только генерацией. Стандартная Argon2id библиотека допустима. Bounded peer-binding по известному user ID и FETCH sender_user необходимы для R16 STOP/confirm без потери сообщения. Legacy tests переводятся на новый auth, а не отключаются. Инструкция разрешает реализацию и итеративные коммиты; remote wipe отложен
- R4/G3/G11 minimal server expansion: paired physical-phone retry exposed a stuck mailbox cursor after another recipient's global seq. Correct `crates/server/src/mbox.rs::ack` to advance over that recipient's delivered events (never over an undelivered event); add interleaved-recipient and replay tests. No schema/wire/deployment topology change. Core must check durable inbox dedup before advancing an Olm ratchet again.
- R4 storage correction: additive encrypted-open API + standard AEAD for sensitive SQLite values; Android supplies a random local key sealed by Keystore/EncryptedFile. Legacy Rust open remains supported. No SQLCipher, key export, cloud recovery or new service. This is necessary because the current live DB is plaintext and seal/wipe can silently replace the identity with an empty DB.
- Allowed: rusqlite/tokio/snow reuse; vodozemac (K3); uniffi (K4)
- Forbidden: remote wipe/несовместимый deploy сейчас; Matrix SDK; WebRTC-стек; копипаста diag-main вместо библиотечной функции; чтение/правки `53-opendesign/`

## Current Checkpoint
- Closes: R12/R13 — новый public profile/wire, затем server; R15 superseded
- Smallest next action: commit уточнённого плана, bounded profile/auth wire с test vectors; параллельно server и core consumers, затем Android и end-to-end gates
- Expected evidence: target Rust tests, workspace green, generated bindings, native/JVM/APK builds; fresh isolated account auth/replace/peer STOP+confirm, старые версии отвергнуты без мутации
- Replan if: Wi-Fi ADB would be disconnected by a test; use an isolated fixture instead, not blind radio toggles. Do not clear/uninstall основной package or claim an emulator is a second physical phone

## Current State
- Resolved: R1/R3/R5–R7 verified; R2 пользовательский UX superseded R12–R16, исторические regression results сохранены отдельно
- Last relevant evidence: standalone Android native recursive DNS works; moto↔native peer E2E in both directions, offline queue after real process death byte-identical and no duplicates. No USB/SSH bridge. Actual new enrol isolated `.gate` succeeded; основной identity unchanged. Current Rust146/server69 tests green; prior JVM13 evidence unchanged, release rebuild green. Schema4 deployed with backup and DNS smoke, same phone process resumed polls
- External state not yet obtained: второй physical Android и no-GMS handset; optical positioning not proved. API36/37-16KB official images now installed, runtime tests still pending. G8 loss policy approved, no key export
- Remaining software/check gap: public profile/login/register/policy/confirmed replace (R12–R14/R16); перенос legacy аккаунтов отменён. Старые R8–R11 аппаратные gates не расширяют текущую auth-итерацию
- Next: исполнить план ниже и коммитить проверенные фазы. Remote schema4 остаётся до отдельного wipe/rollout; основной Android не очищать. Wi-Fi ADB — единственный control channel, не отключать; прежние DNS/deploy проверки не являются новыми auth gates

## Material Decisions
- 2026-09-30: пользователь потребовал копию RECON-плана и итеративную реализацию/коммиты, отменил legacy/обратную совместимость и перенос старых аккаунтов. Единая новая схема/wire; ENROL, token-replay и admin invite-rebind удаляются, R15 superseded. «БД вайпнем потом на remote» трактуется как запрет wipe/несовместимого deploy сейчас; свежие изолированные БД для тестов, основной package не трогать
- 2026-09-30: пользователь «Берём этот вариант … один готовый код/QR … Делай копию плана в цель … прошлый способ авторизации … выкинуть … из документации … потом коммит». Публичный код подключения + логин/пароль принят; приглашение лишь условие signup при invite_only. Это supersedes direct invite-enrol как UX, не удаляет криптографические гарантии/историческое evidence и не выдаёт план за готовый auth-код. Текущий шаг — только документация и commit
- 2026-09-30: пользователь «как заокнчиш текущий шаг, делай коммит и пауза, я хочу подумать» — finish current server rebind iteration, commit intended edits, pause without starting another stage. This is a user-requested pause, not full completion or an external blocker.
- 2026-09-29: пользователь «Делай копию плана в цель и итеративное реализовать и коммит всех правок» утвердил DNS-план (`b784328`), включая исправление G8: same-install restore с ключом, явная потеря после Clear data/uninstall; без key export/cloud/history transfer. Новая авторизация supersedes только старый пользовательский вход; DNS/security/runtime требования и loss policy сохраняются.
- 2026-09-29: G8's sealed-only restore after Clear data conflicts with device-bound Keystore and the v1 prohibition on history transfer: the key is deleted too. Do not export the wrapping key or weaken security to manufacture PASS. Verify same-install restore and explicit loss after a real reset separately; sealed-only reinstall remains unmet unless the source contract is changed.
- 2026-09-29: trait Transport в K1 (direct-TCP за ним); UniFFI только в K4; пагинация — требование сейчас; always-on FGS; contact-QR тем же конвертом; подмена = стоп+confirm; Olm строго K3; identity 0600+шов; ARCH > Hazel-Signal при конфликте

## Checkpoint History
- 2026-09-30: новый account-auth план frozen R12–R16; старый direct-invite UX убран из действующих требований ARCH/M2 и заменён единым публичным кодом/QR + login/register. Protocol/runbook описывают действующий legacy wire/CLI отдельно, старый P3 Goal помечен архивным без активных задач. Сверены формы/два режима/secret-free profile/migration/replace и ссылки на tracked docs; `git diff --check` green. Только семь Markdown-документов, код/deployment/аккаунты/данные и чужие изменения не менялись
- 2026-09-30: committed already-prepared Android x86_64 build-target support after actual API26 cross-build/test-executable linking and 16KB ELF inspection. AOSP/API37-16KB AVDs prepared but not launched; no emulator functional PASS claimed. User-requested pause remains in effect, not a new runtime stage.
- 2026-09-30: R11 server rebind RED→GREEN, schema3→4 forward migration/partial unique index, same-ID fresh-device login and history/quota isolation verified by 69 server tests. Backup `snap-1790762585` (106496-byte DB, zero blobs), isolated service-unit deploy, healthy/pong/dbversion4/DNS smoke PASS8.2s. Read-only production aggregate unchanged16 users/16 devices/16 active; no rebind performed on working accounts. User requested commit and pause before the next stage.
- 2026-09-30: R7 integrated and actual moto fresh `.gate` DNS enrol/reopen passed. Main account not reset; carrier pin and Noise key live negative cases distinct. First paired fetch exposed empty-report cursor0; host e2e RED→GREEN, zero-count existing ACK obtains durable server cursor without new protocol/schema. After USB removal, Wi-Fi-ADB-controlled process death + exact-ciphertext retry delivered1 then0 to native DNS peer. R8 physical pair/background/restricted-egress remain pending; SDK archives recovered with verified bounded ranges instead of treating download EOF as final blocker.
- 2026-09-29: утверждённый план frozen `b784328`. R6 native boundary host/Android linked и реально выполнен на moto: 64 lifecycle cycles stable resources/max cancel54.11мс; full-cert pin/raw streams/loss gate green. R7 TTL RED Expired→GREEN same bound device after deadline; revoked invite/device remain rejected; 6 live enrol probes и 10 enrol unit green. Native builds не заменяют Android recursive-DNS acceptance.
- 2026-09-29: GOAL создан, next: K1 @general
- 2026-09-29: аппаратный enrol обнаружил отсутствие CameraX camera2 и INTERNET; исправлено. После enrol FGS выявил неверный индекс SQLite в load_olm и новый static key в каждом FFI-коннекте. Регрессионные тесты воспроизвели оба отказа; исправления зелёные, FGS действительно опрашивает живой msgd. Сервер не менялся; backend наружу не открыт.
- 2026-09-29: paired gate выявил global seq gaps в mailbox ACK и повторный decrypt durable inbox; RED→GREEN, server `995b50f` deployed с backup/DNS smoke, core `a0c97bb`. Android native/Keystore/offline/UI fixes проверены на moto; screen-off 32:21 PASS diagnostic TCP. Connected-test инцидент уничтожил прежнюю identity: новая enrol identity не является восстановлением старой. Теперь graph-stage guard запрещает connected tests без отдельного `.gate`, stateful gates manual-only. G8 sealed-only UNMET подтверждён reset только `.gate`; cleanup завершён.

## Completion
- Resolved outcomes: R1/R3/R5–R7 verified, R2 UX superseded; R4/R8–R11 не complete, новый R12–R16 implementation pending
- Commands and artifacts: workspace tests 119, JVM 13, routine device 3; final debug/release/native builds, four ELF 16KB alignment + zipalign; hardware measurements в checklist
- Constraint and diff-scope check: docs-only auth-plan checkpoint; no runtime/deploy/data changes, no public TCP, secrets/fixtures не в Git, no key export/history transfer. Пользователь утвердил новый account-auth UX и ранее revised G8, аппаратные ожидания не ослаблены
- Final status: active; текущий docs checkpoint фиксирует новый план, не реализацию авторизации. Полный DNS/R4/auth outcome не complete

## Утверждённый план авторизации — копия для итеративного исполнения

Принят вариант **один готовый код/QR сервера**, а не ручной набор «домен + ключи» или общий секрет сервера. Действующий пользовательский путь один:

```text
Первый запуск → Вставить код подключения / Сканировать QR
→ Проверить сервер → Войти / Создать аккаунт → Диалоги
Следующие запуски → Диалоги; reconnect сохранённым ключом
```

Код — публичная карточка сервера: domain, полный сертификат и Noise pubkey, без пароля и приглашения. Он говорит, куда подключиться и как проверить сервер; право доступа отдельно подтверждается аккаунтом. DNS-параметры выбираются автоматически из сети, crypto-поля остаются в дополнительных настройках.

**Войти:** логин + пароль. **Создать аккаунт:** логин + пароль, приглашение только когда сервер его требует. На сервере два режима:

| Режим | Создание аккаунта | Вход существующего пользователя |
|---|---|---|
| `open` | Логин + пароль | Разрешён |
| `invite_only` (default) | Логин + пароль + действующее приглашение | Разрешён |

Приглашение — дополнительное разрешение на регистрацию, не основной код подключения и не единственный способ входа. Политика проверяется сервером; выключение открытой регистрации не блокирует уже существующие аккаунты. Публичный login directory не нужен, контактный ID остаётся прежним.

### 1. Публичный профиль и единый новый wire (R12)

Один экран подключения, затем два понятных действия. `dmsg://server/` profile v1: domain/full DER/Noise pub, bounded 4 KiB и offline preview. Wire v2: policy/signup/login/key-resume/authenticated/replace-required; wire v1 и `dmsg://join` отвергаются, никаких aliases/fallback. Credentials bounded, canonical ASCII login, пароль не нормализуется. Fresh schema отвергает старую версию без мутации. Paste/scan одного кода; импорт не создаёт аккаунт.

### 2. Добавить серверные аккаунты и режимы регистрации (R13)

Fresh users с уникальным login/Argon2id hash, persisted open/invite_only default, msgctl public-code/policy/secret signup invite. Нет ENROL/token login/rebind invite. Argon вне DB mutex/Tokio executor, bounded slots и attempts. Signup+device bind+invite consumption атомарны; same-device retry после потери ответа возвращает тот же account. Wrong-password/conflict/TTL/revoked/used/races/backup-restore tests. Resume только активным Noise key, без пароля/токена.

### 3. Rust core / UniFFI (R14)

Стабильный pending Noise key сохранён до отправки signup/login; accepted account записывается атомарно, password не сохраняется. Policy/auth operations только после pinned handshake; resume key-only при restart/poll/send/fetch. Нет core token/enrol/profile fallback и credential attach. Typed safe errors, bindings сгенерировать host cdylib. Existing Olm/outbox transactions и pin validation сохранить; тесты fixture перевести на новый auth.

### 4. Android onboarding (R12/R14; R15 исключён)

State: нет profile → вставка/scan; profile без account → login/signup с invite только в invite_only; account → диалоги. Policy получить коротким запросом, не держать pre-auth socket пока пользователь заполняет форму. Import и password поля не сохранять в Android saved state/logs; DNS-only facade/service без DirectTCP fallback. Формы/error/camera-denied/paste/restart проверить на disposable `.gate`, не переустанавливать основной package.

### 5. Проверить замену устройства после входа (R16)

Login на новом key возвращает replace-required с текущим key после проверки credentials, без мутации. После UI confirm второй login с expected-old-key: atomic CAS, revoke/close old sessions, cursor skips старую историю, user/contact IDs прежние; concurrent loser требует нового подтверждения. Cancel не меняет server account. Fresh Noise/Olm, bounded binding по известному user ID; peers видят смену и STOP до confirm, pending сообщение до confirm не ACK/discard. После confirm доставка/ответ без дублей и exact-ciphertext retry. Нет admin password-bypass rebind.

### Проверки, коммиты и остальные ворота

Каждая coding-итерация — целевые tests и commit; финально msgd build/workspace tests, generated bindings, clean native arm64, JVM/debug/release builds. Новые account/DNS gates на свежих disposable fixtures, не выдавать host/DirectTCP за физический recursive DNS. Remote wipe/rollout только позже отдельным шагом с backup и DNS smoke; до него не менять remote data/schema/services. Не сохранять реальные passwords/invites/domains в Git/argv/logs; не менять основной package/identity ради destructive gates.

Проверенный самостоятельный DNS transport (R6/R7), encrypted store, E2E/очередь и реальный обмен phone↔native peer остаются основой. После auth-итераций остаются прежние незакрытые R8–R11: два физических Android, restricted DNS egress, mobile/Wi-Fi, >30 мин DNS screen-off/Doze/оба режима/query/battery/PSS, timeout, AOSP/no-GMS и API36/37-16KB runtime, physical optical QR и постоянный release signing. Исторический TCP 32:21 и image metadata не подменяют их. Политика same-install restore с исходным Keystore и явной loss после reset сохраняется; пароль не возвращает утраченную историю, key export/history transfer в v1 не добавляются. Voice notes/files/calls не ставить впереди text/auth path.

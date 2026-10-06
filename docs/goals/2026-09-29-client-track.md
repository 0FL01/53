# Goal: клиентский трек K1–K4 (Rust core + Olm + Kotlin shell)

Status: active
Source: пользовательский клиентский трек, account-auth 2026-09-30 и frontend/dev DB wipe 2026-10-01; ARCHITECTURE.md §3/§5/§6/§7, WORK_PLAN.md M1–M4, 53-opendesign/DESIGN.md
Last updated: 2026-10-06

## Objective
Утверждённая итерация DNS fallback + Network/DNS handoff/wake 2026-10-06 и её актуальные evidence/blocker вынесены в [`2026-10-06-dns-fallback-handoff.md`](2026-10-06-dns-fallback-handoff.md). Physical handoff не закрыт отсутствием USB control; это не расширяет отложенный R9.

Живой DNS-клиент: один публичный код/QR подключения к серверу → логин/пароль → удобные личные текстовые диалоги по Light/Square из `53-opendesign/`. История обеих сторон и реальные статусы, summaries/локальные aliases/unread; один активный аппарат, reconnect сохранённым ключом. Единая auth без legacy/migration fallback. Сохранить Native/Noise/Olm/outbox/Keystore гарантии. Пользователь разрешил dev DB wipe и fresh remote rollout; server secrets/pins и исходный туннель сохраняются, основной Android package/Keystore не стирать ради тестов.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Independent design/core phases may use @general; orchestrator verifies, tests, commits. Current instruction authorizes R17–R21 including scoped dev DB wipe/remote rollout; older hardware/signing gates remain separate.

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
  - Evidence: happy/reconnect, wrong-pin, expired/revoked, wrong-noise-key были проверены исторически; runtime удалён. Текущий wire2/password auth — `docs/protocol.md`, архив прежних probes — `2026-09-29-msgd-enrol.md`
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
  - Acceptance: сохранённые domain/full DER/Noise pub/resolver; live pinning до account-auth; один transport, Rust reconnect/backoff; новый key-only resume не зависит от invitation TTL, revoked device запрещён
  - Primary evidence: pin/resume/reconnect regression tests; прежний recursive DNS gate исторический, текущая auth DNS-проверка R12–R16
  - Status: verified
  - Evidence: authenticated encrypted `core_dns_profile` (schema v4), immutable domain/full DER/Noise pub, numeric current-network resolvers; Rust single-owner supervisor/backoff/cancel. Android APK independently reached recursive DNS. Fresh `.gate` enrol/reopen/refill/fetch passed on moto via Wi-Fi ADB (actual result code 0, not a skipped fixture); основной account сохранён. Live wrong carrier certificate→typed PinMismatch, wrong Noise key→Transport before enrol, no account created. Explicit same-resolver restart preserved identity/pins/inbox/outbox. Concurrent32-caller initial import RED accepted two different pins→GREEN one winner after BEGIN IMMEDIATE compare/save. TTL fix `93fe771` deployed after backup `snap-1790713278`, healthy/pong and DNS Noise smoke PASS 8.1 s. Workspace129 tests green; JVM13/builds green; generated bindings regenerated from host cdylib.
- R8: самостоятельная DNS-only доставка
  - Source: DNS-план 2026-09-29 (`b784328`), paired transport; WORK_PLAN «Первая контрольная точка» / M1–M3
  - Acceptance: moto + второй DNS native peer, затем два Android аппарата; E2E текст без USB/SSH, только разрешённый DNS-path; Wi-Fi/mobile, offline queued и byte-identical retry, process/server restart, без дублей
  - Primary evidence: paired live DNS gate с ограниченным egress; аппаратные результаты отдельно от Linux/emulator
  - Status: in_progress
  - Evidence: moto↔Rust native peer through actual recursive DNS, not adb reverse/SSH. After user disconnected USB (only Wi-Fi ADB listed), SIGKILL/manual retry preserved exact ciphertext, queued→accepted; DNS peer received1 then0, all skip counters0. Native-client saved cursor bug reproduced in live gate and host e2e, fixed by existing zero-count ACK querying the durable server cursor; repeat fetch cursor no longer resets0. During schema4 deployment the same phone process automatically resumed successful DNS polls after server/carrier restart; transient closed handshakes remained visible. 2026-10-05 two physical Android (moto API35 ↔ A142P API36, both `.gate`, production LinkProperties recursive resolver, no adb reverse/bridge): 18 method executions/0 skips, both directions received1 then0/skips0, double-submit1 row, Queued→Accepted→Delivered with identical ciphertext hash across new process, cursor dedup; sequence in `android/AUTH_GATES.md`. Restricted-egress gate, Wi-Fi↔mobile and live-PID SIGKILL/server restart on the pair still pending; Wi-Fi ADB is control only and must not be disrupted blindly.
  - Evidence extension 2026-10-06: physical-pair live-PID SIGKILL/byte-identical queued retry and joint messenger-server restart/reconnect/queued delivery now verified;38 recorded successful executions/0 skips, both directions1 then0/Delivered, scoped cleanup and main/deployment invariants green. Previous sentence describes the 2026-10-05 boundary; only mobile/restricted egress remain pending in R8. Exact current sequence — `android/AUTH_GATES.md`.
  - Evidence extension 2026-10-06 (independent recovery): server crash self-healing verified — carrier-only kill, msgd-only kill and msgd-only recreate each left the peer container untouched (PID unchanged); both phones recovered via ordinary reconnect/retry with byte-identical ciphertext, receive1 then0/skips0, persistent Delivered (17/17 device-method PASS, 0 skips). New independent-namespaces topology with static private backend; runbook updated.
- R9: фоновая DNS-связь в рамках Android
  - Source: DNS-план 2026-09-29 (`b784328`), фоновые gates; ARCH §5 / WORK_PLAN M4
  - Acceptance: screen-off >30 мин, Doze/Standby, смена сети/DNS, оба режима; корректный FGS type/timeout/stop; реальные status, PSS, DNS queries и battery measurements
  - Primary evidence: runtime DNS background gates и ускоренный системный timeout тест
  - Status: deferred_by_user
  - Evidence: 2026-10-06 user explicitly deferred R9 outside current scope; acceptance is not waived or claimed PASS. Earlier evidence: production service declares `specialUse` subtype; moto API35 accepted real foreground type0x40000000 and repeated DNS polls. Stop/timeout cancels native transport without waiting for the SQLite lock. No Play review or long-duration DNS/Doze/timeout/metrics PASS claimed.
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
  - Evidence: same-install restore и fail-closed после real .gate clear-data проверены, старые ключи не восстановлены. Исторический schema4 `invite-rebind` (`e3e8aa6`) проверен server69/workspace146/deployed после backup `snap-1790762585`, healthy/dbversion4/recursive smoke PASS8.2s, без рабочего rebind. Этот runtime API удалён: R16 password-confirmed replacement + peer STOP/confirm verified local и теперь recursive R21. Fresh dev rollout выполнен, permanent release signing остаётся отдельным и не заявлен готовым.
- R12: один публичный код/QR подключения
  - Source: выбранный пользователем вариант и шаг 1 плана ниже; ARCH §6 / WORK_PLAN M2
  - Acceptance: вставка кода и QR импортируют один bounded versioned профиль (domain/full DER/Noise pub), без invitation token/пароля/приватных ключей. Импорт только выбирает сервер; offline preview и live pins обязательны, резолвер текущей сети автоматический. Обычный экран не требует ручных crypto-полей
  - Primary evidence: parser roundtrip/invalid/oversized/no-secrets tests и одинаковый Android результат paste/scan без создания аккаунта
  - Status: verified
  - Evidence: `fba3916` profile/parser wire vectors, protocol40 green; один Server QR/paste QrGate, offline import не создаёт account. Physical `.gate` UI: actual clipboard multiline paste/offline preview/cancel/accept; live wrong full-cert→PinMismatch, wrong Noise→Transport до credentials. Оптическое считывание камеры отдельно R10, не заявлено
- R13: аккаунты и два серверных режима регистрации
  - Source: утверждённый план, шаг 2; ARCH §4/§6
  - Acceptance: уникальный в пределах сервера логин, Argon2id hash без plaintext password, bounded auth/ограничение попыток; persisted `open` / `invite_only` (default) с msgctl. Invite нужен только для нового аккаунта в invite_only; созданные по новой схеме пользователи входят в обоих режимах. ENROL/token auth отсутствуют; создание/привязка/consumption invite атомарны и replay-safe
  - Primary evidence: backend login/register/policy/TTL/revocation/race tests, fresh-schema backup/restore; несовместимый старый schema/wire явно отклоняется
  - Status: verified
  - Evidence: `1c1ef83`, server62 tests green: fresh5/read-only rejection old/future, Argon2id/bounded workers+attempts, persisted policy, file-only invite lifecycle, signup/replace loss+races+rollback, backup/restore. Physical invite-only/open signup + restart/key-resume passed; remote не обновлялся
- R14: простой Android вход и запомненное устройство
  - Source: утверждённый план, шаг 3; ARCH §5/§6 / WORK_PLAN M4
  - Acceptance: проверенный сервер → Войти / Создать аккаунт → диалоги. Логин/пароль, invitation field только когда нужен; понятные ошибки. Credentials только через DNS после pinned handshake, не в URI/logs. После входа device key сохранён, restart/reconnect не спрашивает пароль; Native/Noise/Olm не переписываются
  - Primary evidence: core auth integration + Android form/error/permission/restart gates на реальном DNS
  - Status: verified
  - Evidence: `57ddbc7` core durable pending-key/immutable atomic acceptance/typed auth APIs, core all-targets59 green; generated Kotlin. `7707700` DNS-only facade/service, short policy then forms, login/signup/conditional invite/dialog state, secret clearing; JVM23 и debug/release/test APK builds green. Moto API35 fresh `.gate`: signup обоих режимов, independent-process resume/refill/fetch, wrong-password typed error, real UI paste/replace/recreate. Local authoritative DNS/QUIC + pins + Noise, не recursive rollout
- R15: перенос старых аккаунтов исключён пользователем
  - Source: новая инструкция «Выкинуть ... легаси логики ... на обратную совместимость похуй ... бд вайпнем потом на remote»
  - Acceptance: нет credential-attach/legacy migrations/auth fallback; новая server/core schema только для новой логики. Старые БД отклоняются без мутации, remote wipe не выполняется этим шагом
  - Primary evidence: отсутствие старых auth API и fail-closed schema tests
  - Status: superseded
  - Evidence: пользователь явно отменил compatibility/migration требование; ENROL/core_token/credential attach/admin invite-rebind удалены, old-schema tests fail-closed без мутации. Рабочие remote/основной Android данные не стирались
- R16: подтверждённая замена устройства после password login
  - Source: утверждённый план, шаг 5; ARCH §6, R11 loss policy
  - Acceptance: успешная проверка credentials и явное подтверждение замены предшествуют revocation старого доступа. Отмена не меняет аккаунт. Прежние user/contact IDs нового аккаунта, один active device, свежие локальные Noise/Olm keys, peer identity-change STOP/confirm; пароль не восстанавливает удалённые ключи/историю. CAS защищает от concurrent replace; нет admin invite-rebind обхода password-login
  - Primary evidence: disposable-account login/confirm/cancel/concurrent-replace/old-access/peer-warning gates; запрет main clear-data/revoke
  - Status: verified
  - Evidence: server auth_probe CAS/cancel/concurrent winner/lost confirm/old-key live+pending close/no-history green; core live-msgd two-peer gate сохраняет сообщение до explicit confirm, после decrypt1 then0 и byte-identical retry. Physical Android cancel сохраняет host-old RESUME, explicit confirm отзывает его (ERR_REVOKED), IDs прежние; real UI warning/cancel/confirm/recreate passed. Только fresh disposable fixtures, no main/remote mutation

- R17: актуальный интерактивный дизайн-источник
  - Source: пользователь «фронтенд натянуть … 53-opendesign … если элементов не хватает, доделаешь», принятый frontend-план, шаг 1
  - Acceptance: prototype/index и handoff отражают public code/QR → preview → login/signup/conditional invite → dialogs, replacement-confirm и реальные ошибки; нет старого invite-enrol. Light/Square сохранён, будущие media остаются явно demo и скрыты в текущем контракте
  - Primary evidence: воспроизводимые HTML state/geometry checks и просмотр контрольных screenshots
  - Status: verified
  - Evidence: byte-identical prototype/index; auth/current-text 492 geometry +82 functional checks, retained future-call330 geometry +58 checks, 0 JS errors/network requests. 360px/200% signup/replace screenshots viewed. Exact source/results — `53-opendesign/QA.md`
- R18: реальные данные текстового frontend
  - Source: принятый frontend-план, шаг 2
  - Acceptance: core владеет защищённой persistent историей обеих сторон; bounded per-contact pages, точный status по message ID, summaries/локальный alias/time/unread/read cursor. History/outbox/ratchet сохраняются атомарно; retries не создают историю/новый ciphertext. Local time не выдаётся за sender time, unread не read receipt; нет plaintext history в Prefs
  - Primary evidence: core restart/encryption/paging/status/dedup/alias/read-cursor tests и generated UniFFI
  - Status: verified
  - Evidence: fresh core schema6; 54 unit +9 live integration +gated binding test +1 example =65 all-targets green, explicit host cdylib/gen_bindings green. Encrypted history/alias/restart/keyset/read cursor/exact monotonic status/rollback ratchet+outbox+inbox/no ACK/dedup tested; API — `crates/core/R18_API.md`
- R19: нативный Light/Square frontend
  - Source: принятый frontend-план, шаг 3; `53-opendesign/DESIGN.md`
  - Acceptance: native Kotlin/AppCompat/XML onboarding/dialogs/chat/contact/my QR/connection/outbox/storage соответствуют визуальному контракту; реальные данные/CTA, draft сохранён при ошибке, pending guards, typed human errors/connection state, STOP до trust confirm. App-owned radius/elevation0, light theme, 48dp touch, insets/IME и крупный шрифт; без WebView/Compose migration/fake metadata/call buttons
  - Primary evidence: JVM + APK builds и actual UI gates/screenshots на disposable `.gate`
  - Status: verified
  - Evidence: `1087acc` native screens/facade/typed connection state, current core6 ARM64 ABI/generated Kotlin; 35 JVM/debug/release/test APK green. Moto API35 .gate: summaries/alias/unread, chronological bubbles/exact status/draft/process-death/trust/FGS/queue/storage; 200% font + portrait/landscape IME. Coordinator viewed private screenshots, send target above keyboard. 27 distinct methods/38 successful executions/0 skips; `android/AUTH_GATES.md`
- R20: проверенный текстовый frontend и коммиты
  - Source: принятый frontend-план, шаг 4 и прежняя инструкция итеративных коммитов
  - Acceptance: host/workspace/native/JVM/APK gates green, physical real facade проверяет обе стороны истории/статусы/reopen/queue/identity-change, narrow/large-text/keyboard; intended правки закоммичены. Fixture skip не PASS, main identity не reset
  - Primary evidence: gate checklist с точными командами/результатами и git log/status
  - Status: verified
  - Evidence: clean-env fmt/msgd/workspace167 passed/0 failed, 1 native-loopback fixture ignored separately; core all-targets65 + explicit host codegen, current NDK r28c ARM64, 35 JVM/debug/release/gate APK green. Physical .gate27 methods/38 passes/0 skips, installed APK/native hashes matched build. Double-submit1 row; durable history/status/ciphertext preserved through process death; fixtures cleaned, main UID/version/install/update unchanged. `45d7e4b`/`db74f88`/`7f3fcaa`/`1087acc`/`a6d21e3`; final closure evidence in checklist
- R21: fresh remote rollout нового текстового клиента
  - Source: принятый frontend-план, шаг 5; пользователь «бд вайпать можно … идёт разработка, потеря данных не страшна»
  - Acceptance: только dmsg53 получает актуальный msgd/fresh server DB, сохраняя server secrets/pins/endpoint/topology; healthy/schema5/policy и recursive DNS signup/resume/E2E phone↔native peer/dedup проверены. Loss старых dev accounts явно зафиксирована, исходный tunnel не меняется
  - Primary evidence: scoped backup/wipe/recreate record + live recursive Android/native smoke
  - Status: verified
  - Evidence: backup `snap-1790845865` schema4/integrity ok + protected independent DB/source/secrets archive/old image; explicitly lost16 dev users/devices. Only verified dmsg53 DB/WAL/SHM and empty blobs volume wiped. Joint recreate 2026-10-01 09:18:15–16 UTC, healthy/schema5/invite_only; carrier/pins/env/topology/nft/original tunnel unchanged. Source `7f3fcaa` + Docker context fix `a6d21e3`, exact image in `docs/deploy.md`. Actual active-network recursive phone↔native DNS/QUIC/pins/Noise/Olm: match/receive1 then0 both directions/all skips0, exact status/retry/reopen/replacement+old-key revocation. Final3 disposable accounts/5 devices/2 retired; private records `.local/frontend-rollout/` and `.local/frontend-gates/`
- R22: рабочая основная dev-установка после несовместимого обновления
  - Source: пользователь «вноси исправление … пока у нас разработка, всё ломать и чистить можно» после фактического Store screen на main
  - Acceptance: несовместимая nonempty schema0 основного `org.dmsg.client` очищена явно в dev rollout; свежая schema6, public code/signup/dialogs работают на main, reconnect/reopen и реальные recursive DNS сообщения обеих сторон/exact delivery проверены. Один установленный 53, main account остаётся рабочим после cleanup. Dev install не объявляет success только по ярлыку: launch/readiness проверены; автоматического wipe/legacy fallback в APK нет
  - Primary evidence: scoped main reset/install + real MainDevRolloutTest/UI/native pair и current 53.apk hash
  - Status: verified
  - Evidence: RED main nonempty schema0/Store после branding-only `47fb5cd`; explicit dev pm clear → fresh6. Actual main UI signup/recreate/resume, peer recursive receive1 then0 both directions/all skips0/equality, real chat double-submit1 row/exact Accepted→Delivered/new-process durable history+identity. Main4 methods/5 passes/0 skips; final no-reset installer GREEN Dialogs, screenshot viewed; single main53, private fixtures/test package cleaned, root APK84484eec… matched installed. JVM35/release/test/export/icons green; `android/AUTH_GATES.md`, private evidence `.local/main-dev-rollout/`

### Constraints
- C1: Auth/wire/schema compatibility отменена. Explicit dev wipes разрешены для dmsg53 и теперь основной Android dev-установки по R22; потеря старой identity/history ожидаема. Secrets/pins и исходный tunnel не менять; обычные destructive gates остаются `.gate`, main reset только явным dev rollout, не автоматикой APK
- C2: Секреты только owner-private файлами (0400 read-only inputs, 0600 temporary private data), не в Git/env/логи/argv (прецеденты сервера)
- C3: Швы сейчас, мясо позже: Transport trait, пагинация как требование, Экономия флагом, contact-QR тем же конвертом
- C4: ARCH выше любых предложений: только DNS-relay, без WebRTC P2P; E2E только vodozemac
- C5: публичный профиль отделён от секретного invitation/пароля; общий access key не добавлять. Credentials не заменяют device/E2E keys и не обещают восстановления истории; один active device, no key export/history transfer

### Non-goals
- Общий storage-крейт (до реального дубля); адресная книга; prefix-search; группы/typing/link-preview; cloud-backup; автозагрузка; второй FGS-режим; SQLCipher; звонки в K1–K3
- Не добавлять общий secret, login directory, web-admin, external auth или обязательные поля регистрации. Нет legacy compatibility/credential attach. Voice/files/calls/video, Compose migration и прежняя аппаратная матрица R8–R11 вне текущего frontend-этапа; server ACK не прочтение

## Change Envelope
- Target: crates/core (+ android/ в K4), protocol-дополнения при нужде
- Expected paths: `crates/core/`, `crates/protocol/`, `android/`, `docs/goals/`
- Approved plan expansion: `crates/slipstream-sys/` для C FFI/native build boundary; узкий tracked embedding/platform patch поверх `vendor/slipstream` pinned Git revision (не копия `.local/slipstream` и не rewrite DNS/QUIC/scheduler). Android build scripts/UI/profile/FGS/emulator tests; server enrol/auth tests + deploy только после backup, без public TCP/topology changes. Постоянный signing key только gitignored private files, не env/argv/logs.
- R11 historical rebind (`e3e8aa6`) больше не сохраняется как runtime API: R16 реализует ту же one-active/no-history гарантию через проверенный password login и подтверждение, без invite-rebind. Не выполнять replacement рабочего аккаунта ради теста.
- R12–R16 auth expansion: новый versioned public profile/auth wire в `crates/protocol/`; fresh credentials/policy/auth/msgctl schema/tests в `crates/server/`; core account-auth/profile/fresh schema/UniFFI в `crates/core/`; Android forms/import/errors/tests в `android/`, bindings только генерацией. Стандартная Argon2id библиотека допустима. Bounded peer-binding по известному user ID и FETCH sender_user необходимы для R16 STOP/confirm без потери сообщения. Legacy tests переведены на новый auth, не отключены. Прежняя отсрочка remote wipe superseded разрешением R21
- R4/G3/G11 minimal server expansion: paired physical-phone retry exposed a stuck mailbox cursor after another recipient's global seq. Correct `crates/server/src/mbox.rs::ack` to advance over that recipient's delivered events (never over an undelivered event); add interleaved-recipient and replay tests. No schema/wire/deployment topology change. Core must check durable inbox dedup before advancing an Olm ratchet again.
- R4 storage correction: additive encrypted-open API + standard AEAD for sensitive SQLite values; Android supplies a random local key sealed by Keystore/EncryptedFile. Legacy Rust open remains supported. No SQLCipher, key export, cloud recovery or new service. This is necessary because the current live DB is plaintext and seal/wipe can silently replace the identity with an empty DB.
- Allowed: rusqlite/tokio/snow reuse; vodozemac (K3); uniffi (K4)
- R17–R21 expansion: `53-opendesign/` prototype/docs/tokens/tests (разрешено пользователем); `crates/core/` history/status/summary/local metadata + generated bindings, `android/` native UI/resources/facade/tests/service typed state, `docs/` evidence/runbook. Стандартный AndroidX list component допустим при нужде. Wire/crypto/transport не менять ради дизайна; fresh local schema допустима без old-schema fallback. Remote operations только `/opt/srv/53`/dmsg53 volumes/images/containers, bounded private fixtures и scoped rollback snapshot
- R22 expansion: scoped `pm clear org.dmsg.client`/main APK update и file-only main signup/native peer fixtures; `android/` dev install/readiness script + main opt-in instrument methods/docs. Никакой old-schema migration/PRAGMA version spoofing/automatic deletion; crypto/core schema/security guards сохраняются. После acceptance оставить main profile/account/history, удалить только test package/служебные fixtures
- Forbidden: Matrix SDK, WebRTC, WebView/JS runtime в APK, plaintext message DB в Kotlin/Prefs, generated bindings manual edits, исходный tunnel/global firewall/daemon changes, secrets в Git/env/logs/argv

## Current Checkpoint
- Closes: 2026-10-06 independent-recovery iteration **complete** (goal `2026-10-06-independent-recovery.md`): server crash self-healing через независимые namespaces + EDNS OPT parser fix; физическая пара подтвердила все три crash/recreate сценария. R8 как whole по-прежнему in_progress (mobile/restricted egress), R9 deferred.
- Frozen finish line verified: kill -9 только carrier и только msgd, а также msgd-only `--force-recreate --no-deps` — peer-контейнер не перезапускается, обе стороны восстанавливаются обычными reconnect/retry с byte-identical ciphertext, peer receive1 then0/skips0, persistent Delivered.
- Evidence: `docs/goals/2026-10-06-independent-recovery.md` (R1–R4 verified), checklist секция independent recovery, приватные proof-артефакты `.local/r8b-independent/`. Backup `snap-1791287385`; healthy/schema5; secrets/volumes/pins/nft/tunnel unchanged.
- Smallest next action: none in this iteration. Commit создан по явному запросу пользователя. R9 deferred; Wi-Fi/mobile, restricted egress, media и другие runtime/signing gates вне scope.

## Current State
- Resolved: R22 actual main rollout verified, not label-only install. R17–R21/R1/R3/R5–R7/R12/R13/R14/R16 verified; R2/R15 superseded
- Last relevant evidence: physical Moto API35/A142P API36 R8 live-PID12559 SIGKILL/stopped=false and byte-identical retry; both queued/ready before final joint recreate11:08:25 UTC, each2 transient Transport errors then reconnect/retry/Accepted, both receive1 then0/skips0/Delivered.38 recorded successful one-test executions/0 skips,3 resolved diagnostic test failures plus1 lost host-result attempt excluded. JVM35 existing green/debug/release/test APK builds green, final test APK rebuilt/installed. Scoped backup `snap-1791284239`; final healthy/schema5/invite_only/send_fail0/mbox_err0, private cleanup/main metadata verified. Earlier R22/R20/workspace evidence unchanged.
- External state not yet obtained: no-GMS handset (второй physical Android получен 2026-10-05, R8 pair verified); optical positioning not proved. API36/37-16KB official images now installed, runtime tests still pending. G8 loss policy approved, no key export
- Remaining outside this iteration: R9 deferred; R8 mobile/restricted-egress, R4/R10/R11 additional hardware/signing gates. Split-namespace failure mode устранён независимой topology; SPCDNS EDNS assertion исправлен shared patch (baseline воспроизведён, sanitizer green). Legacy account transfer cancelled; no load/media acceptance claim.
- Incident: Perl diagnostic subagent ошибочно вывел inherited credential-bearing environment в tool-log. Не copied в repo; log не retractable, owner/provider credential rotation вне доступного task permission остаётся внешней remediation. Disposable fixtures/processes удалены, дальнейшие builds clean allowlist. Не объявлять C2/log invariant соблюдённым этим прогоном
- Next: none; scoped R8 kill/server-restart iteration complete, deferred/out-of-scope gates do not expand it.

## Material Decisions
- 2026-10-06: user authorised only two R8 scenarios (live-process death/queued retry and messenger-server restart on physical pair), explicitly deferred R9. Main data/radios remain protected; controlled restart means joint dmsg53 recreate without build/wipe/pin change. Eventual normal reconnect may expose transient Transport errors; success requires bounded recovery plus unchanged queued ciphertext and exact delivery, not first-attempt success.
- 2026-10-01: main APK после branding показал Store из-за сохранённой nonempty schema0. Пользователь явно разрешил ломать/чистить dev data; прежний main reset запрет superseded только для scoped dev rollout R22. No automatic wipe in app, default protection/destructive .gate policy остаются
- 2026-10-01: пользователь принял копию frontend-плана и разрешил dev DB wipe («потеря данных не страшна»), supersedes прежний запрет remote wipe. `53-opendesign/` теперь разрешённый дизайн-источник. Трактовка: рабочий текстовый frontend с минимальным core extension и fresh dmsg53 rollout; не реализация макетных звонков/видео, не стирание основного Android Keystore
- 2026-09-30: пользователь потребовал копию RECON-плана и итеративную реализацию/коммиты, отменил legacy/обратную совместимость и перенос старых аккаунтов. Единая новая схема/wire; ENROL, token-replay и admin invite-rebind удаляются, R15 superseded. «БД вайпнем потом на remote» трактуется как запрет wipe/несовместимого deploy сейчас; свежие изолированные БД для тестов, основной package не трогать
- 2026-09-30: пользователь «Берём этот вариант … один готовый код/QR … Делай копию плана в цель … прошлый способ авторизации … выкинуть … из документации … потом коммит». Публичный код подключения + логин/пароль принят; приглашение лишь условие signup при invite_only. Это supersedes direct invite-enrol как UX, не удаляет криптографические гарантии/историческое evidence и не выдаёт план за готовый auth-код. Текущий шаг — только документация и commit
- 2026-09-30: пользователь «как заокнчиш текущий шаг, делай коммит и пауза, я хочу подумать» — finish current server rebind iteration, commit intended edits, pause without starting another stage. This is a user-requested pause, not full completion or an external blocker.
- 2026-09-29: пользователь «Делай копию плана в цель и итеративное реализовать и коммит всех правок» утвердил DNS-план (`b784328`), включая исправление G8: same-install restore с ключом, явная потеря после Clear data/uninstall; без key export/cloud/history transfer. Новая авторизация supersedes только старый пользовательский вход; DNS/security/runtime требования и loss policy сохраняются.
- 2026-09-29: G8's sealed-only restore after Clear data conflicts with device-bound Keystore and the v1 prohibition on history transfer: the key is deleted too. Do not export the wrapping key or weaken security to manufacture PASS. Verify same-install restore and explicit loss after a real reset separately; sealed-only reinstall remains unmet unless the source contract is changed.
- 2026-09-29: trait Transport в K1 (direct-TCP за ним); UniFFI только в K4; пагинация — требование сейчас; always-on FGS; contact-QR тем же конвертом; подмена = стоп+confirm; Olm строго K3; identity 0600+шов; ARCH > Hazel-Signal при конфликте

## Checkpoint History
- 2026-10-06: independent-recovery iteration complete: topology независимых namespaces + static private backend, EDNS OPT shared patch (baseline assertion воспроизведён, ASan+UBSan 12/12), production rollout с backup `snap-1791287385`, три crash/recreate цикла на физической паре 17/17 PASS 0 skips, cleanup/main invariants green, commit `feat(deploy): independent carrier/msgd recovery`.
- 2026-10-06: scoped R8 iteration complete; live-PID SIGKILL/stopped=false retained account/inbox/mid/ciphertext, pair delivered exactly once. Both queued+ready across joint server recreate, ordinary reconnect recovered with2 Transport errors each, both directions receive1 then0/skips0/Delivered.38 recorded passes/0 skips, JVM35/builds green; private cleanup and main/config/pin/volume/tunnel invariants verified. R9 deferred; broader R8 mobile/restricted egress still pending. Runbook now warns against carrier-only restart/split namespace.
- 2026-10-05: R8 physical pair: moto↔A142P `.gate` over recursive DNS, 18 executions/0 skips, both directions exact 1 then0, Delivered/ciphertext retry/dedup; invites consumed and removed, gate packages uninstalled, main unchanged. R8 остаётся in_progress: restricted egress, Wi-Fi↔mobile
- 2026-10-01: R22 verified: approved main dev reset resolved schema0 Store; script refuses label-only acceptance. Main real UI signup, recursive phone↔native E2E1 then0/0 skips, double-submit1 row/Accepted→Delivered/history/reopen, main4 methods/5 passes/0 skips. Final sameAPK install without reset/Dialogs and single53, owned fixtures/invites/peer keys removed, main account/history retained; dev credentials only owner0400 private file. No core validation/schema/transport change
- 2026-10-01: R19–R21 verified: `1087acc` native Light/Square, `a6d21e3` complete/secret-free Docker context. Workspace167/JVM35/codegen/current ARM64 and physical27 methods/38 passes/0 skips; coordinator screenshots viewed. Authorised dev16-account wipe after `snap-1790845865`, fresh schema5 with unchanged carrier/pins/tunnel; actual recursive phone↔native E2E/status/byte-identical process-death retry/replacement STOP-confirm. Gate reset/private cleanup completed; main metadata unchanged. No new credential exposure; prior incident external remediation remains open
- 2026-10-01: R17 browser auth/current-data and retained call regression green; R18 fresh6 encrypted UI API and transactional history/status implemented, coordinator65 core all-targets and explicit generated Kotlin green. R19 native frontend implemented/compile35 JVM; native ABI rebuild/physical/R21 rollout next
- 2026-09-30: unified auth реализована/проверена по фазам `4d3e0ab`/`fba3916`/`1c1ef83`/`57ddbc7`/`7707700`; workspace161/core all-targets59/JVM23, regenerated Kotlin, arm64 native/debug/release/gate builds green. Local physical DNS10 final passes, host peer identity STOP/confirm retains delivery; remote untouched. Занятый signup/full-quota retry regressions RED→GREEN. External harness stale Cargo artifact collision исправлен package clean без подгонки tests; log incident отдельно Current State
- 2026-09-30: новый account-auth план frozen R12–R16; старый direct-invite UX убран из действующих требований ARCH/M2 и заменён единым публичным кодом/QR + login/register. Protocol/runbook описывают действующий legacy wire/CLI отдельно, старый P3 Goal помечен архивным без активных задач. Сверены формы/два режима/secret-free profile/migration/replace и ссылки на tracked docs; `git diff --check` green. Только семь Markdown-документов, код/deployment/аккаунты/данные и чужие изменения не менялись
- 2026-09-30: committed already-prepared Android x86_64 build-target support after actual API26 cross-build/test-executable linking and 16KB ELF inspection. AOSP/API37-16KB AVDs prepared but not launched; no emulator functional PASS claimed. User-requested pause remains in effect, not a new runtime stage.
- 2026-09-30: R11 server rebind RED→GREEN, schema3→4 forward migration/partial unique index, same-ID fresh-device login and history/quota isolation verified by 69 server tests. Backup `snap-1790762585` (106496-byte DB, zero blobs), isolated service-unit deploy, healthy/pong/dbversion4/DNS smoke PASS8.2s. Read-only production aggregate unchanged16 users/16 devices/16 active; no rebind performed on working accounts. User requested commit and pause before the next stage.
- 2026-09-30: R7 integrated and actual moto fresh `.gate` DNS enrol/reopen passed. Main account not reset; carrier pin and Noise key live negative cases distinct. First paired fetch exposed empty-report cursor0; host e2e RED→GREEN, zero-count existing ACK obtains durable server cursor without new protocol/schema. After USB removal, Wi-Fi-ADB-controlled process death + exact-ciphertext retry delivered1 then0 to native DNS peer. R8 physical pair/background/restricted-egress remain pending; SDK archives recovered with verified bounded ranges instead of treating download EOF as final blocker.
- 2026-09-29: утверждённый план frozen `b784328`. R6 native boundary host/Android linked и реально выполнен на moto: 64 lifecycle cycles stable resources/max cancel54.11мс; full-cert pin/raw streams/loss gate green. R7 TTL RED Expired→GREEN same bound device after deadline; revoked invite/device remain rejected; 6 live enrol probes и 10 enrol unit green. Native builds не заменяют Android recursive-DNS acceptance.
- 2026-09-29: GOAL создан, next: K1 @general
- 2026-09-29: аппаратный enrol обнаружил отсутствие CameraX camera2 и INTERNET; исправлено. После enrol FGS выявил неверный индекс SQLite в load_olm и новый static key в каждом FFI-коннекте. Регрессионные тесты воспроизвели оба отказа; исправления зелёные, FGS действительно опрашивает живой msgd. Сервер не менялся; backend наружу не открыт.
- 2026-09-29: paired gate выявил global seq gaps в mailbox ACK и повторный decrypt durable inbox; RED→GREEN, server `995b50f` deployed с backup/DNS smoke, core `a0c97bb`. Android native/Keystore/offline/UI fixes проверены на moto; screen-off 32:21 PASS diagnostic TCP. Connected-test инцидент уничтожил прежнюю identity: новая enrol identity не является восстановлением старой. Теперь graph-stage guard запрещает connected tests без отдельного `.gate`, stateful gates manual-only. G8 sealed-only UNMET подтверждён reset только `.gate`; cleanup завершён.

## Completion
- Resolved outcomes: 2026-10-06 independent-recovery iteration complete (server crash self-healing + EDNS fix + physical-pair acceptance); earlier R8 live-PID/controlled-restart, R22/R17–R21/auth/core completion retained in history. Broader R8 mobile/restricted egress и R9 остаются открытыми/deferred.
- Commands/artifacts: см. `docs/goals/2026-10-06-independent-recovery.md` и checklist; приватные proof-артефакты `.local/r8b-independent/`; commit с R8+recovery изменениями в git log.
- Constraint/diff-scope check: deploy/topology/parser-обвязка и docs в заявленном envelope; туннель/secrets/pins/volumes/main identities не тронуты. Previous credential-log incident remains external remediation; this iteration does not close it.
- Final status: scoped iterations complete; global Goal remains active for out-of-scope/deferred outcomes. No new phase started.

## Утверждённый план авторизации — копия для итеративного исполнения

Историческая копия плана 2026-09-30, выполненного по R12–R16. Его отсрочка remote wipe ниже superseded frontend-планом 2026-10-01 и завершённым R21; это не текущий запрет rollout.

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

## Принятый frontend-план — копия для исполнения (2026-10-01)

1. **Актуализировать `53-opendesign` (R17).** Public code/QR и offline preview, login/signup/conditional invite, typed auth errors и explicit replacement confirm. Обновить prototype/index/handoff; Light/Square и demo/current-contract границы сохранить.
2. **Реальные данные вместо demo (R18).** Защищённая история входящих/исходящих в Rust, per-contact paging, exact message status, dialog summary/local alias/time/unread/read cursor. В той же TX с ratchet/outbox/inbox, без изменения wire. Убрать `outbox row absent → delivered`, unread не сетевой receipt.
3. **Нативный Android frontend (R19).** Тема/компоненты → onboarding → диалоги → чат → контакт/my QR → связь/очередь/хранилище. Существующие facade/DNS/auth/FGS/Keystore и STOP/confirm гарантии сохранить; HTML не embed, на Compose не мигрировать.
4. **Функциональная/визуальная приёмка и коммиты (R20).** Core/workspace, generated bindings, clean native/JVM/APK; физический `.gate`: реальные сообщения обеих сторон, reopen/status/queue/identity change, narrow/200% font/IME/insets. Исправлять подтверждённые регрессии, каждый завершённый этап коммитить.
5. **Fresh remote rollout (R21).** Пользователь разрешил dev DB wipe. Scoped rollback snapshot, обновление только dmsg53 с fresh DB и прежними server secrets/pins, joint services recreate, healthy/schema/policy и recursive Android↔native signup/resume/E2E/dedup. Реальные domain/IP/credentials только private files, не Git/argv/logs. Старые dev accounts/history теряются; никакой migration/compatibility логики.

Текущий endpoint/user flow один; local aliases/unread не публичный каталог или read receipts. Звонки/voice/files/video остаются будущими работами, их demo кнопки в release не включать. Основной Android package/Keystore не reset, Wi-Fi ADB не отключать. Предыдущий log-secret incident требует внешней owner/provider rotation и не закрывается frontend gates.

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
  - Status: in_progress
  - Evidence: `docs/goals/k4-gate-checklist.md`: arm64 native/JNA/camera и enrol на moto API35; 119 workspace tests + 13 JVM green, routine device 3/3. G2 32:21 screen-off с доставкой/dedup, G3 persisted byte-identical retry после SIGKILL, G4 no resurrection, G6 permissions, G9 release ~2.78 MiB / active idle PSS ~81.2 MiB / 551-row scroll verified. G7/G10 input/UI verified, optical fixtures не заявлены. G11 diagnostic TCP/core+UI verified. Пользователь утвердил продолжение ниже: самостоятельный DNS transport и дополнительная матрица runtime. G8 sealed-only обещание superseded новой политикой v1, не объявлено восстановленным.
- R5: деплой, тесты, коммиты
  - Source: инструкция «потом коммиты деплой тесты»
  - Acceptance: фазы коммитятся по готовности; серверный трек не сломан (workspace test green); DNS-прогоны против n-de2 PASS
  - Primary evidence: `git log --oneline`, `cargo test --workspace`, smoke PASS
  - Status: verified
  - Evidence: 119 workspace tests green; server fix `995b50f` deployed после backup, msgd healthy/pong и recursive DNS Noise smoke PASS (8.2 с). Core `a0c97bb`, Android `1a569b4`; новые фазы коммитить по мере проверки.
- R6: управляемый C Slipstream на Android
  - Source: утверждённый пользователем план ниже, шаг 1; ARCH §5 / WORK_PLAN M1
  - Acceptance: одна закреплённая ревизия C transport; native arm64 library с start/stop/status, без process signals; повторные циклы освобождают sockets/threads/handles
  - Primary evidence: native lifecycle tests + повторный start/stop на moto
  - Status: verified
  - Evidence: `crates/slipstream-sys` строит pinned Git sources только в target с условным embedding.patch. Host lifecycle и штатные protocol/path/pin/runtime 4/4 green; PEM/DER Ready, неверный pin rejected, 8 raw streams, loss terminal ~30 с. На moto API35 arm64 native executable: 64 start/stop cycles, FD/task stable, max stop 54.11 мс; signal handlers unchanged. APK/core integration относится к R7, recursive DNS не заявлен этим тестом
- R7: DNS transport в core и QR-профиль
  - Source: утверждённый план, шаг 2; ARCH §3/§6/§7
  - Acceptance: сохранённые domain/full DER/Noise pub/resolver; live carrier pinning до enrol; один transport, Rust reconnect/backoff; expired invite не запрещает вход уже зарегистрированного ключа, revoked запрещает
  - Primary evidence: pin/TTL/reconnect regression tests и реальный Android enrol/reopen через recursive DNS
  - Status: pending
  - Evidence:
- R8: самостоятельная DNS-only доставка
  - Source: утверждённый план, шаг 3; WORK_PLAN «Первая контрольная точка» / M1–M3
  - Acceptance: moto + второй DNS native peer, затем два Android аппарата; E2E текст без USB/SSH, только разрешённый DNS-path; Wi-Fi/mobile, offline queued и byte-identical retry, process/server restart, без дублей
  - Primary evidence: paired live DNS gate с ограниченным egress; аппаратные результаты отдельно от Linux/emulator
  - Status: pending
  - Evidence:
- R9: фоновая DNS-связь в рамках Android
  - Source: утверждённый план, шаг 4; ARCH §5 / WORK_PLAN M4
  - Acceptance: screen-off >30 мин, Doze/Standby, смена сети/DNS, оба режима; корректный FGS type/timeout/stop; реальные status, PSS, DNS queries и battery measurements
  - Primary evidence: runtime DNS background gates и ускоренный системный timeout тест
  - Status: pending
  - Evidence:
- R10: дополнительная runtime-матрица и оптические QR
  - Source: утверждённый план, шаг 5; WORK_PLAN M4 / minimal matrix
  - Acceptance: 16KB runtime, доступные Android16/17 images, AOSP no-GMS emulator; отдельно физический no-GMS аппарат и camera malformed/contact/changed-identity fixtures. Emulator не заменяет modem/OEM/battery evidence
  - Primary evidence: native loading/functional emulator tests и отдельные physical gate records
  - Status: pending
  - Evidence:
- R11: честная политика потери identity и пилотный APK
  - Source: утверждённый план, шаг 6 и порядок выполнения; ARCH §6/§10
  - Acceptance: same-install backup restore только с исходным Keystore; Clear data/uninstall→явная loss, no replacement silently; rebind revokes old device/new E2E/peer warning/no old history. Итоговая arm64 pilot сборка подписана постоянным защищённым release key; все intended правки закоммичены
  - Primary evidence: existing .gate reset/restore tests, UI disclosure, rebind/identity gates, APK signature и git status
  - Status: pending
  - Evidence: same-install restore и explicit fail-closed после real .gate clear-data уже проверены; старые ключи после инцидента не восстановлены

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
- Approved plan expansion: `crates/slipstream-sys/` для C FFI/native build boundary; узкий tracked embedding/platform patch поверх `vendor/slipstream` pinned Git revision (не копия `.local/slipstream` и не rewrite DNS/QUIC/scheduler). Android build scripts/UI/profile/FGS/emulator tests; server enrol/auth tests + deploy только после backup, без public TCP/topology changes. Постоянный signing key только gitignored private files, не env/argv/logs.
- R4/G3/G11 minimal server expansion: paired physical-phone retry exposed a stuck mailbox cursor after another recipient's global seq. Correct `crates/server/src/mbox.rs::ack` to advance over that recipient's delivered events (never over an undelivered event); add interleaved-recipient and replay tests. No schema/wire/deployment topology change. Core must check durable inbox dedup before advancing an Olm ratchet again.
- R4 storage correction: additive encrypted-open API + standard AEAD for sensitive SQLite values; Android supplies a random local key sealed by Keystore/EncryptedFile. Legacy Rust open remains supported. No SQLCipher, key export, cloud recovery or new service. This is necessary because the current live DB is plaintext and seal/wipe can silently replace the identity with an empty DB.
- Allowed: rusqlite/tokio/snow reuse; vodozemac (K3); uniffi (K4)
- Forbidden: правки msgd без блокера; Matrix SDK; WebRTC-стек; копипаста diag-main вместо библиотечной функции

## Current Checkpoint
- Closes: R7
- Smallest next action: commit проверенный R6 и TTL regression; backup/deploy server TTL fix; сохранённый DNS profile + единый Rust manager + FFI/Android integration
- Expected evidence: TTL tests и remote health/DNS smoke; profile/pin/reconnect tests + real Android native DNS connection
- Replan if: конкретный cross-build/runtime сбой; диагностировать зависимость, не переписывать транспорт и не стирать identity

## Current State
- Resolved: R1–R3 и R5; доступные аппаратные/UniFFI/UI проверки перечислены в checklist
- Last relevant evidence: 32:21 OFF с живым FGS и новым message без дублей; финальные Rust/JVM/routine device проверки green. Финальный test-signed release установлен без reset, enrolled=true; bridges/fixtures/test packages удалены, FGS выключен
- External state not yet obtained: GMS moto не доказывает physical G5; API36–37/16KB emulator runtime ещё не подготовлены. Исправленная G8 policy утверждена: потеря Keystore честно означает потерю identity, без key export
- Remaining software gap: самостоятельный Android DNS transport ещё не реализован; текущий checkpoint принимал только явно оговорённый diagnostic DirectTCP seam
- Next: реализовать утверждённый план; доступные software/эмуляторные действия выполнять до доказанных аппаратных blockers. Не стирать основной package

## Material Decisions
- 2026-09-29: пользователь «Делай копию плана в цель и итеративное реализовать и коммит всех правок» утвердил приведённый ниже план целиком, включая исправление ожидания G8: same-install restore с ключом, явная потеря после Clear data/uninstall; без key export/cloud/history transfer. Это supersedes прежнее ошибочное sealed-only обещание, не результат восстановления старой identity.
- 2026-09-29: G8's sealed-only restore after Clear data conflicts with device-bound Keystore and the v1 prohibition on history transfer: the key is deleted too. Do not export the wrapping key or weaken security to manufacture PASS. Verify same-install restore and explicit loss after a real reset separately; sealed-only reinstall remains unmet unless the source contract is changed.
- 2026-09-29: trait Transport в K1 (direct-TCP за ним); UniFFI только в K4; пагинация — требование сейчас; always-on FGS; contact-QR тем же конвертом; подмена = стоп+confirm; Olm строго K3; identity 0600+шов; ARCH > Hazel-Signal при конфликте

## Checkpoint History
- 2026-09-29: утверждённый план frozen `b784328`. R6 native boundary host/Android linked и реально выполнен на moto: 64 lifecycle cycles stable resources/max cancel54.11мс; full-cert pin/raw streams/loss gate green. R7 TTL RED Expired→GREEN same bound device after deadline; revoked invite/device remain rejected; 6 live enrol probes и 10 enrol unit green. Native builds не заменяют Android recursive-DNS acceptance.
- 2026-09-29: GOAL создан, next: K1 @general
- 2026-09-29: аппаратный enrol обнаружил отсутствие CameraX camera2 и INTERNET; исправлено. После enrol FGS выявил неверный индекс SQLite в load_olm и новый static key в каждом FFI-коннекте. Регрессионные тесты воспроизвели оба отказа; исправления зелёные, FGS действительно опрашивает живой msgd. Сервер не менялся; backend наружу не открыт.
- 2026-09-29: paired gate выявил global seq gaps в mailbox ACK и повторный decrypt durable inbox; RED→GREEN, server `995b50f` deployed с backup/DNS smoke, core `a0c97bb`. Android native/Keystore/offline/UI fixes проверены на moto; screen-off 32:21 PASS diagnostic TCP. Connected-test инцидент уничтожил прежнюю identity: новая enrol identity не является восстановлением старой. Теперь graph-stage guard запрещает connected tests без отдельного `.gate`, stateful gates manual-only. G8 sealed-only UNMET подтверждён reset только `.gate`; cleanup завершён.

## Completion
- Resolved outcomes: R1–R3, R5; доступная часть R4 не заменяет полную acceptance
- Commands and artifacts: workspace tests 119, JVM 13, routine device 3; final debug/release/native builds, four ELF 16KB alignment + zipalign; hardware measurements в checklist
- Constraint and diff-scope check: no public TCP, original tunnel untouched, secrets/fixtures не в Git, no key export/cloud/history transfer; только G8 revised v1 policy утверждена пользователем, остальные аппаратные ожидания сохранены
- Final status: active, реализовать утверждённый план ниже; предыдущий diagnostic checkpoint завершён, полный DNS/R4 outcome не complete

## Утверждённый план — копия для итеративного исполнения

Следующий главный шаг — сделать так, чтобы телефон сам подключался к серверу через DNS, без компьютера, USB и SSH. Уже проверенные очередь, шифрование и интерфейс будут работать поверх этого соединения.

Сейчас три разных вопроса: DNS-путь нужно дописать в приложении; совместимость нужно проверить в дополнительных средах (часть — в эмуляторе); обещание восстановления после переустановки нужно исправить, поскольку одной зашифрованной копии недостаточно после удаления ключа Android.

### 1. Подготовить приложение к самостоятельной связи

Сейчас приложение умеет разговаривать с msgd, но до сервера его доставлял диагностический мост. Заменяем этот участок встроенным Slipstream-клиентом:

```text
Приложение → встроенный Slipstream → доступный DNS-резолвер
→ наш отдельный сервер → msgd (доставка зашифрованных сообщений)
```

DNS-резолвер — сервер, которому телефон отправляет DNS-запросы; через эти запросы переносится трафик мессенджера. Собрать существующий C-клиент для Android как библиотеку. Сейчас исходники ориентированы на отдельную программу, используют глобальное состояние и process signal handlers. Нужна небольшая обвязка: Rust запускает соединение, узнаёт status, останавливает с освобождением ресурсов и запускает снова после разрыва. DNS/QUIC и управление скоростью остаются существующими; меняется только необходимое для embedding в Android.

Результат: transport загружается на moto, многократно start/stop без падений, зависших потоков и накопления открытых соединений.

### 2. Подключить транспорт к Rust core и QR-регистрации

Провести реальные регистрацию, отправку, получение и retry через библиотеку:

1. Сохранять полноценный профиль из приглашения: domain, certificate, Noise pubkey, DNS parameters переживают restart; ручные диагностические поля не обязательны после каждого запуска.
2. Проверять сервер на живом сетевом соединении: помимо Noise-key — carrier Slipstream certificate; неверный certificate останавливает до enrol.
3. Один transport на приложение, не новый Slipstream на каждую кнопку/poll. Rust управляет соединением/reconnect; Kotlin показывает status и управляет Android service.
4. Wi-Fi пропал/mobile появился/DNS недоступен: видимая потеря связи, сохранённая очередь, попытки с паузами.

Конкретный дефект: login сейчас повторяет invite-token, а server проверяет TTL даже для bound device. Исправить разделение: TTL ограничивает первую регистрацию; зарегистрированный аппарат входит сохранённым ключом; revocation по-прежнему запрещает доступ. Проверить искусственным временем, не ждать сутки.

Результат: телефон регистрируется через DNS, после restart входит прежней identity, в том числе после истечения первоначального invite.

### 3. Доказать работу без диагностического моста

Сначала moto и второй тестовый native client, оба через настоящий recursive DNS. Затем обмен двумя Android аппаратами — исходная цель проекта. Проверить QR enrol, текст в обе стороны, USB/computer отключены, Wi-Fi/mobile, airplane→queued, сеть вернулась→тот же ciphertext, process death/manual start, restart отдельного сервера, отсутствие дублей. Ограничить тестовый egress выбранным DNS-path, чтобы исключить случайный прямой маршрут.

Результат: два телефона читают E2E через DNS, компьютер не нужен, очередь переживает разрывы/restarts.

### 4. Повторить фоновые проверки на настоящем DNS-пути

32 минуты screen-off по мосту не заменяют radio path. Проверить screen-off >30 мин, forced Doze/App Standby, Wi-Fi→mobile, loss/return DNS, «На связи»/«Экономия», память/DNS queries/battery.

Обязательный пункт — FGS type: нынешний dataSync ограничен Android15+ шестью часами фоновой работы за сутки (https://developer.android.com/develop/background-work/services/fgs/timeout). Получасовой тест не доказывает always-on. Выбрать соответствующий сценарию service type, проверить Android requirements и обработку остановки/timeout. Не переименовать без доказательства.

Результат: соблюдены Android limits, UI показывает реальную доступность и не обещает мгновенной доставки при задержках системы.

### 5. Закрыть совместимость и оставшиеся аппаратные проверки

Подготовка параллельна DNS integration. Для первого 16KB runtime не обязательно покупать аппарат: Android предоставляет emulator images (https://developer.android.com/guide/practices/page-sizes). Подготовить подходящий image, при необходимости отдельный test ABI, проверить все native libs (включая новый Slipstream), enrol/messages/storage/restart и доступные Android16/17 images. Emulator не заменяет battery/modem/OEM physical evidence.

Сначала чистый AOSP emulator без GMS: startup/основные операции не требуют Google services. Для исходного physical gate нужен настоящий no-GMS аппарат; Moto GMS не заменяет его; рабочий телефон не перепрошивать ради проверки.

На отдельной test identity физически сканировать malformed QR/contact QR/второй QR same-ID с новыми ключами; warning и Profile confirm. Существующие callback/UI tests сохраняются, optical evidence отдельно.

### 6. Уточнить потерю identity после переустановки

Политика v1: исходный Keystore key существует→same-install local backup restore; после Clear data/uninstall одной sealed-копией identity/history не восстановить. Sealed-копия — закрытый сейф, Keystore — ключ; сейф без удалённого ключа не открыть.

После потери ключей: admin revokes old device, выдаёт доступ новому, новая E2E identity, контакты видят смену, старая история не возвращается. Исправить ошибочное G8 ожидание. Key backup/history transfer — отдельная функция, не обход текущего теста.

### Порядок выполнения и finish line

1. Android Slipstream library и управляемый start/stop.
2. Core integration, QR profile, live certificate pinning, login после invite TTL.
3. DNS exchange без компьютера, затем два Android аппарата.
4. Фон, смена сети, длительная работа сервиса и measurements.
5. Compatibility и physical QR/no-GMS gates.
6. Итоговая приёмка и подписанная пилотная сборка.

На законченной части — целевые tests и отдельный commit. Server update: backup, отдельное deployment, DNS smoke. Ближайшая цель: отключить USB от moto и получить E2E message через DNS. Voice notes/files/calls не ставить впереди text path.

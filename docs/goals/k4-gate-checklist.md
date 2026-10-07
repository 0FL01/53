# K4 gate-чеклист: ручные проверки на аппарате

R4 пока не закрыта. Ниже сохранены исходные ожидания и отдельно записаны
измерения; сборка и instrumented doubles не подменяют аппаратные gates.

## Актуальные voice notes: isolated fresh9/6 (2026-10-08)

- Fixed libopus1.6.1 mono16k/10k VBR/20ms,encoder10/NoLACE7;60s/128KiB final encrypted cap. Без comparative benchmark/manual SHA registry. One core_messages + sealed manifest/encrypted chunks внутри core.db; native foreground recorder/player, Telegram-style gestures/waveform/manual download/seek и оба own-delete scope.
- Parent Rust workspace/build PASS(core64+existingignore/protocol55/live voice/blob/auth/action suites); explicit bindings/NDKr28c arm64; JVM85/debug/release/test APK/scoped lint PASS. Release8783352bytes/zipalign16KiB, не16KiB runtime. Два прежних explicit native-host ignores не выданы за PASS.
- Только Moto fresh `.gate`, **14 distinct methods PASS/zero skips**:5 local native+snapshot,8 full DNS sequence,1 persisted interrupted-own-upload recovery. Two actualmic notes→fresh native peer; defaultSelfOnly peer unchanged, Everyone playing→terminal peer row; reverse12s/192000samples15877bytes, durable8192→new-process resume/play/seek/reopenDelivered.
- Actual UDP DNS через dedicated controlled stub resolver/private zone/carrier/msgd6. Не DirectTCP/USB, не default-ISP/public delegation и не два Android. Working server5/main8/production UDP53/NS/carrier и Main/Pacman identity/data/install сохранены. Incompatible rollout отдельно по явному разрешению.
- Copy waveform-refresh и FINISH-without-manifest delivery RED→GREEN; retry существующим control worker, без нового framework. Команды/границы — `android/AUTH_GATES.md`, цель `2026-10-07-voice-notes.md`. Фото/файлы UI/live calls и длительная background-матрица не объявлены готовыми.

## Предыдущие own-message actions (2026-10-07, main8)

- **Fresh core8**: одна `core_messages`, direct encrypted initialization, без old-schema/plain conversion/legacy E2E. Server5/wire2/carrier/pins/data сохранены. Unsupported/key errors не стирают данные.
- **Edit / SelfOnly / Everyone**: собственные Accepted/Delivered для remote действий; SelfOnly queued не отменяет send. Original IDs/seq/time/ciphertext неизменны; delete терминален, controls невидимы, placeholder нет. Copy/accessibility, отдельные drafts/Save/Cancel и hidden paging сохранены.
- **Проверено**: Rust/core55/protocol53/live actions, explicit codegen, NDKr28c, JVM74 без skips, debug/release/test APK и scoped lint. Moto `.gate` actual Copy/200% font/landscape IME/self-default/cancel/offline/reopen + fresh native DNS peer PASS; stale no-op refresh selection RED→GREEN.
- **Main Moto only**: явно разрешённый reset/install, fresh8 onboarding и DNS обе стороны, UI edit→peer effective text, Everyone→peer tombstone, новый процесс/base+control Delivered: семь exact main methods PASS/zero skips. APK readback совпадает с `53.apk`; Pacman/его данные не затрагивались. Детали — `android/AUTH_GATES.md`, цель `2026-10-07-message-actions.md`.
- Ниже pair/upgrade/core6–7 evidence **историческое**, не fresh8 acceptance. Длительный Doze/background/signing/media не объявлены закрытыми этой фичей.

## Предыдущий вход в приложение (2026-10-06)

Текущий scope 2026-10-06: independent crash/recovery топологии сервера закрыта (carrier/msgd crash + msgd-only recreate на паре, см. секцию ниже). Последовательный DNS fallback Яндекса проверен на обоих телефонах. Цель fallback/Network+DNS/wake **complete** — `2026-10-06-dns-fallback-handoff.md`: JVM/state/worker и physical USB Wi-Fi↔LTE в foreground/economy прошли; новые successful poll2713ms/4932ms вместо ожидания300s. Перед финальным gate пользователь разрешил включить данные выбранной SIM; APN/roaming/VPN не менялись. Same queued mid/ciphertext/account сохранены; native peer actual DNS receive1 then0/skips0/exact plaintext once→phone persistent Delivered, main identity/history/Keystore/install metadata сохранены; disposable fixtures очищены. Same-DNS/new-Network проверен JVM (в физическом gate DNS различались). R9 (длительный screen-off, Doze/Standby и фоновые измерения) **deferred по указанию пользователя**, не PASS; restricted egress непроверен.

Deployment APK уже содержит доверенный публичный server profile; generic APK сохраняет server code/QR и явный trust preview. «Сканировать приглашение» / «Импортировать файл» → signup с логином/паролем → явное создание → диалоги. Приглашение остаётся «считанным», не проверенным до ответа сервера; это отдельный секрет, не browser URI или серверный profile. TTL/отзыв/одноразовость и key-only resume сохранены. `53-1` / `53` / `53ctl qr-invite` deployed с теми же volumes/pins/schema5, carrier не пересоздавался. JVM51/Rust170 и 4 USB onboarding метода PASS: parser, synthetic decoder-result lifecycle, file signup через actual DNS, новый процесс key resume. Optical camera scan user-owned; DocumentsUI picker не заявлен проверенным (проверен точный importer после SAF). Main identity/history сохранены. Goal/evidence — `2026-10-06-invite-onboarding.md`, `android/AUTH_GATES.md`.

Account-auth wire2 и Light/Square Android UI реализованы, без legacy paths; working server5/main8 не обновлены isolated fresh6/9 voice-cut. Проверки прежних ENROL/core6–7 ниже исторические. Полный клиентский контракт — `2026-09-29-client-track.md` R12–R21; текущие fixtures/methods — `android/AUTH_GATES.md`.

| Auth gate | Ожидание |
|---|---|
| Код/QR подключения | Paste/scan выбирают один и тот же публичный профиль; offline import не создаёт аккаунт, malformed/oversized/pin mismatch отклонены до передачи credentials |
| Signup policy | Open: логин/пароль; invite_only: дополнительный valid invitation. Policy enforced server-side, legacy endpoint отсутствует; существующий login работает в обоих режимах |
| Login / restart | Понятные ошибки password/conflict/invite/network; после входа сохранённый device key авторизует restart/reconnect без пароля |
| Unified auth | ENROL/token-replay/credential attach отсутствуют; старые schema/wire отклоняются без мутации. Scoped dev remote wipe выполнен по разрешению пользователя |
| Новый аппарат | Верные credentials + явное подтверждение; cancel без изменений; один active device, старый доступ отозван, peer identity-change STOP/confirm. Удалённая история не возвращается |

Auth/text outcomes проверены local и теперь на fresh remote через actual recursive DNS с физическим disposable `.gate`. Оптическая камера и long DNS background остаются отдельными G/R gates; два физических Android проверены 2026-10-05 (ниже). Основной package/Keystore не очищать/не отзывать.

### Frontend + fresh remote acceptance, 2026-10-01 (R17–R21)

- Native Kotlin/AppCompat/XML Light/Square, реальные encrypted incoming/outgoing history, exact persistent statuses, aliases/local times/unread, per-contact pages/read anchors; UI не выводит delivered из отсутствия outbox. API — `crates/core/R18_API.md`; core schema6/server5/wire2.
- Clean allowlist: `cargo fmt --all --check`; `cargo build -p msgd && cargo test --workspace -- --test-threads=1`: **167 passed, 0 failed**, 1 native-loopback fixture ignored separately. Core all-targets65 и explicit host cdylib/`DMSG_GEN_BINDINGS=1` codegen green. NDK r28c ARM64 rebuilt; JVM **35 passed**, debug/release/gated test APK green. Release unsigned, permanent signing остаётся R11.
- HTML source: auth/current-text **492 geometry +82 functional**, retained future-call **330 geometry +58 functional**, JS errors/network requests0. Browser QA не физическая приёмка.
- Moto API35 ARM64, actual UniFFI/native facade, только Wi-Fi ADB и explicit `.gate` `am instrument` by method: **27 distinct gates, 38 successful one-test executions, 0 skips**. Installed APK/native hashes matched current build. Exact methods/commands — `android/AUTH_GATES.md`, private sanitized evidence — `.local/frontend-gates/evidence.md`.
- Production LinkProperties resolver selection без fixture override; native C peer использует доступный recursive resolver той же сети, RD/RA/non-authoritative answer checked. ADB — управление, SSH — администрация, не message relay. Full-cert pin + Noise + wire2 + Olm обе стороны: equality=true, received1 then0, all skipped0.
- UI Queued→Accepted→Delivered, double submit→1 durable row; process death/reopen/retry сохраняют account/inbox/exact ciphertext. Delivered history переживает outbox removal/recreate, native reopened history incoming1/outgoing1. Actual replacement отзывает старый key; phone STOP/cancel/confirm сохраняет сообщение до receive1 then0.
- Alias/summary/unread/my QR/queue-empty disclosure, actual manual DNS check при FGS off, economy intervals/FGS stop/native cancellation; 551 synthetic rows/paging/read anchors, draft/scroll/recreate, same-key restore и loss после clear-data. 200% font + portrait/landscape IME; coordinator viewed private screenshots, send выше клавиатуры. Synthetic storage/scanner/history fixtures отделены от DNS evidence.
- 7 диагностированных попыток разрешены; два production RED→GREEN: поздний poll после FGS stop и landscape-IME overlap. Остальное — fixture precondition/IPv4 selection/assertion label/settled-layout/camera-denial timing; final gates не suppressed/skipped.
- Backup `snap-1790845865` schema4/integrity ok + protected independent backup/old image; authorised loss16 dev users/devices. Только dmsg53 DB/WAL/SHM и empty blobs wiped, joint recreate 09:18:15–16 UTC: healthy/schema5/invite_only. Pins/env/topology/nft/original tunnel unchanged; deployment — `docs/deploy.md`. После smoke3 disposable accounts/5 devices/2 retired, send_fail/mbox_err0.
- Cleanup: `.gate` cleared/force-stopped, 66 local private fixture/log/key/DB files removed, peer stopped, font restored; main UID/version/install/update unchanged. 16 private screenshots вне Git. Не заявлены long Doze/second physical/no-GMS/16KB runtime/optical QR/permanent signing/media. Прежний credential-log incident требует внешней rotation; новых dumps в этой итерации нет.

### Main dev rollout correction, 2026-10-01 (R22)

- RED: branding install retained nonempty main schema0; actual Store screen. Label/icon PASS did not establish working app.
- Explicit user-approved main `pm clear` removed old DB/history/Keystore identity; no schema spoof/migration/automatic wipe. `dev-install.py` rejects this known startup failure without reset, GREEN after explicit reset and again without reset after auth.
- Main `org.dmsg.client`, current `53.apk`, actual UI onboarding/code/preview/signup and fresh6, production recursive DNS peer both directions receive1 then0/all skips0/equality; exact Accepted→Delivered, no duplicate send/history, new-process reopen/account/history retained. **4 selected main methods/5 passes/0 skips**, `android/AUTH_GATES.md`.
- JVM35/release/test/export green, current launcher check; one main53, test package/private fixtures/invites/peer keys cleaned, dev owner0400 credentials retained outside Git. Main screenshot viewed: Dialogs/«Проверка DNS»/reply preview, no Store. FGS remains explicitly off, not a network failure. Server healthy5/pins/tunnel unchanged. Earlier main-unchanged statements are historical pre-R22 evidence.

### Physical Android pair, 2026-10-05 (R8)

- Moto g54 API35 ↔ A142P Android 16/API36 (4KB pages), оба `.gate`, только Wi-Fi ADB. Production LinkProperties resolver одной Wi-Fi сети, RD/RA non-authoritative проверен; `adb reverse` пуст, TCP bridge/radio toggle нет.
- **18 one-test executions, 0 failures, 0 skips**: signup обоих (invite_only, свежие invites), обмен contact QR файлами, A→B и B→A received1 then0/skips0, double-submit1 row, Queued→Accepted→Delivered, тот же ciphertext hash в новом процессе, cursor dedup. Последовательность — `android/AUTH_GATES.md`.
- Не заявлено: live-PID SIGKILL (процесс B уже завершился с instrumentation), Wi-Fi↔mobile, restricted egress, server restart на паре, оптический QR. Cleanup: `.gate` удалены с обоих, invites consumed/файлы удалены, main metadata unchanged.

### Physical pair live-PID / server restart, 2026-10-06 (R8)

- Moto API35 ↔ A142P API36, оба `.gate`, production recursive DNS без fixture
  override/bridge/radio toggle; main metadata unchanged. **38 записанных успешных
  one-test executions, 21 device/method pairs, 0 skips**; 3 диагностированных
  test failures разрешены, final selected results green. Отдельный SIGKILL hold
  намеренно оборван и не считается PASS; один host SSH/result-reader сбой также
  не засчитан, restart повторён. Точная последовательность — `android/AUTH_GATES.md`.
- A142P live PID12559 подтверждён app-private readiness marker + `pidof`, SIGKILL
  действительно завершил процесс, `stopped=false`. Новый процесс сохранил account,
  inbox, mid, hash ciphertext; Queued→Accepted→Delivered, Moto receive1 then0,
  skips0, одна durable outgoing row несмотря на double submit.
- На обоих телефонах queued rows + actual ready DNS до joint dmsg53 recreate.
  11:08:25 UTC: оба сохранили identity/pins/inbox/ciphertext, normal reconnect
  восстановился в пределах90 s (по2 transient Transport errors, без carrier reset),
  retry→Accepted; оба received1 then0/skips0, reopen→persistent Delivered.
- Initial Transport signup был вызван pre-existing разными network namespaces
  carrier/msgd после carrier-only restart. Backup `snap-1791284239` и joint recreate
  восстановили путь; earlier carrier `decode_rr_opt` assertion не исправлялся и
  текущим прогоном не воспроизведён. Controlled restart не означает crash self-healing.
- Canonical image/mount/port snapshot, config/key fingerprints и other-container
  identities unchanged. Final healthy/schema5/invite_only, send_fail0/mbox_err0.
  Clean JDK21 gated debug/release/test APK builds green, JVM35 existing green;
  final test APK rebuilt/installed, APK/native bytes matched; Rust/native unchanged.
- Cleanup: обе `.gate`/`.gate.test` удалены, PID отсутствуют, consumed invitation
  files/local credentials/resolvers/queue/message/raw diagnostic fixtures удалены.
  R9 **deferred**; Wi-Fi/mobile и restricted egress не проверены в этой итерации.

### Independent carrier/msgd recovery, 2026-10-06 (R8/server topology)

- Топология заменена: независимые network namespaces, msgd static private IP
  `${DMSG_MSGD_IPV4}` (закрытый backend, без published TCP7000), carrier target
  фиксирован; EDNS OPT decoder patch применён в server и embedded build paths
  (baseline assertion воспроизведён, fixed ASan+UBSan 12/12).
- Физическая пара moto API35 ↔ A142P API36, production recursive DNS: **три
  crash/recreate цикла** (kill -9 carrier только / kill -9 msgd только /
  `--force-recreate --no-deps msgd`) — каждый раз peer-контейнер не перезапускался
  (PID unchanged), обе стороны восстановились обычными reconnect/retry с
  byte-identical ciphertext, peer receive1 then0/skips0, persistent Delivered.
  **17/17 device-method PASS, 0 skips** в этой итерации.
- Backup `snap-1791287385` до rollout; healthy/schema5 после; secrets/volumes/
  pins/nft/tunnel unchanged; gate-пакеты удалены, main identities unchanged.
  R9 остаётся **deferred**; Wi-Fi/mobile и restricted egress не проверены.

### Прежнее evidence: unified auth, 2026-09-30

- Rust clean-env msgd build + workspace: **161 passed, 0 failed**; native loopback fixture default ignored отдельно. Core all-targets59, protocol40, server62; live two-peer replacement: STOP/retain до explicit confirm, delivery1 then0, byte-identical retry. Old schema/wire reject без мутации; signup/CAS races/rollback/backup и full-quota dedup green.
- Host cdylib → generated Kotlin; clean NDK r28c arm64 native; JVM **23 passed**, debug/release/test APK green. Release unsigned, permanent signing/R11 не объявлен готовым. Gate APK native bytes matched current AGP stripped output.
- Moto g54 API35 arm64, только Wi-Fi ADB, `.gate`/`.gate.test`, explicit `am instrument` by method, no skipped fixtures:
  - `signupPrivateDnsAccountOnlyInGatePackage` ×2 (invite_only/open);
  - `reopenPrivateDnsAccountOnlyInGatePackage` ×2 (independent process, key-only resume/refill/fetch);
  - `rejectPrivateCredentialsOnlyInGatePackage` ×1 (typed InvalidCredentials);
  - `loginPrivateAccountReplacementOnlyInGatePackage` ×2 (cancel/confirm, host old-key active после cancel и ERR_REVOKED после confirm);
  - `rejectLiveCarrierPinAndNoiseKeyBeforeCredentialsForGate` ×1;
  - `unifiedAuthUiOnlyInGatePackage` ×2 (actual native facade/view clicks, multiline clipboard paste, offline preview, conditional invitation, secret clearing, warning/cancel/confirm/dialogs/recreate).
- **10 successful one-test executions**, final scenarios green/0 skips; 5 промежуточных UI test failures: async Dialog callback race и Android overlay focus, устранены ожиданиями/callback/focus, не suppression. Это локальный LAN high-UDP **authoritative DNS/QUIC + full-cert pin + Noise**, pinned C revision `d7cd5555a88933053551128ff8b3741ae93049a0`, synthetic suffix/public fixture certificate; не SSH bridge/recursive production acceptance.
- Preview не создаёт account; device DB schema5, identity сохраняется при reopen. Gate auth files consumed/wiped, локальные DB/keys/invites/helpers удалены, owned carrier/msgd stopped, `.gate` cleared/force-stopped. Main UID/version/install/update metadata unchanged; no remote changes/radio toggle.
- Diagnostic incident: subagent Perl command вывел inherited credentials в tool-log; repo files их не содержат. Log не retractable, owner/provider rotation требуется отдельно; disposable cleanup не отзывает inherited credentials. Далее clean allowlist environment, no full env dumps.

## Прежнее кодовое evidence (до единой auth, историческое)

- `cargo test --workspace`: core, protocol и server green. Core покрывает
  encrypted-open/migration, offline queue, повторный приём после reopening,
   persistent Noise static, load_olm и authenticated DNS profile/supervisor.
   Прогон R7: 129 tests passed, 0 failed (core45 unit+8 integration,
   protocol23, server52, native lifecycle1). Native live loopback gate требует
   отдельного запуска; штатные C protocol/path/pin/runtime gates green.
- `DMSG_GEN_BINDINGS=1 cargo test -p dmsg-core --test gen_bindings`:
  `android/app/src/main/java/uniffi/dmsg_core/dmsg_core.kt`; additive
  `openEncrypted`, детерминированная нормализация whitespace при генерации.
- `./gradlew assembleDebug`: BUILD SUCCESSFUL
  (AGP 8.13.2 + Gradle 8.14.3 + KGP 2.2.20, compileSdk 37, minSdk 26,
  build-tools 37.0.0, ANDROID_HOME=~/Android/Sdk).
- `./gradlew testDebugUnitTest`: 13/13 OK (QrGate 3, Paging 6, Plan 1,
  FfiErrors 2, SingleWorker 1).
- arm64 native: `ANDROID_NDK_HOME=<NDK r28+> sh android/build-native.sh`.
  Clean environment whitelist, ThinLTO, strip, max-page-size=16384;
  output только в gitignored `app/build/nativeLibs`. JNA 5.18.1 Android AAR,
   а не Java-only JAR/ручной неповторяемый dispatch.
- Финальные debug/release сборки green. APK unsigned release 2 910 380 байт
  (~2.78 MiB), test-signed release 2 918 572 байт; debug 7 410 410 байт.
  `zipalign -c -P 16 4` и LOAD alignment всех четырёх ELF green.
- На аппарате routine instrumentation: 3/3 passed (encrypted migration/restore,
  invalid QR, changed identity). Stateful gates выбирались по методам отдельно;
  skipped operator fixtures при whole-class запуске не считаются PASS.

## Среда: что есть / чего нет

- Есть: Android SDK (platform android-37, build-tools 37.0.0, platform-tools),
  лицензия SDK принята, Gradle-дистрибутивы в кэше, Rust-таргет
  aarch64-linux-android + cargo-ndk.
- Есть NDK r28c и moto g54 5G, Android 15 / API 35 / arm64; сейчас только Wi-Fi ADB, USB отключён пользователем.
  Native core, JNA и камера загружаются. Все четыре ELF библиотеки APK имеют
  LOAD alignment 0x4000. Это статическая 16 KB compatibility, не runtime
  испытание на 16 KB устройстве; Android 16/17 runtime ещё не проверены.
- Moto содержит GMS; настоящего no-GMS аппарата в этой сессии нет.
- Сеть аппарата: встроенный C Slipstream → numeric DNS из current LinkProperties
  → настоящий recursive DNS → отдельный authoritative endpoint → Noise/msgd.
  adb reverse/SSH/netns relay отсутствуют. Ниже исторические TCP результаты
  не заменяются новым DNS claim; отдельный актуальный checkpoint записан далее.
- SDK fixtures подготовлены: API36 AOSP default/x86_64 без Google APIs;
  API37.0 Google APIs ps16k/x86_64 для отдельной 16KB/Android17 проверки.
  Архивы проверены по official SHA1; metadata не считается runtime PASS.

## Контракт ручных gates

2026-09-29 пользователь утвердил исполнение следующего плана и политику G8
из `2026-09-29-client-track.md`: восстановление только с исходным Keystore key;
после Clear data/uninstall — явная потеря. Прежнее sealed-only обещание superseded,
а не выполнено; история исходного аппаратного результата сохранена ниже.

| # | Проверка | Как проверять | Ожидание |
|---|----------|---------------|----------|
| G1 | Forced Doze + App Standby | `dumpsys deviceidle force-idle`, standby bucket rare; ждать 2 опроса FGS | Опросы редеют/пропадают честно, UI показывает состояние; без обещаний мгновенных входящих |
| G2 | Экран выкл. длительно | Выкл. 30+ мин в normal-режиме, затем вкл. | FGS жив (если не прибит OEM), пропущенное подтягивается fetch по cursor, дедуп без дублей |
| G3 | Смерть процесса (НЕ force-stop) | `am kill` / LowMemoryKiller | Перезапуск вручную; inbox/outbox персистентны; retry тем же ciphertext |
| G4 | Force-stop | force-stop из настроек | FGS НЕ воскресает сам (START_STICKY не переживает force-stop); это задокументировано, не маскируется |
| G5 | no-GMS аппарат | Аппарат без Play Services | Всё работает (FCM-зависимостей нет — проверить отсутствие gms в APK) |
| G6 | Отказ разрешений | Запретить камеру/уведомления | Сканер показывает явную ошибку доступа; FGS молча живёт без уведомлений-ошибок (SecurityException глотается) |
| G7 | Битый/oversized QR | QR с мусором, обрезанный, >8 KiB | Явная строка ошибки («битый QR: …» / oversized), enrol/add не вызываются |
| G8 | Очистка данных / переустановка | Same-install unseal с исходным ключом; Clear data/reinstall без ключа, в том числе с оставшейся sealed-копией | Same-install restore сохраняет identity; потеря Keystore→явная reinstall_loss и отказ без silent replacement; нового доступа/истории не выдавать за восстановление старой identity |
| G9 | Пороги | Release APK ≤ 25 MiB; idle PSS ≤ 100 MiB; cold start; скролл 500+ сообщений | Факты вписать сюда; бюджеты — не обещания |
| G10 | Подмена identity e2e | Второй QR того же ID с другими ключами | `identity_changed`, отправка СТОП до явного confirm (Profile-экран) |
| G11 | no сети | Авиарежим: send/retry/fetch | Явные transport-ошибки; outbox queued сохраняется; повтор после сети шлёт тот же ciphertext |

## Аппаратное evidence и ограничения (2026-09-29)

| Gate | Результат | Evidence / граница |
|---|---|---|
| G1 | PARTIAL | Forced deep IDLE + standby rare (bucket 40), два FGS poll через USB ~17 с друг от друга. Doze/возраст последнего ответа раскрыты в UI. USB не доказывает редение радиопути; затем unforce и bucket 10 восстановлены. |
| G2 | PASS (diagnostic TCP) | 18:36:01–19:08:22 UTC: 32 мин 21 с непрерывного OFF, release PID 32254 не сменился, FGS foreground=true. Монитор read-only: 64 наблюдения OFF/PID за 1860 с; в это время без установок/instrumentation/UI. Message отправлен после OFF и fetched фоном. После выдержки inbox baseline+1, seq уникальны, повторный fetch 0 и cursor не уменьшается. Screen-off PSS 41 751 KiB. Первый прерванный прогон не засчитан. Это не DNS-radio/battery/OEM acceptance. |
| G3 | PASS (diagnostic TCP) | SIGKILL реального app PID 30048, package stopped=false; ручной restart instrumentation. Account/inbox/outbox сохранились; SHA-256 сохранённого ciphertext совпал до и после retry; queued→accepted. Получатель получил 1 сообщение, следующий fetch 0, все skip counters 0. Это не power-loss/OEM/два Android аппарата. |
| G4 | PASS | Сначала настоящий foreground service PID 30192, затем `am force-stop`; через 20 с сервисов нет, stopped=true. Instrumentation намеренно прервана force-stop, её “Process crashed” НЕ зеленый unit-тест. Ручной запуск снимает stopped, самозапуска нет. |
| G5 | BLOCKED | GMS есть на moto. Отсутствие FCM/GMS dependencies не заменяет no-GMS runtime. |
| G6 | PASS | На signed-test release отказ камеры дал явную строку; отказ POST_NOTIFICATIONS не убил foreground service, SecurityException/crash не наблюдались. После проверки оба permission восстановлены. |
| G7 | PASS input/UI; optical PENDING | На аппарате реальные UniFFI и ScannerActivity callback: garbage/truncated/>8 KiB→видимая «битый QR», contact count/account не изменились. Вход callback подан instrumentation без exported debug hook; оптическое считывание этих malformed fixtures камерой не заявляется. |
| G8 | PASS revised v1 policy; old promise SUPERSEDED | Реальный `pm clear` ТОЛЬКО отдельного `.gate` package: account fresh; Android Keystore master alias удалён. Возврат одной sealed-копии отказал с reinstall_loss, без новой identity/key. Same-install unseal с исходным ключом проверен. Прежнее sealed-only ожидание было UNMET; пользователь затем утвердил честную v1 policy, без key export/history transfer. Старые ключи этим не восстановлены. |
| G9 | PASS (moto / test-signed release) | Cold main 225 ms; последний финальный release cold 408 ms. Enrolled active FGS idle PSS 83 160 KiB (~81.2 MiB). Отдельная release `.gate` fixture: зашифрованный inbox 551, видимы [550]/[551], UI “551 сообщений”. Gfxinfo 1669 frames: p50/90/95/99=10/12/13/15 ms, janky 1 (0.06%), legacy janky 317 (18.99%); после scroll PSS 92 923 KiB. Release APK ~2.78 MiB. Это синтетическая история, не performance DNS. |
| G10 | PASS input/UI; optical PENDING | На аппарате real UniFFI + отдельный `.gate`: same ID/new keys через Scanner callback→identity_changed; Chat показывает STOP и сохраняет draft, queued rows 0. Нажатия Profile check/confirm снимают mismatch; последующий send проходит identity check и даёт ожидаемую connect error к закрытому fixture endpoint. Ложный STOP при mismatch=false исправлен. Это не два Android аппарата/оптическое повторное считывание QR. |
| G11 | PASS (diagnostic TCP/core + UI fixture) | Авиарежим enabled И adb reverse удалён: fetch/retry explicit connect error, send через существующую Olm session→queued. После SIGKILL/возврата сети retry ciphertext byte-identical, peer fetch 1 затем 0 со всеми skips=0. Дополнительно Chat на `.gate` показывает connect error и не теряет draft; error не перетирается history load. Радиосеть восстановлена. Это не Android DNS-radio acceptance. |

### Найденные ошибки и данные

- CameraX camera2 и INTERNET отсутствовали; исправлены. load_olm читал pickle
  дважды вместо next_key_id; FFI создавал новый Noise static на reconnect;
  воспроизведены regression tests и исправлены без server identity reset.
- Paired device gate обнаружил global-seq gap в server ACK и повторное Olm
  decrypt уже сохранённого message. RED→GREEN tests, server fix `995b50f`,
  backup перед обновлением. Msgd+Slipstream пересозданы только в отдельном
  развёртывании, ключи/volumes сохранены; health healthy, recursive DNS Noise
  smoke PASS (8.2 с, adapter replies 59/errors 10/dropped 22).
- **Инцидент тестирования:** прежний `connectedDebugAndroidTest` был ошибочно
  запущен на основном package; AGP удалил app/Keystore после тестов и потерял
  прежнюю identity. Пользователь уведомлён; восстановление старых ключей НЕ
  произошло. Позже enrol создал новую identity. Теперь connected tests
  требуют `-PgateInstall=true`, отдельный applicationId `.gate`; stateful gates
  запускаются вручную по одному `am instrument -e class ...#method`.
- Секретные fixtures только в `.local`/app sandbox с 0600, без URI в argv/logs.
   Настоящие имена/IP развёртывания не вносятся в трекаемые документы.
- Cleanup завершён: временные invite/peer DB/sealed fixtures и USB/SSH/netns
  relay удалены; `.gate` и instrumentation packages удалены, основной package
  НЕ удалён. На нём финальный test-signed release, enrolled=true, FGS выключен.
  Airplane=0, forced idle=false, standby=10, stay-on=15; camera/notifications
  granted восстановлены. Диагностический loopback profile остался, но без моста
   offline; самостоятельный DNS-клиент этим не заявляется. Msgd healthy/pong.

## Самостоятельный DNS checkpoint (2026-09-30)

- APK/core now embed the pinned C client with immutable certificate/Noise pins
  in authenticated local profile; bootstrap bearer is not copied into that profile.
  Default-network numeric resolvers selected explicitly, no guessed public fallback.
  Current unsigned arm64 release with the embedded carrier: 5 101 800 bytes
  (~4.87 MiB); previous 2.78 MiB figures above belong to the earlier TCP build.
- Реальный Android `.gate` enrol→native stop/restart→same identity reconnect,
  refill и two fetch passed. Основной package не удалялся/не очищался.
  Пропущенная попытка из-за отсутствовавшего fixture не считалась PASS;
  повтор после передачи private fixture прошёл actual result code0.
- Wrong full carrier certificate on live DNS→typed PinMismatch before enrol;
  wrong Noise key with valid carrier→Transport before bearer authentication;
  оба временных account остались unenrolled. Actual main identity unchanged.
- Moto↔Linux native peer E2E messages through recursive DNS in both directions.
  Это native-peer gate, не два Android аппарата. После пользовательского
  отключения USB виден только Wi-Fi ADB, reverse mappings пусты.
- Real no-network queue before Wi-Fi-only transition; затем SIGKILL настоящего
  app PID и manual restart. Same encrypted ciphertext hash/account/inbox retained,
  queued→accepted; DNS recipient received1 then0, all skipped counts0.
  Airplane0/Wi-Fi1/mobile-data1/Bluetooth0 восстановлены/подтверждены.
  Дальше Wi-Fi не отключать без safe control: он единственный ADB путь.
- Empty fetch report erroneously returned cursor0 after a non-empty batch.
  Host e2e RED reproduced; fix sends the existing zero-count ACK to observe
  durable server cursor. Live Android repeat fetch + dedup now passes.
- Explicit same-resolver native restart preserves account/pins/inbox/outbox.
  Foreground UI commands also refresh selected DNS when FGS is disabled.
- Concurrent first profile import: 32-caller regression originally accepted
  both different pins. Compare-and-save now uses one BEGIN IMMEDIATE transaction;
  one immutable profile wins, the other key is rejected; RED→GREEN.
- Real API35 FGS accepted `specialUse` type0x40000000, repeated DNS polls passed;
  correct subtype disclosure and stop/onTimeout handling present. This is not
  six-hour endurance or Play-policy approval. >30-min DNS, Doze/Standby,
  queries/battery/PSS, restricted egress, physical pair/mobile and optical gates
  remain unverified until their own evidence.

## Запреты (проверено кодом, не аппаратом)

- Нет WebRTC-стека (только DNS-relay по ARCH): `grep -ri webrtc android/ crates/core` пуст.
- Нет групп/typing/link-preview, cloud-backup (`allowBackup=false`),
  автозагрузки/автобута (нет BOOT_RECEIVER), SQLCipher, второго FGS-режима
  (один DmsgService; «Экономия» — только интервал опроса).
- UniFFI: только команды/события; списки — пагинацией; PCM/сырые курсоры
  через границу не ходят; ciphertext наружу не отдаётся.

# K4 gate-чеклист: ручные проверки на аппарате

Фаза R4 кодом закрыта (фасад + экраны + миграция + биндинги + unit/моки).
Ниже — что осталось ручным gate: без аппарата эти пункты НЕ пройдены,
маскировать их сборкой запрещено.

## Кодовое evidence (эта среда, 2026-09-29)

- `cargo test -p dmsg-core`: lib 31 OK (26 K1–K3 + 5 ffi), интеграция
  auth 2 OK, e2e 1 OK, enrol 4 OK. K1–K3 не сломаны.
- `DMSG_GEN_BINDINGS=1 cargo test -p dmsg-core --test gen_bindings`:
  `android/app/src/main/java/uniffi/dmsg_core/dmsg_core.kt` (3306 строк).
- `./gradlew assembleDebug`: BUILD SUCCESSFUL
  (AGP 8.13.2 + Gradle 8.14.3 + KGP 2.2.20, compileSdk 37, minSdk 26,
  build-tools 37.0.0, ANDROID_HOME=~/Android/Sdk).
- `./gradlew testDebugUnitTest`: 7/7 OK (QrGate 3, Paging 3, Plan 1).
- APK debug: `app-debug.apk` 6 155 721 байт (~5.9 MiB), package
  org.dmsg.client v0.4.0-k4. Без `libdmsg_core.so` (см. NDK ниже).

## Среда: что есть / чего нет

- Есть: Android SDK (platform android-37, build-tools 37.0.0, platform-tools),
  лицензия SDK принята, Gradle-дистрибутивы в кэше, Rust-таргет
  aarch64-linux-android + cargo-ndk.
- Нет: NDK (ни в SDK, ни в системе) → `libdmsg_core.so` под arm64 НЕ собран,
  в APK его нет; экраны при отсутствии либы деградируют явно
  («core missing», см. Core.kt), а не падают.
- Нет: устройства/эмулятора (`adb devices` пуст) → установка, запуск,
  PSS/startup/батарея не измерены.

## Ручной gate (статус: всё PENDING, нужен аппарат + NDK)

| # | Проверка | Как проверять | Ожидание |
|---|----------|---------------|----------|
| G1 | Forced Doze + App Standby | `dumpsys deviceidle force-idle`, standby bucket rare; ждать 2 опроса FGS | Опросы редеют/пропадают честно, UI показывает состояние; без обещаний мгновенных входящих |
| G2 | Экран выкл. длительно | Выкл. 30+ мин в normal-режиме, затем вкл. | FGS жив (если не прибит OEM), пропущенное подтягивается fetch по cursor, дедуп без дублей |
| G3 | Смерть процесса (НЕ force-stop) | `am kill` / LowMemoryKiller | Перезапуск вручную; inbox/outbox персистентны; retry тем же ciphertext |
| G4 | Force-stop | force-stop из настроек | FGS НЕ воскресает сам (START_STICKY не переживает force-stop); это задокументировано, не маскируется |
| G5 | no-GMS аппарат | Аппарат без Play Services | Всё работает (FCM-зависимостей нет — проверить отсутствие gms в APK) |
| G6 | Отказ разрешений | Запретить камеру/уведомления | Сканер показывает явную ошибку доступа; FGS молча живёт без уведомлений-ошибок (SecurityException глотается) |
| G7 | Битый/oversized QR | QR с мусором, обрезанный, >8 KiB | Явная строка ошибки («битый QR: …» / oversized), enrol/add не вызываются |
| G8 | Очистка данных / переустановка | Clear data; reinstall без sealed-копии; reinstall с sealed-копией | Потеря identity показана строкой reinstall_loss; с sealed-копией — unseal и продолжение |
| G9 | Пороги | Release APK ≤ 25 MiB; idle PSS ≤ 100 MiB; cold start; скролл 500+ сообщений | Факты вписать сюда; бюджеты — не обещания |
| G10 | Подмена identity e2e | Второй QR того же ID с другими ключами | `identity_changed`, отправка СТОП до явного confirm (Profile-экран) |
| G11 | no сети | Авиарежим: send/retry/fetch | Явные transport-ошибки; outbox queued сохраняется; повтор после сети шлёт тот же ciphertext |

## Запреты (проверено кодом, не аппаратом)

- Нет WebRTC-стека (только DNS-relay по ARCH): `grep -ri webrtc android/ crates/core` пуст.
- Нет групп/typing/link-preview, cloud-backup (`allowBackup=false`),
  автозагрузки/автобута (нет BOOT_RECEIVER), SQLCipher, второго FGS-режима
  (один DmsgService; «Экономия» — только интервал опроса).
- UniFFI: только команды/события; списки — пагинацией; PCM/сырые курсоры
  через границу не ходят; ciphertext наружу не отдаётся.

# Проект 53 — DNS-мессенджер (протокол dmsg) поверх отдельного развёртывания slipstream
Бэкенд S0–S3 на n-de2:/opt/srv/53; Rust core и Android shell — в `crates/core/` и `android/`. Статус приёмки клиента — `docs/goals/k4-gate-checklist.md`.

## Map
- `ARCHITECTURE.md` — инварианты: переиспользование C-транспорта, границы v1, crypto-слои
- `WORK_PLAN.md` — порядок M0–M7, ворота приёмки, раскладка репозитория (`crates/`, `deploy/`, `docs/`, `vendor/`)
- `docs/protocol.md`, `crates/server/README.md`, `android/AUTH_GATES.md` — wire2/account-auth, CLI и изолированные device gates
- `android/SELF_SERVICE_INVITATION_GATES.md` — account-owned signup invites, private PNG Share/image picker; contact QR не переиспользовать
- `crates/core/R18_API.md` — timeline vs local-ingest history, exact status и dialog summaries для native UI
- `crates/core/CALLS_API.md`, `docs/goals/2026-10-11-call-client-apk.md` — call actor/JNI/Telecom ownership и текущая приёмка; `live_media/` общий engine, `voice_probe/` только fixtures
- `.local/slipstream` — gitignored исследовательский checkout `feat/rust-parity-ab`, не править и не вендорить копипастом
- `.local/dns-delegation.md` — gitignored детали делегирования поддомена мессенджера; фактические имена/IP только там, в трекаемые файлы не вносить

## Rules
- Не трогать соседний интернет-туннель. Messenger client/carrier используют одну закреплённую Git-ревизию транспорта; pins/secrets/data сохранять
- Пользователь разрешил редактировать исходники Slipstream, включая DNS/QUIC/congestion/scheduler, для исправления DNS-связи (2026-10-12); прежние запреты в исторических целях сняты для этой работы. План — `docs/goals/2026-10-11-call-client-apk.md`; embedding и messenger logic остаются разделены
- Держать границы v1: 1 authoritative endpoint, 1 устройство на аккаунт, только лички, пилот ≤16 устройств; без федерации/HA/групп/мультиустройства/видео/ботов по ARCHITECTURE.md §1
- Секреты только read-only файлами, не в Git/образ/логи/argv; данные — в volumes (ARCHITECTURE.md §4)
- Не коммитить `.local/`, `.opencode/` — они в `.gitignore`
- `53-opendesign/` — разрешённый пользователем дизайн-источник Light/Square; HTML не встраивать в APK, demo/calls не выдавать за working API
- Единая auth: `dmsg://server/`, wire2, deployed fresh server7/core10, strict E2Ev2; одна `core_messages` для TEXT/VOICE/Reply/операций, encrypted chunks внутри core.db. Main8 не мигрируется/не очищается автоматически; old-schema/plain→sealed/legacy decode запрещены, несовместимый rollout только по явному разрешению
- Calls: один NativeClient, отдельные Noise owners signal/RTP/SRTCP; keys через ephemeral Olm, без call rows/history/outbox. Audio и Telecom принадлежат DmsgService, не Activity; routing через Telecom, не параллельные AudioManager setters
- Build/diagnostic tools — clean allowlist environment без credentials; не печатать env. Внешнему disposable Rust harness свой `CARGO_TARGET_DIR`, не перезаписывать workspace host-cdylib/rlib
- `connectedDebugAndroidTest` удаляет target package/Keystore: только `-PgateInstall=true` (отдельный `.gate`). Main dev reset допустим по явному разрешению; остальные identity сохранять. Device gates вручную по методам `am instrument`, не считать label-only install application acceptance.
- Kotlin UniFFI bindings не править вручную; генерировать host-cdylib тестом ниже. USB/SSH DirectTCP smoke не выдавать за Android DNS acceptance.

## Verify
- `git status --short --branch` — перед каждым коммитом только intended-файлы
- `cargo build -p msgd && cargo test --workspace` — сервер нужен живым core integration harness
- `cargo build -p dmsg-core && DMSG_GEN_BINDINGS=1 cargo test -p dmsg-core --test gen_bindings` — перегенерация bindings
- `ANDROID_NDK_HOME=<NDK r28+> sh android/build-native.sh` — arm64 native; бинарники только в build/, clean env без credentials
- Из `android/`: `ANDROID_HOME="$HOME/Android/Sdk" ANDROID_SDK_ROOT="$HOME/Android/Sdk" ./gradlew testDebugUnitTest assembleDebug assembleRelease` — сборки и JVM; runtime gates отдельно
- `python3 android/dev-install.py --serial "$MAIN_SERIAL" [--reset-data]` — main dev install/startup; reset только с разрешением, DNS/onboarding отдельно по `android/AUTH_GATES.md`
- `sh deploy/build-apk.sh --client /absolute/public-profile.txt` — подписанный non-debuggable `53.apk`; без `--client` dev debug. Нужен gitignored `android/keystore/debug.keystore`, передаваемый read-only BuildKit secret, не COPY
- Транспортные проверки — из закреплённой ревизии по её докам, не изобретать команды

## Docs
- `ARCHITECTURE.md` — читать перед любым протокольным/крипто/медиа-решением
- `WORK_PLAN.md` — читать перед стартом этапа; приёмка этапа по его секции `Готово, когда`

## Commits
- Формат: `<type>(<scope>): <description>` + пустая строка + `Changes:` с 2–4 буллетами; однострочные только для тривиальных правок
- Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`

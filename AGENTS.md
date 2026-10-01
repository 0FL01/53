# Проект 53 — DNS-мессенджер (протокол dmsg) поверх отдельного развёртывания slipstream
Бэкенд S0–S3 на n-de2:/opt/srv/53; Rust core и Android shell — в `crates/core/` и `android/`. Статус приёмки клиента — `docs/goals/k4-gate-checklist.md`.

## Map
- `ARCHITECTURE.md` — инварианты: переиспользование C-транспорта, границы v1, crypto-слои
- `WORK_PLAN.md` — порядок M0–M7, ворота приёмки, раскладка репозитория (`crates/`, `deploy/`, `docs/`, `vendor/`)
- `docs/protocol.md`, `crates/server/README.md`, `android/AUTH_GATES.md` — wire2/account-auth, CLI и изолированные device gates
- `crates/core/R18_API.md` — контракт локальной истории, exact status и dialog summaries для native UI
- `.local/slipstream` — gitignored исследовательский checkout `feat/rust-parity-ab`, не править и не вендорить копипастом
- `.local/dns-delegation.md` — gitignored детали делегирования поддомена мессенджера; фактические имена/IP только там, в трекаемые файлы не вносить

## Rules
- Не трогать рабочее развёртывание туннеля: отдельный поддомен/IP/сервер, одна ревизия транспорта через Git (submodule/subtree) — M0
- Не переписывать DNS/QUIC/congestion/scheduler; делить embedding-адаптер и логику мессенджера
- Держать границы v1: 1 authoritative endpoint, 1 устройство на аккаунт, только лички, пилот ≤16 устройств; без федерации/HA/групп/мультиустройства/видео/ботов по ARCHITECTURE.md §1
- Секреты только read-only файлами, не в Git/образ/логи/argv; данные — в volumes (ARCHITECTURE.md §4)
- Не коммитить `.local/`, `.opencode/` — они в `.gitignore`
- `53-opendesign/` — разрешённый пользователем дизайн-источник Light/Square; HTML не встраивать в APK, demo/calls не выдавать за working API
- Единая auth: `dmsg://server/`, wire2, server schema5/core schema6. Не возвращать ENROL/token fallback или old-schema migration; dev DB wipe/несовместимый rollout только по явному разрешению
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
- Транспортные проверки — из закреплённой ревизии по её докам, не изобретать команды

## Docs
- `ARCHITECTURE.md` — читать перед любым протокольным/крипто/медиа-решением
- `WORK_PLAN.md` — читать перед стартом этапа; приёмка этапа по его секции `Готово, когда`

## Commits
- Формат: `<type>(<scope>): <description>` + пустая строка + `Changes:` с 2–4 буллетами; однострочные только для тривиальных правок
- Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`

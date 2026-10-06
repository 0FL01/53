# Goal: полный EN/RU Android UI, автоматический язык и English fallback

Status: active
Source: пользователь выбрал «Автоматически» и «English» fallback, утвердил предложенный план: «реализовать и коммит, установка проверка».
Last updated: 2026-10-06

## Objective
Все тексты, которыми владеет Android-приложение, показываются на первом подходящем языке Android (EN/RU), English если совпадения нет. Без ручного переключателя, без смены auth/data/transport contract.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract
- R1: Полный English default и русский перевод всех пользовательских UI строк (экраны, accessibility, ошибки, предупреждения и уведомления); язык выбирает Android, не собственный resolver.
  - Source: утверждённый план §§1–3 и ответы о выборе языка/fallback.
  - Acceptance: EN/RU и неизвестная локаль; списки en/ru, ru/en, fr/ru дают ожидаемый язык, нет смешанного языка в app-owned text. Логин/пароль остаются простыми, пример только Signup.
  - Primary evidence: resource/lint checks + selected physical locale-resource/Activity gate.
  - Status: verified
  - Evidence: 276 matching EN/RU translatable keys, positional format parity; scoped localization lint PASS. Physical preferredLocalesAndSafeErrorsOnlyInGatePackage covers en/ru/fr/en,ru/ru,en/fr,ru (after native resource packaging fix); English/Russian actual Activity labels/recreation PASS. englishLargeFontDisclosuresOnlyInGatePackage PASS at 200% Configuration font fixture.
- R2: Ошибки/статусы сохраняют конкретный смысл и security guarantees, не локализованные payloads или пользовательские данные.
  - Source: план §2 и §3.
  - Acceptance: resource ID каждой конкретной UI ошибки, неизменные ErrorKind/validation/secret cleanup; Queued/Accepted/Delivered не становятся read receipt; generic/internal errors не выводят raw payload. ViewModel не держит готовую строку старого языка или Activity/Context.
  - Primary evidence: ближайшие JVM error/status/auth tests + device bilingual security/error assertions.
  - Status: verified
  - Evidence: JVM52 PASS (validation/error/status/resource IDs, no payload echo); physical bilingual safe errors/password short-vs-long/Keystore/pin/status/trust assertions PASS. retainedSendStateRendersCurrentResourcesOnlyInGatePackage confirms uncertain/draft and late Saved+Delivered action renders either locale without retaining Activity/resources.
- R3: Изменение locale обновляет UI и активное notification/channel, не меняет delivery/runtime state.
  - Source: план §4.
  - Acceptance: язык после configuration change, сохранённые draft/uncertain/pending/counts, credentials wiped по существующему lifecycle; same notification/channel IDs и prefs, single worker/native path не перезапускаются из-за locale.
  - Primary evidence: selected .gate configuration/notification gate; no main system-language mutation.
  - Status: verified
  - Evidence: authConfigurationClearsSecretsAndUpdatesLabelsOnlyInGatePackage: actual .gate OS app-locale RU→EN Activity recreation, password/invitation wiped, no signup. notificationLocaleRefreshKeepsWorkerAndCountOnlyInGatePackage: actual live DNS account, same Java poll Thread/ConnectionFacts revision/Ready/account/channelID+importance, RU title/body/channel after config; count7 explicitly synthetic runtime fixture, not seven received messages. System/main locales untouched, fixture restored in finally.
- R4: JVM/debug/release/test APK и согласованный lint переводов/форматирования green; основной APK установлен и identity/history/keys/key-resume проверены, intended commit/docs.
  - Source: пользователь «коммит, установка проверка» и план criteria.
  - Acceptance: locale-aware existing gates/dev installer without weakening startup; compatible main install-r без reset, exact artifact, preserved encrypted identity/history/wrapped key/install identity.
  - Primary evidence: clean JDK/SDK gates + selected USB device tests + before/after main proof + diff/commit.
  - Status: in_progress
  - Evidence: JVM52/debug/release/test/main export PASS; scoped translation lint PASS. Seven selected USB API35 gates PASS/no skips. Main installed-r/no-reset; exact APK/native/public asset, automatic English Dialogs and actual DNS key resume/localized diagnostics verified. Before/after encrypted identity/account/history/contacts/wrapped key/UID/first-install/system+main locale proofs unchanged. Initial installer readiness timeout after successful install resolved by subsequent actual UI/DNS proof without reinstall/reset; no guessed cause or full-installer PASS claimed. Own invite revoked/removed, gate apps/locale overrides/secret and raw diagnostic fixtures removed. Intended commit pending.

## Constraints / Non-goals
- Никаких auth/wire/schema/pin/backend-code/topology/policy/native/C transport changes; только собственные disposable signup/invitation fixtures через существующий server control API для FGS/key-resume gate. Не переводить пользовательский текст/aliases/logins/IDs/QR payload/domains/fingerprints/logs/CLI. No credentials in env/argv/Git/logs.
- Нет in-app picker, locale prefs/DB, system per-app language UI (`localeConfig`), translation dependency/service/registry, new layers или strings-to-key matching. Только std Android resources.
- Main identity не сбрасывать. Destructive fixtures строго .gate, exact-method instrumentation. OS language телефона не менять без отдельного разрешения: тесты используют configuration contexts и только disposable .gate OS per-app locale fixture (API33+) с восстановлением/удалением fixture; это не shipping language picker и не main override.
- Native/FFI validation/types, concurrency/Stop/ACK/outbox guarantees сохраняются. Незавершённые auth secrets на locale lifecycle не сохраняются и не переотправляются.

## Change Envelope
- Android res/values (English full default), values-ru, layouts; Kotlin UI Activities/NativeUi/TextUiState/service; DmsgError/FfiErrors and local error producers (resource IDs, no Context in blocking core/FFI).
- Closest JVM/instrumentation tests, locale-aware android/dev-install.py, current docs/WORK_PLAN; ignored private evidence/APK/build outputs. No unrelated cleanup/format/refactor.
- Native Gradle init-script for the explicitly requested translation/formatting lint only (`android/localization-lint.init.gradle`); production full lint config/baseline untouched.
- build.gradle.kts resourceConfigurations EN/RU: minimal R1 envelope expansion after physical fr,ru failure due to merged dependency French resources; no custom resolver/locale override.

## Current Checkpoint
- Closes: R4 compatible main installation/commit.
- Next: commit reviewed intended code/tests/docs, then mark closure complete with commit evidence; no further implementation or verification expansion.
- Expected evidence: commit includes only approved Android localization paths/docs; design PNG/local credentials/build artifacts excluded.

## Current State
- Baseline: master519b059, tracked clean; only user untracked design PNG untouched. USB device online; current main identity preserved from prior objective.
- Blocker: none.
- Resolved: R1–R3. Debug/release/test APK builds, scoped lint, JVM52 PASS; seven exact selected physical tests PASS/no skips (five LocaleGates + existing file signup/reopen over actual recursive DNS).
- Next: R4 intended commit. Main rollout and preservation proof complete; own fixtures cleaned, main foreground relaunched. No backend code/config/service restart.

## Material Decisions
- English in default values, RU values-ru; selection uses Android preferred language list, no custom language logic.
- DmsgError receives optional @StringRes UI message ID; raw/internal string constructor retained but raw payload never rendered. Resolve translations at rendering, do not collapse distinct errors by ErrorKind alone.
- ChatMemory action becomes Resources-to-text function capturing data only; notification uses existing channel/ID and retained runtime count on configuration change, no worker/tunnel restart.
- Full lintDebug обнаружил 14 старых NewApi/camera-opt-in ошибок (generated UniFFI Cleaner, неизменённая тема и существующие scanner callers). Source plan требует lint переводов/форматирования, не исправления всей lint debt: проверяется отдельный checkOnly invocation без изменений production lint config/baseline. Full-lint failure не выдаётся за PASS и соседние ошибки не исправляются.
- Physical preferredLocalesAndSafeErrorsOnlyInGatePackage: EN/RU/FR/en,ru/ru,en passed, fr,ru returned English instead of RU because AppCompat contributes French assets. Restrict packaged locale resources to EN/RU in existing Android DSL (R1 blocker, no extra locale logic).

## Completion
- Not complete.

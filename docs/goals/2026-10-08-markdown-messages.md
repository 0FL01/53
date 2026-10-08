# Goal: Markdown и лимит 4000 символов в сообщениях

Status: active
Source: пользователь: «добавлении поддержки markdown в сообщения», «лимит символов ... на 4к»; аудит плана; 2026-10-08: «Делай копию плана в цель и итеративно реализовать и коммит пуш и тесты на телефоне», «fresh db возможна, что бы не городить миграции».
Last updated: 2026-10-08

## Objective
Native Markdown в TEXT bubbles, исходник без преобразований, единый предел 4000 Unicode scalar values для TEXT/EDIT. Проверить live host E2E и настоящий Android UI на свежем изолированном `.gate`; сохранить рабочие Main/backend и закоммитить/запушить intended изменения.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Native Markdown для incoming/outgoing TEXT с сохранением исходника и message actions.
  - Source: исходный запрос и исправленный план после аудита.
  - Acceptance: bold/italic/strikethrough, inline/fenced code, quotes/lists/headings/links; обычные переносы строк. HTML буквально, images alt-only без загрузки; разрешённые absolute HTTP(S) links открываются только через explicit selection action. Edit получает raw source; системный Copy копирует выбранный отображаемый текст, unchanged refresh сохраняет selection.
  - Primary evidence: renderer/view и fresh native ChatActivity exact-method instrumentation gates.
  - Status: pending
  - Evidence: —
- R2: Единый предел 1–4000 Unicode scalar values исходника для новых TEXT/EDIT.
  - Source: «4к символов», конкретизация исправленного плана.
  - Acceptance: ASCII/Cyrillic/supplementary Unicode и Markdown markers считаются одинаково в Kotlin/Rust. 4000 проходят, 4001 отвергаются. No trim/normalization/truncation. Счётчик N/4000, overflow draft сохраняется, Send/Save блокируются. Malformed UTF-16 не заменяется при FFI encoding. Encode/decode/Core/FFI используют тот же контракт; ciphertext/frame limits и messaging atomicity сохранены.
  - Primary evidence: protocol/core/FFI boundary и Olm fit tests, live message_actions, JVM policy tests, actual composer gate.
  - Status: pending
  - Evidence: —
- R3: Сборки/регрессии и тесты на телефоне без повреждения existing identities/data.
  - Source: последний запрос, AGENTS.md и android/AUTH_GATES.md.
  - Acceptance: clean-env msgd/workspace, arm64 NDK, JVM/debug/release/gated-test APK зелёные; выбранные `.gate` методы проходят на явно выбранном доступном телефоне. Только свежая изолированная DB, не Main reset/install. Host/local UI evidence не выдаётся за Android DNS delivery acceptance.
  - Primary evidence: результаты команд и exact-method instrumentation, APK/native package check.
  - Status: pending
  - Evidence: —
- R4: План в цели, актуальные контракты, scoped commits и push.
  - Source: последний запрос.
  - Acceptance: эта цель содержит copied plan/current evidence/closure; protocol/product/runbook актуальны; intended изменения закоммичены и pushed, без user PNG, private fixtures, secrets/build outputs.
  - Primary evidence: git status/diff/log, push result и remote commit.
  - Status: in_progress
  - Evidence: цель создана; tracked baseline clean, unrelated untracked design PNG оставлен нетронутым.

### Constraints
- 4000 = Unicode scalar values исходника, не bytes/UTF-16 units/grapheme clusters. Пробелы/newlines/Markdown markers входят в count; whitespace-only сохраняет прежнюю допустимость.
- Raw text в E2E/DB/edit/reconciliation не менять. Durable history не перепроверять/не обрезать; retry передаёт прежний ciphertext.
- Fresh core9/server6; без migrations/plain-to-sealed/legacy decode/autowipe. Fresh-DB разрешение применяется к owned disposable `.gate`, не к Main/другим identity или рабочему серверу.
- Сохранить crypto/schema/wire layouts, TX/CAS/ACK/status/chronology, voice flow и transport revision. Не менять production tunnel/backend/делегирование.
- Build/diagnostics — clean allowlist env. Секреты только private read-only files, не env/argv/logs/Git. Kotlin UniFFI bindings не править вручную.
- Device instrumentation только exact method `.gate`; connected tests на Main запрещены. Временные clipboard/font/orientation изменения восстановить.

### Non-goals
- Dialog preview normalization, toolbar/WYSIWYG/live preview, Copy source, Telegram MarkdownV2/tables/spoilers/math/syntax highlighting/link previews/image loading.
- Новый gesture/movement framework, test framework, compatibility negotiation/migration, DNS infrastructure и production rollout. Обычный tap links не обещается: открытие через selection menu.

## Change Envelope
- Protocol lib/e2e и существующие core chat/FFI/Olm tests/live message_actions.
- Android MessageTextPolicy/MessageMarkdown, ChatActivity/NativeUi, chat layout, EN/RU strings/errors/IDs, pinned Markwon core + strikethrough 4.6.2, relevant JVM/fresh native gates.
- ARCHITECTURE §8, WORK_PLAN current limits, docs/protocol.md, android/AUTH_GATES.md и эта цель.
- Binaries только target/build; private disposable fixtures вне Git. Не трогать unrelated `53-opendesign/` PNG, `.local/slipstream`, Main/Pacman/рабочий backend.

## Copied implementation plan (audited KISS version)
1. **Единый Rust/E2E limit.** TEXT_CHAR_MAX=4000, UTF-8 byte bound=16000; один validator в existing E2E module. TEXT/EDIT encode/decode и четыре existing Core/FFI checks используют его с прежним BadText/error ordering. Decode: byte bound → borrowed UTF-8 → scalar count → allocation. Envelope bound=16074, frame/cipher limits unchanged; schema/TX/retry/projection не менять. Проверки 4000/4001 ASCII и multibyte, authenticated invalid inbound rollback/no invalid seq in ACK. Реальный TEXT/EDIT prekey/normal Olm wire fit; normal требует ответного E2E exchange, не server ACK. Один existing live TEXT→EDIT→reopen/retry сценарий exact raw/cipher/MID/order/time.
2. **Android composer.** Один pure MessageTextPolicy: count и well-formed UTF-16; одинаковая validation send/save до dispatch/pending. Счётчик N/4000 и понятная EN/RU overflow indication, paste не обрезать. Маленький общий updater из watcher/render без history rebind. Existing trust/pending/uncertain/CAS/voice guards сохраняются. Hidden Send при empty normal draft сохраняет прежний enabled/readiness; submission требует nonempty, Save также. Counter не отнимает composer width и не исчезает с chat_actions при IME. FakeFacade send/edit используют policy.
3. **Native Markdown.** Один MessageMarkdown wrapper/renderer на HistoryAdapter; Markwon core/strike4.6.2, SoftBreakAddsNewLine, literal HTML/image alt, filtered HTTP(S) link spans/resolver без URL logs. Один guarded parse→iterative depth≤64→render путь; depth overflow/StackOverflowError возвращает полный raw, не ловить Throwable. Selectable TextView/explicit movement; autoLinkMask=0; native selection menu Open link для одного разрешённого span (incoming и outgoing), Edit/Delete eligibility сохраняется. No custom gestures. TEXT only, no model/voice/unchanged-refresh changes; previews unchanged.
4. **Минимальная приёмка.** Existing protocol/core/live/JVM tests расширяются без нового harness. Два exact-method MarkdownMessagesGatesTest: renderer/view без DB и fresh native ChatActivity (paste/counter4000/4001/edit/cancel/recreation/actual Copy/Open link/selection after ticker/mixed TEXT-VOICE/large font+landscape IME). Native fixture не выдаёт successful DNS Send/Save без настоящей session/profile. Финальные clean-env cargo build msgd + test workspace; NDKr28+ arm64; JVM/debug/release/gated instrumentation APK. Codegen только при API/checksum/staleness changes. Device install только `.gate`, run exact methods. Update current contracts/runbook, scoped commits/push, один closure.

## Material Decisions
- 2026-10-08: fresh DB явно разрешена; migrations не добавлять. Узкая безопасная граница — disposable `.gate`, Main/production не затрагивать.
- 2026-10-08: новый scalar-limit несовместим со старыми byte-limit clients в обе стороны, включая уже сохранённый недоставленный ciphertext. Приёмка на fresh clients; future cutover требует отдельного решения для pending/mailbox, не просто согласованного обновления. E2E version/layout не меняются.
- 2026-10-08: статический расчёт pinned vodozemac0.11.0: max EDIT/prekey ciphertext≤16244 при CIPHERTEXT_MAX16304; запас60bytes. Runtime fit ещё не проверен.

## Current Checkpoint
- Closes: R4 (план) и подготовка R1/R2.
- Smallest next action: scoped plan commit, затем независимые Rust limit / Android composer / renderer workstreams; проверить доступ к телефону.
- Expected evidence: goal committed; узкие boundary/render checks; наблюдаемый ADB serial.
- Stop or replan if: конкретный required gate показывает нарушение frozen outcome; без расширения requirements.

## Current State
- Resolved: план скопирован; baseline read-only checks, existing constraints и tools проверены.
- Last relevant evidence: 2026-10-08 initial clean-env `adb devices -l` вернул пустой список; альтернативный configured server/socket и USB доступ ещё не исследованы. Это не окончательно доказанный blocker.
- Blocker: none proven; реализация независима от device discovery.
- Next: implement/verify R1/R2, диагностировать ADB, затем R3.

## Checkpoint History
- 2026-10-08: цель active, audited plan copied; user fresh-DB authorization recorded, no migration/Main rollout.

## Completion
- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.

# QA — 53 / R17 auth, current R18 text + retained future-call regression

Проверено 01.10.2026 в clean allowlist environment. Результаты относятся к byte-identical `prototype.html` / `index.html`, не к Android APK/backend/DNS acceptance или настоящим звонкам.

## R17 auth / current R18 text-state QA

**492 геометрические конфигурации, 82 functional checks — всё прошло.** 41 fixtures × два режима × 360/390/412 CSS px × 100/200% текста. 19 базовых auth/scan/connection состояний + 18 safe error fixtures + 4 current R18 summaries/history/unknown-status/queue fixtures. Проверены root overflow, боковые границы button/input/textarea/summary, targets ≥48×48, radius 0, app shadows 0, clipping заголовков/подписей/параграфов/CTA. Основные CTA и replacement cancel достижимы прокруткой на 360 px / 200%.

Functional evidence: paste/scan только preview до согласия; configureDns → policy после accept; closed default и policy retry; invitation только signup invite_only; login normalization без trim, password UTF-8 byte/control validation без нормализации; reveal/hide; pending dedup; success/cancel/stale reply cleanup; key-only resume/revoked; первый LOGIN challenge non-mutating; cancel/back не посылают второй LOGIN; confirm с expected-old key; CAS conflict требует нового proof; duplicate/late confirm не применяется; все 18 safe error copies; no credential payload in traces; DNS без manual addr/crypto полей; current-contract media controls/lab/board/deep link guard.

R18 current-mode evidence: local alias/preview/time/unread видимы; optional alias/preview/time имеют ID/empty fallback; ID сохраняется в contact card; incoming/outgoing bubbles и local timestamps доступны; newest-first fixture reverse → chronological; incoming без delivery state/receipt; синтетический offline send виден как queued; retry не дублирует историю и не понижает delivered; unknown не становится delivered. Queue notice ссылается на реализованный historyPage/messageStatus и не выводит доставку из отсутствия строки. Исправлено обёртывание длинного API/status label на 360 px / 200%; geometry expectations сохранены.

API — `../crates/core/R18_API.md`/workspace facade. Separate coordinator evidence: core schema6/65 all-targets/codegen/workspace167; JVM35/current ARM64/APK, Moto API35 physical27 distinct gates/38 passes/0 skips и fresh recursive phone↔native E2E/status/trust verified (`../android/AUTH_GATES.md`). Browser checks сами не подтверждают encryption/storage/physical frontend.

`ReplacementChanged` — synthetic UX-конфликт, не новый enum ядра. Mock принимает только демо-сентинел, не настоящие серверные профили. JSON: `qa-auth-results.json`; screenshots: `mobile-auth-signup-360-large-text.png`, `mobile-auth-replace-360-large-text.png` (просмотрены). Вертикальная прокрутка на 200% ожидаема, CTA не обрезаются.

## Автоматические проверки

**330 геометрических конфигураций, без зафиксированных нарушений проверяемых условий.**

23 экрана целевого интерфейса и все 16 text/auth экранов текущего контракта × 3 ширины (360/390/412 CSS px) × 2 масштаба текста (100/200%) = 234 конфигурации. Предыдущие expectations не ослаблены; contract matrix расширена на новые auth-экраны, failures теперь дают exit 1.

Дополнительно 8 фаз звонка × аудио/видео × 3 ширины × 2 масштаба = 96 конфигураций. Фазы: dialing, ringing, incoming, connecting, active, reconnecting, held, ended.

Проверялось: горизонтальное переполнение корня приложения; выход интерактивных элементов за его боковые границы; размеры app-кнопок минимум 48 × 48 CSS px; нулевой border-radius элементов приложения. Это не полная проверка доступности или всех возможных пересечений текста.

**58 функциональных проверок, все пройдены.** В том числе:

- Разделение текущего контракта и будущих звонков; запрет вызова без ключей/согласия, при смене ключа, блокировке, отсутствии сети или медиаканала.
- Установка, автоответ демо, предварительный mute, мини-полоса в другом чате, раздельные черновики, завершение и правильное расположение нового события в ленте.
- Отложенное разрешение, истёкший входящий, другой вызов во время запроса, повтор приглашения и попытка второй линии; поздний ответ не восстанавливает завершённый звонок.
- Восстановление сохраняет callId/таймер/mute; повтор события не продлевает deadline; истечение срока не вызывает автоперезвон.
- Pending и ошибка маршрута, отсоединение наушников, поздний callback после hangup, восстановление фокуса и Tab/Shift+Tab/Escape в панели.
- Видео только по явному действию; ответ без видео; согласие собеседника не включает локальную камеру; отказ не прерывает аудио; уход в чат приостанавливает локальную камеру.
- Приватное имя в схеме блокировки, устаревшее уведомление, очистка истории без завершения разговора, сохранённый лимит сообщений в байтах UTF-8, геометрия пяти диалогов при 360 px / 200% текста.

В выполненных сценариях не зафиксированы JavaScript runtime errors и сетевые запросы. HTML загружался в DOM headless Chromium через Playwright `set_content` с новым контекстом документа между функциональными сценариями. Это не запуск файла в OpenDesign и не проверка поведения всех браузеров.

## Визуальная проверка

Просмотрены обзор звонков и мобильный видеосценарий на 360 px / 200% текста. Исправлены переполнение входящих кнопок на крупном шрифте и вертикальное сжатие блоков видео. Крупный контент теперь растёт в прокручиваемой области, а завершение остаётся в нижней панели.

`mobile-calls-360-large-text.png` обновлён регрессией R17/R18. Остальные старые v2 snapshots (`calls-preview.png`, `messages-preview.png`, `mobile-calls-360.png`, `video-preview.png`) архивные, не evidence нового onboarding. Все снимки — HTML, не работающий Android.

## Воспроизведение

`tests/test_calls.py` содержит исполняемые проверки. Нужны Python, Playwright и Chromium; путь к браузеру ищется в PATH. При отсутствии системного Chromium используется браузер Playwright, который должен быть установлен в вашей среде.

```sh
env -i HOME="$HOME" PATH="$PATH" python3 53-opendesign/tests/test_auth.py
env -i HOME="$HOME" PATH="$PATH" python3 53-opendesign/tests/test_calls.py
```

Запуск из корня репозитория. `qa-results.json` и `qa-auth-results.json` сохраняют конфигурации/результаты. Скрипты управляют artificial fixtures напрямую и проходят действия кликами/клавиатурой. Оба fail-closed: geometry/check/JS/network failure → exit 1. Зафиксировано 0 JS errors и 0 network requests. Это UI-тесты демонстратора, не интеграционные тесты движка.

## Не проверено и не заявляется

В рамках browser QA не проверены реальные media/E2EE/Android/platform/lifecycle/APK/OpenDesign runtime. Separate native text/auth/recursive DNS/frontend lifecycle/build evidence координатора указано выше. Media, long Doze, second physical Android, no-GMS/16KB runtime, optical camera и permanent signing остаются отдельными, не browser PASS.

Предлагаемые дедлайны и результат автоответа относятся только к макету. Нативная матрица приёмки и нерешённые вопросы находятся в `CALLS_HANDOFF.md` и `CALLS_SPEC.md`.

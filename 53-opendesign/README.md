# 53 — R17 native Light / Square text frontend

**Светлая тема, минимализм, прямые углы.** R17 обновляет public-profile onboarding, policy, login/signup, typed errors и explicit replacement под текущий unified auth/DNS. Native handoff — Kotlin/AppCompat/XML, без HTML в APK. Существующее будущее call/video demo сохранено. Android/Rust/backend этим комплектом не изменяются.

## Открыть и попробовать

Откройте `prototype.html` или его идентичную копию `index.html` в браузере. Дополнительные библиотеки, сервер и API keys для макета не нужны.

Первый экран — «Выберите свой сервер». Нажмите «Вставить код сервера» → «Вставить демо-образец» → «Показать данные сервера». Scan открывает тот же offline preview. Только «Использовать этот сервер» переходит к policy, затем «Войти» или «Создать аккаунт». Все ответы локальные и искусственные (600 ms), DNS не вызывается.

Внешние сценарии дают `open`, `invite_only`, policy/error, login/signup, typed errors и replacement. Для ручного signup invite_only используется `DEMO-INVITATION`; server code — отдельный невалидный wire-сентинел `dmsg://server/DEMO-PUBLIC-PROFILE`. Не вставляйте реальные профили/пароли. Login/open signup не показывают поле invitation. При replacement отмена/назад не отправляют второй LOGIN; explicit confirm моделирует expected-old-key/CAS. Key-resume не хранит пароль.

Для сохранённого future-demo в «Целевом интерфейсе» откройте переписку и нажмите трубку. Автоответ ничего не отправляет в сеть. Медиа не является частью R17 нативного переноса.

На экране разговора попробуйте выключить микрофон, выбрать устройство звука, перейти в переписку, открыть другой контакт и вернуться через общую полосу. Входящий, обрыв, системная пауза и ошибки доступны во внешних сценариях. На мобильной ширине контроллер открывается кнопкой «Тест» над приложением.

Журнал доступен из меню диалогов. Строка события открывает детали, отдельная трубка перезванивает. Внешний пункт «Видео · будущий этап» или переключатель «Показать будущие видеозвонки» включает дополнительный сценарий. По умолчанию видео скрыто.

«Текущий контракт» показывает поддерживаемые auth/DNS и R18 encrypted bidirectional history/exact status/dialog summaries/local alias/time/unread. Все значения в HTML синтетические. Звонки/video/lab/call-board скрыты, media deep links заблокированы; переключение сбрасывает call-session. Exact API и descending page order — `../crates/core/R18_API.md`.

Координатор подтвердил core schema6/65 all-targets/codegen, workspace167 и R19 JVM35/current ARM64/APK green. Separate Moto API35 frontend acceptance27 distinct methods/38 passes/0 skips и fresh remote recursive phone↔native E2E verified; evidence — `../android/AUTH_GATES.md`. Browser QA подтверждает только HTML; media/дополнительная аппаратная матрица этим не закрыты.

## Состав

Главные файлы:

- `prototype.html` / `index.html`: byte-identical автономный прототип, 16 text/auth экранов текущего контракта и 7 future media-групп (23 всего).
- `PROMPT.md`: обновлённое полное задание OpenDesign. `CALLS_PROMPT.md`: отдельное задание именно по звонкам.
- `CALLS_SPEC.md`: сценарии, кнопки, состояния, ошибки, приватность, фон, история и видео.
- `CALLS_HANDOFF.md`: будущие модели и команды, граница UI/сигналинга/медиа/Android, первичные источники платформы.
- `DESIGN.md`, `HANDOFF.md`, `CODE_REVIEW.md`: R17 source, R18 storage API, R19 native UI; separate R20/R21 physical/recursive evidence.
- `tokens.json`, `tokens.css`, `calls-state-machine.json`: токены и проектная машина состояний.

`mobile-auth-signup-360-large-text.png` и `mobile-auth-replace-360-large-text.png` — текущие R17 screenshot-референсы. Старые `preview.png`, `calls-preview.png`, `messages-preview.png`, `mobile-calls-360.png`, `video-preview.png` — архивные synthetic v2, не актуальные auth/DNS-состояния. `mobile-calls-360-large-text.png` обновляется регрессией. `QA.md`, `qa-auth-results.json`, `qa-results.json`, `tests/test_auth.py`, `tests/test_calls.py` содержат evidence и воспроизводимые проверки.

## В OpenDesign

Распакуйте папку и дайте агенту доступ к ней. Используйте `DESIGN.md` как дизайн-контракт. Для продолжения начните с `PROMPT.md`; при доработке только звонков используйте `CALLS_PROMPT.md`.

Короткий стартовый запрос:

```text
Прочитай PROMPT.md, DESIGN.md, HANDOFF.md и CODE_REVIEW.md.
Используй prototype.html как исходный интерактивный референс 53.
Доработай нативный текстовый Light/Square frontend по R17.
Сохрани текущий unified auth и все browser checks.
R18 history/status/summary реализованы; exact API — ../crates/core/R18_API.md.
R19 native frontend acceptance verified отдельно в android/AUTH_GATES.md; HTML synthetic, media future-demo.

Светлая тема, radius=0, elevation=0, подписи на русском.
Не используй iPhone seed. Не превращай прототип в лендинг.
В «Текущем контракте» звонки остаются скрыты.
Не меняй backend, transport, криптографию или generated bindings.
Сохрани byte-identical prototype.html/index.html и обнови handoff.
```

Прочитанный для исходного комплекта встроенный mobile-app шаблон OpenDesign ориентирован на iPhone. Для этой задачи нужен самостоятельный Android-прототип, без переноса этих ограничений. В локальном OpenDesign генерация этого комплекта не запускалась.

## Что действительно работает в этом файле

Локальные paste/scan/preview, policy-aware login/signup, validation, safe typed-error copy, reveal, pending, explicit replacement/cancel/CAS и key-resume fixtures. Также сохранены прежние synthetic call-state transitions и checks.

Медиа, сигналинг, DNS, Rust, камера, микрофон, Bluetooth, настоящие системные уведомления, Android lifecycle и криптография звонков не реализованы и не вызываются. Настройки и журнал хранятся только в памяти страницы. Видео является заглушкой без видеопотока. Данные не извлечены из пользовательской переписки или ключей.

Local aliases, время, bidirectional history и unread в обоих режимах — синтетические значения уже доступных R18 полей. Время относится к этому устройству, unread — к локальному incoming read cursor; delivered не означает прочитано. Unknown status остаётся unknown, не выводится из исчезновения outbox. Дизайн звонков не означает, что Android DNS transport способен передавать голос.

## Проверка

R17/current R18 evidence: 492 auth/text-state геометрические конфигурации + 82 functional checks; retained call regression: 330 geometry + 58 functional. Всё прошло, JS errors/network requests — 0. Python + Playwright + Chromium нужны только для тестов. Из корня репозитория, всегда с clean allowlist:

```sh
env -i HOME="$HOME" PATH="$PATH" python3 53-opendesign/tests/test_auth.py
env -i HOME="$HOME" PATH="$PATH" python3 53-opendesign/tests/test_calls.py
```

Оба теста завершаются ненулевым кодом при failed check, geometry, JS error или network request. Скрипты обновляют QA JSON и screenshots внутри этой папки.

Browser QA не проверяет APK/аудиосвязь/workspace OpenDesign. Separate core/JVM/native и физическая text/recursive DNS acceptance подтверждены координатором выше и в `../android/AUTH_GATES.md`. Будущая media-приёмка — `CALLS_HANDOFF.md`.

# Android handoff — 53

R17 / 01.10.2026. Native Light/Square text UI по `docs/goals/2026-09-29-client-track.md`. Auth/DNS и R18 history/status/summary реализованы; exact API — `../crates/core/R18_API.md`. HTML остаётся синтетическим дизайн-референсом, не нативным runtime.

## Стратегия переноса

R19 визуальные компоненты реализованы в Kotlin/AppCompat/XML слое. Координатор подтвердил 35 JVM проверок и compile green; device runtime pending. R18 core schema6 прошёл 65 all-targets проверок, актуальные Kotlin bindings сгенерированы координатором. Не менять Rust API только ради радиусов/цветов. Compose возможен отдельной миграцией: сейчас он не настроен в Gradle. HTML не встраивать в APK.

Слой экранного состояния отделить от View. UI получает готовые immutable модели; блокирующие facade-команды выполняются вне main thread. Повторные нажатия на send/login/signup/confirm блокируются до результата. Устаревшие ответы lifecycle/paging/auth не обновляют закрытый экран. Пароль и приглашение не входят в saved instance state, prefs, trace или уведомления.

## Компоненты

| Компонент | Текущее место | Что меняется |
|---|---|---|
| AppBar / menu | MainActivity + activity_main.xml | Один основной экран без панели из шести больших кнопок |
| DialogRow / paged list | MainActivity + NativeUi | Реализованный dialogsPage: alias/preview/local time/unread/read cursor и trust fields |
| MessageBlock / Composer | ChatActivity + HistoryWindow | Реализованный historyPage обеих сторон; exact delivery state; chronological reverse/prepend |
| ConnectionStrip / state | DmsgService + TextUiState | Реализованный ConnectionUiState по worker outcomes; dnsStatus — диагностика |
| MyQr | ProfileActivity | Только собственные публичные данные |
| ContactCard / trust gate | ProfileActivity + facade | hasKeys и identityMismatch включены в mapping; CTA следует trust state |
| ProfileImport / Scan / ServerPreview / AccountAuth | ScannerActivity + DmsgFacade | Paste/scan → offline preview → explicit accept → policy → login/signup; replacement отдельно |
| ConnectionSettings | DiagnosticsActivity / Prefs | Пользовательская часть отдельно от advanced |
| OutboxList | OutboxActivity + facade.outbox / retry | Очередь queued/accepted; exact per-message status из historyPage/messageStatus; total не выводить из одной страницы |
| StorageSettings | StorageActivity / SecureStore | Ясно различать cache, snapshot, lost key и restore conditions |

## Реальные операции и предлагаемое поведение

`account()` → зарегистрированность и свой ID; не имя/телефон.
`profilePreview(code)` → domain + публичный fullDER fingerprint; локально, до сети. Публичный `dmsg://server/` содержит domain/fullDER/Noise public key, не приглашение.
`configureDns(code, resolvers)` → только после принятия preview; профиль не нужно собирать вручную из криптополей.
`registrationPolicyDns()` → короткий pinned DNS-запрос после принятия профиля. Loading/error не открывают signup; закрытый default `invite_only`.
`signupDns(login, password, invitation?)` → invitation только при invite_only. В open его не показывать и не передавать.
`loginDns(login, password, expectedDevice = null)` → `Authenticated` или `ReplacementRequired(expectedDevice)`. Password proof с новым ключом не заменяет устройство сам по себе.
Второй `loginDns(..., expectedDevice)` → только после отдельного согласия; сохранить тот же login/password в оперативном pending-состоянии до confirm, не на диске. Cancel/back до confirm очищают pending и не вызывают второй LOGIN. После отправки confirm ждать ответа; не обещать отмену уже отправленного запроса. CAS-конфликт/ошибка возвращают к новому proof, без повторного применения старого согласия.
`account()` + ключевой reconnect/resume → старт уже авторизованного приложения без пароля. Revoked/NotEnrolled — явная ошибка, не автоматический signup.
`dnsProfile()` / `dnsStatus()` / `dnsStop()` / `dnsNetworkChanged()` → connection lifecycle; `dnsStatus()` строковый диагностический API. R19 `DmsgService.connectionState()` возвращает `ConnectionUiState` по worker outcomes, отдельно от R18 storage API. Нет DirectTCP runtime.
`myQr()` → собственный contact QR после auth; в HTML используется DEMO.
`request(id)` → локальный requested контакт, не wire request.
`addQr(uri)` → Added / Unchanged / IdentityChanged; состояния не угадывать.
`accept(id)` → доступно после ключей; blocked не снимается.
`confirm(id)` → отдельное подтверждение смены ключа, не обычная primary-кнопка профиля.
`block(id)` → confirm dialog с предупреждением о необратимости в этой версии.
`send(...)` → сохранять черновик до результата; queued только когда подтверждено ядром.
`retry(...)` → повтор существующих message IDs; не повторный send того же plaintext.
`fetch(...)` → входящие и счётчики пропуска; пропуски не объявлять прочитанными сообщениями.
`reconnect(...)` → состояние операции; возвращает число prekeys, не latency, скорость или DNS health.

## UI data handoff

R18 реализован и проверен: `history_page`, `message_status`, `dialogs_page`, `set_contact_alias`, `mark_read`; core schema6, server schema5/wire2 без изменений. История newest-first, для ленты reverse/prepend; read cursor только по реально просмотренной строке. Точные поля, bounds и cursor semantics — `../crates/core/R18_API.md`, generated Kotlin — источник сигнатур. R19 typed connection UI adapter реализован в Android отдельно от этих storage APIs; device runtime pending.

1. `historyPage(contactId, beforeLocalId?, limit)` → `HistoryPage(rows, nextBeforeLocalId?)`; `HistoryMessage(localId, messageIdHex, contactId, direction, text, localTimestampMs, deliveryState?)`. Защищённая persistent история incoming/outgoing; core владеет хранилищем. Страницы newest-first по local ID descending; reverse каждой страницы, prepend older. Next anchor exclusive; `null` next означает конец. Anchor должен существовать в этом контакте, быть положительным; cross-contact/zero/future → InvalidInput.
2. `dialogsPage(cursor?, limit)` → `DialogsPage(rows, nextCursor?)`; `DialogSummary(contactId, localAlias?, preview?, lastLocalTimestampMs?, localUnread, readCursor, hasKeys, identityMismatch, state)`. Все известные контакты, включая empty/requested/blocked. Order local activity descending, contact ID ascending ties; alias/read/status/trust changes не меняют порядок. Empty имеет `null` preview/time; preview максимум 160 Unicode scalar values. Paging keyset, не frozen snapshot; после send/fetch/contact addition начинать с `null`.
3. `messageStatus(messageIdHex)` → nullable `DeliveryState` (Queued/Accepted/Delivered), ровно 32 ASCII hex, case-insensitive. `null` для unknown/incoming-only; incoming history state тоже `null`. Status persistent/monotonic; transport errors не понижают его. Delivered history остаётся после удаления outbox; outbox-page alone не подтверждает delivered. Нет отдельного нового status-event API.
4. UI-state adapter поверх уже существующих typed FfiError: InvalidCredentials, LoginTaken, InviteRequired/Expired/Revoked/Used, AuthRateLimited, BadQr, InvalidInput, PinMismatch, Revoked, Busy, Transport, Store, Crypto, Protocol. Не разбирать raw exception message и не показывать payload.
5. R19 `ConnectionUiState(serviceEnabled, pollInFlight, lastSuccessAt?, lastFailure?, revision)` и `ConnectionFacts` реализованы по реальным outcomes; не выводить DNS health из FGS или таймера.
6. Нативный lifecycle adapter реализован для configureDns/policy/login/signup; explicit preview/accept сохраняются, ENROL/token/old-schema fallback не возвращаются. Compile/JVM evidence не заменяет device runtime gates.
7. `setContactAlias(id, alias?)`: trimmed 1..128 UTF-8 bytes без controls, `null` очищает. Alias/text шифруются отдельными authenticated field domains ядра; не дублировать plaintext в Kotlin prefs/logs. `markRead(id, throughLocalId)` → monotonic cursor, только реально просмотренная строка контакта (incoming или outgoing); чтение страницы само по себе ничего не отмечает. `localUnread` считает incoming за cursor. Times — локальные queue/decrypt milliseconds, не sender/server time/last-seen. Search макета — in-memory demo, отдельный search API не заявлен; alias/read не являются server directory/read receipts.

Все пять R18 операций локальные синхронные storage calls, выполнять вне main thread. Limits clamp 1..100. Schema6 не выполняет migration или automatic wipe старой БД. Разрешённая пользователем dev data loss не добавляет кнопку сброса рабочей identity в production UI.

## Состояния безопасности

`identityMismatch` блокирует send/retry до explicit confirm. `blocked` не имеет unblock. `requested` без keys не допускает send. Старый/новый fingerprint не показывать до отдельного безопасного public-verification API. Lost Keystore не лечится незаметной генерацией новой identity. Cache wipe не уничтожает историю.

## Геометрия и Android

Все app-owned shapes прямоугольные, elevation 0. Insets status/navigation/IME применяются к интерактивному содержимому. 48 dp minimum touch; 16 sp текст сообщения, масштабирование. System permission dialog и keyboard — платформенные, не custom HTML.

## Что не переносить из демо

Внешнюю панель выбора экранов и сценариев, искусственные timestamps/aliases/unread, DEMO-QR, искусственное изменение network/delivery state, фальшивую камеру. Эти элементы нужны для обсуждения и не являются дизайном production-навигации.

## Дополнение v2: будущие звонки

Сохранены семь групп будущих media-экранов. В R17 референсе 16 text/auth экранов и 7 future media-групп (23 всего); в текущем контракте только 16. Реальный API звонков отсутствует. Расширение нельзя реализовывать переименованием `send()` или `DmsgService.dataSync`.

Отдельный handoff: `CALLS_HANDOFF.md`. Там предложены CallCoordinator, Call UI/ViewModel, signaling/media adapter, Android telecom adapter, команды, immutable состояния, call-журнал и правила гонок. `CALLS_SPEC.md` фиксирует поведение кнопок и сценариев. Этот design-пакет не изменяет исходный код Android/core.

В текущем контракте звонки скрыты. В нативном выпуске флаг недоступен без реального движка; видео имеет отдельный выключенный по умолчанию capability. Не переносить в APK автоответ собеседника, фальшивые endpoint-ы, демо-разрешения и внешнюю панель сценариев.

## Synthetic auth QA и ошибки

`DEMO-PUBLIC-PROFILE`, `demo-server.invalid`, `DEMO-CERTIFICATE-FINGERPRINT`, `DEMO-OLD-DEVICE-KEY` и `DEMO-INVITATION` — обозначения, не валидные wire-профили/ключи/приглашения. Макет принимает только точный демо-сентинел и не вызывает сеть. `A.requests` хранит только названия операций/булевы флаги, без пароля, invitation value и profile payload. `A.mutations` — счётчик синтетического завершения, не запись в БД.

`ReplacementChanged` — UX-сценарий CAS-конфликта, **не заявленный новый FfiError enum**. Нативный adapter должен сопоставить фактический ответ подтверждённого API; неизвестная ошибка → безопасный Protocol/failure. PinMismatch останавливает auth; кнопки обхода нет. AuthRateLimited без server retry-after не получает выдуманный countdown. Invite-errors относятся к signup, не к публичному профилю и не к login.

Проверки/скриншоты в `QA.md`: paste/scan/preview, policy, login/signup, typed errors, replacement/cancel/CAS, stale replies, key-resume, 48 dp-референс и 200% текста. Android acceptance остаётся отдельной нативной проверкой.

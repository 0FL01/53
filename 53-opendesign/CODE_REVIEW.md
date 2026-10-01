# 53 — связь дизайна с кодом

R17, 01.10.2026: сверены актуальные auth/DNS-границы, реализованный R18 API (`../crates/core/R18_API.md`) и R19 native facade/UI workspace. Координатор подтвердил core schema6 — 65 all-targets green и generated-current Kotlin; R19 — 35 JVM/compile green. Device runtime pending. Основа визуального комплекта — архив `53(1).zip`, 29.09.2026; архивные диапазоны строк относятся только к первоначальному чтению. В этой design-задаче проверен HTML в Chromium; физическая APK/backend/DNS acceptance не заявляется.

## Текущий auth/DNS контракт R17

| Экран / переход | Актуальный источник / операция | Семантика |
|---|---|---|
| Вставка / QR сервера | DmsgFacade `profilePreview(code)`, core public server profile | `dmsg://server/`: domain/fullDER/Noise public, без token; offline preview до любых сетевых действий |
| Использовать сервер | `configureDns(code, resolvers)` | Только явное согласие после сверки; не signup и не доказательство связи |
| Policy | `registrationPolicyDns()` | Pinned DNS, open/invite_only, закрытый default; loading/error не открывают регистрацию |
| Создать аккаунт | `signupDns(login, password, invitation?)` | Invitation только signup invite_only; no invitation для open |
| Войти | `loginDns(login, password, expectedDevice?)` | Password proof; при новом ключе сначала ReplacementRequired без binding mutation |
| Подтвердить замену | второй LOGIN с expected-old device key | Explicit confirm + CAS; cancel до confirm не мутирует; тот же ID, новые ключи, без старой истории |
| Перезапуск | account + key-only resume/reconnect | Пароль не сохраняется для resume, нет автоматического signup после revoked |
| Связь | `dnsProfile`, `dnsStatus`, `dnsStop`, `dnsNetworkChanged`; R19 `DmsgService.connectionState()` | DNS runtime; ConnectionUiState по worker outcomes; FGS не подтверждает сеть; dnsStatus — строковая диагностика |
| Ошибка | generated FfiError / FfiErrors mapping | Без raw payload, password/invitation в UI/логах; adapter сохраняет typed code |

Источники: `android/app/src/main/java/org/dmsg/client/DmsgFacade.kt`, `UniFfiFacade.kt`, `FfiErrors.kt`; `crates/core/src/ffi.rs`, `auth.rs`; `crates/protocol/src/auth.rs`. Это имена реально доступных операций, не предложение нового API. Пароль 8–128 байт UTF-8 без control/trim/normalization; login 3–32 ASCII `[a-z0-9_.-]`, uppercase ASCII → lowercase без trim. ENROL/join/token fallback, DirectTCP runtime и old-schema migration в новый UI не входят.

Encrypted bidirectional history, exact persistent per-message status, DialogSummary, local alias/time/unread/read cursor — **реализованный R18 контракт**. Он экспортирован через UniFFI и текущий Android facade; точные поля и descending page order ниже. R19 typed connection adapter реализован отдельно в Android. HTML использует только synthetic values, не вызывает эти API. `ReplacementChanged` — безопасный UX-сценарий конфликта, не выдуманный Rust enum.

## Архивная основа визуальных компонентов

| Возможность | Источник в архиве | Следствие для дизайна |
|---|---|---|
| Android UI на AppCompat/XML | `android/app/build.gradle.kts:1–96`; `MainActivity.kt:37–65`; `res/values/themes.xml` | Редизайн можно перенести на текущие Views. Compose в архитектуре — целевое решение, не текущая реализация. |
| Шесть существующих экранов | `MainActivity`, `ChatActivity`, `ProfileActivity`, `ScannerActivity`, `DiagnosticsActivity`, `StorageActivity` | Не изобретать другой продукт: переписка, контакты, импорт, соединение, локальные данные. |
| Команды ядра | `android/app/src/main/java/org/dmsg/client/DmsgFacade.kt:5–36` | Сохранить фасад как границу, не вызывать Rust/сеть напрямую из UI-компонентов. |
| Личные текстовые сообщения | `crates/core/src/ffi.rs:602–640`; `crates/protocol/src/lib.rs:85` | Composer только для текста; лимит 4096 байт UTF-8. |
| Профиль сервера / QR контакта | Актуальный DmsgFacade `profilePreview` / `qrKind`; старый архивный scanner заменён семантикой R17 | Серверный QR требует offline preview и явного принятия, контактный QR не является auth. |
| requested / accepted / blocked | `crates/core/src/contacts.rs:36–44,167–266` | Не считать QR автоматическим разрешением переписки; блокировка терминальна в этой версии. |
| Смена identity останавливает отправку | `crates/core/src/contacts.rs:268–331`; `crates/core/src/chat.rs:306–318` | Блокировать composer и retry, а не ограничиваться предупреждением. |
| Очередь и повтор | `crates/core/src/store.rs:373–455`; `crates/core/src/chat.rs:297–340` | «В очереди» отдельно от «На сервере»; retry использует сохранённый message ID/ciphertext. |
| Background polling | `DmsgService.kt:26–44,119–167`; `Prefs.kt:51–58` | ~15 секунд / ~5 минут; FGS enabled и связь с сервером — разные состояния. |
| Собственный QR | `ProfileActivity.kt:58–88` | Экран «Мой QR»; никакой телефонной регистрации и профиля соцсети. |
| Локальное защищённое хранение и snapshot | `SecureStore.kt:13–16,32–74,79–129`; `StorageActivity.kt:39–72` | Snapshot только для той же установки с исходным Keystore-ключом. Не cloud backup и не перенос на другой телефон. |

В таблице краткие Kotlin-имена относятся к `android/app/src/main/java/org/dmsg/client/`.

## Текущий data/UI контракт и ограничения

### 1. Реализованная R18 модель диалога и истории

Источник истины: `crates/core/R18_API.md`, `src/history.rs`, `src/ffi.rs`; Android `DmsgFacade.kt`, `UniFfiFacade.kt`, generated Kotlin. Ранее архивный inbox-only UI больше не определяет текущий контракт.

| Core API | Android facade | Результат |
|---|---|---|
| `history_page(contact_id, before_local_id?, limit)` | `historyPage(contactId, beforeLocalId?, limit)` | `HistoryPage(rows, next_before_local_id?)` |
| `message_status(message_id_hex)` | `messageStatus(mid)` | nullable `DeliveryState` |
| `dialogs_page(cursor?, limit)` | `dialogsPage(cursor?, limit)` | `DialogsPage(rows, next_cursor?)` |
| `set_contact_alias(contact_id, alias?)` | `setContactAlias(id, alias?)` | local alias write/clear |
| `mark_read(contact_id, through_local_id)` | `markRead(id, throughLocalId)` | monotonic local read cursor |

`HistoryMessage`: `local_id`, `message_id_hex`, `contact_id`, `direction` (Incoming/Outgoing), `text`, `local_timestamp_ms`, optional `delivery_state` (Queued/Accepted/Delivered). `DialogSummary`: `contact_id`, optional `local_alias`/`preview`/`last_local_timestamp_ms`, `local_unread`, `read_cursor`, `has_keys`, `identity_mismatch`, `state`.

History pages newest-first по local ID descending; reverse каждой страницы для хронологической ленты, prepend reversed older pages. Exclusive next-before anchor должен существовать в этом контакте и быть положительным; zero/negative/future/cross-contact → InvalidInput. `None` next — конец. Чтение истории не отмечает её прочитанной.

Dialogs включают все контакты, в том числе empty/requested/blocked. Order local activity milliseconds descending, contact ID ascending ties; read/alias/status/trust changes не поднимают строку. Empty preview/time — `None`; preview до 160 Unicode scalar values. Keyset paging не frozen snapshot: restart с `None` после send/fetch/contact additions. Limits всех страниц clamp 1..100.

Все пять storage calls локальные синхронные: выполнять вне main thread. Core schema6; server schema5/wire2 без изменений. Android `open_encrypted` использует Keystore-wrapped key; history text и aliases шифруются отдельными authenticated field domains, outgoing history атомарна с ratchet/outbox, incoming — с inbox-dedup до ACK. Retry не создаёт новую историю и не понижает статус. Не переносить владение БД в Kotlin и не дублировать plaintext в SharedPreferences/Debug/logs.

Alias trimmed 1..128 UTF-8 bytes без controls, `None` очищает. Time — this-device queue/decrypt milliseconds, не sender/server time/last-seen. `local_unread` считает incoming за contact read cursor; `mark_read` только по реально просмотренной строке, incoming/outgoing anchor допустим, старый valid anchor idempotent. Read receipt не передаётся. Schema6 не мигрирует/автоматически не стирает несовместимую БД.

### 2. delivered — не прочитано

R18 status exact/persistent/monotonic: Queued до подтверждённого SEND_ACK, Accepted при ST_ACCEPTED, Delivered при ST_DELIVERED. Delivered не означает прочтение/расшифровку пользователем. Transport error сохраняет последнее известное состояние; delivered history переживает удаление outbox.

Incoming history `delivery_state=None`. `message_status` принимает ровно 32 ASCII hex case-insensitive, возвращает `None` для unknown/incoming-only IDs. Отсутствие строки в outbox не означает delivered. R19 `TextUiState.deliveryLabel` отображает nullable status как «Статус пока неизвестен», прежний архивный fallback больше не применяется. Отдельный новый status-event API не заявлен.

### 3. Offline очередь не универсальна

Текущий DNS send может сохранить offline очередь только при существующей сессии и подтверждённом ядром durable queue результате. Ошибка Noise/доверия не превращается в успешную очередь. Первый текст новому контакту без сессии может не отправиться offline. В таком случае черновик остаётся, подпись: «Для первого сообщения нужно соединение».

### 4. Контактные состояния требуют аккуратной подачи

Добавление по ID (`contacts.rs:173–185`) создаёт локальную requested-запись, а не реализованный сетевой запрос другому пользователю. Не писать «Запрос отправлен Анне». Предлагаемая надпись: «Контакт добавлен. Чтобы начать переписку, отсканируйте его QR».

После нового QR контакт остаётся requested. `accept` разрешён при наличии ключей. `block` терминален. В текущем `add_from_qr` сценарий «ID без ключей → QR» может пройти ветку IdentityChanged; UI следует возвращённому состоянию, а не предполагает happy path.

Текущий `UniFfiFacade.get` сохраняет `hasKeys`/`identityMismatch`, `dialogsPage` возвращает их в summary. `has_keys` требует все четыре pinned routing/identity values; любой presented `seen_*` устанавливает mismatch. R19 ContactCta/trustLabel следуют реальным полям, не дефолтному false. Metadata не меняет requested/accepted/blocked gates.

Публичных старого/нового отпечатков контакта через facade нет. В дизайн текущего контракта входит повторная проверка QR и явное подтверждение; красивый экран сравнения fingerprint потребует отдельного API, не вывода приватных ключей.

### 5. DNS runtime и FGS — разные состояния

Актуальный клиент использует DNS runtime и публичный профиль с закреплённым fullDER/Noise public. Архивное утверждение про DirectTcp больше не определяет текущую UI-семантику. Прототип не выполняет реальные DNS-запросы.

R19 `TextUiState.ConnectionUiState`/`ConnectionFacts` и `DmsgService.connectionState()` уже реализованы: serviceEnabled/pollInFlight/lastSuccessAt/lastFailure/revision отражают реальные worker outcomes. `dnsStatus()` остаётся строковым диагностическим API. `Worker.running` не доказывает соединение. В прототипе подпись «DNS · демо-состояние», без выдуманного last-success timestamp. Device runtime pending.

### 6. Public profile → offline preview → auth

Paste и server scan открывают один offline preview. До явного принятия не вызываются configure/policy/login/signup. Код сервера не является приглашением; ручной transport addr и криптополя обычному пользователю не нужны. После принятия configureDns и короткий policy request отделены от создания аккаунта.

При запрете камеры primary fallback — «Вставить код сервера». HTML использует только точный сентинел `dmsg://server/DEMO-PUBLIC-PROFILE`, заведомо невалидный реальный wire-профиль. Никакие реальные QR, сертификаты и secrets не читаются.

Offline preview не доказывает pinning сетевого сеанса. PinMismatch в policy/auth останавливает вход; обхода проверки нет. LOGIN replacement — два запроса с explicit confirm между ними. Пароль, invitation и challenge очищаются при отмене/успехе; устаревший callback не заменяет устройство и не открывает закрытый экран.

### 7. Операции и ошибки

Типизированные Rust FfiError уже есть. UI adapter должен сохранить код при отображении, а не парсить `e.message`. В R17 макете отдельно показаны InvalidCredentials, LoginTaken, InviteRequired/Expired/Revoked/Used, AuthRateLimited, InvalidInput, BadQr, PinMismatch, Revoked/NotEnrolled, Busy, Transport, Store, Crypto, Protocol. Серверный retry-after не выдумывается, raw exception payload не отражается.

R19 MainActivity использует `dialogsPage`, ChatActivity — `historyPage`/HistoryWindow и `markRead`; нативный UiGuard защищает pending/lifecycle callbacks. Сохранить keyset paging, reverse/prepend и read anchor только по реально просмотренной строке. Compile/JVM evidence не заменяет проверку scroll/lifecycle на устройстве.

В `DiagnosticsActivity.kt:56–57` размер первой страницы outbox ограничен 100 и не является надёжным общим числом очереди. При наличии next cursor писать «100+» либо добавить агрегатный count. Не считать это unread counter.

### 8. Хранилище и уведомления

Cache wipe (`StorageActivity.kt:63–72`) не означает удаления истории или account. Snapshot не переносим между установками (`SecureStore.kt:15–16`). Переустановка/очистка данных с потерей Keystore не должны сопровождаться кнопкой «Восстановить из облака» или обещанием восстановления из одной sealed-копии.

FGS уведомления в коде не содержат текстов переписки (`DmsgService.kt`, buildNotif). Сохранить этот подход. Публичные метаданные/ID в текущей БД не следует рекламировать как полное шифрование всех полей БД.

## Границы прототипа

HTML не использует Rust, камеру, DNS, сеть или storage API; это локальная интерактивная демонстрация. Переходы состояний и 600 ms auth-ответ — имитация. QR содержит DEMO-текст. Профиль, домен `.invalid`, fingerprint, старый device-key, приглашение, имена, ID и сообщения искусственные; не извлечены из конфигураций, сертификатов или истории пользователя. Операционные trace-флаги не содержат credential/profile payload.

Нет изменений исходного backend, транспорта, generated bindings, app signing и Keystore. Никакие инструкции/скрипты из архива не запускались.

## Дополнение v2: граница будущих звонков

Повторно прочитаны `android/app/src/main/java/org/dmsg/client/DmsgFacade.kt` и `android/app/src/main/AndroidManifest.xml` из пользовательского `53(1).zip`. В facade отсутствуют команды и события звонков. В manifest нет RECORD_AUDIO и MANAGE_OWN_CALLS; текущий DmsgService имеет тип dataSync. CAMERA уже используется существующим QR-сценарием и не доказывает наличие видеозвонков. Manifest отмечает отсутствие GMS/FCM.

По запросу пользователя в целевой дизайн добавлены будущие звонки. Это не утверждение о готовом медиатранспорте. В этой design-задаче Kotlin/Rust и generated bindings не изменялись; подтверждённые координатором R18/R19 изменения отражены выше. Новые call-модели, сигналинг, медиа, call-журнал и системная интеграция остаются предложениями в `CALLS_HANDOFF.md`.

HTML содержит только имитацию звонков, разрешений, маршрутов, video placeholders и уведомлений; он не запрашивает эти возможности браузера. Наличие работающих кнопок в демо не означает наличие аудиосвязи, E2EE звонка или фоновых входящих в Android.

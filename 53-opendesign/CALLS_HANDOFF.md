# Android handoff: будущие звонки

Версия 2.0 · 29.09.2026. Это предлагаемая граница интерфейса, не реализация звонков и не описание существующего UniFFI API. Продуктовые правила: `CALLS_SPEC.md`.

## 1. Что подтверждено исходниками

В повторно прочитанном `android/app/src/main/java/org/dmsg/client/DmsgFacade.kt` из `53(1).zip` доступны операции сообщений, контактов, QR, подключения и хранения. Команд звонков и потока состояния медиасессии нет.

В `android/app/src/main/AndroidManifest.xml` есть INTERNET, FOREGROUND_SERVICE, FOREGROUND_SERVICE_DATA_SYNC, POST_NOTIFICATIONS и CAMERA для существующего QR-сценария. `DmsgService` объявлен как `dataSync`; RECORD_AUDIO, MANAGE_OWN_CALLS и отдельной регистрации звонков в прочитанном manifest нет. Комментарий manifest указывает на отсутствие GMS/FCM и локальные уведомления. Не добавлять зависимость от FCM незаметно ради реализации входящего экрана.

Исходный аудит сообщений сохранён в `CODE_REVIEW.md`. Новые экраны являются самостоятельным проектным расширением. В этой задаче исходники Android/Rust, manifest, generated bindings и протокол не изменялись.

## 2. Разделить четыре слоя

**Call UI / ViewModel.** Рисует immutable состояние, передаёт намерения, обрабатывает экранную навигацию. Ни текстовый outbox, ни таймер Activity не становятся владельцем звонка.

**CallCoordinator.** Единственный владелец жизненного цикла. Сериализует команды и события, проверяет версии, гарантирует один активный callId, синхронизирует историю и поверхностные состояния. Продолжает работать при пересоздании экрана.

**Signaling / Media adapter.** Отдельные обязанности: доставка приглашения/ответа/отмены и передача/получение медиа. Выбор движка и транспорта открыт. Наличие Noise в переписке не означает автоматически защищённую аудиосессию.

**Android telecom adapter.** Связывает сессию с платформенными вызовами, endpoint-ами, системными действиями и уведомлениями. Платформа управляет своим UI; приложение не копирует его в XML.

Для интеграции с платформой предусмотрен адаптер вокруг Core-Telecom. Он не заменяет медиадвижок. Документация описывает создание вызова, callbacks действий и наблюдение за endpoint/mute. Выбор аудиоустройства при этой интеграции следует передавать через Telecom, не конкурирующим независимым AudioManager-контроллером. [A1]

## 3. Предлагаемое состояние UI

Ниже структура данных, а не готовый Kotlin-файл для подключения к проекту:

```kotlin
// Проектный контракт. В существующем DmsgFacade этого пока нет.
data class CallUiState(
    val callId: String,
    val accountId: String,
    val contactId: String,
    val identityRevision: String,
    val revision: Long,
    val direction: CallDirection,
    val phase: CallPhase,
    val negotiatedMedia: MediaKind,
    val connectedAtElapsedMs: Long?,
    val recoveryDeadlineElapsedMs: Long?,
    val endedDurationMs: Long?,
    val microphone: MicrophoneUiState,
    val audio: AudioEndpointUiState,
    val video: VideoUiState,
    val security: CallSecurityUiState,
    val capabilities: CallCapabilities,
    val pendingCommands: Set<String>,
    val endReason: CallEndReason?
)
```

`CallPhase`: Dialing, Ringing, Incoming, Connecting, Active, Reconnecting, SystemHeld, Ended. Отсутствие сессии представляется отдельно, не фальшивым Active с пустым contactId.

`MicrophoneUiState` разделяет пользовательское намерение mute, фактическую передачу, pending, runtime permission и системную недоступность датчика. `AudioEndpointUiState` содержит фактический endpointId, список доступных endpoint-ов и запрошенный endpointId до подтверждения. `VideoUiState` различает preview, локальную публикацию, удалённый трек, переговоры о видео, разрешение и доступные камеры.

`CallSecurityUiState`: NotEstablished, Negotiating, VerifiedSession, Failed. Даже VerifiedSession не равен обещанию конкретной схемы E2EE без отдельного согласованного security contract. UI не вычисляет fingerprints из приватного материала и не выводит секреты.

`CallCapabilities`: audioAvailable, videoAvailable, peerAudioSupport, peerVideoSupport, canMute, canChangeEndpoint, canResumeSystemHeld, canSwitchCamera. Не рисовать пользовательские действия только по версии собственного приложения, когда peer support неизвестна.

Длительность активного разговора вычисляется по монотонному времени. Абсолютное время события хранится отдельно для журнала и локализуется при отображении; монотонные timestamps не сохраняются как переносимые календарные значения между загрузками устройства. После терминального события длительность фиксируется и больше не растёт.

## 4. Команды и события

Предлагаемые команды: `start(contactId, media, operationId)`, `answer(callId, answerMode, operationId)`, `decline(callId)`, `hangUp(callId)`, `setMuted(callId, desired)`, `selectEndpoint(callId, endpointId)`, `requestVideo(callId)`, `respondVideo(callId, accept)`, `setCameraEnabled(callId, desired)`, `switchCamera(callId, cameraId)`, `resume(callId)`, `loadHistory(cursor, limit)` и `removeHistory(callIds)`.

Результат команды означает принятие/отклонение намерения, не автоматически достигнутое сетевое состояние. Каждая операция имеет статическую безопасную ошибку, а не текст исключения с ключами или plaintext. Пока команда pending, не порождать её новые копии.

События: InviteValidated, InviteDelivered, RemoteAnswered, SessionReady, MicrophoneChanged, EndpointsChanged, EndpointChangeFailed, MediaInterrupted, MediaRestored, SystemHeld, ResumeAllowed, VideoRequested, VideoAccepted, VideoDeclined, CameraChanged, IdentityChanged, RemoteEnded, TimedOut, Ended. Все относятся к конкретным accountId/callId/revision.

`SessionReady` должен означать согласованную и прошедшую необходимые проверки сессию, не просто регистрацию в Android. Core-Telecom отдельно предупреждает, что успешное добавление вызова ещё не означает Active. [A1]

Подписки от экрана отменяются с его lifecycle, но не завершают сам звонок. Все важные действия гарнитуры, системного уведомления и приложения проходят через один coordinator. Не создавать второй путь, который может обойти проверку ключа.

## 5. Переходы и устойчивость к гонкам

Машиночитаемая схема находится в `calls-state-machine.json`. Основные переходы:

```text
нет звонка -> проверки/разрешения -> Dialing -> Ringing -> Connecting -> Active
проверенное приглашение -> Incoming -> явный ответ -> Connecting -> Active
Active -> Reconnecting -> Active
Active/Reconnecting -> SystemHeld -> разрешённое возобновление
любая живая фаза -> Ended
Ended -> новый вызов только по новому явному действию с новым callId
```

Отмена опережает поздний ответ. Ended поглощает оставшиеся callbacks этой сессии. Нельзя вернуться в Active по позднему SessionReady. Повтор события Reconnecting не продлевает исходный deadline бесконечно.

Ответ из notification проверяет callId, актуальность и направление. Устаревший PendingIntent не начинает исходящий вызов. Получение разрешения после закрытия входящего или появления другого звонка не выполняет ранее сохранённое намерение без повторной проверки.

Дубликаты incoming отбрасываются по ограниченному replay-cache и сроку действия. Не хранить бесконечный список всех сетевых ID. Не раскрывать удалённому человеку локальный факт блокировки/отключённых входящих, если протокол специально не согласовал такое раскрытие.

История записывается один раз с уникальностью accountId+callId. Журнал и call-события чата читают одну запись. При сбое записи разговор всё равно завершается локально; сообщение об ошибке хранения не должно удерживать микрофон.

## 6. Android: уведомления и жизненный цикл

Для системной поверхности использовать CallStyle, где он доступен, и совместимый системный fallback для выбранного диапазона ОС. Этот стиль предназначен для входящих и текущих звонков; обязательные действия и их подписи формирует система. HTML-экран `call-system` показывает только содержание и намерения, не обещает пиксельное соответствие. [A2]

В Core-Telecom после добавления вызова требуется своевременное foreground-уведомление; документация указывает окно 5 секунд. Callbacks внешних устройств также имеют ограниченный срок обработки. Не выполнять в этих callbacks долгие блокирующие сетевые операции. [A1]

`POST_NOTIFICATIONS`, channel importance и полноэкранный доступ нужно моделировать раздельно. Для корректно настроенных self-managed calls документация описывает исключение для CallStyle из обычного POST_NOTIFICATIONS: MANAGE_OWN_CALLS, ConnectionService и регистрация PhoneAccount. Один permission в manifest сам по себе не создаёт это исключение. Не путать его с пользовательским отключением канала. [A3]

Для соответствующих версий проверять `canUseFullScreenIntent()`, а не считать fullscreen гарантированным. Разрешение управляется отдельно; при необходимости переходить в системные настройки и иметь сценарий через уведомление. Не пытаться обходить ограничения оверлеем поверх других приложений. [A4]

Нужные типы foreground service и их разрешения зависят от выбранной интеграции и target SDK. `RECORD_AUDIO` требуется для захвата микрофона; камера запрашивается отдельно. Существующий `dataSync` сервис нельзя считать готовым разрешением на фоновый захват. Запуск camera/microphone FGS ограничен while-in-use условиями и правилами фонового запуска; учитывать предусмотренные платформой исключения, а не обещать старт из любого состояния. [A5, A6]

Приложение не обязано запрашивать телефонную книгу, системный журнал, READ_PHONE_STATE, CALL_PHONE или становиться default dialer только ради нарисованных кнопок. Каждое новое разрешение обосновать конкретной реализацией; не копировать manifest большого телефонного приложения целиком.

## 7. Входящий без нового скрытого сервиса

В проекте нужно отдельно спроектировать доставку приглашения вовремя, отмену, TTL, пробуждение/доступность процесса и взаимодействие с текущей фоновой службой. Экран звонка этого не реализует. Режим опроса сообщений «15 секунд / 5 минут» не является контрактом своевременного входящего звонка.

Не добавлять новый бесконечный сервис лишь потому, что теперь есть звонки в макете. Не отправлять сигналинг через публичный API `send(text)` как обычный видимый текст. При изменении транспорта согласовать влияние на расход батареи, приватность маршрута и работу в ограниченной сети. Текстовый чат должен сохранять работоспособность, когда медиа недоступно.

## 8. Нативные компоненты

`CallActivity`/экран звонка: наблюдает coordinator, обрабатывает insets и системный Back. `CallMiniBar`: общий элемент shell, а не отдельная копия session state в каждом чате. `CallHistoryFragment`/экран журнала и `CallDetails`: получают пагинированные записи. `AudioEndpointSheet`: только доступные endpoint-ы. `CallPermissionCoordinator`: контекстный запрос и возврат. `CallNotificationController`: актуальные системные действия с callId.

На существующем Kotlin/AppCompat/XML это реализуется без обязательной миграции на Compose. Локальный mock CallFacade для дизайна должен быть отделён от production реализации и не попадать в релиз как активный движок. HTML не встраивать в WebView внутри APK.

## 9. Приёмка на устройстве, не покрытая HTML

Проверить реальный ответ и завершение из гарнитуры, шторки, блокировки и уведомления; одновременное нажатие на разных поверхностях; повторное/опоздавшее приглашение; активный мобильный звонок; потерю Bluetooth; устройство без разговорного динамика; смену Wi-Fi/мобильной сети; только текстовый транспорт; режим экономии батареи и ограниченный фон.

Проверить denial/permanent denial/отзыв RECORD_AUDIO, системный запрет микрофона, отключённый канал, отказ fullscreen, DND, force-stop, гибель процесса, поворот, 200% шрифт, TalkBack, приватность блокировки. Проверять версии ОС, выбранные для выпуска, а не только эмулятор одной версии.

Для видео дополнительно: нет камеры; камера занята; отсутствует задняя камера; фон и блокировка; отказ от upgrade; simultaneous upgrade; потеря удалённого трека; возврат только в аудио без разрыва звонка. Камера ни при одном из этих событий не должна самовольно включаться.

## 10. Источники Android

Первичные документы сверены 29.09.2026. Это ссылки на требования платформы, не подтверждение готовности текущего проекта.

[A1] Core-Telecom: lifecycle, callbacks, endpoint-ы, foreground support.
https://developer.android.com/develop/connectivity/telecom/voip-app/telecom

[A2] CallStyle: входящие/текущие уведомления и системные действия.
https://developer.android.com/develop/ui/compose/notifications/call-style

[A3] Notification runtime permission и исключение для self-managed calls.
https://developer.android.com/develop/ui/compose/notifications/notification-permission

[A4] Android 14+: отдельный доступ для full-screen intent.
https://developer.android.com/about/versions/14/behavior-changes-14#secure-fsi

[A5] Типы foreground services и runtime prerequisites.
https://developer.android.com/develop/background-work/services/fgs/service-types

[A6] Ограничения фонового старта и while-in-use разрешений.
https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start

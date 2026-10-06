# План работ: DNS-мессенджер

Дата: 28.09.2026; account-auth 30.09.2026, native Light/Square text frontend/fresh dev rollout 01.10.2026. Это план и критерии приёмки, не перечень уже выполненного. Текущие результаты и утверждённые итерации — в `docs/goals/2026-09-29-client-track.md`.
Архитектурная основа и источники находятся в ARCHITECTURE.md.

## Порядок выпуска

Первая контрольная точка: два Android-аппарата обмениваются E2E текстом только через DNS; очередь переживает разрыв и перезапуск.

Вторая: голосовые и ограниченные файлы с возобновлением.

Третья: live-звонки, только после проверки сквозного audio path и фоновой доступности.

Эксперимент со звонками выполняется рано, параллельно с инфраструктурой. Продуктовую реализацию звонков не ставим раньше достоверной доставки сообщений.

## M0. Зафиксировать основу и не затронуть текущий туннель

**Задачи.** Создать отдельный проект, закрепить выбранную ревизию `feat/rust-parity-ab` средствами Git, добавить transport dependency, Git-историю небольших адаптаций, базовую CI-сборку. Разделить изменения на embedding/platform adapter и messenger logic. Не переносить в приложение временные benchmark shims и production secrets.

**Проверки.** Выполнить штатные protocol/path/pin/runtime tests, release и sanitizer сборки. Воспроизвести контрольные измерения до собственных изменений на одном и том же domain/resolver/target. Результаты в Git: сценарий, агрегированные метрики, версия и вывод; без ручных checksum manifests.

**Готово, когда.** Клиент и сервер собираются из чистого checkout, соответствуют одной ревизии, существующее развёртывание не изменено. Известно, UDP или TCP нужен до резолвера. Подтверждён рекурсивный путь к новому поддомену, а не только direct authoritative доступ.

**Риск.** Подмена измеренного исходного состояния «последним upstream», смешивание несовместимых клиента/сервера или снятие benchmark с другим DNS-путём.

## M1. Вертикальная интеграция сервера и Android

**Сервер.** Multistage Dockerfile для C-транспорта и Rust msgd, Compose из двух сервисов, фиксированный loopback target через общий network namespace, только UDP DNS наружу. Persistent data/secrets, non-root, healthchecks, restart policy, совместное пересоздание сервисов. Не зависеть от локального образа из benchmark build environment.

**Android.** Kotlin-оболочка, UniFFI, Rust core, C transport library. Реализовать start/stop/status без захвата глобальных process signals. Rust supervisor управляет reconnect/backoff и сетевыми переходами. Один native transport instance; очистка handles/buffers проверяется. При необходимости TCP-DNS добавляется bounded Rust adapter по модели проверенного пути, не Python runtime.

**Проверки.** arm64 release на реальном аппарате; загрузка `.so`, 16 KB compatibility; start/stop многократно; экран on/off; смена Wi-Fi/mobile; перезапуск msgd и Slipstream; запрет внешнего HTTPS/UDP кроме используемого DNS-пути. Проверить соседство с существующим системным туннелем, не вводить новый VpnService.

**Готово, когда.** Два native клиента через рекурсивный DNS обмениваются ограниченными тестовыми данными с msgd. Повторные connect/disconnect не требуют перезапуска телефона и не дают роста RSS/FD. Это диагностический transport test, ещё не защищённый пользовательский чат.

## M1-V. Ранний эксперимент с голосом

**Задачи.** Сделать генератор timestamped frame-sized traffic и минимальный двусторонний Opus loop. Сравнить 1/2/4 media streams, 20/40/60 ms packet duration, DTX, FEC on/off и ограниченный jitter buffer. Не включать в MVP все комбинации: после измерений оставить один профиль.

**Сценарии.** Два клиента на одном сервере; у обоих uplink ограничен 100 kbit/s, downlink 2 Mbit/s. Контролируемые RTT/jitter/burst-loss и реальные DNS-условия; разговор одновременно с bulk, затем с остановленным bulk. RTT/2 не считать односторонней задержкой без подтверждённой симметрии. Для mouth-to-ear использовать аудио-loopback/запись тестовых сигналов либо согласованные часы в тестовой установке.

**Метрики.** On-time frame delivery, p50/p95/p99 audio latency, длительность stalls, накопление очередей, DNS queries/s, transmitted bytes, CPU/PSS, энергия на конкретном аппарате. Idle-empty и quiet-open измерять отдельно, равно как reconnect overhead и время восстановления.

**Решение.** Зафиксировать: live-call приемлем; только экспериментальный; либо временно voice notes/PTT. Не публиковать проценты вероятности без наблюдений и не считать несколько streams эквивалентом unreliable datagrams.

## M2. Код подключения, логин/пароль и учётная запись

**Задачи.** Один готовый публичный код/QR сервера: bounded versioned parser, offline preview, полный certificate pin и Noise server key; без bearer-token, пароля и приватных ключей в профиле. После проверки сервера — «Войти» / «Создать аккаунт» с логином/паролем. На сервере уникальный логин, Argon2id password hash, ограничение попыток и два сохраняемых режима регистрации `open` / `invite_only` (default), управление через msgctl. Приглашение — отдельное дополнительное условие создания аккаунта в `invite_only`, с одноразовостью/TTL/revocation. Локальные device/E2E keys создаются приложением; регистрация/привязка идемпотентны и атомарны. Устройство запоминается, последующие reconnect не требуют пароля.

**Безопасность.** Учётные данные отправляются только после pinned carrier + Noise handshake через DNS. Неизвестный key получает лишь ограниченный account-auth API, не mailbox/blob. Режим регистрации проверяется на сервере; существующие пользователи входят в обоих режимах, revoked device и legacy API не обходят ограничения. Пароль/приглашение/приватные ключи не попадают в публичный код, URI, argv, crash logs и аналитику. Сохранить revocation с закрытием открытых streams, bounded frames/pending sessions и доверенный offline bootstrap без скачивания pin с непроверенного URL.

**Готово, когда.** Телефон импортирует один код/QR без внешнего HTTPS, регистрируется/входит через DNS и далее открывает диалоги без повторного ввода пароля. Проверены оба режима, неверный пароль, занятый логин, expired/revoked/used invitation, регистрационные гонки и потеря ответа. Единый новый wire/schema, без ENROL/token-replay/credential attach и compatibility fallback; старые версии отклоняются без мутации. Вход с нового аппарата меняет активное устройство только после подтверждения/CAS; прежний доступ отозван, E2E keys свежие, peer STOP до confirm без потери сообщения, старая история не обещана. Неверный pin/server key и отозванное устройство отклоняются. Fresh dev rollout теперь выполнен R21; будущие несовместимые wipe требуют отдельного разрешения.

## M3. Надёжный E2E текст и контакты

**Задачи.** Версионированный wire protocol и совместимые fixtures; mailbox events/cursor; persisted outbox; дедупликация; серверный durable ACK и клиентский delivery ACK. E2E Olm/vodozemac, подписанные prekeys, атомарное потребление one-time keys и пополнение. Ratchet state и outbox ciphertext сохраняются в одной локальной транзакции.

**Контакты.** 12-символьный случайный публичный ID, UNIQUE-проверка, добавление по запросу, разрешение сообщений/файлов/звонков по состоянию контакта, блокировка, contact QR с identity key, предупреждение при смене ключа. Нет выгрузки адресной книги и массового prefix-search пользователей.

**Проверки.** Разрыв перед/после server commit и ACK; kill/restart клиента и сервера; повтор одного message_id; переупорядочение; пустой запас prekeys; повреждённый ciphertext; подмена contact identity; диск заполнен; mailbox TTL и quota; неизвестная версия wire protocol.

**Готово, когда.** На двух аппаратах читаются E2E сообщения, сервер хранит только ciphertext. Повторная доставка не создаёт дубликатов; локально принятое сообщение не исчезает после обычного рестарта. Сбой питания проверяется согласно режиму SQLite/диска, а не подменяется kill процесса. Отдельные статусы не называют server ACK пользовательским прочтением.

## M4. Android UX и фоновая доступность

**Уточнение onboarding 2026-10-06 реализовано:** deployment APK содержит доверенный публичный server profile; generic build сохраняет ручной trust preview. Отдельный QR/приватный файл приглашения автоматически выбирает signup с логином/паролем, но не создаёт аккаунт до явного submit и не устанавливает доверие к серверу. `53ctl qr-invite` выдаёт terminal QR + файл0600. Проверки и границы evidence — `docs/goals/2026-10-06-invite-onboarding.md` и `android/AUTH_GATES.md`; остальная M4 background-матрица не расширяется.

**Задачи.** Первый запуск: вставить код/сканировать QR → проверить сервер → войти/создать аккаунт → диалоги. Приглашение показывать только когда его требует сервер; технические поля вынести в дополнительные настройки. Список диалогов, экран чата, composer, понятные ошибки («неверный логин или пароль», «логин занят», «нужно приглашение», «нет связи»), contact QR, настройки хранения и связи. Явные режимы «На связи»/«Экономия»; постоянное уведомление для активного соединения; корректные service types, разрешения и экран диагностики фоновых ограничений. Интеграция уведомлений без обязательного FCM.

**Хранение.** Keystore-wrapped local key, encrypted secrets/ratchets/content в app-private storage, отключение небезопасного auto-backup для identity state. Локальная очистка кэша и истории; политика потери телефона и перевыпуска доступа. Полный cloud backup/перенос истории отложить.

**Текстовый frontend R17–R21 реализован.** Light/Square на Kotlin/AppCompat/XML; Rust schema6 хранит защищённую историю обеих сторон, exact status, summaries/local aliases/time/unread/read cursor. Pages per-contact, TX с ratchet/outbox/inbox; time локальное, delivered не read receipt. Actual recursive phone↔native/UI/200% font/IME verified, полная M4 background/compatibility матрица ниже этим не закрыта.

**Проверки.** Forced Doze и App Standby; экран выключен продолжительное время; normal process death и force-stop отдельно; отсутствие Google Play Services; отказ разрешений; unreadable/oversized QR; очистка данных/переустановка; OEM battery restrictions. Измерить queries/s и батарею в обоих режимах. Проверить штатный выбор клавиатуры, clipboard, accessibility и большие списки сообщений.

**Готово, когда.** Интерфейс не блокируется сетью и шифрованием. Пользователь видит реальное состояние фоновой доступности. Недоступный клиент не обозначается как гарантированно принимающий звонки. Отдельно получены APK size/PSS/startup/scrolling результаты для выбранного тестового устройства.

## M5. Голосовые сообщения и вложения

**Задачи.** Запись Opus mono, ограничение по длительности и размеру, локальное прослушивание. Client-side photo resize/metadata removal, file picker, размеры до отправки. Blob manifest внутри E2E, шифрование chunks, quota reservation, upload/download resume, отмена и уборка временных файлов. Перезапуск не должен требовать повторной передачи уже подтверждённых chunks.

**Лимиты.** Текст 4 KiB, voice 60 s/128 KiB, photo 1024 px/256 KiB, общий attachment 512 KiB. Размер файлового лимита считается по итоговому wire ciphertext; сервер не доверяет заявленному размеру. Одна bulk-передача, chunk до 8 KiB. Бюджеты и TTL описаны в ARCHITECTURE.md.

**Безопасность.** Права на blob привязаны к разрешённым участникам, угадывание blob ID не даёт чтения. Проверить reorder/truncation/chunk replay, недостаток места, ложный MIME, изображение с огромной распакованной памятью, неверную длительность и оборванный manifest. Прежний ciphertext можно переслать повторно, но изменённые данные нельзя шифровать с прежним key/nonce.

**Готово, когда.** Голосовое и файл проходят между телефонами только через DNS, переживают разрыв и restart. Контрольные сообщения идут, несмотря на bulk. Сервер и принимающее приложение независимо соблюдают доступные им ограничения, не притворяясь, что сервер прочитал E2E media metadata.

## M6. Продуктовые звонки, если M1-V подтверждён

**Задачи.** Состояния invite/ringing/accepted/active/reconnecting/ended/busy, call TTL, согласование возможностей. Раннее нажатие «завершить» идемпотентно. Короткий ring TTL исключает «входящий через час» после восстановления связи. Одновременно один вызов на устройство.

**Media.** Opus выбранного профиля, RTP/SRTP, fresh directional keys/salts через E2E, call/epoch binding, replay protection. Bounded send queues, jitter buffer, deadline drops и PLC. Control остаётся отдельным stream; bulk останавливается в обе стороны, включая server egress. При потере транспорта повторно не воспроизводится старый голос, новая epoch не сбрасывает счётчики старого key.

**Android.** Telecom/call notification, microphone permission lifecycle, audio focus, AEC, speaker/headset/Bluetooth routing, proximity, экран блокировки, корректное завершение при отмене разрешения. PCM не гоняется по high-level FFI на каждый sample и не обрабатывается сетевым task.

**Готово, когда.** Двусторонний разговор на целевых аппаратах проходит gate по latency/deadline/stalls и понятности речи. Проверены mute, hangup, busy, missed call, потеря сети и восстановление; голосовой трафик не накапливается. Звонок не считается готовым по факту единичного echo или прохождения Opus frames.

## M7. Пилот, безопасность и развёртывание

**Задачи.** Испытание до 16 concurrent devices; ограничение числа параллельных вызовов по полученной ёмкости. Интеграционные тесты регистрации, сообщений и медиа; parser fuzzing; ревью key/nonce/prekey/ACK/ACL логики; C sanitizers. Проверить отсутствие утечек секретов в app/server/CI logs.

**Эксплуатация.** Согласованные backup/restore SQLite+blobs+server keys, проверка обновления/rollback с миграциями, восстановление чистого сервера, заполнение диска и graceful restart. Подписанный arm64 APK, стабильный release signing key, lockfiles, Git releases и краткий runbook. Команды создания invite и admin operations не являются публичным API.

**Готово, когда.** Новый инстанс поднимается по runbook, получает делегированный DNS-поддомен и обслуживает два телефона без внешних сервисов. Restore проверен, потеря единственного сервера честно описана как отсутствие HA. В release notes указаны проверенные аппараты, путь резолва, режим фоновой связи и фактическая готовность звонков.

## Структура репозитория

```text
android/                 # Kotlin UI и платформенная интеграция
crates/
  protocol/              # версии, bounded framing, wire DTO
  core/                  # identity, E2E, outbox, transfer, media control
  server/                # msgd и msgctl
  mobile-ffi/            # UniFFI facade
  slipstream-sys/        # узкая C FFI boundary
vendor/slipstream/       # исходная ревизия, закреплённая Git
deploy/                  # multistage Dockerfiles, Compose, конфиги
tests/                   # integration, fixtures, network profiles
docs/                    # архитектура, протокол, решения, результаты
```

Криптография/медиа вначале могут быть модулями core. Дополнительные crates создаются по реальной границе сборки/зависимостей, не для симметрии дерева.

## Минимальная матрица приёмки

Функциональность: подключение по публичному коду/QR через recursive DNS; неверный pin; login/register в обоих режимах; неверный пароль/занятый логин; one-time invitation race; old schema/wire fail-closed без migration; подтверждённая замена единственного устройства; reconnect без пароля; encrypted history/status/alias/local unread; dedup; prekey depletion; revoked device; oversized frames; interrupted blob; missed-call expiry.

Надёжность: server/client restart; транспортный обрыв; обе стороны с низким upload; RAM/disk quota; неконсистентный backup не принимается; restore и schema update; цикл смены сети.

Реальное время: call latency распределение, deadline loss, stalls, очередь под нагрузкой, AEC/гарнитура, передача текста во время звонка, bulk pause в обе стороны.

Android: release ABI; 16 KB native loading; Doze; background permissions; force-stop; GMS-free аппарат; память и батарея с открытым тихим stream и без него.

Безопасность: server trust bootstrap; peer identity verification; нет ключей в URI/logs; транзакционное сохранение ratchet/outbox; nonce/replay/epoch handling; blob ACL; contact restrictions; SDK не обращаются к внешнему интернету.

## Что оставляем после v1

HA и несколько authoritative серверов; транспортная client authentication до прикладного handshake; настоящий unreliable media transport, если stream-based вариант не подходит; multi-device; перенос истории; группы; публичный каталог/поиск по логинам (логин для входа входит в текущий M2); iOS/desktop; большие вложения и видео.

Очередь будущих работ не является разрешением заранее внедрять соответствующую инфраструктуру.

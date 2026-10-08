# dmsg wire v2: единая account-auth логика

Реализованный путь: **доверенный профиль сервера → Войти / Создать аккаунт → диалоги**. Регистрация `invite_only` (default) или `open`; приглашение — только одноразовое разрешение signup. Wire2/auth сохраняются; fresh server7 добавляет клиентский выпуск приглашений, local core10/E2Ev2 не меняются. Old-schema/plain→sealed conversion и legacy E2E decode отсутствуют. Working server5/main8 не обновлены; несовместимый cutover отдельно по явному разрешению. Ошибка не вызывает wipe/новую identity. ENROL/token replay/credential attach/admin invite-rebind отсутствуют.

## Публичный профиль

```text
dmsg://server/<base64url_nopad(binary)>
[version:u8=1][domain_len:u8][domain:domain_len]
[cert_len:u16 BE][full_cert_der:cert_len][noise_pubkey:32]
```

Raw cap 4 KiB, LDH domain 1–253, точное потребление payload; длина base64 ограничена до allocation/decode, padding/noncanonical tail/trailing secret отвергаются. Полный сертификат обязателен для DER pinning. Код **не содержит** invitation/password/private key и ничего не авторизует; импорт/offline preview не создают аккаунт. Contact QR остаётся отдельным `dmsg://contact/` типом.

## Framing и auth

Внутри завершённого pinned carrier + Noise IK: `[version:u8=2][opcode:u8][length:u16 BE][payload]`, полный кадр ≤16 KiB. Каждому stream свой Noise state; device key берётся из authenticated initiator static, никогда из auth payload. Первое сообщение — `AUTH_DOMAIN(3)` с точным доменом → `WELCOME(2)`. До account-auth нет mailbox/blob доступа.

| Request | Payload | Response |
|---|---|---|
| POLICY `7` | empty | POLICY_RESP `8`: mode u8 (`0` invite_only, `1` open) |
| SIGNUP `9` | credentials + optional invitation | AUTHENTICATED `12` или ERROR `6` |
| LOGIN `10` | credentials + optional expected-old-device | AUTHENTICATED, REPLACE_REQUIRED `14` или ERROR |
| RESUME `11` | empty | AUTHENTICATED или ERROR; только active Noise key |

Credentials: `[login_len:u8][login][password_len:u16 BE][password][optional_flag:u8]`, при flag=1 ещё 32 байта invitation/expected-old-device, при 0 никаких trailing bytes. Login — lowercase ASCII `[a-z0-9_.-]`, 3–32; builders переводят ASCII uppercase в lowercase, wire parser требует canonical value. Password — точный UTF-8 8–128 bytes без control chars, пробелы не trim/normalize. Secret DTO не имеют `Debug`.

AUTHENTICATED — user_id16 + contact_id12 Crockford; REPLACE_REQUIRED — текущий device key32. Стабильный pending private key сохраняется core до auth request, accepted account — атомарно/неизменно; пароль не сохраняется. При потере signup/confirm ответа повтор тем же ключом и credentials возвращает прежний аккаунт, не зависит от последующей policy/TTL. Новый signup занятого login — CONFLICT, даже с правильным password. RESUME не использует invitation/password.

Новый LOGIN после проверки credentials возвращает challenge **без мутации**. Подтверждение пользователя отправляет второй LOGIN с expected key; immediate transaction/CAS отзывает прежний доступ, удаляет старые prekeys, устанавливает новый cursor на recipient high-water mark, сохраняет user/contact IDs. Старые live/pending sessions закрываются; concurrent loser получает новый challenge, требующий нового подтверждения. Retired key не оживляется unblock/password-login. История/ключи не переносятся, квоты/дедуп не обнуляются.

## Error codes

ERROR payload — один byte: BAD1, EXPIRED2, REVOKED3, BOUND_OTHER4, NO_PREKEY5, QUOTA6, BUSY7, CREDENTIALS8, CONFLICT9, INVITE_REQUIRED10, INVALID_INPUT11, THROTTLED12, INVITE_USED13, INVITE_LIMIT14. Opcode и error code — разные пространства: INVITE_LIMIT не REPLACE_REQUIRED и не mailbox QUOTA. Missing account/wrong password имеют одинаковый CREDENTIALS и same-cost verification; приглашение имеет отдельные lifecycle ошибки. Клиент показывает статические typed сообщения, unknown error остаётся retryable.

Argon2id v19: 19 MiB, t=2, p=1, random16 salt; максимум два nonqueued hash workers вне DB mutex/Tokio executor. Attempts: 32 global и 8 на canonical login/device за 60 s, bounded counters; без IP/resolver ban. Invitation — отдельная canonical base64url строка 43 символа/raw32, только signup; issue пишет новый файл0600, stdout только `ok`.

## Самостоятельные одноразовые приглашения

Только после account-auth/RESUME, с актуальной exact device/user проверкой active/unblocked:

| Request | Exact payload | Response |
|---|---|---|
| INVITE_ISSUE46 | issue_id16 | INVITE_ISSUED47: now8 + id16 + created8 + expires8 + state1; только Active добавляет phrase_len1 + canonical phrase |
| INVITE_REVOKE48 | issue_id16 | INVITE_REVOKED49, empty |
| INVITE_LIST50 | empty | INVITE_LISTED51: now8 + count1 + ≤8 records `[id16,created8,expires8]` |

Время — неотрицательные Unix seconds/i64 BE; state Active0/Used1/Revoked2/Expired3.
LIST — полный bounded own-active snapshot в порядке created/id, не история/каталог;
без секретов и повторных ID. Terminal ISSUE не содержит фразу. Parser отвергает
truncation/trailing bytes, неверные состояния/TTL/IDs. Один Noise message содержит
ровно один application frame.

Сервер равномерно выбирает шесть слов pinned EFF Long7776; повтор слова допустим.
`T = SHA256(b"dmsg signup phrase v1\0" || canonical_phrase)`. QR содержит прежний raw43
от T, фраза даёт те же32 байта существующего SIGNUP. Self-service entropy≈77.55bit,
не256bit; admin random32 остаётся256bit. Словарь/атрибуция —
`crates/protocol/EFF-WORDLIST.md`. Client input raw≤256bytes: строгий raw43 либо
ровно шесть слов, ASCII lowercase/case normalization и ASCII whitespace; без URI,
Unicode folding, fuzzy correction или нового серверного профиля. Raw-file/admin
parser остаётся строгим raw43.

TTL24h, ≤8 active/account. Idempotency `(owner_user_id,issue_id)` переживает restart:
найденный ID возвращает исходные времена и актуальное состояние, без продления или
расхода квоты. Новый commit расходует отдельный rolling60s бюджет8/account,
32/global; auth-attempt budget не меняется. Quota → INVITE_LIMIT, rate → THROTTLED.
Exact access, retry/quota/limiter/insert/commit сериализованы DB lock/IMMEDIATE,
без await. QR/фраза атомарно потребляют одну строку существующим signup; пароль,
occupied-login и same-key retry сохраняют прежний порядок.

Своя terminal REVOKE — no-op ACK; чужой/неизвестный ID одинаково BAD. Management
state precedence used/revoked/expired/active не меняет signup error precedence
revoked/used/expired. Приглашение принадлежит аккаунту, переживает replacement/block
выдавшего устройства; само устройство лишается новых команд. Отзыв не отключает
созданный аккаунт. Terminal rows сохраняются без GC ради durable retry, поэтому
persistent storage не ограничен восемью строками. Phrase/token в SQLite/backup —
bearer secrets, hash не является шифрованием хранения.

Первый аккаунт fresh invite_only сервера получает операторское приглашение;
последующие приглашения выпускает клиент. Secret UI очищается при pause; явные
Copy/QR-PNG Share разрешают экспорт. PNG — narrow read-only private-cache URI с
best-effort cleanup, не public secret link; секретов нет в navigation/saved state.
Android gallery/phrase import не выполняет autosubmit и не меняет trust/contact QR.
Runtime contract — `android/SELF_SERVICE_INVITATION_GATES.md`.

## Mailbox и peer binding

Opcodes16–27: SEND16/ACK17, FETCH18/RESP19, DELIVERY_ACK20, UPLOAD_PREKEYS21, CLAIM22, COUNT23, PREKEY24, COUNT_RESP25, BLOB_RESERVE26/RESERVED27. SEND_ACK: accepted1/delivered2/error3, не read receipt. Payloads/builders/test vectors — `crates/protocol/src/mailbox.rs`.

- SEND: recipient16 + message_id16 + ciphertext ≤16,304 bytes; bound гарантирует помещение события в FETCH_RESP. Durable dedup по sender_device/message_id **до quota**, retry сохраняет ciphertext/status даже на полной quota.
- FETCH_RESP: count:u16 BE, ≤32 records; каждый `seq8 + sender_device32 + sender_user16 + message_id16 + ciphertext_len2 + ciphertext`. Recipient берётся из authenticated session.
- UPLOAD_PREKEYS: Ed25519 identity32 + Curve25519 identity32 + count2 + entries(key_id4 + one_time1 + pubkey32 + signature64). Подпись над device_key/key_id/pubkey; две identity immutable для устройства.
- DEVICE_BINDING28: known user_id16 → RESP29 user_id16 + active device32 + Ed32 + Curve32. Только authenticated exact lookup, не directory/prefix search.

Core обновляет binding известных контактов перед отправкой; смена Noise/Ed/Curve сохраняет warning и **STOP до explicit confirm**. Outgoing event фиксирует SHA256 domain-separated binding полного user/device/Ed/Curve tuple; confirm не делает старый ciphertext пригодным для новой identity. Inbound event со сменившимся sender связывается с известным user_id, не ACK/discard до подтверждения. Ratchet/event ciphertext одна TX, retry byte-identical. Undecryptable integrity failures не продвигают ratchet/ACK.

## E2E v2: текст, голос, Reply и собственные изменения

Olm type0/1 framing не меняется. Plaintext строго кодируется `crates/protocol/src/e2e.rs`:

```text
[version:u8=2][kind:u8][event_mid16][sender_ed32][reply_flag:u8][optional_reply_ref48][body]
fixed header51: version0, kind1, event_mid2..18, sender_ed18..50, reply_flag50
flag0: body51; flag1: reply_ref51..99, body99
reply_ref: original_sender_noise_device32 | original_message_id16
TEXT   kind1: UTF-8 source, 1..4000 Unicode scalar values
EDIT   kind2: target_mid16 | revision:u64 BE | same UTF-8 source contract
DELETE kind3: target_mid16 | revision:u64 BE   (exact length)
VOICE  kind4: fixed166-byte voice manifest v1/profile1 (below)
```

Исходник TEXT/EDIT сохраняется byte-for-byte, включая Markdown markers, пробелы
и newlines: без trim/normalization/truncation. Whitespace-only допустим. 4000 —
Unicode scalar values, не UTF-8 bytes/UTF-16 units/grapheme clusters. Один
`e2e::validate_text` обслуживает encode/decode и Core/FFI submission. Decode
проверяет byte bound 16000 до borrowed UTF-8/scalar count и allocation; durable
history не перепроверяется, retry отправляет прежний ciphertext.
Максимальный replied TEXT и обычный EDIT проходят обе реальные Olm формы и
полные SEND/FETCH frames с pinned vodozemac0.11.0: ciphertext≤16276 при cap16304,
frame≤16384. Type1 подтверждается расшифрованным E2E ответом, не server ACK.
Reply metadata не входит в 4000 scalars / 16000 UTF-8 bytes. Static bound не
заменяет real fit test; normal Olm проверяется после настоящего E2E ответа.

Reply flag только0/1, flag1 допустим только для TEXT/VOICE; control с reference
отвергается. Reference — Noise device key автора **оригинала**, не Ed25519 key,
local ID или current contact pins. Это shallow annotation внутри Olm; incoming
reference не даёт новых прав и может указывать на отсутствующий original.
Revision1..i64MAX, whole envelope≤16099; unknown version/kind/flag, invalid UTF8/length,
legacy plaintext и trailing DELETE bytes отвергаются. MID выбирается до encrypt,
inner MID должен совпадать с outer FETCH MID; Ed сверяется с закреплённым sender.
EDIT target — TEXT; DELETE target — TEXT/VOICE того же contact/sender/MID,
не чужое сообщение/control/self-target. Pending EDIT не применяется к VOICE.

EDIT с большей revision обновляет effective text; lower/equal не перезаписывает.
DELETE терминален; поздний original/edit не воскрешает bubble. Control-before-base
сохраняется в том же `core_messages`; только winning pending edit удерживает body,
applied/superseded bodies очищаются. Crypto/event/projection commit до ACK. Controls
не bubbles/received texts/unread и не меняют original seq/time/ciphertext. SelfOnly
не wire event, queued send не отменяет. Remote actions — own Accepted/Delivered,
локальная TX без network/CLAIM; доставка отдельным saved ciphertext/retry.

Это logical live-history delete, не удаление ciphertext/SQLite pages/snapshots.
TTL/quota/Olm bounds сохраняются; indefinite convergence и lost-original recovery
не обещаются. Участвующие отправители должны перейти согласованно, old E2E decode
не вводится; backend не обязан стирать accounts/mailbox для этого формата.
Строгий E2Ev2 отвергает v1, включая уже сохранённые pending/mailbox ciphertext;
fresh local DB сама по себе не удаляет retained v1 mailbox на сервере. Приёмка
использует fresh core10/server6; будущий cutover/pending drain требует отдельного
решения. Wire2 и frame/ciphertext bounds не меняются.

Core10 хранит nullable `reply_ref` BLOB48 только на BASE TEXT/VOICE, отдельно от
control `target_mid`, без FK/new index. Reference immutable при Edit/Delete и
сохраняется атомарно с исходным event/ratchet. Новый local selector обязан быть
видимым BASE того же contact, incoming/outgoing, включая own Queued; invalid,
cross-contact, control, hidden/deleted → existing `MessageUnavailable`. TEXT
проверяет selector до network/bootstrap и повторно в write TX; новый VOICE —
в queue TX до `VoiceSessionRequired`. Доказанный duplicate VOICE того же MID,
contact/kind/reference возвращает прежнюю canonical row до audio/trust/session
проверок; сравнивается retained target identity, а не его availability/current pins.
Смена selector или None/Some — конфликт `MessageUnavailable`.

История разрешает reference одним scoped shallow LEFT JOIN contact+device+MID+BASE.
Цитата текущая: target edit обновляет raw preview≤160 scalars. Known hidden/deleted
targets сохраняют local ID/revision/direction/kind, но их text/manifest не unseal
для preview/duration. Missing имеет пустой preview и все optional target fields
None. Late original разрешается при следующем read; missing target не мешает
durable receive/ACK. Recursive quotes и stored quoted-content snapshots отсутствуют.
`HistoryMessage.reply` — единственная Reply DTO projection; received/inbox и
dialog summaries не расширяются. Rust Debug не выводит preview.

## VOICE и encrypted blobs

```text
manifest: version1 | profile1 | blob_id16 | key32 | nonce_prefix8 |
          recipient_binding32 | plain_len:u32 BE | byte_len:u32 BE |
          sample_count:u32 BE | waveform64
```

Profile1: libopus1.6.1 mono PCM16/16kHz,10kbit/s VBR/20ms,encoder10/NoLACE
decoder7, DTX/FEC/loss/DRED/BWE off. Sample count1..960000; whole encrypted
note≤128KiB including Olm/SEND_MEDIA metadata. Canonical bounded ULEB128 packet
lengths, queried lookahead/pre-skip/flush/end trim; отсутствующие packets не
скрываются PLC. Encoded parser/actual decode ограничены до allocation/playback.

ChaCha20Poly1305 key случайный на blob; nonce=`prefix8|index:u32 BE`.
AAD domain-separated и связывает blob/MID/senderEd/recipient epoch/index/count/
plain length. Plain chunk8176bytes + tag16 = ciphertext≤8192; last-size точный,
retry bytes неизменны. Manifest/key E2E; server codec/audio не читает.
Дополнительного digest/checksum registry нет.

| Request | Payload | Response |
|---|---|---|
| RESERVE26 | blob16 + ciphertext size:u32 BE | RESERVED27 blob16 |
| STATUS37 | blob16 | RESP38 blob16 + size:u32 + state:u8 + receipt bitmap:u64 |
| PUT39 | blob16 + index:u16 + length:u16 + chunk bytes | ACK40 blob16 + index:u16 |
| FINISH41 | blob16 | ACK42 blob16 |
| GET43 | blob16 + index:u16 | DATA44 blob16 + index:u16 + length:u16 + chunk bytes |
| SEND_MEDIA45 | recipient user16 + expected device32 + MID16 + blob16 + Olm ciphertext | SEND_ACK17 |

Все числа big-endian, strict parsers/geometry — `crates/protocol/src/blob.rs`.
Generic reserve≤512KiB/64chunks, window1 на клиенте. Owner и recipient ACL —
exact authenticated active device; replacement не наследует ACL. SEND_MEDIA
atomically mailbox+ACL+dedup, только completed blob; retry не меняет исходные
blob/device/ciphertext. STATUS/PUT идемпотентны, different bytes того же index —
BAD. Temp/sync/rename/parent-sync и durable DB receipt предшествуют PUT ACK;
FINISH проверяет все receipts/actual sizes. Quotas учитывают reserved+complete,
TTL24h/7d, coherent blob/SQLite backup и orphan GC.

Core10 хранит sealed manifest и encrypted chunks внутри core.db, одну canonical
VOICE row в core_messages. Queue атомарен с ratchet и immutable Olm event. После
FINISH обычный control retry отправляет manifest; upload-pending не задерживает
TEXT/DELETE. Получатель ACKed manifest без auto-download. Delivered не означает
скачивание/прослушивание. Manual download resumes bitmap после reopen; поздний
receipt не оживляет tombstone. SelfOnlyQueued не отменяет upload/send; удаление
логически очищает playable key/cache, ciphertext сервера остаётся до TTL/GC.

## Один contact QR → входящий запрос

Contact QR v1: `dmsg://contact/<base64url>` от raw126 `[version1][cid_len12][contact_id12][user_id16][device32][Ed32][Curve32]`. Профиль сервера внутри него отсутствует. Preview/cancel не меняют контакты; Add закрепляет QR pins и durable `inviting`, без второго местного Accept.

| Authenticated request | Exact payload | Response |
|---|---|---|
| CONTACT_REQUEST `30` | recipient user_id16 | CONTACT_OK `34`, empty, после durable записи |
| CONTACT_REQUESTS `31` | empty | RESP `32`: count:u8, ≤32 records `[contact_id12][binding112]` |
| CONTACT_DECIDE `33` | peer user_id16 + decision:u8 (`1` accepted, `2` blocked) | CONTACT_OK или ERROR |

Sender определяется только authenticated session. `contact_permissions` хранит directed recipient/peer requested/accepted/blocked; повтор не открывает accepted/blocked заново, pending cap32. List возвращает только собственные incoming requests и текущие active/unblocked public bindings, не каталог пользователей. SEND также создаёт отсутствующий request **в той же TX**, что новый ciphertext; dedup не создаёт новый request.

Core повторяет `inviting` через существующий reconnect/fetch; ACK переводит его в `accepted`. Request не зависит от публикации prekeys получателем: это только routing/public metadata. SEND сохраняет строгие binding/claim gates, первая отправка без ключей/сессии не сохраняет plaintext. Перед FETCH core публикует свои prekeys и получает incoming requests. Новый receiver contact имеет `incoming`, после явного Accept — `accepted_server`: ключи получены через pinned server, **не проверены лично по QR**. Старые pins не перезаписываются; подмена остаётся STOP. Explicit consent/block отправляется DECIDE при следующем fetch, повтор идемпотентен; transient сеть не отменяет локально сохранённое согласие.

Unknown/unaccepted ciphertext остаётся без ACK в существующем TTL/quota mailbox до согласия; текст не расшифровывается/не появляется в истории заранее. Explicit blocked сохраняет drop/ACK policy, включая pre-block по ID без QR keys. ACK означает обработку события устройством, не прочтение; blocked drop не является сохранением текста. Pending события могут удерживать непрерывный cursor/пачку до решения или TTL. Уже ACKed прежним клиентом drop не восстанавливается сбросом cursor.

При одновременных первых отправках нужны две независимые Olm sessions. Они ограничены двумя и хранятся в **существующем sealed session pickle**: primary fields плюс optional `dmsg_receiving_sessions` (≤1). Используются vodozemac session IDs/decrypt; после успешной проверки deterministic session-ID ordering выбирает primary для новых отправок, второй ratchet сохраняет приём ранее отправленного ciphertext. Failed authentication работает с disposable copies; account/session/event/projection commit атомарен. Это не новая криптография или повторное шифрование outbox.

## Подтверждённый порядок и время

Подтверждённая хронология — первоначальные `mailbox_events.seq` и `created_at`,
одинаковые для отправителя/получателя. Не время нажатия Send, decrypt или FETCH.
Offline send остаётся pending локально и занимает подтверждённое место только
после server commit; retry/dedup возвращают первоначальные seq/date без нового
ciphertext/MID/bubble. SEND_ACK17 и FETCH_RESP19 сохраняют прежние точные layout.

| Authenticated request | Exact payload | Response |
|---|---|---|
| MESSAGE_METADATA `35` | count:u8, 1..32, `[sender_device32][message_id16]` per key | RESP `36`: same count/order, `[seq:u64 BE][accepted_seconds:u64 BE]` per key |

Lookup разрешён только владельцу sender account либо recipient account; не каталог
чужих сообщений. Неизвестный и чужой key одинаково возвращают `(0,0)`. Positive
seq, nonnegative seconds и checked seconds→milliseconds conversion валидируются; partial,
trailing, oversized или неверное число записей — ошибка. Transport/auth layouts
не меняются; E2E v2 выше отдельный строгий формат. Metadata не означает прочтение.

Core проверяет durable replay до optional metadata lookup вне write-lock, затем
decrypt один раз в disposable crypto state под IMMEDIATE. Только unseen TEXT/VOICE
требует positive metadata с current FETCH seq; missing/stale base не продвигает
ratchet/ACK, malformed response остаётся Protocol error. Controls игнорируют order
и ACK current seq после durable commit. SEND_ACK для TEXT/VOICE сохраняет original order/
status одной TX; control ACK сохраняет только его status. Нет backfill/checked
marker/legacy prefix. Known seq/time immutable; unique index защищает base order.
Обычный base после server TTL/manual GC не получает выдуманную старую chronology;
более сильный race-free retry/retention contract не входит в эту фичу.

## Проверка и границы

`cargo build -p msgd && cargo test --workspace`, core `accounts/e2e_olm/one_qr_contacts/chronology/message_actions/voice_notes` и server `blob_probe`/fixtures проверяют schemas/auth/trust/consent/chronology/actions/atomicity/resume/ACL. Current Android voice acceptance: `android/AUTH_GATES.md`, goal `2026-10-07-voice-notes.md`; предыдущие main/pair/upgrade PASS исторические. Credentials/keys/invitations/deployment values не в Git/argv/logs; secrets read-only файлами.

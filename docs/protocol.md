# dmsg wire v2: единая account-auth логика

Реализованный пользовательский путь: **доверенный профиль сервера → Войти / Создать аккаунт → диалоги**. Серверная регистрация `invite_only` (default) или `open`; приглашение QR/файлом — только одноразовое разрешение signup, не credential входа. Server schema5/core schema6, старые schema/wire/`dmsg://join` отвергаются; ENROL/token replay/credential attach/admin invite-rebind удалены. Односторонние contact requests добавлены совместимо, без SQL migration/wipe; сначала обновляется backend, затем клиенты.

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

ERROR payload — один byte: BAD1, EXPIRED2, REVOKED3, BOUND_OTHER4, NO_PREKEY5, QUOTA6, BUSY7, CREDENTIALS8, CONFLICT9, INVITE_REQUIRED10, INVALID_INPUT11, THROTTLED12, INVITE_USED13. Opcode и error code — разные пространства. Missing account/wrong password имеют одинаковый CREDENTIALS и same-cost verification; secret invitation 256-bit имеет отдельные lifecycle ошибки. Клиент показывает статические typed сообщения, unknown error остаётся retryable.

Argon2id v19: 19 MiB, t=2, p=1, random16 salt; максимум два nonqueued hash workers вне DB mutex/Tokio executor. Attempts: 32 global и 8 на canonical login/device за 60 s, bounded counters; без IP/resolver ban. Invitation — отдельная canonical base64url строка 43 символа/raw32, только signup; issue пишет новый файл0600, stdout только `ok`.

## Mailbox и peer binding

Opcodes16–27: SEND16/ACK17, FETCH18/RESP19, DELIVERY_ACK20, UPLOAD_PREKEYS21, CLAIM22, COUNT23, PREKEY24, COUNT_RESP25, BLOB_RESERVE26/RESERVED27. SEND_ACK: accepted1/delivered2/error3, не read receipt. Payloads/builders/test vectors — `crates/protocol/src/mailbox.rs`.

- SEND: recipient16 + message_id16 + ciphertext ≤16,304 bytes; bound гарантирует помещение события в FETCH_RESP. Durable dedup по sender_device/message_id **до quota**, retry сохраняет ciphertext/status даже на полной quota.
- FETCH_RESP: count:u16 BE, ≤32 records; каждый `seq8 + sender_device32 + sender_user16 + message_id16 + ciphertext_len2 + ciphertext`. Recipient берётся из authenticated session.
- UPLOAD_PREKEYS: Ed25519 identity32 + Curve25519 identity32 + count2 + entries(key_id4 + one_time1 + pubkey32 + signature64). Подпись над device_key/key_id/pubkey; две identity immutable для устройства.
- DEVICE_BINDING28: known user_id16 → RESP29 user_id16 + active device32 + Ed32 + Curve32. Только authenticated exact lookup, не directory/prefix search.

Core обновляет binding известных контактов перед отправкой; смена Noise/Ed/Curve сохраняет warning и **STOP до explicit confirm**. Inbound event со сменившимся sender связывается с известным user_id, не ACK/discard до подтверждения; после confirm новая сессия и доставка один раз. Ratchet/outbox ciphertext одна TX, retry byte-identical. Undecryptable integrity failures также не продвигают ratchet/ACK.

## Один contact QR → входящий запрос

Contact QR v1: `dmsg://contact/<base64url>` от raw126 `[version1][cid_len12][contact_id12][user_id16][device32][Ed32][Curve32]`. Профиль сервера внутри него отсутствует. Preview/cancel не меняют контакты; Add закрепляет QR pins и durable `inviting`, без второго местного Accept.

| Authenticated request | Exact payload | Response |
|---|---|---|
| CONTACT_REQUEST `30` | recipient user_id16 | CONTACT_OK `34`, empty, после durable записи |
| CONTACT_REQUESTS `31` | empty | RESP `32`: count:u8, ≤32 records `[contact_id12][binding112]` |
| CONTACT_DECIDE `33` | peer user_id16 + decision:u8 (`1` accepted, `2` blocked) | CONTACT_OK или ERROR |

Sender определяется только authenticated session. Existing schema5 `contact_permissions` хранит directed recipient/peer requested/accepted/blocked; повтор не открывает accepted/blocked заново, pending cap32. List возвращает только собственные incoming requests и текущие active/unblocked public bindings, не каталог пользователей. SEND также создаёт отсутствующий request **в той же TX**, что новый ciphertext; dedup не создаёт новый request.

Core повторяет `inviting` через существующий reconnect/fetch; ACK переводит его в `accepted`. Request не зависит от публикации prekeys получателем: это только routing/public metadata. SEND сохраняет строгие binding/claim gates, первая отправка без ключей/сессии не сохраняет plaintext. Перед FETCH core публикует свои prekeys и получает incoming requests. Новый receiver contact имеет `incoming`, после явного Accept — `accepted_server`: ключи получены через pinned server, **не проверены лично по QR**. Старые pins не перезаписываются; подмена остаётся STOP. Explicit consent/block отправляется DECIDE при следующем fetch, повтор идемпотентен; transient сеть не отменяет локально сохранённое согласие.

Unknown/unaccepted ciphertext остаётся без ACK в существующем TTL/quota mailbox до согласия; текст не расшифровывается/не появляется в истории заранее. Explicit blocked сохраняет drop/ACK policy, включая pre-block по ID без QR keys. ACK означает обработку события устройством, не прочтение; blocked drop не является сохранением текста. Pending события могут удерживать непрерывный cursor/пачку до решения или TTL. Уже ACKed прежним клиентом drop не восстанавливается сбросом cursor.

При одновременных первых отправках нужны две независимые Olm sessions. Они ограничены двумя и хранятся в **существующем sealed session pickle**: legacy primary fields плюс optional `dmsg_receiving_sessions` (≤1). Используются vodozemac session IDs/decrypt; после успешной проверки deterministic session-ID ordering выбирает primary для новых отправок, второй ratchet сохраняет приём ранее отправленного ciphertext. Failed authentication работает с disposable copies; account/session/inbox/history commit атомарен. Это не новая криптография, SQL schema или повторное шифрование outbox.

## Проверка и границы

`cargo build -p msgd && cargo test --workspace`, core `accounts`/`e2e_olm`/`one_qr_contacts`, server `auth_probe`/`msgctl_probe`/backup fixtures проверяют schemas, policy/races/retry/replace/peer STOP, deferred consent, simultaneous first sends и pre-block. Android recursive-DNS one-QR acceptance и ограничения optical evidence: `android/AUTH_GATES.md`, `docs/goals/2026-10-07-one-qr-contacts.md`. Реальные credentials/keys/invitations/deployment values не в Git/argv/logs; production secrets read-only файлами.

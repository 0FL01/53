# dmsg: account-auth target и реализованный wire

## Действующий продуктовый контракт (2026-09-30)

Пользовательский путь: **публичный код/QR сервера → Войти / Создать аккаунт с логином/паролем → диалоги**. Приглашение отдельно требуется только для signup в `invite_only`; default `invite_only`, второй режим `open`. Вход существующего пользователя разрешён в обоих режимах, последующие подключения идут по сохранённому device key. Код содержит domain/full carrier DER/Noise pubkey, но не bearer-token, пароль или приватные ключи. Полный контракт: `ARCHITECTURE.md` §6 и `docs/goals/2026-09-29-client-track.md` R12–R16.

**Новые public-profile encoding, auth DTO/opcodes и credential/policy schema ещё не реализованы.** Их версии, bounded lengths и совместимость фиксируются coding-итерацией вместе с test vectors. Эта документация не назначает неподдерживаемые opcode и не предлагает передавать пароль в старом `ENROL` или URI.

Ниже описан **действующий legacy wire для совместимости и диагностики**, а не альтернативный пользовательский способ входа. Его успешные тесты не доказывают готовность нового login/register. Mailbox/Noise/device-key гарантии сохраняются при переходе.

## Legacy bootstrap URI (реализован)

Существующий bootstrap объединяет bearer-токен приглашения и данные подключения. Это секретное приглашение, **не новый публичный код сервера**. Его нельзя публиковать как общий профиль; приватных ключей в нём нет.

```text
dmsg://join/<base64url_nopad(versioned-binary)>
```

Legacy QR содержит ту же URI-строку. Сканер/preview офлайн; действующий путь сохраняется в старых клиентах/тестах до явной совместимой миграции, не переносится в целевой UX напрямую.

## Legacy binary layout (реализован)

```text
[version:u8 = 1]
[domain_len:u8][domain:domain_len]      # туннельный домен, ascii, 1..=253
[cert_len:u16 BE][cert_der:cert_len]    # ПОЛНЫЙ pinned carrier cert DER
[noise_pubkey:32]                        # static-публичный ключ msgd
[token:32]                               # bearer-token, 256 бит
```

Кап сырых байт: `BOOTSTRAP_MAX = 4 KiB` (типично ~1.1 KiB: домен ~30 + DER ~1024 + 64).

## Opcode (wire v1, резервы)

- `1–15` — auth/enrol: `HELLO=1` (legacy, удалён с P2), `WELCOME=2`, `AUTH_DOMAIN=3`, `ENROL=4` (token 32), `ENROLLED=5` (user_id 16 + contact_id 12 без дефисов), `ERROR=6` (код u8).
- `16+` — mailbox (P4): `SEND=16` (recipient_user 16 + sender_msg_id 16 + ciphertext ≤ CIPHERTEXT_MAX) → `SEND_ACK=17` (status + sender_msg_id); `FETCH=18` (batch ≤ 32) → `FETCH_RESP=19` (seq8 + recipient 16 + sender_device 32 + msg_id 16 + ciphertext); `DELIVERY_ACK=20` (список seq8, селективный); `UPLOAD_PREKEYS=21` (пачки device 32 + key_id 4 + pubkey 32 + sig 64); `CLAIM=22` (device 32) → `PREKEY=24` (device 32 + key_id 4 + pubkey 32 + sig 64) или `ERROR 5`; `COUNT=23` → `COUNT_RESP=25` (unconsumed u32); `BLOB_RESERVE=26` (blob_id 16 + size u32) → `BLOB_RESERVED=27`.
- Статусы `SEND_ACK`: `1` accepted, `2` delivered, `3` error. Коды ERROR: `5` no-prekey, `6` quota (1–4 — enrol, см. ниже).

## Коды ERROR

`1` bad/unknown, `2` expired, `3` revoked, `4` bound-to-other. Различимы специально: токен 256 бит не перебрать, оракла нет, а слепая диагностика дороже.

## Legacy device enrol/replay (реализован, поверх Noise)

1. Клиент после handshake и `AUTH_DOMAIN/WELCOME` шлёт `ENROL(token)`.
2. Сервер в одной транзакции: revocation → same-bound-key replay либо TTL/проверка первого bind → `ENROLLED` или `ERROR`. Device key — static инициатора из IK-сессии, не из тела запроса.
3. Повтор тем же ключом (потеря ответа/reconnect) возвращает сохранённый аккаунт даже после TTL; отозванный token/device отклоняется. Другой ключ не забирает bound token; expired unbound token отклоняется.
4. Revoke invite/device закрывает открытые streams сервера.

Account/password auth добавляется внутри завершённого pinned канала и не заменяет E2E keys. Режим регистрации проверяется на сервере; старый endpoint не должен позволить обойти новую policy, а существующий авторизованный аккаунт не теряется из-за отсутствия credentials. Новый wire потребуется проверить отдельно до объявления login/register готовыми.

Реальные значения (домен, DER, ключи, пароли и bearer) — только в защищённом deployment/app state/private fixtures, никогда в Git/argv/logs. Public profile и secret invitation различаются даже если оба передаются QR.

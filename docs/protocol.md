# dmsg protocol: bootstrap invite (P3)

Приглашение — bearer-токен + данные для подключения. Приватных ключей в нём нет.

## URI

```text
dmsg://join/<base64url_nopad(versioned-binary)>
```

QR — тот же payload (байты до base64). Сканер офлайн, превью профиля — до handshake (M2-клиент, не здесь).

## Binary layout

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
- `16+` — mailbox и дальше (P4).

## Коды ERROR

`1` bad/unknown, `2` expired, `3` revoked, `4` bound-to-other. Различимы специально: токен 256 бит не перебрать, оракла нет, а слепая диагностика дороже.

## Enrolment (сервер, поверх Noise)

1. Клиент после handshake и `AUTH_DOMAIN/WELCOME` шлёт `ENROL(token)`.
2. Сервер в одной транзакции: проверка token (срок/отзыв) → bind token→device_key (device_key — static инициатора из IK-сессии, не из тела!) → ответ `ENROLLED` или `ERROR`.
3. Повтор тем же ключом (потеря ответа) возвращает сохранённый ответ; другой ключ — `ERROR 4`, без overwrite.
4. Revoke invite/device закрывает открытые streams сервера.

Реальные значения (домен, DER, ключи) — только на сервере и в выданных URI, никогда в Git.

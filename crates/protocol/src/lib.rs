//! dmsg wire v2: `[version:u8][opcode:u8][len:u16 BE][payload..len]`.
//! Лимит — на ПОЛНЫЙ кадр (header + payload), не только payload.
//! Внутренние структуры не являются wire-контрактом: совместимость — тест-векторами ниже.

pub mod auth;
pub mod chronology;
pub mod contacts;
pub mod e2e;
pub mod mailbox;
pub mod profile;

/// Wire-версия протокола. Неизвестная версия → close.
pub const VERSION: u8 = 2;
/// WELCOME: payload — домен сервера.
pub const OP_WELCOME: u8 = 2;
/// AUTH_DOMAIN: первое transport-сообщение после Noise-handshake,
/// payload — ожидаемый клиентом домен. Сверка домена только в шифрованном канале.
pub const OP_AUTH_DOMAIN: u8 = 3;
/// ERROR: payload — код u8 (ERR_*). Opcode и код ошибки — разные пространства.
pub const OP_ERROR: u8 = 6;
/// POLICY: пустой payload; доступен после AUTH_DOMAIN, до account-auth.
pub const OP_POLICY: u8 = 7;
/// POLICY_RESP: mode u8 (InviteOnly=0, Open=1).
pub const OP_POLICY_RESP: u8 = 8;
/// SIGNUP: credentials + optional invitation (см. auth).
pub const OP_SIGNUP: u8 = 9;
/// LOGIN: credentials + optional expected-old-device (см. auth).
pub const OP_LOGIN: u8 = 10;
/// RESUME: пустой payload; device_key берётся из завершённой Noise IK-сессии.
pub const OP_RESUME: u8 = 11;
/// AUTHENTICATED: user_id 16 + contact_id 12 (Crockford, без дефисов).
pub const OP_AUTHENTICATED: u8 = 12;
/// REPLACE_REQUIRED: старое активное device_key 32; замена требует нового LOGIN.
pub const OP_REPLACE_REQUIRED: u8 = 14;
/// Коды ERROR.
pub const ERR_BAD: u8 = 1;
/// Коды ERROR.
pub const ERR_EXPIRED: u8 = 2;
/// Коды ERROR.
pub const ERR_REVOKED: u8 = 3;
/// Коды ERROR.
pub const ERR_BOUND_OTHER: u8 = 4;
/// Коды ERROR.
pub const ERR_NO_PREKEY: u8 = 5;
/// Коды ERROR.
pub const ERR_QUOTA: u8 = 6;
/// Коды ERROR: SQLITE_BUSY — повторить позже (retryable).
pub const ERR_BUSY: u8 = 7;
/// Неверный логин или пароль.
pub const ERR_CREDENTIALS: u8 = 8;
/// Конфликт логина или ожидаемой привязки устройства.
pub const ERR_CONFLICT: u8 = 9;
/// Для регистрации требуется приглашение.
pub const ERR_INVITE_REQUIRED: u8 = 10;
/// Неверный account-auth payload.
pub const ERR_INVALID_INPUT: u8 = 11;
/// Ограничение частоты попыток account-auth.
pub const ERR_THROTTLED: u8 = 12;
/// Одноразовое приглашение уже использовано.
pub const ERR_INVITE_USED: u8 = 13;
/// Диапазон 16+ — mailbox и дальше (P4).
/// SEND: recipient 16 + message_id 16 + ciphertext (rest, ≤ CIPHERTEXT_MAX).
pub const OP_SEND: u8 = 16;
/// SEND_ACK: message_id 16 + status u8 (ST_*). Только после commit.
pub const OP_SEND_ACK: u8 = 17;
/// FETCH: пустой payload — сервер отдаёт пачку после cursor (свой user/device из сессии).
pub const OP_FETCH: u8 = 18;
/// FETCH_RESP: count u16 + записи
/// (seq u64 + sender 32 + sender_user 16 + msgid 16 + ctlen u16 + ct).
pub const OP_FETCH_RESP: u8 = 19;
/// DELIVERY_ACK: count u16 + seq u64*. Cursor двигается по непрерывному.
pub const OP_DELIVERY_ACK: u8 = 20;
/// UPLOAD_PREKEYS: Ed25519 identity 32 + Curve25519 identity 32 + count u16
/// + записи (key_id u32 + one_time u8 + pubkey 32 + sig 64).
/// Подпись: Ed25519 identity-ключом по (device_key || key_id BE || pubkey).
pub const OP_UPLOAD_PREKEYS: u8 = 21;
/// CLAIM: запросить один unconsumed one-time key: device_key 32.
pub const OP_CLAIM: u8 = 22;
/// COUNT: запросить число unconsumed: device_key 32.
pub const OP_COUNT: u8 = 23;
/// PREKEY: ответ на CLAIM: key_id u32 + pubkey 32.
pub const OP_PREKEY: u8 = 24;
/// COUNT_RESP: ответ на COUNT: count u32 BE.
pub const OP_COUNT_RESP: u8 = 25;
/// BLOB_RESERVE: blob_id 16 + size u32 BE.
pub const OP_BLOB_RESERVE: u8 = 26;
/// BLOB_RESERVED: ответ: blob_id 16.
pub const OP_BLOB_RESERVED: u8 = 27;
/// DEVICE_BINDING: user_id 16; только после авторизации, без каталога логинов.
pub const OP_DEVICE_BINDING: u8 = 28;
/// DEVICE_BINDING_RESP: user_id 16 + device_key 32 + Ed25519 32 + Curve25519 32.
pub const OP_DEVICE_BINDING_RESP: u8 = 29;
/// CONTACT_REQUEST: recipient user_id16; sender comes from authenticated session.
pub const OP_CONTACT_REQUEST: u8 = 30;
/// CONTACT_REQUESTS: empty; only this account's incoming requests.
pub const OP_CONTACT_REQUESTS: u8 = 31;
/// CONTACT_REQUESTS_RESP: count u8 + (contact_id12 + DeviceBinding112)*.
pub const OP_CONTACT_REQUESTS_RESP: u8 = 32;
/// CONTACT_DECIDE: peer user_id16 + accepted1/blocked2.
pub const OP_CONTACT_DECIDE: u8 = 33;
/// CONTACT_OK: empty, after durable request/decision commit.
pub const OP_CONTACT_OK: u8 = 34;
/// MESSAGE_METADATA: count u8 + (sender_device32 + message_id16)*, max32.
pub const OP_MESSAGE_METADATA: u8 = 35;
/// MESSAGE_METADATA_RESP: count u8 + (seq u64 + accepted_seconds u64)*.
/// All-zero record means missing/not owned; no directory information is exposed.
pub const OP_MESSAGE_METADATA_RESP: u8 = 36;
/// Статусы доставки (без «прочитано» — read receipts отложены).
pub const ST_ACCEPTED: u8 = 1;
/// Статусы доставки (без «прочитано» — read receipts отложены).
pub const ST_DELIVERED: u8 = 2;
/// Статусы доставки (без «прочитано» — read receipts отложены).
pub const ST_ERROR: u8 = 3;

/// Максимальный ПОЛНЫЙ кадр в байтах (ARCH §8: frame до 16 KiB).
pub const MAX_FRAME: usize = 16 * 1024;
/// Длина заголовка кадра.
pub const HEADER_LEN: usize = 4;
/// Максимальный payload = MAX_FRAME − header.
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;
/// Максимальная длина домена.
pub const DOMAIN_MAX: usize = 253;
/// Лимит текста (E2E UTF-8).
pub const TEXT_MAX: usize = 4 * 1024;
/// Mailbox: событий на аккаунт.
pub const MAILBOX_EVENTS_MAX: usize = 512;
/// Mailbox: байт на аккаунт.
pub const MAILBOX_BYTES_MAX: usize = 32 * 1024 * 1024;
/// TTL недоставленных данных и blobs, секунд (7 суток).
pub const DATA_TTL_SECS: u64 = 7 * 24 * 3600;
/// Sweep незавершённых blob-reservation, секунд (24 часа).
pub const BLOB_RESERVE_TTL_SECS: u64 = 24 * 3600;
/// Максимум ciphertext в SEND, который также помещается в один FETCH_RESP:
/// payload минус count и расширенный event header (включая sender_user).
pub const CIPHERTEXT_MAX: usize = MAX_PAYLOAD - 2 - (8 + 32 + 16 + 16 + 2);
/// Максимум wire-ciphertext одного attachment, 512 KiB.
/// Зеркало blob::BLOB_SIZE_MAX: парсер parse_reserve режет раньше БД.
pub const BLOB_SIZE_MAX: u32 = 512 * 1024;
/// Максимум событий в одном FETCH_RESP (кап пачки).
pub const FETCH_BATCH_MAX: usize = 32;
/// Длина user_id / message_id / blob_id / device static.
pub const ID_LEN: usize = 16;
/// Длина device static / identity / prekey pubkey.
pub const KEY_LEN32: usize = 32;
/// Длина Ed25519-подписи prekey.
pub const SIG_LEN: usize = 64;

/// Ошибка декодирования кадра.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Неизвестная wire-версия.
    UnknownVersion(u8),
    /// Кадр целиком больше MAX_FRAME.
    Oversize(usize),
    /// Буфер короче заголовка или заявленной длины.
    Truncated,
}

/// Декодировать один кадр из начала буфера.
/// Возвращает `(version, opcode, payload)` и полную длину кадра в байтах.
/// Контракт: длина из сети — через usize::from (без as); переполнение
/// HEADER_LEN+len — Oversize через checked_add; срез — только после проверки
/// total против реальной длины (no panic на сетевых байтах).
pub fn decode_frame(buf: &[u8]) -> Result<(u8, u8, &[u8], usize), FrameError> {
    if buf.len() < HEADER_LEN {
        return Err(FrameError::Truncated);
    }
    let ver = buf[0];
    if ver != VERSION {
        return Err(FrameError::UnknownVersion(ver));
    }
    let len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
    let total = HEADER_LEN
        .checked_add(len)
        .filter(|&t| t <= MAX_FRAME)
        .ok_or(FrameError::Oversize(HEADER_LEN.saturating_add(len)))?;
    if buf.len() < total {
        return Err(FrameError::Truncated);
    }
    Ok((ver, buf[1], &buf[HEADER_LEN..total], total))
}

/// Закодировать кадр. Ошибка — если payload не влезает в MAX_FRAME.
/// Длина в заголовок — через u16::try_from (без as; после проверки всегда Ok,
/// т.к. MAX_PAYLOAD < u16::MAX).
pub fn encode_frame(opcode: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::Oversize(HEADER_LEN + payload.len()));
    }
    let len16 = u16::try_from(payload.len())
        .map_err(|_| FrameError::Oversize(HEADER_LEN + payload.len()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(VERSION);
    out.push(opcode);
    out.extend_from_slice(&len16.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_auth_domain() {
        let f = encode_frame(OP_AUTH_DOMAIN, b"msg.example.com").unwrap();
        let (v, op, p, total) = decode_frame(&f).unwrap();
        assert_eq!((v, op, total), (2, OP_AUTH_DOMAIN, f.len()));
        assert_eq!(p, b"msg.example.com");
    }

    #[test]
    fn max_payload_ok_oversize_err() {
        let ok = encode_frame(OP_AUTH_DOMAIN, &vec![0xAB; MAX_PAYLOAD]).unwrap();
        assert_eq!(ok.len(), MAX_FRAME);
        let (_, _, _, total) = decode_frame(&ok).unwrap();
        assert_eq!(total, MAX_FRAME);
        assert_eq!(
            encode_frame(OP_AUTH_DOMAIN, &vec![0xAB; MAX_PAYLOAD + 1]),
            Err(FrameError::Oversize(MAX_FRAME + 1))
        );
    }

    #[test]
    fn unknown_version_rejected() {
        let mut f = encode_frame(OP_AUTH_DOMAIN, b"x").unwrap();
        for version in [0, 1, 9] {
            f[0] = version;
            assert_eq!(decode_frame(&f), Err(FrameError::UnknownVersion(version)));
        }
    }

    #[test]
    fn truncated_rejected() {
        assert_eq!(decode_frame(&[2, 3]), Err(FrameError::Truncated));
        let f = encode_frame(OP_AUTH_DOMAIN, b"abcdef").unwrap();
        assert_eq!(decode_frame(&f[..5]), Err(FrameError::Truncated));
    }

    #[test]
    fn unified_auth_opcode_and_error_vectors() {
        assert_eq!(
            [
                OP_WELCOME,
                OP_AUTH_DOMAIN,
                OP_ERROR,
                OP_POLICY,
                OP_POLICY_RESP,
                OP_SIGNUP,
                OP_LOGIN,
                OP_RESUME,
                OP_AUTHENTICATED,
                OP_REPLACE_REQUIRED,
                OP_DEVICE_BINDING,
                OP_DEVICE_BINDING_RESP
            ],
            [2, 3, 6, 7, 8, 9, 10, 11, 12, 14, 28, 29]
        );
        assert_eq!(
            [
                ERR_BAD,
                ERR_EXPIRED,
                ERR_REVOKED,
                ERR_BOUND_OTHER,
                ERR_NO_PREKEY,
                ERR_QUOTA,
                ERR_BUSY,
                ERR_CREDENTIALS,
                ERR_CONFLICT,
                ERR_INVITE_REQUIRED,
                ERR_INVALID_INPUT,
                ERR_THROTTLED,
                ERR_INVITE_USED
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]
        );
    }
}

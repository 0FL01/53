//! dmsg wire v1: `[version:u8][opcode:u8][len:u16 BE][payload..len]`.
//! Лимит — на ПОЛНЫЙ кадр (header + payload), не только payload.
//! Внутренние структуры не являются wire-контрактом: совместимость — тест-векторами ниже.

pub mod bootstrap;
pub mod mailbox;

/// Wire-версия протокола. Неизвестная версия → close.
pub const VERSION: u8 = 1;
/// HELLO: payload — ожидаемый клиентом домен.
pub const OP_HELLO: u8 = 1;
/// WELCOME: payload — домен сервера.
pub const OP_WELCOME: u8 = 2;
/// AUTH_DOMAIN: первое transport-сообщение после Noise-handshake (P2),
/// payload — ожидаемый клиентом домен. Plaintext-HELLO удалён: сверка домена
/// только внутри шифрованного канала.
pub const OP_AUTH_DOMAIN: u8 = 3;
/// Резерв диапазонов: 1–15 auth/enrol, 16+ mailbox и дальше (P4).
/// ENROL: payload — token 32 байта (device_key берётся из IK-сессии, не из тела).
pub const OP_ENROL: u8 = 4;
/// ENROLLED: payload — user_id 16 + contact_id 12 (без дефисов).
pub const OP_ENROLLED: u8 = 5;
/// ERROR: payload — код u8: 1 bad/unknown, 2 expired, 3 revoked, 4 bound-to-other,
/// 5 no-prekey, 6 quota, 7 busy. ERROR=6 — это opcode кадра, не путать с payload-кодом 6.
/// Коды различимы специально (токен 256 бит не перебрать — оракла нет).
pub const OP_ERROR: u8 = 6;
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
/// Правило для клиентов: любой неизвестный payload-код считать retryable (C4),
/// старые клиенты уже так себя ведут, новые коды их не ломают.
pub const ERR_BUSY: u8 = 7;
/// Диапазон 16+ — mailbox и дальше (P4).
/// SEND: recipient 16 + message_id 16 + ciphertext (rest, ≤ CIPHERTEXT_MAX).
pub const OP_SEND: u8 = 16;
/// SEND_ACK: message_id 16 + status u8 (ST_*). Только после commit.
pub const OP_SEND_ACK: u8 = 17;
/// FETCH: пустой payload — сервер отдаёт пачку после cursor (свой user/device из сессии).
pub const OP_FETCH: u8 = 18;
/// FETCH_RESP: count u16 + записи (seq u64 + sender 32 + msgid 16 + ctlen u16 + ct).
pub const OP_FETCH_RESP: u8 = 19;
/// DELIVERY_ACK: count u16 + seq u64*. Cursor двигается по непрерывному.
pub const OP_DELIVERY_ACK: u8 = 20;
/// UPLOAD_PREKEYS: identity 32 + count u16 + записи (key_id u32 + one_time u8 + pubkey 32 + sig 64).
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
/// Максимум ciphertext в SEND: кадр минус recipient+msgid.
pub const CIPHERTEXT_MAX: usize = MAX_PAYLOAD - 32;
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
    fn roundtrip_hello() {
        let f = encode_frame(OP_HELLO, b"msg.example.com").unwrap();
        let (v, op, p, total) = decode_frame(&f).unwrap();
        assert_eq!((v, op, total), (1, OP_HELLO, f.len()));
        assert_eq!(p, b"msg.example.com");
    }

    #[test]
    fn max_payload_ok_oversize_err() {
        let ok = encode_frame(OP_HELLO, &vec![0xAB; MAX_PAYLOAD]).unwrap();
        assert_eq!(ok.len(), MAX_FRAME);
        let (_, _, _, total) = decode_frame(&ok).unwrap();
        assert_eq!(total, MAX_FRAME);
        assert_eq!(
            encode_frame(OP_HELLO, &vec![0xAB; MAX_PAYLOAD + 1]),
            Err(FrameError::Oversize(MAX_FRAME + 1))
        );
    }

    #[test]
    fn unknown_version_rejected() {
        let mut f = encode_frame(OP_HELLO, b"x").unwrap();
        f[0] = 9;
        assert_eq!(decode_frame(&f), Err(FrameError::UnknownVersion(9)));
    }

    #[test]
    fn truncated_rejected() {
        assert_eq!(decode_frame(&[1, 1]), Err(FrameError::Truncated));
        let f = encode_frame(OP_HELLO, b"abcdef").unwrap();
        assert_eq!(decode_frame(&f[..5]), Err(FrameError::Truncated));
    }
}

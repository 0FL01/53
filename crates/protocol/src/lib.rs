//! dmsg wire v1: `[version:u8][opcode:u8][len:u16 BE][payload..len]`.
//! Лимит — на ПОЛНЫЙ кадр (header + payload), не только payload.
//! Внутренние структуры не являются wire-контрактом: совместимость — тест-векторами ниже.

pub mod bootstrap;

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
/// ERROR: payload — код u8: 1 bad/unknown, 2 expired, 3 revoked, 4 bound-to-other.
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
pub fn decode_frame(buf: &[u8]) -> Result<(u8, u8, &[u8], usize), FrameError> {
    if buf.len() < HEADER_LEN {
        return Err(FrameError::Truncated);
    }
    let ver = buf[0];
    if ver != VERSION {
        return Err(FrameError::UnknownVersion(ver));
    }
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
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
pub fn encode_frame(opcode: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::Oversize(HEADER_LEN + payload.len()));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(VERSION);
    out.push(opcode);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
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

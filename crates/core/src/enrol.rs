//! K2 offline-enrol: parse → preview (без сети) → DER → Noise IK pinned →
//! AUTH_DOMAIN → ENROL → staticgen → persist 0600.
//!
//! Порядок обязателен: preview не ходит в сеть (только domain и fingerprint
//! полного DER; contact_id/user_id приходят только в ENROLLED и раньше их
//! нет); сверка ПОЛНОГО DER идёт до любого dial; Noise handshake — только с
//! pinned pubkey из invite (plaintext-HELLO запрещён — только
//! [`crate::transport::initiate_with_key`]); device_key берётся только из
//! IK-сессии (инициатор знает свой static — он и есть device_key, из тел
//! сообщений ключ никогда не читается).
//!
//! Persist: static-приватник пишется в store ДО ENROL, чтобы потеря ответа
//! переигрывалась тем же ключом (серверный replay идемпотентен); user_id,
//! contact_id и bearer-token — после ENROLLED (token — для ENROL-replay на
//! каждом коннекте, K3 `Core::login`). Olm identity — строго K3, здесь её нет.

use dmsg_protocol::{
    bootstrap, ERR_BAD, ERR_BOUND_OTHER, ERR_BUSY, ERR_EXPIRED, ERR_REVOKED, OP_ENROL, OP_ENROLLED,
    OP_ERROR,
};

use crate::transport::{Transport, TransportError};

/// Итог enrolment: только Noise static (лежит в store) + user_id/contact_id.
/// Olm identity — строго K3, здесь её нет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    /// Внутренний user_id, 16 случайных байт с сервера.
    pub user_id: [u8; 16],
    /// Публичный контактный ID, 12 символов Crockford.
    pub contact_id: String,
}

/// Офлайн-предпросмотр invite: профиль v1 = домен (один профиль на сервер,
/// ARCH §1) + fingerprint полного pinned DER. Строится БЕЗ сети.
/// contact_id/user_id здесь отсутствуют по построению: они существуют только
/// после ENROLLED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Туннельный домен из invite.
    pub domain: String,
    /// SHA-256 ПОЛНОГО carrier-DER (показать пользователю для сверки).
    pub pin_fingerprint: [u8; 32],
}

impl Preview {
    /// Fingerprint hex-строкой для показа.
    pub fn pin_fingerprint_hex(&self) -> String {
        hex(&self.pin_fingerprint)
    }
}

/// Ошибка enrolment. Ключевой материал и token сюда никогда не попадают:
/// BadQr/Protocol — статические строки (Debug bootstrap со всеми байтами не
/// печатаем — там token), остальные — коды/безопасные детали транспорта.
#[derive(Debug, PartialEq, Eq)]
pub enum EnrolError {
    /// QR не парсится (статическая причина, без содержимого).
    BadQr(&'static str),
    /// Полный DER из QR не сошёлся с ожидаемым pin (проверено до сети).
    PinMismatch,
    /// Noise handshake не сошёлся: чужой/битый pinned server key, обрыв.
    KeyMismatch(String),
    /// AUTH_DOMAIN отвергнут (закрыто без WELCOME).
    Auth(String),
    /// ERROR bad/unknown: токена нет (токен 256 бит не перебрать — оракла нет,
    /// детали не раскрываем).
    Bad,
    /// ERROR expired: срок invite вышел.
    Expired,
    /// ERROR revoked: invite или устройство отозваны.
    Revoked,
    /// ERROR bound-to-other: токен уже привязан к другому ключу (второй ключ
    /// использованным токеном учётку не клонирует).
    BoundOther,
    /// ERROR busy=7: SQLITE_BUSY на сервере — повторить позже.
    Busy,
    /// Неизвестный код ERROR: считать retryable (контракт protocol C4).
    Server(u8),
    /// Битый ответ сервера (не ENROLLED/ERROR, не та длина).
    Protocol(&'static str),
    /// Сетевая/кадровая ошибка транспорта (текст io, без секретов).
    Transport(String),
    /// Ошибка локального store (пути, не секреты).
    Store(String),
}

impl std::fmt::Display for EnrolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadQr(e) => write!(f, "bad qr: {e}"),
            Self::PinMismatch => write!(f, "transport pin mismatch"),
            Self::KeyMismatch(e) => write!(f, "noise key mismatch: {e}"),
            Self::Auth(e) => write!(f, "auth: {e}"),
            Self::Bad => write!(f, "bad token"),
            Self::Expired => write!(f, "invite expired"),
            Self::Revoked => write!(f, "revoked"),
            Self::BoundOther => write!(f, "token bound to other key"),
            Self::Busy => write!(f, "server busy, retry later"),
            Self::Server(c) => write!(f, "server error {c}, retry later"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::Store(e) => write!(f, "store: {e}"),
        }
    }
}

impl std::error::Error for EnrolError {}

impl EnrolError {
    /// true — повтор позже осмыслен (busy, неизвестные коды сервера).
    /// Pin/Key/Auth/Bad/Expired/Revoked/BoundOther повтором не лечатся.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy | Self::Server(_))
    }
}

/// SHA-256 полного carrier-DER: pin-fingerprint для preview.
pub fn pin_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(cert_der).into()
}

/// Предпросмотр invite БЕЗ сети: только domain и pin-fingerprint.
/// contact_id здесь нет и быть не может — он приходит только в ENROLLED.
pub fn preview(qr: &str) -> Result<Preview, EnrolError> {
    let b = bootstrap::parse(qr).map_err(qr_err)?;
    Ok(Preview {
        domain: String::from_utf8(b.domain).map_err(|_| EnrolError::BadQr("bad domain"))?,
        pin_fingerprint: pin_fingerprint(&b.cert_der),
    })
}

/// Полный путь enrolment из QR-строки.
///
/// Шаги: parse → сверка ПОЛНОГО DER с `expected_pin_der` (до сети) → Noise IK
/// с pinned pubkey + AUTH_DOMAIN внутри канала → ENROL → persist 0600.
///
/// `expected_pin_der`: None доверяет DER из сканированного QR как корню
/// доверия (v1 direct-путь: живого TLS-сертификата для встречной сверки нет —
/// сервер по TCP свой carrier-cert не предъявляет); Some(pin) дополнительно
/// требует полного побайтового совпадения, mismatch — [`EnrolError::PinMismatch`]
/// без единого dial. Обе проверки Noise-ключа обязательны всегда: handshake
/// падает без pinned pubkey из invite.
///
/// `addr` — `127.0.0.1:port` msgd (direct-TCP за тем же швом; DNS-путь позже).
/// Store: существующий static переиспользуется (reconnect тем же ключом →
/// серверный replay), новый генерируется и пишется ДО ENROL, user_id и
/// contact_id — после ENROLLED. Olm identity не генерируется (K3).
pub async fn enrol_from_qr(
    qr: &str,
    addr: &str,
    db_path: &std::path::Path,
    expected_pin_der: Option<&[u8]>,
) -> Result<Enrolled, EnrolError> {
    let b = bootstrap::parse(qr).map_err(qr_err)?;
    if let Some(exp) = expected_pin_der {
        if b.cert_der.as_slice() != exp {
            return Err(EnrolError::PinMismatch);
        }
    }
    let conn = crate::store::open(db_path).map_err(EnrolError::Store)?;
    let privkey = match crate::store::load_identity(&conn).map_err(EnrolError::Store)? {
        Some(k) => k,
        None => gen_static()?,
    };
    let mut ch =
        crate::transport::initiate_with_key(addr, &b.noise_pubkey, &b.domain, &privkey)
            .await
            .map_err(|e| match e {
                TransportError::Handshake(d) => EnrolError::KeyMismatch(d),
                TransportError::Auth(d) => EnrolError::Auth(d),
                other => EnrolError::Transport(other.to_string()),
            })?;
    // Persist static ДО ENROL: потеря ENROLLED переигрывается тем же ключом.
    crate::store::save_identity(&conn, &privkey).map_err(EnrolError::Store)?;
    ch.send_frame(OP_ENROL, &b.token)
        .await
        .map_err(|e| EnrolError::Transport(e.to_string()))?;
    let (op, payload) = ch.recv_frame().await.map_err(|e| EnrolError::Transport(e.to_string()))?;
    if op == OP_ENROLLED {
        let (user_id, contact_id) = parse_enrolled(&payload)?;
        crate::store::save_account(&conn, &user_id, &contact_id).map_err(EnrolError::Store)?;
        crate::store::save_token(&conn, &b.token).map_err(EnrolError::Store)?;
        return Ok(Enrolled { user_id, contact_id });
    }
    if op == OP_ERROR && payload.len() == 1 {
        return Err(map_error_code(payload[0]));
    }
    Err(EnrolError::Protocol("unexpected reply"))
}

/// Сгенерировать свежий Noise static-приватник устройства.
fn gen_static() -> Result<[u8; 32], EnrolError> {
    let params: snow::params::NoiseParams = crate::transport::PATTERN
        .parse()
        .map_err(|_| EnrolError::Store("pattern".into()))?;
    let kp = snow::Builder::new(params)
        .generate_keypair()
        .map_err(|_| EnrolError::Store("keygen".into()))?;
    kp.private
        .as_slice()
        .try_into()
        .map_err(|_| EnrolError::Store("keygen len".into()))
}

/// ENROLLED: ровно user_id 16 + contact_id 12 (Crockford, без дефисов).
fn parse_enrolled(p: &[u8]) -> Result<([u8; 16], String), EnrolError> {
    if p.len() != 28 {
        return Err(EnrolError::Protocol("bad enrolled len"));
    }
    let mut user_id = [0u8; 16];
    user_id.copy_from_slice(&p[..16]);
    let contact_id = std::str::from_utf8(&p[16..]).map_err(|_| EnrolError::Protocol("bad contact"))?;
    if contact_id.len() != 12 || !contact_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(EnrolError::Protocol("bad contact"));
    }
    Ok((user_id, contact_id.to_string()))
}

/// Маппинг wire-кодов ERROR. Неизвестные — retryable (контракт protocol C4).
fn map_error_code(code: u8) -> EnrolError {
    match code {
        ERR_BAD => EnrolError::Bad,
        ERR_EXPIRED => EnrolError::Expired,
        ERR_REVOKED => EnrolError::Revoked,
        ERR_BOUND_OTHER => EnrolError::BoundOther,
        ERR_BUSY => EnrolError::Busy,
        other => EnrolError::Server(other),
    }
}

/// Bootstrap-ошибка → статическая причина (структуру с token не печатаем).
fn qr_err(e: bootstrap::BootstrapError) -> EnrolError {
    EnrolError::BadQr(match e {
        bootstrap::BootstrapError::BadPrefix => "bad prefix",
        bootstrap::BootstrapError::BadEncoding => "bad encoding",
        bootstrap::BootstrapError::Truncated => "truncated",
        bootstrap::BootstrapError::BadVersion(_) => "bad version",
        bootstrap::BootstrapError::BadDomain => "bad domain",
        bootstrap::BootstrapError::BadCert => "bad cert",
    })
}

fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_qr() -> (String, Vec<u8>) {
        let der = vec![0x30u8, 0x03, 0x01, 0x01, 0x00];
        let uri =
            bootstrap::build(b"msg.example.com", &der, &[9u8; 32], &[7u8; 32]).expect("build");
        (uri, der)
    }

    #[test]
    fn preview_needs_no_network_and_hides_contact() {
        let (uri, der) = sample_qr();
        let pv = preview(&uri).expect("preview");
        assert_eq!(pv.domain, "msg.example.com");
        assert_eq!(pv.pin_fingerprint, pin_fingerprint(&der));
        assert_eq!(pv.pin_fingerprint_hex(), hex(&pin_fingerprint(&der)));
        // Preview по построению не содержит contact_id/user_id/token —
        // struct Preview имеет только domain и pin_fingerprint.
    }

    #[test]
    fn bad_qr_variants_are_static() {
        assert!(matches!(preview("https://x/"), Err(EnrolError::BadQr(_))));
        assert!(matches!(preview("dmsg://join/!!!"), Err(EnrolError::BadQr(_))));
        let (uri, _) = sample_qr();
        assert!(matches!(preview(&uri[..uri.len() - 4]), Err(EnrolError::BadQr(_))));
    }

    #[test]
    fn error_code_mapping_and_retryable() {
        assert_eq!(map_error_code(ERR_BAD), EnrolError::Bad);
        assert_eq!(map_error_code(ERR_EXPIRED), EnrolError::Expired);
        assert_eq!(map_error_code(ERR_REVOKED), EnrolError::Revoked);
        assert_eq!(map_error_code(ERR_BOUND_OTHER), EnrolError::BoundOther);
        assert_eq!(map_error_code(ERR_BUSY), EnrolError::Busy);
        assert!(EnrolError::Busy.is_retryable());
        // Неизвестный код — retryable по контракту protocol C4.
        assert_eq!(map_error_code(9), EnrolError::Server(9));
        assert!(EnrolError::Server(9).is_retryable());
        // Отказы повтором не лечатся.
        for e in [
            EnrolError::Bad,
            EnrolError::Expired,
            EnrolError::Revoked,
            EnrolError::BoundOther,
            EnrolError::PinMismatch,
        ] {
            assert!(!e.is_retryable(), "{e}");
        }
    }

    #[test]
    fn enrolled_parsing_is_strict() {
        let mut p = vec![0u8; 28];
        p[..16].copy_from_slice(&[1u8; 16]);
        p[16..].copy_from_slice(b"ABCD1234EFGH");
        let (uid, cid) = parse_enrolled(&p).expect("parse");
        assert_eq!((uid, cid.as_str()), ([1u8; 16], "ABCD1234EFGH"));
        assert!(parse_enrolled(&p[..27]).is_err());
        let mut bad = p.clone();
        bad[16] = b'_';
        assert!(parse_enrolled(&bad).is_err());
    }

    #[tokio::test]
    async fn wrong_pin_fails_before_any_dial() {
        // Закрытый порт + чужой DER: отказ обязан быть PinMismatch, а не
        // Transport — сверка идёт до сети.
        let (uri, _) = sample_qr();
        let b = bootstrap::parse(&uri).expect("parse");
        let mut tampered = b.cert_der.clone();
        tampered[2] ^= 0xFF;
        let _ = tampered;
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let dir = std::env::temp_dir()
            .join(format!("dmsg-k2-{}-pin", std::process::id()));
        let db = dir.join("core.db");
        let r = enrol_from_qr(&uri, &format!("127.0.0.1:{port}"), &db, Some(&[0x30, 0x00]))
            .await;
        assert_eq!(r, Err(EnrolError::PinMismatch));
        // Битый QR — BadQr, тоже без сети.
        let r2 =
            enrol_from_qr("dmsg://join/!!!", &format!("127.0.0.1:{port}"), &db, None).await;
        assert!(matches!(r2, Err(EnrolError::BadQr(_))));
        std::fs::remove_dir_all(&dir).ok();
    }
}

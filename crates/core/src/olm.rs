//! K3 Olm E2E поверх vodozemac (свой X3DH запрещён, Matrix SDK запрещён).
//!
//! Модель (согласована с сервером P4.3, см. `crates/server/src/prekey.rs`):
//! - identity — Ed25519-ключ [`vodozemac::olm::Account`]; сервер pin'ит его
//!   при первой загрузке и сверяет подпись каждой one-time записи по
//!   `(device_key || key_id BE || pubkey)`, где device_key — Noise static-
//!   публичник устройства из IK-сессии (клиент выводит его сам из
//!   хранимого приватника через X25519 — из тел сообщений ключ не берётся);
//! - one-time ключи — Curve25519; claim атомарно забирает один (consume),
//!   пустой запас — явная ошибка [`OlmError::NoPeerPrekeys`], а не тишина;
//! - refill без планировщика: [`ensure_prekeys`] зовётся на четырёх побудках —
//!   ответ claim (в send-path после claim), low-count (COUNT_RESP ниже
//!   [`PREKEY_MIN` — внутри самого ensure), reconnect
//!   ([`Core::on_reconnect`][crate::chat]), send-path (начало send_text).
//!   COUNT после upload — refill-сигнал сервера.
//!
//! Пиклы Account/Session сериализуются через store; encrypted-open шифрует
//! both columns before SQLite; the internal plain harness uses the same current schema.

use dmsg_protocol::{
    mailbox as mp, ERR_BAD, ERR_BUSY, ERR_NO_PREKEY, ERR_QUOTA, ERR_REVOKED, OP_CLAIM, OP_COUNT,
    OP_COUNT_RESP, OP_ERROR, OP_PREKEY, OP_UPLOAD_PREKEYS,
};
use vodozemac::{
    olm::{Account, OlmMessage, Session, SessionConfig},
    Curve25519PublicKey,
};

use crate::transport::Transport;

/// Ниже этого числа unconsumed one-time на сервере — догрузить до TARGET.
pub const PREKEY_MIN: u32 = 8;
/// Целевой запас one-time после refill.
pub const PREKEY_TARGET: u32 = 16;

/// Ошибка Olm-подсистемы. Ключевой материал и plaintext сюда не попадают.
#[derive(Debug, PartialEq, Eq)]
pub enum OlmError {
    /// Контакт неизвестен (нет строки).
    UnknownContact,
    /// Контакт не в accepted (requested — ждёт согласия).
    NotAccepted,
    /// Контакт заблокирован — отправка/приём запрещены.
    Blocked,
    /// Подмена identity: pinned ≠ presented. СТОП отправки до явного
    /// [`confirm_identity`][crate::contacts::confirm_identity]
    /// (голый warning запрещён).
    IdentityMismatch,
    /// Нет presented-ключей для confirm.
    NothingToConfirm,
    /// Accept без ключей (контакт заведён только по ID, QR не сканирован).
    MissingKeys,
    /// No accepted account or persistent device identity in the store.
    NotEnrolled,
    /// У пира нет свободных one-time (пустой запас — явная ошибка;
    /// повтор после refill на его стороне).
    NoPeerPrekeys,
    /// Сервер отверг upload (ERR_BAD: битва подпись либо смена identity —
    /// сервер pin'ит первую).
    UploadRejected,
    /// Квота mailbox на сервере.
    Quota,
    /// Устройство отозвано сервером.
    Revoked,
    /// Прочий ERR_BAD сервера.
    Bad,
    /// SQLITE_BUSY на сервере — повторить позже.
    Busy,
    /// Неизвестный код ERROR — retryable (контракт protocol C4).
    Server(u8),
    /// Сетевая/кадровая ошибка транспорта.
    Transport(String),
    /// Ошибка локального store.
    Store(String),
    /// Ошибка vodozemac (строка Debug без ключей — vodozemac не печатает
    /// секреты в Debug своих ошибок... во избежание утечек текст режется:
    /// только тип ошибки).
    Crypto(&'static str),
    /// Битый ответ/аргумент (статическая причина, без сетевых байт).
    Protocol(&'static str),
    /// Неизвестный тип Olm-wire-байта (первый байт ciphertext не 0/1).
    WireType(u8),
    /// Неизвестная wire-версия кадра.
    WireVersion(u8),
    /// Пустой текст или длиннее TEXT_MAX.
    BadText,
    /// Key-only resume failure, with safe typed account-auth errors.
    Auth(crate::auth::AuthError),
}

impl std::fmt::Display for OlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownContact => write!(f, "unknown contact"),
            Self::NotAccepted => write!(f, "contact not accepted"),
            Self::Blocked => write!(f, "contact blocked"),
            Self::IdentityMismatch => {
                write!(f, "identity changed, sending stopped until confirm")
            }
            Self::NothingToConfirm => write!(f, "nothing to confirm"),
            Self::MissingKeys => write!(f, "contact has no keys (scan QR first)"),
            Self::NotEnrolled => write!(f, "not authenticated"),
            Self::NoPeerPrekeys => write!(f, "peer has no one-time keys"),
            Self::UploadRejected => write!(f, "server rejected prekey upload"),
            Self::Quota => write!(f, "server quota"),
            Self::Revoked => write!(f, "device revoked"),
            Self::Bad => write!(f, "server bad request"),
            Self::Busy => write!(f, "server busy, retry later"),
            Self::Server(c) => write!(f, "server error {c}, retry later"),
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::Store(e) => write!(f, "store: {e}"),
            Self::Crypto(e) => write!(f, "crypto: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::WireType(t) => write!(f, "unknown olm wire type {t}"),
            Self::WireVersion(v) => write!(f, "unknown wire version {v}"),
            Self::BadText => write!(f, "bad text (empty or over limit)"),
            Self::Auth(e) => write!(f, "authentication: {e}"),
        }
    }
}

impl std::error::Error for OlmError {}

impl OlmError {
    /// true — повтор позже осмыслен (busy, неизвестные коды сервера).
    /// Подмена, блок, пустой запас пира, отказы — повтором не лечатся.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy | Self::Server(_))
    }
}

/// Noise static-публичник устройства (device_key для подписи prekeys и
/// CLAIM/COUNT) из хранимого приватника. Тот же вывод, что сервер делает
/// из IK-сессии (см. `crates/server/src/noise.rs`).
pub fn device_pubkey(device_priv: &[u8; 32]) -> [u8; 32] {
    let secret = x25519_dalek::StaticSecret::from(*device_priv);
    *x25519_dalek::PublicKey::from(&secret).as_bytes()
}

/// Загрузить Account из store или создать новый (пикл + next_key_id=1).
/// Возвращает (account, next_key_id).
pub fn load_or_create(conn: &rusqlite::Connection) -> Result<(Account, u32), OlmError> {
    match crate::store::load_olm(conn).map_err(OlmError::Store)? {
        Some((pickle, next)) => {
            let ap: vodozemac::olm::AccountPickle =
                serde_json::from_str(&pickle).map_err(|_| OlmError::Store("olm pickle".into()))?;
            Ok((Account::from_pickle(ap), next))
        }
        None => {
            let acc = Account::new();
            persist(conn, &acc, 1)?;
            Ok((acc, 1))
        }
    }
}

/// Сохранить пикл Account + счётчик key_id (одна строка, upsert).
pub fn persist(
    conn: &rusqlite::Connection,
    acc: &Account,
    next_key_id: u32,
) -> Result<(), OlmError> {
    let p = serde_json::to_string(&acc.pickle()).map_err(|_| OlmError::Store("pickle".into()))?;
    crate::store::save_olm(conn, &p, next_key_id).map_err(OlmError::Store)
}

/// Сериализовать сессию в пикл-строку для store.
pub fn pickle_session(s: &Session) -> Result<String, OlmError> {
    serde_json::to_string(&s.pickle()).map_err(|_| OlmError::Store("pickle".into()))
}

/// Восстановить сессию из пикла store.
pub fn unpickle_session(pickle: &str) -> Result<Session, OlmError> {
    let sp: vodozemac::olm::SessionPickle =
        serde_json::from_str(pickle).map_err(|_| OlmError::Store("session pickle".into()))?;
    Ok(Session::from_pickle(sp))
}

/// A simultaneous first send can establish a second inbound Olm session.
/// Keep both ratchets (including queued ciphertext) in the same sealed row;
/// the core chooses a deterministic primary. Legacy SessionPickle fields stay
/// at the root; no SQL schema change or destructive session replacement.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSessions {
    #[serde(flatten)]
    primary: vodozemac::olm::SessionPickle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dmsg_receiving_sessions: Vec<vodozemac::olm::SessionPickle>,
}

pub fn unpickle_sessions(pickle: &str) -> Result<Vec<Session>, OlmError> {
    let stored: StoredSessions =
        serde_json::from_str(pickle).map_err(|_| OlmError::Store("session pickle".into()))?;
    if stored.dmsg_receiving_sessions.len() > 1 {
        return Err(OlmError::Store("session bound".into()));
    }
    let mut sessions = vec![Session::from_pickle(stored.primary)];
    sessions.extend(
        stored
            .dmsg_receiving_sessions
            .into_iter()
            .map(Session::from_pickle),
    );
    Ok(sessions)
}

pub fn pickle_sessions(sessions: &[Session]) -> Result<String, OlmError> {
    if sessions.is_empty() || sessions.len() > 2 {
        return Err(OlmError::Store("session bound".into()));
    }
    serde_json::to_string(&StoredSessions {
        primary: sessions[0].pickle(),
        dmsg_receiving_sessions: sessions[1..].iter().map(Session::pickle).collect(),
    })
    .map_err(|_| OlmError::Store("session pickle".into()))
}

/// Ed25519 identity-публичник Account (32 байта — то, что pin'ит сервер).
pub fn ed_identity(acc: &Account) -> [u8; 32] {
    *acc.ed25519_key().as_bytes()
}

/// Curve25519 identity-публичник Account (для X3DH с пиром).
pub fn curve_identity(acc: &Account) -> [u8; 32] {
    *acc.curve25519_key().as_bytes()
}

/// Кодировать OlmMessage в wire-bytes для SEND: `[type:u8][body]`,
/// type — 0 prekey / 1 normal (как `to_parts`). Длина проверяется вызывающим
/// против CIPHERTEXT_MAX.
pub fn encode_wire(msg: &OlmMessage) -> Vec<u8> {
    let (t, mut body) = msg.to_parts();
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(t as u8);
    out.append(&mut body);
    out
}

/// Декодировать wire-bytes из FETCH. Неизвестный type — явная ошибка
/// (не паника, не misparse).
pub fn decode_wire(raw: &[u8]) -> Result<OlmMessage, OlmError> {
    let (&t, body) = raw
        .split_first()
        .ok_or(OlmError::Protocol("empty ciphertext"))?;
    if t != 0 && t != 1 {
        return Err(OlmError::WireType(t));
    }
    OlmMessage::from_parts(t as usize, body).map_err(|_| OlmError::Protocol("bad olm bytes"))
}

/// Создать outbound-сессию к пиру (X3DH делает vodozemac; свой код запрещён).
pub fn outbound(
    acc: &Account,
    peer_curve: &[u8; 32],
    one_time: &[u8; 32],
) -> Result<Session, OlmError> {
    let peer = Curve25519PublicKey::from_bytes(*peer_curve);
    let ot = Curve25519PublicKey::from_bytes(*one_time);
    acc.create_outbound_session(SessionConfig::version_1(), peer, ot)
        .map_err(|_| OlmError::Crypto("outbound"))
}

/// Создать inbound-сессию из prekey-сообщения. Сверяет curve-identity пира:
/// несовпадение — подмена (MismatchedIdentityKey → IdentityMismatch, СТОП).
/// Возвращает (сессия, presented_curve_identity, plaintext).
pub fn inbound(
    acc: &mut Account,
    peer_curve: &[u8; 32],
    pre: &vodozemac::olm::PreKeyMessage,
) -> Result<(Session, [u8; 32], Vec<u8>), OlmError> {
    let presented = *pre.identity_key().as_bytes();
    if presented != *peer_curve {
        return Err(OlmError::IdentityMismatch);
    }
    let peer = Curve25519PublicKey::from_bytes(*peer_curve);
    acc.create_inbound_session(SessionConfig::version_1(), peer, pre)
        .map(|r| (r.session, presented, r.plaintext))
        .map_err(|_| OlmError::Crypto("inbound"))
}

/// Проверить подпись one-time ключа пина пира: Ed25519 по
/// (peer_device || key_id BE || pubkey) — та же формула, что сервер
/// (`crates/server/src/prekey.rs::verify`). Несовпадение — подмена/битый
/// ключ, отправка СТОП.
pub fn verify_prekey_sig(
    peer_ed: &[u8; 32],
    peer_device: &[u8; 32],
    key_id: u32,
    pubkey: &[u8; 32],
    sig: &[u8; 64],
) -> Result<(), OlmError> {
    use ed25519_dalek::{Signature, VerifyingKey};
    let vk = VerifyingKey::from_bytes(peer_ed).map_err(|_| OlmError::IdentityMismatch)?;
    let mut msg = Vec::with_capacity(68);
    msg.extend_from_slice(peer_device);
    msg.extend_from_slice(&key_id.to_be_bytes());
    msg.extend_from_slice(pubkey);
    let sig = Signature::from_bytes(sig);
    vk.verify_strict(&msg, &sig)
        .map_err(|_| OlmError::IdentityMismatch)?;
    Ok(())
}

/// Подписать свой one-time для upload: та же формула своим identity-ключом.
pub fn sign_prekey(
    acc: &Account,
    device_key: &[u8; 32],
    key_id: u32,
    pubkey: &[u8; 32],
) -> [u8; 64] {
    let mut msg = Vec::with_capacity(68);
    msg.extend_from_slice(device_key);
    msg.extend_from_slice(&key_id.to_be_bytes());
    msg.extend_from_slice(pubkey);
    acc.sign(&msg).to_bytes()
}

/// COUNT: число своих unconsumed one-time на сервере.
pub async fn count(t: &mut impl Transport, device_key: &[u8; 32]) -> Result<u32, OlmError> {
    t.send_frame(OP_COUNT, device_key)
        .await
        .map_err(|e| OlmError::Transport(e.to_string()))?;
    let (op, p) = t
        .recv_frame()
        .await
        .map_err(|e| OlmError::Transport(e.to_string()))?;
    if op == OP_COUNT_RESP {
        return p
            .as_slice()
            .try_into()
            .map(u32::from_be_bytes)
            .map_err(|_| OlmError::Protocol("bad count"));
    }
    if op == OP_ERROR && p.len() == 1 {
        return Err(map_error_code(p[0]));
    }
    Err(OlmError::Protocol("unexpected count reply"))
}

/// Загрузить до `n` свежих one-time на сервер (подписанных). Возвращает
/// COUNT_RESP сервера (refill-сигнал). Пикл сохраняется ДО upload (чтобы
/// потеря ответа не сиротила ключи на сервере) и помечается published
/// ПОСЛЕ успеха. `next_key_id` — in/out счётчик.
pub async fn upload_batch(
    conn: &rusqlite::Connection,
    acc: &mut Account,
    t: &mut impl Transport,
    device_key: &[u8; 32],
    next_key_id: &mut u32,
    n: u32,
) -> Result<u32, OlmError> {
    let unpublished: Vec<[u8; 32]> = acc
        .one_time_keys()
        .values()
        .map(|k| *k.as_bytes())
        .collect();
    let need = (n as usize).saturating_sub(unpublished.len());
    if need > 0 {
        acc.generate_one_time_keys(need);
    }
    let mut pubs: Vec<[u8; 32]> = acc
        .one_time_keys()
        .values()
        .map(|k| *k.as_bytes())
        .collect();
    pubs.sort_unstable();
    pubs.truncate(n as usize);
    if pubs.is_empty() {
        return Err(OlmError::Protocol("no keys generated"));
    }
    // Резервируем key_id и сохраняем пикл ДО upload (см. выше).
    let mut entries: Vec<(u32, [u8; 32], [u8; 64])> = Vec::with_capacity(pubs.len());
    for pubkey in &pubs {
        let kid = *next_key_id;
        *next_key_id = next_key_id.saturating_add(1).max(1);
        entries.push((kid, *pubkey, sign_prekey(acc, device_key, kid, pubkey)));
    }
    persist(conn, acc, *next_key_id)?;
    let identity = ed_identity(acc);
    let mut payload = Vec::with_capacity(66 + entries.len() * 101);
    payload.extend_from_slice(&identity);
    payload.extend_from_slice(&curve_identity(acc));
    payload.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (kid, pubkey, sig) in &entries {
        payload.extend_from_slice(&kid.to_be_bytes());
        payload.push(1u8);
        payload.extend_from_slice(pubkey);
        payload.extend_from_slice(sig);
    }
    t.send_frame(OP_UPLOAD_PREKEYS, &payload)
        .await
        .map_err(|e| OlmError::Transport(e.to_string()))?;
    let (op, p) = t
        .recv_frame()
        .await
        .map_err(|e| OlmError::Transport(e.to_string()))?;
    if op == OP_COUNT_RESP {
        acc.mark_keys_as_published();
        persist(conn, acc, *next_key_id)?;
        return p
            .as_slice()
            .try_into()
            .map(u32::from_be_bytes)
            .map_err(|_| OlmError::Protocol("bad count"));
    }
    if op == OP_ERROR && p.len() == 1 {
        return Err(map_upload_error(p[0]));
    }
    Err(OlmError::Protocol("unexpected upload reply"))
}

/// Refill: если своих unconsumed меньше MIN — догрузить до TARGET.
/// Побудки снаружи: send-path, fetch-path, reconnect, после claim.
pub async fn ensure_prekeys(
    conn: &rusqlite::Connection,
    acc: &mut Account,
    t: &mut impl Transport,
    device_key: &[u8; 32],
    next_key_id: &mut u32,
) -> Result<u32, OlmError> {
    let left = count(t, device_key).await?;
    if left < PREKEY_MIN {
        upload_batch(conn, acc, t, device_key, next_key_id, PREKEY_TARGET).await
    } else {
        Ok(left)
    }
}

/// CLAIM: забрать один one-time key пира. Пустой запас — явная
/// [`OlmError::NoPeerPrekeys`]. Подпись сверяется вызывающим по пину
/// (см. [`verify_prekey_sig`]) — сервер сверял по своему пину при upload.
pub async fn claim_key(
    t: &mut impl Transport,
    peer_device: &[u8; 32],
) -> Result<(u32, [u8; 32]), OlmError> {
    t.send_frame(OP_CLAIM, peer_device)
        .await
        .map_err(|e| OlmError::Transport(e.to_string()))?;
    let (op, p) = t
        .recv_frame()
        .await
        .map_err(|e| OlmError::Transport(e.to_string()))?;
    if op == OP_PREKEY {
        let (kid, pubkey) = mp::parse_prekey(&p).ok_or(OlmError::Protocol("bad prekey"))?;
        return Ok((kid, pubkey));
    }
    if op == OP_ERROR && p.len() == 1 {
        return Err(map_error_code(p[0]));
    }
    Err(OlmError::Protocol("unexpected claim reply"))
}

/// Маппинг wire-кодов ERROR. Неизвестные — retryable (контракт protocol C4).
/// NO_PREKEY (пустой запас) — явная не-retryable ошибка: нужен refill пира.
fn map_error_code(code: u8) -> OlmError {
    match code {
        ERR_NO_PREKEY => OlmError::NoPeerPrekeys,
        ERR_QUOTA => OlmError::Quota,
        ERR_REVOKED => OlmError::Revoked,
        ERR_BAD => OlmError::Bad,
        ERR_BUSY => OlmError::Busy,
        other => OlmError::Server(other),
    }
}

/// Маппинг для upload: ERR_BAD здесь — это отказ подписи/смена identity
/// (сервер pin'ит первую) — явная ошибка, не retryable.
fn map_upload_error(code: u8) -> OlmError {
    match code {
        ERR_BAD => OlmError::UploadRejected,
        ERR_BUSY => OlmError::Busy,
        ERR_REVOKED => OlmError::Revoked,
        ERR_QUOTA => OlmError::Quota,
        other => OlmError::Server(other),
    }
}

/// Смаппить ошибку кадра в OlmError. Неизвестная версия — явно, не молча.
pub fn map_frame_err(e: dmsg_protocol::FrameError) -> OlmError {
    match e {
        dmsg_protocol::FrameError::UnknownVersion(v) => OlmError::WireVersion(v),
        dmsg_protocol::FrameError::Oversize(_) => OlmError::Protocol("oversize frame"),
        dmsg_protocol::FrameError::Truncated => OlmError::Protocol("truncated frame"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pubkey_matches_snow_derivation() {
        // Та же деривация, что сервер видит из IK (noise.rs::pubkey_of):
        // snow-пара и x25519-деривация из приватника совпадают.
        let params: snow::params::NoiseParams = crate::transport::PATTERN.parse().expect("pattern");
        let kp = snow::Builder::new(params)
            .generate_keypair()
            .expect("keygen");
        let privb: [u8; 32] = kp.private.as_slice().try_into().expect("len");
        assert_eq!(device_pubkey(&privb), kp.public.as_slice());
    }

    #[test]
    fn wire_roundtrip_and_unknown_type() {
        let alice = Account::new();
        let mut bob = Account::new();
        bob.generate_one_time_keys(1);
        let ot = *bob.one_time_keys().values().next().expect("ot").as_bytes();
        let mut s = alice
            .create_outbound_session(
                SessionConfig::version_1(),
                bob.curve25519_key(),
                Curve25519PublicKey::from_bytes(ot),
            )
            .expect("outbound");
        let m = s.encrypt(b"hi").expect("encrypt");
        let raw = encode_wire(&m);
        assert_eq!(raw[0], 0, "first message must be prekey type");
        let back = decode_wire(&raw).expect("decode");
        assert!(matches!(back, OlmMessage::PreKey(_)));
        assert_eq!(decode_wire(&[9, 0, 1]), Err(OlmError::WireType(9)));
        assert!(decode_wire(&[]).is_err());
    }

    #[test]
    fn error_mapping_and_retryable() {
        assert_eq!(map_error_code(ERR_NO_PREKEY), OlmError::NoPeerPrekeys);
        assert!(
            !OlmError::NoPeerPrekeys.is_retryable(),
            "empty stock needs peer refill"
        );
        assert!(OlmError::Busy.is_retryable());
        assert!(OlmError::Server(9).is_retryable());
        assert!(
            !OlmError::IdentityMismatch.is_retryable(),
            "substitution never auto-retries"
        );
        assert!(!OlmError::Blocked.is_retryable());
        assert_eq!(map_upload_error(ERR_BAD), OlmError::UploadRejected);
        assert_eq!(
            map_frame_err(dmsg_protocol::FrameError::UnknownVersion(9)),
            OlmError::WireVersion(9)
        );
    }

    #[test]
    fn prekey_sig_verify_roundtrip() {
        let acc = Account::new();
        let dev = [11u8; 32];
        let pk = [22u8; 32];
        let sig = sign_prekey(&acc, &dev, 7, &pk);
        verify_prekey_sig(&ed_identity(&acc), &dev, 7, &pk, &sig).expect("verify");
        let mut bad = sig;
        bad[0] ^= 0xFF;
        assert_eq!(
            verify_prekey_sig(&ed_identity(&acc), &dev, 7, &pk, &bad),
            Err(OlmError::IdentityMismatch)
        );
        // Чужой identity — тоже подмена.
        let other = Account::new();
        assert_eq!(
            verify_prekey_sig(&ed_identity(&other), &dev, 7, &pk, &sig),
            Err(OlmError::IdentityMismatch)
        );
    }
}

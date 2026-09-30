//! K3 контакты: ID, request-add, block, contact-QR, pin identity.
//!
//! Источники ID: собственный contact_id из AUTHENTICATED и чужой —
//! из contact-QR. Добавление по ID — ограниченный запрос (state `requested`,
//! без ключей отправка невозможна); доверенный QR закрепляет ключи;
//! согласие — [`accept`]; блок — [`block`] (терминален в v1).
//!
//! Contact-QR — bounded versioned binary envelope +
//! base64url URI), но другой type (префикс `dmsg://contact/`) и своя
//! независимая версия [`CONTACT_VERSION`]. Layout:
//! `[version:u8][cid_len:u8=12][contact_id 12][user_id 16]`
//! `[device_key 32][ed_identity 32][curve_identity 32]`.
//! device_key — Noise static-публичник пира (нужен CLAIM), user_id —
//! получатель SEND, ed/curve — E2E identity-ключи (pin при accept).
//!
//! Подмена identity = СТОП отправки до явного [`confirm_identity`]
//! (голый warning запрещён): повторный QR с тем же contact_id, но другими
//! ключами, pin не перезаписывает — складывает presented в `seen_*` и
//! возвращает [`QrResult::IdentityChanged`]; [`sendable`] после этого
//! отдаёт [`OlmError::IdentityMismatch`][crate::olm::OlmError].

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

use crate::olm::OlmError;

/// Contact QR prefix, distinct from the public server profile.
pub const CONTACT_PREFIX: &str = "dmsg://contact/";
/// Contact envelope version, independent of the public server profile.
pub const CONTACT_VERSION: u8 = 1;
/// Кап длины base64url-части ДО decode (raw ровно 126 байт → ~168 символов).
const CONTACT_B64_MAX: usize = 256;
/// Contact ID length (as in AUTHENTICATED).
pub const CONTACT_ID_LEN: usize = 12;

/// Состояния контакта.
pub mod state {
    /// Запрос создан (по ID или QR), согласия/ключей может не быть.
    pub const REQUESTED: &str = "requested";
    /// Согласие дано, ключи закреплены — отправка разрешена.
    pub const ACCEPTED: &str = "accepted";
    /// Заблокирован — отправка/приём запрещены (терминально в v1).
    pub const BLOCKED: &str = "blocked";
}

/// Строка контакта (ключи — Option: запрос по ID бывает без ключей).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub contact_id: String,
    pub user_id: Option<[u8; 16]>,
    pub device_key: Option<[u8; 32]>,
    pub ed_identity: Option<[u8; 32]>,
    pub curve_identity: Option<[u8; 32]>,
    pub state: String,
    pub seen_user: Option<[u8; 16]>,
    pub seen_device: Option<[u8; 32]>,
    /// Presented-подмена (None — нет). Пока Some — отправка СТОП.
    pub seen_ed: Option<[u8; 32]>,
    pub seen_curve: Option<[u8; 32]>,
}

/// Данные contact-QR (всё нужное для пина и отправки).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactQr {
    pub contact_id: String,
    pub user_id: [u8; 16],
    pub device_key: [u8; 32],
    pub ed_identity: [u8; 32],
    pub curve_identity: [u8; 32],
}

/// Ошибка contact-QR (статические причины, без содержимого).
#[derive(Debug, PartialEq, Eq)]
pub enum QrError {
    BadPrefix,
    BadEncoding,
    Truncated,
    BadVersion(u8),
    BadContact,
}

impl std::fmt::Display for QrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPrefix => write!(f, "bad prefix"),
            Self::BadEncoding => write!(f, "bad encoding"),
            Self::Truncated => write!(f, "truncated"),
            Self::BadVersion(v) => write!(f, "bad version {v}"),
            Self::BadContact => write!(f, "bad contact id"),
        }
    }
}

impl std::error::Error for QrError {}

/// Итог обработки contact-QR.
#[derive(Debug, PartialEq, Eq)]
pub enum QrResult {
    /// Новый контакт (state requested, ключи закреплены как pin).
    Added,
    /// Тот же contact_id и те же ключи — pin не тронут.
    Unchanged,
    /// Тот же contact_id, ключи ДРУГИЕ — pin не тронут, presented сложены
    /// в seen_*; отправка СТОП до confirm_identity.
    IdentityChanged,
}

/// Собрать contact-QR из своих данных (user_id — из AUTHENTICATED, device_key —
/// свой Noise static-публичник, ed/curve — свой Olm identity).
pub fn build_qr(
    contact_id: &str,
    user_id: &[u8; 16],
    device_key: &[u8; 32],
    ed_identity: &[u8; 32],
    curve_identity: &[u8; 32],
) -> Result<String, QrError> {
    if !valid_contact_id(contact_id) {
        return Err(QrError::BadContact);
    }
    let mut raw = Vec::with_capacity(126);
    raw.push(CONTACT_VERSION);
    raw.push(CONTACT_ID_LEN as u8);
    raw.extend_from_slice(contact_id.as_bytes());
    raw.extend_from_slice(user_id);
    raw.extend_from_slice(device_key);
    raw.extend_from_slice(ed_identity);
    raw.extend_from_slice(curve_identity);
    Ok(format!("{CONTACT_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw)))
}

/// Разобрать contact-QR. Строго: точная длина, версия, contact_id.
pub fn parse_qr(uri: &str) -> Result<ContactQr, QrError> {
    let b64 = uri.strip_prefix(CONTACT_PREFIX).ok_or(QrError::BadPrefix)?;
    if b64.len() > CONTACT_B64_MAX {
        return Err(QrError::Truncated);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| QrError::BadEncoding)?;
    // Layout: 1 ver + 1 cid_len + 12 cid + 16 user + 32 device + 32 ed + 32 curve = 126.
    if raw.len() != 126 {
        return Err(QrError::Truncated);
    }
    if raw[0] != CONTACT_VERSION {
        return Err(QrError::BadVersion(raw[0]));
    }
    if raw[1] != CONTACT_ID_LEN as u8 {
        return Err(QrError::BadContact);
    }
    let cid = std::str::from_utf8(&raw[2..14]).map_err(|_| QrError::BadContact)?;
    if !valid_contact_id(cid) {
        return Err(QrError::BadContact);
    }
    let mut user_id = [0u8; 16];
    let mut device_key = [0u8; 32];
    let mut ed = [0u8; 32];
    let mut curve = [0u8; 32];
    user_id.copy_from_slice(&raw[14..30]);
    device_key.copy_from_slice(&raw[30..62]);
    ed.copy_from_slice(&raw[62..94]);
    curve.copy_from_slice(&raw[94..126]);
    Ok(ContactQr {
        contact_id: cid.to_string(),
        user_id,
        device_key,
        ed_identity: ed,
        curve_identity: curve,
    })
}

/// contact_id: ровно 12 ASCII-алфанумерик (как серверный Crockford без дефисов).
pub fn valid_contact_id(s: &str) -> bool {
    s.len() == CONTACT_ID_LEN && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Запрос на добавление по ID (без ключей — отправка невозможна до QR+accept).
/// Идемпотентно: существующую строку не трогает (блок не снимает).
pub fn request_add(conn: &rusqlite::Connection, contact_id: &str) -> Result<String, OlmError> {
    if !valid_contact_id(contact_id) {
        return Err(OlmError::Protocol("bad contact id"));
    }
    conn.execute(
        "INSERT INTO core_contacts(contact_id, state) VALUES(?1,'requested')
         ON CONFLICT(contact_id) DO NOTHING",
        [contact_id],
    )
    .map_err(|e| OlmError::Store(format!("request: {e}")))?;
    Ok(get(conn, contact_id)?
        .map(|c| c.state)
        .unwrap_or_else(|| state::REQUESTED.into()))
}

/// Обработать сканированный contact-QR: новый — pin + requested; те же
/// ключи — Unchanged; другие ключи — seen_* + IdentityChanged (pin цел).
pub fn add_from_qr(conn: &rusqlite::Connection, uri: &str) -> Result<QrResult, OlmError> {
    let q = parse_qr(uri).map_err(|_| OlmError::Protocol("bad contact qr"))?;
    match get(conn, &q.contact_id)? {
        None => {
            conn.execute(
                "INSERT INTO core_contacts(contact_id, user_id, device_key,
                 ed_identity, curve_identity, state)
                 VALUES(?1,?2,?3,?4,?5,'requested')",
                rusqlite::params![
                    q.contact_id,
                    q.user_id.as_slice(),
                    q.device_key.as_slice(),
                    q.ed_identity.as_slice(),
                    q.curve_identity.as_slice(),
                ],
            )
            .map_err(|e| OlmError::Store(format!("contact: {e}")))?;
            Ok(QrResult::Added)
        }
        Some(c) => {
            let same = c.ed_identity == Some(q.ed_identity)
                && c.curve_identity == Some(q.curve_identity)
                && c.device_key == Some(q.device_key)
                && c.user_id == Some(q.user_id);
            if same {
                return Ok(QrResult::Unchanged);
            }
            // Подмена (или плановая ротация через новое устройство): pin цел,
            // presented — в seen_*. Отправка встанет через sendable.
            conn.execute(
                "UPDATE core_contacts SET seen_user=?1, seen_device=?2,
                 seen_ed=?3, seen_curve=?4 WHERE contact_id=?5",
                rusqlite::params![
                    q.user_id.as_slice(),
                    q.device_key.as_slice(),
                    q.ed_identity.as_slice(),
                    q.curve_identity.as_slice(),
                    q.contact_id,
                ],
            )
            .map_err(|e| OlmError::Store(format!("contact: {e}")))?;
            Ok(QrResult::IdentityChanged)
        }
    }
}

/// Согласие: requested + ключи есть → accepted. Blocked не снимается.
pub fn accept(conn: &rusqlite::Connection, contact_id: &str) -> Result<(), OlmError> {
    let c = get(conn, contact_id)?.ok_or(OlmError::UnknownContact)?;
    if c.state == state::BLOCKED {
        return Err(OlmError::Blocked);
    }
    if c.ed_identity.is_none() || c.curve_identity.is_none() {
        return Err(OlmError::MissingKeys);
    }
    conn.execute(
        "UPDATE core_contacts SET state='accepted' WHERE contact_id=?1",
        [contact_id],
    )
    .map_err(|e| OlmError::Store(format!("accept: {e}")))?;
    Ok(())
}

/// Блок: терминален в v1 (разблока нет). Строку создаёт при нужды (pre-block).
pub fn block(conn: &rusqlite::Connection, contact_id: &str) -> Result<(), OlmError> {
    if !valid_contact_id(contact_id) {
        return Err(OlmError::Protocol("bad contact id"));
    }
    conn.execute(
        "INSERT INTO core_contacts(contact_id, state) VALUES(?1,'blocked')
         ON CONFLICT(contact_id) DO UPDATE SET state='blocked'",
        [contact_id],
    )
    .map_err(|e| OlmError::Store(format!("block: {e}")))?;
    Ok(())
}

/// Явное подтверждение подмены: seen_* → pin, seen чистятся, сессия
/// удаляется (следующая отправка строит свежую с новыми ключами).
/// Без presented — NothingToConfirm (молчаливого approve нет).
pub fn confirm_identity(conn: &rusqlite::Connection, contact_id: &str) -> Result<(), OlmError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| OlmError::Store("confirm transaction".into()))?;
    let c = get(&tx, contact_id)?.ok_or(OlmError::UnknownContact)?;
    let (seen_ed, seen_curve) = c
        .seen_ed
        .zip(c.seen_curve)
        .ok_or(OlmError::NothingToConfirm)?;
    let user = c.seen_user.or(c.user_id).ok_or(OlmError::MissingKeys)?;
    let device = c
        .seen_device
        .or(c.device_key)
        .ok_or(OlmError::MissingKeys)?;
    tx.execute(
        "UPDATE core_contacts SET ed_identity=?1, curve_identity=?2,
         user_id=?4, device_key=?5, seen_user=NULL, seen_device=NULL,
         seen_ed=NULL, seen_curve=NULL WHERE contact_id=?3",
        rusqlite::params![
            seen_ed.as_slice(),
            seen_curve.as_slice(),
            contact_id,
            user.as_slice(),
            device.as_slice()
        ],
    )
    .map_err(|e| OlmError::Store(format!("confirm: {e}")))?;
    crate::store::delete_session(&tx, contact_id).map_err(OlmError::Store)?;
    tx.commit()
        .map_err(|_| OlmError::Store("confirm commit".into()))
}

/// Зафиксировать presented curve с receive-path (prekey с чужим identity):
/// pin цел, seen выставлен — отправка встанет до confirm.
pub fn note_presented_curve(
    conn: &rusqlite::Connection,
    contact_id: &str,
    presented: &[u8; 32],
) -> Result<(), OlmError> {
    conn.execute(
        "UPDATE core_contacts SET seen_curve=?1 WHERE contact_id=?2",
        rusqlite::params![presented.as_slice(), contact_id],
    )
    .map_err(|e| OlmError::Store(format!("contact: {e}")))?;
    Ok(())
}

/// Зафиксировать presented ed из расшифрованного envelope (подмена):
/// pin цел, seen выставлен — отправка встанет до confirm.
pub fn note_presented_ed(
    conn: &rusqlite::Connection,
    contact_id: &str,
    presented: &[u8; 32],
) -> Result<(), OlmError> {
    conn.execute(
        "UPDATE core_contacts SET seen_ed=?1 WHERE contact_id=?2",
        rusqlite::params![presented.as_slice(), contact_id],
    )
    .map_err(|e| OlmError::Store(format!("contact: {e}")))?;
    Ok(())
}

/// Предотправочный гейт: blocked → Blocked; не accepted → NotAccepted;
/// seen_* выставлены → IdentityMismatch (СТОП). Вызывать ДО любой сети.
pub fn sendable(c: &Contact) -> Result<(), OlmError> {
    if c.state == state::BLOCKED {
        return Err(OlmError::Blocked);
    }
    if c.state != state::ACCEPTED {
        return Err(OlmError::NotAccepted);
    }
    if c.seen_ed.is_some()
        || c.seen_curve.is_some()
        || c.seen_device.is_some()
        || c.seen_user.is_some()
    {
        return Err(OlmError::IdentityMismatch);
    }
    if c.user_id.is_none() || c.device_key.is_none() || c.ed_identity.is_none() {
        return Err(OlmError::MissingKeys);
    }
    if c.curve_identity.is_none() {
        return Err(OlmError::MissingKeys);
    }
    Ok(())
}

/// Загрузить контакт. None — неизвестен.
pub fn get(conn: &rusqlite::Connection, contact_id: &str) -> Result<Option<Contact>, OlmError> {
    let row: Option<(
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        String,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
    )> = conn
        .query_row(
            "SELECT user_id, device_key, ed_identity, curve_identity, state, seen_ed, seen_curve
             , seen_user, seen_device FROM core_contacts WHERE contact_id=?1",
            [contact_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()
        .map_err(|e| OlmError::Store(format!("contact: {e}")))?;
    row.map(|(u, d, e, c, st, se, sc, su, sd)| {
        Ok(Contact {
            contact_id: contact_id.to_string(),
            user_id: bytes16(u)?,
            device_key: bytes32(d)?,
            ed_identity: bytes32(e)?,
            curve_identity: bytes32(c)?,
            state: st,
            seen_user: bytes16(su)?,
            seen_device: bytes32(sd)?,
            seen_ed: bytes32(se)?,
            seen_curve: bytes32(sc)?,
        })
    })
    .transpose()
}

/// Найти контакт по Noise device_key пира (receive-path маппинг).
pub fn get_by_device(
    conn: &rusqlite::Connection,
    device: &[u8],
) -> Result<Option<Contact>, OlmError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT contact_id FROM core_contacts WHERE device_key=?1",
            [device],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| OlmError::Store(format!("contact: {e}")))?;
    id.map(|i| get(conn, &i)).transpose().map(|o| o.flatten())
}

/// Stable user lookup still identifies a known peer after device replacement.
pub fn get_by_user(conn: &rusqlite::Connection, user: &[u8]) -> Result<Option<Contact>, OlmError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT contact_id FROM core_contacts WHERE user_id=?1",
            [user],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| OlmError::Store("contact user lookup".into()))?;
    id.map(|id| get(conn, &id)).transpose().map(Option::flatten)
}

/// A pinned channel authenticates the directory, not a change of peer trust.
/// Persist the complete candidate without changing any pinned routing keys.
pub fn check_binding(
    conn: &rusqlite::Connection,
    contact_id: &str,
    binding: &dmsg_protocol::auth::DeviceBinding,
) -> Result<(), OlmError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| OlmError::Store("binding transaction".into()))?;
    let c = get(&tx, contact_id)?.ok_or(OlmError::UnknownContact)?;
    if c.user_id != Some(binding.user_id) {
        return Err(OlmError::Protocol("foreign binding"));
    }
    if c.device_key == Some(binding.device_key)
        && c.ed_identity == Some(binding.ed25519)
        && c.curve_identity == Some(binding.curve25519)
    {
        if c.seen_ed.is_some() || c.seen_curve.is_some() || c.seen_device.is_some() {
            return Err(OlmError::IdentityMismatch);
        }
        return Ok(());
    }
    tx.execute("UPDATE core_contacts SET seen_user=?2, seen_device=?3, seen_ed=?4, seen_curve=?5 WHERE contact_id=?1",rusqlite::params![contact_id,binding.user_id.as_slice(),binding.device_key.as_slice(),binding.ed25519.as_slice(),binding.curve25519.as_slice()]).map_err(|_| OlmError::Store("save peer warning".into()))?;
    tx.commit()
        .map_err(|_| OlmError::Store("peer warning commit".into()))?;
    Err(OlmError::IdentityMismatch)
}

/// Список контактов с пагинацией по контракту FFI: cursor = contact_id
/// последней выданной строки (None = сначала), limit ≤ 100.
/// Возврат: (rows, next_cursor). Без ключей — только id+state.
pub fn list(
    conn: &rusqlite::Connection,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(Vec<(String, String)>, Option<String>), OlmError> {
    let lim = (limit.min(100).max(1) + 1) as i64;
    let after = cursor.unwrap_or("");
    let mut stmt = conn
        .prepare(
            "SELECT contact_id, state FROM core_contacts
             WHERE contact_id>?1 ORDER BY contact_id LIMIT ?2",
        )
        .map_err(|e| OlmError::Store(format!("contacts: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![after, lim], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| OlmError::Store(format!("contacts: {e}")))?;
    let mut out: Vec<(String, String)> = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| OlmError::Store(format!("contact row: {e}")))?);
    }
    let next = if out.len() == lim as usize {
        out.pop();
        out.last().map(|(id, _)| id.clone())
    } else {
        None
    };
    Ok((out, next))
}

fn bytes16(v: Option<Vec<u8>>) -> Result<Option<[u8; 16]>, OlmError> {
    v.map(|b| {
        b.as_slice()
            .try_into()
            .map_err(|_| OlmError::Store("contact row".into()))
    })
    .transpose()
}

fn bytes32(v: Option<Vec<u8>>) -> Result<Option<[u8; 32]>, OlmError> {
    v.map(|b| {
        b.as_slice()
            .try_into()
            .map_err(|_| OlmError::Store("contact row".into()))
    })
    .transpose()
}

/// Минимум `optional` без нового dep (как в store.rs).
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_conn(name: &str) -> (rusqlite::Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dmsg-k3c-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let p = dir.join("core.db");
        let _ = std::fs::remove_file(&p);
        let conn = crate::store::open(&p).expect("open");
        (conn, dir)
    }

    fn sample_qr() -> (String, ContactQr) {
        let uri = build_qr(
            "ABCD1234EFGH",
            &[1u8; 16],
            &[2u8; 32],
            &[3u8; 32],
            &[4u8; 32],
        )
        .expect("build");
        let q = parse_qr(&uri).expect("parse");
        (uri, q)
    }

    #[test]
    fn qr_roundtrip_same_envelope_other_type() {
        let (uri, q) = sample_qr();
        // Тот же конверт (base64url URI), другой type (префикс) и версия.
        assert!(uri.starts_with(CONTACT_PREFIX));
        assert!(!uri.starts_with(dmsg_protocol::profile::URI_PREFIX));
        assert_eq!(
            (
                q.contact_id.as_str(),
                q.user_id,
                q.device_key,
                q.ed_identity,
                q.curve_identity
            ),
            ("ABCD1234EFGH", [1u8; 16], [2u8; 32], [3u8; 32], [4u8; 32])
        );
    }

    #[test]
    fn qr_rejects_garbage_explicitly() {
        assert_eq!(parse_qr("dmsg://join/AAAA"), Err(QrError::BadPrefix));
        assert_eq!(parse_qr("dmsg://contact/!!!"), Err(QrError::BadEncoding));
        let (uri, _) = sample_qr();
        assert_eq!(parse_qr(&uri[..uri.len() - 4]), Err(QrError::Truncated));
        // Чужой version-байт — явная ошибка (unknown version не молчит).
        let mut raw = URL_SAFE_NO_PAD
            .decode(&uri[CONTACT_PREFIX.len()..])
            .expect("raw");
        raw[0] = 9;
        assert_eq!(
            parse_qr(&format!("{CONTACT_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw))),
            Err(QrError::BadVersion(9))
        );
        assert_eq!(
            build_qr("short", &[1u8; 16], &[2u8; 32], &[3u8; 32], &[4u8; 32]),
            Err(QrError::BadContact)
        );
    }

    #[test]
    fn substitution_stops_send_until_confirm() {
        let (conn, dir) = tmp_conn("sub");
        let (uri, _) = sample_qr();
        assert_eq!(add_from_qr(&conn, &uri), Ok(QrResult::Added));
        accept(&conn, "ABCD1234EFGH").expect("accept");
        assert!(sendable(&get(&conn, "ABCD1234EFGH").expect("get").expect("row")).is_ok());
        // Тот же ID, другие ключи — подмена: pin цел, отправка СТОП.
        let evil = build_qr(
            "ABCD1234EFGH",
            &[1u8; 16],
            &[2u8; 32],
            &[9u8; 32],
            &[8u8; 32],
        )
        .expect("evil");
        assert_eq!(add_from_qr(&conn, &evil), Ok(QrResult::IdentityChanged));
        let c = get(&conn, "ABCD1234EFGH").expect("get").expect("row");
        assert_eq!(c.ed_identity, Some([3u8; 32]), "pin must not move");
        assert_eq!(c.seen_ed, Some([9u8; 32]));
        assert_eq!(sendable(&c), Err(OlmError::IdentityMismatch));
        // Голого approve нет: confirm без seen — ошибка tested below; здесь seen есть.
        confirm_identity(&conn, "ABCD1234EFGH").expect("confirm");
        let c2 = get(&conn, "ABCD1234EFGH").expect("get").expect("row");
        assert_eq!(c2.ed_identity, Some([9u8; 32]));
        assert_eq!(c2.seen_ed, None);
        assert!(sendable(&c2).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn request_block_flows() {
        let (conn, dir) = tmp_conn("flows");
        // Запрос по ID без ключей: accept невозможен (MissingKeys), отправка закрыта.
        assert_eq!(
            request_add(&conn, "ZZZZ9999YYYY"),
            Ok(state::REQUESTED.into())
        );
        assert_eq!(accept(&conn, "ZZZZ9999YYYY"), Err(OlmError::MissingKeys));
        let c = get(&conn, "ZZZZ9999YYYY").expect("get").expect("row");
        assert_eq!(sendable(&c), Err(OlmError::NotAccepted));
        // Блок терминален: accept после блока — Blocked.
        block(&conn, "ZZZZ9999YYYY").expect("block");
        assert_eq!(accept(&conn, "ZZZZ9999YYYY"), Err(OlmError::Blocked));
        assert_eq!(
            sendable(&get(&conn, "ZZZZ9999YYYY").expect("get").expect("row")),
            Err(OlmError::Blocked)
        );
        // Confirm без presented — явная ошибка, не молчаливый approve.
        assert_eq!(
            confirm_identity(&conn, "ZZZZ9999YYYY"),
            Err(OlmError::NothingToConfirm)
        );
        assert!(get(&conn, "NONEXISTENT12").expect("get").is_none());
        // Пагинация: cursor/limit → страница + next.
        request_add(&conn, "AAAA0000BBBB").expect("r");
        let (page, next) = list(&conn, None, 1).expect("list");
        assert_eq!(page.len(), 1);
        assert!(next.is_some(), "second page must exist");
        let (page2, next2) = list(&conn, next.as_deref(), 10).expect("list2");
        assert!(!page2.is_empty() && next2.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}

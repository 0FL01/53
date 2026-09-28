//! Enrolment P3: атомарный bind token→device_key в одной транзакции.
//! device_key приходит из IK-сессии (static инициатора), НЕ из тела запроса.
//! Повтор тем же ключом (потеря ответа) возвращает сохранённый ответ через JOIN,
//! без новых строк. Всё время — параметром now (Clock-injection для тестов).

use rusqlite::{Connection, OptionalExtension};

/// Успешный итог enrolment.
#[derive(Debug, PartialEq, Eq)]
pub struct Enrolled {
    /// user_id 16 байт.
    pub user_id: [u8; 16],
    /// contact_id 12 символов Crockford, без дефисов.
    pub contact_id: String,
}

/// Исход enrolment (маппится на коды ERROR 1..4).
#[derive(Debug, PartialEq, Eq)]
pub enum EnrolError {
    /// Нет токена.
    Bad,
    /// Срок вышел.
    Expired,
    /// Invite или device отозваны.
    Revoked,
    /// Привязан к другому ключу / ключ к другому user.
    BoundOther,
    /// Внутренняя ошибка хранилища.
    Store(String),
}

/// Crockford Base32 без I,L,O,U (12 символов = 60 бит).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn gen_contact_id() -> Result<String, EnrolError> {
    let mut raw = [0u8; 8];
    getrandom::fill(&mut raw).map_err(|e| EnrolError::Store(format!("rng: {e}")))?;
    let mut v: u64 = u64::from_le_bytes(raw) & ((1 << 60) - 1);
    let mut s = String::with_capacity(12);
    for _ in 0..12 {
        s.push(CROCKFORD[(v & 31) as usize] as char);
        v >>= 5;
    }
    Ok(s)
}

/// Атомарный enrol. Блокировки короткие; вызывать БЕЗ удержания через .await.
pub fn enrol(
    conn: &Connection,
    token: &[u8],
    device_key: &[u8],
    now: i64,
) -> Result<Enrolled, EnrolError> {
    if token.len() != 32 || device_key.len() != 32 {
        return Err(EnrolError::Bad);
    }
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| EnrolError::Store(format!("begin: {e}")))?;
    let out = enrol_tx(conn, token, device_key, now);
    match &out {
        Ok(_) => conn
            .execute_batch("COMMIT")
            .map_err(|e| EnrolError::Store(format!("commit: {e}")))?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    out
}

fn enrol_tx(
    conn: &Connection,
    token: &[u8],
    device_key: &[u8],
    now: i64,
) -> Result<Enrolled, EnrolError> {
    let row: Option<(i64, Option<Vec<u8>>, i64)> = conn
        .query_row(
            "SELECT expires_at, bound_device_key, revoked FROM invites WHERE token=?1",
            [token],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| EnrolError::Store(format!("invite: {e}")))?;
    let (expires_at, bound, revoked) = row.ok_or(EnrolError::Bad)?;
    if revoked != 0 {
        return Err(EnrolError::Revoked);
    }
    if now > expires_at {
        return Err(EnrolError::Expired);
    }
    // Устройство уже известно?
    let known_user: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT user_id, revoked FROM devices WHERE device_key=?1",
            [device_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| EnrolError::Store(format!("device: {e}")))?;
    match (bound, known_user) {
        (Some(b), _) if b.as_slice() == device_key => {
            // Replay тем же ключом: сохранённый ответ через JOIN.
            if device_revoked(conn, device_key)? {
                return Err(EnrolError::Revoked);
            }
            saved_answer(conn, device_key)
        }
        (Some(_), _) => Err(EnrolError::BoundOther),
        (None, Some(_)) => Err(EnrolError::BoundOther), // ключ уже в другом аккаунте
        (None, None) => {
            // Первый bind: новый user + device.
            let mut user_id = [0u8; 16];
            getrandom::fill(&mut user_id).map_err(|e| EnrolError::Store(format!("rng: {e}")))?;
            let mut contact_id = String::new();
            let mut ok = false;
            for _ in 0..3 {
                contact_id = gen_contact_id()?;
                let r = conn.execute(
                    "INSERT INTO users(user_id, contact_id, created_at) VALUES(?1,?2,?3)",
                    rusqlite::params![user_id.as_slice(), contact_id, now],
                );
                match r {
                    Ok(_) => {
                        ok = true;
                        break;
                    }
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        continue; // коллизия contact_id ~2^-60, ретрай
                    }
                    Err(e) => return Err(EnrolError::Store(format!("user: {e}"))),
                }
            }
            if !ok {
                return Err(EnrolError::Store("contact_id collision".into()));
            }
            conn.execute(
                "INSERT INTO devices(device_key, user_id, created_at, revoked) VALUES(?1,?2,?3,0)",
                rusqlite::params![device_key, user_id.as_slice(), now],
            )
            .map_err(|e| EnrolError::Store(format!("device: {e}")))?;
            conn.execute(
                "UPDATE invites SET bound_device_key=?1 WHERE token=?2",
                rusqlite::params![device_key, token],
            )
            .map_err(|e| EnrolError::Store(format!("bind: {e}")))?;
            Ok(Enrolled { user_id, contact_id })
        }
    }
}

fn device_revoked(conn: &Connection, device_key: &[u8]) -> Result<bool, EnrolError> {
    conn.query_row(
        "SELECT revoked FROM devices WHERE device_key=?1",
        [device_key],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|o| o.unwrap_or(0) != 0)
    .map_err(|e| EnrolError::Store(format!("device: {e}")))
}

fn saved_answer(conn: &Connection, device_key: &[u8]) -> Result<Enrolled, EnrolError> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT u.user_id, u.contact_id FROM users u
             JOIN devices d ON d.user_id = u.user_id
             WHERE d.device_key=?1",
            [device_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| EnrolError::Store(format!("saved: {e}")))?;
    match row {
        Some((uid, cid)) if uid.len() == 16 => {
            let mut user_id = [0u8; 16];
            user_id.copy_from_slice(&uid);
            Ok(Enrolled { user_id, contact_id: cid })
        }
        _ => Err(EnrolError::Store("saved answer missing".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    fn issue(conn: &Connection, token: &[u8], now: i64, ttl: i64) {
        conn.execute(
            "INSERT INTO invites(token, created_at, expires_at, revoked) VALUES(?1,?2,?3,0)",
            rusqlite::params![token, now, now + ttl],
        )
        .unwrap();
    }

    #[test]
    fn happy_and_replay() {
        let conn = mem();
        let token = [1u8; 32];
        let key = [2u8; 32];
        issue(&conn, &token, 1000, 3600);
        let a = enrol(&conn, &token, &key, 1001).unwrap();
        assert_eq!(a.contact_id.len(), 12);
        let b = enrol(&conn, &token, &key, 1002).unwrap();
        assert_eq!(a, b); // потеря ответа: тот же итог, новых строк нет
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn other_key_refused_used_token_refused() {
        let conn = mem();
        let token = [3u8; 32];
        issue(&conn, &token, 1000, 3600);
        enrol(&conn, &token, &[4u8; 32], 1001).unwrap();
        assert_eq!(enrol(&conn, &token, &[5u8; 32], 1002), Err(EnrolError::BoundOther));
        assert_eq!(enrol(&conn, &[9u8; 32], &[5u8; 32], 1002), Err(EnrolError::Bad));
    }

    #[test]
    fn expired_revoked_and_revoked_device() {
        let conn = mem();
        let t1 = [11u8; 32];
        issue(&conn, &t1, 1000, 10);
        assert_eq!(enrol(&conn, &t1, &[12u8; 32], 2000), Err(EnrolError::Expired));
        let t2 = [13u8; 32];
        issue(&conn, &t2, 1000, 3600);
        conn.execute("UPDATE invites SET revoked=1 WHERE token=?1", [&t2[..]]).unwrap();
        assert_eq!(enrol(&conn, &t2, &[14u8; 32], 1001), Err(EnrolError::Revoked));
        // replay после revoke устройства
        let t3 = [15u8; 32];
        let k3 = [16u8; 32];
        issue(&conn, &t3, 1000, 3600);
        enrol(&conn, &t3, &k3, 1001).unwrap();
        conn.execute("UPDATE devices SET revoked=1 WHERE device_key=?1", [&k3[..]]).unwrap();
        assert_eq!(enrol(&conn, &t3, &k3, 1002), Err(EnrolError::Revoked));
    }

    #[test]
    fn second_token_on_same_key_refused() {
        let conn = mem();
        let k = [21u8; 32];
        issue(&conn, &[22u8; 32], 1000, 3600);
        issue(&conn, &[23u8; 32], 1000, 3600);
        enrol(&conn, &[22u8; 32], &k, 1001).unwrap();
        assert_eq!(enrol(&conn, &[23u8; 32], &k, 1002), Err(EnrolError::BoundOther));
    }
}

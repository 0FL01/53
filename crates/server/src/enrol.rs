//! Enrolment P3: атомарный bind token→device_key в одной транзакции.
//! device_key приходит из IK-сессии (static инициатора), НЕ из тела запроса.
//! Повтор тем же ключом (потеря ответа) возвращает сохранённый ответ через JOIN,
//! без новых строк. Всё время — параметром now (Clock-injection для тестов).

use rusqlite::{Connection, OptionalExtension};
use dmsg_protocol::{ERR_BAD, ERR_BOUND_OTHER, ERR_BUSY, ERR_EXPIRED, ERR_REVOKED};

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
    /// SQLITE_BUSY: повторить позже (wire ERR_BUSY=7). Маппится ДО схлопывания в Store.
    Busy,
    /// Внутренняя ошибка хранилища.
    Store(String),
}

/// Wire-код ошибки (см. dmsg_protocol::ERR_*).
impl EnrolError {
    pub fn code(&self) -> u8 {
        match self {
            EnrolError::Bad | EnrolError::Store(_) => ERR_BAD,
            EnrolError::Expired => ERR_EXPIRED,
            EnrolError::Revoked => ERR_REVOKED,
            EnrolError::BoundOther => ERR_BOUND_OTHER,
            EnrolError::Busy => ERR_BUSY,
        }
    }
}

/// SQLITE_BUSY → Busy, остальное — Store с контекстом. Вызывать на КАЖДОМ
/// rusqlite-результате до схлопывания, иначе busy утонет в Store→ERR_BAD.
fn store(prefix: &str, e: rusqlite::Error) -> EnrolError {
    if matches!(&e, rusqlite::Error::SqliteFailure(f, _)
        if f.code == rusqlite::ErrorCode::DatabaseBusy)
    {
        EnrolError::Busy
    } else {
        EnrolError::Store(format!("{prefix}: {e}"))
    }
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

/// Атомарный enrol. RAII-транзакция: Drop без commit = ROLLBACK, соединение
/// никогда не остаётся отравленным (следующий enrol жив). Блокировки короткие;
/// вызывать БЕЗ удержания через .await.
pub fn enrol(
    conn: &mut Connection,
    token: &[u8],
    device_key: &[u8],
    now: i64,
) -> Result<Enrolled, EnrolError> {
    if token.len() != 32 || device_key.len() != 32 {
        return Err(EnrolError::Bad);
    }
    let tx = conn.transaction().map_err(|e| store("begin", e))?;
    let out = enrol_tx(&tx, token, device_key, now)?;
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(out)
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
        .map_err(|e| store("invite", e))?;
    let (expires_at, bound, revoked) = row.ok_or(EnrolError::Bad)?;
    if revoked != 0 {
        return Err(EnrolError::Revoked);
    }
    if bound.as_deref() == Some(device_key) {
        // TTL ограничивает первую регистрацию, а не вход bound Noise static.
        // Отзыв и сохранённый JOIN проверяются и после истечения приглашения.
        if device_revoked(conn, device_key)? {
            return Err(EnrolError::Revoked);
        }
        return saved_answer(conn, device_key);
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
        .map_err(|e| store("device", e))?;
    match (bound, known_user) {
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
                    Err(e) => return Err(store("user", e)),
                }
            }
            if !ok {
                return Err(EnrolError::Store("contact_id collision".into()));
            }
            conn.execute(
                "INSERT INTO devices(device_key, user_id, created_at, revoked) VALUES(?1,?2,?3,0)",
                rusqlite::params![device_key, user_id.as_slice(), now],
            )
            .map_err(|e| store("device", e))?;
        conn.execute(
            "UPDATE invites SET bound_device_key=?1 WHERE token=?2",
            rusqlite::params![device_key, token],
        )
        .map_err(|e| store("bind", e))?;
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
    .map_err(|e| store("device", e))
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
        .map_err(|e| store("saved", e))?;
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
        let mut conn = mem();
        let token = [1u8; 32];
        let key = [2u8; 32];
        issue(&conn, &token, 1000, 3600);
        let a = enrol(&mut conn, &token, &key, 1001).unwrap();
        assert_eq!(a.contact_id.len(), 12);
        let b = enrol(&mut conn, &token, &key, 1002).unwrap();
        assert_eq!(a, b); // потеря ответа: тот же итог, новых строк нет
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn bound_replay_after_expiry_keeps_identity_and_rows() {
        let mut conn = mem();
        let token = [41u8; 32];
        let key = [42u8; 32];
        issue(&conn, &token, 1000, 10);
        let first = enrol(&mut conn, &token, &key, 1001).unwrap();
        for now in [1010, 1011, 100_000] {
            assert_eq!(enrol(&mut conn, &token, &key, now), Ok(Enrolled {
                user_id: first.user_id,
                contact_id: first.contact_id.clone(),
            }));
        }
        let counts: (i64, i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM users), (SELECT COUNT(*) FROM devices),
                    (SELECT COUNT(*) FROM invites)",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(counts, (1, 1, 1));
        let bound: Vec<u8> = conn.query_row(
            "SELECT bound_device_key FROM invites WHERE token=?1",
            [&token[..]], |r| r.get(0),
        ).unwrap();
        assert_eq!(bound.as_slice(), key.as_slice());
    }

    #[test]
    fn expired_invites_reject_first_bind_and_other_keys() {
        let mut conn = mem();
        let used = [43u8; 32];
        let unused = [44u8; 32];
        let key = [45u8; 32];
        issue(&conn, &used, 1000, 10);
        issue(&conn, &unused, 1000, 10);
        enrol(&mut conn, &used, &key, 1001).unwrap();
        for (token, device) in [(&used, &[46u8; 32]), (&unused, &[46u8; 32]), (&unused, &key)] {
            assert_eq!(enrol(&mut conn, token, device, 1011), Err(EnrolError::Expired));
        }
        assert_eq!(enrol(&mut conn, &[47u8; 32], &key, 1011), Err(EnrolError::Bad));
        let counts: (i64, i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM users), (SELECT COUNT(*) FROM devices),
                    (SELECT COUNT(*) FROM invites WHERE bound_device_key IS NOT NULL)",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(counts, (1, 1, 1));
    }

    #[test]
    fn expired_bound_replay_still_checks_revocation() {
        for revoke in ["UPDATE invites SET revoked=1", "UPDATE devices SET revoked=1"] {
            let mut conn = mem();
            let token = [48u8; 32];
            let key = [49u8; 32];
            issue(&conn, &token, 1000, 10);
            enrol(&mut conn, &token, &key, 1001).unwrap();
            conn.execute(revoke, []).unwrap();
            assert_eq!(enrol(&mut conn, &token, &key, 1011), Err(EnrolError::Revoked));
        }
    }

    #[test]
    fn expired_bound_replay_with_missing_or_corrupt_identity_fails_closed() {
        for corrupt in [
            "DELETE FROM devices",
            "DELETE FROM users",
            "UPDATE users SET user_id=x'01'; UPDATE devices SET user_id=x'01'",
            "UPDATE devices SET user_id=NULL",
        ] {
            let mut conn = mem();
            let token = [50u8; 32];
            let key = [51u8; 32];
            issue(&conn, &token, 1000, 10);
            enrol(&mut conn, &token, &key, 1001).unwrap();
            conn.execute_batch(corrupt).unwrap();
            let err = enrol(&mut conn, &token, &key, 1011).unwrap_err();
            assert!(matches!(err, EnrolError::Store(_)));
            assert_eq!(err.code(), ERR_BAD);
        }
    }

    #[test]
    fn second_token_on_same_key_refused() {
        let mut conn = mem();
        let k = [21u8; 32];
        issue(&conn, &[22u8; 32], 1000, 3600);
        issue(&conn, &[23u8; 32], 1000, 3600);
        enrol(&mut conn, &[22u8; 32], &k, 1001).unwrap();
        assert_eq!(enrol(&mut conn, &[23u8; 32], &k, 1002), Err(EnrolError::BoundOther));
    }

    #[test]
    fn failed_tx_rolls_back_and_next_enrol_alive() {
        let mut conn = mem();
        // ROLLBACK-семантика RAII: незакоммиченная TX исчезает с Drop.
        {
            let tx = conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO users(user_id,contact_id,created_at) VALUES(?1,'t',1)",
                [vec![9u8; 16]],
            )
            .unwrap();
            // без commit: Drop = ROLLBACK
        }
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // Неудачный enrol не травит соединение: следующий жив.
        let t = [31u8; 32];
        issue(&conn, &t, 1000, 10);
        assert_eq!(enrol(&mut conn, &t, &[32u8; 32], 2000), Err(EnrolError::Expired));
        let t2 = [33u8; 32];
        issue(&conn, &t2, 1000, 3600);
        assert!(enrol(&mut conn, &t2, &[34u8; 32], 1001).is_ok());
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn sqlite_busy_maps_to_busy_code() {
        let e = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None);
        assert_eq!(store("t", e), EnrolError::Busy);
        assert_eq!(EnrolError::Busy.code(), dmsg_protocol::ERR_BUSY);
        assert_eq!(dmsg_protocol::ERR_BUSY, 7);
    }

    #[test]
    fn busy_lock_from_second_conn_is_busy_and_next_enrol_alive() {
        // Файловый DB: второй коннект держит BEGIN IMMEDIATE, наш transaction()
        // упирается в SQLITE_BUSY → Busy (wire 7), а не Store→ERR_BAD.
        let dir = std::env::temp_dir().join(format!("msgd-busy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("busy.db");
        {
            let setup = Connection::open(&path).unwrap();
            db::migrate(&setup).unwrap();
            setup
                .execute(
                    "INSERT INTO invites(token, created_at, expires_at, revoked) VALUES(?1,?2,?3,0)",
                    rusqlite::params![[1u8; 32].as_slice(), 1000, 1000 + 3600],
                )
                .unwrap();
        }
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut locked = Connection::open(&path).unwrap();
        let err = enrol(&mut locked, &[1u8; 32], &[2u8; 32], 1001).unwrap_err();
        assert_eq!(err, EnrolError::Busy);
        assert_eq!(err.code(), 7);
        holder.execute_batch("ROLLBACK").unwrap();
        // Лок снят — следующий enrol жив.
        let done = enrol(&mut locked, &[1u8; 32], &[2u8; 32], 1002).unwrap();
        assert_eq!(done.contact_id.len(), 12);
        drop(holder);
        drop(locked);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_revoked_and_revoked_device() {
        let mut conn = mem();
        let t1 = [11u8; 32];
        issue(&conn, &t1, 1000, 10);
        assert_eq!(enrol(&mut conn, &t1, &[12u8; 32], 2000), Err(EnrolError::Expired));
        let t2 = [13u8; 32];
        issue(&conn, &t2, 1000, 3600);
        conn.execute("UPDATE invites SET revoked=1 WHERE token=?1", [&t2[..]]).unwrap();
        assert_eq!(enrol(&mut conn, &t2, &[14u8; 32], 1001), Err(EnrolError::Revoked));
        // replay после revoke устройства
        let t3 = [15u8; 32];
        let k3 = [16u8; 32];
        issue(&conn, &t3, 1000, 3600);
        enrol(&mut conn, &t3, &k3, 1001).unwrap();
        conn.execute("UPDATE devices SET revoked=1 WHERE device_key=?1", [&k3[..]]).unwrap();
        assert_eq!(enrol(&mut conn, &t3, &k3, 1002), Err(EnrolError::Revoked));
    }
}

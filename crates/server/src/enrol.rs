//! Enrolment P3: атомарный bind token→device_key в одной транзакции.
//! device_key приходит из IK-сессии (static инициатора), НЕ из тела запроса.
//! Повтор тем же ключом (потеря ответа) возвращает сохранённый ответ через JOIN,
//! без новых строк. Всё время — параметром now (Clock-injection для тестов).

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
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
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| store("begin", e))?;
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
    let row: Option<(i64, Option<Vec<u8>>, i64, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT expires_at, bound_device_key, revoked, rebind_user_id FROM invites WHERE token=?1",
            [token],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| store("invite", e))?;
    let (expires_at, bound, revoked, rebind_user) = row.ok_or(EnrolError::Bad)?;
    if revoked != 0 {
        return Err(EnrolError::Revoked);
    }
    if bound.as_deref() == Some(device_key) {
        // TTL ограничивает первую регистрацию, а не вход bound Noise static.
        // Отзыв и сохранённый JOIN проверяются и после истечения приглашения.
        if device_revoked(conn, device_key)? {
            return Err(EnrolError::Revoked);
        }
        let answer = saved_answer(conn, device_key)?;
        if let Some(target) = rebind_user {
            if target.as_slice() != answer.user_id {
                return Err(EnrolError::Bad);
            }
        }
        return Ok(answer);
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
            if let Some(target) = rebind_user {
                return claim_rebind(conn, token, device_key, &target, now);
            }
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
        Some((uid, cid)) if uid.len() == 16 && valid_contact_id(&cid) => {
            let mut user_id = [0u8; 16];
            user_id.copy_from_slice(&uid);
            Ok(Enrolled { user_id, contact_id: cid })
        }
        _ => Err(EnrolError::Store("saved answer missing".into())),
    }
}

fn valid_contact_id(cid: &str) -> bool {
    cid.len() == 12 && cid.bytes().all(|b| CROCKFORD.contains(&b))
}

fn target_answer(conn: &Connection, target: &[u8]) -> Result<Enrolled, EnrolError> {
    if target.len() != 16 {
        return Err(EnrolError::Bad);
    }
    let cid: Option<String> = conn.query_row(
        "SELECT contact_id FROM users WHERE user_id=?1", [target], |r| r.get(0),
    ).optional().map_err(|e| store("target", e))?;
    let contact_id = cid.filter(|c| valid_contact_id(c)).ok_or(EnrolError::Bad)?;
    Ok(Enrolled { user_id: target.try_into().expect("checked"), contact_id })
}

fn active_other(conn: &Connection, user: &[u8], device: &[u8]) -> Result<bool, EnrolError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM devices WHERE user_id=?1 AND revoked=0 AND device_key<>?2)",
        rusqlite::params![user, device], |r| r.get(0),
    ).map_err(|e| store("active", e))
}

/// Local admin operation only. Every mutation, including old login-token
/// invalidation and the one-time invitation, commits together.
pub fn issue_rebind(
    conn: &mut Connection,
    old_device: &[u8],
    token: &[u8],
    now: i64,
    expires_at: i64,
) -> Result<Enrolled, EnrolError> {
    if old_device.len() != 32 || token.len() != 32 || expires_at < now {
        return Err(EnrolError::Bad);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| store("begin", e))?;
    let answer = saved_answer(&tx, old_device)?;
    if active_other(&tx, &answer.user_id, old_device)? {
        return Err(EnrolError::BoundOther);
    }
    tx.execute("UPDATE devices SET revoked=1 WHERE device_key=?1", [old_device])
        .map_err(|e| store("revoke", e))?;
    // A later issuance supersedes unclaimed invitations; all prior bound
    // account login tokens are retired, including those of blocked devices.
    tx.execute(
        "UPDATE invites SET revoked=1 WHERE
         bound_device_key IN (SELECT device_key FROM devices WHERE user_id=?1)
         OR (rebind_user_id=?1 AND bound_device_key IS NULL)",
        [answer.user_id.as_slice()],
    ).map_err(|e| store("retire", e))?;
    tx.execute("DELETE FROM prekeys WHERE device_key=?1", [old_device])
        .map_err(|e| store("prekeys", e))?;
    tx.execute(
        "INSERT INTO invites(token,created_at,expires_at,rebind_user_id) VALUES(?1,?2,?3,?4)",
        rusqlite::params![token, now, expires_at, answer.user_id.as_slice()],
    ).map_err(|e| store("invite", e))?;
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(answer)
}

fn claim_rebind(
    conn: &Connection, token: &[u8], device: &[u8], target: &[u8], now: i64,
) -> Result<Enrolled, EnrolError> {
    let answer = target_answer(conn, target)?;
    if active_other(conn, target, device)? {
        return Err(EnrolError::BoundOther);
    }
    conn.execute(
        "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,?3)",
        rusqlite::params![device, target, now],
    ).map_err(|e| store("device", e))?;
    // Old ciphertext stays for dedup/quota/TTL, but is never fetched by the
    // replacement. This is abandonment, not a forged DELIVERY_ACK: neither
    // delivered flags nor another device/user's cursor are changed.
    conn.execute(
        "INSERT INTO cursors(recipient_user_id,device_key,last_seq)
         SELECT ?1,?2,COALESCE(MAX(seq),0) FROM mailbox_events WHERE recipient_user_id=?1",
        rusqlite::params![target, device],
    ).map_err(|e| store("cursor", e))?;
    conn.execute("UPDATE invites SET bound_device_key=?1 WHERE token=?2",
        rusqlite::params![device, token],
    ).map_err(|e| store("bind", e))?;
    conn.execute(
        "UPDATE invites SET revoked=1 WHERE rebind_user_id=?1 AND token<>?2 AND bound_device_key IS NULL",
        rusqlite::params![target, token],
    ).map_err(|e| store("retire", e))?;
    Ok(answer)
}

/// Undo a mistaken block only if it cannot enable a second active device.
/// Retired rebind login tokens stay retired. Unknown keys retain the old no-op.
pub fn unblock(conn: &mut Connection, device: &[u8]) -> Result<(), EnrolError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| store("begin", e))?;
    let known: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE device_key=?1)",
        [device], |r| r.get(0),
    ).map_err(|e| store("device", e))?;
    if known {
        let answer = saved_answer(&tx, device)?;
        if active_other(&tx, &answer.user_id, device)? {
            return Err(EnrolError::BoundOther);
        }
        tx.execute("UPDATE devices SET revoked=0 WHERE device_key=?1", [device])
            .map_err(|e| store("unblock", e))?;
    }
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(())
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

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn rebind_fixture() -> (Connection, Enrolled) {
        let mut conn = mem();
        issue(&conn, &[1u8; 32], 1000, 10);
        let account = enrol(&mut conn, &[1u8; 32], &[2u8; 32], 1001).unwrap();
        (conn, account)
    }

    #[test]
    fn rebind_blocked_old_same_account_replay_and_unblock_conflict() {
        let (mut conn, account) = rebind_fixture();
        conn.execute("UPDATE devices SET revoked=1", []).unwrap();
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
        assert_eq!(enrol(&mut conn, &[1u8; 32], &[2u8; 32], 1003), Err(EnrolError::Revoked));
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[2u8; 32], 1003), Err(EnrolError::BoundOther));
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1003).unwrap(), account);
        for now in [1004, 1013, 100_000] {
            assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], now).unwrap(), account);
        }
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM users"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices WHERE revoked=0"), 1);
        assert_eq!(unblock(&mut conn, &[2u8; 32]), Err(EnrolError::BoundOther));
        assert_eq!(issue_rebind(&mut conn, &[2u8; 32], &[5u8; 32], 1005, 1015), Err(EnrolError::BoundOther));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites"), 2);
        // A block mistake on the replacement can still be undone.
        conn.execute("UPDATE devices SET revoked=1", []).unwrap();
        unblock(&mut conn, &[4u8; 32]).unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 100_000).unwrap(), account);
        conn.execute("UPDATE devices SET revoked=1", []).unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 100_000), Err(EnrolError::Revoked));
        conn.execute("UPDATE invites SET revoked=1 WHERE token=?1", [[3u8; 32].as_slice()]).unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 100_000), Err(EnrolError::Revoked));
    }

    #[test]
    fn unclaimed_rebind_expiry_revocation_and_latest_issue_wins() {
        let (mut conn, account) = rebind_fixture();
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1013), Err(EnrolError::Expired));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices"), 1);
        issue_rebind(&mut conn, &[2u8; 32], &[5u8; 32], 1003, 1013).unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1014), Err(EnrolError::Revoked));
        // Simulate another pending invite (old/corrupt admin writer). Claim
        // invalidates it in the same TX, regardless of issuance ordering.
        conn.execute("INSERT INTO invites(token,created_at,expires_at,rebind_user_id) VALUES(?1,1003,1013,?2)",
            rusqlite::params![[6u8; 32].as_slice(), account.user_id.as_slice()],
        ).unwrap();
        assert_eq!(enrol(&mut conn, &[5u8; 32], &[4u8; 32], 1013).unwrap(), account);
        assert_eq!(enrol(&mut conn, &[6u8; 32], &[7u8; 32], 1013), Err(EnrolError::Revoked));
        assert_eq!(enrol(&mut conn, &[5u8; 32], &[7u8; 32], 1013), Err(EnrolError::BoundOther));
    }

    #[test]
    fn failed_rebind_issue_rolls_back_device_tokens_and_prekeys() {
        let (mut conn, _) = rebind_fixture();
        conn.execute("INSERT INTO prekeys VALUES(?1,1,?2,?3,1,0)",
            rusqlite::params![[2u8; 32].as_slice(), [8u8; 32].as_slice(), [9u8; 64].as_slice()],
        ).unwrap();
        // Token collision occurs after revoke/delete; all mutations roll back.
        assert!(matches!(issue_rebind(&mut conn, &[2u8; 32], &[1u8; 32], 1002, 1012), Err(EnrolError::Store(_))));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices WHERE revoked=0"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites WHERE revoked=0"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM prekeys"), 1);
        assert!(enrol(&mut conn, &[1u8; 32], &[2u8; 32], 100_000).is_ok());
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1003, 1013).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM prekeys"), 0);
    }

    #[test]
    fn missing_corrupt_targets_fail_closed_without_mutations() {
        for corrupt in [
            "DELETE FROM devices", "DELETE FROM users",
            "UPDATE devices SET user_id=NULL",
            "UPDATE users SET contact_id=NULL", "UPDATE users SET contact_id='bad'",
            "UPDATE users SET user_id=x'01'; UPDATE devices SET user_id=x'01'",
        ] {
            let (mut conn, _) = rebind_fixture();
            conn.execute_batch(corrupt).unwrap();
            assert!(issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).is_err());
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites WHERE revoked=0"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices WHERE revoked<>0"), 0);
        }
        for target in [vec![9u8; 16], vec![9u8]] {
            let (mut conn, _) = rebind_fixture();
            conn.execute("INSERT INTO invites(token,created_at,expires_at,rebind_user_id) VALUES(?1,1002,1012,?2)",
                rusqlite::params![[3u8; 32].as_slice(), target],
            ).unwrap();
            assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1003), Err(EnrolError::Bad));
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites WHERE bound_device_key IS NOT NULL"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM cursors"), 0);
        }
    }

    #[test]
    fn unblocking_before_claim_prevents_rebind_until_blocked_again() {
        let (mut conn, _) = rebind_fixture();
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
        unblock(&mut conn, &[2u8; 32]).unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1003), Err(EnrolError::BoundOther));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices"), 1);
        // Unblock never resurrects the retired old login token.
        assert_eq!(enrol(&mut conn, &[1u8; 32], &[2u8; 32], 1003), Err(EnrolError::Revoked));
    }

    #[test]
    fn rebind_abandons_history_preserves_dedup_quotas_and_other_users() {
        use crate::{mbox, prekey};
        let (mut conn, account) = rebind_fixture();
        issue(&conn, &[10u8; 32], 1000, 100);
        let peer = enrol(&mut conn, &[10u8; 32], &[11u8; 32], 1001).unwrap();
        let old_seq = match mbox::send(&mut conn, &[11u8; 32], &account.user_id, &[12u8; 16], b"old", 1001).unwrap() {
            mbox::SendOutcome::New(seq) => seq, _ => unreachable!(),
        };
        mbox::ack(&mut conn, &account.user_id, &[2u8; 32], &[old_seq]).unwrap();
        mbox::send(&mut conn, &[2u8; 32], &peer.user_id, &[13u8; 16], b"peer", 1001).unwrap();
        conn.execute("INSERT INTO blob_meta VALUES(?1,?2,512,1001,9999,'reserved')",
            rusqlite::params![[14u8; 16].as_slice(), account.user_id.as_slice()],
        ).unwrap();
        let old_identity = ed25519_dalek::SigningKey::from_bytes(&[15u8; 32]);
        upload_test_key(&mut conn, &[2u8; 32], &account.user_id, &old_identity);
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
        // Old-key ciphertext received while waiting for claim is abandoned too.
        mbox::send(&mut conn, &[11u8; 32], &account.user_id, &[16u8; 16], b"pending", 1002).unwrap();
        enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1003).unwrap();
        assert!(mbox::fetch(&conn, &account.user_id, &[4u8; 32]).unwrap().is_empty());
        assert_eq!(mbox::fetch(&conn, &peer.user_id, &[11u8; 32]).unwrap().len(), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM mailbox_events WHERE delivered=1"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM mailbox_events"), 3);
        assert_eq!(count(&conn, "SELECT SUM(LENGTH(ciphertext)) FROM mailbox_events"), 14);
        assert_eq!(count(&conn, "SELECT SUM(size) FROM blob_meta WHERE state='reserved'"), 512);
        assert_eq!(prekey::count(&conn, &[4u8; 32]).unwrap(), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM device_identities"), 1);
        assert!(matches!(mbox::send(&mut conn, &[11u8; 32], &account.user_id, &[12u8; 16], b"old", 1004).unwrap(), mbox::SendOutcome::Exists(_, true)));
        // Fresh keys are uploaded normally, not copied from the lost device.
        let fresh_identity = ed25519_dalek::SigningKey::from_bytes(&[17u8; 32]);
        upload_test_key(&mut conn, &[4u8; 32], &account.user_id, &fresh_identity);
        let stored: Vec<u8> = conn.query_row("SELECT identity_pubkey FROM device_identities WHERE device_key=?1",
            [[4u8; 32].as_slice()], |r| r.get(0),
        ).unwrap();
        assert_ne!(stored, old_identity.verifying_key().to_bytes());
        assert_eq!(stored, fresh_identity.verifying_key().to_bytes());
        let fresh_seq = match mbox::send(&mut conn, &[11u8; 32], &account.user_id, &[18u8; 16], b"fresh", 1004).unwrap() {
            mbox::SendOutcome::New(seq) => seq, _ => unreachable!(),
        };
        assert_eq!(mbox::fetch(&conn, &account.user_id, &[4u8; 32]).unwrap().len(), 1);
        assert_eq!(mbox::ack(&mut conn, &account.user_id, &[4u8; 32], &[fresh_seq]).unwrap(), fresh_seq);
        assert!(mbox::fetch(&conn, &account.user_id, &[4u8; 32]).unwrap().is_empty());
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 100_000).unwrap(), account);
        assert_eq!(mbox::ack(&mut conn, &account.user_id, &[4u8; 32], &[]).unwrap(), fresh_seq);
        let old_cursor: i64 = conn.query_row("SELECT last_seq FROM cursors WHERE device_key=?1",
            [[2u8; 32].as_slice()], |r| r.get(0),
        ).unwrap();
        assert_eq!(old_cursor, old_seq);
    }

    fn upload_test_key(conn: &mut Connection, device: &[u8], user: &[u8], identity: &ed25519_dalek::SigningKey) {
        use ed25519_dalek::Signer;
        let pubkey = [19u8; 32];
        let mut binding = device.to_vec();
        binding.extend_from_slice(&1u32.to_be_bytes());
        binding.extend_from_slice(&pubkey);
        let signature = identity.sign(&binding).to_bytes();
        crate::prekey::upload(conn, device, user, &identity.verifying_key().to_bytes(),
            &[crate::prekey::Entry { key_id: 1, one_time: 1, pubkey: &pubkey, sig: &signature }],
        ).unwrap();
    }

    #[test]
    fn simultaneous_rebind_claims_one_active_device_same_or_distinct_tokens() {
        for distinct_tokens in [false, true] {
            let dir = std::env::temp_dir().join(format!("msgd-rebind-race-{}-{distinct_tokens}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("race.db");
            let mut conn = db::connect(&path).unwrap();
            db::migrate(&conn).unwrap();
            issue(&conn, &[1u8; 32], 1000, 100);
            let account = enrol(&mut conn, &[1u8; 32], &[2u8; 32], 1001).unwrap();
            issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
            if distinct_tokens {
                conn.execute("INSERT INTO invites(token,created_at,expires_at,rebind_user_id) VALUES(?1,1002,1012,?2)",
                    rusqlite::params![[5u8; 32].as_slice(), account.user_id.as_slice()],
                ).unwrap();
            }
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let mut workers = Vec::new();
            for i in 0..2 {
                let path = path.clone();
                let barrier = barrier.clone();
                workers.push(std::thread::spawn(move || {
                    let mut db = db::connect(path).unwrap();
                    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                    barrier.wait();
                    let token = if i == 1 && distinct_tokens { [5u8; 32] } else { [3u8; 32] };
                    enrol(&mut db, &token, &[6 + i; 32], 1003)
                }));
            }
            let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            let expected = if distinct_tokens { EnrolError::Revoked } else { EnrolError::BoundOther };
            assert_eq!(results.iter().find_map(|r| r.as_ref().err()).unwrap(), &expected);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM users"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices WHERE revoked=0"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM cursors"), 1);
            drop(conn);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn claim_after_concurrent_expiry_or_revocation_has_no_partial_writes() {
        for revoked in [false, true] {
            let dir = std::env::temp_dir().join(format!("msgd-rebind-revoke-{}-{revoked}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("race.db");
            let mut conn = db::connect(&path).unwrap();
            db::migrate(&conn).unwrap();
            issue(&conn, &[1u8; 32], 1000, 100);
            enrol(&mut conn, &[1u8; 32], &[2u8; 32], 1001).unwrap();
            issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            let update = if revoked { "UPDATE invites SET revoked=1" } else { "UPDATE invites SET expires_at=0" };
            conn.execute(update, []).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let worker_barrier = barrier.clone();
            let (attempted, attempt) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut other = Connection::open(path).unwrap();
                other.busy_timeout(std::time::Duration::ZERO).unwrap();
                worker_barrier.wait();
                attempted.send(enrol(&mut other, &[3u8; 32], &[4u8; 32], 1003)).unwrap();
                worker_barrier.wait();
                enrol(&mut other, &[3u8; 32], &[4u8; 32], 1003)
            });
            barrier.wait();
            // The claimant demonstrably raced the still-uncommitted writer.
            assert_eq!(attempt.recv().unwrap(), Err(EnrolError::Busy));
            conn.execute_batch("COMMIT").unwrap();
            barrier.wait();
            assert_eq!(worker.join().unwrap(), Err(if revoked { EnrolError::Revoked } else { EnrolError::Expired }));
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices"), 1);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM cursors"), 0);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites WHERE bound_device_key IS NOT NULL"), 1);
            drop(conn);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn claim_storage_failure_rolls_back_device_cursor_bind_and_stale_retirement() {
        let (mut conn, account) = rebind_fixture();
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
        conn.execute("INSERT INTO invites(token,created_at,expires_at,rebind_user_id) VALUES(?1,1002,1012,?2)",
            rusqlite::params![[5u8; 32].as_slice(), account.user_id.as_slice()],
        ).unwrap();
        conn.execute_batch("CREATE TRIGGER fail_retire BEFORE UPDATE OF revoked ON invites
            WHEN NEW.revoked=1 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(matches!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1003), Err(EnrolError::Store(_))));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM devices"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM cursors"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM invites WHERE revoked=0 AND bound_device_key IS NULL"), 2);
        conn.execute_batch("DROP TRIGGER fail_retire").unwrap();
        assert_eq!(enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1004).unwrap(), account);
    }

    #[test]
    fn rebind_does_not_reset_account_mailbox_quota() {
        let (mut conn, account) = rebind_fixture();
        conn.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<512)
            INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at)
            SELECT ?1,?2,randomblob(16),x'01',1001 FROM n",
            rusqlite::params![account.user_id.as_slice(), [8u8; 32].as_slice()],
        ).unwrap();
        issue_rebind(&mut conn, &[2u8; 32], &[3u8; 32], 1002, 1012).unwrap();
        enrol(&mut conn, &[3u8; 32], &[4u8; 32], 1003).unwrap();
        assert!(crate::mbox::fetch(&conn, &account.user_id, &[4u8; 32]).unwrap().is_empty());
        assert_eq!(crate::mbox::send(&mut conn, &[8u8; 32], &account.user_id, &[9u8; 16], b"new", 1004).unwrap_err(), crate::mbox::MboxError::Quota);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM mailbox_events"), 512);
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

//! Account auth: bounded Argon2id work outside the DB mutex; atomic signup/CAS.
use argon2::{
    password_hash::{PasswordHash, SaltString},
    Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version,
};
use dmsg_protocol::{
    auth::{self, RegistrationMode},
    *,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

pub const HASH_SLOTS: usize = 2;
const WINDOW: Duration = Duration::from_secs(60);
const GLOBAL_LIMIT: u32 = 32;
const KEY_LIMIT: u32 = 8;
const KEY_CAP: usize = 2048;

fn argon() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19 * 1024, 2, 1, None).expect("fixed params"),
    )
}
fn hash(password: &[u8]) -> Result<String, u8> {
    let mut salt = [0; 16];
    getrandom::fill(&mut salt).map_err(|_| ERR_BAD)?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| ERR_BAD)?;
    argon()
        .hash_password(password, &salt)
        .map(|h| h.to_string())
        .map_err(|_| ERR_BAD)
}
fn verify(password: &[u8], hash: &str) -> bool {
    // Only this server's fixed-cost format is accepted. Corrupt/untrusted DB
    // parameters must not turn a bounded worker into unbounded memory work.
    if hash.len() > 256 || !hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$") {
        return false;
    }
    PasswordHash::new(hash)
        .ok()
        .is_some_and(|h| argon().verify_password(password, &h).is_ok())
}

#[derive(Default)]
struct Attempts {
    global: Option<(Instant, u32)>,
    keys: HashMap<Vec<u8>, (Instant, u32)>,
}
impl Attempts {
    fn admit(&mut self, login: &str, device: &[u8; 32], now: Instant) -> bool {
        self.keys
            .retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        if self
            .global
            .is_none_or(|(start, _)| now.duration_since(start) >= WINDOW)
        {
            self.global = Some((now, 0));
        }
        let global = self.global.as_mut().expect("initialized");
        if global.1 >= GLOBAL_LIMIT {
            return false;
        }
        let mut login_key = vec![0];
        login_key.extend_from_slice(login.as_bytes());
        let mut device_key = vec![1];
        device_key.extend_from_slice(device);
        let keys = [login_key, device_key];
        let missing = keys.iter().filter(|k| !self.keys.contains_key(*k)).count();
        if self.keys.len() + missing > KEY_CAP
            || keys
                .iter()
                .any(|k| self.keys.get(k).is_some_and(|(_, n)| *n >= KEY_LIMIT))
        {
            return false;
        }
        global.1 += 1;
        for k in keys {
            self.keys.entry(k).or_insert((now, 0)).1 += 1;
        }
        true
    }
}

pub struct Engine {
    slots: Arc<Semaphore>,
    attempts: Mutex<Attempts>,
    dummy: String,
}
impl Engine {
    pub async fn new() -> Result<Self, u8> {
        let dummy = tokio::task::spawn_blocking(|| hash(b"dummy-credential-unavailable"))
            .await
            .map_err(|_| ERR_BAD)??;
        Ok(Self {
            slots: Arc::new(Semaphore::new(HASH_SLOTS)),
            attempts: Mutex::new(Attempts::default()),
            dummy,
        })
    }

    /// Returns only non-secret wire errors. The owned slot stays with the blocking
    /// worker even if its connection is revoked/cancelled while hashing.
    pub async fn account(
        &self,
        db: &Arc<Mutex<Connection>>,
        device: &[u8; 32],
        op: u8,
        payload: &[u8],
        now: i64,
    ) -> Result<Outcome, u8> {
        let (creds, optional) = match op {
            OP_SIGNUP => {
                let r = auth::parse_signup(payload).map_err(|_| ERR_INVALID_INPUT)?;
                (r.credentials, r.invitation.copied())
            }
            OP_LOGIN => {
                let r = auth::parse_login(payload).map_err(|_| ERR_INVALID_INPUT)?;
                (r.credentials, r.replace.copied())
            }
            _ => return Err(ERR_INVALID_INPUT),
        };
        if !self
            .attempts
            .lock()
            .expect("attempts")
            .admit(creds.login, device, Instant::now())
        {
            return Err(ERR_THROTTLED);
        }
        // No queue of pending memory-hard jobs. At most two running workers.
        let mut snapshot = {
            let conn = db.lock().expect("db");
            let snapshot = lookup(&conn, creds.login)?;
            if op == OP_SIGNUP {
                if let Some(account) = snapshot.as_ref() {
                    check_signup_retry(&conn, device, account)?;
                }
            }
            snapshot
        };
        loop {
            let permit = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ERR_THROTTLED)?;
            let mut password = creds.password.as_bytes().to_vec();
            let target = snapshot
                .as_ref()
                .map(|a| a.hash.clone())
                .unwrap_or_else(|| self.dummy.clone());
            let create = op == OP_SIGNUP && snapshot.is_none();
            let (verified, candidate) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let result = if create {
                    hash(&password).map(|h| (false, Some(h)))
                } else {
                    Ok((verify(&password, &target), None))
                };
                password.fill(0);
                result
            })
            .await
            .map_err(|_| ERR_BAD)??;
            if !create && (!verified || snapshot.is_none()) {
                return Err(ERR_CREDENTIALS);
            }
            let mut conn = db.lock().expect("db");
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(store)?;
            let current = lookup(&tx, creds.login)?;
            if create && current.is_some() {
                // A new device sees an occupied login even if the winner used a
                // different password. Only a same-active-key retry can proceed
                // to verify the winner's independently salted hash off-lock.
                check_signup_retry(&tx, device, current.as_ref().expect("checked"))?;
                drop(tx);
                drop(conn);
                snapshot = current;
                continue;
            }
            let outcome = if op == OP_SIGNUP {
                signup(
                    &tx,
                    device,
                    current.as_ref(),
                    creds.login,
                    candidate.as_deref(),
                    optional.as_ref(),
                    now,
                )?
            } else {
                let account = current.as_ref().ok_or(ERR_CREDENTIALS)?;
                if snapshot.as_ref().is_none_or(|s| s.hash != account.hash) {
                    return Err(ERR_CREDENTIALS);
                }
                login(&tx, device, account, optional.as_ref(), now)?
            };
            tx.commit().map_err(store)?;
            return Ok(outcome);
        }
    }
}

fn store(e: rusqlite::Error) -> u8 {
    if matches!(e, rusqlite::Error::SqliteFailure(f,_) if matches!(f.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked))
    {
        ERR_BUSY
    } else {
        ERR_BAD
    }
}
struct Account {
    user: [u8; 16],
    contact: String,
    hash: String,
}
fn lookup(conn: &Connection, login: &str) -> Result<Option<Account>, u8> {
    conn.query_row(
        "SELECT user_id,contact_id,password_hash FROM users WHERE login=?1",
        [login],
        |r| {
            let user: Vec<u8> = r.get(0)?;
            Ok(Account {
                user: user.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,
                contact: r.get(1)?,
                hash: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(store)
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Authenticated {
        user: [u8; 16],
        contact: String,
        revoked: Option<[u8; 32]>,
    },
    ReplaceRequired([u8; 32]),
}
fn done(a: &Account, revoked: Option<[u8; 32]>) -> Outcome {
    Outcome::Authenticated {
        user: a.user,
        contact: a.contact.clone(),
        revoked,
    }
}
fn association(conn: &Connection, device: &[u8; 32]) -> Result<Option<(Vec<u8>, bool)>, u8> {
    conn.query_row(
        "SELECT user_id, revoked<>0 OR blocked<>0 FROM devices WHERE device_key=?1",
        [device.as_slice()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(store)
}
/// An occupied login can only be replayed by its current active device, and
/// the caller must still verify the password before returning account IDs.
/// Shared by the initial snapshot, race-winner snapshot, and final transaction.
fn check_signup_retry(conn: &Connection, device: &[u8; 32], account: &Account) -> Result<(), u8> {
    match association(conn, device)? {
        Some((user, false)) if user == account.user => Ok(()),
        Some((user, _)) if user != account.user => Err(ERR_BOUND_OTHER),
        Some((_, true)) => Err(ERR_REVOKED),
        _ => Err(ERR_CONFLICT),
    }
}
fn insert_device(
    conn: &Connection,
    device: &[u8; 32],
    user: &[u8; 16],
    now: i64,
    highwater: bool,
) -> Result<(), u8> {
    conn.execute(
        "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,?3)",
        rusqlite::params![device.as_slice(), user.as_slice(), now],
    )
    .map_err(store)?;
    conn.execute(
        "INSERT INTO cursors(recipient_user_id,device_key,last_seq) VALUES(?1,?2,?3)",
        rusqlite::params![
            user.as_slice(),
            device.as_slice(),
            if highwater {
                conn.query_row("SELECT COALESCE(MAX(seq),0) FROM mailbox_events", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(store)?
            } else {
                0
            }
        ],
    )
    .map_err(store)?;
    Ok(())
}
fn signup(
    conn: &Connection,
    device: &[u8; 32],
    account: Option<&Account>,
    login: &str,
    candidate: Option<&str>,
    invite: Option<&[u8; 32]>,
    now: i64,
) -> Result<Outcome, u8> {
    if let Some(a) = account {
        check_signup_retry(conn, device, a)?;
        return Ok(done(a, None));
    }
    if let Some((_, revoked)) = association(conn, device)? {
        return Err(if revoked {
            ERR_REVOKED
        } else {
            ERR_BOUND_OTHER
        });
    }
    let required = mode(conn)? == RegistrationMode::InviteOnly;
    // If supplied even in open mode, consume exactly once and validate it.
    if required && invite.is_none() {
        return Err(ERR_INVITE_REQUIRED);
    }
    if let Some(token) = invite {
        let state: Option<(i64, bool, Option<i64>)> = conn
            .query_row(
                "SELECT expires_at,revoked,used_at FROM invites WHERE token=?1",
                [token.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(store)?;
        let (expires, revoked, used) = state.ok_or(ERR_BAD)?;
        if revoked {
            return Err(ERR_REVOKED);
        }
        if used.is_some() {
            return Err(ERR_INVITE_USED);
        }
        if expires <= now {
            return Err(ERR_EXPIRED);
        }
    }
    let mut user = [0; 16];
    getrandom::fill(&mut user).map_err(|_| ERR_BAD)?;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut random = [0; 12];
    getrandom::fill(&mut random).map_err(|_| ERR_BAD)?;
    let contact: String = random
        .iter()
        .map(|b| ALPHABET[usize::from(b & 31)] as char)
        .collect();
    let hash = candidate.ok_or(ERR_BAD)?;
    conn.execute("INSERT INTO users(user_id,contact_id,login,password_hash,created_at) VALUES(?1,?2,?3,?4,?5)", rusqlite::params![user.as_slice(),contact,login,hash,now]).map_err(store)?;
    insert_device(conn, device, &user, now, false)?;
    if let Some(token) = invite {
        conn.execute(
            "UPDATE invites SET used_at=?2 WHERE token=?1 AND used_at IS NULL",
            rusqlite::params![token.as_slice(), now],
        )
        .map_err(store)?;
    }
    Ok(Outcome::Authenticated {
        user,
        contact,
        revoked: None,
    })
}
fn login(
    conn: &Connection,
    device: &[u8; 32],
    account: &Account,
    expected: Option<&[u8; 32]>,
    now: i64,
) -> Result<Outcome, u8> {
    if let Some((user, revoked)) = association(conn, device)? {
        if user != account.user {
            return Err(ERR_BOUND_OTHER);
        }
        return if revoked {
            Err(ERR_REVOKED)
        } else {
            Ok(done(account, None))
        };
    }
    let current: Option<Vec<u8>> = conn
        .query_row(
            "SELECT device_key FROM devices WHERE user_id=?1 AND revoked=0",
            [account.user.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .map_err(store)?;
    let old: [u8; 32] = current
        .ok_or(ERR_REVOKED)?
        .try_into()
        .map_err(|_| ERR_BAD)?;
    if expected != Some(&old) {
        return Ok(Outcome::ReplaceRequired(old));
    }
    let updated = conn
        .execute(
            "UPDATE devices SET revoked=1 WHERE device_key=?1 AND user_id=?2 AND revoked=0",
            rusqlite::params![old.as_slice(), account.user.as_slice()],
        )
        .map_err(store)?;
    if updated != 1 {
        return Err(ERR_CONFLICT);
    }
    conn.execute("DELETE FROM prekeys WHERE device_key=?1", [old.as_slice()])
        .map_err(store)?;
    insert_device(conn, device, &account.user, now, true)?;
    Ok(done(account, Some(old)))
}
pub fn resume(conn: &Connection, device: &[u8; 32]) -> Result<Outcome, u8> {
    conn.query_row("SELECT u.user_id,u.contact_id FROM devices d JOIN users u ON u.user_id=d.user_id WHERE d.device_key=?1 AND d.revoked=0 AND d.blocked=0",[device.as_slice()],|r| {
        let user: Vec<u8> = r.get(0)?;
        Ok(Outcome::Authenticated { user: user.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?, contact:r.get(1)?,revoked:None })
    }).optional().map_err(store)?.ok_or(ERR_REVOKED)
}
pub fn mode(conn: &Connection) -> Result<RegistrationMode, u8> {
    let value: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='registration_mode'",
            [],
            |r| r.get(0),
        )
        .map_err(store)?;
    match value.as_str() {
        "open" => Ok(RegistrationMode::Open),
        "invite_only" => Ok(RegistrationMode::InviteOnly),
        _ => Err(ERR_BAD),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PASSWORD: &str = "correct-fixture-password";
    fn memory() -> Arc<Mutex<Connection>> {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        Arc::new(Mutex::new(c))
    }
    fn counts(db: &Arc<Mutex<Connection>>) -> (i64, i64, i64, i64) {
        db.lock().unwrap().query_row("SELECT (SELECT COUNT(*) FROM users),(SELECT COUNT(*) FROM devices),(SELECT COUNT(*) FROM cursors),(SELECT COUNT(*) FROM invites WHERE used_at IS NOT NULL)",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
    }

    #[test]
    fn argon2id_owasp_cost_random_salts_and_dummy_verification() {
        let a = hash(PASSWORD.as_bytes()).unwrap();
        let b = hash(PASSWORD.as_bytes()).unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify(PASSWORD.as_bytes(), &a));
        assert!(!verify(b"wrong-fixture-password", &a));
        let dummy = hash(b"dummy-credential-unavailable").unwrap();
        assert!(PasswordHash::new(&dummy).is_ok());
        assert!(!verify(PASSWORD.as_bytes(), &dummy));
        assert!(!verify(
            PASSWORD.as_bytes(),
            "$argon2id$v=19$m=4294967295,t=2,p=1$AAAA$AAAA"
        ));
    }

    #[test]
    fn attempt_limits_normalized_login_device_global_memory_and_expiry() {
        let now = Instant::now();
        let mut a = Attempts::default();
        for i in 0..KEY_LIMIT {
            assert!(a.admit("alice", &[i as u8; 32], now));
        }
        assert!(!a.admit("alice", &[99; 32], now));
        let mut a = Attempts::default();
        for i in 0..KEY_LIMIT {
            assert!(a.admit(&format!("user{i}"), &[1; 32], now));
        }
        assert!(!a.admit("other", &[1; 32], now));
        let mut a = Attempts::default();
        for i in 0..GLOBAL_LIMIT {
            assert!(a.admit(&format!("user{i}"), &[i as u8; 32], now));
        }
        assert!(!a.admit("other", &[99; 32], now));
        // Saturated tables fail closed rather than evicting unexpired counters.
        let mut a = Attempts::default();
        for i in 0..KEY_CAP / 2 {
            a.global = Some((now, 0));
            let mut device = [0; 32];
            device[..8].copy_from_slice(&(i as u64).to_be_bytes());
            assert!(a.admit(&format!("user{i}"), &device, now));
        }
        assert_eq!(a.keys.len(), KEY_CAP);
        a.global = Some((now, 0));
        assert!(!a.admit("new-login", &[255; 32], now));
        assert_eq!(a.keys.len(), KEY_CAP);
        assert!(a.admit("new-login", &[255; 32], now + WINDOW));
        assert_eq!(a.keys.len(), 2);
    }

    #[tokio::test]
    async fn hash_slots_fail_before_work_and_db_mutex_is_not_held() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        let p = auth::build_login("missing", PASSWORD, None).unwrap();
        let slot1 = engine.slots.clone().acquire_owned().await.unwrap();
        let slot2 = engine.slots.clone().acquire_owned().await.unwrap();
        assert_eq!(
            engine.account(&db, &[1; 32], OP_LOGIN, &p, 1).await,
            Err(ERR_THROTTLED)
        );
        drop((slot1, slot2));
        let work = engine.account(&db, &[2; 32], OP_LOGIN, &p, 1);
        tokio::pin!(work);
        tokio::select! {
            _ = &mut work => panic!("Argon verification unexpectedly completed before yielding"),
            _ = tokio::task::yield_now() => {}
        }
        // An unrelated DB operation remains available while Argon runs.
        assert!(db.try_lock().is_ok());
        assert!(engine.slots.available_permits() < HASH_SLOTS);
        assert_eq!(work.await, Err(ERR_CREDENTIALS));
        assert_eq!(engine.slots.available_permits(), HASH_SLOTS);
    }

    #[tokio::test]
    async fn cancelled_hash_retains_slot_until_worker_finishes() {
        let engine = Arc::new(Engine::new().await.unwrap());
        let db = memory();
        let e = engine.clone();
        let d = db.clone();
        let task = tokio::spawn(async move {
            e.account(
                &d,
                &[3; 32],
                OP_LOGIN,
                &auth::build_login("missing", PASSWORD, None).unwrap(),
                1,
            )
            .await
        });
        while engine.slots.available_permits() == HASH_SLOTS {
            tokio::task::yield_now().await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(engine.slots.available_permits() < HASH_SLOTS);
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.slots.available_permits() < HASH_SLOTS {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn signup_failures_roll_back_users_devices_cursor_and_invite() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        let token = [4u8; 32];
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO invites(token,created_at,expires_at) VALUES(?1,1,100)",
                [token.as_slice()],
            )
            .unwrap();
        let payload = auth::build_signup("alice", PASSWORD, Some(&token)).unwrap();
        for trigger in [
            "CREATE TRIGGER fail BEFORE INSERT ON cursors BEGIN SELECT RAISE(ABORT,'fixture failure'); END;",
            "CREATE TRIGGER fail BEFORE UPDATE OF used_at ON invites BEGIN SELECT RAISE(ABORT,'fixture failure'); END;",
        ] {
            db.lock().unwrap().execute_batch(trigger).unwrap();
            assert_eq!(engine.account(&db,&[5;32],OP_SIGNUP,&payload,2).await,Err(ERR_BAD));
            assert_eq!(counts(&db),(0,0,0,0)); db.lock().unwrap().execute_batch("DROP TRIGGER fail").unwrap();
        }
        assert!(matches!(
            engine.account(&db, &[5; 32], OP_SIGNUP, &payload, 2).await,
            Ok(Outcome::Authenticated { .. })
        ));
        assert_eq!(counts(&db), (1, 1, 1, 1));
    }

    #[tokio::test]
    async fn replacement_failed_cursor_insert_rolls_back_revoke_and_prekey_delete() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        db.lock()
            .unwrap()
            .execute(
                "UPDATE meta SET value='open' WHERE key='registration_mode'",
                [],
            )
            .unwrap();
        let original = engine
            .account(
                &db,
                &[6; 32],
                OP_SIGNUP,
                &auth::build_signup("alice", PASSWORD, None).unwrap(),
                2,
            )
            .await
            .unwrap();
        let c = db.lock().unwrap();
        c.execute(
            "INSERT INTO prekeys VALUES(?1,1,?2,?3,1,0)",
            rusqlite::params![
                [6u8; 32].as_slice(),
                [1u8; 32].as_slice(),
                [1u8; 64].as_slice()
            ],
        )
        .unwrap();
        c.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON cursors BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        drop(c);
        let payload = auth::build_login("alice", PASSWORD, Some(&[6; 32])).unwrap();
        assert_eq!(
            engine.account(&db, &[7; 32], OP_LOGIN, &payload, 3).await,
            Err(ERR_BAD)
        );
        assert_eq!(counts(&db), (1, 1, 1, 0));
        let c = db.lock().unwrap();
        assert_eq!(resume(&c, &[6; 32]).unwrap(), original);
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM prekeys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        c.execute_batch("DROP TRIGGER fail").unwrap();
        drop(c);
        assert!(
            matches!(engine.account(&db,&[7;32],OP_LOGIN,&payload,3).await,Ok(Outcome::Authenticated{revoked:Some(key),..}) if key == [6;32])
        );
    }

    #[tokio::test]
    async fn raced_signup_new_devices_conflict_regardless_of_password() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        db.lock()
            .unwrap()
            .execute(
                "UPDATE meta SET value='open' WHERE key='registration_mode'",
                [],
            )
            .unwrap();
        let a = auth::build_signup("alice", PASSWORD, None).unwrap();
        let b = auth::build_signup("alice", "different-fixture-password", None).unwrap();
        // Both futures take their absent-account snapshots before either hash
        // finishes. The race loser must use the same rule as an initial lookup.
        let (a, b) = tokio::join!(
            engine.account(&db, &[9; 32], OP_SIGNUP, &a, 1),
            engine.account(&db, &[10; 32], OP_SIGNUP, &b, 1)
        );
        assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1);
        assert_eq!(if a.is_err() { a } else { b }, Err(ERR_CONFLICT));
        assert_eq!(counts(&db), (1, 1, 1, 0));
    }

    #[tokio::test]
    async fn raced_signup_must_verify_winners_password_not_just_reuse_key() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        db.lock()
            .unwrap()
            .execute(
                "UPDATE meta SET value='open' WHERE key='registration_mode'",
                [],
            )
            .unwrap();
        let a = auth::build_signup("alice", PASSWORD, None).unwrap();
        let b = auth::build_signup("alice", "different-fixture-password", None).unwrap();
        let (a, b) = tokio::join!(
            engine.account(&db, &[8; 32], OP_SIGNUP, &a, 1),
            engine.account(&db, &[8; 32], OP_SIGNUP, &b, 1)
        );
        assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1);
        assert_eq!(if a.is_err() { a } else { b }, Err(ERR_CREDENTIALS));
        assert_eq!(counts(&db), (1, 1, 1, 0));
    }
}

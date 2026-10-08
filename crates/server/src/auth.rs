//! Account auth: bounded Argon2id work outside the DB mutex; atomic signup/CAS.
use argon2::{
    password_hash::{PasswordHash, SaltString},
    Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version,
};
use dmsg_protocol::invitation::{self as ip, Issued, Metadata, State as InviteState};
use dmsg_protocol::{
    auth::{self, RegistrationMode},
    *,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

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
    issuance: Mutex<Issuance>,
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
            issuance: Mutex::new(Issuance::default()),
            dummy,
        })
    }

    /// Synchronous management under DB -> limiter locks. Secrets are guarded
    /// from DB materialization through response encoding; no await under locks.
    pub fn invitation(
        &self,
        db: &Arc<Mutex<Connection>>,
        device: &[u8; 32],
        user: &[u8; 16],
        op: u8,
        payload: &[u8],
    ) -> Result<(u8, Zeroizing<Vec<u8>>), u8> {
        let id = match op {
            OP_INVITE_ISSUE | OP_INVITE_REVOKE => {
                Some(ip::parse_issue_id(payload).map_err(|_| ERR_INVALID_INPUT)?)
            }
            OP_INVITE_LIST if payload.is_empty() => None,
            _ => return Err(ERR_INVALID_INPUT),
        };
        let mut conn = db.lock().expect("db");
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store)?;
        // Take the management wall-clock snapshot only after entering the DB
        // operation. Existing account/Argon timestamp behavior stays separate.
        let now = crate::now_secs();
        check_invitation_owner(&tx, device, user)?;
        match op {
            OP_INVITE_ISSUE => {
                let id = id.expect("parsed");
                if let Some(existing) = owned_invitation(&tx, user, &id, now)? {
                    let response =
                        Zeroizing::new(ip::build_issued(&existing).map_err(|_| ERR_BAD)?);
                    tx.commit().map_err(store)?;
                    return Ok((OP_INVITE_ISSUED, response));
                }
                let active: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM invites WHERE owner_user_id=?1 AND used_at IS NULL AND revoked=0 AND expires_at>?2",
                    rusqlite::params![user.as_slice(), now], |r| r.get(0)).map_err(store)?;
                if active >= ip::MAX_ACTIVE as i64 {
                    return Err(ERR_INVITE_LIMIT);
                }
                let mut limiter = self.issuance.lock().expect("issuance");
                if !limiter.available(user, Instant::now()) {
                    return Err(ERR_THROTTLED);
                }
                let expires_at = now.checked_add(ip::TTL_SECONDS).ok_or(ERR_BAD)?;
                // A token collision is an ordinary single-PK retry, never a
                // second namespace/alias. Regenerate only for that collision.
                let (phrase, token) = loop {
                    let phrase = generate_phrase()?;
                    let token =
                        Zeroizing::new(ip::parse_invitation_input(&phrase).map_err(|_| ERR_BAD)?);
                    let collision: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM invites WHERE token=?1)",
                            [token.as_slice()],
                            |r| r.get(0),
                        )
                        .map_err(store)?;
                    if !collision {
                        break (phrase, token);
                    }
                };
                let issued = Zeroizing::new(Issued {
                    server_now: now,
                    invitation: Metadata {
                        issue_id: id,
                        created_at: now,
                        expires_at,
                    },
                    state: InviteState::Active,
                    phrase: Some(phrase.to_string()),
                });
                let response = Zeroizing::new(ip::build_issued(&issued).map_err(|_| ERR_BAD)?);
                tx.execute("INSERT INTO invites(token,created_at,expires_at,owner_user_id,issue_id,phrase_ascii) VALUES(?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![token.as_slice(), now, expires_at, user.as_slice(), id.as_slice(), issued.phrase.as_deref().ok_or(ERR_BAD)?]).map_err(store)?;
                tx.commit().map_err(store)?;
                // Only a durable new insertion spends budget. Retries, quotas,
                // randomness/storage failures and rolled-back commits are free.
                limiter.charge(*user, Instant::now());
                Ok((OP_INVITE_ISSUED, response))
            }
            OP_INVITE_REVOKE => {
                let id = id.expect("parsed");
                let existing = owned_invitation(&tx, user, &id, now)?.ok_or(ERR_BAD)?;
                if existing.state == InviteState::Active {
                    tx.execute(
                        "UPDATE invites SET revoked=1 WHERE owner_user_id=?1 AND issue_id=?2",
                        rusqlite::params![user.as_slice(), id.as_slice()],
                    )
                    .map_err(store)?;
                }
                tx.commit().map_err(store)?;
                Ok((OP_INVITE_REVOKED, Zeroizing::new(Vec::new())))
            }
            OP_INVITE_LIST => {
                let mut statement = tx.prepare("SELECT issue_id,created_at,expires_at FROM invites WHERE owner_user_id=?1 AND used_at IS NULL AND revoked=0 AND expires_at>?2 ORDER BY created_at,issue_id").map_err(store)?;
                let rows = statement
                    .query_map(rusqlite::params![user.as_slice(), now], |r| {
                        let id: Vec<u8> = r.get(0)?;
                        Ok(Metadata {
                            issue_id: id.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,
                            created_at: r.get(1)?,
                            expires_at: r.get(2)?,
                        })
                    })
                    .map_err(store)?;
                let mut invitations = Vec::new();
                for row in rows {
                    invitations.push(row.map_err(store)?);
                    // Detect inconsistent DB state; never silently truncate.
                    if invitations.len() > ip::MAX_ACTIVE {
                        return Err(ERR_BAD);
                    }
                }
                drop(statement);
                let response = ip::build_list(&ip::List {
                    server_now: now,
                    invitations,
                })
                .map_err(|_| ERR_BAD)?;
                tx.commit().map_err(store)?;
                Ok((OP_INVITE_LISTED, Zeroizing::new(response)))
            }
            _ => unreachable!("validated"),
        }
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

/// A rolling window of successful commits is also the entire owner tracking
/// state: bounded by 32 global charges, without an independent owner registry.
#[derive(Default)]
struct Issuance {
    commits: VecDeque<(Instant, [u8; 16])>,
}
impl Issuance {
    fn available(&mut self, owner: &[u8; 16], now: Instant) -> bool {
        while self
            .commits
            .front()
            .is_some_and(|(start, _)| now.duration_since(*start) >= WINDOW)
        {
            self.commits.pop_front();
        }
        self.commits.len() < GLOBAL_LIMIT as usize
            && self
                .commits
                .iter()
                .filter(|(_, user)| user == owner)
                .count()
                < KEY_LIMIT as usize
    }
    fn charge(&mut self, owner: [u8; 16], now: Instant) {
        self.commits.push_back((now, owner));
        debug_assert!(self.commits.len() <= GLOBAL_LIMIT as usize);
    }
}

fn check_invitation_owner(conn: &Connection, device: &[u8; 32], user: &[u8; 16]) -> Result<(), u8> {
    let valid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM devices d JOIN users u ON u.user_id=d.user_id WHERE d.device_key=?1 AND d.user_id=?2 AND d.revoked=0 AND d.blocked=0)",
        rusqlite::params![device.as_slice(), user.as_slice()], |r| r.get(0)).map_err(store)?;
    if valid {
        Ok(())
    } else {
        Err(ERR_REVOKED)
    }
}

fn owned_invitation(
    conn: &Connection,
    owner: &[u8; 16],
    id: &[u8; 16],
    now: i64,
) -> Result<Option<Zeroizing<Issued>>, u8> {
    conn.query_row("SELECT created_at,expires_at,used_at,revoked,phrase_ascii FROM invites WHERE owner_user_id=?1 AND issue_id=?2",
        rusqlite::params![owner.as_slice(), id.as_slice()], |r| {
            let created_at = r.get(0)?;
            let expires_at = r.get(1)?;
            let used: Option<i64> = r.get(2)?;
            let revoked: bool = r.get(3)?;
            let state = if used.is_some() { InviteState::Used } else if revoked { InviteState::Revoked }
                else if expires_at <= now { InviteState::Expired } else { InviteState::Active };
            // Terminal responses do not materialize a secret from SQLite.
            let phrase = if state == InviteState::Active { Some(r.get(4)?) } else { None };
            Ok(Zeroizing::new(Issued { server_now: now, invitation: Metadata {issue_id:*id,created_at,expires_at}, state, phrase }))
        }).optional().map_err(store)
}

fn generate_phrase() -> Result<Zeroizing<String>, u8> {
    let mut phrase = Zeroizing::new(String::with_capacity(59));
    // Rejection sampling: 8 complete 7776-sized buckets fit in u16. No
    // modulo bias, no deduplication; each of the six words is independent.
    let bound = (65536 / ip::WORD_COUNT) * ip::WORD_COUNT;
    for i in 0..6 {
        let index = loop {
            let mut bytes = [0; 2];
            getrandom::fill(&mut bytes).map_err(|_| ERR_BAD)?;
            let n = usize::from(u16::from_be_bytes(bytes));
            if n < bound {
                break n % ip::WORD_COUNT;
            }
        };
        if i != 0 {
            phrase.push(' ');
        }
        phrase.push_str(ip::word(index).ok_or(ERR_BAD)?);
    }
    Ok(phrase)
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

    fn owner_fixture(db: &Arc<Mutex<Connection>>, n: u8) -> ([u8; 16], [u8; 32]) {
        let user = [n; 16];
        let device = [n; 32];
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO users VALUES(?1,?2,?3,'$argon2id$fixture',1)",
            rusqlite::params![user.as_slice(), format!("{n:012}"), format!("owner{n}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
            rusqlite::params![device.as_slice(), user.as_slice()],
        )
        .unwrap();
        (user, device)
    }

    fn issue(
        engine: &Engine,
        db: &Arc<Mutex<Connection>>,
        user: &[u8; 16],
        device: &[u8; 32],
        id: &[u8; 16],
    ) -> Issued {
        let (op, bytes) = engine
            .invitation(db, device, user, OP_INVITE_ISSUE, id)
            .unwrap();
        assert_eq!(op, OP_INVITE_ISSUED);
        ip::parse_issued(&bytes).unwrap()
    }

    #[tokio::test]
    async fn invitation_commit_failure_is_free_and_rollback_preserves_same_id() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        let (user, device) = owner_fixture(&db, 1);
        // Deferred FK violation specifically fails COMMIT, after successful INSERT.
        db.lock().unwrap().execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE fail_commit(owner BLOB REFERENCES users(user_id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail AFTER INSERT ON invites BEGIN INSERT INTO fail_commit VALUES(zeroblob(16)); END;").unwrap();
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_ISSUE, &[1; 16])
                .err()
                .expect("invitation rejected"),
            ERR_BAD
        );
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM invites", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(engine.issuance.lock().unwrap().commits.is_empty());
        db.lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail")
            .unwrap();
        let original = issue(&engine, &db, &user, &device, &[1; 16]);
        for _ in 0..20 {
            let retry = issue(&engine, &db, &user, &device, &[1; 16]);
            assert_eq!(retry.invitation, original.invitation);
            assert!(retry.phrase == original.phrase);
        }
        assert_eq!(engine.issuance.lock().unwrap().commits.len(), 1);
    }

    #[tokio::test]
    async fn invitation_owner_scoping_device_fence_and_terminal_priority() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        let (user, device) = owner_fixture(&db, 1);
        let (other, other_device) = owner_fixture(&db, 2);
        let original = issue(&engine, &db, &user, &device, &[3; 16]);
        assert_eq!(
            engine
                .invitation(&db, &other_device, &other, OP_INVITE_REVOKE, &[3; 16])
                .err()
                .expect("invitation rejected"),
            ERR_BAD
        );
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_REVOKE, &[4; 16])
                .err()
                .expect("invitation rejected"),
            ERR_BAD
        );
        assert_eq!(
            engine
                .invitation(&db, &device, &other, OP_INVITE_LIST, &[])
                .err()
                .expect("invitation rejected"),
            ERR_REVOKED
        );
        // Same IDs live in different owner scopes and mint independent bearers.
        let foreign = issue(&engine, &db, &other, &other_device, &[3; 16]);
        assert!(foreign.phrase != original.phrase);
        for flag in ["blocked", "revoked"] {
            db.lock()
                .unwrap()
                .execute(
                    &format!("UPDATE devices SET {flag}=1 WHERE device_key=?1"),
                    [device.as_slice()],
                )
                .unwrap();
            for (op, payload) in [
                (OP_INVITE_ISSUE, &[3; 16][..]),
                (OP_INVITE_REVOKE, &[3; 16][..]),
                (OP_INVITE_LIST, &[][..]),
            ] {
                assert_eq!(
                    engine
                        .invitation(&db, &device, &user, op, payload)
                        .err()
                        .expect("invitation rejected"),
                    ERR_REVOKED
                );
            }
            db.lock()
                .unwrap()
                .execute(
                    &format!("UPDATE devices SET {flag}=0 WHERE device_key=?1"),
                    [device.as_slice()],
                )
                .unwrap();
        }
        let now = crate::now_secs();
        db.lock().unwrap().execute("UPDATE invites SET created_at=?1,expires_at=?2,revoked=1,used_at=?2 WHERE owner_user_id=?3",rusqlite::params![now-ip::TTL_SECONDS,now,user.as_slice()]).unwrap();
        let used = issue(&engine, &db, &user, &device, &[3; 16]);
        assert_eq!(used.state, InviteState::Used);
        assert!(used.phrase.is_none());
        engine
            .invitation(&db, &device, &user, OP_INVITE_REVOKE, &[3; 16])
            .unwrap();
        db.lock()
            .unwrap()
            .execute(
                "UPDATE invites SET used_at=NULL WHERE owner_user_id=?1",
                [user.as_slice()],
            )
            .unwrap();
        assert_eq!(
            issue(&engine, &db, &user, &device, &[3; 16]).state,
            InviteState::Revoked
        );
        db.lock()
            .unwrap()
            .execute(
                "UPDATE invites SET revoked=0 WHERE owner_user_id=?1",
                [user.as_slice()],
            )
            .unwrap();
        assert_eq!(
            issue(&engine, &db, &user, &device, &[3; 16]).state,
            InviteState::Expired
        );
        engine
            .invitation(&db, &device, &user, OP_INVITE_REVOKE, &[3; 16])
            .unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row(
                    "SELECT revoked FROM invites WHERE owner_user_id=?1",
                    [user.as_slice()],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(engine.issuance.lock().unwrap().commits.len(), 2);
    }

    #[tokio::test]
    async fn invitation_active_quota_retries_before_limits_and_rolling_commit_budgets() {
        let engine = Engine::new().await.unwrap();
        let db = memory();
        let (user, device) = owner_fixture(&db, 1);
        for i in 0..8 {
            issue(&engine, &db, &user, &device, &[i; 16]);
        }
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_ISSUE, &[8; 16])
                .err()
                .expect("invitation rejected"),
            ERR_INVITE_LIMIT
        );
        assert_eq!(
            issue(&engine, &db, &user, &device, &[0; 16]).state,
            InviteState::Active
        );
        engine
            .invitation(&db, &device, &user, OP_INVITE_REVOKE, &[0; 16])
            .unwrap();
        assert_eq!(
            issue(&engine, &db, &user, &device, &[0; 16]).state,
            InviteState::Revoked
        );
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_ISSUE, &[8; 16])
                .err()
                .expect("invitation rejected"),
            ERR_THROTTLED
        );
        assert_eq!(engine.issuance.lock().unwrap().commits.len(), 8);
        // Other owners can commit up to the global 32 budget; quota failures
        // above did not consume the remaining 24 charges.
        for n in 2..=4 {
            let (u, d) = owner_fixture(&db, n);
            for i in 0..8 {
                issue(&engine, &db, &u, &d, &[i; 16]);
            }
        }
        let (u, d) = owner_fixture(&db, 5);
        assert_eq!(
            engine
                .invitation(&db, &d, &u, OP_INVITE_ISSUE, &[0; 16])
                .err()
                .expect("invitation rejected"),
            ERR_THROTTLED
        );
        assert_eq!(engine.issuance.lock().unwrap().commits.len(), 32);
        for entry in &mut engine.issuance.lock().unwrap().commits {
            entry.0 -= WINDOW;
        }
        issue(&engine, &db, &user, &device, &[8; 16]);
        assert_eq!(engine.issuance.lock().unwrap().commits.len(), 1);
        let (_, bytes) = engine
            .invitation(&db, &device, &user, OP_INVITE_LIST, &[])
            .unwrap();
        let list = ip::parse_list(&bytes).unwrap();
        assert_eq!(list.invitations.len(), 8);
        assert!(list
            .invitations
            .windows(2)
            .all(|w| (w[0].created_at, w[0].issue_id) <= (w[1].created_at, w[1].issue_id)));
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_LIST, &[0])
                .err()
                .expect("invitation rejected"),
            ERR_INVALID_INPUT
        );
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_ISSUE, &[0; 15])
                .err()
                .expect("invitation rejected"),
            ERR_INVALID_INPUT
        );
        // Corrupt/inconsistent active count fails closed instead of truncating LIST.
        let phrase = "abacus abacus abacus abacus abacus abacus";
        let now = crate::now_secs();
        db.lock().unwrap().execute("INSERT INTO invites(token,created_at,expires_at,owner_user_id,issue_id,phrase_ascii) VALUES(?1,?2,?3,?4,?5,?6)",rusqlite::params![[99u8;32].as_slice(),now,now+ip::TTL_SECONDS,user.as_slice(),[99u8;16].as_slice(),phrase]).unwrap();
        assert_eq!(
            engine
                .invitation(&db, &device, &user, OP_INVITE_LIST, &[])
                .err()
                .expect("invitation rejected"),
            ERR_BAD
        );
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

//! Unified account auth over a pinned, completed Noise channel. No credentials
//! are stored. A pending device key is durable before any signup/login request.
use crate::transport::{Transport, TransportError};
use dmsg_protocol::{auth as wire, profile, *};
use rusqlite::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub user_id: [u8; 16],
    pub contact_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    Authenticated(Account),
    ReplacementRequired([u8; 32]),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub domain: String,
    pub pin_fingerprint: [u8; 32],
}
impl Preview {
    pub fn pin_fingerprint_hex(&self) -> String {
        self.pin_fingerprint
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
pub fn pin_fingerprint(der: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(der).into()
}
pub fn preview(code: &str) -> Result<Preview, AuthError> {
    let p = profile::parse(code).map_err(|_| AuthError::BadQr)?;
    Ok(Preview {
        domain: String::from_utf8(p.domain).map_err(|_| AuthError::BadQr)?,
        pin_fingerprint: pin_fingerprint(&p.cert_der),
    })
}
#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    BadQr,
    PinMismatch,
    InvalidInput,
    InvalidCredentials,
    LoginTaken,
    InviteRequired,
    InviteExpired,
    InviteRevoked,
    InviteUsed,
    InviteLimit,
    AuthRateLimited,
    Revoked,
    AlreadyAuthenticated,
    Busy,
    Server(u8),
    Protocol(&'static str),
    Transport(String),
    Store(String),
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::BadQr => "invalid public server profile",
            Self::PinMismatch => "transport pin mismatch",
            Self::InvalidInput => "invalid auth input",
            Self::InvalidCredentials => "invalid login or password",
            Self::LoginTaken => "login taken",
            Self::InviteRequired => "invitation required",
            Self::InviteExpired => "invitation expired",
            Self::InviteRevoked => "invitation revoked",
            Self::InviteUsed => "invitation used",
            Self::InviteLimit => "active invitation limit reached",
            Self::AuthRateLimited => "auth rate limited",
            Self::Revoked => "device revoked",
            Self::AlreadyAuthenticated => "account already authenticated",
            Self::Busy => "server busy",
            Self::Server(_) => "server error",
            Self::Protocol(_) => "invalid auth response",
            Self::Transport(_) => "auth transport failed",
            Self::Store(_) => "auth storage failed",
        };
        f.write_str(text)
    }
}
impl std::error::Error for AuthError {}
pub fn map_error(code: u8, signup: bool) -> AuthError {
    match code {
        ERR_CREDENTIALS => AuthError::InvalidCredentials,
        ERR_CONFLICT if signup => AuthError::LoginTaken,
        ERR_CONFLICT | ERR_INVALID_INPUT | ERR_BAD => AuthError::InvalidInput,
        ERR_INVITE_REQUIRED => AuthError::InviteRequired,
        ERR_EXPIRED => AuthError::InviteExpired,
        ERR_REVOKED if signup => AuthError::InviteRevoked,
        ERR_REVOKED => AuthError::Revoked,
        ERR_INVITE_USED => AuthError::InviteUsed,
        ERR_THROTTLED => AuthError::AuthRateLimited,
        ERR_BUSY => AuthError::Busy,
        other => AuthError::Server(other),
    }
}
pub fn generate_key() -> Result<[u8; 32], AuthError> {
    let mut key = [0; 32];
    getrandom::fill(&mut key).map_err(|_| AuthError::Store("key generation failed".into()))?;
    Ok(key)
}
/// IMMEDIATE serializes concurrent attempts before choosing the pending key.
pub fn pending_key(conn: &mut Connection) -> Result<[u8; 32], AuthError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| AuthError::Store("auth transaction".into()))?;
    if crate::store::load_account(&tx)
        .map_err(AuthError::Store)?
        .is_some()
    {
        return Err(AuthError::AlreadyAuthenticated);
    }
    let key = match crate::store::load_identity(&tx).map_err(AuthError::Store)? {
        Some(k) => k,
        None => {
            let k = generate_key()?;
            crate::store::save_identity(&tx, &k).map_err(AuthError::Store)?;
            k
        }
    };
    tx.commit()
        .map_err(|_| AuthError::Store("pending key commit".into()))?;
    Ok(key)
}
pub async fn registration_policy(
    t: &mut impl Transport,
) -> Result<wire::RegistrationMode, AuthError> {
    t.send_frame(OP_POLICY, &[]).await.map_err(net)?;
    let (op, p) = t.recv_frame().await.map_err(net)?;
    if op == OP_POLICY_RESP {
        return wire::RegistrationMode::parse(&p).ok_or(AuthError::Protocol("bad policy"));
    }
    if op == OP_ERROR && p.len() == 1 {
        return Err(map_error(p[0], false));
    }
    Err(AuthError::Protocol("unexpected policy response"))
}
fn net(e: TransportError) -> AuthError {
    AuthError::Transport(e.to_string())
}
/// Account acceptance and key comparison share one transaction; foreign replies
/// and a second caller cannot overwrite an already accepted account.
pub async fn authenticate(
    conn: &mut Connection,
    t: &mut impl Transport,
    key: &[u8; 32],
    opcode: u8,
    payload: &[u8],
) -> Result<LoginOutcome, AuthError> {
    if crate::store::load_account(conn)
        .map_err(AuthError::Store)?
        .is_some()
    {
        return Err(AuthError::AlreadyAuthenticated);
    }
    t.send_frame(opcode, payload).await.map_err(net)?;
    let (op, p) = t.recv_frame().await.map_err(net)?;
    if op == OP_REPLACE_REQUIRED && opcode == OP_LOGIN {
        return Ok(LoginOutcome::ReplacementRequired(
            p.as_slice()
                .try_into()
                .map_err(|_| AuthError::Protocol("bad replacement response"))?,
        ));
    }
    if op == OP_ERROR && p.len() == 1 {
        return Err(map_error(p[0], opcode == OP_SIGNUP));
    }
    if op != OP_AUTHENTICATED {
        return Err(AuthError::Protocol("unexpected auth response"));
    }
    let accepted = wire::parse_authenticated(&p)
        .map_err(|_| AuthError::Protocol("bad authenticated response"))?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| AuthError::Store("accept transaction".into()))?;
    if crate::store::load_identity(&tx).map_err(AuthError::Store)? != Some(*key) {
        return Err(AuthError::Protocol("pending identity changed"));
    }
    crate::store::save_account(&tx, &accepted.user_id, &accepted.contact_id)
        .map_err(AuthError::Store)?;
    tx.commit()
        .map_err(|_| AuthError::Store("accept commit".into()))?;
    Ok(LoginOutcome::Authenticated(Account {
        user_id: accepted.user_id,
        contact_id: accepted.contact_id,
    }))
}
/// Narrow direct-network seam for isolated live-server harnesses. The public
/// profile supplies both trust anchors; there is no key discovery or TOFU.
pub async fn signup_direct(
    code: &str,
    addr: &str,
    db: &std::path::Path,
    pin: Option<&[u8]>,
    storage_key: Option<&[u8]>,
    login: &str,
    password: &str,
    invitation: Option<&str>,
) -> Result<Account, AuthError> {
    let invite = invitation
        .map(dmsg_protocol::invitation::parse_invitation_input)
        .transpose()
        .map_err(|_| AuthError::InvalidInput)?;
    let invite = invite.map(zeroize::Zeroizing::new);
    let payload = zeroize::Zeroizing::new(
        wire::build_signup(login, password, invite.as_deref())
            .map_err(|_| AuthError::InvalidInput)?,
    );
    match direct(code, addr, db, pin, storage_key, OP_SIGNUP, &payload).await? {
        LoginOutcome::Authenticated(a) => Ok(a),
        _ => Err(AuthError::Protocol("signup replacement response")),
    }
}
pub async fn login_direct(
    code: &str,
    addr: &str,
    db: &std::path::Path,
    pin: Option<&[u8]>,
    storage_key: Option<&[u8]>,
    login: &str,
    password: &str,
    expected: Option<&[u8; 32]>,
) -> Result<LoginOutcome, AuthError> {
    let payload =
        wire::build_login(login, password, expected).map_err(|_| AuthError::InvalidInput)?;
    direct(code, addr, db, pin, storage_key, OP_LOGIN, &payload).await
}
async fn direct(
    code: &str,
    addr: &str,
    db: &std::path::Path,
    pin: Option<&[u8]>,
    storage_key: Option<&[u8]>,
    opcode: u8,
    payload: &[u8],
) -> Result<LoginOutcome, AuthError> {
    let p = profile::parse(code).map_err(|_| AuthError::BadQr)?;
    if pin.is_some_and(|pin| pin != p.cert_der) {
        return Err(AuthError::PinMismatch);
    }
    let mut conn = match storage_key {
        Some(k) => crate::store::open_encrypted(db, k),
        None => crate::store::open(db),
    }
    .map_err(AuthError::Store)?;
    // Pins are immutable, including the direct harness seam.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| AuthError::Store("profile transaction".into()))?;
    let profile = crate::dns::Profile::from_qr(code, vec!["127.0.0.1:53".into()])
        .map_err(|_| AuthError::BadQr)?;
    if let Some(old) = crate::dns::load(&tx).map_err(AuthError::Store)? {
        if old.domain != profile.domain
            || old.certificate != profile.certificate
            || old.noise_pubkey != profile.noise_pubkey
        {
            return Err(AuthError::PinMismatch);
        }
    }
    crate::dns::save(&tx, &profile).map_err(AuthError::Store)?;
    tx.commit()
        .map_err(|_| AuthError::Store("profile commit".into()))?;
    let key = pending_key(&mut conn)?;
    let mut t = crate::transport::initiate_with_key(addr, &p.noise_pubkey, &p.domain, &key)
        .await
        .map_err(net)?;
    authenticate(&mut conn, &mut t, &key, opcode, payload).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, path::PathBuf};
    struct Reply {
        queue: VecDeque<Result<(u8, Vec<u8>), TransportError>>,
        sent: Vec<u8>,
    }
    impl Transport for Reply {
        async fn connect(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
        async fn send_frame(&mut self, op: u8, _: &[u8]) -> Result<(), TransportError> {
            self.sent.push(op);
            Ok(())
        }
        async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
            self.queue
                .pop_front()
                .unwrap_or(Err(TransportError::Closed))
        }
        async fn close(&mut self) {}
        fn is_connected(&self) -> bool {
            true
        }
    }
    fn path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dmsg-auth-unit-{}-{tag}.db", std::process::id()))
    }
    #[tokio::test]
    async fn pending_key_survives_lost_response_and_acceptance_is_immutable() {
        let p = path("lost");
        let _ = std::fs::remove_file(&p);
        let mut c = crate::store::open(&p).unwrap();
        let key = pending_key(&mut c).unwrap();
        let payload = wire::build_signup("alice", "a secret password", None).unwrap();
        let mut lost = Reply {
            queue: VecDeque::new(),
            sent: vec![],
        };
        assert!(matches!(
            authenticate(&mut c, &mut lost, &key, OP_SIGNUP, &payload).await,
            Err(AuthError::Transport(_))
        ));
        assert_eq!(crate::store::load_account(&c).unwrap(), None);
        drop(c);
        let mut c = crate::store::open(&p).unwrap();
        assert_eq!(pending_key(&mut c).unwrap(), key);
        let response = wire::build_authenticated(&[9; 16], "ABCD1234EFGH").unwrap();
        let mut ok = Reply {
            queue: VecDeque::from([Ok((OP_AUTHENTICATED, response))]),
            sent: vec![],
        };
        assert_eq!(
            authenticate(&mut c, &mut ok, &key, OP_SIGNUP, &payload)
                .await
                .unwrap(),
            LoginOutcome::Authenticated(Account {
                user_id: [9; 16],
                contact_id: "ABCD1234EFGH".into()
            })
        );
        assert_eq!(pending_key(&mut c), Err(AuthError::AlreadyAuthenticated));
        let mut foreign = Reply {
            queue: VecDeque::from([Ok((
                OP_AUTHENTICATED,
                wire::build_authenticated(&[8; 16], "ZXCV1234ASDF").unwrap(),
            ))]),
            sent: vec![],
        };
        assert_eq!(
            authenticate(&mut c, &mut foreign, &key, OP_LOGIN, &payload).await,
            Err(AuthError::AlreadyAuthenticated)
        );
        assert!(foreign.sent.is_empty());
        assert_eq!(crate::store::load_account(&c).unwrap().unwrap().0, [9; 16]);
        let names: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!names
            .iter()
            .any(|s| s.contains("token") || s.contains("credential")));
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(!raw.windows(17).any(|w| w == b"a secret password"));
        drop(c);
        let _ = std::fs::remove_file(p);
    }
    #[tokio::test]
    async fn foreign_key_and_malformed_account_reply_cannot_be_accepted() {
        let p = path("foreign");
        let _ = std::fs::remove_file(&p);
        let mut c = crate::store::open(&p).unwrap();
        let key = pending_key(&mut c).unwrap();
        for (wrong, reply) in [
            (
                [8; 32],
                wire::build_authenticated(&[7; 16], "ABCD1234EFGH").unwrap(),
            ),
            (key, vec![0; 28]),
        ] {
            let mut t = Reply {
                queue: VecDeque::from([Ok((OP_AUTHENTICATED, reply))]),
                sent: vec![],
            };
            assert!(matches!(
                authenticate(&mut c, &mut t, &wrong, OP_SIGNUP, &[]).await,
                Err(AuthError::Protocol(_))
            ));
            assert_eq!(crate::store::load_account(&c).unwrap(), None);
            assert_eq!(pending_key(&mut c).unwrap(), key);
        }
        drop(c);
        let _ = std::fs::remove_file(p);
    }
    #[tokio::test]
    async fn replacement_response_is_nonmutating_and_strict() {
        let p = path("replace");
        let _ = std::fs::remove_file(&p);
        let mut c = crate::store::open(&p).unwrap();
        let key = pending_key(&mut c).unwrap();
        let mut t = Reply {
            queue: VecDeque::from([Ok((OP_REPLACE_REQUIRED, vec![7; 32]))]),
            sent: vec![],
        };
        assert_eq!(
            authenticate(&mut c, &mut t, &key, OP_LOGIN, &[])
                .await
                .unwrap(),
            LoginOutcome::ReplacementRequired([7; 32])
        );
        assert_eq!(crate::store::load_account(&c).unwrap(), None);
        assert_eq!(pending_key(&mut c).unwrap(), key);
        drop(c);
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn public_preview_is_offline_and_legacy_join_is_rejected() {
        let code = profile::build(b"server.test", &[0x30, 0], &[7; 32]).unwrap();
        assert_eq!(preview(&code).unwrap().domain, "server.test");
        assert_eq!(preview("dmsg://join/AAAA"), Err(AuthError::BadQr));
        assert_eq!(
            preview(&format!("dmsg://server/{}", "A".repeat(8192))),
            Err(AuthError::BadQr)
        );
    }
}

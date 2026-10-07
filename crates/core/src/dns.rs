//! Persistent public carrier profile and Rust-owned native reconnect lifecycle.
//! The public profile contains no invitation or credentials.
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use slipstream_sys::{Config, NativeClient, Status};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub domain: String,
    pub certificate: Vec<u8>,
    pub noise_pubkey: [u8; 32],
    pub resolvers: Vec<SocketAddr>,
}

impl Profile {
    pub fn from_qr(qr: &str, resolvers: Vec<String>) -> Result<Self, &'static str> {
        let b = dmsg_protocol::profile::parse(qr).map_err(|_| "invalid public profile")?;
        let profile = Self {
            domain: String::from_utf8(b.domain).map_err(|_| "invalid domain")?,
            certificate: b.cert_der,
            noise_pubkey: b.noise_pubkey,
            resolvers: parse_resolvers(resolvers)?,
        };
        Ok(profile)
    }
    fn config(&self) -> Config {
        Config::new(
            self.domain.clone(),
            self.resolvers.clone(),
            self.certificate.clone(),
        )
    }
}

pub fn parse_resolvers(values: Vec<String>) -> Result<Vec<SocketAddr>, &'static str> {
    if values.is_empty() || values.len() > 8 {
        return Err("resolver count must be 1..8");
    }
    let addresses: Vec<SocketAddr> = values
        .into_iter()
        .map(|v| v.parse().map_err(|_| "resolver must be numeric IP:port"))
        .collect::<Result<_, _>>()?;
    let v4 = addresses[0].is_ipv4();
    if addresses.iter().any(|a| {
        a.port() == 0
            || a.is_ipv4() != v4
            || a.ip().is_unspecified()
            || a.ip().is_multicast()
            || matches!(a, SocketAddr::V6(v) if v.scope_id() != 0 || v.flowinfo() != 0)
    }) {
        return Err("invalid resolver address/port/family");
    }
    Ok(addresses)
}

fn schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS core_dns_profile (
        id INTEGER PRIMARY KEY CHECK(id=1), profile TEXT NOT NULL)",
    )
    .map_err(|_| "dns profile schema".into())
}
pub fn load(conn: &Connection) -> Result<Option<Profile>, String> {
    schema(conn)?;
    let raw: Option<String> = conn.query_row(
        "SELECT CAST(dmsg_unseal('dns_profile',profile) AS TEXT) FROM core_dns_profile WHERE id=1", [], |r| r.get(0))
        .optional().map_err(|_| "load dns profile".to_string())?;
    raw.map(|value| {
        let p: Profile =
            serde_json::from_str(&value).map_err(|_| "invalid stored dns profile".to_string())?;
        if !dmsg_protocol::profile::valid_domain(p.domain.as_bytes())
            || p.certificate.len() < 2
            || p.certificate.len() > dmsg_protocol::profile::PROFILE_MAX
            || p.certificate[0] != 0x30
        {
            return Err("invalid stored dns profile".into());
        }
        parse_resolvers(p.resolvers.iter().map(ToString::to_string).collect())
            .map_err(str::to_string)?;
        Ok(p)
    })
    .transpose()
}
pub fn save(conn: &Connection, profile: &Profile) -> Result<(), String> {
    schema(conn)?;
    let raw = serde_json::to_string(profile).map_err(|_| "encode dns profile".to_string())?;
    conn.execute(
        "INSERT INTO core_dns_profile VALUES(1,dmsg_seal('dns_profile',?1))
        ON CONFLICT(id) DO UPDATE SET profile=excluded.profile",
        [raw],
    )
    .map_err(|_| "save dns profile".to_string())?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Connecting,
    Ready(SocketAddr),
    Backoff(u32),
    Failed(i32),
    Stopped,
}
struct Run {
    owner: String,
    profile: Profile,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}
static RUN: OnceLock<Mutex<Option<Run>>> = OnceLock::new();
fn manager() -> &'static Mutex<Option<Run>> {
    RUN.get_or_init(|| Mutex::new(None))
}
fn finish(run: Run) -> Result<(), &'static str> {
    run.stop.store(true, Ordering::Release);
    run.worker.join().map_err(|_| "dns supervisor panicked")
}

/// A different store cannot silently replace another running account's carrier.
fn start(owner: &str, profile: &Profile) -> Result<Arc<Mutex<State>>, &'static str> {
    start_with_fallback(owner, profile, yandex_resolvers())
}

fn yandex_resolvers() -> Vec<SocketAddr> {
    ["77.88.8.8:53", "77.88.8.1:53"]
        .into_iter()
        .map(|s| s.parse().expect("fixed numeric resolver"))
        .collect()
}

fn start_with_fallback(
    owner: &str,
    profile: &Profile,
    fallback: Vec<SocketAddr>,
) -> Result<Arc<Mutex<State>>, &'static str> {
    let mut running = manager().lock().map_err(|_| "dns manager poisoned")?;
    if let Some(run) = running.as_ref() {
        if run.owner != owner {
            return Err("another account owns DNS transport");
        }
        if run.profile == *profile {
            return Ok(run.state.clone());
        }
    }
    if let Some(run) = running.take() {
        finish(run)?;
    }
    let state = Arc::new(Mutex::new(State::Connecting));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_state = state.clone();
    let worker_stop = stop.clone();
    let config = profile.config();
    let worker = thread::Builder::new()
        .name("dmsg-dns-supervisor".into())
        .spawn(move || supervise(config, fallback, worker_state, worker_stop))
        .map_err(|_| "spawn dns supervisor")?;
    *running = Some(Run {
        owner: owner.into(),
        profile: profile.clone(),
        state: state.clone(),
        stop,
        worker,
    });
    Ok(state)
}

fn publish(state: &Mutex<State>, value: State) {
    if let Ok(mut s) = state.lock() {
        *s = value;
    }
}
#[derive(Debug, PartialEq, Eq)]
enum NextAttempt {
    Terminal,
    Fallback,
    Backoff,
}

fn after_failure(code: i32, is_primary: bool, was_ready: bool) -> NextAttempt {
    if matches!(code, 2 | 4 | 5 | 7) {
        NextAttempt::Terminal
    } else if is_primary && !was_ready {
        NextAttempt::Fallback
    } else {
        NextAttempt::Backoff
    }
}

fn supervise(
    primary: Config,
    fallback: Vec<SocketAddr>,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
) {
    let mut backup = primary.clone();
    backup.resolvers = fallback;
    let mut is_primary = true;
    let mut delay = 1u32;
    while !stop.load(Ordering::Acquire) {
        publish(&state, State::Connecting);
        // Separate native runs: upstream probes every resolver in a successful
        // run as an additional QUIC path. Never append backup to the primary list.
        let config = if is_primary { &primary } else { &backup };
        let mut native = match NativeClient::start(config.clone()) {
            Ok(n) => n,
            Err(_) => {
                publish(&state, State::Failed(-1));
                return;
            }
        };
        let mut was_ready = false;
        let failure = loop {
            if stop.load(Ordering::Acquire) {
                break None;
            }
            match native.status() {
                Status::Ready(endpoint) => {
                    was_ready = true;
                    delay = 1;
                    publish(&state, State::Ready(endpoint.into()));
                }
                Status::Failed(code) => break Some(code),
                Status::Stopped => break Some(1),
                Status::Starting | Status::Listening(_) => (),
            }
            thread::sleep(Duration::from_millis(25));
        };
        // Join and release native lease before a subsequent attempt.
        if native.stop().is_err() {
            publish(&state, State::Failed(-1));
            return;
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        match after_failure(failure.unwrap_or(1), is_primary, was_ready) {
            NextAttempt::Terminal => {
                publish(&state, State::Failed(failure.unwrap_or(1)));
                return;
            }
            NextAttempt::Fallback => {
                is_primary = false;
                continue;
            }
            NextAttempt::Backoff => (),
        }
        publish(&state, State::Backoff(delay));
        let until = Instant::now() + Duration::from_secs(delay as u64);
        while Instant::now() < until && !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(25));
        }
        delay = (delay * 2).min(60);
        is_primary = true;
    }
    publish(&state, State::Stopped);
}

/// Wait for actual QUIC readiness, not merely an available local TCP listener.
pub fn endpoint(owner: &str, profile: &Profile) -> Result<SocketAddr, State> {
    let state = start(owner, profile).map_err(|_| State::Failed(-1))?;
    // Bootstrap bounds are 3s/resolver (at most 8 primary + 2 backup),
    // plus setup/join/status polling margin. Listening is never success.
    let until = Instant::now() + Duration::from_secs(35);
    loop {
        let s = *state.lock().map_err(|_| State::Failed(-1))?;
        match s {
            State::Ready(a) => return Ok(a),
            State::Failed(_) | State::Stopped | State::Backoff(_) => return Err(s),
            State::Connecting => (),
        }
        if Instant::now() >= until {
            return Err(State::Connecting);
        }
        thread::sleep(Duration::from_millis(25));
    }
}
pub fn status(owner: &str) -> Result<State, &'static str> {
    let running = manager().lock().map_err(|_| "dns manager poisoned")?;
    match running.as_ref().filter(|r| r.owner == owner) {
        Some(r) => Ok(*r.state.lock().map_err(|_| "dns state poisoned")?),
        None => Ok(State::Stopped),
    }
}
pub fn stop(owner: &str) -> Result<(), &'static str> {
    let mut running = manager().lock().map_err(|_| "dns manager poisoned")?;
    if running.as_ref().is_some_and(|r| r.owner == owner) {
        finish(running.take().unwrap())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervisor_single_owner_cancel_and_backoff_restart() {
        use std::net::UdpSocket;
        let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
        let profile = Profile {
            domain: "supervisor-fixture.invalid".into(),
            certificate: include_bytes!("../../../vendor/slipstream/certs/cert.pem").to_vec(),
            noise_pubkey: [0; 32],
            resolvers: vec![sink.local_addr().unwrap()],
        };
        let backup = UdpSocket::bind("127.0.0.1:0").unwrap();
        let fallback = vec![backup.local_addr().unwrap()];
        let shared = start_with_fallback("test-owner", &profile, fallback.clone()).unwrap();
        assert!(Arc::ptr_eq(
            &shared,
            &start_with_fallback("test-owner", &profile, fallback.clone()).unwrap()
        ));
        assert!(start("different-owner", &profile).is_err());
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut backed_off = false;
        loop {
            let state = *shared.lock().unwrap();
            if matches!(state, State::Backoff(1)) {
                backed_off = true;
            }
            if backed_off && state == State::Connecting {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "supervisor did not restart: {state:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let began = Instant::now();
        stop("test-owner").unwrap();
        assert!(began.elapsed() < Duration::from_secs(1));
        assert_eq!(status("test-owner").unwrap(), State::Stopped);
        let shared = start_with_fallback("test-owner", &profile, fallback).unwrap();
        stop("test-owner").unwrap();
        assert_eq!(*shared.lock().unwrap(), State::Stopped);
        stop("test-owner").unwrap();
    }
    #[test]
    fn group_policy_preserves_ready_and_fails_closed() {
        assert_eq!(yandex_resolvers().len(), 2);
        for code in [1, 3, 6, -1] {
            assert_eq!(after_failure(code, true, false), NextAttempt::Fallback);
            assert_eq!(after_failure(code, true, true), NextAttempt::Backoff);
            assert_eq!(after_failure(code, false, false), NextAttempt::Backoff);
            assert_eq!(after_failure(code, false, true), NextAttempt::Backoff);
        }
        for code in [2, 4, 5, 7] {
            for primary in [true, false] {
                for ready in [true, false] {
                    assert_eq!(after_failure(code, primary, ready), NextAttempt::Terminal);
                }
            }
        }
    }
    #[test]
    fn public_profile_roundtrip_excludes_invite_and_rejects_corruption() {
        let conn = Connection::open_in_memory().unwrap();
        crate::secure::register(&conn, None).unwrap();
        let uri =
            dmsg_protocol::profile::build(b"fixture.invalid", &[0x30, 0], &[8u8; 32]).unwrap();
        let p = Profile::from_qr(&uri, vec!["127.0.0.1:53".into()]).unwrap();
        assert!(load(&conn).unwrap().is_none());
        save(&conn, &p).unwrap();
        assert!(load(&conn).unwrap().as_ref() == Some(&p));
        let raw: String = conn
            .query_row("SELECT profile FROM core_dns_profile", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("token") && !raw.contains(&uri));
        conn.execute("UPDATE core_dns_profile SET profile='{}'", [])
            .unwrap();
        assert!(load(&conn).is_err());
    }
    #[test]
    fn resolver_validation_is_numeric_bounded_and_same_family() {
        for values in [
            vec![],
            vec!["dns.example:53".into()],
            vec!["0.0.0.0:53".into()],
            vec!["127.0.0.1:0".into()],
            vec!["127.0.0.1:53".into(), "[::1]:53".into()],
            vec!["127.0.0.1:53".into(); 9],
        ] {
            assert!(parse_resolvers(values).is_err());
        }
        assert!(parse_resolvers(vec!["[::1]:53".into()]).is_ok());
    }
    #[test]
    fn fresh_encrypted_profile_is_authenticated_and_survives_reopen() {
        let path = std::env::temp_dir().join(format!("dmsg-dns-profile-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let uri = dmsg_protocol::profile::build(b"fixture.invalid", &[0x30, 0], &[8; 32]).unwrap();
        let profile = Profile::from_qr(&uri, vec!["127.0.0.1:53".into()]).unwrap();
        let conn = crate::store::open_encrypted(&path, &[42; 32]).unwrap();
        save(&conn, &profile).unwrap();
        drop(conn);
        let conn = crate::store::open_encrypted(&path, &[42; 32]).unwrap();
        assert!(load(&conn).unwrap().as_ref() == Some(&profile));
        let kind: String = conn
            .query_row("SELECT typeof(profile) FROM core_dns_profile", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "blob");
        assert!(conn
            .execute("UPDATE core_dns_profile SET profile='{}'", [])
            .is_err());
        conn.execute("UPDATE core_dns_profile SET profile=x'444d53472d5331'", [])
            .unwrap();
        assert!(load(&conn).is_err());
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
#[path = "dns_native_tests.rs"]
mod native_tests;

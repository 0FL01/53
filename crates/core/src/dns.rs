//! Persistent public carrier profile and Rust-owned native reconnect lifecycle.
//! The public profile contains no invitation or credentials.
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use slipstream_sys::{
    Config, NativeClient, Status, StreamSnapshot, TailAckEliciting, TransportSnapshot,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    transport: Arc<Mutex<Option<ObservedTransport>>>,
    stream: Arc<Mutex<Option<SelectedStream>>>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

#[derive(Clone, Copy)]
struct ObservedTransport {
    snapshot: TransportSnapshot,
    observed_at: Instant,
}

struct SelectedStream {
    port: u16,
    generation: u64,
    observed: Option<(StreamSnapshot, Instant)>,
}

/// Private owner/run-scoped lease, not a port in diagnostic output.
pub(crate) struct StreamSelection {
    generation: u64,
    slot: Arc<Mutex<Option<SelectedStream>>>,
}
impl StreamSelection {
    pub(crate) fn evidence(&self) -> Option<serde_json::Value> {
        let slot = self.slot.try_lock().ok()?;
        let selected = slot.as_ref().filter(|s| s.generation == self.generation)?;
        let (s, at) = selected.observed?;
        if s.generation != self.generation {
            return None;
        }
        Some(serde_json::json!({
            "age_since_observed_ms": u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX),
            "publication_age_us": null,
            "schema_version": s.schema_version,
            "now_native_us": s.now_native_us,
            "tx_submitted": s.tx_submitted,
            "rx_contiguous": s.rx_contiguous,
            "rx_flushed": s.rx_flushed,
            "rx_pending": s.rx_pending,
            "stream_active": s.stream_active,
            "send_queue": s.send_queue,
            "failed": s.failed,
            "local_fin": s.local_fin,
            "remote_fin": s.remote_fin,
            "remote_stream_credit": s.remote_stream_credit,
        }))
    }
}
impl Drop for StreamSelection {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.slot.lock() {
            if slot
                .as_ref()
                .is_some_and(|s| s.generation == self.generation)
            {
                *slot = None;
            }
        }
    }
}

pub(crate) fn select_stream(owner: &str, port: u16) -> Option<StreamSelection> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let running = manager().try_lock().ok()?;
    let run = running
        .as_ref()
        .filter(|r| r.owner == owner && !r.stop.load(Ordering::Acquire))?;
    if port == 0 {
        return None;
    }
    let generation = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |g| {
            (g < (1 << 48) - 1).then_some(g + 1)
        })
        .ok()?;
    let mut slot = run.stream.lock().ok()?;
    // A second owner cannot silently displace a surviving lane lease.
    if slot.is_some() {
        return None;
    }
    *slot = Some(SelectedStream {
        port,
        generation,
        observed: None,
    });
    Some(StreamSelection {
        generation,
        slot: run.stream.clone(),
    })
}

fn reset_stream_observation(slot: &Mutex<Option<SelectedStream>>) {
    if let Ok(mut slot) = slot.lock() {
        if let Some(selected) = slot.as_mut() {
            selected.observed = None;
        }
    }
}

fn observe_stream(slot: &Mutex<Option<SelectedStream>>, native: &NativeClient) {
    let request = slot
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|s| (s.port, s.generation)));
    native.select_stream(request);
    let snapshot = native.stream_snapshot();
    if let Ok(mut slot) = slot.lock() {
        if let Some(selected) = slot.as_mut() {
            selected.observed = snapshot
                .filter(|s| s.generation == selected.generation)
                .map(|s| (s, Instant::now()));
        }
    }
}
static RUN: OnceLock<Mutex<Option<Run>>> = OnceLock::new();
fn manager() -> &'static Mutex<Option<Run>> {
    RUN.get_or_init(|| Mutex::new(None))
}
fn finish(run: Run) -> Result<(), &'static str> {
    run.stop.store(true, Ordering::Release);
    observe_transport(&run.transport, None);
    reset_stream_observation(&run.stream);
    run.worker.join().map_err(|_| "dns supervisor panicked")
}

// Only the existing supervisor reads NativeClient. Readers copy this one fixed
// snapshot without waiting on a diagnostic lock or holding it during JSON work.
fn observe_transport(
    current: &Mutex<Option<ObservedTransport>>,
    snapshot: Option<TransportSnapshot>,
) {
    if let Ok(mut current) = current.lock() {
        *current = snapshot.map(|snapshot| ObservedTransport {
            snapshot,
            observed_at: Instant::now(),
        });
    }
}

fn transport_observation(
    owner: &str,
    run_owner: &str,
    stopped: bool,
    current: &Mutex<Option<ObservedTransport>>,
) -> Option<ObservedTransport> {
    if owner != run_owner || stopped {
        return None;
    }
    *current.try_lock().ok()?
}

/// Owner-scoped numeric evidence, never a native handle or upstream read.
/// `age_since_observed_ms` is only time since our latest successful copy: a
/// lower bound on publication age, whose actual value/freshness stays unknown.
/// All native clocks/ages below `native_capture` describe native capture time,
/// with no app-clock mapping. Legacy packet context is duplicated across paths
/// when `multipath` is false; pending counts must not be summed in that mode.
pub(crate) fn transport_evidence(owner: &str) -> Option<serde_json::Value> {
    let observed = {
        let running = manager().try_lock().ok()?;
        let run = running.as_ref()?;
        transport_observation(
            owner,
            &run.owner,
            run.stop.load(Ordering::Acquire),
            &run.transport,
        )?
    };
    Some(transport_json(observed))
}

fn transport_json(observed: ObservedTransport) -> serde_json::Value {
    let snapshot = observed.snapshot;
    let paths: Vec<_> = snapshot
        .paths
        .iter()
        .take(usize::from(snapshot.captured_path_count))
        .map(|path| {
            path.map(|path| {
                serde_json::json!({
                    "slot": path.slot,
                    "validated": path.validated,
                    "demoted": path.demoted,
                    "smoothed_rtt_us": path.smoothed_rtt_us,
                    "base_retransmit_timer_us": path.base_retransmit_timer_us,
                    "consecutive_pto": path.consecutive_pto,
                    "bytes_in_transit": path.bytes_in_transit,
                    "cwin": path.cwin,
                    "pacing_bytes_per_second": path.pacing_bytes_per_second,
                    "send_mtu": path.send_mtu,
                    "pending_count": path.pending_count,
                    "pending_truncated": path.pending_truncated,
                    "oldest_pending_age_us": path.oldest_pending_age_us,
                    "tail_pending_age_us": path.tail_pending_age_us,
                    "highest_ack_age_us": path.highest_ack_age_us,
                    "last_packet_receive_age_us": path.last_packet_receive_age_us,
                    "tail_ack_eliciting": match path.tail_ack_eliciting {
                        TailAckEliciting::Unknown => None,
                        TailAckEliciting::No => Some(false),
                        TailAckEliciting::Yes => Some(true),
                    },
                })
            })
        })
        .collect();
    serde_json::json!({
        "age_since_observed_ms": u64::try_from(observed.observed_at.elapsed().as_millis())
            .unwrap_or(u64::MAX),
        "publication_age_us": null,
        "native_capture": {
            "schema_version": snapshot.schema_version,
            "connection_state": snapshot.connection_state,
            "multipath": snapshot.multipath,
            "path_count": snapshot.path_count,
            "captured_path_count": snapshot.captured_path_count,
            "now_native_us": snapshot.now_native_us,
            "packets_received": snapshot.packets_received,
            "data_sent": snapshot.data_sent,
            "data_received": snapshot.data_received,
            "latest_receive_age_us": snapshot.latest_receive_age_us,
            "latest_progress_age_us": snapshot.latest_progress_age_us,
            "paths": paths,
        },
    })
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
    let transport = Arc::new(Mutex::new(None));
    let stream = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_state = state.clone();
    let worker_transport = transport.clone();
    let worker_stream = stream.clone();
    let worker_stop = stop.clone();
    let config = profile.config();
    let worker = thread::Builder::new()
        .name("dmsg-dns-supervisor".into())
        .spawn(move || {
            supervise(
                config,
                fallback,
                worker_state,
                worker_transport,
                worker_stream,
                worker_stop,
            )
        })
        .map_err(|_| "spawn dns supervisor")?;
    *running = Some(Run {
        owner: owner.into(),
        profile: profile.clone(),
        state: state.clone(),
        transport,
        stream,
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
    transport: Arc<Mutex<Option<ObservedTransport>>>,
    stream: Arc<Mutex<Option<SelectedStream>>>,
    stop: Arc<AtomicBool>,
) {
    let mut backup = primary.clone();
    backup.resolvers = fallback;
    let mut is_primary = true;
    let mut delay = 1u32;
    while !stop.load(Ordering::Acquire) {
        observe_transport(&transport, None);
        reset_stream_observation(&stream);
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
            observe_transport(&transport, native.transport_snapshot());
            observe_stream(&stream, &native);
            thread::sleep(Duration::from_millis(25));
        };
        observe_transport(&transport, None);
        reset_stream_observation(&stream);
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
    observe_transport(&transport, None);
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
    fn stream_lease_is_generation_scoped_optional_and_contains_no_selector() {
        let snapshot = StreamSnapshot {
            generation: 7,
            schema_version: 1,
            now_native_us: 123,
            tx_submitted: 144,
            rx_contiguous: 200,
            rx_flushed: 190,
            rx_pending: 10,
            stream_active: true,
            send_queue: false,
            failed: false,
            local_fin: false,
            remote_fin: false,
            remote_stream_credit: 65536,
        };
        let slot = Arc::new(Mutex::new(Some(SelectedStream {
            port: 32123,
            generation: 7,
            observed: Some((snapshot, Instant::now())),
        })));
        let lease = StreamSelection {
            generation: 7,
            slot: slot.clone(),
        };
        let evidence = lease.evidence().unwrap();
        assert_eq!(evidence["tx_submitted"], 144);
        assert!(evidence.get("port").is_none() && evidence.get("generation").is_none());
        assert!(evidence["publication_age_us"].is_null());
        let lock = slot.lock().unwrap();
        assert!(
            lease.evidence().is_none(),
            "contention cannot reuse a previous copy"
        );
        drop(lock);
        reset_stream_observation(&slot);
        assert!(lease.evidence().is_none());
        // A stale lease must neither read nor clear a replacement generation.
        slot.lock().unwrap().as_mut().unwrap().generation = 8;
        assert!(lease.evidence().is_none());
        drop(lease);
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().generation, 8);
        let current = StreamSelection {
            generation: 8,
            slot: slot.clone(),
        };
        drop(current);
        assert!(slot.lock().unwrap().is_none());
    }

    fn transport_fixture(now_native_us: u64) -> TransportSnapshot {
        TransportSnapshot {
            schema_version: 1,
            connection_state: u32::MAX,
            multipath: false,
            path_count: 9,
            captured_path_count: 8,
            now_native_us,
            packets_received: u64::MAX,
            data_sent: u64::MAX,
            data_received: u64::MAX,
            latest_receive_age_us: None,
            latest_progress_age_us: None,
            paths: std::array::from_fn(|slot| {
                Some(slipstream_sys::TransportPathSnapshot {
                    slot: slot as u8,
                    validated: true,
                    demoted: false,
                    smoothed_rtt_us: u64::MAX,
                    base_retransmit_timer_us: u64::MAX,
                    consecutive_pto: u64::MAX,
                    bytes_in_transit: u64::MAX,
                    cwin: u64::MAX,
                    pacing_bytes_per_second: u64::MAX,
                    send_mtu: u64::MAX,
                    pending_count: slipstream_sys::TRANSPORT_SNAPSHOT_SCAN_LIMIT,
                    pending_truncated: true,
                    oldest_pending_age_us: None,
                    tail_pending_age_us: None,
                    highest_ack_age_us: None,
                    last_packet_receive_age_us: None,
                    tail_ack_eliciting: TailAckEliciting::Unknown,
                })
            }),
        }
    }

    #[test]
    fn transport_evidence_current_owner_attempt_clearing_and_contention() {
        let current = Mutex::new(None);
        let read = |owner, stopped| transport_observation(owner, "owner", stopped, &current);
        assert!(read("owner", false).is_none());
        observe_transport(&current, Some(transport_fixture(11)));
        assert!(read("another-owner", false).is_none());
        assert!(read("owner", true).is_none());
        assert_eq!(read("owner", false).unwrap().snapshot.now_native_us, 11);
        {
            let _guard = current.lock().unwrap();
            assert!(read("owner", false).is_none(), "diagnostics must not wait");
        }
        // The same clearing operation is used for a None native read, attempt
        // transitions and stop; none of them can fall back to the previous copy.
        observe_transport(&current, None);
        assert!(read("owner", false).is_none());
        observe_transport(&current, Some(transport_fixture(99)));
        assert_eq!(read("owner", false).unwrap().snapshot.now_native_us, 99);
        observe_transport(&current, None);
        assert!(read("owner", false).is_none());
        assert!(transport_evidence("nonexistent-diagnostic-owner").is_none());
    }

    #[test]
    fn transport_evidence_json_is_bounded_numeric_native_capture_schema() {
        use serde_json::Value;
        fn keys(value: &Value, expected: &[&str]) {
            let actual: std::collections::BTreeSet<_> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(actual, expected.iter().copied().collect());
        }
        fn numeric_only(value: &Value) {
            match value {
                Value::Null | Value::Bool(_) | Value::Number(_) => (),
                Value::Array(values) => values.iter().for_each(numeric_only),
                Value::Object(values) => values.values().for_each(numeric_only),
                Value::String(_) => panic!("transport evidence must contain no string values"),
            }
        }
        let observed_at = Instant::now() - Duration::from_millis(100);
        let mut snapshot = transport_fixture(1);
        snapshot.latest_receive_age_us = Some(23);
        snapshot.paths[1].as_mut().unwrap().tail_ack_eliciting = TailAckEliciting::No;
        snapshot.paths[2].as_mut().unwrap().tail_ack_eliciting = TailAckEliciting::Yes;
        let value = transport_json(ObservedTransport {
            snapshot,
            observed_at,
        });
        keys(
            &value,
            &[
                "age_since_observed_ms",
                "publication_age_us",
                "native_capture",
            ],
        );
        assert!(value["age_since_observed_ms"].as_u64().unwrap() >= 100);
        assert!(value["publication_age_us"].is_null());
        let capture = &value["native_capture"];
        keys(
            capture,
            &[
                "schema_version",
                "connection_state",
                "multipath",
                "path_count",
                "captured_path_count",
                "now_native_us",
                "packets_received",
                "data_sent",
                "data_received",
                "latest_receive_age_us",
                "latest_progress_age_us",
                "paths",
            ],
        );
        // Core elapsed time is never added to a native clock or native age.
        assert_eq!(capture["now_native_us"], 1);
        assert_eq!(capture["latest_receive_age_us"], 23);
        assert!(capture["latest_progress_age_us"].is_null());
        assert_eq!(capture["path_count"], 9);
        assert_eq!(capture["captured_path_count"], 8);
        let paths = capture["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 8);
        for (slot, path) in paths.iter().enumerate() {
            keys(
                path,
                &[
                    "slot",
                    "validated",
                    "demoted",
                    "smoothed_rtt_us",
                    "base_retransmit_timer_us",
                    "consecutive_pto",
                    "bytes_in_transit",
                    "cwin",
                    "pacing_bytes_per_second",
                    "send_mtu",
                    "pending_count",
                    "pending_truncated",
                    "oldest_pending_age_us",
                    "tail_pending_age_us",
                    "highest_ack_age_us",
                    "last_packet_receive_age_us",
                    "tail_ack_eliciting",
                ],
            );
            assert_eq!(path["slot"], slot as u64);
            assert_eq!(path["pending_count"], 64);
            for age in [
                "oldest_pending_age_us",
                "tail_pending_age_us",
                "highest_ack_age_us",
                "last_packet_receive_age_us",
            ] {
                assert!(path[age].is_null());
            }
        }
        assert!(paths[0]["tail_ack_eliciting"].is_null());
        assert_eq!(paths[1]["tail_ack_eliciting"], false);
        assert_eq!(paths[2]["tail_ack_eliciting"], true);
        numeric_only(&value);
        // Worst-width numeric values remain bounded, with no aggregate of the
        // duplicated legacy packet contexts and no extra identifying fields.
        snapshot.now_native_us = u64::MAX;
        snapshot.latest_receive_age_us = Some(u64::MAX);
        snapshot.latest_progress_age_us = Some(u64::MAX);
        for path in snapshot.paths.iter_mut().flatten() {
            path.oldest_pending_age_us = Some(u64::MAX);
            path.tail_pending_age_us = Some(u64::MAX);
            path.highest_ack_age_us = Some(u64::MAX);
            path.last_packet_receive_age_us = Some(u64::MAX);
        }
        let maximal = transport_json(ObservedTransport {
            snapshot,
            observed_at,
        });
        assert!(serde_json::to_vec(&maximal).unwrap().len() < 8_192);
        snapshot.paths[0] = None;
        let absent = transport_json(ObservedTransport {
            snapshot,
            observed_at,
        });
        assert!(absent["native_capture"]["paths"][0].is_null());
        snapshot.captured_path_count = 0;
        snapshot.paths = [None; slipstream_sys::TRANSPORT_SNAPSHOT_MAX_PATHS];
        let empty = transport_json(ObservedTransport {
            snapshot,
            observed_at,
        });
        assert!(empty["native_capture"]["paths"]
            .as_array()
            .unwrap()
            .is_empty());
    }

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
        assert!(transport_evidence("different-owner").is_none());
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
        assert!(transport_evidence("test-owner").is_none());
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

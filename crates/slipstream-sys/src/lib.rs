//! Single-instance, managed embedding of the pinned C Slipstream client.
//! `Listening` is only a bound loopback socket. Wait for `Ready` before core
//! opens streams; Noise authentication on each stream belongs to core.
use std::{
    ffi::{c_char, c_int, c_void, CString},
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

/// Native recursive DNS carrier is UDP; address/port are numeric, never resolved.
#[derive(Clone, Debug)]
pub struct Config {
    pub domain: String,
    pub resolvers: Vec<SocketAddr>,
    /// Full expected leaf X.509 certificate, DER or single PEM certificate.
    pub certificate: Vec<u8>,
    pub congestion_control: CongestionControl,
    pub active_keepalive_ms: u32,
    pub idle_keepalive_ms: u32,
    /// Zero selects an ephemeral port. The bind address is always 127.0.0.1.
    pub listen_port: u16,
}

impl Config {
    pub fn new(domain: String, resolvers: Vec<SocketAddr>, certificate: Vec<u8>) -> Self {
        Self {
            domain,
            resolvers,
            certificate,
            congestion_control: CongestionControl::Dcubic,
            active_keepalive_ms: 400,
            idle_keepalive_ms: 5000,
            listen_port: 0,
        }
    }
    fn validate(&self) -> Result<(), Error> {
        if self.domain.is_empty()
            || self.domain.len() > 180
            || !self.domain.is_ascii()
            || self.domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(Error::InvalidConfig("domain"));
        }
        if self.resolvers.is_empty() || self.resolvers.len() > 8 {
            return Err(Error::InvalidConfig("resolver count (1..=8)"));
        }
        let v4 = self.resolvers[0].is_ipv4();
        if self.resolvers.iter().any(|a| {
            a.port() == 0
                || a.is_ipv4() != v4
                || a.ip().is_unspecified()
                || a.ip().is_multicast()
                || matches!(a, SocketAddr::V6(v) if v.scope_id() != 0 || v.flowinfo() != 0)
        }) {
            return Err(Error::InvalidConfig("resolver address/port/family"));
        }
        if self.certificate.is_empty() || self.certificate.len() > 65536 {
            return Err(Error::InvalidConfig("certificate size (1..=65536)"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CongestionControl {
    Dcubic,
    Bbr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    Listening(SocketAddrV4),
    Ready(SocketAddrV4),
    Stopped,
    /// Native code: 1 loss/bootstrap exhausted, 2 invalid arguments,
    /// 3 QUIC allocation/configuration, 4 invalid pin, 5 listener, 6 connect,
    /// 7 presented full certificate mismatch;
    /// other values are propagated packet-loop errors.
    Failed(i32),
}

/// Fixed diagnostic bounds; this snapshot has no packet, address or CID identity.
pub const TRANSPORT_SNAPSHOT_MAX_PATHS: usize = 8;
pub const TRANSPORT_SNAPSHOT_SCAN_LIMIT: u64 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailAckEliciting {
    /// No non-trap tail, or the worker has not already classified its frames.
    Unknown,
    No,
    Yes,
}

/// Immutable numeric copy from one native worker publication (private schema 1).
/// Times are native microseconds, with no mapping to an app/sample clock. All
/// absent, uninitialised and invalid-time ages are `None`, not zero. Connection
/// receive/progress and highest-ACK times seeded at connection start stay unknown.
/// Ages describe capture time, not read time: a stalled worker can leave an old
/// publication. Publication age is unknown; there is no freshness guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportSnapshot {
    pub schema_version: u32,
    /// Numeric `picoquic_state_enum` from the pinned C transport.
    pub connection_state: u32,
    pub multipath: bool,
    /// Actual native count, which may exceed the eight captured ordinal slots.
    pub path_count: u32,
    pub captured_path_count: u8,
    /// Original native capture clock; do not subtract an app/sample timestamp.
    pub now_native_us: u64,
    pub packets_received: u64,
    /// Native QUIC flow-control data counters, not DNS wire byte counters.
    pub data_sent: u64,
    pub data_received: u64,
    pub latest_receive_age_us: Option<u64>,
    pub latest_progress_age_us: Option<u64>,
    /// `None` for uncaptured/absent slots. Ordinals can change between snapshots.
    pub paths: [Option<TransportPathSnapshot>; TRANSPORT_SNAPSHOT_MAX_PATHS],
}

/// App packet context: per-path in multipath; otherwise the connection-wide
/// application context is duplicated in EVERY path slot, without path filtering.
/// Do not sum legacy pending counts or treat these context ages as path-specific.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportPathSnapshot {
    /// Array ordinal only: never the native unique path ID.
    pub slot: u8,
    pub validated: bool,
    pub demoted: bool,
    /// Stored values, including native defaults; no RTT/pacing helper is invoked.
    pub smoothed_rtt_us: u64,
    /// Base timer, without exponential PTO backoff or deadline calculation.
    pub base_retransmit_timer_us: u64,
    pub consecutive_pto: u64,
    pub bytes_in_transit: u64,
    pub cwin: u64,
    pub pacing_bytes_per_second: u64,
    pub send_mtu: u64,
    /// Counts non-traps in a backwards scan of at most 64 nodes INCLUDING traps.
    /// ACK/POLL-only packets count; unsent ACK traps do not. Lower bound if truncated.
    pub pending_count: u64,
    pub pending_truncated: bool,
    /// Unknown for an empty or truncated scan (not the oldest scanned prefix).
    pub oldest_pending_age_us: Option<u64>,
    /// Actual last non-trap; unknown if absent, invalid, or hidden behind >64 traps.
    pub tail_pending_age_us: Option<u64>,
    pub highest_ack_age_us: Option<u64>,
    pub last_packet_receive_age_us: Option<u64>,
    /// Cached classification of that tail, not a recomputed PTO anchor.
    pub tail_ack_eliciting: TailAckEliciting,
}

/// Numeric frontiers for exactly one privately selected loopback stream.
/// All bytes include the lane's existing framing/handshake, not just SRTCP.
/// Capture is worker-owned; native time has no app-clock mapping and publication
/// age is unknown. This is observation only, never a delivery/recovery policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSnapshot {
    /// Internal selection match key only; core must not emit this identifier.
    pub generation: u64,
    pub schema_version: u32,
    pub now_native_us: u64,
    /// QUIC sent_offset: submitted stream bytes, not ACKed or UDP delivery.
    pub tx_submitted: u64,
    /// QUIC consumed_offset: contiguous bytes delivered to the native bridge.
    pub rx_contiguous: u64,
    /// Bytes accepted by the local TCP socket, not read/authenticated by Rust.
    pub rx_flushed: u64,
    /// Bytes still in the bridge output buffer (out_len - out_off).
    pub rx_pending: u64,
    pub stream_active: bool,
    pub send_queue: bool,
    /// Failed/retired streams are unavailable, so an accepted snapshot is false.
    pub failed: bool,
    pub local_fin: bool,
    pub remote_fin: bool,
    /// Remaining stream flow-control credit: maxdata_remote - sent_offset.
    pub remote_stream_credit: u64,
    /// Highest accepted STREAM end offset (fin_offset), including zero-length
    /// ranges/FIN. Excess over contiguous is an end-offset gap, not a count of
    /// buffered suffix bytes or ACK/delivery proof.
    pub rx_highest: u64,
    /// Sender STREAM byte coverage [0,E), not path ACKs or app authentication.
    /// None means invalid ranges or an unresolved bounded scan; zero is known
    /// empty coverage. The pinned transport preserves empty-frame ACK semantics.
    pub tx_ack_prefix_end: Option<u64>,
}

#[derive(Debug)]
pub enum Error {
    InvalidConfig(&'static str),
    AlreadyRunning,
    Allocation,
    Spawn(std::io::Error),
    WorkerPanicked,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(field) => write!(f, "invalid native {field}"),
            Self::AlreadyRunning => {
                f.write_str("native instance is still owned; stop/join it first")
            }
            Self::Allocation => f.write_str("native control allocation failed"),
            Self::Spawn(e) => write!(f, "native worker spawn failed: {e}"),
            Self::WorkerPanicked => f.write_str("native worker panicked"),
        }
    }
}
impl std::error::Error for Error {}

#[repr(C)]
struct CConfig {
    domain: *const c_char,
    resolvers: [*const c_char; 8],
    resolver_ports: [u16; 8],
    resolver_count: usize,
    certificate: *const u8,
    certificate_len: usize,
    cc: *const c_char,
    active_keepalive_ms: usize,
    idle_keepalive_ms: usize,
    listen_port: u16,
}

// Deliberately explicit C layout, all u64 words: 11 header + 8 * 18 path = 155.
// This plain struct is a caller-owned copy; the shared C bytes are ALL atomic.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CTransportPath {
    slot: u64,
    present: u64,
    validated: u64,
    demoted: u64,
    smoothed_rtt_us: u64,
    base_retransmit_timer_us: u64,
    consecutive_pto: u64,
    bytes_in_transit: u64,
    cwin: u64,
    pacing_bytes_per_second: u64,
    send_mtu: u64,
    pending_count: u64,
    pending_truncated: u64,
    oldest_pending_age_us: u64,
    tail_pending_age_us: u64,
    highest_ack_age_us: u64,
    last_packet_receive_age_us: u64,
    tail_ack_eliciting: u64,
}
#[repr(C)]
#[derive(Default)]
struct CTransportSnapshot {
    schema_version: u64,
    connection_state: u64,
    multipath: u64,
    path_count: u64,
    captured_path_count: u64,
    now_native_us: u64,
    packets_received: u64,
    data_sent: u64,
    data_received: u64,
    latest_receive_age_us: u64,
    latest_progress_age_us: u64,
    paths: [CTransportPath; TRANSPORT_SNAPSHOT_MAX_PATHS],
}
const _: () = assert!(std::mem::size_of::<CTransportPath>() == 18 * 8);
const _: () = assert!(std::mem::size_of::<CTransportSnapshot>() == 155 * 8);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CStreamSnapshot {
    generation: u64,
    schema_version: u64,
    now_native_us: u64,
    tx_submitted: u64,
    rx_contiguous: u64,
    rx_flushed: u64,
    rx_pending: u64,
    stream_active: u64,
    send_queue: u64,
    failed: u64,
    local_fin: u64,
    remote_fin: u64,
    remote_stream_credit: u64,
    rx_highest: u64,
    tx_ack_prefix_end: u64,
}
const _: () = assert!(std::mem::size_of::<CStreamSnapshot>() == 15 * 8);
const STREAM_MAX_GENERATION: u64 = (1 << 48) - 1;

fn stream_request(selection: Option<(u16, u64)>) -> u64 {
    match selection {
        Some((port, generation))
            if port != 0 && (1..=STREAM_MAX_GENERATION).contains(&generation) =>
        {
            (generation << 16) | u64::from(port)
        }
        _ => 0,
    }
}
impl CStreamSnapshot {
    fn decode(self) -> Option<StreamSnapshot> {
        if self.schema_version != 3
            || !(1..=STREAM_MAX_GENERATION).contains(&self.generation)
            || self.failed != 0
            || self.rx_flushed.checked_add(self.rx_pending)? != self.rx_contiguous
            || self.rx_highest < self.rx_contiguous
            || (self.tx_ack_prefix_end != u64::MAX
                && (self.tx_ack_prefix_end > self.tx_submitted
                    || self.tx_ack_prefix_end > (1 << 62) - 1))
        {
            return None;
        }
        Some(StreamSnapshot {
            generation: self.generation,
            schema_version: 3,
            now_native_us: self.now_native_us,
            tx_submitted: self.tx_submitted,
            rx_contiguous: self.rx_contiguous,
            rx_flushed: self.rx_flushed,
            rx_pending: self.rx_pending,
            stream_active: transport_bool(self.stream_active)?,
            send_queue: transport_bool(self.send_queue)?,
            failed: false,
            local_fin: transport_bool(self.local_fin)?,
            remote_fin: transport_bool(self.remote_fin)?,
            remote_stream_credit: self.remote_stream_credit,
            rx_highest: self.rx_highest,
            tx_ack_prefix_end: (self.tx_ack_prefix_end != u64::MAX)
                .then_some(self.tx_ack_prefix_end),
        })
    }
}

fn transport_bool(value: u64) -> Option<bool> {
    match value {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}
fn transport_age(value: u64) -> Option<u64> {
    (value != u64::MAX).then_some(value)
}
impl CTransportSnapshot {
    fn decode(self) -> Option<TransportSnapshot> {
        if self.schema_version != 1
            || self.captured_path_count != self.path_count.min(TRANSPORT_SNAPSHOT_MAX_PATHS as u64)
        {
            return None;
        }
        let mut paths = [None; TRANSPORT_SNAPSHOT_MAX_PATHS];
        for (i, path) in self.paths[..self.captured_path_count as usize]
            .iter()
            .enumerate()
        {
            if path.slot != i as u64 || path.pending_count > TRANSPORT_SNAPSHOT_SCAN_LIMIT {
                return None;
            }
            if !transport_bool(path.present)? {
                continue;
            }
            paths[i] = Some(TransportPathSnapshot {
                slot: i as u8,
                validated: transport_bool(path.validated)?,
                demoted: transport_bool(path.demoted)?,
                smoothed_rtt_us: path.smoothed_rtt_us,
                base_retransmit_timer_us: path.base_retransmit_timer_us,
                consecutive_pto: path.consecutive_pto,
                bytes_in_transit: path.bytes_in_transit,
                cwin: path.cwin,
                pacing_bytes_per_second: path.pacing_bytes_per_second,
                send_mtu: path.send_mtu,
                pending_count: path.pending_count,
                pending_truncated: transport_bool(path.pending_truncated)?,
                oldest_pending_age_us: transport_age(path.oldest_pending_age_us),
                tail_pending_age_us: transport_age(path.tail_pending_age_us),
                highest_ack_age_us: transport_age(path.highest_ack_age_us),
                last_packet_receive_age_us: transport_age(path.last_packet_receive_age_us),
                tail_ack_eliciting: match path.tail_ack_eliciting {
                    0 => TailAckEliciting::Unknown,
                    1 => TailAckEliciting::No,
                    2 => TailAckEliciting::Yes,
                    _ => return None,
                },
            });
        }
        Some(TransportSnapshot {
            schema_version: 1,
            connection_state: self.connection_state.try_into().ok()?,
            multipath: transport_bool(self.multipath)?,
            path_count: self.path_count.try_into().ok()?,
            captured_path_count: self.captured_path_count as u8,
            now_native_us: self.now_native_us,
            packets_received: self.packets_received,
            data_sent: self.data_sent,
            data_received: self.data_received,
            latest_receive_age_us: transport_age(self.latest_receive_age_us),
            latest_progress_age_us: transport_age(self.latest_progress_age_us),
            paths,
        })
    }
}
extern "C" {
    fn dmsg_control_new() -> *mut c_void;
    fn dmsg_control_free(control: *mut c_void);
    fn dmsg_control_stop(control: *mut c_void);
    fn dmsg_control_status(control: *mut c_void) -> u64;
    fn dmsg_control_transport_snapshot(
        control: *const c_void,
        out: *mut CTransportSnapshot,
        out_size: usize,
    ) -> c_int;
    fn dmsg_control_select_stream(control: *mut c_void, packed_request: u64);
    fn dmsg_control_stream_snapshot(
        control: *const c_void,
        out: *mut CStreamSnapshot,
        out_size: usize,
    ) -> c_int;
    fn dmsg_native_run(control: *mut c_void, config: *const CConfig) -> c_int;
}

struct Control(NonNull<c_void>);
// C allocates and accesses control only through C11 atomics. Free occurs after
// the last Arc, including the worker's Arc, is dropped.
unsafe impl Send for Control {}
unsafe impl Sync for Control {}
impl Drop for Control {
    fn drop(&mut self) {
        unsafe { dmsg_control_free(self.0.as_ptr()) };
    }
}
static OWNED: AtomicBool = AtomicBool::new(false);
struct Lease;
impl Drop for Lease {
    fn drop(&mut self) {
        OWNED.store(false, Ordering::Release);
    }
}

/// One worker/run. Start is asynchronous: native setup failures are `Failed`,
/// while Rust validation/allocation/spawn errors are returned from `start`.
/// Ownership remains reserved even after terminal failure until `stop`/Drop
/// joins the worker. `request_stop` can race with setup and status reads.
pub struct NativeClient {
    control: Arc<Control>,
    worker: Option<JoinHandle<()>>,
    lease: Option<Lease>,
}

impl NativeClient {
    pub fn start(config: Config) -> Result<Self, Error> {
        config.validate()?;
        OWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::AlreadyRunning)?;
        let lease = Lease;
        let control = Arc::new(Control(
            NonNull::new(unsafe { dmsg_control_new() }).ok_or(Error::Allocation)?,
        ));
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("slipstream-native".into())
            .spawn(move || run(&worker_control, config))
            .map_err(Error::Spawn)?;
        Ok(Self {
            control,
            worker: Some(worker),
            lease: Some(lease),
        })
    }
    pub fn status(&self) -> Status {
        let value = unsafe { dmsg_control_status(self.control.0.as_ptr()) };
        let endpoint = SocketAddrV4::new(Ipv4Addr::LOCALHOST, (value >> 8) as u16);
        match value as u8 {
            0 => Status::Starting,
            1 => Status::Listening(endpoint),
            2 => Status::Ready(endpoint),
            3 => Status::Stopped,
            4 => Status::Failed((value >> 24) as u32 as i32),
            _ => unreachable!("private native status ABI"),
        }
    }
    /// Latest coherent copy from the existing worker's before-select callback,
    /// after service when ready. At most three native read attempts. `None`
    /// before first publication, after native termination, on contention or an
    /// invalid schema. Never returns a cached previous attempt as current.
    /// A successful read can still be stale; ages are relative to `now_native_us`
    /// at capture and publication age is unknown. A concurrent stop may race the
    /// read; terminal publication invalidates the bytes on the owning worker.
    pub fn transport_snapshot(&self) -> Option<TransportSnapshot> {
        let mut out = CTransportSnapshot::default();
        let available = unsafe {
            dmsg_control_transport_snapshot(
                self.control.0.as_ptr(),
                &mut out,
                std::mem::size_of::<CTransportSnapshot>(),
            )
        };
        (available == 1).then(|| out.decode()).flatten()
    }
    /// Select one lane using the Rust socket's LOCAL loopback ephemeral port,
    /// never the native listener port. Use a fresh generation in 1..=2^48-1 on
    /// every changed selection/lane lifetime; never reuse a packed key during
    /// this client's lifetime. Neither port nor upstream pointers are public.
    /// None (or invalid zero port/out-of-range generation) clears selection.
    /// Only a private atomic request changes here; upstream traversal stays on
    /// the existing worker. A changed request immediately hides the old lane.
    pub fn select_stream(&self, selection: Option<(u16, u64)>) {
        unsafe {
            dmsg_control_select_stream(self.control.0.as_ptr(), stream_request(selection));
        }
    }
    /// Latest coherent selected-lane copy after native service. Three attempts,
    /// no stale-copy fallback. None before matching publication, on selection
    /// change/no-match/retirement/failure, inconsistent offsets or contention.
    /// Worker stalls can leave an old publication of the SAME lane; native
    /// capture time does not establish its age against an application clock.
    pub fn stream_snapshot(&self) -> Option<StreamSnapshot> {
        let mut out = CStreamSnapshot::default();
        let available = unsafe {
            dmsg_control_stream_snapshot(
                self.control.0.as_ptr(),
                &mut out,
                std::mem::size_of::<CStreamSnapshot>(),
            )
        };
        (available == 1).then(|| out.decode()).flatten()
    }
    /// Bound address while listening/ready; this does not imply QUIC readiness.
    pub fn endpoint(&self) -> Option<SocketAddrV4> {
        match self.status() {
            Status::Listening(a) | Status::Ready(a) => Some(a),
            _ => None,
        }
    }
    pub fn request_stop(&self) {
        unsafe { dmsg_control_stop(self.control.0.as_ptr()) };
    }
    /// Idempotent cancellation + join; returns the terminal native status.
    /// Releases the single-instance reservation only after native cleanup.
    pub fn stop(&mut self) -> Result<Status, Error> {
        self.request_stop();
        let joined = self.worker.take().map(|worker| worker.join());
        self.lease.take();
        if matches!(joined, Some(Err(_))) {
            return Err(Error::WorkerPanicked);
        }
        Ok(self.status())
    }
}
impl Drop for NativeClient {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run(control: &Control, config: Config) {
    let domain = CString::new(config.domain).expect("validated ASCII domain");
    let addresses: Vec<_> = config
        .resolvers
        .iter()
        .map(|a| {
            CString::new(match a.ip() {
                IpAddr::V4(a) => a.to_string(),
                IpAddr::V6(a) => a.to_string(),
            })
            .unwrap()
        })
        .collect();
    let mut resolvers = [std::ptr::null(); 8];
    let mut ports = [0; 8];
    for (i, address) in addresses.iter().enumerate() {
        resolvers[i] = address.as_ptr();
        ports[i] = config.resolvers[i].port();
    }
    let cc = match config.congestion_control {
        CongestionControl::Dcubic => b"dcubic\0".as_slice(),
        CongestionControl::Bbr => b"bbr\0".as_slice(),
    };
    let native = CConfig {
        domain: domain.as_ptr(),
        resolvers,
        resolver_ports: ports,
        resolver_count: addresses.len(),
        certificate: config.certificate.as_ptr(),
        certificate_len: config.certificate.len(),
        cc: cc.as_ptr().cast(),
        active_keepalive_ms: config.active_keepalive_ms as usize,
        idle_keepalive_ms: config.idle_keepalive_ms as usize,
        listen_port: config.listen_port,
    };
    // All input allocations outlive this blocking call. No upstream pointer
    // escapes the worker; Rust owns reconnection by starting a subsequent run.
    unsafe { dmsg_native_run(control.0.as_ptr(), &native) };
}

#[cfg(test)]
mod snapshot_decode_tests {
    use super::*;

    #[test]
    fn stream_selection_packing_and_strict_frontier_decode() {
        assert_eq!(stream_request(None), 0);
        for selection in [(0, 1), (1, 0), (1, 1 << 48), (1, u64::MAX)] {
            assert_eq!(stream_request(Some(selection)), 0);
        }
        assert_eq!(stream_request(Some((0xbeef, 42))), (42 << 16) | 0xbeef);
        assert_eq!(
            stream_request(Some((u16::MAX, STREAM_MAX_GENERATION))),
            u64::MAX
        );
        let valid = CStreamSnapshot {
            generation: 42,
            schema_version: 3,
            now_native_us: 1042,
            rx_contiguous: 70,
            rx_pending: 70,
            rx_highest: 90,
            ..Default::default()
        };
        let value = valid.decode().unwrap();
        assert_eq!(value.rx_flushed, 0); // Known zero, not unavailable.
        assert_eq!(value.rx_pending, 70);
        assert!(!value.failed);
        assert_eq!(value.rx_highest, 90); // Gap is distinct from bridge pending.
        assert_eq!(value.tx_ack_prefix_end, Some(0));
        assert_eq!(
            CStreamSnapshot {
                tx_ack_prefix_end: u64::MAX,
                ..valid
            }
            .decode()
            .unwrap()
            .tx_ack_prefix_end,
            None
        );
        for prefix in [0, 100, (1 << 62) - 1] {
            assert_eq!(
                CStreamSnapshot {
                    tx_submitted: (1 << 62) - 1,
                    tx_ack_prefix_end: prefix,
                    ..valid
                }
                .decode()
                .unwrap()
                .tx_ack_prefix_end,
                Some(prefix)
            );
        }
        assert!(CStreamSnapshot {
            tx_submitted: 1 << 62,
            tx_ack_prefix_end: 1 << 62,
            ..valid
        }
        .decode()
        .is_none());
        for field in 0..14 {
            let mut raw = valid;
            match field {
                0 => raw.schema_version = 0,
                1 => raw.schema_version = 1,
                2 => raw.generation = 0,
                3 => raw.generation = 1 << 48,
                4 => raw.failed = 1,
                5 => raw.stream_active = 2,
                6 => raw.send_queue = 2,
                7 => raw.local_fin = 2,
                8 => raw.remote_fin = 2,
                9 => raw.rx_flushed = 1,
                10 => raw.rx_flushed = u64::MAX,
                11 => raw.rx_highest = 69,
                12 => raw.schema_version = 2,
                13 => raw.tx_ack_prefix_end = 1,
                _ => unreachable!(),
            }
            assert!(raw.decode().is_none(), "invalid stream field {field}");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_stream_input_highest_offset_distinguishes_reassembly_holes() {
        use std::{path::PathBuf, process::Command};
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let out = PathBuf::from(env!("OUT_DIR"));
        let source = out.join("source");
        let binary = out.join("stream-received-frontier");
        let mut compile = Command::new("cc");
        compile.env_clear().env("PATH", "/usr/bin:/bin").args([
            "-std=c11",
            "-D_GNU_SOURCE",
            "-O1",
            "-Wall",
            "-Wextra",
            "-Werror",
        ]);
        for include in [
            package.join("native"),
            source.join("include"),
            source.join("subprojects/picoquic/picoquic"),
            out.join("build/_deps/picotls-src/include"),
            out.join("openssl-build/install/include"),
        ] {
            compile.arg("-I").arg(include);
        }
        compile
            .arg(package.join("native/tests/stream_received_frontier.c"))
            .arg("-Wl,--start-group");
        for lib in [
            "dmsg_slipstream",
            "picoquic-core",
            "picotls-openssl",
            "picotls-minicrypto",
            "picotls-core",
        ] {
            compile.arg(out.join("lib").join(format!("lib{lib}.a")));
        }
        for lib in ["ssl", "crypto"] {
            compile.arg(
                out.join("openssl-build/install/lib")
                    .join(format!("lib{lib}.a")),
            );
        }
        let output = compile
            .args(["-Wl,--end-group", "-lm", "-lpthread", "-ldl", "-o"])
            .arg(&binary)
            .output()
            .expect("host C compiler");
        assert!(
            output.status.success(),
            "real STREAM fixture compile: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(&binary).env_clear().output().unwrap();
        assert!(
            output.status.success(),
            "real STREAM fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!("{}", String::from_utf8_lossy(&output.stdout).trim());
    }

    #[test]
    fn real_c_stream_control_clear_and_unpublished_are_unavailable() {
        extern "C" {
            fn dmsg_stream_publish(control: *mut c_void, conn: *const c_void, now: u64);
        }
        let control = Arc::new(Control(
            NonNull::new(unsafe { dmsg_control_new() }).unwrap(),
        ));
        let client = NativeClient {
            control,
            worker: None,
            lease: None,
        };
        assert!(client.stream_snapshot().is_none());
        client.select_stream(Some((54321, 42)));
        assert!(client.stream_snapshot().is_none());
        unsafe { dmsg_stream_publish(client.control.0.as_ptr(), std::ptr::null(), 1042) };
        assert!(client.stream_snapshot().is_none());
        client.select_stream(None);
        assert!(client.stream_snapshot().is_none());
        client.select_stream(Some((54321, 1 << 48)));
        assert!(client.stream_snapshot().is_none());
    }

    #[test]
    fn fixed_layout_unknown_zero_and_invalid_schema() {
        let mut raw = CTransportSnapshot {
            schema_version: 1,
            path_count: 1,
            captured_path_count: 1,
            latest_receive_age_us: u64::MAX,
            latest_progress_age_us: 0,
            ..Default::default()
        };
        raw.paths[0].present = 1;
        raw.paths[0].oldest_pending_age_us = u64::MAX;
        raw.paths[0].highest_ack_age_us = u64::MAX;
        let value = raw.decode().unwrap();
        assert_eq!(value.latest_receive_age_us, None);
        assert_eq!(value.latest_progress_age_us, Some(0));
        let path = value.paths[0].unwrap();
        assert_eq!(path.oldest_pending_age_us, None);
        assert_eq!(path.tail_pending_age_us, Some(0));
        assert_eq!(path.highest_ack_age_us, None);
        assert_eq!(path.tail_ack_eliciting, TailAckEliciting::Unknown);
        assert!(value.paths[1..].iter().all(Option::is_none));
        for count in [9, u64::MAX] {
            assert!(CTransportSnapshot {
                schema_version: 1,
                path_count: count,
                captured_path_count: count,
                ..Default::default()
            }
            .decode()
            .is_none());
        }
        assert!(CTransportSnapshot::default().decode().is_none());
        assert!(CTransportSnapshot {
            schema_version: 2,
            ..Default::default()
        }
        .decode()
        .is_none());
    }

    #[test]
    fn real_c_control_unpublished_and_invalidated_are_unavailable() {
        extern "C" {
            fn dmsg_transport_publish(control: *mut c_void, cnx: *const c_void, now: u64);
        }
        let control = Control(NonNull::new(unsafe { dmsg_control_new() }).unwrap());
        let mut out = CTransportSnapshot::default();
        let read = |out: &mut CTransportSnapshot| unsafe {
            dmsg_control_transport_snapshot(control.0.as_ptr(), out, std::mem::size_of_val(out))
        };
        assert_eq!(read(&mut out), 0);
        unsafe { dmsg_transport_publish(control.0.as_ptr(), std::ptr::null(), 0) };
        out.schema_version = 99;
        assert_eq!(read(&mut out), 0);
        assert_eq!(out.schema_version, 0);
    }

    #[test]
    fn malformed_bounds_flags_and_tail_class_are_rejected() {
        let valid = || {
            let mut raw = CTransportSnapshot {
                schema_version: 1,
                path_count: 1,
                captured_path_count: 1,
                ..Default::default()
            };
            raw.paths[0].present = 1;
            raw
        };
        for field in 0..8 {
            let mut raw = valid();
            match field {
                0 => raw.multipath = 2,
                1 => raw.paths[0].present = 2,
                2 => raw.paths[0].validated = 2,
                3 => raw.paths[0].demoted = 2,
                4 => raw.paths[0].pending_truncated = 2,
                5 => raw.paths[0].tail_ack_eliciting = 3,
                6 => raw.paths[0].pending_count = 65,
                7 => raw.paths[0].slot = 1,
                _ => unreachable!(),
            }
            assert!(raw.decode().is_none(), "malformed field {field}");
        }
        let mut raw = valid();
        raw.path_count = u64::MAX;
        raw.captured_path_count = 8;
        for (i, path) in raw.paths.iter_mut().enumerate() {
            path.slot = i as u64;
        }
        assert!(raw.decode().is_none());
        let mut raw = valid();
        raw.connection_state = u64::MAX;
        assert!(raw.decode().is_none());
    }

    /// The disposable TU includes the production publisher/reader verbatim and
    /// uses real staged, pinned Picoquic structures, not Rust-only decode mocks.
    #[cfg(target_os = "linux")]
    #[test]
    fn real_c_publisher_layout_bounds_read_only_and_concurrent_payload() {
        use std::{path::PathBuf, process::Command};
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let out = PathBuf::from(env!("OUT_DIR"));
        let source = out.join("source");
        assert!(std::fs::read_to_string(source.join(".embedding-stamp"))
            .unwrap()
            .starts_with("83d2bba00355d0eb8e4bcaa77121608acf74e4c7"));
        for sanitize in [false, true] {
            let binary = out.join(format!("transport-snapshot-{sanitize}"));
            let mut compile = Command::new(if sanitize { "clang" } else { "cc" });
            compile.env_clear();
            for key in ["HOME", "PATH", "TMPDIR"] {
                if let Some(value) = std::env::var_os(key) {
                    compile.env(key, value);
                }
            }
            compile.args([
                "-std=gnu11",
                "-O1",
                "-g",
                "-UNDEBUG",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-ffunction-sections",
                "-fdata-sections",
                "-Wl,--gc-sections",
                "-pthread",
            ]);
            if sanitize {
                compile.args([
                    "-fsanitize=address,undefined",
                    "-fno-sanitize-recover=all",
                    "-fno-omit-frame-pointer",
                ]);
            }
            for include in [
                package.join("native"),
                source.join("include"),
                source.join("subprojects/picoquic/picoquic"),
                out.join("build/_deps/picotls-src/include"),
                out.join("openssl-build/install/include"),
            ] {
                compile.arg("-I").arg(include);
            }
            let output = compile
                .arg(package.join("native/tests/transport_snapshot.c"))
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("host C compiler");
            assert!(
                output.status.success(),
                "C fixture compile sanitize={sanitize}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = Command::new(&binary).env_clear().output().unwrap();
            assert!(
                output.status.success(),
                "C fixture sanitize={sanitize}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            eprintln!(
                "sanitize={sanitize}: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );

            // Decode the actual C publication through the private Rust repr(C)
            // layout too, checking every field rather than just ABI byte size.
            let output = Command::new(&binary)
                .arg("--dto")
                .env_clear()
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "C DTO: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.stdout.len(),
                std::mem::size_of::<CTransportSnapshot>()
            );
            let raw = unsafe {
                std::ptr::read_unaligned(output.stdout.as_ptr().cast::<CTransportSnapshot>())
            };
            let value = raw.decode().unwrap();
            assert_eq!(value.schema_version, 1);
            assert_eq!(value.connection_state, 14); // pinned picoquic_state_ready
            assert!(value.multipath);
            assert_eq!(value.path_count, 8);
            assert_eq!(value.captured_path_count, 8);
            assert_eq!(value.now_native_us, 1042);
            assert_eq!(value.packets_received, 43);
            assert_eq!(value.data_sent, 44);
            assert_eq!(value.data_received, 45);
            assert_eq!(value.latest_receive_age_us, Some(1040));
            assert_eq!(value.latest_progress_age_us, Some(1039));
            for (i, path) in value.paths.into_iter().enumerate() {
                let i = i as u64;
                assert_eq!(
                    path.unwrap(),
                    TransportPathSnapshot {
                        slot: i as u8,
                        validated: i % 2 == 0,
                        demoted: i % 2 != 0,
                        smoothed_rtt_us: 142 + i,
                        base_retransmit_timer_us: 242 + i,
                        consecutive_pto: 342 + i,
                        bytes_in_transit: 442 + i,
                        cwin: 542 + i,
                        pacing_bytes_per_second: 642 + i,
                        send_mtu: 742 + i,
                        pending_count: 2,
                        pending_truncated: false,
                        oldest_pending_age_us: Some(1039 - i),
                        tail_pending_age_us: Some(1038 - i),
                        highest_ack_age_us: Some(1032 - i),
                        last_packet_receive_age_us: Some(1022 - i),
                        tail_ack_eliciting: if i % 2 == 0 {
                            TailAckEliciting::Yes
                        } else {
                            TailAckEliciting::No
                        },
                    }
                );
            }
            let output = Command::new(&binary)
                .arg("--stream-dto")
                .env_clear()
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "C stream DTO: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout.len(), std::mem::size_of::<CStreamSnapshot>());
            let raw = unsafe {
                std::ptr::read_unaligned(output.stdout.as_ptr().cast::<CStreamSnapshot>())
            };
            assert_eq!(
                raw.decode().unwrap(),
                StreamSnapshot {
                    generation: 42,
                    schema_version: 3,
                    now_native_us: 1042,
                    tx_submitted: 124,
                    rx_contiguous: 457,
                    rx_flushed: 387,
                    rx_pending: 70,
                    stream_active: false,
                    send_queue: true,
                    failed: false,
                    local_fin: true,
                    remote_fin: false,
                    remote_stream_credit: 877,
                    rx_highest: 601,
                    tx_ack_prefix_end: Some(0),
                }
            );
        }
    }
}

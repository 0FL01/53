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
    /// 3 QUIC allocation/configuration, 4 invalid pin, 5 listener, 6 connect;
    /// other values are propagated packet-loop errors.
    Failed(i32),
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
extern "C" {
    fn dmsg_control_new() -> *mut c_void;
    fn dmsg_control_free(control: *mut c_void);
    fn dmsg_control_stop(control: *mut c_void);
    fn dmsg_control_status(control: *mut c_void) -> u64;
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

//! Explicit local native gate. The UDP relays count DNS payload bytes, excluding
//! IP/UDP/link overhead. No production resolver is contacted by this fixture.
use super::*;
use std::{
    io::ErrorKind,
    net::UdpSocket,
    process::{Child, Command, Stdio},
    sync::atomic::AtomicU64,
};

#[derive(Default, Debug)]
struct Traffic {
    tx_packets: AtomicU64,
    tx_bytes: AtomicU64,
    rx_packets: AtomicU64,
    rx_bytes: AtomicU64,
}
struct Relay {
    address: SocketAddr,
    traffic: Arc<Traffic>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Relay {
    fn new(upstream: Option<SocketAddr>) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        socket.set_nonblocking(true).unwrap();
        let forward = UdpSocket::bind("127.0.0.1:0").unwrap();
        forward.set_nonblocking(true).unwrap();
        if let Some(a) = upstream {
            forward.connect(a).unwrap();
        }
        let traffic = Arc::new(Traffic::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (stats, cancelled) = (traffic.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut buf = [0u8; 65536];
            let mut peer = None;
            while !cancelled.load(Ordering::Acquire) {
                match socket.recv_from(&mut buf) {
                    Ok((n, a)) => {
                        peer = Some(a);
                        stats.tx_packets.fetch_add(1, Ordering::Relaxed);
                        stats.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                        if upstream.is_some() {
                            forward.send(&buf[..n]).unwrap();
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                    Err(e) => panic!("relay recv: {e}"),
                }
                if upstream.is_some() {
                    match forward.recv(&mut buf) {
                        Ok(n) => {
                            socket.send_to(&buf[..n], peer.unwrap()).unwrap();
                            stats.rx_packets.fetch_add(1, Ordering::Relaxed);
                            stats.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                        // Late connection cleanup can leave a datagram with no live receiver.
                        Err(e) if e.kind() == ErrorKind::ConnectionRefused => (),
                        Err(e) => panic!("relay upstream: {e}"),
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            address,
            traffic,
            stop,
            worker: Some(worker),
        }
    }
    fn packets(&self) -> u64 {
        self.traffic.tx_packets.load(Ordering::Relaxed)
    }
    fn report(&self, scenario: &str) {
        eprintln!("DNS payload {scenario}: {:?}", self.traffic);
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait_state(state: &Mutex<State>, matches: impl Fn(State) -> bool) -> State {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let s = *state.lock().unwrap();
        if matches(s) {
            return s;
        }
        assert!(Instant::now() < deadline, "state timeout: {s:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "explicit native gate: DMSG_TEST_CARRIER points at pinned slipstream-server"]
fn actual_ready_fallback_retention_terminal_and_failed_cycle_traffic() {
    let binary = std::env::var("DMSG_TEST_CARRIER").expect("pinned carrier binary required");
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/slipstream/certs");
    let free = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let mut server = Server(
        Command::new(binary)
            .env_clear()
            .args([
                "--dns-listen-host",
                "127.0.0.1",
                "--dns-listen-port",
                &port.to_string(),
                "--domain",
                "supervisor-fixture.invalid",
                "--target-address",
                "127.0.0.1:9",
                "--cert",
            ])
            .arg(root.join("cert.pem"))
            .arg("--key")
            .arg(root.join("key.pem"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    thread::sleep(Duration::from_millis(200));
    assert!(server.0.try_wait().unwrap().is_none());
    let upstream = format!("127.0.0.1:{port}").parse().unwrap();
    let primary = Relay::new(Some(upstream));
    let backup = Relay::new(Some(upstream));
    let mut profile = Profile {
        domain: "supervisor-fixture.invalid".into(),
        certificate: include_bytes!("../../../vendor/slipstream/certs/cert.pem").to_vec(),
        noise_pubkey: [0; 32],
        resolvers: vec![primary.address],
    };
    let state = start_with_fallback("native-gate", &profile, vec![backup.address]).unwrap();
    wait_state(&state, |s| matches!(s, State::Ready(_)));
    thread::sleep(Duration::from_millis(250));
    assert_eq!(backup.packets(), 0, "primary Ready must not probe backup");
    stop("native-gate").unwrap();
    primary.report("primary Ready");
    backup.report("unused backup");

    let failed_primary = Relay::new(None);
    profile.resolvers = vec![failed_primary.address];
    let state = start_with_fallback("native-gate", &profile, vec![backup.address]).unwrap();
    let endpoint = wait_state(&state, |s| matches!(s, State::Ready(_)));
    let primary_attempt_packets = failed_primary.packets();
    assert!(primary_attempt_packets > 0 && backup.packets() > 0);
    // The profile remains PRIMARY, so repeated commands reuse the working backup.
    assert!(Arc::ptr_eq(
        &state,
        &start_with_fallback("native-gate", &profile, vec![backup.address]).unwrap()
    ));
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(*state.lock().unwrap(), endpoint);
    assert_eq!(failed_primary.packets(), primary_attempt_packets);
    stop("native-gate").unwrap();
    failed_primary.report("exhausted primary attempt");
    backup.report("backup Ready + 1.2s hold");

    let failed_backup = Relay::new(None);
    let failed_primary = Relay::new(None);
    profile.resolvers = vec![failed_primary.address];
    let state = start_with_fallback("native-gate", &profile, vec![failed_backup.address]).unwrap();
    wait_state(&state, |s| s == State::Backoff(1));
    failed_primary.report("failed cycle primary");
    failed_backup.report("failed cycle backup");
    assert!(failed_primary.packets() > 0 && failed_backup.packets() > 0);
    wait_state(&state, |s| s == State::Connecting);
    let deadline = Instant::now() + Duration::from_secs(1);
    let before = failed_primary.packets();
    while failed_primary.packets() == before && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        failed_primary.packets() > before,
        "retry must restart primary"
    );
    let began = Instant::now();
    stop("native-gate").unwrap();
    assert!(began.elapsed() < Duration::from_secs(1));

    profile.certificate = vec![0x30, 0];
    let silent_backup = Relay::new(None);
    let state = start_with_fallback("native-gate", &profile, vec![silent_backup.address]).unwrap();
    wait_state(&state, |s| s == State::Failed(4));
    stop("native-gate").unwrap();
    assert_eq!(
        silent_backup.packets(),
        0,
        "invalid pin is terminal, not fallback"
    );
}

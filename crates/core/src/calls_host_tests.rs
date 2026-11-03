//! Actual localhost msgd/Noise/account/Olm/SRTP integration. Synthetic PCM here
//! checks host routing, not Android microphone, hardware clocks or DNS acceptance.
use super::*;
use crate::live_media::{AudioPort, CaptureTimestamp};
use std::{
    path::PathBuf,
    process::{Child, Command as Process},
    sync::{mpsc as std_mpsc, Barrier},
};

const DER: &[u8] = &[0x30, 0x03, 0x01, 0x01, 0];
const STORE_KEY: [u8; 32] = [67; 32];
struct Server {
    child: Child,
    dir: PathBuf,
    addr: String,
    public: [u8; 32],
    profile: String,
}
impl Server {
    fn binary() -> PathBuf {
        let binary = std::env::var_os("MSGD_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/msgd")
            });
        assert!(
            binary.exists(),
            "build msgd in the matching target directory first"
        );
        binary
    }
    fn start(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dmsg-calls-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("noise-key");
        let generated = Process::new(Self::binary())
            .env_clear()
            .args(["keygen", "--out"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(generated.status.success());
        let hex = String::from_utf8(generated.stdout).unwrap();
        let mut public = [0; 32];
        for (i, b) in public.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        std::fs::write(dir.join("carrier.der"), DER).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let child = Process::new(Self::binary())
            .env_clear()
            .env("DMSG_DOMAIN", "calls.test")
            .env("MSGD_LISTEN", &addr)
            .env("MSGD_DATA_DIR", dir.join("data"))
            .env("MSGD_BLOBS_DIR", dir.join("blobs"))
            .env("MSGCTL_SOCK", dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", key)
            .env("CARRIER_CERT_FILE", dir.join("carrier.der"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(&addr).is_err() || !dir.join("ctl.sock").exists() {
            assert!(Instant::now() < deadline, "msgd startup deadline");
            thread::sleep(Duration::from_millis(20));
        }
        let profile = dmsg_protocol::profile::build(b"calls.test", DER, &public).unwrap();
        Self {
            child,
            dir,
            addr,
            public,
            profile,
        }
    }
    async fn signup(&self, name: &str) -> (String, String) {
        let invitation_file = self.dir.join(format!("{name}.invite"));
        let issued = Process::new(Self::binary())
            .env_clear()
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .args(["msgctl", "invite-issue", "--out-file"])
            .arg(&invitation_file)
            .arg("3600")
            .output()
            .unwrap();
        assert!(issued.status.success());
        let invitation = Zeroizing::new(std::fs::read_to_string(invitation_file).unwrap());
        let db = self.dir.join(format!("{name}.db"));
        let account = crate::signup_direct(
            &self.profile,
            &self.addr,
            &db,
            Some(DER),
            Some(&STORE_KEY),
            name,
            "test password only",
            Some(invitation.trim()),
        )
        .await
        .unwrap();
        (db.to_string_lossy().into_owned(), account.contact_id)
    }
    async fn connect(&self, db: &str) -> DirectTcp {
        let conn = crate::store::open_encrypted(Path::new(db), &STORE_KEY).unwrap();
        let key = Zeroizing::new(crate::store::load_identity(&conn).unwrap().unwrap());
        crate::initiate_with_key(&self.addr, &self.public, b"calls.test", &key)
            .await
            .unwrap()
    }
    async fn pair(&self) -> ((String, String), (String, String)) {
        let a = self.signup("alice").await;
        let b = self.signup("bobby").await;
        let mut ca = Core::open_encrypted(Path::new(&a.0), &STORE_KEY).unwrap();
        let mut cb = Core::open_encrypted(Path::new(&b.0), &STORE_KEY).unwrap();
        let mut ta = self.connect(&a.0).await;
        let mut tb = self.connect(&b.0).await;
        cb.on_reconnect(&mut tb).await.unwrap();
        let (user, contact) = cb.my_account().unwrap();
        let (ed, curve) = cb.identity_keys();
        let qr = crate::contacts::build_qr(&contact, &user, &cb.device_pub(), &ed, &curve).unwrap();
        crate::contacts::invite_from_qr(&ca.conn, &qr).unwrap();
        ca.on_reconnect(&mut ta).await.unwrap();
        cb.fetch_and_decrypt(&mut tb).await.unwrap();
        cb.accept_contact(&a.1).unwrap();
        cb.on_reconnect(&mut tb).await.unwrap();
        // Acceptance is local until the normal contact sync sends DECIDE.
        cb.fetch_and_decrypt(&mut tb).await.unwrap();
        ca.fetch_and_decrypt(&mut ta).await.unwrap();
        ca.call_peer(&b.1)
            .expect("caller has an accepted pinned contact");
        cb.call_peer(&a.1)
            .expect("callee has an accepted pinned contact");
        assert!(
            crate::olm::count(&mut tb, &cb.device_pub()).await.unwrap() > 0,
            "callee published prekeys"
        );
        let server_db = rusqlite::Connection::open_with_flags(
            self.dir.join("data/msgd.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let permissions: i64 = server_db
            .query_row(
                "SELECT count(*) FROM contact_permissions WHERE state='accepted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            permissions > 0,
            "server accepted contact permission must be committed"
        );
        assert!(
            crate::store::load_session(&ca.conn, &b.1)
                .unwrap()
                .is_none(),
            "first call must claim a real prekey"
        );
        (a, b)
    }
    fn restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.child = Process::new(Self::binary())
            .env_clear()
            .env("DMSG_DOMAIN", "calls.test")
            .env("MSGD_LISTEN", &self.addr)
            .env("MSGD_DATA_DIR", self.dir.join("data"))
            .env("MSGD_BLOBS_DIR", self.dir.join("blobs"))
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", self.dir.join("noise-key"))
            .env("CARRIER_CERT_FILE", self.dir.join("carrier.der"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Observer {
    events: std_mpsc::Sender<CallSnapshot>,
    pause: Mutex<Option<(CallPhase, Arc<Barrier>)>>,
}
impl CallObserver for Observer {
    fn changed(&self, snapshot: CallSnapshot) {
        let pause = {
            let mut pause = self.pause.lock().unwrap();
            if pause.as_ref().is_some_and(|(phase, _)| {
                *phase == snapshot.phase && (*phase != CallPhase::Listening || snapshot.available)
            }) {
                pause.take().map(|(_, gate)| gate)
            } else {
                None
            }
        };
        let _ = self.events.send(snapshot);
        if let Some(gate) = pause {
            gate.wait();
        }
    }
}
struct Client {
    calls: Arc<DmsgCalls>,
    observer: Arc<Observer>,
    events: std_mpsc::Receiver<CallSnapshot>,
    offers: Arc<Mutex<Vec<[u8; 32]>>>,
    owner: String,
}
impl Client {
    fn start(server: &Server, owner: &str, pause: Option<(CallPhase, Arc<Barrier>)>) -> Self {
        let (events, receiver) = std_mpsc::channel();
        let observer = Arc::new(Observer {
            events,
            pause: Mutex::new(pause),
        });
        let offers = Arc::new(Mutex::new(Vec::new()));
        Context::load(
            owner.into(),
            Some(Zeroizing::new(STORE_KEY.to_vec())),
            Endpoint::Local {
                address: server.addr.clone(),
                offers: offers.clone(),
            },
        )
        .expect("call actor startup requires persisted profile/account/identity");
        let calls = DmsgCalls::create(
            owner.into(),
            Some(Zeroizing::new(STORE_KEY.to_vec())),
            observer.clone(),
            Endpoint::Local {
                address: server.addr.clone(),
                offers: offers.clone(),
            },
        )
        .unwrap();
        let starting = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(starting.phase, CallPhase::Listening);
        assert!(!starting.available, "connecting is not stopped or ready");
        Self {
            calls,
            observer,
            events: receiver,
            offers,
            owner: owner.into(),
        }
    }
    fn wait(&self, phase: CallPhase) -> CallSnapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut recent = VecDeque::with_capacity(8);
        loop {
            let state = self.events.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("call callback deadline: expected {phase:?}; recent phase/reason/available={recent:?}; {error:?}"));
            if recent.len() == 8 {
                recent.pop_front();
            }
            recent.push_back((state.phase, state.reason, state.available));
            if state.phase == phase {
                return state;
            }
            assert_ne!(
                state.phase,
                CallPhase::Ended,
                "expected {phase:?}; unexpected call end: {:?}; generated offers={}",
                state.reason,
                self.offers.lock().unwrap().len()
            );
        }
    }
    fn pause(&self, phase: CallPhase) -> Arc<Barrier> {
        let gate = Arc::new(Barrier::new(2));
        *self.observer.pause.lock().unwrap() = Some((phase, gate.clone()));
        gate
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.calls.shutdown().unwrap();
    }
}

fn retire(handle: i64) {
    let entry = crate::call_media_handles::get(handle).expect("registered media handle");
    assert!(
        entry.audio.snapshot().stopped,
        "media worker must join before retirement"
    );
    let stats = crate::call_media_handles::retire(handle).expect("joined media handle retirement");
    assert!(stats.stopped);
}
fn audio(handle: i64) -> AudioPort {
    crate::call_media_handles::get(handle)
        .expect("media handle")
        .audio
        .clone()
}
fn pump(ports: &[AudioPort], millis: u64) {
    let start = Instant::now();
    let mut frame = 0;
    while start.elapsed() < Duration::from_millis(millis) {
        let due = start + Duration::from_millis(frame * 10);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        // Explicit synthesized mono16k recording epoch; never hardware evidence.
        let position = frame as i64 * 160;
        let nano_time = 1_000_000_000 + (position + 160) * 62_500;
        for port in ports {
            let _ = port.push_recorded(
                &[300; 160],
                CaptureTimestamp {
                    read_position: position,
                    frame_position: position + 160,
                    nano_time,
                    observed_ns: nano_time,
                },
                Instant::now(),
            );
            let mut sink = [0; 160];
            port.pull(&mut sink);
            port.sink_queued(0);
        }
        frame += 1;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn calls_host_transient_tcp_loss_rekeys_same_call_and_resumes_real_media() {
    let mut server = Server::start("recover-media");
    let (alice, bobby) = server.pair().await;
    let upstream = server.addr.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_address = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    let (outage, mut changed) = watch::channel(false);
    let proxy = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let mut streams = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    result = changed.changed() => { if result.is_err() { break; } }
                    result = listener.accept() => {
                        let (mut incoming, _) = result.unwrap();
                        if *changed.borrow() { continue; }
                        let upstream = upstream.clone();
                        let mut loss = changed.clone();
                        loss.borrow_and_update();
                        streams.spawn(async move {
                            let mut outgoing = tokio::net::TcpStream::connect(upstream).await.unwrap();
                            tokio::select! {
                                _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing) => (),
                                _ = loss.changed() => (),
                            }
                        });
                    }
                    _ = streams.join_next(), if !streams.is_empty() => (),
                }
            }
            streams.abort_all();
            while streams.join_next().await.is_some() {}
        });
    });
    // Forward complete opaque TCP streams. This fixture cannot forge authenticated
    // commands, media keys, ACKs or feedback, and never stands in for Android DNS.
    let direct = std::mem::replace(&mut server.addr, proxy_address);
    let a = Client::start(&server, &alice.0, None);
    let b = Client::start(&server, &bobby.0, None);
    server.addr = direct;
    assert!(a.wait(CallPhase::Listening).available);
    assert!(b.wait(CallPhase::Listening).available);
    a.calls.dial(bobby.1.clone()).unwrap();
    let outgoing = a.wait(CallPhase::Dialing);
    a.wait(CallPhase::Ringing);
    let incoming = b.wait(CallPhase::Ringing);
    b.calls.accept(incoming.call_id).unwrap();
    let mut am = a.wait(CallPhase::Connecting);
    let mut bm = b.wait(CallPhase::Connecting);
    pump(&[audio(am.media_handle), audio(bm.media_handle)], 1_000);
    a.wait(CallPhase::Active);
    b.wait(CallPhase::Active);
    for _ in 0..2 {
        outage.send_replace(true);
        for client in [&a, &b] {
            let reconnecting = client.wait(CallPhase::Reconnecting);
            assert_eq!(reconnecting.call_id, outgoing.call_id);
            assert_eq!(reconnecting.media_handle, 0);
            assert!(busy(&client.owner));
        }
        for old in [am.media_handle, bm.media_handle] {
            assert!(crate::call_media_handles::retire(old).unwrap().stopped);
            assert!(crate::call_media_handles::get(old).is_none());
        }
        let old = (am.media_epoch, am.media_handle, bm.media_handle);
        outage.send_replace(false);
        am = a.wait(CallPhase::Connecting);
        bm = b.wait(CallPhase::Connecting);
        assert_eq!(am.call_id, outgoing.call_id);
        assert_eq!(bm.call_id, outgoing.call_id);
        assert!(am.media_epoch > old.0);
        assert_eq!(am.media_epoch, bm.media_epoch);
        assert_ne!(am.media_handle, old.1);
        assert_ne!(bm.media_handle, old.2);
        let ports = [audio(am.media_handle), audio(bm.media_handle)];
        pump(&ports, 6_500); // Both endpoints must establish five seconds of real progress.
        a.wait(CallPhase::Active);
        b.wait(CallPhase::Active);
        for port in ports {
            let stats = port.snapshot();
            assert!(
                !stats.failed && !stats.stopped,
                "recovered media failed: {}",
                stats.error
            );
            assert!(stats.encoded_packets > 100 && stats.decoded_packets > 100);
            assert!(stats.received_rtp_packets > 100 && stats.terminal_feedback > 20);
        }
    }
    a.calls.end(outgoing.call_id).unwrap();
    assert_eq!(a.wait(CallPhase::Ended).reason, CallEndReason::LocalEnded);
    assert_eq!(b.wait(CallPhase::Ended).reason, CallEndReason::RemoteEnded);
    retire(am.media_handle);
    retire(bm.media_handle);
    assert!(!busy(&alice.0) && !busy(&bobby.0));
    a.calls.shutdown().unwrap();
    b.calls.shutdown().unwrap();
    for client in [&a, &b] {
        let keys = client.offers.lock().unwrap();
        assert!(keys.len() >= 3);
        let distinct: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "each epoch needs fresh directional key material"
        );
        let core = Core::open_encrypted(Path::new(&client.owner), &STORE_KEY).unwrap();
        let rows: i64 = core
            .conn
            .query_row("SELECT count(*) FROM core_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "recovery must not create messages/history/outbox");
    }
    drop(outage);
    proxy.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn calls_host_real_offer_accept_media_end_redial_and_transport_loss() {
    let mut server = Server::start("media");
    let (alice, bobby) = server.pair().await;
    let a = Client::start(&server, &alice.0, None);
    let b = Client::start(&server, &bobby.0, None);
    assert!(a.wait(CallPhase::Listening).available);
    assert!(b.wait(CallPhase::Listening).available);
    assert!(matches!(
        DmsgCalls::create(
            alice.0.clone(),
            Some(Zeroizing::new(STORE_KEY.to_vec())),
            a.observer.clone(),
            Endpoint::Local {
                address: server.addr.clone(),
                offers: Arc::new(Mutex::new(vec![]))
            }
        ),
        Err(FfiError::Busy)
    ));
    let mut prior_id = String::new();
    for round in 0..2 {
        a.calls.dial(bobby.1.clone()).unwrap();
        assert!(busy(&alice.0));
        let outgoing = a.wait(CallPhase::Dialing);
        a.wait(CallPhase::Ringing);
        let incoming = b.wait(CallPhase::Ringing);
        assert_eq!(incoming.direction, CallDirection::Incoming);
        assert_eq!(incoming.contact_id, alice.1);
        assert_eq!(incoming.call_id, outgoing.call_id);
        assert_ne!(incoming.call_id, prior_id);
        prior_id = incoming.call_id.clone();
        assert!(busy(&bobby.0));
        b.calls.accept(incoming.call_id.clone()).unwrap();
        let am = a.wait(CallPhase::Connecting);
        let bm = b.wait(CallPhase::Connecting);
        assert!(am.media_handle > 0 && bm.media_handle > 0 && am.media_handle != bm.media_handle);
        let ports = [audio(am.media_handle), audio(bm.media_handle)];
        pump(&ports, 1000);
        a.wait(CallPhase::Active);
        b.wait(CallPhase::Active);
        for port in &ports {
            let stats = port.snapshot();
            assert!(stats.encoded_packets >= 5 && stats.decoded_packets >= 5);
            assert!(!stats.failed, "host media failed: {}", stats.error);
        }
        if round == 0 {
            let terminal_gate = b.pause(CallPhase::Ended);
            a.calls.end(outgoing.call_id).unwrap();
            let local = a.wait(CallPhase::Ended);
            let remote = b.wait(CallPhase::Ended);
            // The observer is still blocked, but native stop/handle completion
            // must already be joined. Android may consume Ended later still.
            let stopped_before_retirement = ports[1].snapshot().stopped;
            let joined = crate::call_media_handles::retire(bm.media_handle).unwrap();
            terminal_gate.wait();
            assert_eq!(local.reason, CallEndReason::LocalEnded);
            assert_eq!(remote.reason, CallEndReason::RemoteEnded);
            assert_eq!(remote.media_handle, 0);
            assert!(stopped_before_retirement);
            assert!(joined.stopped);
            assert!(!ports[1].push(&[300; 160]));
            assert_eq!(ports[1].pull(&mut [0; 160]), 0);
        } else {
            server.restart();
            assert_eq!(
                a.wait(CallPhase::Ended).reason,
                CallEndReason::ConnectionLost
            );
            assert_eq!(
                b.wait(CallPhase::Ended).reason,
                CallEndReason::ConnectionLost
            );
        }
        assert!(!busy(&alice.0) && !busy(&bobby.0));
        retire(am.media_handle);
        if round != 0 {
            retire(bm.media_handle);
        }
        a.wait(CallPhase::Listening);
        b.wait(CallPhase::Listening);
    }
    let ak = a.offers.lock().unwrap();
    let bk = b.offers.lock().unwrap();
    assert_eq!((ak.len(), bk.len()), (2, 2));
    assert_ne!(ak[0], ak[1]);
    assert_ne!(bk[0], bk[1]);
    assert_ne!(ak[0], bk[0]);
    assert_ne!(ak[1], bk[1]);
    drop(ak);
    drop(bk);
    for owner in [&alice.0, &bobby.0] {
        let core = Core::open_encrypted(Path::new(owner), &STORE_KEY).unwrap();
        let count: i64 = core
            .conn
            .query_row("SELECT count(*) FROM core_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "calls must not enter message history/outbox");
    }
    a.calls.shutdown().unwrap();
    a.calls.shutdown().unwrap();
    b.calls.shutdown().unwrap();
    assert!(!busy(&a.owner));
    assert!(matches!(a.calls.dial(bobby.1), Err(FfiError::Transport(_))));

    // Drive the same production media-progress and authenticated-event handlers
    // with both failure and JNI retirement present before the authoritative END.
    // The fixture port has no media worker; the real joined contract is above.
    let context = Context::load(
        alice.0.clone(),
        Some(Zeroizing::new(STORE_KEY.to_vec())),
        Endpoint::Local {
            address: server.addr.clone(),
            offers: Arc::new(Mutex::new(vec![])),
        },
    )
    .unwrap();
    let (events, receiver) = std_mpsc::channel();
    let observer = Arc::new(Observer {
        events,
        pause: Mutex::new(None),
    });
    let (_commands, requests) = mpsc::channel(4);
    let shared = a.calls.shared.clone();
    let mut actor = Actor::new(
        Arc::new(context),
        shared.clone(),
        observer,
        requests,
        shared.urgent.subscribe(),
    );
    let id = [91; 16];
    actor.call = Some(Call {
        id,
        epoch: 1,
        generation: shared.admission.lock().unwrap().generation,
        contact: "peer".into(),
        direction: CallDirection::Incoming,
        phase: CallPhase::Connecting,
        peer_user: [92; 16],
        peer_device: [93; 32],
        local: None,
        remote: None,
        deadline: None,
        submitted: true,
        accepted: true,
        ready: true,
        ending: None,
        lanes: None,
        media: None,
        retiring_handle: None,
        local_cipher: None,
        remote_cipher: None,
        recovery_deadline: None,
        stable_since: None,
        stable_sample: None,
        recovered_sent: false,
        bitrate: 12_000,
    });
    // Initialized lanes are not established audio: cold AudioRecord source
    // qualification must not consume the active-media stall sample.
    let startup_at = Instant::now();
    actor.call.as_mut().unwrap().deadline = Some(startup_at + Duration::from_secs(ATTACH_SECS));
    let initialized = crate::live_media::Stats {
        ready: true,
        ..Default::default()
    };
    actor.media_progress(startup_at, &initialized, false).await;
    actor
        .media_progress(startup_at + Duration::from_secs(2), &initialized, false)
        .await;
    assert_eq!(actor.call.as_ref().unwrap().phase, CallPhase::Connecting);
    assert!(actor.call.as_ref().unwrap().stable_sample.is_none());
    assert!(actor.call.as_ref().unwrap().recovery_deadline.is_none());
    assert!(receiver.try_recv().is_err());

    // The physical call can keep authenticating RTP/feedback while every new
    // packet is late: ready/failed alone must not leave silent media Active.
    let episode_start = Instant::now();
    let mut flowing = crate::live_media::Stats {
        ready: true,
        encoded_packets: 26,
        decoded_packets: 10,
        received_rtp_packets: 26,
        terminal_feedback: 5,
        ..Default::default()
    };
    actor.media_progress(episode_start, &flowing, false).await;
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .phase,
        CallPhase::Active
    );
    assert!(actor.call.as_ref().unwrap().deadline.is_none());
    assert!(actor.call.as_ref().unwrap().recovery_deadline.is_none());
    flowing.encoded_packets += 25;
    flowing.received_rtp_packets += 25;
    flowing.terminal_feedback += 5;
    actor
        .media_progress(episode_start + Duration::from_secs(1), &flowing, false)
        .await;
    assert_eq!(actor.call.as_ref().unwrap().phase, CallPhase::Reconnecting);
    let stall_deadline = actor.call.as_ref().unwrap().recovery_deadline.unwrap();
    let stalled = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(stalled.phase, CallPhase::Reconnecting);
    assert_eq!(stalled.media_handle, 0);
    let (port, _capture) = crate::live_media::session::new_port();
    port.finish(Err("controlled native failure before END".into()));
    let mut stats = port.snapshot();
    stats.failure_kind = Some(crate::live_media::session::MediaFailureKind::ProgressExpired);
    assert!(stats.failed && stats.stopped);
    let (handle, entry) = crate::call_media_handles::register(&alice.0, port.audio()).unwrap();
    let retirement = thread::spawn(move || crate::call_media_handles::retire(handle).unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !entry.stop_requested() {
        assert!(Instant::now() < deadline, "JNI retirement did not start");
        thread::yield_now();
    }
    let now = Instant::now();
    actor
        .media_progress(now, &stats, entry.stop_requested())
        .await;
    let ending = actor.call.as_ref().unwrap().ending;
    let resolution_deadline = actor.call.as_ref().unwrap().recovery_deadline.unwrap();
    assert_eq!(
        resolution_deadline, stall_deadline,
        "another failure must not extend the recovery episode"
    );
    let local_end_sent = actor
        .pending
        .iter()
        .any(|p| matches!(p.action, Action::End | Action::Reject));
    let remote_end = Event {
        kind: EventKind::Ended,
        call_id: id,
        epoch: 1,
        peer_user: [92; 16],
        peer_device: [93; 32],
        remaining_ms: 0,
        reason: Reason::RemoteEnded,
        ciphertext: vec![],
    };
    let ended = actor
        .frame(OP_CALL_EVENT, &remote_end.encode().unwrap())
        .await;
    entry.complete(stats);
    assert!(retirement.join().unwrap().stopped);
    ended.unwrap();
    let reconnecting = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(reconnecting.phase, CallPhase::Reconnecting);
    assert_eq!(reconnecting.media_handle, 0);
    let terminal = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(terminal.phase, CallPhase::Ended);
    assert_eq!(terminal.reason, CallEndReason::RemoteEnded);
    assert_eq!(
        ending, None,
        "a known transient retires media, not the logical call"
    );
    assert!(
        resolution_deadline >= episode_start + Duration::from_secs(30)
            && resolution_deadline <= Instant::now() + Duration::from_secs(30)
    );
    assert!(
        !local_end_sent,
        "a transient starts recovery, not a local End"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn calls_host_active_block_joins_media_and_releases_routes() {
    // Exercise the public Core outgoing and FFI incoming block paths. Blocking
    // is terminal, so each case uses its own real accounts and accepted pair.
    for (tag, via_ffi) in [("block-core", false), ("block-ffi", true)] {
        let server = Server::start(tag);
        let (alice, bobby) = server.pair().await;
        let a = Client::start(&server, &alice.0, None);
        let b = Client::start(&server, &bobby.0, None);
        a.wait(CallPhase::Listening);
        b.wait(CallPhase::Listening);
        let mut ca = Core::open_encrypted(Path::new(&alice.0), &STORE_KEY).unwrap();
        let mut cb = Core::open_encrypted(Path::new(&bobby.0), &STORE_KEY).unwrap();
        let mut ta = server.connect(&alice.0).await;
        let mut tb = server.connect(&bobby.0).await;
        ca.login(&mut ta).await.unwrap();
        cb.login(&mut tb).await.unwrap();
        let ffi =
            crate::ffi::DmsgClient::open_encrypted(bobby.0.clone(), STORE_KEY.to_vec()).unwrap();

        a.calls.dial(bobby.1.clone()).unwrap();
        a.wait(CallPhase::Ringing);
        let incoming = b.wait(CallPhase::Ringing);
        b.calls.accept(incoming.call_id).unwrap();
        let am = a.wait(CallPhase::Connecting);
        let bm = b.wait(CallPhase::Connecting);
        let ports = [audio(am.media_handle), audio(bm.media_handle)];
        pump(&ports, 1000);
        a.wait(CallPhase::Active);
        b.wait(CallPhase::Active);
        for port in &ports {
            let stats = port.snapshot();
            assert!(stats.encoded_packets >= 5 && stats.decoded_packets >= 5 && !stats.failed);
        }
        // Raw authenticated FETCH does no contact synchronization, and proves
        // that both devices actually hold server routes before the local block.
        for t in [&mut ta, &mut tb] {
            t.send_frame(dmsg_protocol::OP_FETCH, &[]).await.unwrap();
            assert_eq!(t.recv_frame().await.unwrap(), (OP_ERROR, vec![ERR_BUSY]));
        }
        if via_ffi {
            ffi.contact_block("UNRELATED001".into()).unwrap();
        } else {
            ca.block_contact("UNRELATED001").unwrap();
        }
        pump(&ports, 200);
        assert!(busy(&alice.0) && busy(&bobby.0));
        assert!(ports
            .iter()
            .all(|p| !p.snapshot().stopped && !p.snapshot().failed));
        for (client, active) in [(&a, &am), (&b, &bm)] {
            for update in client.events.try_iter() {
                // Adaptive bitrate may publish another Active snapshot. It must
                // not change the logical call, generation, handle or phase.
                assert_eq!(
                    (
                        update.call_id,
                        update.media_epoch,
                        update.media_handle,
                        update.phase
                    ),
                    (
                        active.call_id.clone(),
                        active.media_epoch,
                        active.media_handle,
                        CallPhase::Active
                    ),
                    "unrelated block must not interrupt the active call"
                );
                assert!(matches!(update.bitrate_bps, 8_000 | 12_000));
            }
        }

        if via_ffi {
            ffi.contact_block(alice.1.clone()).unwrap();
            assert_eq!(b.wait(CallPhase::Ended).reason, CallEndReason::LocalEnded);
            assert_eq!(a.wait(CallPhase::Ended).reason, CallEndReason::RemoteEnded);
            assert_eq!(
                crate::contacts::get(&cb.conn, &alice.1)
                    .unwrap()
                    .unwrap()
                    .state,
                crate::contacts::state::BLOCKED
            );
        } else {
            ca.block_contact(&bobby.1).unwrap();
            assert_eq!(a.wait(CallPhase::Ended).reason, CallEndReason::LocalEnded);
            assert_eq!(b.wait(CallPhase::Ended).reason, CallEndReason::RemoteEnded);
            assert_eq!(
                crate::contacts::get(&ca.conn, &bobby.1)
                    .unwrap()
                    .unwrap()
                    .state,
                crate::contacts::state::BLOCKED
            );
        }
        assert!(!busy(&alice.0) && !busy(&bobby.0));
        retire(am.media_handle);
        retire(bm.media_handle);
        assert!(crate::call_media_handles::get(am.media_handle).is_none());
        assert!(crate::call_media_handles::get(bm.media_handle).is_none());
        a.wait(CallPhase::Listening);
        b.wait(CallPhase::Listening);
        for t in [&mut ta, &mut tb] {
            t.send_frame(dmsg_protocol::OP_FETCH, &[]).await.unwrap();
            let (op, payload) = t.recv_frame().await.unwrap();
            assert_eq!(
                op,
                dmsg_protocol::OP_FETCH_RESP,
                "local block must release server routes without DECIDE sync"
            );
            assert!(dmsg_protocol::mailbox::parse_fetch_resp(&payload)
                .unwrap()
                .is_empty());
        }
        a.calls.shutdown().unwrap();
        b.calls.shutdown().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn calls_host_early_stop_and_full_command_queue_reject() {
    let server = Server::start("urgent");
    let (alice, bobby) = server.pair().await;
    let gate = Arc::new(Barrier::new(2));
    let a = Client::start(
        &server,
        &alice.0,
        Some((CallPhase::Listening, gate.clone())),
    );
    let b = Client::start(&server, &bobby.0, None);
    a.wait(CallPhase::Listening);
    b.wait(CallPhase::Listening);
    let dial = a.calls.dial(bobby.1.clone());
    let stop = a.calls.end(String::new());
    gate.wait();
    dial.unwrap();
    stop.unwrap();
    a.wait(CallPhase::Listening);
    assert!(!busy(&alice.0));
    assert!(
        b.events.recv_timeout(Duration::from_millis(250)).is_err(),
        "cancelled queued dial must not send an Invite"
    );
    assert!(a.offers.lock().unwrap().is_empty());

    let gate = b.pause(CallPhase::Ringing);
    a.calls.dial(bobby.1).unwrap();
    a.wait(CallPhase::Dialing);
    a.wait(CallPhase::Ringing);
    let incoming = b.wait(CallPhase::Ringing);
    let queued: Vec<_> = (0..5)
        .map(|_| b.calls.accept(incoming.call_id.clone()))
        .collect();
    let reject = b.calls.reject(incoming.call_id);
    gate.wait();
    assert!(queued[..4].iter().all(Result::is_ok));
    assert!(matches!(queued[4], Err(FfiError::Busy)));
    reject.unwrap();
    assert_eq!(b.wait(CallPhase::Ended).reason, CallEndReason::Rejected);
    assert_eq!(a.wait(CallPhase::Ended).reason, CallEndReason::Rejected);
    assert!(!busy(&alice.0) && !busy(&bobby.0));
    assert!(
        b.offers.lock().unwrap().is_empty(),
        "urgent reject must beat queued accepts"
    );
    a.calls.shutdown().unwrap();
    b.calls.shutdown().unwrap();
}

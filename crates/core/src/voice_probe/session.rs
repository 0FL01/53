use super::{
    fixture::{self, EndpointFixture, PublicConfig},
    lane::{Commit, SendRequest, OP_RTCP, OP_RTP},
    packet, relay,
};
use dmsg_opus_sys::live::{LiveDecoder, LiveEncoder};
use serde::Serialize;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Default, Serialize)]
pub struct Stats {
    pub encoded_packets: u64,
    pub decoded_packets: u64,
    pub received_rtp_packets: u64,
    pub capture_gap_batches: u64,
    pub fixture_peer_encoded_packets: u64,
    pub fixture_peer_dropped_capture: u64,
    pub fixture_peer_capture_gap_batches: u64,
    pub plc_slots: u64,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub dropped_capture: u64,
    pub dropped_render: u64,
    pub terminal_feedback: u64,
    pub late_packets: u64,
    pub tiny_non_dtx_packets: u64,
    pub max_unconfirmed_bytes: usize,
    pub max_feedback_cycle_ms: u64,
    pub sink_queue_samples: usize,
    pub ready: bool,
    pub dns_carrier: bool,
    pub failed: bool,
    pub stopped: bool,
    pub error: String,
    pub stop_ms: u64,
    pub native_stop_ms: u64,
    pub tx_soft_deadline_packets: u64,
}

struct Captured {
    pcm: [i16; 160],
    len: usize,
    position: u64,
    at: Instant,
}
impl Drop for Captured {
    fn drop(&mut self) {
        self.pcm.zeroize();
    }
}

struct Port {
    input: mpsc::Sender<Captured>,
    position: AtomicU64,
    render: Mutex<VecDeque<i16>>,
    sink_queue: AtomicU64,
    stats: Mutex<Stats>,
    closed: AtomicBool,
}

impl Port {
    fn stats(&self, update: impl FnOnce(&mut Stats)) {
        update(&mut self.stats.lock().expect("probe stats owner"));
    }
    fn clear(&self) {
        let mut render = self.render.lock().expect("probe render owner");
        for sample in render.iter_mut() {
            sample.zeroize();
        }
        render.clear();
    }
}

/// One explicit fixture owner. Its worker and carrier must join before reuse.
pub struct Probe {
    port: Arc<Port>,
    stop: watch::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct AudioPort {
    port: Arc<Port>,
}

impl AudioPort {
    pub(crate) fn push(&self, pcm: &[i16]) -> bool {
        if pcm.is_empty() || pcm.len() > 160 || self.port.closed.load(Ordering::Relaxed) {
            return false;
        }
        let position = self
            .port
            .position
            .fetch_add(pcm.len() as u64, Ordering::Relaxed);
        let mut chunk = Captured {
            pcm: [0; 160],
            len: pcm.len(),
            position,
            at: Instant::now(),
        };
        chunk.pcm[..pcm.len()].copy_from_slice(pcm);
        if self.port.input.try_send(chunk).is_err() {
            self.port
                .stats(|stats| stats.dropped_capture += pcm.len() as u64);
            return false;
        }
        true
    }

    pub(crate) fn pull(&self, pcm: &mut [i16]) -> usize {
        if pcm.is_empty() || pcm.len() > 160 || self.port.closed.load(Ordering::Relaxed) {
            return 0;
        }
        let Ok(mut queue) = self.port.render.try_lock() else {
            return 0;
        };
        let count = pcm.len().min(queue.len());
        for sample in &mut pcm[..count] {
            *sample = queue.pop_front().unwrap();
        }
        count
    }

    pub(crate) fn sink_queued(&self, samples: usize) {
        self.port
            .sink_queue
            .store(samples.min(640) as u64, Ordering::Relaxed);
    }
}

impl Probe {
    pub fn start_local() -> Result<Self, String> {
        Self::start(None)
    }

    pub fn start_dns(path: &Path) -> Result<Self, String> {
        let fixture = EndpointFixture::load(path)?;
        if fixture.carrier.is_none() {
            return Err("DNS probe requires carrier fixture".into());
        }
        Self::start(Some(fixture))
    }

    /// Diagnostic host path; direct TCP is never labelled DNS acceptance.
    pub fn start_fixture(fixture: EndpointFixture) -> Result<Self, String> {
        fixture.validate()?;
        Self::start(Some(fixture))
    }

    fn start(fixture: Option<EndpointFixture>) -> Result<Self, String> {
        let (input, incoming) = mpsc::channel(4); // 40ms PCM maximum
        let port = Arc::new(Port {
            input,
            position: AtomicU64::new(0),
            render: Mutex::new(VecDeque::with_capacity(640)),
            sink_queue: AtomicU64::new(0),
            stats: Mutex::new(Stats::default()),
            closed: AtomicBool::new(false),
        });
        let (stop, cancelled) = watch::channel(false);
        let shared = port.clone();
        let worker = thread::Builder::new()
            .name("m1v-media".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "probe runtime failed".to_string())
                    .and_then(|runtime| {
                        runtime.block_on(run(fixture, shared.clone(), incoming, cancelled))
                    });
                if let Err(error) = result {
                    shared.stats(|stats| {
                        stats.failed = true;
                        stats.error = error;
                    });
                }
                shared.clear();
                shared.closed.store(true, Ordering::Relaxed);
                shared.stats(|stats| stats.stopped = true);
            })
            .map_err(|_| "probe worker failed")?;
        Ok(Self {
            port,
            stop,
            worker: Some(worker),
        })
    }

    /// Nonblocking 10ms PCM transfer. Dropped positions still advance source time.
    pub fn push(&self, pcm: &[i16]) -> bool {
        self.audio().push(pcm)
    }

    pub fn pull(&self, pcm: &mut [i16]) -> usize {
        self.audio().pull(pcm)
    }

    pub fn sink_queued(&self, samples: usize) {
        self.audio().sink_queued(samples)
    }

    pub(crate) fn audio(&self) -> AudioPort {
        AudioPort {
            port: self.port.clone(),
        }
    }

    pub fn snapshot(&self) -> Stats {
        self.port.stats.lock().expect("probe stats owner").clone()
    }

    pub fn stop(&mut self) -> Stats {
        let at = Instant::now();
        self.port.closed.store(true, Ordering::Relaxed);
        let _ = self.stop.send(true);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                self.port.stats(|stats| {
                    stats.failed = true;
                    stats.error = "probe worker panicked".into();
                });
            }
            self.port
                .stats(|stats| stats.stop_ms = at.elapsed().as_millis() as u64);
        }
        self.port.clear();
        self.snapshot()
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

async fn run(
    fixture: Option<EndpointFixture>,
    port: Arc<Port>,
    incoming: mpsc::Receiver<Captured>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    if let Some(fixture) = fixture {
        let mut native = if let Some(carrier) = &fixture.carrier {
            let certificate = fixture::read_private(Path::new(&carrier.certificate_path), 65536)?;
            let resolvers = carrier
                .resolvers
                .iter()
                .map(|address| {
                    address
                        .parse()
                        .map_err(|_| "invalid numeric fixture resolver".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Some(
                slipstream_sys::NativeClient::start(slipstream_sys::Config::new(
                    carrier.domain.clone(),
                    resolvers,
                    certificate.to_vec(),
                ))
                .map_err(|_| "probe carrier unavailable or already owned".to_string())?,
            )
        } else {
            None
        };
        port.stats(|stats| stats.dns_carrier = native.is_some());
        let endpoint_stop = stop.clone();
        let result = async {
            let addr = if let Some(native) = &native {
                let bootstrap = async {
                    loop {
                        match native.status() {
                            slipstream_sys::Status::Ready(endpoint) => {
                                return Ok(endpoint.to_string())
                            }
                            slipstream_sys::Status::Failed(_) | slipstream_sys::Status::Stopped => {
                                return Err("probe carrier bootstrap failed".to_string())
                            }
                            _ => tokio::time::sleep(Duration::from_millis(10)).await,
                        }
                    }
                };
                tokio::time::timeout(Duration::from_secs(30), bootstrap)
                    .await
                    .map_err(|_| "probe carrier bootstrap timed out")??
            } else {
                fixture.relay_addr.clone()
            };
            let media = relay::connect(&addr, &fixture, true).await?;
            let control = relay::connect(&addr, &fixture, false).await?;
            endpoint(
                fixture,
                port.clone(),
                incoming,
                media,
                control,
                endpoint_stop,
            )
            .await
        };
        let outcome =
            tokio::select! { _ = cancelled(&mut stop) => Ok(()), result = result => result };
        if let Some(native) = &mut native {
            let at = Instant::now();
            native.stop().map_err(|_| "probe carrier join failed")?;
            port.stats(|stats| stats.native_stop_ms = at.elapsed().as_millis() as u64);
        }
        outcome
    } else {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "probe local listener failed")?;
        let config = PublicConfig {
            relay_addr: listener
                .local_addr()
                .map_err(|_| "probe local address failed")?
                .to_string(),
            domain: "m1v.fixture".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        };
        let (a, b, relay_fixture, relay_key) = fixture::pair(config)?;
        let addr = a.relay_addr.clone();
        let relay_owner = tokio::spawn(relay::serve(
            listener,
            relay_fixture,
            relay_key,
            stop.clone(),
        ));
        let (peer_input, peer_incoming) = mpsc::channel(4);
        let peer = Arc::new(Port {
            input: peer_input,
            position: AtomicU64::new(0),
            render: Mutex::new(VecDeque::with_capacity(640)),
            sink_queue: AtomicU64::new(0),
            stats: Mutex::new(Stats::default()),
            closed: AtomicBool::new(false),
        });
        let peer_shared = peer.clone();
        let peer_addr = addr.clone();
        let peer_stop = stop.clone();
        let peer_owner = tokio::spawn(async move {
            let media = relay::connect(&peer_addr, &b, true).await?;
            let control = relay::connect(&peer_addr, &b, false).await?;
            endpoint(b, peer_shared, peer_incoming, media, control, peer_stop).await
        });
        let tone_peer = peer.clone();
        let observed = port.clone();
        let mut tone_stop = stop.clone();
        let tone = tokio::spawn(async move {
            let origin = tokio::time::Instant::now();
            let mut timer = tokio::time::interval_at(origin, Duration::from_millis(10));
            // Simulate continuously sampled hardware, not a timer-based VAD.
            // A late callback can still drain recent 10ms capture slots into the
            // bounded 40ms ring. The endpoint rejects aged PCM before encoding;
            // its media admission never flushes a queued audio burst.
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
            loop {
                tokio::select! {
                    _ = cancelled(&mut tone_stop) => break,
                    scheduled = timer.tick() => {
                        // The simulated microphone clock must not slow down when
                        // this runtime misses a tick. Use the exact scheduled slot:
                        // flooring wall-clock jitter can repeat a source position.
                        let position = (scheduled.duration_since(origin).as_micros() as usize / 10_000) * 160;
                        let mut chunk = Captured { pcm: [0; 160], len: 160, position: position as u64, at: scheduled.into_std() };
                        chunk.pcm.copy_from_slice(&super::test_tone(position, 160));
                        if tone_peer.input.try_send(chunk).is_err() {
                            tone_peer.stats(|stats| stats.dropped_capture += 160);
                        }
                        tone_peer.clear(); // no fake remote hardware presentation claim
                        let evidence = tone_peer.stats.lock().expect("fixture peer stats").clone();
                        observed.stats(|stats| {
                            stats.fixture_peer_encoded_packets = evidence.encoded_packets;
                            stats.fixture_peer_dropped_capture = evidence.dropped_capture;
                            stats.fixture_peer_capture_gap_batches = evidence.capture_gap_batches;
                        });
                    }
                }
            }
        });
        let endpoint_stop = stop.clone();
        let result = async {
            let media = relay::connect(&addr, &a, true).await?;
            let control = relay::connect(&addr, &a, false).await?;
            endpoint(a, port, incoming, media, control, endpoint_stop).await
        };
        let outcome =
            tokio::select! { _ = cancelled(&mut stop) => Ok(()), result = result => result };
        tone.abort();
        peer_owner.abort();
        relay_owner.abort();
        let _ = tone.await;
        let _ = peer_owner.await;
        let _ = relay_owner.await;
        peer.clear();
        outcome
    }
}

struct PendingCommit {
    index: u64,
    timestamp: u32,
    opus_bytes: usize,
    submitted: Instant,
    receipt: oneshot::Receiver<Commit>,
}
struct Received {
    timestamp: u64,
    opus: Zeroizing<Vec<u8>>,
}
struct SendCounters {
    timestamp: u32,
    packets: u32,
    octets: u32,
    since_report: bool,
}

fn reap_commits(
    commits: &mut VecDeque<PendingCommit>,
    ledger: &mut packet::Ledger,
    port: &Port,
    samples: usize,
    counters: &mut SendCounters,
) -> Result<(), String> {
    while let Some(pending) = commits.front_mut() {
        match pending.receipt.try_recv() {
            Ok(Commit::Committed { framed_bytes, at }) => {
                ledger.commit(pending.index, framed_bytes, at)?;
                counters.timestamp = pending.timestamp;
                counters.packets = counters.packets.wrapping_add(1);
                counters.octets = counters.octets.wrapping_add(pending.opus_bytes as u32);
                counters.since_report = true;
                let soft_late =
                    at.saturating_duration_since(pending.submitted) > Duration::from_millis(20);
                commits.pop_front();
                port.stats(|stats| {
                    stats.tx_bytes += framed_bytes as u64;
                    stats.max_unconfirmed_bytes = stats.max_unconfirmed_bytes.max(ledger.bytes());
                    stats.tx_soft_deadline_packets += u64::from(soft_late);
                });
            }
            Ok(Commit::Dropped) => {
                commits.pop_front();
                port.stats(|stats| stats.dropped_capture += samples as u64);
            }
            Err(oneshot::error::TryRecvError::Empty) => break,
            Err(_) => return Err("probe commitment owner closed".into()),
        }
    }
    Ok(())
}

async fn endpoint(
    fixture: EndpointFixture,
    port: Arc<Port>,
    mut input: mpsc::Receiver<Captured>,
    mut media: super::lane::Lane,
    mut control: super::lane::Lane,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let profile = fixture::profile(fixture.profile_ms)?;
    let mut encoder = LiveEncoder::new(profile)?;
    let mut decoder = LiveDecoder::new(profile)?;
    let mut sender = dmsg_srtp_sys::Sender::new(&fixture.media_tx, fixture.ssrc_tx)?;
    let mut receiver = dmsg_srtp_sys::Receiver::new(&fixture.media_rx, fixture.ssrc_rx)?;
    let mut ledger = packet::Ledger::new(
        profile.packet_cap(),
        fixture.profile_ms,
        fixture.healthy_cycle_ms,
    );
    let mut budget = packet::Budget::new(
        fixture.capacity_bps,
        profile.packet_cap() + packet::MEDIA_OVERHEAD,
        Instant::now(),
    );
    let mut commits: VecDeque<PendingCommit> = VecDeque::with_capacity(2);
    let mut control_commits: VecDeque<oneshot::Receiver<Commit>> = VecDeque::with_capacity(2);
    let mut pcm = Zeroizing::new(Vec::with_capacity(profile.samples()));
    let mut expected_position = 0u64;
    let mut frame_position = 0u64;
    let mut frame_capture = Instant::now();
    let mut source_index = u64::from(fixture.initial_sequence);
    let mut last_received_index = u64::from(fixture.peer_initial_sequence);
    let mut last_timestamp = u64::from(fixture.peer_initial_timestamp);
    let mut feedback = packet::Feedback::default();
    let mut encoded: VecDeque<Received> = VecDeque::with_capacity(10);
    let mut next_playout: Option<(u64, Instant)> = None;
    let mut ready = false;
    let mut sent = SendCounters {
        since_report: false,
        packets: 0,
        octets: 0,
        timestamp: fixture.initial_timestamp,
    };
    let mut timer = tokio::time::interval(Duration::from_millis(10));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut report_due = Instant::now();
    let mut last_control_submission = Instant::now();

    let outcome = 'session: loop {
        let now = Instant::now();
        if let Err(error) = reap_commits(
            &mut commits,
            &mut ledger,
            &port,
            profile.samples(),
            &mut sent,
        ) {
            break Err(error);
        }
        while let Some(pending) = control_commits.front_mut() {
            match pending.try_recv() {
                Ok(Commit::Committed { framed_bytes, .. }) => {
                    port.stats(|stats| stats.tx_bytes += framed_bytes as u64);
                    control_commits.pop_front();
                }
                Ok(Commit::Dropped) => {
                    control_commits.pop_front();
                }
                Err(oneshot::error::TryRecvError::Empty) => break,
                Err(_) => break 'session Err("probe control commitment owner closed".into()),
            }
        }
        if ready {
            if let Err(error) = ledger.check(now, 0) {
                break Err(error);
            }
            if now.saturating_duration_since(last_control_submission) >= Duration::from_millis(400)
            {
                break Err("control submission expired; retire generation".into());
            }
        }
        tokio::select! {
            _ = cancelled(&mut stop) => break Ok(()),
            packet = control.incoming.recv() => {
                let (op, cipher) = match packet { Some(Ok(frame)) => frame, _ => break Err("probe control lane failed".into()) };
                if op != OP_RTCP { break Err("probe control opcode rejected".into()); }
                let plain = match receiver.unprotect_rtcp(&cipher) { Ok(packet) => packet, Err(error) => break Err(error) };
                let remote = match packet::read_compound(&plain, fixture.ssrc_rx, fixture.ssrc_tx, &fixture.peer_cname) { Ok(report) => report, Err(error) => break Err(error) };
                // A fast peer may ACK while select was waiting. Noise's commitment
                // receipt precedes its socket write; consume it before validation.
                if let Err(error) = reap_commits(&mut commits, &mut ledger, &port, profile.samples(), &mut sent) { break Err(error); }
                if let Some(index) = remote.terminal {
                    match ledger.acknowledge(index, Instant::now()) {
                        Ok(cycle) => port.stats(|stats| { stats.terminal_feedback += 1; stats.max_feedback_cycle_ms = stats.max_feedback_cycle_ms.max(cycle.as_millis() as u64); }),
                        Err(error) => break Err(error),
                    }
                }
                ready = true;
                port.stats(|stats| { stats.ready = true; stats.rx_bytes += (cipher.len() + packet::FRAMING_BYTES) as u64; });
            }
            packet = media.incoming.recv() => {
                let (op, cipher) = match packet { Some(Ok(frame)) => frame, _ => break Err("probe media lane failed".into()) };
                if op != OP_RTP { break Err("probe media opcode rejected".into()); }
                let plain = match receiver.unprotect_rtp(&cipher) { Ok(packet) => Zeroizing::new(packet), Err(error) => break Err(error) };
                let (seq, timestamp, opus) = match packet::read_rtp(&plain, fixture.ssrc_rx) { Ok(packet) => packet, Err(error) => break Err(error) };
                if let Err(error) = dmsg_opus_sys::live::validate_packet(profile, opus) { break Err(error); }
                let index = packet::extend(last_received_index, u64::from(seq), 16);
                let timestamp = packet::extend(last_timestamp, u64::from(timestamp), 32);
                if feedback.terminal.is_some_and(|old| index <= old) { break Err("unordered fixture media frontier".into()); }
                // One immutable ordered source clock keeps the future queue bounded,
                // including across a 32-bit RTP timestamp rollover.
                if timestamp < u64::from(fixture.peer_initial_timestamp)
                    || (timestamp - u64::from(fixture.peer_initial_timestamp)) % u64::from(profile.rtp_ticks()) != 0
                    || (feedback.terminal.is_some() && timestamp <= last_timestamp)
                { break Err("invalid fixture source clock".into()); }
                last_received_index = index;
                last_timestamp = timestamp;
                feedback.terminal = Some(index); // authenticated terminal receipt, even for late drop
                port.stats(|stats| {
                    stats.rx_bytes += (cipher.len() + packet::FRAMING_BYTES) as u64;
                    stats.received_rtp_packets += 1;
                });
                let (cursor, _) = *next_playout.get_or_insert((timestamp, Instant::now() + Duration::from_millis(80)));
                if timestamp < cursor {
                    feedback.late = feedback.late.saturating_add(1);
                    port.stats(|stats| stats.late_packets += 1);
                } else if timestamp - cursor <= 48_000 / 5
                    && encoded.len() < 200 / usize::from(fixture.profile_ms) + 1 {
                    encoded.push_back(Received { timestamp, opus: Zeroizing::new(opus.to_vec()) });
                } else { port.stats(|stats| stats.dropped_render += profile.samples() as u64); }
            }
            captured = input.recv() => {
                let Some(captured) = captured else { break Err("probe capture owner closed".into()); };
                if !ready || captured.at.elapsed() > Duration::from_millis(40) {
                    let discarded = pcm.len();
                    pcm.zeroize(); pcm.clear(); expected_position = captured.position + captured.len as u64;
                    port.stats(|stats| stats.dropped_capture += (captured.len + discarded) as u64);
                    continue;
                }
                if captured.position != expected_position {
                    let discarded = pcm.len();
                    pcm.zeroize(); pcm.clear();
                    port.stats(|stats| { stats.capture_gap_batches += 1; stats.dropped_capture += discarded as u64; });
                }
                expected_position = captured.position + captured.len as u64;
                let mut offset = 0;
                if pcm.is_empty() {
                    let residue = captured.position % profile.samples() as u64;
                    if residue != 0 { offset = (profile.samples() as u64 - residue).min(captured.len as u64) as usize; }
                    frame_position = captured.position + offset as u64;
                    frame_capture = captured.at;
                }
                port.stats(|stats| stats.dropped_capture += offset as u64);
                pcm.extend_from_slice(&captured.pcm[offset..captured.len]);
                if pcm.len() != profile.samples() { continue; }
                let encoded_packet = match encoder.encode(&pcm) { Ok(packet) => packet, Err(error) => break Err(error) };
                pcm.zeroize(); pcm.clear();
                port.stats(|stats| { stats.encoded_packets += 1; if encoded_packet.bytes.len() <= 2 && !encoded_packet.in_dtx { stats.tiny_non_dtx_packets += 1; } });
                feedback.dtx = encoded_packet.in_dtx;
                let timestamp = fixture.initial_timestamp.wrapping_add((frame_position * 3) as u32);
                let bytes = encoded_packet.bytes.len() + packet::MEDIA_OVERHEAD;
                // No catch-up: old capture, a full pending slot or exhausted tokens drops BEFORE SRTP/Noise.
                if frame_capture.elapsed() > Duration::from_millis(u64::from(fixture.profile_ms) + 40)
                    || commits.len() >= 2 || media.outgoing.capacity() == 0 || !budget.admit(bytes, Instant::now())
                { port.stats(|stats| stats.dropped_capture += profile.samples() as u64); continue; }
                let pending_bytes: usize = commits.iter().map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD).sum();
                if let Err(error) = ledger.check(Instant::now(), bytes + pending_bytes) { break Err(error); }
                let cipher = match sender.protect_rtp(&packet::rtp(fixture.ssrc_tx, source_index, timestamp, &encoded_packet.bytes)) { Ok(packet) => packet, Err(error) => break Err(error) };
                let (committed, receipt) = oneshot::channel();
                if media.outgoing.try_send(SendRequest { opcode: OP_RTP, payload: cipher,
                    deadline: Some(Instant::now() + Duration::from_millis(40)), committed: Some(committed) }).is_err()
                { break Err("probe media admission race".into()); }
                commits.push_back(PendingCommit { index: source_index, timestamp, opus_bytes: encoded_packet.bytes.len(), submitted: Instant::now(), receipt });
                source_index += 1;
            }
            _ = timer.tick() => {
                let now = Instant::now();
                if now >= report_due {
                    if control_commits.len() < 2 && control.outgoing.capacity() > 0 && budget.admit(packet::FEEDBACK_FRAMED_MAX, now) {
                        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| "probe wall clock unavailable")?;
                        let ntp = ((wall.as_secs() + 2_208_988_800) << 32) | ((u64::from(wall.subsec_nanos()) << 32) / 1_000_000_000);
                        let compound = packet::compound(packet::Report { sender_ssrc: fixture.ssrc_tx, peer_ssrc: fixture.ssrc_rx, cname: &fixture.cname,
                            sender: sent.since_report.then_some((ntp, sent.timestamp, sent.packets, sent.octets)), feedback });
                        let cipher = sender.protect_rtcp(&compound)?;
                        let (committed, receipt) = oneshot::channel();
                        control.outgoing.try_send(SendRequest { opcode: OP_RTCP, payload: cipher, deadline: Some(now + Duration::from_millis(200)), committed: Some(committed) })
                            .map_err(|_| "probe control admission failed")?;
                        control_commits.push_back(receipt);
                        last_control_submission = now;
                        sent.since_report = false;
                    }
                    report_due = now + Duration::from_millis(200); // coalesce missed ticks
                }
                if let Some((cursor, due)) = next_playout {
                    let sink = port.sink_queue.load(Ordering::Relaxed) as usize;
                    port.stats(|stats| stats.sink_queue_samples = sink);
                    let lead = Duration::from_micros(sink as u64 * 1_000_000 / 16000);
                    // Do not retire a new slot into a full one-frame PCM queue.
                    // AudioTrack lead can otherwise advance decoding while the
                    // preceding native frame is still waiting for the renderer.
                    if now + lead >= due && port.render.lock().expect("probe render owner").is_empty() {
                        while encoded.front().is_some_and(|packet| packet.timestamp < cursor) { encoded.pop_front(); }
                        let output = if encoded.front().is_some_and(|packet| packet.timestamp == cursor) {
                            let packet = encoded.pop_front().unwrap();
                            let pcm = decoder.decode(&packet.opus)?;
                            port.stats(|stats| stats.decoded_packets += 1);
                            pcm
                        } else {
                            feedback.plc = feedback.plc.saturating_add(1);
                            port.stats(|stats| stats.plc_slots += 1);
                            decoder.conceal()?
                        };
                        let mut render = port.render.lock().expect("probe render owner");
                        if render.len() + output.len() <= profile.samples() {
                            render.extend(output);
                        } else { port.stats(|stats| stats.dropped_render += output.len() as u64); }
                        feedback.playout = Some(cursor + u64::from(profile.rtp_ticks()));
                        next_playout = Some((cursor + u64::from(profile.rtp_ticks()), due + Duration::from_millis(u64::from(fixture.profile_ms))));
                    }
                }
            }
        }
    };
    media.joined_close().await;
    control.joined_close().await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_local_pair_and_complete_retirement() {
        let mut probe = Probe::start_local().unwrap();
        let at = Instant::now();
        let mut pcm = [0; 160];
        let mut output = [0; 160];
        let mut position = 0;
        while at.elapsed() < Duration::from_secs(2) {
            pcm.copy_from_slice(&crate::voice_probe::test_tone(position, 160));
            probe.push(&pcm);
            probe.pull(&mut output);
            position += 160;
            thread::sleep(Duration::from_millis(10));
        }
        let stats = probe.stop();
        assert!(!stats.failed, "{}", stats.error);
        assert!(stats.stopped && stats.ready && !stats.dns_carrier);
        assert!(stats.encoded_packets > 20 && stats.decoded_packets > 20);
        assert!(stats.terminal_feedback > 2 && stats.tx_bytes > 2000 && stats.rx_bytes > 2000);
        assert_eq!(probe.pull(&mut output), 0);
        assert!(!probe.push(&pcm));
        assert!(probe.stop().stopped);
    }

    #[test]
    fn model_evidence_uses_actual_linked_library() {
        let evidence = crate::voice_probe::codec_evidence().unwrap();
        assert_eq!(evidence.lookahead_samples, 104);
        assert!(evidence.nolace_changed_samples > 1000 && evidence.deep_plc_changed_samples > 100);
    }
}

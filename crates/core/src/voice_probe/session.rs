use super::{
    fixture::{self, EndpointFixture, PublicConfig},
    lane::{Commit, SendRequest, OP_RTCP, OP_RTP},
    packet, relay,
};
use dmsg_opus_sys::live::{LiveDecoder, LiveEncoder, LiveProfile};
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

/// Fixed early missing-packet preparation allowance, not a render/skip SLA.
pub const DECODE_WRITE_PREPARATION_MS: u64 = 10;
const PREPARATION: Duration = Duration::from_millis(DECODE_WRITE_PREPARATION_MS);
const SAMPLE_NS: u64 = 1_000_000_000 / 16_000;

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
    pub arrival_after_nominal_due_packets: u64,
    pub late_before_nominal_due_packets: u64,
    pub plc_before_nominal_due_slots: u64,
    pub skipped_playout_slots: u64,
    pub expired_render_samples: u64,
    pub decode_after_nominal_due_slots: u64,
    pub max_decode_us: u64,
    pub max_playout_tick_lateness_ms: u64,
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

struct RenderQueue {
    pcm: VecDeque<i16>,
    end_due: Option<Instant>,
}

impl RenderQueue {
    fn new() -> Self {
        Self {
            pcm: VecDeque::with_capacity(640),
            end_due: None,
        }
    }

    fn enqueue(&mut self, due: Instant, pcm: Vec<i16>) {
        debug_assert!(self.pcm.is_empty()); // One decoded frame, including PLC.
        self.end_due = Some(due + Duration::from_nanos(pcm.len() as u64 * SAMPLE_NS));
        self.pcm.extend(pcm);
    }

    // JNI transfers whole batches, not samples at their nominal presentation
    // instants. Expire the remaining frame at its immutable source end, rather
    // than trimming prefixes using the unrelated early-PLC preparation reserve.
    fn expire(&mut self, now: Instant) -> usize {
        let Some(end) = self.end_due else {
            return 0;
        };
        if now < end {
            return 0;
        }
        let count = self.pcm.len();
        self.clear();
        count
    }

    fn pull(&mut self, now: Instant, pcm: &mut [i16]) -> (usize, usize) {
        let expired = self.expire(now);
        let count = pcm.len().min(self.pcm.len());
        for sample in &mut pcm[..count] {
            *sample = self.pcm.pop_front().unwrap();
        }
        if self.pcm.is_empty() {
            self.end_due = None;
        }
        (count, expired)
    }

    fn clear(&mut self) {
        for sample in self.pcm.iter_mut() {
            sample.zeroize();
        }
        self.pcm.clear();
        self.end_due = None;
    }
}

struct Port {
    input: mpsc::Sender<Captured>,
    position: AtomicU64,
    render: Mutex<RenderQueue>,
    sink_queue: AtomicU64,
    stats: Mutex<Stats>,
    closed: AtomicBool,
}

impl Port {
    fn stats(&self, update: impl FnOnce(&mut Stats)) {
        update(&mut self.stats.lock().expect("probe stats owner"));
    }
    fn clear(&self) {
        self.render.lock().expect("probe render owner").clear();
    }
    fn expired_render(&self, samples: usize) {
        if samples != 0 {
            self.stats(|stats| {
                stats.expired_render_samples += samples as u64;
                stats.dropped_render += samples as u64;
            });
        }
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
        let (count, expired) = queue.pull(Instant::now(), pcm);
        drop(queue);
        self.port.expired_render(expired);
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
            render: Mutex::new(RenderQueue::new()),
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
            render: Mutex::new(RenderQueue::new()),
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

struct PlayoutClock {
    cursor: u64,
    due: Instant,
}

impl PlayoutClock {
    fn nominal_due(&self, timestamp: u64) -> Instant {
        let ticks = timestamp.abs_diff(self.cursor);
        let distance = Duration::from_secs(ticks / 48_000)
            + Duration::from_nanos(ticks % 48_000 * 1_000_000_000 / 48_000);
        if timestamp >= self.cursor {
            self.due + distance
        } else {
            self.due - distance
        }
    }

    fn advance(&mut self, slots: u64, profile: LiveProfile) {
        self.cursor += slots * u64::from(profile.rtp_ticks());
        self.due += Duration::from_millis(slots * u64::from(profile.duration_ms()));
    }

    fn skip_expired(&mut self, now: Instant, profile: LiveProfile) -> u64 {
        let Some(elapsed) = now.checked_duration_since(self.due) else {
            return 0;
        };
        // Only wholly elapsed source frames are obsolete. Tick/decode lateness
        // inside the active frame is still accounted, not converted into loss.
        let slots = (elapsed.as_nanos() / (u128::from(profile.duration_ms()) * 1_000_000)) as u64;
        self.advance(slots, profile);
        slots
    }

    fn can_prepare(&self, now: Instant, sink: usize, available: bool) -> bool {
        let lead = Duration::from_nanos(sink as u64 * SAMPLE_NS);
        // Sink lead permits preparing PRESENT audio early. It cannot establish
        // loss. Missing audio gets only the fixed decode/write allowance.
        now + lead >= self.due && (available || now + PREPARATION >= self.due)
    }
}

struct ReceiveState {
    index: u64,
    timestamp: u64,
    encoded: VecDeque<Received>,
    playout: Option<PlayoutClock>,
}

impl ReceiveState {
    // The lane inbox has capacity one. Drain that ready item before a timer
    // can irreversibly conceal it; do not bias/starve capture or control select.
    fn drain_queued(
        &mut self,
        incoming: &mut mpsc::Receiver<Result<(u8, Vec<u8>), String>>,
        receiver: &mut dmsg_srtp_sys::Receiver,
        fixture: &EndpointFixture,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        match incoming.try_recv() {
            Ok(Ok(frame)) => self.receive(frame, receiver, fixture, feedback, port),
            Err(mpsc::error::TryRecvError::Empty) => Ok(()),
            _ => Err("probe media lane failed".into()),
        }
    }

    fn receive(
        &mut self,
        frame: (u8, Vec<u8>),
        receiver: &mut dmsg_srtp_sys::Receiver,
        fixture: &EndpointFixture,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        let (op, cipher) = frame;
        if op != OP_RTP {
            return Err("probe media opcode rejected".into());
        }
        let plain = Zeroizing::new(receiver.unprotect_rtp(&cipher)?);
        let (seq, timestamp, opus) = packet::read_rtp(&plain, fixture.ssrc_rx)?;
        let profile = fixture::profile(fixture.profile_ms)?;
        dmsg_opus_sys::live::validate_packet(profile, opus)?;
        let index = packet::extend(self.index, u64::from(seq), 16);
        let timestamp = packet::extend(self.timestamp, u64::from(timestamp), 32);
        if feedback.terminal.is_some_and(|old| index <= old) {
            return Err("unordered fixture media frontier".into());
        }
        // Authentication and the immutable ordered source clock precede every
        // deadline drop, including across the 32-bit RTP timestamp rollover.
        if timestamp < u64::from(fixture.peer_initial_timestamp)
            || (timestamp - u64::from(fixture.peer_initial_timestamp))
                % u64::from(profile.rtp_ticks())
                != 0
            || (feedback.terminal.is_some() && timestamp <= self.timestamp)
        {
            return Err("invalid fixture source clock".into());
        }
        self.index = index;
        self.timestamp = timestamp;
        feedback.terminal = Some(index); // terminal receipt, even for late drop
        port.stats(|stats| {
            stats.rx_bytes += (cipher.len() + packet::FRAMING_BYTES) as u64;
            stats.received_rtp_packets += 1;
        });
        self.admit(timestamp, opus, Instant::now(), profile, feedback, port);
        Ok(())
    }

    fn admit(
        &mut self,
        timestamp: u64,
        opus: &[u8],
        now: Instant,
        profile: LiveProfile,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) {
        let clock = self.playout.get_or_insert(PlayoutClock {
            cursor: timestamp,
            due: now + Duration::from_millis(80),
        });
        let due = clock.nominal_due(timestamp);
        let after_due = now > due;
        if timestamp < clock.cursor || after_due {
            feedback.late = feedback.late.saturating_add(1);
            port.stats(|stats| {
                stats.late_packets += 1;
                stats.arrival_after_nominal_due_packets += u64::from(after_due);
                stats.late_before_nominal_due_packets += u64::from(now < due);
            });
        } else if timestamp - clock.cursor <= 48_000 / 5
            && self.encoded.len() < 200 / profile.duration_ms() as usize + 1
        {
            self.encoded.push_back(Received {
                timestamp,
                opus: Zeroizing::new(opus.to_vec()),
            });
        } else {
            port.stats(|stats| stats.dropped_render += profile.samples() as u64);
        }
    }

    fn tick(
        &mut self,
        now: Instant,
        profile: LiveProfile,
        decoder: &mut LiveDecoder,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        let tick_started = Instant::now();
        let expired = port.render.lock().expect("probe render owner").expire(now);
        port.expired_render(expired);
        let Some(clock) = &mut self.playout else {
            return Ok(());
        };
        // A renderer stall must not freeze the source baseline. Skip fully elapsed
        // slots without decoding/playing a catch-up burst or rebasing due.
        let skipped = clock.skip_expired(now, profile);
        if skipped != 0 {
            feedback.playout = Some(clock.cursor);
            port.stats(|stats| stats.skipped_playout_slots += skipped);
            // Discard stale predictive state once, not one PLC job per old slot.
            *decoder = LiveDecoder::new(profile)?;
        }
        while self
            .encoded
            .front()
            .is_some_and(|packet| packet.timestamp < clock.cursor)
        {
            self.encoded.pop_front();
            port.stats(|stats| stats.dropped_render += profile.samples() as u64);
        }
        let available = self
            .encoded
            .front()
            .is_some_and(|packet| packet.timestamp == clock.cursor);
        let sink = port.sink_queue.load(Ordering::Relaxed) as usize;
        port.stats(|stats| stats.sink_queue_samples = sink);
        if !clock.can_prepare(now, sink, available)
            || !port
                .render
                .lock()
                .expect("probe render owner")
                .pcm
                .is_empty()
        {
            return Ok(());
        }
        let started = Instant::now();
        let output = if available {
            let packet = self.encoded.pop_front().unwrap();
            let pcm = decoder.decode(&packet.opus)?;
            port.stats(|stats| stats.decoded_packets += 1);
            pcm
        } else {
            feedback.plc = feedback.plc.saturating_add(1);
            port.stats(|stats| {
                stats.plc_slots += 1;
                stats.plc_before_nominal_due_slots += u64::from(now < clock.due);
            });
            decoder.conceal()?
        };
        let elapsed = started.elapsed();
        let completed = now + tick_started.elapsed();
        port.stats(|stats| {
            stats.max_decode_us = stats.max_decode_us.max(elapsed.as_micros() as u64);
            stats.decode_after_nominal_due_slots += u64::from(completed > clock.due);
        });
        let mut render = port.render.lock().expect("probe render owner");
        render.enqueue(clock.due, output);
        let expired = render.expire(completed);
        drop(render);
        port.expired_render(expired);
        clock.advance(1, profile);
        feedback.playout = Some(clock.cursor);
        Ok(())
    }
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
    let mut received = ReceiveState {
        index: u64::from(fixture.peer_initial_sequence),
        timestamp: u64::from(fixture.peer_initial_timestamp),
        encoded: VecDeque::with_capacity(10),
        playout: None,
    };
    let mut feedback = packet::Feedback::default();
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
                let frame = match packet { Some(Ok(frame)) => frame, _ => break Err("probe media lane failed".into()) };
                if let Err(error) = received.receive(frame, &mut receiver, &fixture, &mut feedback, &port) { break Err(error); }
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
            scheduled = timer.tick() => {
                let now = Instant::now();
                port.stats(|stats| stats.max_playout_tick_lateness_ms = stats.max_playout_tick_lateness_ms.max(now.saturating_duration_since(scheduled.into_std()).as_millis() as u64));
                if let Err(error) = received.drain_queued(&mut media.incoming, &mut receiver, &fixture, &mut feedback, &port) { break Err(error); }
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
                if let Err(error) = received.tick(Instant::now(), profile, &mut decoder, &mut feedback, &port) { break Err(error); }
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

    fn test_port() -> Port {
        let (input, _) = mpsc::channel(4);
        Port {
            input,
            position: AtomicU64::new(0),
            render: Mutex::new(RenderQueue::new()),
            sink_queue: AtomicU64::new(0),
            stats: Mutex::new(Stats::default()),
            closed: AtomicBool::new(false),
        }
    }

    fn test_received() -> ReceiveState {
        ReceiveState {
            index: 0,
            timestamp: 0,
            encoded: VecDeque::new(),
            playout: None,
        }
    }

    fn opus40() -> Vec<u8> {
        LiveEncoder::new(LiveProfile::Ms40)
            .unwrap()
            .encode(&super::super::test_tone(0, 640))
            .unwrap()
            .bytes
    }

    #[test]
    fn sink_table_waits_for_missing_packet_then_prepares_present_packet_early() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let mut received = test_received();
        let mut decoder = LiveDecoder::new(profile).unwrap();
        let mut feedback = packet::Feedback::default();
        let opus = opus40();
        received.admit(0, &opus, at(0), profile, &mut feedback, &port);
        received
            .tick(at(80), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        port.clear(); // A has transferred into the real sink model.
        port.sink_queue.store(640, Ordering::Relaxed);
        assert_eq!(received.playout.as_ref().unwrap().due, at(120));

        port.sink_queue.store(480, Ordering::Relaxed); // 30ms at t=90
        received
            .tick(at(90), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
        assert_eq!(port.stats.lock().unwrap().plc_slots, 0);

        received.admit(1920, &opus, at(100), profile, &mut feedback, &port);
        port.sink_queue.store(320, Ordering::Relaxed); // 20ms at t=100
        received
            .tick(at(100), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.decoded_packets, 2);
        assert_eq!(stats.plc_slots, 0);
        assert_eq!(stats.late_packets, 0);
        assert_eq!(stats.arrival_after_nominal_due_packets, 0);
        assert_eq!(received.playout.as_ref().unwrap().due, at(160));
        assert_eq!(port.render.lock().unwrap().end_due, Some(at(160)));
    }

    #[test]
    fn missing_packet_gets_only_fixed_preparation_reserve_and_retires_once() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let mut received = test_received();
        received.playout = Some(PlayoutClock {
            cursor: 1920,
            due: at(120),
        });
        let mut decoder = LiveDecoder::new(profile).unwrap();
        let mut feedback = packet::Feedback::default();
        port.sink_queue.store(640, Ordering::Relaxed);
        for ms in [80, 90, 100, 109] {
            received
                .tick(at(ms), profile, &mut decoder, &mut feedback, &port)
                .unwrap();
            assert_eq!(feedback.plc, 0);
            assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
        }
        port.sink_queue.store(160, Ordering::Relaxed);
        received
            .tick(at(110), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        received
            .tick(at(110), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        assert_eq!(feedback.plc, 1);
        assert_eq!(received.playout.as_ref().unwrap().due, at(160));
        assert_eq!(port.stats.lock().unwrap().plc_before_nominal_due_slots, 1);

        received.admit(1920, &opus40(), at(115), profile, &mut feedback, &port);
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.late_packets, 1);
        assert_eq!(stats.late_before_nominal_due_packets, 1);
        assert_eq!(stats.arrival_after_nominal_due_packets, 0);
        assert!(received.encoded.is_empty());
    }

    #[tokio::test]
    async fn queued_authenticated_media_is_drained_even_if_timer_wins_select() {
        let (local, peer, _, _) = fixture::pair(PublicConfig {
            relay_addr: "127.0.0.1:1".into(),
            domain: "m1v.fixture".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        })
        .unwrap();
        let profile = LiveProfile::Ms40;
        let port = test_port();
        // Synthetic deadline; real runtime wakeup latency must not affect the
        // forced timer-first ordering or this test's packet classification.
        let due = Instant::now() + Duration::from_secs(60);
        let mut received = ReceiveState {
            index: u64::from(local.peer_initial_sequence),
            timestamp: u64::from(local.peer_initial_timestamp),
            encoded: VecDeque::new(),
            playout: Some(PlayoutClock {
                cursor: u64::from(local.peer_initial_timestamp),
                due,
            }),
        };
        let mut sender = dmsg_srtp_sys::Sender::new(&peer.media_tx, peer.ssrc_tx).unwrap();
        let mut receiver = dmsg_srtp_sys::Receiver::new(&local.media_rx, local.ssrc_rx).unwrap();
        let cipher = sender
            .protect_rtp(&packet::rtp(
                peer.ssrc_tx,
                u64::from(peer.initial_sequence),
                peer.initial_timestamp,
                &opus40(),
            ))
            .unwrap();
        let (tx, mut incoming) = mpsc::channel(1);
        tx.try_send(Ok((OP_RTP, cipher))).unwrap();
        let mut timer = tokio::time::interval(Duration::from_millis(10));
        let ready_tick = timer.tick().await;
        let mut feedback = packet::Feedback::default();
        let mut decoder = LiveDecoder::new(profile).unwrap();
        port.sink_queue.store(160, Ordering::Relaxed);
        tokio::select! {
            biased;
            _ = std::future::ready(ready_tick) => {
                received.drain_queued(&mut incoming, &mut receiver, &local, &mut feedback, &port).unwrap();
                received.tick(due - PREPARATION, profile, &mut decoder, &mut feedback, &port).unwrap();
            }
            _ = incoming.recv() => panic!("test must force the timer-first ordering"),
        }
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.received_rtp_packets, 1);
        assert_eq!(stats.decoded_packets, 1);
        assert_eq!(stats.plc_slots, 0);
        assert_eq!(stats.late_packets, 0);
        assert_eq!(feedback.terminal, Some(u64::from(peer.initial_sequence)));
        assert!(matches!(
            incoming.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn renderer_stall_rejects_nominally_late_arrival_and_skips_without_rebase() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let mut received = test_received();
        let mut decoder = LiveDecoder::new(profile).unwrap();
        let mut feedback = packet::Feedback::default();
        let opus = opus40();
        received.admit(0, &opus, at(0), profile, &mut feedback, &port);
        received
            .tick(at(80), profile, &mut decoder, &mut feedback, &port)
            .unwrap(); // native A is left undrained
        received.admit(1920, &opus, at(100), profile, &mut feedback, &port);
        received.admit(3840, &opus, at(161), profile, &mut feedback, &port);
        assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
        assert_eq!(received.encoded.len(), 1); // timely B only, late C rejected
        received
            .tick(at(200), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.arrival_after_nominal_due_packets, 1);
        assert_eq!(stats.late_before_nominal_due_packets, 0);
        assert_eq!(stats.late_packets, 1);
        assert_eq!(stats.skipped_playout_slots, 2);
        assert_eq!(stats.expired_render_samples, 640);
        assert_eq!(stats.dropped_render, 1280); // native A + queued encoded B
        assert_eq!(stats.decoded_packets, 1);
        assert_eq!(stats.plc_slots, 1); // current D only, no PLC jobs for obsolete B/C
        assert!(received.encoded.is_empty());
        let render = port.render.lock().unwrap();
        assert_eq!(render.pcm.len(), 640); // one current frame, not a replay backlog
        assert_eq!(render.end_due, Some(at(240)));
        let clock = received.playout.as_ref().unwrap();
        assert_eq!(clock.cursor, 7680);
        assert_eq!(clock.due, at(240)); // original 80 + 4*40, never now+80
        assert_eq!(feedback.playout, Some(7680));
    }

    #[test]
    fn native_frame_end_deadline_is_immutable_through_partial_pulls() {
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let mut queue = RenderQueue::new();
        queue.enqueue(at(120), (0..640).collect());
        let mut output = [0; 160];
        assert_eq!(queue.pull(at(130), &mut output), (160, 0));
        assert_eq!(output.as_slice(), (0..160).collect::<Vec<i16>>());
        assert_eq!(queue.end_due, Some(at(160)));
        assert_eq!(queue.expire(at(150)), 0);
        assert_eq!(queue.pcm.front(), Some(&160));
        assert_eq!(queue.end_due, Some(at(160)));
        assert_eq!(queue.expire(at(160)), 480);
        assert!(queue.pcm.is_empty());
        assert_eq!(queue.end_due, None);
    }

    #[test]
    fn expired_native_pcm_is_not_returned_by_audio_port() {
        let port = Arc::new(test_port());
        {
            let mut queue = port.render.lock().unwrap();
            queue.enqueue(Instant::now() - Duration::from_millis(200), vec![123; 640]);
        }
        let audio = AudioPort { port: port.clone() };
        let mut output = [77; 160];
        assert_eq!(audio.pull(&mut output), 0);
        assert_eq!(output, [77; 160]);
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.expired_render_samples, 640);
        assert_eq!(stats.dropped_render, 640);
    }

    #[test]
    fn fixed_baseline_and_skip_boundaries_hold_for_all_diagnostic_profiles() {
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            let origin = Instant::now();
            let mut clock = PlayoutClock {
                cursor: u64::from(u32::MAX) - 1000,
                due: origin + Duration::from_millis(80),
            };
            let first_timestamp = clock.cursor;
            let first_due = clock.due;
            let frame = Duration::from_millis(u64::from(profile.duration_ms()));
            assert_eq!(
                clock.skip_expired(first_due + frame - Duration::from_nanos(1), profile),
                0
            );
            assert_eq!(
                clock.skip_expired(
                    first_due + frame * 4 + PREPARATION + Duration::from_nanos(1),
                    profile
                ),
                4
            );
            assert_eq!(
                clock.cursor,
                first_timestamp + u64::from(profile.rtp_ticks()) * 4
            );
            assert_eq!(clock.due, first_due + frame * 4);
            assert_eq!(clock.nominal_due(first_timestamp), first_due);
        }
    }

    #[test]
    fn decode_latency_and_batched_sink_space_do_not_trim_valid_native_pcm() {
        let origin = Instant::now();
        let due = origin + Duration::from_millis(120);
        let mut queue = RenderQueue::new();
        queue.enqueue(due, (0..640).collect());
        // Decode finishes 8.6ms late; the renderer's next poll has 320 samples
        // of sink space. It submits two batches, then one per 10ms of playback.
        let decoded_at = due + Duration::from_micros(8_600);
        assert_eq!(queue.expire(decoded_at), 0);
        let mut output = Vec::new();
        for offset_us in [14_125, 14_125, 24_125, 34_125] {
            let pull_at = due + Duration::from_micros(offset_us);
            let mut batch = [0; 160];
            assert_eq!(queue.pull(pull_at, &mut batch), (160, 0));
            output.extend_from_slice(&batch);
        }
        assert_eq!(output, (0..640).collect::<Vec<i16>>());
        assert!(queue.pcm.is_empty());
    }

    #[test]
    fn delayed_tick_prepares_timely_packet_without_skipping_its_active_frame() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let mut received = test_received();
        let mut decoder = LiveDecoder::new(profile).unwrap();
        let mut feedback = packet::Feedback::default();
        received.admit(0, &opus40(), at(0), profile, &mut feedback, &port);
        received
            .tick(at(96), profile, &mut decoder, &mut feedback, &port)
            .unwrap(); // 16ms timer lateness is within A's 80..120ms frame.
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.skipped_playout_slots, 0);
        assert_eq!(stats.dropped_render, 0);
        assert_eq!(stats.expired_render_samples, 0);
        assert_eq!(stats.decoded_packets, 1);
        assert_eq!(stats.decode_after_nominal_due_slots, 1);
        assert_eq!(stats.plc_slots, 0);
        assert_eq!(stats.late_packets, 0);
        let clock = received.playout.as_ref().unwrap();
        assert_eq!(clock.cursor, 1920);
        assert_eq!(clock.due, at(120));
        let render = port.render.lock().unwrap();
        assert_eq!(render.pcm.len(), 640);
        assert_eq!(render.end_due, Some(at(120))); // no completion-time rebase
    }

    #[test]
    fn seconds_of_renderer_stall_expire_backlog_and_prepare_only_current_slot() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let mut received = test_received();
        let mut decoder = LiveDecoder::new(profile).unwrap();
        let mut feedback = packet::Feedback::default();
        let opus = opus40();
        received.admit(0, &opus, at(0), profile, &mut feedback, &port);
        received
            .tick(at(80), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        // Fill the existing <=200ms future queue while leaving native A pending.
        for slot in 1..=6 {
            received.admit(slot * 1920, &opus, at(100), profile, &mut feedback, &port);
        }
        received
            .tick(at(4_080), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.skipped_playout_slots, 99);
        assert_eq!(stats.expired_render_samples, 640);
        assert_eq!(stats.dropped_render, 7 * 640); // native A and six encoded frames
        assert_eq!(stats.decoded_packets, 1); // no historical packet replay
        assert_eq!(stats.plc_slots, 1); // one current slot, not 99 catch-up jobs
        assert!(received.encoded.is_empty());
        let render = port.render.lock().unwrap();
        assert_eq!(render.pcm.len(), 640);
        assert_eq!(render.end_due, Some(at(4_120)));
        let clock = received.playout.as_ref().unwrap();
        assert_eq!(clock.cursor, 101 * 1920);
        assert_eq!(clock.due, at(4_120));
        assert_eq!(feedback.playout, Some(clock.cursor));
    }

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

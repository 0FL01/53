use super::{
    clock::{self, LocalClock, Rate, RemoteClock},
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
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
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
// Application backlog above the frozen observed initial hardware-age floor.
const CAPTURE_MAX_AGE: Duration = Duration::from_millis(40);
#[cfg(any(target_os = "android", test))]
const CAPTURE_CALIBRATION_BATCHES: u8 = 20;

#[derive(Clone, Serialize)]
pub struct Stats {
    pub encoded_packets: u64,
    pub decoded_packets: u64,
    pub received_rtp_packets: u64,
    pub capture_gap_batches: u64,
    /// Oldest batch sample age at JNI/dequeue; host synthetic pushes aren't hardware evidence.
    pub max_capture_age_us: u64,
    /// Minimum of the initial 20 valid full batches, not proven intrinsic/acoustic latency.
    pub initial_capture_age_floor_us: u64,
    /// Hardware age above the frozen floor, plus native waiting at dequeue.
    pub max_additional_capture_age_us: u64,
    /// Native bracket around Java clock acquisition; bounds pairing uncertainty, not acoustic time.
    pub max_capture_clock_observation_gap_us: u64,
    pub capture_clock_calibrated: bool,
    pub capture_age_unavailable_batches: u64,
    pub capture_age_rejected_batches: u64,
    /// Positive source-vs-producer-handoff advance, epoch = first admitted frame.
    pub max_sender_phase_advance_us: u64,
    /// Positive source-vs-authenticated-core-arrival advance, epoch = first RTP.
    pub max_receiver_phase_advance_us: u64,
    pub future_rejected_packets: u64,
    pub max_future_lead_ms: u64,
    pub remote_clock_ticks: u64,
    pub remote_clock_ns: u64,
    pub remote_clock_calibrated: bool,
    pub remote_clock_valid: bool,
    pub remote_clock_rejected_reports: u64,
    /// Cumulative first-failing clock-check bits; see the private clock module.
    pub remote_clock_rejection_mask: u64,
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

impl Default for Stats {
    fn default() -> Self {
        Self {
            encoded_packets: 0,
            decoded_packets: 0,
            received_rtp_packets: 0,
            capture_gap_batches: 0,
            max_capture_age_us: 0,
            initial_capture_age_floor_us: 0,
            max_additional_capture_age_us: 0,
            max_capture_clock_observation_gap_us: 0,
            capture_clock_calibrated: false,
            capture_age_unavailable_batches: 0,
            capture_age_rejected_batches: 0,
            max_sender_phase_advance_us: 0,
            max_receiver_phase_advance_us: 0,
            future_rejected_packets: 0,
            max_future_lead_ms: 0,
            remote_clock_ticks: 48_000,
            remote_clock_ns: 1_000_000_000,
            remote_clock_calibrated: false,
            remote_clock_valid: false,
            remote_clock_rejected_reports: 0,
            remote_clock_rejection_mask: 0,
            fixture_peer_encoded_packets: 0,
            fixture_peer_dropped_capture: 0,
            fixture_peer_capture_gap_batches: 0,
            plc_slots: 0,
            tx_bytes: 0,
            rx_bytes: 0,
            dropped_capture: 0,
            dropped_render: 0,
            terminal_feedback: 0,
            late_packets: 0,
            arrival_after_nominal_due_packets: 0,
            late_before_nominal_due_packets: 0,
            plc_before_nominal_due_slots: 0,
            skipped_playout_slots: 0,
            expired_render_samples: 0,
            decode_after_nominal_due_slots: 0,
            max_decode_us: 0,
            max_playout_tick_lateness_ms: 0,
            tiny_non_dtx_packets: 0,
            max_unconfirmed_bytes: 0,
            max_feedback_cycle_ms: 0,
            sink_queue_samples: 0,
            ready: false,
            dns_carrier: false,
            failed: false,
            stopped: false,
            error: String::new(),
            stop_ms: 0,
            native_stop_ms: 0,
            tx_soft_deadline_packets: 0,
        }
    }
}

struct Captured {
    pcm: [i16; 160],
    len: usize,
    position: u64,
    // Synthetic source age, or physical additional age above the frozen floor.
    at: Instant,
    pushed_at: Instant,
    // Absolute physical age at handoff stays separate from application admission.
    hardware_age: Option<Duration>,
    capture_clock: Option<CaptureClockSpan>,
}
impl Captured {
    fn source_time(&self) -> Option<Instant> {
        // SR pairs the actual sample clock, not the application-age epoch above
        // its observed floor. Native observation-bracket uncertainty remains.
        self.hardware_age
            .map_or(Some(self.at), |age| self.pushed_at.checked_sub(age))
    }

    fn ages(&self, now: Instant) -> (Duration, Duration) {
        let additional = now.saturating_duration_since(self.at);
        let hardware = self.hardware_age.map_or(additional, |age| {
            age.saturating_add(now.saturating_duration_since(self.pushed_at))
        });
        (hardware, additional)
    }
}
impl Drop for Captured {
    fn drop(&mut self) {
        self.pcm.zeroize();
    }
}

/// AudioRecord frames and Java nanoTime are in the recording's fixed mono16k /
/// TIMEBASE_MONOTONIC epoch. No first-timestamp offset or restart is inferred.
#[cfg(any(target_os = "android", test))]
pub(crate) struct CaptureTimestamp {
    pub read_position: i64, // Oldest sample of this batch, including partial reads.
    pub frame_position: i64,
    pub nano_time: i64,
    pub observed_ns: i64,
}

#[cfg(any(target_os = "android", test))]
impl CaptureTimestamp {
    fn age(&self, position: u64, samples: usize, previous: Option<(i64, i64)>) -> Option<Duration> {
        if self.read_position < 0
            || self.read_position as u64 != position
            || self.frame_position < 0
            || self.nano_time <= 0
            || self.observed_ns < self.nano_time
            || self.observed_ns - self.nano_time > CAPTURE_MAX_AGE.as_nanos() as i64
            || previous.is_some_and(|(frame, time)| {
                self.frame_position < frame
                    || self.nano_time < time
                    || ((self.frame_position == frame) != (self.nano_time == time))
            })
        {
            return None;
        }
        let start = i128::from(self.nano_time)
            + (i128::from(self.read_position) - i128::from(self.frame_position))
                * i128::from(SAMPLE_NS);
        let end = start + samples as i128 * i128::from(SAMPLE_NS);
        // Never clamp a negative age to fresh, extrapolate unread future PCM,
        // or accept an arithmetic wrap as a real recording-clock observation.
        if start < 0 || end > i128::from(self.observed_ns) {
            return None;
        }
        u64::try_from(i128::from(self.observed_ns) - start)
            .ok()
            .map(Duration::from_nanos)
    }
}

// A measured span of validated recording-clock frames, not transport/service
// time. Keep the ratio integral: rounding a sample period accumulates over hours.
#[derive(Clone, Copy)]
struct CaptureClockSpan {
    frames: u64,
    elapsed_ns: u64,
}

impl Default for CaptureClockSpan {
    fn default() -> Self {
        Self {
            frames: 1,
            elapsed_ns: SAMPLE_NS,
        }
    }
}

impl CaptureClockSpan {
    fn duration(self, frames: u64) -> Option<Duration> {
        let ns =
            (u128::from(frames) * u128::from(self.elapsed_ns)).div_ceil(u128::from(self.frames));
        Some(Duration::new(
            u64::try_from(ns / 1_000_000_000).ok()?,
            (ns % 1_000_000_000) as u32,
        ))
    }

    fn slot(self, elapsed: Duration, samples: usize) -> u64 {
        let slot = elapsed.as_nanos().saturating_mul(u128::from(self.frames))
            / (samples as u128 * u128::from(self.elapsed_ns));
        slot.min(u128::from(u64::MAX)) as u64
    }
}

#[cfg(any(target_os = "android", test))]
#[derive(Default)]
struct RecordClock {
    first: Option<(i64, i64)>,
    latest: Option<(i64, i64)>,
    initial_batches: u8,
    initial_floor: Option<Duration>,
}

#[cfg(any(target_os = "android", test))]
impl RecordClock {
    fn span(&self) -> Option<CaptureClockSpan> {
        // The first validated hardware observation is immutable, including
        // across rejected PCM and native/transport stalls. No admission time
        // or producer-handoff interval contributes to this measured ratio.
        let (first_frame, first_ns) = self.first?;
        let (frame, ns) = self.latest?;
        if frame <= first_frame || ns <= first_ns {
            return None;
        }
        Some(CaptureClockSpan {
            frames: (frame - first_frame) as u64,
            elapsed_ns: (ns - first_ns) as u64,
        })
    }

    fn frozen_floor(&self) -> Option<Duration> {
        if self.initial_batches == CAPTURE_CALIBRATION_BATCHES {
            self.initial_floor
        } else {
            None
        }
    }

    fn observe(
        &mut self,
        timestamp: CaptureTimestamp,
        position: u64,
        samples: usize,
    ) -> Option<(Duration, Option<Duration>)> {
        let age = timestamp.age(position, samples, self.latest)?;
        self.latest = Some((timestamp.frame_position, timestamp.nano_time));
        self.first
            .get_or_insert((timestamp.frame_position, timestamp.nano_time));
        // Even observation 20 is startup loss. PCM admission begins with the
        // next valid batch; no buffered calibration audio can be replayed.
        let floor = self.frozen_floor();
        if floor.is_none() && samples == 160 {
            self.initial_floor = Some(self.initial_floor.map_or(age, |floor| floor.min(age)));
            self.initial_batches += 1;
        }
        Some((age, floor))
    }
}

struct SendPhase {
    first: Option<(u64, Instant)>,
}

impl SendPhase {
    fn observe(&mut self, position: u64, pushed_at: Instant) -> u64 {
        let (first_position, first_push) = *self.first.get_or_insert((position, pushed_at));
        let source_ns = u128::from(position.saturating_sub(first_position)) * u128::from(SAMPLE_NS);
        // Compare source progress with the producer's handoff clock, not the
        // backdated hardware sample time (which would conceal a catch-up burst).
        let elapsed_ns = pushed_at.saturating_duration_since(first_push).as_nanos();
        ((source_ns.saturating_sub(elapsed_ns) / 1000).min(u128::from(u64::MAX))) as u64
    }
}

struct RenderQueue {
    pcm: VecDeque<i16>,
    end_due: Option<Instant>,
    source_end: Option<u64>,
}

impl RenderQueue {
    fn new() -> Self {
        Self {
            pcm: VecDeque::with_capacity(640),
            end_due: None,
            source_end: None,
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
            self.source_end = None;
        }
        (count, expired)
    }

    fn clear(&mut self) {
        for sample in self.pcm.iter_mut() {
            sample.zeroize();
        }
        self.pcm.clear();
        self.end_due = None;
        self.source_end = None;
    }
}

struct Port {
    input: mpsc::Sender<Captured>,
    position: AtomicU64,
    render: Mutex<RenderQueue>,
    sink_queue: AtomicU64,
    sink_rate: AtomicI64,
    remote_clock_fresh_until: Mutex<Option<Instant>>,
    stats: Mutex<Stats>,
    closed: AtomicBool,
    #[cfg(any(target_os = "android", test))]
    record_clock: Mutex<RecordClock>,
}

impl Port {
    fn snapshot(&self) -> Stats {
        let fresh_until = *self
            .remote_clock_fresh_until
            .lock()
            .expect("probe clock health owner");
        let mut stats = self.stats.lock().expect("probe stats owner").clone();
        stats.remote_clock_valid &= !stats.stopped
            && !self.closed.load(Ordering::Relaxed)
            && fresh_until.is_some_and(|until| Instant::now() <= until);
        stats
    }

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
        let now = Instant::now();
        self.push_at(pcm, position, now, now, None, None)
    }

    #[cfg(any(target_os = "android", test))]
    // Java's sample is inside this bracket. Pair at completion so acquisition
    // is not added twice; the exported width bounds the remaining uncertainty.
    pub(crate) fn push_recorded_observed(
        &self,
        pcm: &[i16],
        timestamp: CaptureTimestamp,
        observation_started: Instant,
        observation_completed: Instant,
    ) -> bool {
        let gap_us = observation_completed
            .saturating_duration_since(observation_started)
            .as_nanos()
            .div_ceil(1_000)
            .min(u128::from(u64::MAX)) as u64;
        self.port.stats(|stats| {
            stats.max_capture_clock_observation_gap_us =
                stats.max_capture_clock_observation_gap_us.max(gap_us);
        });
        self.push_recorded(pcm, timestamp, observation_completed)
    }

    #[cfg(any(target_os = "android", test))]
    pub(crate) fn push_recorded(
        &self,
        pcm: &[i16],
        timestamp: CaptureTimestamp,
        pushed_at: Instant,
    ) -> bool {
        if pcm.is_empty() || pcm.len() > 160 || self.port.closed.load(Ordering::Relaxed) {
            return false;
        }
        // Every completed physical batch owns its positions, even when age is
        // unknown/expired or the bounded input ring cannot accept it.
        let position = self
            .port
            .position
            .fetch_add(pcm.len() as u64, Ordering::Relaxed);
        let (observation, frozen_floor, capture_clock) = {
            let mut clock = self
                .port
                .record_clock
                .lock()
                .expect("probe capture clock owner");
            let observation = clock.observe(timestamp, position, pcm.len());
            (observation, clock.frozen_floor(), clock.span())
        };
        if let Some(floor) = frozen_floor {
            self.port.stats(|stats| {
                stats.capture_clock_calibrated = true;
                stats.initial_capture_age_floor_us = floor.as_micros() as u64;
            });
        }
        let Some((age, floor)) = observation else {
            self.port.stats(|stats| {
                stats.capture_age_unavailable_batches += 1;
                stats.dropped_capture += pcm.len() as u64;
            });
            return false;
        };
        self.port.stats(|stats| {
            stats.max_capture_age_us = stats.max_capture_age_us.max(age.as_micros() as u64);
        });
        let Some(floor) = floor else {
            self.port
                .stats(|stats| stats.dropped_capture += pcm.len() as u64);
            return false;
        };
        let additional = age.saturating_sub(floor);
        self.port.stats(|stats| {
            stats.max_additional_capture_age_us = stats
                .max_additional_capture_age_us
                .max(additional.as_micros() as u64);
        });
        if additional > CAPTURE_MAX_AGE {
            self.port.stats(|stats| {
                stats.capture_age_rejected_batches += 1;
                stats.dropped_capture += pcm.len() as u64;
            });
            return false;
        }
        let Some(at) = pushed_at.checked_sub(additional) else {
            self.port.stats(|stats| {
                stats.capture_age_unavailable_batches += 1;
                stats.dropped_capture += pcm.len() as u64;
            });
            return false;
        };
        self.push_at(pcm, position, at, pushed_at, Some(age), capture_clock)
    }

    fn push_at(
        &self,
        pcm: &[i16],
        position: u64,
        at: Instant,
        pushed_at: Instant,
        hardware_age: Option<Duration>,
        capture_clock: Option<CaptureClockSpan>,
    ) -> bool {
        let mut chunk = Captured {
            pcm: [0; 160],
            len: pcm.len(),
            position,
            at,
            pushed_at,
            hardware_age,
            capture_clock,
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

    /// Actual sink consumption relative to nominal16k, not a requested actuator.
    pub(crate) fn sink_rate(&self, ppb: i64) -> bool {
        if !(-1_000_000..=1_000_000).contains(&ppb) || self.port.closed.load(Ordering::Relaxed) {
            return false;
        }
        self.port.sink_rate.store(ppb, Ordering::Relaxed);
        true
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
            sink_rate: AtomicI64::new(0),
            remote_clock_fresh_until: Mutex::new(None),
            stats: Mutex::new(Stats::default()),
            closed: AtomicBool::new(false),
            #[cfg(any(target_os = "android", test))]
            record_clock: Mutex::new(RecordClock::default()),
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

    pub fn sink_rate(&self, ppb: i64) -> bool {
        self.audio().sink_rate(ppb)
    }

    pub(crate) fn audio(&self) -> AudioPort {
        AudioPort {
            port: self.port.clone(),
        }
    }

    pub fn snapshot(&self) -> Stats {
        self.port.snapshot()
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
            let mut config = slipstream_sys::Config::new(
                carrier.domain.clone(),
                resolvers,
                certificate.to_vec(),
            );
            config.congestion_control = carrier.congestion_control.native();
            Some(
                slipstream_sys::NativeClient::start(config)
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
            sink_rate: AtomicI64::new(0),
            remote_clock_fresh_until: Mutex::new(None),
            stats: Mutex::new(Stats::default()),
            closed: AtomicBool::new(false),
            #[cfg(any(target_os = "android", test))]
            record_clock: Mutex::new(RecordClock::default()),
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
                        let mut chunk = Captured { pcm: [0; 160], len: 160, position: position as u64, at: scheduled.into_std(), pushed_at: Instant::now(), hardware_age: None, capture_clock: None };
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

struct WaitingPacket {
    opus: Zeroizing<Vec<u8>>,
    position: u64,
    pushed_at: Instant,
    encoded_at: Instant,
    deadline: Instant,
    capture_clock: Option<CaptureClockSpan>,
}

impl WaitingPacket {
    fn new(
        opus: Vec<u8>,
        position: u64,
        captured_at: Instant,
        pushed_at: Instant,
        encoded_at: Instant,
        profile: LiveProfile,
    ) -> Self {
        Self {
            opus: Zeroizing::new(opus),
            position,
            pushed_at,
            encoded_at,
            capture_clock: None,
            // Waiting and Noise admission share these original age limits.
            // Submission cannot give an already encoded packet another 40ms.
            deadline: (encoded_at + CAPTURE_MAX_AGE).min(
                captured_at
                    + Duration::from_millis(u64::from(profile.duration_ms()))
                    + CAPTURE_MAX_AGE,
            ),
        }
    }

    fn bytes(&self) -> usize {
        self.opus.len() + packet::MEDIA_OVERHEAD
    }
}

#[derive(Default)]
struct MediaAdmission {
    // Reserving the EXISTING one-slot lane channel makes this waiting Opus
    // mutually exclusive with a queued Noise-plaintext SendRequest. The lane
    // can still finish its one committed frame and receive in both directions.
    pending: Option<(WaitingPacket, mpsc::OwnedPermit<SendRequest>)>,
    first: Option<(u64, Instant)>,
    last: Option<Instant>,
    capture_clock: CaptureClockSpan,
    next_slot: u64,
}

impl MediaAdmission {
    fn stage(
        &mut self,
        packet: WaitingPacket,
        outgoing: &mpsc::Sender<SendRequest>,
        commits: usize,
        now: Instant,
        profile: LiveProfile,
        port: &Port,
    ) -> Result<(), String> {
        if now >= packet.deadline || self.pending.is_some() || commits >= 2 {
            port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
            return Ok(());
        }
        let permit = match outgoing.clone().try_reserve_owned() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Full(_)) => {
                port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
                return Ok(());
            }
            Err(_) => {
                port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
                return Err("probe media admission owner closed".into());
            }
        };
        if let Some(clock) = packet.capture_clock {
            self.capture_clock = clock;
        }
        self.pending = Some((packet, permit));
        Ok(())
    }

    fn due(&self, position: u64, profile: LiveProfile, now: Instant) -> Instant {
        let Some((first_position, first_at)) = self.first else {
            return now;
        };
        let samples = position.saturating_sub(first_position);
        let source_due = self
            .capture_clock
            .duration(samples)
            .and_then(|elapsed| first_at.checked_add(elapsed));
        // One admission per source-clock slot at the immutable source epoch.
        // Keep consumed slot ordinals across updated hardware spans, including
        // slots occupied by delayed Noise commits. Service jitter must not
        // accumulate into last+duration drift. Byte credit still bounds sends
        // near adjacent slot boundaries; obsolete packets keep their deadline.
        let next_slot = self.last.map_or(self.next_slot, |last| {
            self.next_slot.max(
                self.capture_clock
                    .slot(last.saturating_duration_since(first_at), profile.samples())
                    .saturating_add(1),
            )
        });
        let slot_due = next_slot
            .checked_mul(profile.samples() as u64)
            .and_then(|samples| self.capture_clock.duration(samples))
            .and_then(|elapsed| first_at.checked_add(elapsed));
        match (source_due, slot_due) {
            (Some(source), Some(slot)) => source.max(slot),
            // An unrepresentable projection cannot admit before the existing
            // encode deadline. It does not reset or enlarge that deadline.
            _ => now + CAPTURE_MAX_AGE,
        }
    }

    fn wake_at(
        &self,
        budget: &mut packet::Budget,
        now: Instant,
        profile: LiveProfile,
    ) -> Option<Instant> {
        let (packet, _) = self.pending.as_ref()?;
        Some(
            budget
                .media_ready_at(packet.bytes(), now)
                .map_or(packet.deadline, |credit_due| {
                    credit_due
                        .max(self.due(packet.position, profile, now))
                        .min(packet.deadline)
                }),
        )
    }

    fn take_ready(
        &mut self,
        budget: &mut packet::Budget,
        now: Instant,
        profile: LiveProfile,
        port: &Port,
    ) -> Option<(WaitingPacket, mpsc::OwnedPermit<SendRequest>)> {
        let (packet, _) = self.pending.as_ref()?;
        if now >= packet.deadline {
            self.discard(profile, port);
            return None;
        }
        if now < self.due(packet.position, profile, now) || !budget.admit_media(packet.bytes(), now)
        {
            return None;
        }
        let pending = self.pending.take().unwrap();
        self.first.get_or_insert((pending.0.position, now));
        self.last = Some(now);
        self.next_slot = self.next_slot.max(
            self.capture_clock
                .slot(now.duration_since(self.first.unwrap().1), profile.samples())
                .saturating_add(1),
        );
        Some(pending)
    }

    fn discard(&mut self, profile: LiveProfile, port: &Port) {
        if self.pending.take().is_some() {
            port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
        }
    }

    fn committed(&mut self, at: Instant, samples: usize) {
        // Socket backpressure can postpone a queued packet's Noise commitment.
        // Consume its absolute slot, not another full period of relative delay.
        self.last = Some(self.last.map_or(at, |last| last.max(at)));
        if let Some((_, first_at)) = self.first {
            self.next_slot = self.next_slot.max(
                self.capture_clock
                    .slot(at.saturating_duration_since(first_at), samples)
                    .saturating_add(1),
            );
        }
    }
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

    fn can_prepare(&self, now: Instant, sink: usize, available: bool, ppb: i64) -> bool {
        let lead = clock::sink_lead(sink, ppb);
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
    remote_clock: RemoteClock,
    first_playout: Option<(u64, Instant)>,
}

impl ReceiveState {
    fn source_due(&self, timestamp: u64) -> Instant {
        if !self.remote_clock.calibrated {
            return self.playout.as_ref().unwrap().nominal_due(timestamp);
        }
        let (first, due) = self.first_playout.expect("probe playout source epoch");
        let distance = self.remote_clock.rate.duration(timestamp.abs_diff(first));
        if timestamp >= first {
            due + distance
        } else {
            due - distance
        }
    }

    fn synchronize_clock(&mut self, now: Instant, port: &Port) {
        if let Some(clock) = &self.playout {
            self.first_playout.get_or_insert((clock.cursor, clock.due));
            let due = self.source_due(clock.cursor);
            self.playout.as_mut().unwrap().due = due;
            let mut render = port.render.lock().expect("probe render owner");
            if let Some(timestamp) = render.source_end {
                render.end_due = Some(self.source_due(timestamp));
            }
        }
        *port
            .remote_clock_fresh_until
            .lock()
            .expect("probe clock health owner") = self.remote_clock.fresh_until();
        port.stats(|stats| {
            stats.remote_clock_ticks = self.remote_clock.rate.ticks;
            stats.remote_clock_ns = self.remote_clock.rate.ns;
            stats.remote_clock_calibrated = self.remote_clock.calibrated;
            stats.remote_clock_valid = self.remote_clock.valid(now);
            stats.remote_clock_rejection_mask = self.remote_clock.rejection_mask;
        });
    }

    fn report_clock(&mut self, report: packet::Feedback, now: Instant, port: &Port) {
        if let Some(sender) = report.sender_clock {
            if !self.remote_clock.observe(sender, now) {
                port.stats(|stats| stats.remote_clock_rejected_reports += 1);
            }
        }
        self.synchronize_clock(now, port);
    }

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
        self.playout.get_or_insert(PlayoutClock {
            cursor: timestamp,
            due: now + Duration::from_millis(80),
        });
        self.synchronize_clock(now, port);
        let clock = self.playout.as_ref().unwrap();
        let due = self.source_due(timestamp);
        port.stats(|stats| {
            // due retains the first authenticated arrival +80ms forever.
            stats.max_receiver_phase_advance_us = stats.max_receiver_phase_advance_us.max(
                due.saturating_duration_since(now + Duration::from_millis(80))
                    .as_micros() as u64,
            );
            stats.max_future_lead_ms = stats
                .max_future_lead_ms
                .max((u128::from(timestamp.saturating_sub(clock.cursor)) * 1000 / 48_000) as u64);
        });
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
            port.stats(|stats| {
                stats.future_rejected_packets += 1;
                stats.dropped_render += profile.samples() as u64;
            });
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
        self.synchronize_clock(now, port);
        let expired = port.render.lock().expect("probe render owner").expire(now);
        port.expired_render(expired);
        let Some(clock) = &mut self.playout else {
            return Ok(());
        };
        // A renderer stall must not freeze the source baseline. Skip fully elapsed
        // slots without decoding/playing a catch-up burst or rebasing due.
        let rate = self.remote_clock.rate;
        let (first_timestamp, first_due) = self.first_playout.unwrap();
        let skipped = if self.remote_clock.calibrated {
            let source_now =
                first_timestamp + rate.ticks_at(now.saturating_duration_since(first_due));
            let skipped = source_now.saturating_sub(clock.cursor) / u64::from(profile.rtp_ticks());
            clock.cursor += skipped * u64::from(profile.rtp_ticks());
            clock.due = first_due + rate.duration(clock.cursor - first_timestamp);
            skipped
        } else {
            clock.skip_expired(now, profile)
        };
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
        if !clock.can_prepare(now, sink, available, port.sink_rate.load(Ordering::Relaxed))
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
        let end_timestamp = clock.cursor + u64::from(profile.rtp_ticks());
        render.end_due = Some(first_due + rate.duration(end_timestamp - first_timestamp));
        render.source_end = Some(end_timestamp);
        let expired = render.expire(completed);
        drop(render);
        port.expired_render(expired);
        clock.cursor = end_timestamp;
        clock.due = first_due + rate.duration(end_timestamp - first_timestamp);
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
    admission: &mut MediaAdmission,
    ledger: &mut packet::Ledger,
    port: &Port,
    samples: usize,
    counters: &mut SendCounters,
) -> Result<(), String> {
    while let Some(pending) = commits.front_mut() {
        match pending.receipt.try_recv() {
            Ok(Commit::Committed { framed_bytes, at }) => {
                ledger.commit(pending.index, framed_bytes, at)?;
                admission.committed(at, samples);
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
    let mut admission = MediaAdmission::default();
    let mut control_commits: VecDeque<oneshot::Receiver<Commit>> = VecDeque::with_capacity(2);
    let mut pcm = Zeroizing::new(Vec::with_capacity(profile.samples()));
    let mut expected_position = 0u64;
    let mut frame_position = 0u64;
    let mut frame_capture = Instant::now();
    let mut frame_pushed = frame_capture;
    let mut frame_clock = None;
    let epoch = Instant::now();
    let wall = clock::ntp_from_system_time(std::time::SystemTime::now());
    let mut local_clock = LocalClock::new(epoch, wall, fixture.initial_timestamp);
    let mut sender_phase = SendPhase { first: None };
    let mut source_index = u64::from(fixture.initial_sequence);
    let mut received = ReceiveState {
        index: u64::from(fixture.peer_initial_sequence),
        timestamp: u64::from(fixture.peer_initial_timestamp),
        encoded: VecDeque::with_capacity(10),
        playout: None,
        remote_clock: RemoteClock::default(),
        first_playout: None,
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
            &mut admission,
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
        let media_wake = admission.wake_at(&mut budget, Instant::now(), profile);
        tokio::select! {
            _ = cancelled(&mut stop) => break Ok(()),
            packet = control.incoming.recv() => {
                let (op, cipher) = match packet { Some(Ok(frame)) => frame, _ => break Err("probe control lane failed".into()) };
                if op != OP_RTCP { break Err("probe control opcode rejected".into()); }
                let plain = match receiver.unprotect_rtcp(&cipher) { Ok(packet) => packet, Err(error) => break Err(error) };
                let remote = match packet::read_compound(&plain, fixture.ssrc_rx, fixture.ssrc_tx, &fixture.peer_cname) { Ok(report) => report, Err(error) => break Err(error) };
                // A fast peer may ACK while select was waiting. Noise's commitment
                // receipt precedes its socket write; consume it before validation.
                if let Err(error) = reap_commits(&mut commits, &mut admission, &mut ledger, &port, profile.samples(), &mut sent) { break Err(error); }
                if let Some(index) = remote.terminal {
                    match ledger.acknowledge(index, Instant::now()) {
                        Ok(cycle) => port.stats(|stats| { stats.terminal_feedback += 1; stats.max_feedback_cycle_ms = stats.max_feedback_cycle_ms.max(cycle.as_millis() as u64); }),
                        Err(error) => break Err(error),
                    }
                }
                received.report_clock(remote, Instant::now(), &port);
                ready = true;
                port.stats(|stats| { stats.ready = true; stats.rx_bytes += (cipher.len() + packet::FRAMING_BYTES) as u64; });
            }
            packet = media.incoming.recv() => {
                let frame = match packet { Some(Ok(frame)) => frame, _ => break Err("probe media lane failed".into()) };
                if let Err(error) = received.receive(frame, &mut receiver, &fixture, &mut feedback, &port) { break Err(error); }
            }
            _ = tokio::time::sleep_until(media_wake.unwrap_or(now).into()), if media_wake.is_some() => {
                let now = Instant::now();
                let Some((waiting, permit)) = admission.take_ready(&mut budget, now, profile, &port) else { continue; };
                let bytes = waiting.bytes();
                let pending_bytes: usize = commits.iter().map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD).sum();
                if let Err(error) = ledger.check(now, bytes + pending_bytes) {
                    port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
                    break Err(error);
                }
                let timestamp = fixture.initial_timestamp.wrapping_add((waiting.position * 3) as u32);
                let plain = Zeroizing::new(packet::rtp(fixture.ssrc_tx, source_index, timestamp, &waiting.opus));
                let cipher = match sender.protect_rtp(&plain) {
                    Ok(packet) => packet,
                    Err(error) => {
                        port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
                        break Err(error);
                    }
                };
                let (committed, receipt) = oneshot::channel();
                permit.send(SendRequest { opcode: OP_RTP, payload: cipher,
                    deadline: Some(waiting.deadline), committed: Some(committed) });
                commits.push_back(PendingCommit { index: source_index, timestamp, opus_bytes: waiting.opus.len(), submitted: waiting.encoded_at, receipt });
                let phase = sender_phase.observe(waiting.position, waiting.pushed_at);
                port.stats(|stats| stats.max_sender_phase_advance_us = stats.max_sender_phase_advance_us.max(phase));
                source_index += 1;
            }
            // Leave subsequent PCM in the original bounded capture ring while
            // its sole encoded packet waits. RX/control/cancel remain selectable.
            captured = input.recv(), if admission.pending.is_none() && media.outgoing.capacity() > 0 && commits.len() < 2 => {
                let Some(captured) = captured else { break Err("probe capture owner closed".into()); };
                if let Some(source_at) = captured.source_time() {
                    local_clock.observe(captured.position, source_at, captured.capture_clock.map(|span| Rate { ticks: span.frames * 3, ns: span.elapsed_ns }));
                }
                let (hardware_age, capture_age) = captured.ages(Instant::now());
                port.stats(|stats| {
                    stats.max_capture_age_us = stats.max_capture_age_us.max(hardware_age.as_micros() as u64);
                    if captured.hardware_age.is_some() {
                        stats.max_additional_capture_age_us = stats.max_additional_capture_age_us.max(capture_age.as_micros() as u64);
                    }
                });
                if !ready || capture_age > CAPTURE_MAX_AGE {
                    let discarded = pcm.len();
                    pcm.zeroize(); pcm.clear(); expected_position = captured.position + captured.len as u64;
                    port.stats(|stats| {
                        stats.dropped_capture += (captured.len + discarded) as u64;
                        stats.capture_age_rejected_batches += u64::from(capture_age > CAPTURE_MAX_AGE);
                    });
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
                    frame_pushed = captured.pushed_at;
                    frame_clock = captured.capture_clock;
                }
                port.stats(|stats| stats.dropped_capture += offset as u64);
                pcm.extend_from_slice(&captured.pcm[offset..captured.len]);
                if pcm.len() != profile.samples() { continue; }
                let encoded_packet = match encoder.encode(&pcm) { Ok(packet) => packet, Err(error) => break Err(error) };
                pcm.zeroize(); pcm.clear();
                port.stats(|stats| { stats.encoded_packets += 1; if encoded_packet.bytes.len() <= 2 && !encoded_packet.in_dtx { stats.tiny_non_dtx_packets += 1; } });
                feedback.dtx = encoded_packet.in_dtx;
                let encoded_at = Instant::now();
                let mut waiting = WaitingPacket::new(encoded_packet.bytes, frame_position, frame_capture, frame_pushed, encoded_at, profile);
                waiting.capture_clock = frame_clock;
                if let Err(error) = admission.stage(waiting, &media.outgoing, commits.len(), encoded_at, profile, &port) { break Err(error); }
            }
            scheduled = timer.tick() => {
                let now = Instant::now();
                port.stats(|stats| stats.max_playout_tick_lateness_ms = stats.max_playout_tick_lateness_ms.max(now.saturating_duration_since(scheduled.into_std()).as_millis() as u64));
                if let Err(error) = received.drain_queued(&mut media.incoming, &mut receiver, &fixture, &mut feedback, &port) { break Err(error); }
                if now >= report_due {
                    if control_commits.len() < 2 && control.outgoing.capacity() > 0 && budget.admit_feedback(now) {
                        let compound = packet::compound(packet::Report { sender_ssrc: fixture.ssrc_tx, peer_ssrc: fixture.ssrc_rx, cname: &fixture.cname,
                            sender: sent.since_report.then(|| local_clock.report(now, sent.packets, sent.octets)).flatten().map(|report| (report.ntp, report.rtp, report.packets, report.octets)), feedback });
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
    admission.discard(profile, &port);
    port.stats(|stats| stats.remote_clock_valid = false);
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
            sink_rate: AtomicI64::new(0),
            remote_clock_fresh_until: Mutex::new(None),
            stats: Mutex::new(Stats::default()),
            closed: AtomicBool::new(false),
            record_clock: Mutex::new(RecordClock::default()),
        }
    }

    fn test_received() -> ReceiveState {
        ReceiveState {
            index: 0,
            timestamp: 0,
            encoded: VecDeque::new(),
            playout: None,
            remote_clock: RemoteClock::default(),
            first_playout: None,
        }
    }

    fn opus40() -> Vec<u8> {
        LiveEncoder::new(LiveProfile::Ms40)
            .unwrap()
            .encode(&super::super::test_tone(0, 640))
            .unwrap()
            .bytes
    }

    fn test_audio() -> (AudioPort, mpsc::Receiver<Captured>) {
        let (input, incoming) = mpsc::channel(4);
        let mut port = test_port();
        port.input = input;
        (
            AudioPort {
                port: Arc::new(port),
            },
            incoming,
        )
    }

    fn record_timestamp(
        read_position: i64,
        frame_position: i64,
        observed_ns: i64,
    ) -> CaptureTimestamp {
        CaptureTimestamp {
            read_position,
            frame_position,
            nano_time: 1_000_000_000,
            observed_ns,
        }
    }

    fn aged_record_timestamp(position: u64, age_ms: u64) -> CaptureTimestamp {
        let frame_position = position + age_ms * 16;
        let nano_time = 1_000_000_000 + frame_position * SAMPLE_NS;
        CaptureTimestamp {
            read_position: position as i64,
            frame_position: frame_position as i64,
            nano_time: nano_time as i64,
            observed_ns: nano_time as i64,
        }
    }

    fn calibrate_audio(audio: &AudioPort, ages_ms: &[u64; 20]) {
        for age_ms in ages_ms {
            let position = audio.port.position.load(Ordering::Relaxed);
            assert!(!audio.push_recorded(
                &[1; 160],
                aged_record_timestamp(position, *age_ms),
                Instant::now(),
            ));
        }
        let stats = audio.port.stats.lock().unwrap();
        assert!(stats.capture_clock_calibrated);
        assert_eq!(
            stats.initial_capture_age_floor_us,
            ages_ms.iter().min().unwrap() * 1000
        );
    }

    fn waiting20(position: u64, captured_at: Instant, encoded_at: Instant) -> WaitingPacket {
        WaitingPacket::new(
            vec![7; 30],
            position,
            captured_at,
            encoded_at,
            encoded_at,
            LiveProfile::Ms20,
        )
    }

    #[test]
    fn timely_batched_media_waits_for_source_pacing_instead_of_dropping() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let (outgoing, mut requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        admission
            .stage(
                waiting20(3200, at(0), at(0)),
                &outgoing,
                0,
                at(0),
                profile,
                &port,
            )
            .unwrap();
        let (first, permit) = admission
            .take_ready(&mut budget, at(0), profile, &port)
            .unwrap();
        permit.send(SendRequest {
            opcode: OP_RTP,
            payload: first.opus.to_vec(),
            deadline: Some(first.deadline),
            committed: None,
        });
        requests.try_recv().unwrap(); // The lane takes its committed frame.
        admission
            .stage(
                waiting20(3520, at(1), at(1)),
                &outgoing,
                0,
                at(1),
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(outgoing.capacity(), 0); // The SAME existing slot is reserved.
        assert_eq!(admission.wake_at(&mut budget, at(1), profile), Some(at(20)));
        assert!(admission
            .take_ready(&mut budget, at(19), profile, &port)
            .is_none());
        let (second, permit) = admission
            .take_ready(&mut budget, at(20), profile, &port)
            .unwrap();
        assert_eq!(second.position, 3520);
        assert_eq!(second.encoded_at, at(1));
        assert_eq!(second.deadline, at(41)); // Not submission + 40ms.
        assert_eq!(&*second.opus, &[7; 30]); // The codec output was never retried.
        drop(permit);
        assert_eq!(admission.first, Some((3200, at(0))));
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 0);
    }

    #[test]
    fn late_media_admission_cannot_flush_a_source_catchup_burst() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        for (position, encoded_ms, admitted_ms, credit_due_ns) in [
            (0, 0, 0, None),
            (320, 20, 35, None),
            (640, 36, 49, Some(48_749_205)),
            (960, 50, 68, Some(67_590_707)),
        ] {
            admission
                .stage(
                    waiting20(position, at(encoded_ms), at(encoded_ms)),
                    &outgoing,
                    0,
                    at(encoded_ms),
                    profile,
                    &port,
                )
                .unwrap();
            if position >= 640 {
                // A late admission consumes its grid slot, not a fresh 20ms
                // interval. Here exact byte credit is later than that boundary.
                assert_eq!(
                    admission.due(position, profile, at(encoded_ms)),
                    at(admitted_ms / 20 * 20)
                );
                assert_eq!(
                    admission.wake_at(&mut budget, at(encoded_ms), profile),
                    Some(origin + Duration::from_nanos(credit_due_ns.unwrap()))
                );
                assert!(admission
                    .take_ready(&mut budget, at(admitted_ms - 1), profile, &port)
                    .is_none());
            }
            let (_, permit) = admission
                .take_ready(&mut budget, at(admitted_ms), profile, &port)
                .unwrap();
            drop(permit);
        }
        assert_eq!(admission.first, Some((0, at(0))));
        assert_eq!(admission.due(1600, profile, at(76)), at(100)); // Fixed source epoch.
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 0);
    }

    #[test]
    fn delayed_noise_receipt_paces_the_next_packet_without_rebasing_source() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        admission
            .stage(
                waiting20(0, origin, origin),
                &outgoing,
                0,
                origin,
                profile,
                &port,
            )
            .unwrap();
        let (_, permit) = admission
            .take_ready(&mut budget, origin, profile, &port)
            .unwrap();
        drop(permit);
        let (notify, receipt) = oneshot::channel();
        notify
            .send(Commit::Committed {
                framed_bytes: 74,
                at: at(35),
            })
            .unwrap_or_else(|_| panic!("receipt closed"));
        let mut commits = VecDeque::from([PendingCommit {
            index: 65535,
            timestamp: u32::MAX - 959,
            opus_bytes: 30,
            submitted: origin,
            receipt,
        }]);
        let mut ledger = packet::Ledger::new(50, 20, 600);
        let mut counters = SendCounters {
            timestamp: 0,
            packets: 0,
            octets: 0,
            since_report: false,
        };
        reap_commits(
            &mut commits,
            &mut admission,
            &mut ledger,
            &port,
            320,
            &mut counters,
        )
        .unwrap();
        assert_eq!(ledger.bytes(), 74); // Commitment is not an authenticated ACK.
        assert_eq!(port.stats.lock().unwrap().tx_soft_deadline_packets, 1);
        admission
            .stage(
                waiting20(320, at(20), at(20)),
                &outgoing,
                0,
                at(35),
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(
            admission.wake_at(&mut budget, at(35), profile),
            Some(at(40))
        );
        assert!(admission
            .take_ready(&mut budget, at(39), profile, &port)
            .is_none());
        let (_, permit) = admission
            .take_ready(&mut budget, at(40), profile, &port)
            .unwrap();
        drop(permit);
        assert_eq!(admission.first, Some((0, origin)));
        assert_eq!(admission.due(1920, profile, at(41)), at(120));
        ledger.acknowledge(65535, at(40)).unwrap();
        assert_eq!(ledger.bytes(), 0);
    }

    #[test]
    fn five_hundred_source_slots_do_not_accumulate_one_ms_service_jitter() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        let mut worker_at = origin;
        let mut report_due = origin;
        let mut admitted = 0;
        let mut bytes = 0u128;
        let mut max_capture_wait = Duration::ZERO;
        let mut max_source_lag = Duration::ZERO;
        let mut first_deadline_drop = None;
        for slot in 0..500u64 {
            let captured_at = origin + Duration::from_millis(slot * 20);
            let encoded_at = captured_at.max(worker_at);
            max_capture_wait = max_capture_wait.max(encoded_at.duration_since(captured_at));
            admission
                .stage(
                    waiting20(3200 + slot * 320, captured_at, encoded_at),
                    &outgoing,
                    0,
                    encoded_at,
                    profile,
                    &port,
                )
                .unwrap();
            let due = admission
                .wake_at(&mut budget, encoded_at, profile)
                .unwrap_or(encoded_at);
            // Establish the epoch at slot zero, then service every wake 1ms
            // late and deliver each Noise receipt another 1ms later.
            let now = due + Duration::from_millis(u64::from(slot != 0));
            if now >= report_due {
                assert!(budget.admit_feedback(now));
                bytes += packet::FEEDBACK_FRAMED_MAX as u128;
                report_due = now + Duration::from_millis(200);
            }
            let expired = admission
                .pending
                .as_ref()
                .is_some_and(|(packet, _)| now >= packet.deadline);
            if let Some((waiting, permit)) = admission.take_ready(&mut budget, now, profile, &port)
            {
                assert_eq!(waiting.position, 3200 + slot * 320);
                admitted += 1;
                bytes += waiting.bytes() as u128;
                drop(permit);
                max_source_lag = max_source_lag.max(now.duration_since(captured_at));
                worker_at = now + Duration::from_millis(1);
                admission.committed(worker_at, profile.samples());
            } else {
                assert!(admission.pending.is_none()); // No unbounded waiting or retry.
                if expired && first_deadline_drop.is_none() {
                    first_deadline_drop = Some(slot);
                }
                worker_at = now;
            }
            let elapsed_ns = now.duration_since(origin).as_nanos();
            assert!(
                bytes * 8 * 1_000_000_000 <= (94 + 152) * 8 * 1_000_000_000 + 37_500 * elapsed_ns
            );
            assert_eq!(admission.first, Some((3200, origin)));
        }
        let dropped = port.stats.lock().unwrap().dropped_capture;
        assert_eq!(
            (admitted, dropped, first_deadline_drop),
            (500, 0, None),
            "capture wait {max_capture_wait:?}, source lag {max_source_lag:?}"
        );
        assert_eq!(max_capture_wait, Duration::ZERO);
        assert_eq!(max_source_lag, Duration::from_millis(1));
        assert_eq!(outgoing.capacity(), 1);
    }

    #[test]
    fn forty_five_minutes_of_punctual_capture_skew_do_not_expire_media() {
        let mut evidence = Vec::new();
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            for ppm in [-500i64, -100, 100, 500] {
                let origin = Instant::now();
                let captured = |samples: u64| {
                    origin
                        + Duration::from_nanos(
                            (u128::from(samples) * u128::from(SAMPLE_NS) * 1_000_000
                                / (1_000_000 + ppm) as u128) as u64,
                        )
                };
                let port = test_port();
                let (outgoing, _requests) = mpsc::channel(1);
                let mut admission = MediaAdmission::default();
                let maximum = profile.packet_cap() + packet::MEDIA_OVERHEAD;
                let mut budget = packet::Budget::new(50_000, maximum, origin);
                let mut phase = SendPhase { first: None };
                let mut worker_at = origin;
                let mut report_due = origin;
                let mut admitted = 0u64;
                let mut bytes = 0u128;
                let mut max_wait = Duration::ZERO;
                let mut max_service_lag = Duration::ZERO;
                let mut max_phase = 0;
                let mut first_drop_ms = None;
                let mut clock = RecordClock::default();
                let timestamp = |position: u64| {
                    let frame_position = position + 800;
                    let nano_time = 1_000_000_000
                        + captured(frame_position).duration_since(origin).as_nanos() as i64;
                    CaptureTimestamp {
                        read_position: position as i64,
                        frame_position: frame_position as i64,
                        nano_time,
                        observed_ns: nano_time,
                    }
                };
                for batch in 0..20 {
                    let position = batch * 160;
                    clock.observe(timestamp(position), position, 160).unwrap();
                }
                assert_eq!(clock.frozen_floor(), Some(Duration::from_millis(50)));
                let slots = 45 * 60 * 1000 / u64::from(profile.duration_ms());
                for slot in 0..slots {
                    let position = slot * profile.samples() as u64;
                    let recording_position = 3200 + position;
                    clock
                        .observe(timestamp(recording_position), recording_position, 160)
                        .unwrap();
                    let captured_at = captured(position);
                    // Each 160-sample read is punctual. A complete frame is
                    // available at its last batch, with the actual source clock.
                    let available_at = captured(position + profile.samples() as u64 - 160);
                    let encoded_at = available_at.max(worker_at);
                    max_wait = max_wait.max(encoded_at.duration_since(available_at));
                    let mut waiting = WaitingPacket::new(
                        vec![7; profile.duration_ms() as usize * 3 / 2],
                        position,
                        captured_at,
                        captured_at,
                        encoded_at,
                        profile,
                    );
                    waiting.capture_clock = clock.span();
                    admission
                        .stage(waiting, &outgoing, 0, encoded_at, profile, &port)
                        .unwrap();
                    let due = admission
                        .wake_at(&mut budget, encoded_at, profile)
                        .unwrap_or(encoded_at);
                    let now = due + Duration::from_millis(u64::from(slot != 0));
                    if now >= report_due {
                        assert!(budget.admit_feedback(now));
                        bytes += packet::FEEDBACK_FRAMED_MAX as u128;
                        report_due = now + Duration::from_millis(200);
                    }
                    if let Some((waiting, permit)) =
                        admission.take_ready(&mut budget, now, profile, &port)
                    {
                        admitted += 1;
                        bytes += waiting.bytes() as u128;
                        max_phase = max_phase.max(phase.observe(position, captured_at));
                        max_service_lag = max_service_lag.max(now.duration_since(available_at));
                        drop(permit);
                        worker_at = now + Duration::from_millis(1);
                        admission.committed(worker_at, profile.samples());
                    } else {
                        assert!(admission.pending.is_none());
                        first_drop_ms
                            .get_or_insert_with(|| available_at.duration_since(origin).as_millis());
                        worker_at = now;
                    }
                    assert!(
                        bytes * 8 * 1_000_000_000
                            <= (maximum + packet::FEEDBACK_FRAMED_MAX) as u128 * 8 * 1_000_000_000
                                + 37_500 * now.duration_since(origin).as_nanos()
                    );
                    assert_eq!(outgoing.capacity(), 1);
                    assert_eq!(
                        admission.first,
                        Some((0, captured(profile.samples() as u64 - 160)))
                    );
                }
                let dropped = port.stats.lock().unwrap().dropped_capture;
                assert_eq!(max_wait, Duration::ZERO);
                assert!(max_service_lag < Duration::from_millis(2));
                let last_position = (slots - 1) * profile.samples() as u64;
                let expected_phase = (u128::from(last_position) * u128::from(SAMPLE_NS))
                    .saturating_sub(captured(last_position).duration_since(origin).as_nanos())
                    / 1000;
                assert_eq!(u128::from(max_phase), expected_phase);
                evidence.push((
                    profile.duration_ms(),
                    ppm,
                    slots,
                    admitted,
                    dropped,
                    first_drop_ms,
                    max_wait.as_micros(),
                    max_phase,
                ));
            }
        }
        assert!(
            evidence.iter().all(|row| row.2 == row.3 && row.4 == 0),
            "profile/ppm/slots/admitted/dropped/first-drop-ms/max-wait-us/sender-phase-us: {evidence:?}"
        );
    }

    #[test]
    fn physical_clock_span_survives_queue_wait_and_invalid_observations() {
        let (audio, mut captured) = test_audio();
        let origin = Instant::now();
        let hardware_ns =
            |frame: u64| 1_000_000_000 + (u128::from(frame) * 62_500_000_000 / 1_000_500) as i64;
        let timestamp = |position: u64| CaptureTimestamp {
            read_position: position as i64,
            frame_position: (position + 800) as i64,
            nano_time: hardware_ns(position + 800),
            observed_ns: hardware_ns(position + 800),
        };
        for batch in 0..20 {
            assert!(!audio.push_recorded(&[1; 160], timestamp(batch * 160), origin));
        }
        assert!(audio.push_recorded(&[1; 160], timestamp(3200), origin));
        let chunk = captured.try_recv().unwrap();
        let span = chunk.capture_clock.unwrap();
        let first = audio.port.record_clock.lock().unwrap().first;
        assert_eq!(first, Some((800, hardware_ns(800))));
        assert_eq!(span.frames, 3200);
        assert_eq!(
            span.elapsed_ns,
            (hardware_ns(4000) - hardware_ns(800)) as u64
        );
        let initial = audio.port.record_clock.lock().unwrap().latest;
        let mut invalid = timestamp(3360);
        invalid.observed_ns = -1;
        assert!(!audio.push_recorded(&[1; 160], invalid, origin + Duration::from_millis(10)));
        assert_eq!(audio.port.record_clock.lock().unwrap().first, first);
        assert_eq!(audio.port.record_clock.lock().unwrap().latest, initial);
        assert_eq!(
            chunk.ages(origin + Duration::from_millis(40)).1,
            CAPTURE_MAX_AGE
        );
        assert_eq!(chunk.capture_clock.unwrap().elapsed_ns, span.elapsed_ns);
        assert!(audio.push_recorded(
            &[1; 160],
            timestamp(3520),
            origin + Duration::from_millis(20)
        ));
        let next = captured.try_recv().unwrap();
        let grown = next.capture_clock.unwrap();
        assert_eq!(grown.frames, 3520);
        assert_eq!(audio.port.record_clock.lock().unwrap().first, first);
        assert_eq!(
            audio
                .port
                .stats
                .lock()
                .unwrap()
                .initial_capture_age_floor_us,
            50_000
        );
        assert_eq!(
            audio
                .port
                .stats
                .lock()
                .unwrap()
                .capture_age_unavailable_batches,
            1
        );
        assert_eq!(audio.port.position.load(Ordering::Relaxed), 3680);
    }

    #[test]
    fn physical_source_slots_keep_late_commit_and_backpressure_bounds() {
        for ppm in [-500i64, -100, 100, 500] {
            let profile = LiveProfile::Ms20;
            let samples = profile.samples() as u64;
            let clock = CaptureClockSpan {
                frames: (1_000_000 + ppm) as u64,
                elapsed_ns: 62_500_000_000,
            };
            let origin = Instant::now();
            let at = |slot: u64| origin + clock.duration(slot * samples).unwrap();
            let waiting = |slot: u64, encoded_at| {
                let mut packet = waiting20(slot * samples, at(slot), encoded_at);
                packet.capture_clock = Some(clock);
                packet
            };
            let port = test_port();
            let (outgoing, _requests) = mpsc::channel(1);
            let mut admission = MediaAdmission::default();
            let mut budget = packet::Budget::new(50_000, 94, origin);
            for (slot, encoded_at, admitted_at, committed_at) in [
                (0, origin, origin, origin + Duration::from_millis(1)),
                (
                    1,
                    at(1) + Duration::from_millis(10),
                    at(2) - Duration::from_millis(2),
                    at(3) + Duration::from_millis(5),
                ),
            ] {
                admission
                    .stage(
                        waiting(slot, encoded_at),
                        &outgoing,
                        0,
                        encoded_at,
                        profile,
                        &port,
                    )
                    .unwrap();
                let (packet, permit) = admission
                    .take_ready(&mut budget, admitted_at, profile, &port)
                    .unwrap();
                assert!(committed_at < packet.deadline);
                drop(permit);
                admission.committed(committed_at, profile.samples());
            }
            assert_eq!(admission.due(2 * samples, profile, at(3)), at(4));
            assert_eq!(admission.due(10 * samples, profile, at(3)), at(10));
            let encoded_at = at(3) + Duration::from_millis(6);
            admission
                .stage(
                    waiting(2, encoded_at),
                    &outgoing,
                    0,
                    encoded_at,
                    profile,
                    &port,
                )
                .unwrap();
            assert!(admission
                .take_ready(&mut budget, at(4) - Duration::from_nanos(1), profile, &port)
                .is_none());
            let (_, permit) = admission
                .take_ready(&mut budget, at(4), profile, &port)
                .unwrap();
            drop(permit);
            admission.committed(at(4) + Duration::from_millis(1), profile.samples());
            let encoded_at = at(4) + Duration::from_millis(2);
            admission
                .stage(
                    waiting(3, encoded_at),
                    &outgoing,
                    0,
                    encoded_at,
                    profile,
                    &port,
                )
                .unwrap();
            assert_eq!(outgoing.capacity(), 0);
            let stalled_at = origin + Duration::from_secs(4);
            assert!(admission
                .take_ready(&mut budget, stalled_at, profile, &port)
                .is_none());
            assert_eq!(outgoing.capacity(), 1);
            assert_eq!(port.stats.lock().unwrap().dropped_capture, samples);
            assert_eq!(budget.media_ready_at(94, stalled_at), Some(stalled_at));
            let current_slot = clock.slot(Duration::from_secs(4), profile.samples());
            admission
                .stage(
                    waiting(current_slot, stalled_at),
                    &outgoing,
                    0,
                    stalled_at,
                    profile,
                    &port,
                )
                .unwrap();
            let (_, permit) = admission
                .take_ready(&mut budget, stalled_at, profile, &port)
                .unwrap();
            drop(permit);
            assert_eq!(admission.first, Some((0, origin)));
            assert_eq!(
                admission.due((current_slot + 1) * samples, profile, stalled_at),
                at(current_slot + 1)
            );
            // The extended hardware span has the same ratio and cannot rebase
            // source slots to the late service/receipt time.
            let next_slot = admission.next_slot;
            let mut next = waiting(current_slot + 1, at(current_slot + 1));
            next.capture_clock = Some(CaptureClockSpan {
                frames: clock.frames * 2,
                elapsed_ns: clock.elapsed_ns * 2,
            });
            let next_at = at(current_slot + 1);
            let credit_at = budget.media_ready_at(next.bytes(), next_at).unwrap();
            admission
                .stage(next, &outgoing, 0, next_at, profile, &port)
                .unwrap();
            assert_eq!(admission.next_slot, next_slot);
            assert_eq!(
                admission.wake_at(&mut budget, next_at, profile),
                Some(next_at.max(credit_at))
            );
            let (_, permit) = admission
                .take_ready(&mut budget, next_at.max(credit_at), profile, &port)
                .unwrap();
            drop(permit);
            assert_eq!(admission.first, Some((0, origin)));
            assert_eq!(port.stats.lock().unwrap().dropped_capture, samples);
        }
    }

    #[test]
    fn delayed_commit_across_two_grid_boundaries_consumes_its_absolute_slot() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        for (position, encoded_ms, admitted_ms, committed_ms) in [(0, 0, 0, 1), (320, 30, 39, 69)] {
            admission
                .stage(
                    waiting20(position, at(encoded_ms), at(encoded_ms)),
                    &outgoing,
                    0,
                    at(encoded_ms),
                    profile,
                    &port,
                )
                .unwrap();
            let (waiting, permit) = admission
                .take_ready(&mut budget, at(admitted_ms), profile, &port)
                .unwrap();
            assert!(at(committed_ms) < waiting.deadline);
            drop(permit);
            admission.committed(at(committed_ms), profile.samples());
        }
        // The 39ms admission's valid 69ms commit crosses both 40 and 60ms.
        // Reaping that receipt at 70ms consumes slot 3; it does not rebase to
        // 69+20=89ms, nor use the observer's later receipt-processing time.
        assert_eq!(admission.due(640, profile, at(70)), at(80));
        assert_eq!(admission.due(1600, profile, at(70)), at(100));
        admission
            .stage(
                waiting20(640, at(70), at(70)),
                &outgoing,
                0,
                at(70),
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(
            admission.wake_at(&mut budget, at(70), profile),
            Some(at(80))
        );
        assert!(admission
            .take_ready(&mut budget, at(79), profile, &port)
            .is_none());
        let (_, permit) = admission
            .take_ready(&mut budget, at(80), profile, &port)
            .unwrap();
        drop(permit);
        // A second overdue source timestamp cannot consume the same slot.
        admission
            .stage(
                waiting20(960, at(81), at(81)),
                &outgoing,
                0,
                at(81),
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(
            admission.wake_at(&mut budget, at(81), profile),
            Some(at(100))
        );
        assert!(admission
            .take_ready(&mut budget, at(99), profile, &port)
            .is_none());
        let (_, permit) = admission
            .take_ready(&mut budget, at(100), profile, &port)
            .unwrap();
        drop(permit);
        assert_eq!(admission.first, Some((0, origin)));
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 0);
    }

    #[test]
    fn seconds_of_admission_backpressure_expire_plaintext_without_grid_replay() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        admission
            .stage(
                waiting20(0, origin, origin),
                &outgoing,
                0,
                origin,
                profile,
                &port,
            )
            .unwrap();
        let (_, permit) = admission
            .take_ready(&mut budget, origin, profile, &port)
            .unwrap();
        drop(permit);
        admission.committed(at(1), profile.samples());
        admission
            .stage(
                waiting20(320, at(20), at(20)),
                &outgoing,
                0,
                at(20),
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(outgoing.capacity(), 0);
        // A stalled admission worker cannot encrypt or reset the old deadline.
        assert!(admission
            .take_ready(&mut budget, at(4000), profile, &port)
            .is_none());
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 320);
        assert_eq!(outgoing.capacity(), 1);
        assert_eq!(budget.media_ready_at(94, at(4000)), Some(at(4000)));
        for (position, encoded_ms, admitted_ms) in
            [(200 * 320, 4000, 4000), (201 * 320, 4001, 4020)]
        {
            admission
                .stage(
                    waiting20(position, at(encoded_ms), at(encoded_ms)),
                    &outgoing,
                    0,
                    at(encoded_ms),
                    profile,
                    &port,
                )
                .unwrap();
            assert_eq!(
                admission.wake_at(&mut budget, at(encoded_ms), profile),
                Some(at(admitted_ms))
            );
            if encoded_ms != admitted_ms {
                assert!(admission
                    .take_ready(&mut budget, at(4019), profile, &port)
                    .is_none());
            }
            let (_, permit) = admission
                .take_ready(&mut budget, at(admitted_ms), profile, &port)
                .unwrap();
            drop(permit);
        }
        assert_eq!(admission.first, Some((0, origin)));
        assert_eq!(admission.due(202 * 320, profile, at(4021)), at(4040));
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 320);
    }

    #[test]
    fn source_gaps_and_nonsteady_handoffs_preserve_all_profile_grids() {
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let period = u64::from(profile.duration_ms());
            let samples = profile.samples() as u64;
            let first_position = 10 * samples;
            let port = test_port();
            let (outgoing, _requests) = mpsc::channel(1);
            let mut admission = MediaAdmission::default();
            let mut budget = packet::Budget::new(
                50_000,
                profile.packet_cap() + packet::MEDIA_OVERHEAD,
                origin,
            );
            for (slot, encoded_ms, admitted_ms) in [
                (0, 0, 0),
                (1, period - 5, period + 1),
                (4, 4 * period, 4 * period + 1),
                (5, 5 * period - 5, 5 * period),
                (6, 6 * period + 1, 6 * period + 1),
                (9, 9 * period - 1, 9 * period),
            ] {
                let position = first_position + slot * samples;
                let waiting = WaitingPacket::new(
                    vec![7; 30],
                    position,
                    at(encoded_ms),
                    at(encoded_ms),
                    at(encoded_ms),
                    profile,
                );
                admission
                    .stage(waiting, &outgoing, 0, at(encoded_ms), profile, &port)
                    .unwrap();
                let expected_due = at((slot * period).max(encoded_ms));
                assert_eq!(
                    admission.wake_at(&mut budget, at(encoded_ms), profile),
                    Some(expected_due)
                );
                if expected_due > at(encoded_ms) {
                    assert!(admission
                        .take_ready(
                            &mut budget,
                            expected_due - Duration::from_nanos(1),
                            profile,
                            &port
                        )
                        .is_none());
                }
                let (waiting, permit) = admission
                    .take_ready(&mut budget, at(admitted_ms), profile, &port)
                    .unwrap();
                assert_eq!(waiting.position, position);
                drop(permit);
                admission.committed(at(admitted_ms + 1), profile.samples());
                assert_eq!(admission.first, Some((first_position, origin)));
            }
            assert_eq!(
                admission.due(first_position + 10 * samples, profile, at(9 * period + 2)),
                at(10 * period)
            );
            assert_eq!(port.stats.lock().unwrap().dropped_capture, 0);
        }
    }

    #[test]
    fn pending_media_shares_the_lane_slot_and_counts_real_queue_and_age_drops() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = test_port();
        let (outgoing, mut requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        outgoing
            .try_send(SendRequest {
                opcode: OP_RTP,
                payload: vec![1],
                deadline: None,
                committed: None,
            })
            .unwrap();
        admission
            .stage(
                waiting20(0, origin, origin),
                &outgoing,
                0,
                origin,
                profile,
                &port,
            )
            .unwrap();
        assert!(admission.pending.is_none()); // A lane-queued packet already owns the slot.
        requests.try_recv().unwrap();
        admission
            .stage(
                waiting20(320, origin, origin),
                &outgoing,
                2,
                origin,
                profile,
                &port,
            )
            .unwrap();
        assert!(admission.pending.is_none()); // Unreaped commitment bound unchanged.
        admission
            .stage(
                waiting20(640, origin, origin),
                &outgoing,
                0,
                origin,
                profile,
                &port,
            )
            .unwrap();
        admission
            .stage(
                waiting20(960, origin, origin),
                &outgoing,
                0,
                origin,
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(admission.pending.as_ref().unwrap().0.position, 640); // No replacement/retry.
        assert!(admission
            .take_ready(&mut budget, at(40), profile, &port)
            .is_none());
        assert!(admission.pending.is_none());
        assert_eq!(outgoing.capacity(), 1);
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 4 * 320);
        assert!(budget.admit_media(94, at(40))); // All four drops were before budget/SRTP.
        assert!(budget.admit_feedback(at(40)));

        // Existing capture-age deadline wins even if the encode-age budget has
        // time remaining. Credit cannot refill before this particular deadline.
        admission
            .stage(
                waiting20(1280, origin - Duration::from_millis(2), at(40)),
                &outgoing,
                0,
                at(40),
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(admission.pending.as_ref().unwrap().0.deadline, at(58));
        assert_eq!(
            admission.wake_at(&mut budget, at(40), profile),
            Some(at(58))
        );
        assert!(admission
            .take_ready(&mut budget, at(57), profile, &port)
            .is_none());
        // A delayed worker must discard, rather than admit, at the immutable end.
        assert!(admission
            .take_ready(&mut budget, at(58), profile, &port)
            .is_none());
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 5 * 320);
        admission.discard(profile, &port);
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 5 * 320);
        drop(requests);
        assert!(admission
            .stage(
                waiting20(1600, at(60), at(60)),
                &outgoing,
                0,
                at(60),
                profile,
                &port,
            )
            .is_err());
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 6 * 320);
    }

    #[test]
    fn paced_twenty_ms_bursts_conserve_feedback_priority_and_total_allowance() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        let mut bytes = 0u128;
        let mut last = None;
        for slot in 0..100u64 {
            let encoded_ms = slot / 2 * 40 + slot % 2;
            let encoded_at = origin + Duration::from_millis(encoded_ms);
            admission
                .stage(
                    waiting20(slot * 320, encoded_at, encoded_at),
                    &outgoing,
                    0,
                    encoded_at,
                    profile,
                    &port,
                )
                .unwrap();
            let admitted_ms = slot * 20;
            let now = origin + Duration::from_millis(admitted_ms);
            if slot % 10 == 0 {
                assert!(budget.admit_feedback(now));
                bytes += packet::FEEDBACK_FRAMED_MAX as u128;
            }
            let (waiting, permit) = admission
                .take_ready(&mut budget, now, profile, &port)
                .unwrap();
            assert_eq!(waiting.position, slot * 320);
            bytes += waiting.bytes() as u128;
            drop(permit);
            if let Some(previous) = last {
                assert!(now.duration_since(previous) >= Duration::from_millis(20));
            }
            last = Some(now);
            // Exact original envelope: pmax+152 burst, 75% of 50kbit/s refill.
            assert!(bytes * 8 * 1_000 <= (94 + 152) * 8 * 1_000 + 37_500 * u128::from(admitted_ms));
        }
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 0);
        assert_eq!(bytes, 100 * 74 + 10 * 152);
        assert_eq!(admission.first, Some((0, origin)));
    }

    #[test]
    fn persistent_twenty_ms_peak_still_drops_with_fixed_deadlines_and_allowance() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let port = test_port();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 94, origin);
        let mut bytes = 0u128;
        let mut last: Option<Instant> = None;
        for millis in 0..2_000u64 {
            let now = origin + Duration::from_millis(millis);
            if millis % 20 == 0 {
                let mut waiting = waiting20(millis * 16, now, now);
                *waiting.opus = vec![7; 50]; // Legal cap50: 37.6kbit/s media peak.
                admission
                    .stage(waiting, &outgoing, 0, now, profile, &port)
                    .unwrap();
            }
            if millis % 200 == 0 {
                assert!(budget.admit_feedback(now));
                bytes += packet::FEEDBACK_FRAMED_MAX as u128;
            }
            if let Some((waiting, permit)) = admission.take_ready(&mut budget, now, profile, &port)
            {
                bytes += waiting.bytes() as u128;
                drop(permit);
                if let Some(previous) = last {
                    // Slot occupancy, rather than last actual send +20ms,
                    // prevents catch-up while exact credit limits byte bursts.
                    assert!(
                        now.duration_since(origin).as_millis() / 20
                            > previous.duration_since(origin).as_millis() / 20
                    );
                }
                last = Some(now);
            }
            assert!(bytes * 8 * 1_000 <= (94 + 152) * 8 * 1_000 + 37_500 * u128::from(millis));
        }
        assert!(port.stats.lock().unwrap().dropped_capture > 0); // Waiting cannot cure sustained excess.
        assert_eq!(admission.first, Some((0, origin)));
        admission.discard(profile, &port);
        assert_eq!(outgoing.capacity(), 1);
    }

    #[tokio::test]
    async fn waiting_media_slot_keeps_real_lane_receive_and_control_duplex_live() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (a, b, relay_fixture, relay_key) = fixture::pair(PublicConfig {
            relay_addr: addr.clone(),
            domain: "m1v.fixture".into(),
            profile_ms: 20,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        })
        .unwrap();
        let (stop, stopped) = watch::channel(false);
        let relay_owner = tokio::spawn(relay::serve(listener, relay_fixture, relay_key, stopped));
        let mut a_media = relay::connect(&addr, &a, true).await.unwrap();
        let mut a_control = relay::connect(&addr, &a, false).await.unwrap();
        let mut b_media = relay::connect(&addr, &b, true).await.unwrap();
        let mut b_control = relay::connect(&addr, &b, false).await.unwrap();
        let profile = LiveProfile::Ms20;
        let port = test_port();
        let origin = Instant::now();
        let mut admission = MediaAdmission::default();
        admission
            .stage(
                waiting20(320, origin, origin),
                &a_media.outgoing,
                0,
                origin,
                profile,
                &port,
            )
            .unwrap();
        assert_eq!(a_media.outgoing.capacity(), 0);

        let mut a_sender = dmsg_srtp_sys::Sender::new(&a.media_tx, a.ssrc_tx).unwrap();
        let mut b_sender = dmsg_srtp_sys::Sender::new(&b.media_tx, b.ssrc_tx).unwrap();
        let mut a_receiver = dmsg_srtp_sys::Receiver::new(&a.media_rx, a.ssrc_rx).unwrap();
        let mut b_receiver = dmsg_srtp_sys::Receiver::new(&b.media_rx, b.ssrc_rx).unwrap();
        let opus = LiveEncoder::new(profile)
            .unwrap()
            .encode(&[0; 320])
            .unwrap()
            .bytes;
        let rtp = packet::rtp(
            b.ssrc_tx,
            u64::from(b.initial_sequence),
            b.initial_timestamp,
            &opus,
        );
        b_media
            .outgoing
            .try_send(SendRequest {
                opcode: OP_RTP,
                payload: b_sender.protect_rtp(&rtp).unwrap(),
                deadline: None,
                committed: None,
            })
            .unwrap();
        for (fixture, sender, control) in [
            (&a, &mut a_sender, &a_control),
            (&b, &mut b_sender, &b_control),
        ] {
            let report = packet::compound(packet::Report {
                sender_ssrc: fixture.ssrc_tx,
                peer_ssrc: fixture.ssrc_rx,
                cname: &fixture.cname,
                sender: None,
                feedback: packet::Feedback::default(),
            });
            control
                .outgoing
                .try_send(SendRequest {
                    opcode: OP_RTCP,
                    payload: sender.protect_rtcp(&report).unwrap(),
                    deadline: None,
                    committed: None,
                })
                .unwrap();
        }
        let received = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                a_media.incoming.recv(),
                a_control.incoming.recv(),
                b_control.incoming.recv()
            )
        })
        .await
        .unwrap();
        let (op, cipher) = received.0.unwrap().unwrap();
        assert_eq!(op, OP_RTP);
        assert_eq!(a_receiver.unprotect_rtp(&cipher).unwrap(), rtp);
        for (frame, receiver, fixture) in [
            (received.1, &mut a_receiver, &a),
            (received.2, &mut b_receiver, &b),
        ] {
            let (op, cipher) = frame.unwrap().unwrap();
            assert_eq!(op, OP_RTCP);
            let plain = receiver.unprotect_rtcp(&cipher).unwrap();
            packet::read_compound(
                &plain,
                fixture.ssrc_rx,
                fixture.ssrc_tx,
                &fixture.peer_cname,
            )
            .unwrap();
        }
        assert!(b_media.incoming.try_recv().is_err()); // No premature SRTP/Noise submission.
        assert!(admission.pending.is_some()); // All three receives happened while held.
        admission.discard(profile, &port);
        assert_eq!(port.stats.lock().unwrap().dropped_capture, 320);
        stop.send(true).unwrap();
        a_media.joined_close().await;
        a_control.joined_close().await;
        b_media.joined_close().await;
        b_control.joined_close().await;
        tokio::time::timeout(Duration::from_secs(2), relay_owner)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn old_physical_capture_is_not_rejuvenated_by_a_fresh_jni_push() {
        let pcm = [123; 160];
        let (host, mut host_input) = test_audio();
        assert!(host.push(&pcm)); // The old JNI path behaved exactly like this.
        let fresh = host_input.try_recv().unwrap();
        assert_eq!(fresh.at, fresh.pushed_at); // No physical age was carried.

        let (physical, mut input) = test_audio();
        calibrate_audio(&physical, &[50; 20]);
        // Absolute age 91ms is 41ms above the observed 50ms floor. A fresh
        // JNI submission cannot rejuvenate this application backlog.
        assert!(!physical.push_recorded(&pcm, aged_record_timestamp(3200, 91), Instant::now()));
        assert!(matches!(
            input.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(physical.port.position.load(Ordering::Relaxed), 3360);
        let stats = physical.port.stats.lock().unwrap();
        assert_eq!(stats.dropped_capture, 21 * 160);
        assert_eq!(stats.capture_age_rejected_batches, 1);
        assert_eq!(stats.capture_age_unavailable_batches, 0);
        assert_eq!(stats.max_capture_age_us, 91_000);
        assert_eq!(stats.max_additional_capture_age_us, 41_000);
        assert_eq!(stats.initial_capture_age_floor_us, 50_000);
    }

    #[test]
    fn physical_capture_age_survives_native_queue_time_at_the_exact_bound() {
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        let pushed_at = Instant::now();
        assert!(audio.push_recorded(&[1; 160], aged_record_timestamp(3200, 90), pushed_at));
        let chunk = input.try_recv().unwrap();
        assert_eq!(chunk.at, pushed_at - CAPTURE_MAX_AGE);
        assert_eq!(chunk.pushed_at, pushed_at);
        assert_eq!(chunk.position, 3200);
        assert_eq!(pushed_at.duration_since(chunk.at), CAPTURE_MAX_AGE);
        assert_eq!(
            chunk.ages(pushed_at),
            (Duration::from_millis(90), CAPTURE_MAX_AGE),
        );
        // True hardware age and additional age both retain native waiting;
        // the application budget is not reset on dequeue.
        let (hardware, additional) = chunk.ages(pushed_at + Duration::from_nanos(1));
        assert_eq!(
            hardware,
            Duration::from_millis(90) + Duration::from_nanos(1)
        );
        assert_eq!(additional, CAPTURE_MAX_AGE + Duration::from_nanos(1));
        assert!(additional > CAPTURE_MAX_AGE);
    }

    #[test]
    fn completed_clock_observation_does_not_count_acquisition_as_queue_waiting() {
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        let entry = Instant::now();
        let completed = entry + Duration::from_millis(8);
        let mut timestamp = aged_record_timestamp(3200, 80);
        timestamp.observed_ns += 8_000_000; // Absolute age 88ms at the Java observation.
        assert!(audio.push_recorded_observed(&[1; 160], timestamp, entry, completed));
        let chunk = input.try_recv().unwrap();
        assert_eq!(
            chunk.ages(completed),
            (Duration::from_millis(88), Duration::from_millis(38)),
        );
        assert_eq!(chunk.pushed_at, completed);
        assert_eq!(chunk.at, completed - Duration::from_millis(38));
        assert_eq!(chunk.position, 3200);
        // The unchanged application bound is reached only by subsequent real
        // waiting: equality is allowed, while another nanosecond expires it.
        let at_bound = completed + Duration::from_millis(2);
        assert_eq!(
            chunk.ages(at_bound),
            (Duration::from_millis(90), CAPTURE_MAX_AGE),
        );
        assert!(chunk.ages(at_bound).1 <= CAPTURE_MAX_AGE);
        assert!(chunk.ages(at_bound + Duration::from_nanos(1)).1 > CAPTURE_MAX_AGE);
        {
            let stats = audio.port.stats.lock().unwrap();
            assert_eq!(stats.max_capture_clock_observation_gap_us, 8_000);
            assert_eq!(stats.initial_capture_age_floor_us, 50_000);
            assert_eq!(stats.max_capture_age_us, 88_000);
            assert_eq!(stats.max_additional_capture_age_us, 38_000);
            assert_eq!(stats.capture_age_rejected_batches, 0);
            assert_eq!(stats.capture_age_unavailable_batches, 0);
            assert_eq!(stats.dropped_capture, 20 * 160);
        }
        // Failed clock acquisition is still rejected and consumes its source
        // positions. A later shorter bracket cannot erase the diagnostic max.
        let next_entry = completed + Duration::from_millis(20);
        let mut invalid = aged_record_timestamp(3360, 50);
        invalid.observed_ns = -1;
        assert!(!audio.push_recorded_observed(
            &[1; 160],
            invalid,
            next_entry,
            next_entry + Duration::from_millis(4),
        ));
        assert!(matches!(
            input.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(audio.port.position.load(Ordering::Relaxed), 3520);
        let stats = audio.port.stats.lock().unwrap();
        assert_eq!(stats.max_capture_clock_observation_gap_us, 8_000);
        assert_eq!(stats.initial_capture_age_floor_us, 50_000);
        assert_eq!(stats.capture_age_unavailable_batches, 1);
        assert_eq!(stats.capture_age_rejected_batches, 0);
        assert_eq!(stats.dropped_capture, 21 * 160);
    }

    #[test]
    fn capture_timestamp_uses_oldest_partial_read_and_fixed_recording_origin() {
        let stamp = record_timestamp(0, 320, 1_000_000_000);
        assert_eq!(stamp.age(0, 160, None), Some(Duration::from_millis(20)));
        // Two partial reads of 80+80 still own 0..160, not the last 80..160.
        assert_eq!(
            record_timestamp(80, 320, 1_000_000_000).age(0, 160, None),
            None
        );
        assert_eq!(
            record_timestamp(0, 0, 1_010_000_000).age(0, 160, None),
            Some(Duration::from_millis(10)),
        );
        // A first successful anchor at a nonzero hardware position must not
        // redefine recording frame zero or rejuvenate the unread prefix.
        assert_eq!(
            record_timestamp(0, 3200, 1_000_000_000).age(0, 160, None),
            Some(Duration::from_millis(200)),
        );
    }

    #[test]
    fn capture_timestamp_rejects_unavailable_stale_future_and_regressing_mappings() {
        assert_eq!(
            record_timestamp(0, -1, 1_000_000_000).age(0, 160, None),
            None
        );
        let mut stamp = record_timestamp(0, 320, 1_000_000_000);
        stamp.nano_time = 0;
        assert_eq!(stamp.age(0, 160, None), None);
        assert_eq!(
            record_timestamp(0, 320, 1_040_000_001).age(0, 160, None),
            None
        );
        assert_eq!(
            record_timestamp(0, 320, 999_999_999).age(0, 160, None),
            None
        );
        // Positive oldest age is insufficient if this batch's end is future.
        assert_eq!(
            record_timestamp(160, 240, 1_000_000_000).age(160, 160, None),
            None
        );
        assert_eq!(
            record_timestamp(0, 320, 1_000_000_000).age(0, 160, Some((480, 1_000_000_000))),
            None,
        );
        assert_eq!(
            record_timestamp(0, 320, 1_000_000_000).age(0, 160, Some((320, 999_999_999))),
            None,
        );
        // Identical readback may be reused only while the anchor remains fresh.
        assert_eq!(
            record_timestamp(160, 320, 1_010_000_000).age(160, 160, Some((320, 1_000_000_000))),
            Some(Duration::from_millis(20)),
        );
        let stamp = CaptureTimestamp {
            read_position: i64::MAX - 160,
            frame_position: i64::MAX,
            nano_time: i64::MAX - 10_000_000,
            observed_ns: i64::MAX,
        };
        assert_eq!(
            stamp.age((i64::MAX - 160) as u64, 160, None),
            Some(Duration::from_millis(20))
        );
    }

    #[test]
    fn invalid_and_full_capture_batches_advance_positions_and_are_never_retried() {
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        let base = 3200;
        assert!(!audio.push_recorded(
            &[7; 160],
            record_timestamp(base, -1, 1_000_000_000),
            Instant::now(),
        ));
        let anchor_frame = base + 1760;
        let anchor_time = 1_000_000_000 + anchor_frame * SAMPLE_NS as i64;
        for slot in 1..=5 {
            assert_eq!(
                audio.push_recorded(
                    &[7; 160],
                    CaptureTimestamp {
                        read_position: base + slot * 160,
                        frame_position: anchor_frame,
                        nano_time: anchor_time,
                        observed_ns: anchor_time,
                    },
                    Instant::now(),
                ),
                // Slot one has 50ms additional age; slots 2..5 have 40..10ms
                // and fill the unchanged four-batch input ring.
                slot != 1,
            );
        }
        assert!(!audio.push_recorded(
            &[7; 160],
            aged_record_timestamp(base as u64 + 960, 60),
            Instant::now(),
        ));
        // Keep the fixed hardware epoch: the next anchor must advance both
        // frame and time. The full-ring drop still consumes sample positions.
        assert_eq!(
            audio.port.position.load(Ordering::Relaxed),
            base as u64 + 1120
        );
        let mut positions = Vec::new();
        while let Ok(chunk) = input.try_recv() {
            positions.push(chunk.position);
        }
        assert_eq!(positions, vec![3520, 3680, 3840, 4000]);
        let stats = audio.port.stats.lock().unwrap();
        assert_eq!(stats.dropped_capture, 23 * 160);
        assert_eq!(stats.capture_age_unavailable_batches, 1);
        assert_eq!(stats.capture_age_rejected_batches, 1);
        drop(stats);
        let stamp = aged_record_timestamp(base as u64 + 1120, 60);
        assert!(audio.push_recorded(&[7; 160], stamp, Instant::now()));
        assert_eq!(input.try_recv().unwrap().position, base as u64 + 1120);
        // A final partial read is never encoded/retried during teardown, but
        // its actual read samples still consume positions and loss accounting.
        assert!(!audio.push_recorded(
            &[7; 80],
            record_timestamp(base + 1280, -1, 1_020_000_000),
            Instant::now(),
        ));
        assert_eq!(
            audio.port.position.load(Ordering::Relaxed),
            base as u64 + 1360
        );
        let stats = audio.port.stats.lock().unwrap();
        assert_eq!(stats.dropped_capture, 23 * 160 + 80);
        assert_eq!(stats.capture_age_unavailable_batches, 2);
    }

    #[test]
    fn observed_initial_floor_accepts_normal_nonzero_hardware_age_without_hiding_it() {
        let (audio, mut input) = test_audio();
        let mut initial = [50; 20];
        initial[0] = 70;
        initial[1] = 60;
        calibrate_audio(&audio, &initial);
        assert!(matches!(
            input.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        for (slot, age_ms) in [50, 60, 70].into_iter().enumerate() {
            let position = 3200 + slot as u64 * 160;
            let pushed_at = Instant::now();
            assert!(audio.push_recorded(
                &[7; 160],
                aged_record_timestamp(position, age_ms),
                pushed_at,
            ));
            let chunk = input.try_recv().unwrap();
            assert_eq!(chunk.position, position);
            assert_eq!(
                chunk.ages(pushed_at),
                (
                    Duration::from_millis(age_ms),
                    Duration::from_millis(age_ms - 50)
                ),
            );
        }
        let stats = audio.port.stats.lock().unwrap();
        assert!(stats.capture_clock_calibrated);
        assert_eq!(stats.initial_capture_age_floor_us, 50_000);
        assert_eq!(stats.max_capture_age_us, 70_000);
        assert_eq!(stats.max_additional_capture_age_us, 20_000);
        assert_eq!(stats.dropped_capture, 20 * 160);
        assert_eq!(stats.capture_age_rejected_batches, 0);
        assert_eq!(stats.capture_age_unavailable_batches, 0);
    }

    #[test]
    fn calibration_requires_twenty_valid_full_batches_and_drops_all_startup_pcm() {
        let (audio, mut input) = test_audio();
        assert!(!audio.push_recorded(
            &[1; 160],
            record_timestamp(0, -1, 1_000_000_000),
            Instant::now(),
        ));
        assert!(!audio.push_recorded(&[1; 80], aged_record_timestamp(160, 50), Instant::now()));
        for slot in 0..19 {
            assert!(!audio.push_recorded(
                &[1; 160],
                aged_record_timestamp(240 + slot * 160, 50),
                Instant::now(),
            ));
        }
        {
            let stats = audio.port.stats.lock().unwrap();
            assert!(!stats.capture_clock_calibrated);
            assert_eq!(stats.initial_capture_age_floor_us, 0);
            assert_eq!(audio.port.record_clock.lock().unwrap().initial_batches, 19);
        }
        // The twentieth valid full batch freezes the floor but is still loss.
        assert!(!audio.push_recorded(&[1; 160], aged_record_timestamp(3280, 50), Instant::now()));
        assert!(matches!(
            input.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(audio.port.position.load(Ordering::Relaxed), 3440);
        {
            let stats = audio.port.stats.lock().unwrap();
            assert!(stats.capture_clock_calibrated);
            assert_eq!(stats.initial_capture_age_floor_us, 50_000);
            assert_eq!(stats.dropped_capture, 3440);
            assert_eq!(stats.capture_age_unavailable_batches, 1);
            assert_eq!(stats.capture_age_rejected_batches, 0);
        }
        assert!(audio.push_recorded(&[1; 160], aged_record_timestamp(3440, 50), Instant::now()));
        assert_eq!(input.try_recv().unwrap().position, 3440);
    }

    #[test]
    fn frozen_initial_floor_cannot_grow_to_hide_a_two_hundred_ms_backlog() {
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        for (slot, age_ms) in [91, 200, 250].into_iter().enumerate() {
            assert!(!audio.push_recorded(
                &[1; 160],
                aged_record_timestamp(3200 + slot as u64 * 160, age_ms),
                Instant::now(),
            ));
        }
        assert!(matches!(
            input.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(audio.port.position.load(Ordering::Relaxed), 23 * 160);
        let clock = audio.port.record_clock.lock().unwrap();
        assert_eq!(clock.initial_batches, 20);
        assert_eq!(clock.frozen_floor(), Some(Duration::from_millis(50)));
        let stats = audio.port.stats.lock().unwrap();
        assert_eq!(stats.initial_capture_age_floor_us, 50_000);
        assert_eq!(stats.max_capture_age_us, 250_000);
        assert_eq!(stats.max_additional_capture_age_us, 200_000);
        assert_eq!(stats.capture_age_rejected_batches, 3);
        assert_eq!(stats.capture_age_unavailable_batches, 0);
        assert_eq!(stats.dropped_capture, 23 * 160);
    }

    #[test]
    fn calibrated_clock_still_rejects_invalid_references_without_changing_its_floor() {
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        let previous = audio.port.record_clock.lock().unwrap().latest.unwrap();
        for slot in 0..5 {
            let position = 3200 + slot * 160;
            let mut stamp = aged_record_timestamp(position, 50);
            match slot {
                0 => stamp.frame_position = -1,
                1 => stamp.observed_ns += 40_000_001, // Stale hardware anchor.
                2 => stamp.observed_ns = stamp.nano_time - 1, // Future reference.
                3 => stamp.read_position += 160,      // Not this batch's origin.
                4 => {
                    stamp.frame_position = previous.0 - 1;
                    stamp.nano_time = previous.1;
                    stamp.observed_ns = previous.1;
                }
                _ => unreachable!(),
            }
            assert!(!audio.push_recorded(&[1; 160], stamp, Instant::now()));
        }
        assert!(matches!(
            input.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(audio.port.position.load(Ordering::Relaxed), 25 * 160);
        let clock = audio.port.record_clock.lock().unwrap();
        assert_eq!(clock.latest, Some(previous));
        assert_eq!(clock.frozen_floor(), Some(Duration::from_millis(50)));
        let stats = audio.port.stats.lock().unwrap();
        assert!(stats.capture_clock_calibrated);
        assert_eq!(stats.initial_capture_age_floor_us, 50_000);
        assert_eq!(stats.max_capture_age_us, 50_000);
        assert_eq!(stats.capture_age_unavailable_batches, 5);
        assert_eq!(stats.capture_age_rejected_batches, 0);
        assert_eq!(stats.dropped_capture, 25 * 160);
    }

    #[test]
    fn sender_phase_uses_first_admitted_handoff_epoch_without_rebase() {
        let origin = Instant::now();
        let mut phase = SendPhase { first: None };
        assert_eq!(phase.observe(3200, origin), 0); // Earlier startup drops aren't an epoch.
        assert_eq!(phase.observe(3520, origin + Duration::from_millis(20)), 0);
        assert_eq!(
            phase.observe(3840, origin + Duration::from_millis(21)),
            19_000
        );
        assert_eq!(
            phase.observe(4160, origin + Duration::from_millis(41)),
            19_000
        );
        assert_eq!(phase.first, Some((3200, origin)));
    }

    #[test]
    fn twenty_ms_backlog_has_future_rejections_and_phase_advance_without_lateness() {
        let profile = LiveProfile::Ms20;
        let origin = Instant::now();
        let port = test_port();
        let mut received = test_received();
        let mut feedback = packet::Feedback::default();
        // Admission does not decode, so packet content is immaterial here.
        received.admit(0, &[1], origin, profile, &mut feedback, &port);
        for slot in 1..=11 {
            received.admit(
                slot * 960,
                &[1],
                origin + Duration::from_millis(1),
                profile,
                &mut feedback,
                &port,
            );
        }
        assert_eq!(received.encoded.len(), 11); // Exact existing 0..200ms bound.
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.future_rejected_packets, 1);
        assert_eq!(stats.dropped_render, 320);
        assert_eq!(stats.max_future_lead_ms, 220);
        assert_eq!(stats.max_receiver_phase_advance_us, 219_000);
        assert_eq!(stats.late_packets, 0);
        assert_eq!(stats.arrival_after_nominal_due_packets, 0);
        assert_eq!(received.playout.as_ref().unwrap().cursor, 0);
        assert_eq!(
            received.playout.as_ref().unwrap().due,
            origin + Duration::from_millis(80)
        );
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
            remote_clock: RemoteClock::default(),
            first_playout: None,
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
    fn protected_rtp_sequence_and_timestamp_rollover() {
        let (mut local, mut peer, _, _) = fixture::pair(PublicConfig {
            relay_addr: "127.0.0.1:1".into(),
            domain: "m1v.fixture".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        })
        .unwrap();
        // Diagnostic near-wrap origins: both 16-bit SEQ and 32-bit 48 kHz TS
        // cross zero inside three authenticated packets.
        peer.initial_sequence = 65_534;
        peer.initial_timestamp = 0xffff_f880;
        local.peer_initial_sequence = peer.initial_sequence;
        local.peer_initial_timestamp = peer.initial_timestamp;
        let port = test_port();
        let mut received = ReceiveState {
            index: u64::from(local.peer_initial_sequence),
            timestamp: u64::from(local.peer_initial_timestamp),
            encoded: VecDeque::new(),
            playout: None,
            remote_clock: RemoteClock::default(),
            first_playout: None,
        };
        let mut sender = dmsg_srtp_sys::Sender::new(&peer.media_tx, peer.ssrc_tx).unwrap();
        let mut receiver = dmsg_srtp_sys::Receiver::new(&local.media_rx, local.ssrc_rx).unwrap();
        let mut feedback = packet::Feedback::default();
        let mut previous = None;
        for step in 0..3u64 {
            let index = u64::from(peer.initial_sequence) + step;
            let timestamp = peer.initial_timestamp.wrapping_add(1920 * step as u32);
            let cipher = sender
                .protect_rtp(&packet::rtp(peer.ssrc_tx, index, timestamp, &opus40()))
                .unwrap();
            received
                .receive(
                    (OP_RTP, cipher),
                    &mut receiver,
                    &local,
                    &mut feedback,
                    &port,
                )
                .unwrap();
            assert_eq!(feedback.terminal, Some(index));
            assert_eq!(
                received.timestamp,
                u64::from(peer.initial_timestamp) + 1920 * step
            );
            assert!(previous.is_none_or(|old| index > old));
            previous = Some(index);
        }
        assert_eq!(feedback.terminal, Some(65_536));
        assert_eq!(received.timestamp, 0x1_0000_0780);
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.received_rtp_packets, 3);
        assert_eq!(stats.late_packets, 0);
        assert_eq!(stats.future_rejected_packets, 0);
        assert_eq!(received.encoded.len(), 3);
    }

    #[test]
    fn protected_active_silence_dtx_and_application_drop_accounting() {
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
        // receive() uses wall time; a future origin keeps its authenticated
        // admission deterministic while tick/pull use this virtual timeline.
        let origin = Instant::now() + Duration::from_secs(60);
        let first_timestamp = u64::from(peer.initial_timestamp);
        let source = test_port();
        let port = test_port();
        let mut received = test_received();
        received.index = u64::from(peer.initial_sequence);
        received.timestamp = first_timestamp;
        received.playout = Some(PlayoutClock {
            cursor: first_timestamp,
            due: origin + Duration::from_millis(80),
        });
        let mut encoder = LiveEncoder::new(profile).unwrap();
        let mut decoder = LiveDecoder::new(profile).unwrap();
        let mut sender = dmsg_srtp_sys::Sender::new(&peer.media_tx, peer.ssrc_tx).unwrap();
        let mut receiver = dmsg_srtp_sys::Receiver::new(&local.media_rx, local.ssrc_rx).unwrap();
        let mut feedback = packet::Feedback::default();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut admission = MediaAdmission::default();
        let mut budget = packet::Budget::new(50_000, 144, origin);
        let mut index = u64::from(peer.initial_sequence);
        let (mut active_decoded, mut silent_decoded, mut active_plc) = (0, 0, 0);
        let (mut tiny_dtx, mut framed_bytes, mut rendered_samples) = (0, 0, 0);
        for slot in 0..120u64 {
            // Activity is fixture truth, not an encoder/VAD/decoded-PCM guess.
            let active = !(10..110).contains(&slot);
            let position = slot as usize * profile.samples();
            let pcm = if active {
                super::super::test_tone(position, profile.samples())
            } else {
                vec![0; profile.samples()]
            };
            let encoded = encoder.encode(&pcm).unwrap(); // Continuous, including the drop.
            if encoded.in_dtx && encoded.bytes.len() <= 2 {
                assert!(!active);
                assert_eq!(encoded.info.concealed_samples, 640);
                tiny_dtx += 1;
            }
            if slot == 4 {
                let at = origin + Duration::from_millis(slot * 40);
                let waiting =
                    WaitingPacket::new(encoded.bytes, position as u64, at, at, at, profile);
                let deadline = waiting.deadline;
                admission
                    .stage(waiting, &outgoing, 0, at, profile, &source)
                    .unwrap();
                assert!(admission
                    .take_ready(&mut budget, deadline, profile, &source)
                    .is_none());
                assert_eq!(outgoing.capacity(), 1); // Drop precedes SRTP/sequence use.
                active_plc += 1;
            } else {
                if slot == 5 {
                    assert_eq!(index, u64::from(peer.initial_sequence) + 4);
                    assert_eq!(received.timestamp, first_timestamp + 3 * 1920);
                }
                let timestamp = peer.initial_timestamp.wrapping_add((position * 3) as u32);
                let cipher = sender
                    .protect_rtp(&packet::rtp(peer.ssrc_tx, index, timestamp, &encoded.bytes))
                    .unwrap();
                framed_bytes += (cipher.len() + packet::FRAMING_BYTES) as u64;
                received
                    .receive(
                        (OP_RTP, cipher),
                        &mut receiver,
                        &local,
                        &mut feedback,
                        &port,
                    )
                    .unwrap();
                assert_eq!(feedback.terminal, Some(index));
                assert_eq!(received.timestamp, first_timestamp + slot * 1920);
                index += 1;
                active_decoded += u64::from(active);
                silent_decoded += u64::from(!active);
            }
            let due = origin + Duration::from_millis(80 + slot * 40);
            let before = port.stats.lock().unwrap().clone();
            received
                .tick(
                    due - Duration::from_nanos(1),
                    profile,
                    &mut decoder,
                    &mut feedback,
                    &port,
                )
                .unwrap();
            assert_eq!(
                received.playout.as_ref().unwrap().cursor,
                first_timestamp + slot * 1920
            );
            received
                .tick(due, profile, &mut decoder, &mut feedback, &port)
                .unwrap();
            {
                let stats = port.stats.lock().unwrap();
                assert_eq!(
                    stats.decoded_packets - before.decoded_packets,
                    u64::from(slot != 4)
                );
                assert_eq!(stats.plc_slots - before.plc_slots, u64::from(slot == 4));
            }
            let mut batch = [0; 160];
            for _ in 0..4 {
                assert_eq!(port.render.lock().unwrap().pull(due, &mut batch), (160, 0));
                rendered_samples += batch.len();
            }
            received
                .tick(due, profile, &mut decoder, &mut feedback, &port)
                .unwrap(); // Draining cannot decode/conceal the same slot again.
            let stats = port.stats.lock().unwrap();
            assert_eq!(stats.decoded_packets, active_decoded + silent_decoded);
            assert_eq!(stats.plc_slots, active_plc);
            assert_eq!(feedback.playout, Some(first_timestamp + (slot + 1) * 1920));
        }
        assert_eq!((active_decoded, silent_decoded, active_plc), (19, 100, 1));
        assert!(
            tiny_dtx > 10,
            "intentional zero PCM must exercise legal tiny DTX"
        );
        assert_eq!(rendered_samples, 120 * 640);
        assert_eq!(source.stats.lock().unwrap().dropped_capture, 640);
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.received_rtp_packets, 119);
        assert_eq!(stats.rx_bytes, framed_bytes);
        assert_eq!((feedback.late, feedback.plc), (0, 1));
        assert_eq!(stats.plc_before_nominal_due_slots, 0);
        assert_eq!(stats.late_packets + stats.future_rejected_packets, 0);
        assert_eq!(stats.dropped_render + stats.skipped_playout_slots, 0);
        assert!(received.encoded.is_empty() && port.render.lock().unwrap().pcm.is_empty());
        // The known active denominator is 20*40ms: one intentional 40ms/5%
        // source-active loss, even though every transmitted packet decoded.
    }

    #[test]
    fn relative_source_sink_skew_preserves_fixed_receive_bounds_and_negative_results() {
        let profile = LiveProfile::Ms40;
        let slots = 45 * 60 * 1000 / 40;
        // (relative source ppm, first failed source slot, first arrival ns,
        //  first PLC due ns, maximum observed future lead ms, maximum queue).
        for (ppm, first_failed, first_arrival_ns, first_plc_ns, max_lead, max_queue) in [
            (
                -500i64,
                3999u64,
                160_040_020_010u64,
                160_040_000_000u64,
                40,
                2,
            ),
            (-100, 19999, 800_040_004_000, 800_040_000_000, 40, 2),
            (100, 40004, 1_600_000_000_000, 1_600_240_000_000, 320, 6),
            (500, 8004, 320_000_000_000, 320_240_000_000, 1400, 6),
        ] {
            let origin = Instant::now();
            let due = |slot| origin + Duration::from_millis(80 + slot * 40);
            let port = test_port();
            let mut received = test_received();
            let mut decoder = LiveDecoder::new(profile).unwrap();
            let mut feedback = packet::Feedback::default();
            // Legal WB40 DTX exercises actual decode/PLC cheaply. Authentication
            // is covered above; admit() supplies explicit virtual arrival time.
            let opus = [0x50];
            dmsg_opus_sys::live::validate_packet(profile, &opus).unwrap();
            let mut first_late = None;
            let mut first_future = None;
            let mut first_plc = None;
            let mut observed_queue = 0;
            let mut next_sink_slot = 0;
            let mut render_slot = |received: &mut ReceiveState,
                                   decoder: &mut LiveDecoder,
                                   feedback: &mut packet::Feedback,
                                   slot| {
                let before = feedback.plc;
                received
                    .tick(due(slot), profile, decoder, feedback, &port)
                    .unwrap();
                if feedback.plc != before && first_plc.is_none() {
                    first_plc = Some((slot, due(slot).duration_since(origin).as_nanos() as u64));
                }
                let mut batch = [0; 160];
                for _ in 0..4 {
                    assert_eq!(
                        port.render.lock().unwrap().pull(due(slot), &mut batch),
                        (160, 0)
                    );
                }
                let clock = received.playout.as_ref().unwrap();
                assert_eq!(clock.cursor, (slot + 1) * 1920);
                assert_eq!(clock.due, due(slot + 1)); // No remote-rate compensation/rebase.
            };
            for slot in 0..slots {
                // Sink is exactly nominal16k/48k; only the independent source
                // advances at (1+ppm/1e6). No sender/admission-clock reuse.
                let arrival_ns = slot * 40_000_000 * 1_000_000 / (1_000_000 + ppm) as u64;
                let arrival = origin + Duration::from_nanos(arrival_ns);
                // Ready arrivals win exact ties with the sink tick, as on-lane
                // drain-before-tick does. Both clocks otherwise progress independently.
                while next_sink_slot < slots && due(next_sink_slot) < arrival {
                    render_slot(&mut received, &mut decoder, &mut feedback, next_sink_slot);
                    next_sink_slot += 1;
                }
                let before = port.stats.lock().unwrap().clone();
                received.admit(slot * 1920, &opus, arrival, profile, &mut feedback, &port);
                let stats = port.stats.lock().unwrap();
                if stats.late_packets != before.late_packets && first_late.is_none() {
                    first_late = Some((slot, arrival_ns));
                }
                if stats.future_rejected_packets != before.future_rejected_packets
                    && first_future.is_none()
                {
                    first_future = Some((slot, arrival_ns));
                }
                observed_queue = observed_queue.max(received.encoded.len());
                assert!(received.encoded.len() <= 6); // Existing200ms+current bound.
                assert!(port.render.lock().unwrap().pcm.is_empty());
            }
            while next_sink_slot < slots {
                render_slot(&mut received, &mut decoder, &mut feedback, next_sink_slot);
                next_sink_slot += 1;
            }
            let failure = Some((first_failed, first_arrival_ns));
            assert_eq!(
                first_late,
                if ppm < 0 { failure } else { None },
                "ppm={ppm}"
            );
            assert_eq!(
                first_future,
                if ppm > 0 { failure } else { None },
                "ppm={ppm}"
            );
            assert_eq!(first_plc, Some((first_failed, first_plc_ns)), "ppm={ppm}");
            let stats = port.stats.lock().unwrap();
            assert_eq!(stats.decoded_packets, first_failed, "ppm={ppm}");
            assert_eq!(stats.plc_slots, slots - first_failed, "ppm={ppm}");
            assert_eq!(
                stats.late_packets,
                if ppm < 0 { slots - first_failed } else { 0 }
            );
            assert_eq!(
                stats.future_rejected_packets,
                if ppm > 0 { slots - first_failed } else { 0 }
            );
            assert_eq!(stats.arrival_after_nominal_due_packets, stats.late_packets);
            assert_eq!(
                stats.late_before_nominal_due_packets + stats.plc_before_nominal_due_slots,
                0
            );
            assert_eq!(stats.dropped_render, stats.future_rejected_packets * 640);
            assert_eq!(stats.max_future_lead_ms, max_lead);
            assert_eq!(observed_queue, max_queue);
            assert_eq!(
                stats.expired_render_samples + stats.skipped_playout_slots,
                0
            );
            assert_eq!(stats.decoded_packets + stats.plc_slots, slots);
            assert!(received.encoded.is_empty());
        }
    }

    #[test]
    fn authenticated_reports_compensate_forty_five_minutes_without_arrival_rate_sampling() {
        for profile in [LiveProfile::Ms20, LiveProfile::Ms40, LiveProfile::Ms60] {
            for ppm in [-500i64, -100, 100, 500] {
                let origin = Instant::now();
                let measured = Rate {
                    ticks: (48_000 * (1_000_000 + ppm)) as u64,
                    ns: 1_000_000_000_000_000,
                };
                let slots = 45 * 60 * 1000 / u64::from(profile.duration_ms());
                let first_timestamp = u64::from(u32::MAX - 1_000);
                let mut local = LocalClock::new(origin, 1 << 32, first_timestamp as u32);
                local.observe(
                    0,
                    origin - measured.duration(u64::from(profile.rtp_ticks()) - 480),
                    Some(measured),
                );
                let mut sender = dmsg_srtp_sys::Sender::new(&[42; 30], 7).unwrap();
                let mut receiver = dmsg_srtp_sys::Receiver::new(&[42; 30], 7).unwrap();
                let port = Arc::new(test_port());
                let audio = AudioPort { port: port.clone() };
                let mut received = test_received();
                let mut decoder = LiveDecoder::new(profile).unwrap();
                let mut feedback = packet::Feedback::default();
                let opus = [match profile {
                    LiveProfile::Ms20 => 0x48,
                    LiveProfile::Ms40 => 0x50,
                    LiveProfile::Ms60 => 0x58,
                }];
                dmsg_opus_sys::live::validate_packet(profile, &opus).unwrap();
                let (mut next_source, mut queued, mut remainder, mut previous_ns) =
                    (0u64, 0u64, 0u128, 0u64);
                let (mut max_encoded, mut max_native, mut consumed, mut written) =
                    (0usize, 0usize, 0u64, 0u64);
                let mut ppb = 0i64;
                let mut last_cursor = first_timestamp;
                let mut tick = 0u64;
                loop {
                    let ns = tick * 10_000_000;
                    let now = origin + Duration::from_nanos(ns);
                    let consumption =
                        u128::from(ns - previous_ns) * 16_000 * (1_000_000_000 + ppb) as u128
                            + remainder;
                    let samples = (consumption / 1_000_000_000_000_000_000) as u64;
                    remainder = consumption % 1_000_000_000_000_000_000;
                    let drained = queued.min(samples);
                    queued -= drained;
                    consumed += drained;
                    previous_ns = ns;
                    // Independent source arrivals; arrival time does not feed
                    // the estimator. Only protected, fully parsed SR deltas do.
                    while next_source < slots
                        && measured
                            .duration(next_source * u64::from(profile.rtp_ticks()))
                            .as_nanos()
                            <= u128::from(ns)
                    {
                        let arrival = origin
                            + measured.duration(next_source * u64::from(profile.rtp_ticks()));
                        received.admit(
                            first_timestamp + next_source * u64::from(profile.rtp_ticks()),
                            &opus,
                            arrival,
                            profile,
                            &mut feedback,
                            &port,
                        );
                        next_source += 1;
                        max_encoded = max_encoded.max(received.encoded.len());
                    }
                    if tick % 20 == 0 && next_source != 0 && next_source < slots {
                        let report = local
                            .report(now, next_source as u32, next_source as u32)
                            .unwrap();
                        let plain = packet::compound(packet::Report {
                            sender_ssrc: 7,
                            peer_ssrc: 8,
                            cname: b"test0001",
                            sender: Some((report.ntp, report.rtp, report.packets, report.octets)),
                            feedback: packet::Feedback::default(),
                        });
                        assert_eq!(plain.len(), 116);
                        let protected = sender.protect_rtcp(&plain).unwrap();
                        assert_eq!(protected.len() + packet::FRAMING_BYTES, 152);
                        let plain = receiver.unprotect_rtcp(&protected).unwrap();
                        let report = packet::read_compound(&plain, 7, 8, b"test0001").unwrap();
                        received.report_clock(report, now, &port);
                        ppb = received.remote_clock.rate.ppb();
                        assert!(audio.sink_rate(ppb));
                    }
                    port.sink_queue.store(queued, Ordering::Relaxed);
                    if received.playout.as_ref().is_some_and(|clock| {
                        clock.cursor < first_timestamp + slots * u64::from(profile.rtp_ticks())
                    }) {
                        received
                            .tick(now, profile, &mut decoder, &mut feedback, &port)
                            .unwrap();
                    }
                    let mut render = port.render.lock().unwrap();
                    max_native = max_native.max(render.pcm.len());
                    let mut batch = [0; 160];
                    while queued + 160 <= 640 && !render.pcm.is_empty() {
                        let (count, expired) = render.pull(now, &mut batch);
                        assert_eq!(
                            expired,
                            0,
                            "profile={} ppm={ppm} tick={tick}",
                            profile.duration_ms()
                        );
                        queued += count as u64;
                        written += count as u64;
                    }
                    assert!(queued <= 640 && render.pcm.len() <= profile.samples());
                    drop(render);
                    if let Some(clock) = &received.playout {
                        assert!(clock.cursor >= last_cursor);
                        last_cursor = clock.cursor;
                        assert_eq!(
                            received.first_playout,
                            Some((first_timestamp, origin + Duration::from_millis(80)))
                        );
                    }
                    assert!(received.encoded.len() <= 200 / profile.duration_ms() as usize + 1);
                    if consumed == slots * profile.samples() as u64 {
                        break;
                    }
                    // This is a finite virtual input, not another absent frame
                    // after its declared end. A failed model must fail, not hang.
                    assert!(ns < 2_705_000_000_000, "model did not finish profile={} ppm={ppm} source={next_source} written={written} consumed={consumed} stats={}", profile.duration_ms(), serde_json::to_string(&*port.stats.lock().unwrap()).unwrap());
                    tick += 1;
                }
                let stats = port.stats.lock().unwrap();
                assert_eq!(
                    stats.decoded_packets,
                    slots,
                    "profile={} ppm={ppm}",
                    profile.duration_ms()
                );
                assert_eq!(
                    stats.plc_slots
                        + stats.late_packets
                        + stats.future_rejected_packets
                        + stats.skipped_playout_slots
                        + stats.dropped_render,
                    0,
                    "profile={} ppm={ppm}",
                    profile.duration_ms()
                );
                assert!(stats.remote_clock_calibrated && stats.remote_clock_valid);
                assert_eq!(stats.remote_clock_rejected_reports, 0);
                assert_eq!(written, slots * profile.samples() as u64);
                assert!(
                    max_encoded <= 200 / profile.duration_ms() as usize + 1
                        && max_native <= profile.samples()
                );
                assert!(
                    received.encoded.is_empty()
                        && port.render.lock().unwrap().pcm.is_empty()
                        && queued == 0
                );
            }
        }
    }

    #[test]
    fn sink_rate_bounds_default_health_and_lead_are_explicit() {
        let stats = Stats::default();
        assert_eq!(
            (stats.remote_clock_ticks, stats.remote_clock_ns),
            (48_000, 1_000_000_000)
        );
        assert!(!stats.remote_clock_calibrated && !stats.remote_clock_valid);
        let (audio, _) = test_audio();
        assert_eq!(audio.port.sink_rate.load(Ordering::Relaxed), 0);
        for ppb in [-1_000_000, -500_000, 0, 500_000, 1_000_000] {
            assert!(audio.sink_rate(ppb));
            let lead = clock::sink_lead(640, ppb);
            assert_eq!(
                lead.as_nanos(),
                (640u128 * 1_000_000_000 * 1_000_000_000)
                    .div_ceil(16_000 * (1_000_000_000 + ppb) as u128)
            );
        }
        assert!(!audio.sink_rate(-1_000_001) && !audio.sink_rate(1_000_001));
        assert_eq!(audio.port.sink_rate.load(Ordering::Relaxed), 1_000_000);
        audio.port.closed.store(true, Ordering::Relaxed);
        assert!(!audio.sink_rate(0));
    }

    #[test]
    fn sender_report_uses_absolute_sample_time_not_the_application_floor() {
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        let entry = Instant::now();
        let completed = entry + Duration::from_millis(8);
        let mut timestamp = aged_record_timestamp(3_200, 80);
        timestamp.observed_ns += 8_000_000;
        assert!(audio.push_recorded_observed(&[1; 160], timestamp, entry, completed));
        let captured = input.try_recv().unwrap();
        assert_eq!(
            captured.ages(completed),
            (Duration::from_millis(88), Duration::from_millis(38))
        );
        assert_eq!(
            captured.source_time(),
            Some(completed - Duration::from_millis(88))
        );
        let span = captured.capture_clock.unwrap();
        let mut local = LocalClock::new(completed, 1 << 32, 7);
        local.observe(
            captured.position,
            captured.source_time().unwrap(),
            Some(Rate {
                ticks: span.frames * 3,
                ns: span.elapsed_ns,
            }),
        );
        assert!(local.report(completed, 7, 321).is_none());
        // A later full hardware window enables SR. Keep the same measured
        // ratio and original absolute source reference, not a newer age floor.
        let elapsed = Duration::from_nanos(span.elapsed_ns * 99);
        let now = completed + elapsed;
        local.observe(
            captured.position + span.frames * 99,
            captured.source_time().unwrap() + elapsed,
            Some(Rate {
                ticks: span.frames * 3 * 100,
                ns: span.elapsed_ns * 100,
            }),
        );
        let report = local.report(now, 7, 321).unwrap();
        assert_eq!(
            report.rtp,
            7 + 3_200 * 3 + 88 * 48 + Rate::default().ticks_at(elapsed) as u32
        );
        assert_eq!((report.packets, report.octets), (7, 321));
        assert_eq!(
            local
                .report(now + Duration::from_millis(2), 7, 321)
                .unwrap()
                .rtp,
            report.rtp + 96
        );
    }

    #[test]
    fn startup_hardware_quantization_waits_for_a_long_source_span_before_sr() {
        let mut outcomes = Vec::new();
        for ppm in [-500i64, -100, 100, 500] {
            let origin = Instant::now();
            let mut hardware = RecordClock::default();
            let first = CaptureTimestamp {
                read_position: 0,
                frame_position: 800,
                nano_time: 1_000_800_000,
                observed_ns: 1_001_000_000,
            };
            assert!(hardware.observe(first, 0, 160).is_some());
            let mut local = LocalClock::new(origin, 1 << 32, 7);
            let mut remote = RemoteClock::default();
            let mut sender = dmsg_srtp_sys::Sender::new(&[42; 30], 7).unwrap();
            let mut receiver = dmsg_srtp_sys::Receiver::new(&[42; 30], 7).unwrap();
            let (mut first_sr, mut first_valid, mut rejected) = (None, None, 0u64);
            for millis in (0..=60_000u64).step_by(200) {
                // Both raw hardware points pass the existing validity checks.
                // <=0.8ms timestamp quantization over short startup spans is
                // allowed by those checks, but is not a certified SR frequency.
                let elapsed_ns = (millis + 200) * 1_000_000;
                let frames = (u128::from(millis + 200) * 16_000 * (1_000_000 + ppm) as u128
                    / 1_000_000_000) as u64;
                let error = if millis < 3_000 {
                    if millis % 400 == 0 {
                        800_000
                    } else {
                        -800_000
                    }
                } else {
                    0
                };
                let timestamp = CaptureTimestamp {
                    read_position: frames as i64,
                    frame_position: 800 + frames as i64,
                    nano_time: 1_000_000_000 + elapsed_ns as i64 + error,
                    observed_ns: 1_001_000_000 + elapsed_ns as i64,
                };
                let (age, _) = hardware.observe(timestamp, frames, 160).unwrap();
                let span = hardware.span().unwrap();
                let now = origin + Duration::from_millis(millis);
                local.observe(
                    frames,
                    now - age,
                    Some(Rate {
                        ticks: span.frames * 3,
                        ns: span.elapsed_ns,
                    }),
                );
                let source = local.report(now, millis as u32 / 40 + 1, 123);
                let plain = packet::compound(packet::Report {
                    sender_ssrc: 7,
                    peer_ssrc: 8,
                    cname: b"test0001",
                    sender: source
                        .map(|report| (report.ntp, report.rtp, report.packets, report.octets)),
                    feedback: packet::Feedback {
                        terminal: Some(9),
                        ..packet::Feedback::default()
                    },
                });
                let expected = if source.is_some() { 116 } else { 96 };
                assert_eq!(plain.len(), expected);
                let protected = sender.protect_rtcp(&plain).unwrap();
                assert_eq!(protected.len() + packet::FRAMING_BYTES, expected + 36);
                let plain = receiver.unprotect_rtcp(&protected).unwrap();
                let parsed = packet::read_compound(&plain, 7, 8, b"test0001").unwrap();
                assert_eq!(parsed.terminal, Some(9));
                if let Some(report) = parsed.sender_clock {
                    first_sr.get_or_insert(millis);
                    rejected += u64::from(!remote.observe(report, now));
                    if remote.valid(now) {
                        first_valid.get_or_insert(millis);
                    }
                }
            }
            outcomes.push((ppm, first_sr, first_valid, rejected, remote.rate.ppb()));
            assert!(remote.valid(origin + Duration::from_secs(60)));
        }
        assert!(
            outcomes.iter().all(|(ppm, sr, valid, rejected, ppb)| {
                *sr == Some(20_000)
                    && *valid == Some(40_000)
                    && *rejected == 0
                    && ppb.abs_diff(ppm * 1_000) <= 2_000
            }),
            "startup clock outcomes: {outcomes:?}"
        );
    }

    #[test]
    fn ongoing_hardware_quantization_does_not_jump_authenticated_source_phase() {
        let mut outcomes = Vec::new();
        for ppm in [-500i64, -100, 100, 500] {
            let origin = Instant::now();
            let mut hardware = RecordClock::default();
            assert!(hardware
                .observe(
                    CaptureTimestamp {
                        read_position: 0,
                        frame_position: 800,
                        nano_time: 1_000_000_000,
                        observed_ns: 1_001_000_000,
                    },
                    0,
                    160,
                )
                .is_some());
            let mut local = LocalClock::new(origin, 1 << 32, u32::MAX - 1_000);
            let mut remote = RemoteClock::default();
            let mut sender = dmsg_srtp_sys::Sender::new(&[44; 30], 7).unwrap();
            let mut receiver = dmsg_srtp_sys::Receiver::new(&[44; 30], 7).unwrap();
            let (mut rejected, mut first_valid, mut max_phase_error) = (0u64, None, 0u64);
            let mut source_reference = None;
            for millis in (0..=2_700_000u64).step_by(200) {
                // Continue valid sub-ms hardware quantization after source
                // readiness. It is neither a wall jump nor a delivery burst.
                let elapsed_ns = (millis + 200) * 1_000_000;
                let frames = (u128::from(millis + 200) * 16_000 * (1_000_000 + ppm) as u128
                    / 1_000_000_000) as u64;
                let error = if millis <= 20_000 {
                    0
                } else if millis % 400 == 0 {
                    800_000
                } else {
                    -800_000
                };
                let (age, _) = hardware
                    .observe(
                        CaptureTimestamp {
                            read_position: frames as i64,
                            frame_position: 800 + frames as i64,
                            nano_time: 1_000_000_000 + elapsed_ns as i64 + error,
                            observed_ns: 1_001_000_000 + elapsed_ns as i64,
                        },
                        frames,
                        160,
                    )
                    .unwrap();
                let span = hardware.span().unwrap();
                let now = origin + Duration::from_millis(millis);
                let (first_position, first_capture) =
                    *source_reference.get_or_insert((frames, now - age));
                local.observe(
                    frames,
                    now - age,
                    Some(Rate {
                        ticks: span.frames * 3,
                        ns: span.elapsed_ns,
                    }),
                );
                let source = local.report(now, millis as u32 / 40 + 1, 321);
                let plain = packet::compound(packet::Report {
                    sender_ssrc: 7,
                    peer_ssrc: 8,
                    cname: b"test0001",
                    sender: source.map(|r| (r.ntp, r.rtp, r.packets, r.octets)),
                    feedback: packet::Feedback {
                        terminal: Some(9),
                        ..packet::Feedback::default()
                    },
                });
                assert_eq!(plain.len(), if source.is_some() { 116 } else { 96 });
                let protected = sender.protect_rtcp(&plain).unwrap();
                let plain = receiver.unprotect_rtcp(&protected).unwrap();
                let parsed = packet::read_compound(&plain, 7, 8, b"test0001").unwrap();
                assert_eq!(parsed.terminal, Some(9));
                if let Some(report) = parsed.sender_clock {
                    let true_ticks = (now.duration_since(first_capture).as_nanos()
                        * (48_000 * (1_000_000 + ppm)) as u128
                        / 1_000_000_000_000_000) as u64;
                    let expected =
                        (u32::MAX - 1_000).wrapping_add((first_position * 3 + true_ticks) as u32);
                    let error = i64::from(report.rtp.wrapping_sub(expected) as i32).unsigned_abs();
                    max_phase_error = max_phase_error.max(error);
                    rejected += u64::from(!remote.observe(report, now));
                    if remote.valid(now) {
                        first_valid.get_or_insert(millis);
                    }
                }
            }
            outcomes.push((
                ppm,
                rejected,
                remote.rejection_mask,
                first_valid,
                remote.rate.ppb(),
                remote.valid(origin + Duration::from_secs(2_700)),
                max_phase_error,
            ));
        }
        assert!(
            outcomes
                .iter()
                .all(|(ppm, rejected, mask, first, ppb, valid, phase_error)| {
                    *rejected == 0
                    && *mask == 0
                    // The first exact 20s hardware span is at monotonic 19.8s.
                    && *first == Some(39_800)
                    && ppb.abs_diff(ppm * 1_000) <= 2_000
                    && *valid
                    && *phase_error <= 48
                }),
            "ongoing hardware clock outcomes: {outcomes:?}"
        );
    }

    #[test]
    fn remote_clock_snapshot_expires_even_when_media_actor_is_not_ticking() {
        let origin = Instant::now() - Duration::from_secs(25);
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        local.observe(0, origin, None);
        let port = test_port();
        let mut received = test_received();
        for millis in (0..=20_000).step_by(200) {
            let now = origin + Duration::from_millis(millis);
            let report = local.report(now, millis as u32 / 40 + 1, 123).unwrap();
            received.report_clock(
                packet::Feedback {
                    sender_clock: Some(report),
                    ..packet::Feedback::default()
                },
                now,
                &port,
            );
        }
        // The actor's cached sample was fresh at its virtual observation time.
        assert!(port.stats.lock().unwrap().remote_clock_valid);
        let snapshot = port.snapshot();
        assert!(snapshot.remote_clock_calibrated && !snapshot.remote_clock_valid);
        assert_eq!(
            snapshot.remote_clock_ticks * 1_000_000_000 / snapshot.remote_clock_ns,
            48_000
        );
        assert_eq!(snapshot.remote_clock_rejected_reports, 0);
        *port.remote_clock_fresh_until.lock().unwrap() =
            Some(Instant::now() + Duration::from_secs(1));
        port.closed.store(true, Ordering::Relaxed);
        assert!(
            !port.snapshot().remote_clock_valid,
            "retired owners cannot advertise a healthy clock"
        );
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
    fn one_batch_discarding_fixture_is_not_a_bounded_playback_sink() {
        let due = Instant::now();
        let mut output = [0; 160];
        let mut old = RenderQueue::new();
        old.enqueue(due, vec![7; 640]);
        let mut consumed = 0;
        let mut expired = 0;
        for tick in 1..=4 {
            let (count, loss) = old.pull(due + Duration::from_millis(tick * 10), &mut output);
            consumed += count;
            expired += loss;
        }
        assert_eq!((consumed, expired), (480, 160));

        let mut bounded_sink = RenderQueue::new();
        bounded_sink.enqueue(due, vec![7; 640]);
        let mut consumed = 0;
        for _ in 0..4 {
            let (count, loss) = bounded_sink.pull(due + Duration::from_millis(10), &mut output);
            assert_eq!(loss, 0);
            consumed += count;
        }
        assert_eq!(consumed, 640); // fill a40ms sink once; its clock consumes it later
        assert_eq!(bounded_sink.expire(due + Duration::from_millis(40)), 0);
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

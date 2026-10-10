use super::{
    clock::{self, LocalClock, Rate, RemoteClock},
    codec,
    fixture::{self, EndpointFixture, PublicConfig},
    lane::{Commit, InboundFrame, SendRequest, OP_RTCP, OP_RTP},
    packet, relay,
};
use dmsg_opus_sys::live::LiveProfile;
#[cfg(test)]
use dmsg_opus_sys::live::{LiveDecoder, LiveEncoder};
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
// Bounded compute headroom for authenticated PRESENT packets. It is not a
// missing-packet retirement allowance or a later presentation/expiry deadline.
const KNOWN_PACKET_COMPUTE: Duration = Duration::from_millis(20);
const SAMPLE_NS: u64 = 1_000_000_000 / 16_000;
// Application backlog above the frozen observed initial hardware-age floor.
const CAPTURE_MAX_AGE: Duration = Duration::from_millis(40);
// Inclusive upper bounds in microseconds; the final bin is unbounded.
const TIMING_BOUNDS_US: [u64; 7] = [1_000, 2_000, 5_000, 10_000, 20_000, 40_000, 80_000];

fn observe_duration(buckets: &mut [u64; 8], duration: Duration) {
    let us = duration.as_nanos().div_ceil(1_000);
    let index = TIMING_BOUNDS_US
        .iter()
        .position(|bound| us <= u128::from(*bound))
        .unwrap_or(7);
    buckets[index] = buckets[index].saturating_add(1);
}
#[cfg(any(target_os = "android", test))]
const CAPTURE_CALIBRATION_BATCHES: u8 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderExpiryKind {
    Queued,
    Completion,
}

/// One local monotonic observation, not AudioTrack submission or presentation.
/// Signed differences are truncated toward zero to microseconds. Unknown times
/// remain null; no absolute timestamp, source identifier or PCM is serialized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RenderExpiry {
    pub kind: RenderExpiryKind,
    pub publication_vs_start_us: Option<i64>,
    pub codec_duration_us: Option<u64>,
    pub first_pull_vs_start_us: Option<i64>,
    pub last_pull_vs_start_us: Option<i64>,
    pub successful_pull_calls: u64,
    pub expiry_vs_end_us: i64,
    pub initial_samples: u64,
    /// Samples returned by successful native pulls, not hardware-consumed PCM.
    pub transferred_samples: u64,
    pub discarded_samples: u64,
    pub source_frame_duration_us: u64,
    /// Last declared values read at accounting, not a new hardware observation.
    pub sink_queue_samples: u64,
    pub sink_rate_ppb: i64,
}

fn relative_micros(at: Instant, reference: Instant) -> i64 {
    let distance = if at >= reference {
        at - reference
    } else {
        reference - at
    };
    let magnitude = distance.as_micros().min(i64::MAX as u128) as i64;
    if at >= reference {
        magnitude
    } else {
        -magnitude
    }
}

/// Last fully authenticated/profile/frontier-valid control, observed locally.
/// A sender-clock flag means SR metadata was present, not a healthy rate estimate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProgressControl {
    pub terminal: Option<u64>,
    pub sender_clock_present: bool,
    pub body_to_noise_us: Option<u64>,
    pub noise_to_processed_us: Option<u64>,
    pub since_processed_us: Option<u64>,
    pub since_noise_us: Option<u64>,
    pub since_body_us: Option<u64>,
}

/// The original decision site, not a new admission stage or retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressCheckSite {
    TopTurn,
    MediaAdmission,
    ReceiptCommit,
}

/// One replaced-on-failure observation, not a new credit/deadline decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProgressFailure {
    pub reason: packet::ProgressFailureReason,
    pub check_site: ProgressCheckSite,
    /// Only a receipt-commit check attempts to insert a new ledger entry.
    pub attempted_index: Option<u64>,
    pub frame_bytes: Option<u64>,
    pub ledger_bytes: u64,
    pub window_bytes: u64,
    pub entry_count: u64,
    pub oldest_index: Option<u64>,
    pub oldest_age_us: Option<u64>,
    pub highest_committed_index: Option<u64>,
    pub terminal_frontier: Option<u64>,
    pub max_age_us: u64,
    pub waiting_bytes: Option<u64>,
    /// Receipts still unreaped, excluding the consumed receipt being attempted.
    pub pending_bytes: u64,
    pub pending_receipt_count: u64,
    /// Existing processed Noise commitment counter, modulo 2^32 like RTCP.
    pub committed_receipt_count: u32,
    /// Exact charge: zero at top, waiting+pending at admission, frame at commit.
    pub check_extra_bytes: u64,
    pub total_check_bytes: u64,
    pub last_authenticated_control: Option<ProgressControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthenticatedControl {
    terminal: Option<u64>,
    sender_clock_present: bool,
    body_complete: Instant,
    noise_done: Instant,
    processed: Instant,
}

fn elapsed_micros(at: Instant, reference: Instant) -> Option<u64> {
    at.checked_duration_since(reference)
        .map(|elapsed| elapsed.as_micros().min(u128::from(u64::MAX)) as u64)
}

impl AuthenticatedControl {
    fn observe(self, now: Instant) -> ProgressControl {
        ProgressControl {
            terminal: self.terminal,
            sender_clock_present: self.sender_clock_present,
            body_to_noise_us: elapsed_micros(self.noise_done, self.body_complete),
            noise_to_processed_us: elapsed_micros(self.processed, self.noise_done),
            since_processed_us: elapsed_micros(now, self.processed),
            since_noise_us: elapsed_micros(now, self.noise_done),
            since_body_us: elapsed_micros(now, self.body_complete),
        }
    }
}

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
    pub last_render_expiry: Option<RenderExpiry>,
    pub last_progress_failure: Option<ProgressFailure>,
    pub decode_after_nominal_due_slots: u64,
    pub max_decode_us: u64,
    /// Fixed duration bins bracket only the named synchronous operation.
    pub encode_duration_bins: [u64; 8],
    pub decode_duration_bins: [u64; 8],
    pub plc_duration_bins: [u64; 8],
    /// Rust Noise completion to endpoint dequeue; not physical DNS arrival.
    pub rx_post_noise_wait_bins: [u64; 8],
    pub rx_validation_duration_bins: [u64; 8],
    pub source_ready_to_commit_bins: [u64; 8],
    pub max_rx_noise_auth_us: u64,
    pub max_rx_post_noise_wait_us: u64,
    pub max_rx_validation_us: u64,
    pub noise_after_nominal_due_packets: u64,
    pub noise_before_due_admitted_after_due_packets: u64,
    pub max_source_ready_to_commit_us: u64,
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
            last_render_expiry: None,
            last_progress_failure: None,
            decode_after_nominal_due_slots: 0,
            max_decode_us: 0,
            encode_duration_bins: [0; 8],
            decode_duration_bins: [0; 8],
            plc_duration_bins: [0; 8],
            rx_post_noise_wait_bins: [0; 8],
            rx_validation_duration_bins: [0; 8],
            source_ready_to_commit_bins: [0; 8],
            max_rx_noise_auth_us: 0,
            max_rx_post_noise_wait_us: 0,
            max_rx_validation_us: 0,
            noise_after_nominal_due_packets: 0,
            noise_before_due_admitted_after_due_packets: 0,
            max_source_ready_to_commit_us: 0,
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
    start_due: Option<Instant>,
    end_due: Option<Instant>,
    source_end: Option<u64>,
    frame_samples: usize,
    rate: Rate,
    published_at: Option<Instant>,
    codec_duration: Option<Duration>,
    first_pull: Option<Instant>,
    last_pull: Option<Instant>,
    successful_pull_calls: u64,
}

impl RenderQueue {
    fn new() -> Self {
        Self {
            pcm: VecDeque::with_capacity(640),
            start_due: None,
            end_due: None,
            source_end: None,
            frame_samples: 0,
            rate: Rate::default(),
            published_at: None,
            codec_duration: None,
            first_pull: None,
            last_pull: None,
            successful_pull_calls: 0,
        }
    }

    #[cfg(test)]
    fn enqueue(&mut self, due: Instant, pcm: Vec<i16>) {
        self.enqueue_observed(due, pcm, None, None);
    }

    fn enqueue_observed(
        &mut self,
        due: Instant,
        pcm: Vec<i16>,
        published_at: Option<Instant>,
        codec_duration: Option<Duration>,
    ) {
        debug_assert!(self.pcm.is_empty()); // One decoded frame, including PLC.
        self.start_due = Some(due);
        self.end_due = Some(due + Duration::from_nanos(pcm.len() as u64 * SAMPLE_NS));
        self.frame_samples = pcm.len();
        self.rate = Rate::default();
        self.published_at = published_at;
        self.codec_duration = codec_duration;
        self.first_pull = None;
        self.last_pull = None;
        self.successful_pull_calls = 0;
        self.pcm.extend(pcm);
    }

    // JNI transfers whole batches, not samples at their nominal presentation
    // instants. Expire the remaining frame at its immutable source end, rather
    // than trimming prefixes using the unrelated early-PLC preparation reserve.
    #[cfg(test)]
    fn expire(&mut self, now: Instant) -> usize {
        self.expire_observed(now).0
    }

    fn expire_observed(&mut self, now: Instant) -> (usize, Option<RenderExpiry>) {
        let Some(end) = self.end_due else {
            return (0, None);
        };
        if now < end {
            return (0, None);
        }
        let count = self.pcm.len();
        let trace = (count != 0).then(|| {
            let start = self.start_due.expect("probe render source start");
            RenderExpiry {
                kind: RenderExpiryKind::Queued,
                publication_vs_start_us: self.published_at.map(|at| relative_micros(at, start)),
                codec_duration_us: self
                    .codec_duration
                    .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64),
                first_pull_vs_start_us: self.first_pull.map(|at| relative_micros(at, start)),
                last_pull_vs_start_us: self.last_pull.map(|at| relative_micros(at, start)),
                successful_pull_calls: self.successful_pull_calls,
                expiry_vs_end_us: relative_micros(now, end),
                initial_samples: self.frame_samples as u64,
                transferred_samples: (self.frame_samples - count) as u64,
                discarded_samples: count as u64,
                source_frame_duration_us: end
                    .saturating_duration_since(start)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
                sink_queue_samples: 0,
                sink_rate_ppb: 0,
            }
        });
        self.clear();
        (count, trace)
    }

    #[cfg(test)]
    fn pull(&mut self, now: Instant, pcm: &mut [i16]) -> (usize, usize) {
        self.pull_with_lead(now, Duration::ZERO, pcm)
    }

    #[cfg(test)]
    fn pull_with_lead(&mut self, now: Instant, lead: Duration, pcm: &mut [i16]) -> (usize, usize) {
        let (count, expired, _) = self.pull_observed(now, lead, pcm);
        (count, expired)
    }

    fn pull_observed(
        &mut self,
        now: Instant,
        lead: Duration,
        pcm: &mut [i16],
    ) -> (usize, usize, Option<RenderExpiry>) {
        let (expired, trace) = self.expire_observed(now);
        if self.pcm.is_empty() {
            return (0, expired, trace);
        }
        // Computing early must not present early. Project the next untransferred
        // sample from the immutable frame start, including through partial pulls.
        let offset = self.frame_samples - self.pcm.len();
        let start = self.start_due.unwrap() + self.rate.duration(offset as u64 * 3);
        if now + lead < start {
            return (0, expired, trace);
        }
        let count = pcm.len().min(self.pcm.len());
        for sample in &mut pcm[..count] {
            *sample = self.pcm.pop_front().unwrap();
        }
        if count != 0 {
            self.first_pull.get_or_insert(now);
            self.last_pull = Some(now);
            self.successful_pull_calls = self.successful_pull_calls.saturating_add(1);
        }
        if self.pcm.is_empty() {
            self.start_due = None;
            self.end_due = None;
            self.source_end = None;
            self.frame_samples = 0;
            self.published_at = None;
            self.codec_duration = None;
            self.first_pull = None;
            self.last_pull = None;
            self.successful_pull_calls = 0;
        }
        (count, expired, trace)
    }

    fn clear(&mut self) {
        for sample in self.pcm.iter_mut() {
            sample.zeroize();
        }
        self.pcm.clear();
        self.start_due = None;
        self.end_due = None;
        self.source_end = None;
        self.frame_samples = 0;
        self.published_at = None;
        self.codec_duration = None;
        self.first_pull = None;
        self.last_pull = None;
        self.successful_pull_calls = 0;
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
    fn expired_render(&self, samples: usize, trace: Option<RenderExpiry>) {
        if samples != 0 {
            // The caller builds the value under the render lock, then releases
            // it before accounting. Never reacquire render from the stats path.
            let trace = trace.map(|mut trace| {
                trace.sink_queue_samples = self.sink_queue.load(Ordering::Relaxed);
                trace.sink_rate_ppb = self.sink_rate.load(Ordering::Relaxed);
                trace
            });
            self.stats(|stats| {
                stats.expired_render_samples += samples as u64;
                stats.dropped_render += samples as u64;
                if let Some(trace) = trace {
                    stats.last_render_expiry = Some(trace);
                }
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
        self.pull_at(Instant::now(), pcm)
    }

    /// Read-only diagnostic; a busy or closed port is an unknown observation.
    #[cfg(any(target_os = "android", test))]
    pub(crate) fn render_expired_samples(&self) -> Option<u64> {
        if self.port.closed.load(Ordering::Relaxed) {
            return None;
        }
        self.port
            .stats
            .try_lock()
            .ok()
            .map(|stats| stats.expired_render_samples)
    }

    fn pull_at(&self, now: Instant, pcm: &mut [i16]) -> usize {
        if pcm.is_empty() || pcm.len() > 160 || self.port.closed.load(Ordering::Relaxed) {
            return 0;
        }
        let Ok(mut queue) = self.port.render.try_lock() else {
            return 0;
        };
        let lead = clock::sink_lead(
            self.port.sink_queue.load(Ordering::Relaxed) as usize,
            self.port.sink_rate.load(Ordering::Relaxed),
        );
        let (count, expired, trace) = queue.pull_observed(now, lead, pcm);
        drop(queue);
        self.port.expired_render(expired, trace);
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
    source_ready: Instant,
    receipt: oneshot::Receiver<Commit>,
}

struct WaitingPacket {
    opus: Zeroizing<Vec<u8>>,
    position: u64,
    pushed_at: Instant,
    encoded_at: Instant,
    captured_at: Instant,
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
            captured_at,
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

    fn source_ready(&self, profile: LiveProfile) -> Instant {
        self.captured_at
            + self
                .capture_clock
                .unwrap_or_default()
                .duration(profile.samples() as u64)
                .expect("bounded source-frame duration")
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
    #[cfg(test)]
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
        self.stage_reserved(packet, permit, commits, now, profile, port)
    }

    fn stage_reserved(
        &mut self,
        packet: WaitingPacket,
        permit: mpsc::OwnedPermit<SendRequest>,
        commits: usize,
        now: Instant,
        profile: LiveProfile,
        port: &Port,
    ) -> Result<(), String> {
        if now >= packet.deadline || self.pending.is_some() || commits >= 2 {
            port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
            return Ok(());
        }
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

struct DecodeWork {
    timestamp: u64,
    end: u64,
    plc: bool,
    opus: Option<Zeroizing<Vec<u8>>>,
}

struct EncodeSource {
    position: u64,
    captured_at: Instant,
    pushed_at: Instant,
    clock: Option<CaptureClockSpan>,
    permit: mpsc::OwnedPermit<SendRequest>,
}

// The existing assembled source frame, not another capture/job queue. It may
// remain full while the one codec owner serves the current receive frame.
struct CaptureFrame {
    pcm: Zeroizing<Vec<i16>>,
    expected_position: u64,
    position: u64,
    captured_at: Instant,
    pushed_at: Instant,
    clock: Option<CaptureClockSpan>,
    port: Arc<Port>,
}

impl CaptureFrame {
    fn new(samples: usize, port: Arc<Port>) -> Self {
        let now = Instant::now();
        Self {
            pcm: Zeroizing::new(Vec::with_capacity(samples)),
            expected_position: 0,
            position: 0,
            captured_at: now,
            pushed_at: now,
            clock: None,
            port,
        }
    }

    fn discard(&mut self, additional: usize) {
        let count = self.pcm.len() + additional;
        self.pcm.zeroize();
        self.pcm.clear();
        self.port
            .stats(|stats| stats.dropped_capture += count as u64);
    }

    fn full(&self, profile: LiveProfile) -> bool {
        self.pcm.len() == profile.samples()
    }

    fn can_ingest(
        &self,
        profile: LiveProfile,
        flight: Option<&CodecFlight>,
        admission: &MediaAdmission,
        outgoing: &mpsc::Sender<SendRequest>,
        commits: usize,
    ) -> bool {
        // Decode owns only the render slot. Its held operation must not age
        // valid input in the ring while the original source frame is free.
        // Encode (including its unconsumed result) still owns that source slot.
        let source_free = match flight {
            None => true,
            Some(flight) => matches!(flight.source.as_ref(), Some(CodecSource::Decode(_))),
        };
        source_free
            && !self.full(profile)
            && admission.pending.is_none()
            && outgoing.capacity() > 0
            && commits < 2
    }

    fn ingest(&mut self, captured: Captured, ready: bool, now: Instant, profile: LiveProfile) {
        debug_assert!(!self.full(profile));
        let (hardware_age, capture_age) = captured.ages(now);
        self.port.stats(|stats| {
            stats.max_capture_age_us = stats
                .max_capture_age_us
                .max(hardware_age.as_micros() as u64);
            if captured.hardware_age.is_some() {
                stats.max_additional_capture_age_us = stats
                    .max_additional_capture_age_us
                    .max(capture_age.as_micros() as u64);
            }
        });
        if !ready || capture_age > CAPTURE_MAX_AGE {
            self.discard(captured.len);
            self.expected_position = captured.position + captured.len as u64;
            self.port.stats(|stats| {
                stats.capture_age_rejected_batches += u64::from(capture_age > CAPTURE_MAX_AGE)
            });
            return;
        }
        if captured.position != self.expected_position {
            self.discard(0);
            self.port.stats(|stats| stats.capture_gap_batches += 1);
        }
        self.expected_position = captured.position + captured.len as u64;
        let mut offset = 0;
        if self.pcm.is_empty() {
            let residue = captured.position % profile.samples() as u64;
            if residue != 0 {
                offset = (profile.samples() as u64 - residue).min(captured.len as u64) as usize;
            }
            self.position = captured.position + offset as u64;
            self.captured_at = captured.at;
            self.pushed_at = captured.pushed_at;
            self.clock = captured.capture_clock;
        }
        self.port
            .stats(|stats| stats.dropped_capture += offset as u64);
        self.pcm
            .extend_from_slice(&captured.pcm[offset..captured.len]);
    }
}

impl Drop for CaptureFrame {
    fn drop(&mut self) {
        self.port
            .stats(|stats| stats.dropped_capture += self.pcm.len() as u64);
    }
}

enum CodecSource {
    Encode(EncodeSource),
    Decode(DecodeWork),
}

// A cancelled endpoint future also releases the existing slot and accounts its
// source loss. Owner::drop joins the C owner before the native carrier can retire.
struct CodecFlight {
    source: Option<CodecSource>,
    port: Arc<Port>,
    samples: usize,
}

impl Drop for CodecFlight {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            self.port.stats(|stats| match source {
                CodecSource::Encode(_) => stats.dropped_capture += self.samples as u64,
                CodecSource::Decode(_) => stats.dropped_render += self.samples as u64,
            });
        }
    }
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
        if available {
            // The existing native frame may hold computed PCM until its actual
            // sink handoff is due. No added PCM/job queue or presentation delay.
            now + lead + KNOWN_PACKET_COMPUTE >= self.due
        } else {
            // Preserve the original missing-packet decision and 10ms allowance.
            now + lead >= self.due && now + PREPARATION >= self.due
        }
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

#[derive(Clone, Copy)]
struct ReceiveTiming {
    body_complete: Instant,
    noise_done: Instant,
    dequeued: Instant,
}

impl ReceiveTiming {
    fn record(self, admitted: Instant, due: Instant, port: &Port) {
        let noise = self
            .noise_done
            .saturating_duration_since(self.body_complete);
        let wait = self.dequeued.saturating_duration_since(self.noise_done);
        let validation = admitted.saturating_duration_since(self.dequeued);
        port.stats(|stats| {
            observe_duration(&mut stats.rx_post_noise_wait_bins, wait);
            observe_duration(&mut stats.rx_validation_duration_bins, validation);
            stats.max_rx_noise_auth_us = stats.max_rx_noise_auth_us.max(noise.as_micros() as u64);
            stats.max_rx_post_noise_wait_us =
                stats.max_rx_post_noise_wait_us.max(wait.as_micros() as u64);
            stats.max_rx_validation_us = stats
                .max_rx_validation_us
                .max(validation.as_micros() as u64);
            stats.noise_after_nominal_due_packets += u64::from(self.noise_done > due);
            stats.noise_before_due_admitted_after_due_packets +=
                u64::from(self.noise_done <= due && admitted > due);
        });
    }
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
                render.start_due =
                    Some(self.source_due(timestamp - render.frame_samples as u64 * 3));
                render.end_due = Some(self.source_due(timestamp));
                render.rate = self.remote_clock.rate;
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
        incoming: &mut mpsc::Receiver<Result<InboundFrame, String>>,
        receiver: &mut dmsg_srtp_sys::Receiver,
        fixture: &EndpointFixture,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        match incoming.try_recv() {
            Ok(Ok(frame)) => self.receive_lane(frame, receiver, fixture, feedback, port),
            Err(mpsc::error::TryRecvError::Empty) => Ok(()),
            _ => Err("probe media lane failed".into()),
        }
    }

    #[cfg(test)]
    fn receive(
        &mut self,
        frame: (u8, Vec<u8>),
        receiver: &mut dmsg_srtp_sys::Receiver,
        fixture: &EndpointFixture,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        self.receive_inner(frame, receiver, fixture, feedback, port, None)
    }

    fn receive_lane(
        &mut self,
        frame: InboundFrame,
        receiver: &mut dmsg_srtp_sys::Receiver,
        fixture: &EndpointFixture,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        let timing = ReceiveTiming {
            body_complete: frame.body_complete,
            noise_done: frame.noise_done,
            dequeued: Instant::now(),
        };
        self.receive_inner(frame.frame, receiver, fixture, feedback, port, Some(timing))
    }

    fn receive_inner(
        &mut self,
        frame: (u8, Vec<u8>),
        receiver: &mut dmsg_srtp_sys::Receiver,
        fixture: &EndpointFixture,
        feedback: &mut packet::Feedback,
        port: &Port,
        timing: Option<ReceiveTiming>,
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
        let admitted = Instant::now();
        if let Some(timing) = timing {
            let due = if self.playout.is_some() {
                self.source_due(timestamp)
            } else {
                admitted + Duration::from_millis(80)
            };
            timing.record(admitted, due, port);
        }
        self.admit(timestamp, opus, admitted, profile, feedback, port);
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

    fn maintain_playout(
        &mut self,
        now: Instant,
        profile: LiveProfile,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> bool {
        self.synchronize_clock(now, port);
        let (expired, trace) = port
            .render
            .lock()
            .expect("probe render owner")
            .expire_observed(now);
        port.expired_render(expired, trace);
        let Some(clock) = &mut self.playout else {
            return false;
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
        }
        while self
            .encoded
            .front()
            .is_some_and(|packet| packet.timestamp < clock.cursor)
        {
            self.encoded.pop_front();
            port.stats(|stats| stats.dropped_render += profile.samples() as u64);
        }
        skipped != 0
    }

    fn decode_ready(&self, now: Instant, port: &Port) -> bool {
        let Some(clock) = &self.playout else {
            return false;
        };
        let available = self
            .encoded
            .front()
            .is_some_and(|packet| packet.timestamp == clock.cursor);
        clock.can_prepare(
            now,
            port.sink_queue.load(Ordering::Relaxed) as usize,
            available,
            port.sink_rate.load(Ordering::Relaxed),
        ) && port
            .render
            .lock()
            .expect("probe render owner")
            .pcm
            .is_empty()
    }

    fn prepare_decode(
        &mut self,
        now: Instant,
        profile: LiveProfile,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Option<DecodeWork> {
        if self.playout.is_some() {
            port.stats(|stats| {
                stats.sink_queue_samples = port.sink_queue.load(Ordering::Relaxed) as usize
            });
        }
        if !self.decode_ready(now, port) {
            return None;
        }
        let clock = self.playout.as_mut()?;
        let available = self
            .encoded
            .front()
            .is_some_and(|packet| packet.timestamp == clock.cursor);
        let opus = if available {
            Some(self.encoded.pop_front().unwrap().opus)
        } else {
            feedback.plc = feedback.plc.saturating_add(1);
            port.stats(|stats| {
                stats.plc_slots += 1;
                stats.plc_before_nominal_due_slots += u64::from(now < clock.due);
            });
            None
        };
        let work = DecodeWork {
            timestamp: clock.cursor,
            end: clock.cursor + u64::from(profile.rtp_ticks()),
            plc: !available,
            opus,
        };
        // Retirement is the one irreversible decode/PLC decision, not the
        // eventual completion. Authentication continues without rewinding it.
        clock.cursor = work.end;
        let (first, due) = self.first_playout.unwrap();
        clock.due = due + self.remote_clock.rate.duration(work.end - first);
        feedback.playout = Some(clock.cursor);
        Some(work)
    }

    fn apply_decode(
        &mut self,
        work: DecodeWork,
        completed: Instant,
        duration: Duration,
        mut output: Zeroizing<Vec<i16>>,
        port: &Port,
    ) -> Result<(), String> {
        let due = self.source_due(work.timestamp);
        let end_due = self.source_due(work.end);
        port.stats(|stats| {
            if work.plc {
                observe_duration(&mut stats.plc_duration_bins, duration);
            } else {
                stats.decoded_packets += 1;
                observe_duration(&mut stats.decode_duration_bins, duration);
            }
            stats.max_decode_us = stats.max_decode_us.max(duration.as_micros() as u64);
            stats.decode_after_nominal_due_slots += u64::from(completed > due);
        });
        if self.playout.as_ref().unwrap().cursor > work.end || completed >= end_due {
            let samples = output.len();
            port.expired_render(
                samples,
                Some(RenderExpiry {
                    kind: RenderExpiryKind::Completion,
                    publication_vs_start_us: Some(relative_micros(completed, due)),
                    codec_duration_us: Some(duration.as_micros().min(u128::from(u64::MAX)) as u64),
                    first_pull_vs_start_us: None,
                    last_pull_vs_start_us: None,
                    successful_pull_calls: 0,
                    expiry_vs_end_us: relative_micros(completed, end_due),
                    initial_samples: samples as u64,
                    transferred_samples: 0,
                    discarded_samples: samples as u64,
                    source_frame_duration_us: end_due
                        .saturating_duration_since(due)
                        .as_micros()
                        .min(u128::from(u64::MAX))
                        as u64,
                    sink_queue_samples: 0,
                    sink_rate_ppb: 0,
                }),
            );
            return Ok(()); // Zeroizing output never reaches AudioPort.
        }
        let mut render = port.render.lock().expect("probe render owner");
        if !render.pcm.is_empty() {
            return Err("probe decoded slot already occupied".into());
        }
        render.enqueue_observed(
            due,
            std::mem::take(&mut *output),
            Some(completed),
            Some(duration),
        );
        render.end_due = Some(end_due);
        render.source_end = Some(work.end);
        render.rate = self.remote_clock.rate;
        let (expired, trace) = render.expire_observed(completed);
        drop(render);
        port.expired_render(expired, trace);
        Ok(())
    }

    #[cfg(test)]
    fn tick(
        &mut self,
        now: Instant,
        profile: LiveProfile,
        decoder: &mut LiveDecoder,
        feedback: &mut packet::Feedback,
        port: &Port,
    ) -> Result<(), String> {
        let started = Instant::now();
        if self.maintain_playout(now, profile, feedback, port) {
            *decoder = LiveDecoder::new(profile)?;
        }
        let Some(mut work) = self.prepare_decode(now, profile, feedback, port) else {
            return Ok(());
        };
        let codec_started = Instant::now();
        let output = match work.opus.take() {
            Some(opus) => decoder.decode(&opus)?,
            None => decoder.conceal()?,
        };
        self.apply_decode(
            work,
            now + started.elapsed(),
            codec_started.elapsed(),
            Zeroizing::new(output),
            port,
        )
    }
}

struct SendCounters {
    timestamp: u32,
    packets: u32,
    octets: u32,
    since_report: bool,
    last_control: Option<AuthenticatedControl>,
}

fn progress_failure_observation(
    failure: &packet::LedgerFailure,
    check_site: ProgressCheckSite,
    checked_at: Instant,
    attempted: Option<(u64, usize)>,
    waiting_bytes: Option<usize>,
    pending: (usize, usize),
    sent: &SendCounters,
    extra_bytes: usize,
) -> ProgressFailure {
    let view = &failure.observation;
    ProgressFailure {
        reason: failure.reason,
        check_site,
        attempted_index: attempted.map(|(index, _)| index),
        frame_bytes: attempted.map(|(_, bytes)| bytes as u64),
        ledger_bytes: view.bytes as u64,
        window_bytes: view.window as u64,
        entry_count: view.entries as u64,
        oldest_index: view.oldest_index,
        oldest_age_us: view
            .oldest_age
            .map(|age| age.as_micros().min(u128::from(u64::MAX)) as u64),
        highest_committed_index: view.highest,
        terminal_frontier: view.terminal,
        max_age_us: view.max_age.as_micros().min(u128::from(u64::MAX)) as u64,
        waiting_bytes: waiting_bytes.map(|bytes| bytes as u64),
        pending_bytes: pending.0 as u64,
        pending_receipt_count: pending.1 as u64,
        committed_receipt_count: sent.packets,
        check_extra_bytes: extra_bytes as u64,
        total_check_bytes: (view.bytes + extra_bytes) as u64,
        last_authenticated_control: sent.last_control.map(|control| control.observe(checked_at)),
    }
}

fn reap_commits(
    commits: &mut VecDeque<PendingCommit>,
    admission: &mut MediaAdmission,
    ledger: &mut packet::Ledger,
    port: &Port,
    samples: usize,
    counters: &mut SendCounters,
    waiting_bytes: Option<usize>,
) -> Result<(), String> {
    while let Some(pending) = commits.front_mut() {
        match pending.receipt.try_recv() {
            Ok(Commit::Committed { framed_bytes, at }) => {
                if let Err(error) = ledger.commit_observed(pending.index, framed_bytes, at) {
                    if let packet::LedgerCommitFailure::Progress(failure) = &error {
                        let attempted_index = pending.index;
                        // The front receipt was consumed, but the original failure
                        // leaves its entry in this deque. Observe only the remaining
                        // receipts; its actual framed bytes are the check's charge.
                        let remaining_bytes = commits
                            .iter()
                            .skip(1)
                            .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
                            .sum();
                        let waiting = waiting_bytes.or_else(|| {
                            admission
                                .pending
                                .as_ref()
                                .map(|(waiting, _)| waiting.bytes())
                        });
                        let observation = progress_failure_observation(
                            failure,
                            ProgressCheckSite::ReceiptCommit,
                            at,
                            Some((attempted_index, framed_bytes)),
                            waiting,
                            (remaining_bytes, commits.len() - 1),
                            counters,
                            framed_bytes,
                        );
                        port.stats(|stats| stats.last_progress_failure = Some(observation));
                    }
                    return Err(error.message().into());
                }
                admission.committed(at, samples);
                counters.timestamp = pending.timestamp;
                counters.packets = counters.packets.wrapping_add(1);
                counters.octets = counters.octets.wrapping_add(pending.opus_bytes as u32);
                counters.since_report = true;
                let soft_late =
                    at.saturating_duration_since(pending.submitted) > Duration::from_millis(20);
                let source_delay = at.saturating_duration_since(pending.source_ready);
                commits.pop_front();
                port.stats(|stats| {
                    stats.tx_bytes += framed_bytes as u64;
                    stats.max_unconfirmed_bytes = stats.max_unconfirmed_bytes.max(ledger.bytes());
                    stats.tx_soft_deadline_packets += u64::from(soft_late);
                    observe_duration(&mut stats.source_ready_to_commit_bins, source_delay);
                    stats.max_source_ready_to_commit_us = stats
                        .max_source_ready_to_commit_us
                        .max(source_delay.as_micros() as u64);
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

fn receive_control(
    frame: Option<Result<InboundFrame, String>>,
    receiver: &mut dmsg_srtp_sys::Receiver,
    fixture: &EndpointFixture,
    commits: &mut VecDeque<PendingCommit>,
    admission: &mut MediaAdmission,
    ledger: &mut packet::Ledger,
    sent: &mut SendCounters,
    received: &mut ReceiveState,
    ready: &mut bool,
    port: &Port,
    // The actual taken waiting slot, if this handler runs during media admission.
    waiting_bytes: Option<usize>,
    // Production observes Instant::now at the original ACK/clock boundaries;
    // tests supply explicit monotonic decision instants without timer sleeps.
    now: impl Fn() -> Instant,
) -> Result<(), String> {
    let InboundFrame {
        frame: (op, cipher),
        body_complete,
        noise_done,
    } = match frame {
        Some(Ok(frame)) => frame,
        _ => return Err("probe control lane failed".into()),
    };
    if op != OP_RTCP {
        return Err("probe control opcode rejected".into());
    }
    let plain = receiver.unprotect_rtcp(&cipher)?;
    let remote = packet::read_compound(
        &plain,
        fixture.ssrc_rx,
        fixture.ssrc_tx,
        &fixture.peer_cname,
    )?;
    // A fully authenticated prefix of already-known commitments can release
    // its credit before an unrelated receipt checks the remaining ledger.
    // The original validator still rejects a stale frontier before any pop.
    let known_prefix = remote.terminal.filter(|index| {
        ledger
            .highest_committed()
            .is_some_and(|highest| *index <= highest)
    });
    let acknowledge = |ledger: &mut packet::Ledger, index| -> Result<(), String> {
        let cycle = ledger.acknowledge(index, now())?;
        port.stats(|stats| {
            stats.terminal_feedback += 1;
            stats.max_feedback_cycle_ms = stats.max_feedback_cycle_ms.max(cycle.as_millis() as u64);
        });
        Ok(())
    };
    if let Some(index) = known_prefix {
        acknowledge(ledger, index)?;
    }
    // A frontier beyond the known highest still requires actual receipts:
    // Noise commitment precedes its socket write, never fabricated credit.
    reap_commits(
        commits,
        admission,
        ledger,
        port,
        fixture::profile(fixture.profile_ms)?.samples(),
        sent,
        waiting_bytes,
    )?;
    if let Some(index) = remote.terminal.filter(|_| known_prefix.is_none()) {
        acknowledge(ledger, index)?;
    }
    let processed = now();
    received.report_clock(remote, processed, port);
    *ready = true;
    port.stats(|stats| {
        stats.ready = true;
        stats.rx_bytes += (cipher.len() + packet::FRAMING_BYTES) as u64;
    });
    sent.last_control = Some(AuthenticatedControl {
        terminal: remote.terminal,
        sender_clock_present: remote.sender_clock.is_some(),
        body_complete,
        noise_done,
        processed,
    });
    Ok(())
}

fn control_progress(
    incoming: &mut mpsc::Receiver<Result<InboundFrame, String>>,
    receiver: &mut dmsg_srtp_sys::Receiver,
    fixture: &EndpointFixture,
    commits: &mut VecDeque<PendingCommit>,
    admission: &mut MediaAdmission,
    ledger: &mut packet::Ledger,
    sent: &mut SendCounters,
    received: &mut ReceiveState,
    ready: &mut bool,
    port: &Port,
    waiting_bytes: Option<usize>,
    now: impl Fn() -> Instant,
) -> Result<bool, String> {
    // The capacity-one inbox may already contain authoritative progress when
    // either progress decision runs, including a media wake selected after the
    // top-of-turn check. Authenticate/process at most one item, then retain the
    // original ledger age and full waiting-plus-pending byte admission bounds.
    let handled = match incoming.try_recv() {
        Ok(frame) => {
            receive_control(
                Some(frame),
                receiver,
                fixture,
                commits,
                admission,
                ledger,
                sent,
                received,
                ready,
                port,
                waiting_bytes,
                &now,
            )?;
            true
        }
        Err(mpsc::error::TryRecvError::Empty) => false,
        Err(_) => return Err("probe control lane failed".into()),
    };
    // Receipts can become ready while select waits even without a control frame.
    // Reap before charging so each frame is in the ledger OR still pending, and
    // never both. Control reaps before validating a not-yet-known frontier.
    reap_commits(
        commits,
        admission,
        ledger,
        port,
        fixture::profile(fixture.profile_ms)?.samples(),
        sent,
        waiting_bytes,
    )?;
    if *ready {
        // None is the top-of-turn age check; Some is the taken waiting frame's
        // own byte count, with the current (not pre-handler) pending charge.
        let extra_bytes = waiting_bytes.map_or(0, |waiting| {
            waiting
                + commits
                    .iter()
                    .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
                    .sum::<usize>()
        });
        let checked_at = now();
        if let Err(failure) = ledger.check_observed(checked_at, extra_bytes) {
            let pending_bytes = commits
                .iter()
                .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
                .sum::<usize>();
            let waiting = waiting_bytes.or_else(|| {
                admission
                    .pending
                    .as_ref()
                    .map(|(waiting, _)| waiting.bytes())
            });
            let observation = progress_failure_observation(
                &failure,
                if waiting_bytes.is_some() {
                    ProgressCheckSite::MediaAdmission
                } else {
                    ProgressCheckSite::TopTurn
                },
                checked_at,
                None,
                waiting,
                (pending_bytes, commits.len()),
                sent,
                extra_bytes,
            );
            port.stats(|stats| stats.last_progress_failure = Some(observation));
            return Err(failure.reason.message().into());
        }
    }
    Ok(handled)
}

fn apply_codec_completion(
    completed: codec::Completion,
    flight: &mut Option<CodecFlight>,
    received: &mut ReceiveState,
    admission: &mut MediaAdmission,
    feedback: &mut packet::Feedback,
    commits: usize,
    now: Instant,
    profile: LiveProfile,
    port: &Port,
) -> Result<(), String> {
    let source = flight
        .as_mut()
        .expect("one codec flight")
        .source
        .take()
        .unwrap();
    flight.take();
    match (source, completed.output) {
        (CodecSource::Encode(source), codec::Output::Encoded { mut bytes, in_dtx }) => {
            port.stats(|stats| {
                observe_duration(&mut stats.encode_duration_bins, completed.duration);
                stats.encoded_packets += 1;
                stats.tiny_non_dtx_packets += u64::from(bytes.len() <= 2 && !in_dtx);
            });
            feedback.dtx = in_dtx;
            let mut waiting = WaitingPacket::new(
                std::mem::take(&mut *bytes),
                source.position,
                source.captured_at,
                source.pushed_at,
                completed.finished,
                profile,
            );
            waiting.capture_clock = source.clock;
            admission.stage_reserved(waiting, source.permit, commits, now, profile, port)
        }
        (CodecSource::Decode(work), codec::Output::Decoded(output)) => {
            // Publishing, not the earlier C completion, determines whether PCM
            // can still enter the unchanged source-frame presentation interval.
            received.apply_decode(work, now, completed.duration, output, port)
        }
        _ => Err("probe codec completion kind rejected".into()),
    }
}

fn dispatch_codec(
    now: Instant,
    profile: LiveProfile,
    received: &mut ReceiveState,
    capture: &mut CaptureFrame,
    admission: &MediaAdmission,
    feedback: &mut packet::Feedback,
    decoder_reset: &mut bool,
    codec: &mut codec::Owner,
    flight: &mut Option<CodecFlight>,
    outgoing: &mpsc::Sender<SendRequest>,
    commits: usize,
    port: &Arc<Port>,
) -> Result<(), String> {
    *decoder_reset |= received.maintain_playout(now, profile, feedback, port);
    // Holding the existing full source frame for decode does not grant it a
    // fresh age/deadline. Retire it even while the sole codec operation is busy.
    if capture.full(profile)
        && now
            >= capture.captured_at
                + Duration::from_millis(u64::from(profile.duration_ms()))
                + CAPTURE_MAX_AGE
    {
        capture.discard(0);
    }
    if flight.is_some() {
        return Ok(());
    }
    // A ready receive frame gets the freed codec slot now, not on the next
    // periodic tick or behind a newly assembled encode. Its checks/reserve and
    // once-only cursor retirement remain ReceiveState's existing decision.
    if let Some(mut work) = received.prepare_decode(now, profile, feedback, port) {
        let operation = codec::Operation::Decode {
            opus: work.opus.take(),
            reset: std::mem::take(decoder_reset),
        };
        *flight = Some(CodecFlight {
            source: Some(CodecSource::Decode(work)),
            port: port.clone(),
            samples: profile.samples(),
        });
        return codec.submit(operation);
    }
    if !capture.full(profile) || admission.pending.is_some() || commits >= 2 {
        return Ok(());
    }
    let permit = match outgoing.clone().try_reserve_owned() {
        Ok(permit) => permit,
        Err(mpsc::error::TrySendError::Full(_)) => {
            capture.discard(0);
            return Ok(());
        }
        Err(_) => {
            capture.discard(0);
            return Err("probe media admission owner closed".into());
        }
    };
    *flight = Some(CodecFlight {
        source: Some(CodecSource::Encode(EncodeSource {
            position: capture.position,
            captured_at: capture.captured_at,
            pushed_at: capture.pushed_at,
            clock: capture.clock,
            permit,
        })),
        port: port.clone(),
        samples: profile.samples(),
    });
    codec.submit(codec::Operation::Encode(Zeroizing::new(std::mem::take(
        &mut *capture.pcm,
    ))))
}

async fn endpoint(
    fixture: EndpointFixture,
    port: Arc<Port>,
    input: mpsc::Receiver<Captured>,
    media: super::lane::Lane,
    control: super::lane::Lane,
    stop: watch::Receiver<bool>,
) -> Result<(), String> {
    endpoint_owned(
        fixture,
        port,
        input,
        media,
        control,
        stop,
        #[cfg(test)]
        None,
    )
    .await
}

async fn endpoint_owned(
    fixture: EndpointFixture,
    port: Arc<Port>,
    mut input: mpsc::Receiver<Captured>,
    mut media: super::lane::Lane,
    mut control: super::lane::Lane,
    mut stop: watch::Receiver<bool>,
    #[cfg(test)] hold: Option<super::codec::Hold>,
) -> Result<(), String> {
    let profile = fixture::profile(fixture.profile_ms)?;
    let mut codec = codec::Owner::start(
        profile,
        #[cfg(test)]
        hold.clone(),
    )
    .await?;
    let mut flight: Option<CodecFlight> = None;
    let mut decoder_reset = false;
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
    let mut capture = CaptureFrame::new(profile.samples(), port.clone());
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
        last_control: None,
        packets: 0,
        octets: 0,
        timestamp: fixture.initial_timestamp,
    };
    let mut timer = tokio::time::interval(Duration::from_millis(10));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut report_due = Instant::now();
    let mut last_control_submission = Instant::now();

    let outcome = 'session: loop {
        if *stop.borrow() {
            break Ok(());
        }
        let now = Instant::now();
        if flight.is_some() {
            match codec.try_completed() {
                Ok(Some(completed)) => {
                    if let Err(error) = apply_codec_completion(
                        completed,
                        &mut flight,
                        &mut received,
                        &mut admission,
                        &mut feedback,
                        commits.len(),
                        Instant::now(),
                        profile,
                        &port,
                    ) {
                        break Err(error);
                    }
                }
                Ok(None) => {}
                Err(error) => break Err(error),
            }
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
        // Give the existing one-frame control turn its known-prefix opportunity
        // before reaping media receipts; it also retains the empty-inbox reap.
        match control_progress(
            &mut control.incoming,
            &mut receiver,
            &fixture,
            &mut commits,
            &mut admission,
            &mut ledger,
            &mut sent,
            &mut received,
            &mut ready,
            &port,
            None,
            Instant::now,
        ) {
            Ok(handled) => {
                #[cfg(test)]
                if handled {
                    if let Some(hold) = &hold {
                        hold.controlled.fetch_add(1, Ordering::Relaxed);
                    }
                }
                #[cfg(not(test))]
                let _ = handled;
            }
            Err(error) => break Err(error),
        }
        if ready {
            if now.saturating_duration_since(last_control_submission) >= Duration::from_millis(400)
            {
                break Err("control submission expired; retire generation".into());
            }
        }
        if flight.is_none() {
            #[cfg(test)]
            let before_drain = port.snapshot().received_rtp_packets;
            if let Err(error) = received.drain_queued(
                &mut media.incoming,
                &mut receiver,
                &fixture,
                &mut feedback,
                &port,
            ) {
                break Err(error);
            }
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold.admitted.fetch_add(
                    port.snapshot().received_rtp_packets - before_drain,
                    Ordering::Relaxed,
                );
            }
        }
        // Reconsider immediately after every completion/admission/capture wake.
        // Keep this outside select branches so all work shares one decision.
        if let Err(error) = dispatch_codec(
            Instant::now(),
            profile,
            &mut received,
            &mut capture,
            &admission,
            &mut feedback,
            &mut decoder_reset,
            &mut codec,
            &mut flight,
            &media.outgoing,
            commits.len(),
            &port,
        ) {
            break Err(error);
        }
        let media_wake = admission.wake_at(&mut budget, Instant::now(), profile);
        tokio::select! {
            _ = cancelled(&mut stop) => break Ok(()),
            result = codec.completed(), if flight.is_some() => {
                let completed = match result { Ok(completed) => completed, Err(error) => break Err(error) };
                if let Err(error) = apply_codec_completion(completed, &mut flight, &mut received, &mut admission,
                    &mut feedback, commits.len(), Instant::now(), profile, &port) { break Err(error); }
            }
            packet = control.incoming.recv() => {
                if let Err(error) = receive_control(packet, &mut receiver, &fixture, &mut commits,
                    &mut admission, &mut ledger, &mut sent, &mut received, &mut ready, &port, None, Instant::now) { break Err(error); }
                #[cfg(test)]
                if let Some(hold) = &hold { hold.controlled.fetch_add(1, Ordering::Relaxed); }
            }
            packet = media.incoming.recv() => {
                let frame = match packet { Some(Ok(frame)) => frame, _ => break Err("probe media lane failed".into()) };
                if let Err(error) = received.receive_lane(frame, &mut receiver, &fixture, &mut feedback, &port) { break Err(error); }
                #[cfg(test)]
                if let Some(hold) = &hold { hold.admitted.fetch_add(1, Ordering::Relaxed); }
            }
            _ = tokio::time::sleep_until(media_wake.unwrap_or(now).into()), if media_wake.is_some() => {
                let now = Instant::now();
                let Some((waiting, permit)) = admission.take_ready(&mut budget, now, profile, &port) else { continue; };
                let bytes = waiting.bytes();
                match control_progress(&mut control.incoming, &mut receiver, &fixture, &mut commits,
                    &mut admission, &mut ledger, &mut sent, &mut received, &mut ready, &port,
                    Some(bytes), Instant::now) {
                    Ok(handled) => {
                        #[cfg(test)]
                        if handled {
                            if let Some(hold) = &hold { hold.controlled.fetch_add(1, Ordering::Relaxed); }
                        }
                        #[cfg(not(test))]
                        let _ = handled;
                    }
                    Err(error) => {
                        port.stats(|stats| stats.dropped_capture += profile.samples() as u64);
                        break Err(error);
                    }
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
                commits.push_back(PendingCommit { index: source_index, timestamp, opus_bytes: waiting.opus.len(), submitted: waiting.encoded_at, source_ready: waiting.source_ready(profile), receipt });
                let phase = sender_phase.observe(waiting.position, waiting.pushed_at);
                port.stats(|stats| stats.max_sender_phase_advance_us = stats.max_sender_phase_advance_us.max(phase));
                source_index += 1;
            }
            // Collect into the original source frame even during decode, but
            // leave subsequent PCM in the ring while its source slot is owned.
            captured = input.recv(), if capture.can_ingest(profile, flight.as_ref(), &admission, &media.outgoing, commits.len()) => {
                let Some(captured) = captured else { break Err("probe capture owner closed".into()); };
                if let Some(source_at) = captured.source_time() {
                    local_clock.observe(captured.position, source_at, captured.capture_clock.map(|span| Rate { ticks: span.frames * 3, ns: span.elapsed_ns }));
                }
                capture.ingest(captured, ready, Instant::now(), profile);
            }
            scheduled = timer.tick() => {
                let now = Instant::now();
                port.stats(|stats| stats.max_playout_tick_lateness_ms = stats.max_playout_tick_lateness_ms.max(now.saturating_duration_since(scheduled.into_std()).as_millis() as u64));
                #[cfg(test)]
                let before_drain = port.snapshot().received_rtp_packets;
                if let Err(error) = received.drain_queued(&mut media.incoming, &mut receiver, &fixture, &mut feedback, &port) { break Err(error); }
                #[cfg(test)]
                if let Some(hold) = &hold { hold.admitted.fetch_add(port.snapshot().received_rtp_packets - before_drain, Ordering::Relaxed); }
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
            }
        }
    };
    drop(flight);
    admission.discard(profile, &port);
    port.stats(|stats| stats.remote_clock_valid = false);
    let joined = codec.close();
    media.joined_close().await;
    control.joined_close().await;
    outcome.and(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ControlTurn {
        fixture: EndpointFixture,
        peer: EndpointFixture,
        sender: dmsg_srtp_sys::Sender,
        receiver: dmsg_srtp_sys::Receiver,
        queued: mpsc::Sender<Result<InboundFrame, String>>,
        incoming: mpsc::Receiver<Result<InboundFrame, String>>,
        commits: VecDeque<PendingCommit>,
        admission: MediaAdmission,
        ledger: packet::Ledger,
        sent: SendCounters,
        received: ReceiveState,
        ready: bool,
        port: Port,
        origin: Instant,
    }

    impl ControlTurn {
        fn new() -> Self {
            let (fixture, peer, _, _) = fixture::pair(PublicConfig {
                relay_addr: "127.0.0.1:1".into(),
                domain: "control-turn.invalid".into(),
                profile_ms: 40,
                healthy_cycle_ms: 600,
                capacity_bps: 50_000,
                carriers: [None, None],
            })
            .unwrap();
            // The production Lane inbox type and capacity, not a ledger-only ACK.
            let (queued, incoming) = mpsc::channel(1);
            Self {
                sender: dmsg_srtp_sys::Sender::new(&peer.media_tx, peer.ssrc_tx).unwrap(),
                receiver: dmsg_srtp_sys::Receiver::new(&fixture.media_rx, fixture.ssrc_rx).unwrap(),
                fixture,
                peer,
                queued,
                incoming,
                commits: VecDeque::new(),
                admission: MediaAdmission::default(),
                ledger: packet::Ledger::new(100, 40, 600),
                sent: SendCounters {
                    timestamp: 0,
                    packets: 0,
                    octets: 0,
                    since_report: false,
                    last_control: None,
                },
                received: test_received(),
                ready: true,
                port: test_port(),
                origin: Instant::now(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.origin + Duration::from_millis(ms)
        }

        fn commit(&mut self, index: u64, ms: u64) {
            let (notify, receipt) = oneshot::channel();
            notify
                .send(Commit::Committed {
                    framed_bytes: 144,
                    at: self.at(ms),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            self.commits.push_back(PendingCommit {
                index,
                timestamp: index as u32 * 1920,
                opus_bytes: 100,
                submitted: self.at(ms),
                source_ready: self.at(ms),
                receipt,
            });
            self.reap();
        }

        fn reap(&mut self) {
            reap_commits(
                &mut self.commits,
                &mut self.admission,
                &mut self.ledger,
                &self.port,
                640,
                &mut self.sent,
                None,
            )
            .unwrap();
        }

        fn compound(&self, terminal: Option<u64>) -> Vec<u8> {
            packet::compound(packet::Report {
                sender_ssrc: self.peer.ssrc_tx,
                peer_ssrc: self.peer.ssrc_rx,
                cname: &self.peer.cname,
                sender: None,
                feedback: packet::Feedback {
                    terminal,
                    ..packet::Feedback::default()
                },
            })
        }

        fn queue_cipher(&mut self, cipher: Vec<u8>, ms: u64) {
            self.queued
                .try_send(Ok(InboundFrame {
                    frame: (OP_RTCP, cipher),
                    body_complete: self.at(ms),
                    noise_done: self.at(ms),
                }))
                .unwrap_or_else(|_| panic!("control inbox full"));
            assert_eq!(self.queued.capacity(), 0);
        }

        fn queue_ack(&mut self, terminal: Option<u64>, ms: u64) {
            let compound = self.compound(terminal);
            let cipher = self.sender.protect_rtcp(&compound).unwrap();
            self.queue_cipher(cipher, ms);
        }

        fn decide(&mut self, ms: u64, waiting_bytes: Option<usize>) -> Result<bool, String> {
            // Both production paths use the bounded helper before media reaping.
            let now = self.at(ms);
            control_progress(
                &mut self.incoming,
                &mut self.receiver,
                &self.fixture,
                &mut self.commits,
                &mut self.admission,
                &mut self.ledger,
                &mut self.sent,
                &mut self.received,
                &mut self.ready,
                &self.port,
                waiting_bytes,
                || now,
            )
        }
    }

    #[test]
    fn media_wake_prefix_ack_does_not_double_charge_newly_ready_receipt() {
        let mut turn = ControlTurn::new();
        let profile = LiveProfile::Ms40;
        for index in 7..=25 {
            turn.commit(index, if index == 7 { 0 } else { 700 });
        }
        assert_eq!(turn.ledger.bytes(), 2736);
        assert_eq!(turn.decide(719, None), Ok(false));

        let (outgoing, _requests) = mpsc::channel(1);
        let waiting = WaitingPacket::new(
            vec![0; 100],
            0,
            turn.at(679),
            turn.at(719),
            turn.at(719),
            profile,
        );
        turn.admission
            .stage(waiting, &outgoing, 0, turn.at(719), profile, &turn.port)
            .unwrap();
        let (notify, receipt) = oneshot::channel();
        turn.commits.push_back(PendingCommit {
            index: 26,
            timestamp: 26 * 1920,
            opus_bytes: 100,
            submitted: turn.at(719),
            source_ready: turn.at(719),
            receipt,
        });
        // Receipt and protected prefix ACK become ready after the empty top turn.
        notify
            .send(Commit::Committed {
                framed_bytes: 144,
                at: turn.at(720),
            })
            .unwrap_or_else(|_| panic!("receipt closed"));
        turn.queue_ack(Some(7), 719);
        let mut budget = packet::Budget::new(50_000, 144, turn.at(719));
        let (waiting, _permit) = turn
            .admission
            .take_ready(&mut budget, turn.at(721), profile, &turn.port)
            .unwrap();
        let pending_bytes: usize = turn
            .commits
            .iter()
            .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
            .sum();
        assert_eq!(waiting.bytes(), 144);
        assert_eq!(pending_bytes, 144);
        let decision = turn.decide(721, Some(waiting.bytes()));
        if decision.is_err() {
            // The media-wake caller accounts its taken unsent frame exactly once.
            turn.port
                .stats(|stats| stats.dropped_capture += profile.samples() as u64);
        }
        assert!(turn.commits.is_empty());
        assert!(turn.incoming.is_empty());
        assert_eq!(turn.sent.packets, 20);
        assert_eq!(turn.ledger.bytes(), 2736);
        assert_eq!(turn.port.snapshot().terminal_feedback, 1);
        assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 721);
        assert_eq!(turn.port.snapshot().rx_bytes, 132);
        assert_eq!(turn.ledger.bytes() + waiting.bytes(), 2880);
        assert_eq!(
            decision,
            Ok(true),
            "fresh ledger plus waiting fits 2880; stale pending charges 3024"
        );
        assert_eq!(turn.port.snapshot().dropped_capture, 0);
        assert!(turn.port.snapshot().last_progress_failure.is_none());
    }

    #[test]
    fn media_wake_reaps_one_receipt_but_still_charges_unresolved_pending_bytes() {
        for one_byte_over in [false, true] {
            let mut turn = ControlTurn::new();
            let profile = LiveProfile::Ms40;
            turn.commit(7, 0);
            for index in 8..=24 {
                turn.commit(index, 700);
            }
            turn.ledger
                .commit(25, 99 + usize::from(one_byte_over), turn.at(700))
                .unwrap();
            assert_eq!(turn.decide(719, None), Ok(false));
            let (notify, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 26,
                timestamp: 26 * 1920,
                opus_bytes: 1,
                submitted: turn.at(719),
                source_ready: turn.at(719),
                receipt,
            });
            notify
                .send(Commit::Committed {
                    framed_bytes: 45,
                    at: turn.at(720),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            let (_pending, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 27,
                timestamp: 27 * 1920,
                opus_bytes: 100,
                submitted: turn.at(719),
                source_ready: turn.at(719),
                receipt,
            });
            turn.queue_ack(Some(7), 719);

            let (outgoing, _requests) = mpsc::channel(1);
            let waiting = WaitingPacket::new(
                vec![0; 100],
                0,
                turn.at(679),
                turn.at(719),
                turn.at(719),
                profile,
            );
            turn.admission
                .stage(waiting, &outgoing, 0, turn.at(719), profile, &turn.port)
                .unwrap();
            let mut budget = packet::Budget::new(50_000, 144, turn.at(719));
            let (waiting, _permit) = turn
                .admission
                .take_ready(&mut budget, turn.at(721), profile, &turn.port)
                .unwrap();
            let decision = turn.decide(721, Some(waiting.bytes()));
            if decision.is_err() {
                turn.port
                    .stats(|stats| stats.dropped_capture += profile.samples() as u64);
            }
            assert_eq!(
                decision,
                if one_byte_over {
                    Err("remote media window exhausted; retire generation".into())
                } else {
                    Ok(true)
                }
            );
            assert_eq!(turn.commits.len(), 1);
            assert_eq!(turn.commits.front().unwrap().index, 27);
            let pending_bytes: usize = turn
                .commits
                .iter()
                .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
                .sum();
            assert_eq!(pending_bytes, 144);
            assert_eq!(turn.ledger.bytes(), 2592 + usize::from(one_byte_over));
            assert_eq!(
                turn.ledger.bytes() + pending_bytes + waiting.bytes(),
                2880 + usize::from(one_byte_over)
            );
            // The one-byte excess would pass if the still-pending frame vanished
            // from the charge when its preceding receipt was reaped.
            assert!(turn.ledger.check(turn.at(721), waiting.bytes()).is_ok());
            assert_eq!(turn.sent.packets, 19);
            assert_eq!(turn.port.snapshot().terminal_feedback, 1);
            assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 721);
            assert_eq!(
                turn.port.snapshot().dropped_capture,
                u64::from(one_byte_over) * profile.samples() as u64
            );
            assert!(turn.incoming.is_empty());
        }
    }

    #[test]
    fn media_wake_without_control_reaps_receipts_without_releasing_credit_or_age() {
        for waiting_bytes in [None, Some(144)] {
            let mut turn = ControlTurn::new();
            for index in 7..=24 {
                turn.commit(index, 700);
            }
            assert_eq!(turn.decide(719, None), Ok(false));
            let (notify, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 25,
                timestamp: 25 * 1920,
                opus_bytes: 100,
                submitted: turn.at(719),
                source_ready: turn.at(719),
                receipt,
            });
            notify
                .send(Commit::Committed {
                    framed_bytes: 144,
                    at: turn.at(720),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            assert_eq!(turn.decide(721, waiting_bytes), Ok(false));
            assert!(turn.commits.is_empty());
            assert_eq!(turn.sent.packets, 19);
            assert_eq!(turn.ledger.bytes(), 2736);
            assert_eq!(turn.port.snapshot().terminal_feedback, 0);
            assert_eq!(turn.port.snapshot().rx_bytes, 0);
            assert!(turn.ledger.check(turn.at(721), 144).is_ok());
            assert_eq!(
                turn.ledger.check(turn.at(721), 145),
                Err("remote media window exhausted; retire generation".into())
            );
        }

        let mut turn = ControlTurn::new();
        let (notify, receipt) = oneshot::channel();
        turn.commits.push_back(PendingCommit {
            index: 7,
            timestamp: 7 * 1920,
            opus_bytes: 100,
            submitted: turn.at(0),
            source_ready: turn.at(0),
            receipt,
        });
        notify
            .send(Commit::Committed {
                framed_bytes: 144,
                at: turn.at(0),
            })
            .unwrap_or_else(|_| panic!("receipt closed"));
        // An old receipt cannot hide as a pending byte charge until the next
        // top turn: the unchanged 720ms age limit applies at this media wake.
        assert_eq!(
            turn.decide(721, Some(144)),
            Err("remote media progress expired; retire generation".into())
        );
        assert!(turn.commits.is_empty());
        assert_eq!(turn.ledger.bytes(), 144);
        assert_eq!(turn.sent.packets, 1);
        assert_eq!(turn.port.snapshot().terminal_feedback, 0);
    }

    #[test]
    fn queued_terminal_ack_precedes_expired_ledger_decision() {
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        assert_eq!(turn.ledger.bytes(), 144);
        assert_eq!(turn.sent.packets, 1);
        assert!(turn.ledger.check(turn.at(720), 0).is_ok());
        turn.queue_ack(Some(7), 719);
        // Explicit std::Instant decision times: no sleeps or scheduler timing.
        assert_eq!(turn.decide(721, None), Ok(true));
        assert_eq!(turn.ledger.bytes(), 0);
        assert_eq!(turn.port.snapshot().terminal_feedback, 1);
        assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 721);
    }

    #[test]
    fn absent_terminal_ack_keeps_original_ledger_expiry() {
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        assert_eq!(turn.decide(720, None), Ok(false));
        assert_eq!(
            turn.decide(721, None),
            Err("remote media progress expired; retire generation".into())
        );
        assert_eq!(turn.ledger.bytes(), 144);
        assert_eq!(turn.port.snapshot().terminal_feedback, 0);
    }

    #[test]
    fn known_terminal_prefix_precedes_later_receipts_at_each_control_turn() {
        for actor_ms in [721, 999] {
            // Selected control, media wake, and the actual top-turn ordering.
            for route in [1, 2, 0] {
                let mut turn = ControlTurn::new();
                turn.commit(7, 0);
                turn.queue_ack(Some(6), 199);
                assert_eq!(turn.decide(200, None), Ok(true));
                let (outgoing, _requests) = mpsc::channel(1);
                let waiting = WaitingPacket::new(
                    vec![0; 30],
                    0,
                    turn.at(680),
                    turn.at(700),
                    turn.at(700),
                    LiveProfile::Ms40,
                );
                let deadline = waiting.deadline;
                turn.admission
                    .stage(
                        waiting,
                        &outgoing,
                        0,
                        turn.at(700),
                        LiveProfile::Ms40,
                        &turn.port,
                    )
                    .unwrap();
                let (notify, receipt) = oneshot::channel();
                notify
                    .send(Commit::Committed {
                        framed_bytes: 144,
                        at: turn.at(721),
                    })
                    .unwrap_or_else(|_| panic!("receipt closed"));
                turn.commits.push_back(PendingCommit {
                    index: 8,
                    timestamp: 8 * 1920,
                    opus_bytes: 100,
                    submitted: turn.at(700),
                    source_ready: turn.at(700),
                    receipt,
                });
                let (_unresolved, receipt) = oneshot::channel();
                turn.commits.push_back(PendingCommit {
                    index: 9,
                    timestamp: 9 * 1920,
                    opus_bytes: 40,
                    submitted: turn.at(710),
                    source_ready: turn.at(710),
                    receipt,
                });
                let mut budget = packet::Budget::new(50_000, 144, turn.at(721));
                let taken = if route == 2 {
                    Some(
                        turn.admission
                            .take_ready(&mut budget, turn.at(721), LiveProfile::Ms40, &turn.port)
                            .unwrap(),
                    )
                } else {
                    None
                };
                turn.queue_ack(Some(7), 719);
                let at = turn.at(actor_ms);
                let result = match route {
                    0 => turn.decide(actor_ms, None).map(|handled| {
                        assert!(handled);
                    }),
                    1 => {
                        let frame = turn.incoming.try_recv().unwrap();
                        receive_control(
                            Some(frame),
                            &mut turn.receiver,
                            &turn.fixture,
                            &mut turn.commits,
                            &mut turn.admission,
                            &mut turn.ledger,
                            &mut turn.sent,
                            &mut turn.received,
                            &mut turn.ready,
                            &turn.port,
                            None,
                            || at,
                        )
                    }
                    _ => turn
                        .decide(actor_ms, Some(taken.as_ref().unwrap().0.bytes()))
                        .map(|handled| {
                            assert!(handled);
                        }),
                };
                assert_eq!(result, Ok(()), "known ACK7 must cover old entry before receipt8; route={route}, actor={actor_ms}");
                assert_eq!(turn.ledger.bytes(), 144);
                assert!(turn.ledger.check(turn.at(1441), 0).is_ok());
                let view = turn
                    .ledger
                    .check_observed(turn.at(1442), 0)
                    .unwrap_err()
                    .observation;
                assert_eq!(view.oldest_index, Some(8));
                assert_eq!(view.oldest_age, Some(Duration::from_millis(721)));
                assert_eq!(view.highest, Some(8));
                assert_eq!(view.terminal, Some(7));
                assert_eq!(turn.commits.len(), 1);
                assert_eq!(turn.commits.front().unwrap().index, 9);
                assert_eq!(turn.sent.packets, 2);
                assert_eq!(turn.port.snapshot().terminal_feedback, 2);
                assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, actor_ms);
                assert_eq!(turn.port.snapshot().rx_bytes, 264);
                assert!(turn.port.snapshot().last_progress_failure.is_none());
                assert_eq!(turn.sent.last_control.unwrap().terminal, Some(7));
                assert_eq!(deadline, turn.at(740));
                if let Some((waiting, _permit)) = taken {
                    assert_eq!(waiting.deadline, deadline);
                    assert_eq!(waiting.encoded_at, turn.at(700));
                } else {
                    assert_eq!(
                        turn.admission.pending.as_ref().unwrap().0.deadline,
                        deadline
                    );
                    let ready =
                        turn.admission
                            .take_ready(&mut budget, at, LiveProfile::Ms40, &turn.port);
                    if actor_ms == 999 {
                        assert!(ready.is_none(), "terminal credit cannot rescue expired PCM");
                        assert_eq!(turn.port.snapshot().dropped_capture, 640);
                    } else {
                        assert_eq!(ready.unwrap().0.deadline, deadline);
                        assert_eq!(turn.port.snapshot().dropped_capture, 0);
                    }
                }
            }
        }
    }

    #[test]
    fn known_prefix_does_not_hide_uncovered_or_invalid_receipt_failures() {
        for failure_kind in 0..5 {
            let mut turn = ControlTurn::new();
            if failure_kind == 4 {
                turn.ledger.commit(7, 143, turn.at(0)).unwrap();
            } else {
                turn.commit(7, 0);
            }
            if failure_kind == 0 {
                turn.commit(8, 0);
            }
            turn.queue_ack(Some(6), 199);
            assert_eq!(turn.decide(200, None), Ok(true));
            let prior_control = turn.sent.last_control;
            if failure_kind == 4 {
                for index in 8..=25 {
                    turn.commit(index, 700);
                }
                turn.ledger.commit(26, 99, turn.at(700)).unwrap();
                turn.ledger.commit(27, 46, turn.at(700)).unwrap();
                assert_eq!(turn.ledger.bytes(), 2880);
            }
            let index = match failure_kind {
                0 => 9,
                1 => 7,
                2 => 1 << 48,
                3 => 8,
                _ => 28,
            };
            let (notify, receipt) = oneshot::channel();
            if failure_kind == 3 {
                drop(notify);
            } else {
                notify
                    .send(Commit::Committed {
                        framed_bytes: 144,
                        at: turn.at(721),
                    })
                    .unwrap_or_else(|_| panic!("receipt closed"));
            }
            turn.commits.push_back(PendingCommit {
                index,
                timestamp: 0,
                opus_bytes: 100,
                submitted: turn.at(700),
                source_ready: turn.at(700),
                receipt,
            });
            turn.queue_ack(Some(7), 719);
            assert_eq!(
                turn.decide(999, None),
                Err(match failure_kind {
                    0 => "remote media progress expired; retire generation",
                    1 | 2 => "invalid committed media index",
                    3 => "probe commitment owner closed",
                    _ => "remote media window exhausted; retire generation",
                }
                .into())
            );
            assert_eq!(turn.port.snapshot().terminal_feedback, 2);
            assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 999);
            assert_eq!(
                turn.sent.last_control, prior_control,
                "ACK applied, but the entire control handler did not complete"
            );
            assert_eq!(turn.port.snapshot().rx_bytes, 132);
            assert_eq!(turn.commits.len(), 1);
            if failure_kind == 0 || failure_kind == 4 {
                let failure = turn.port.snapshot().last_progress_failure.unwrap();
                assert_eq!(failure.check_site, ProgressCheckSite::ReceiptCommit);
                assert_eq!(failure.attempted_index, Some(index));
                assert_eq!(failure.frame_bytes, Some(144));
                assert_eq!(failure.oldest_index, Some(8));
                assert_eq!(
                    failure.oldest_age_us,
                    Some(if failure_kind == 0 { 721000 } else { 21000 })
                );
                assert_eq!(failure.terminal_frontier, Some(7));
                assert_eq!(
                    failure.reason,
                    if failure_kind == 0 {
                        packet::ProgressFailureReason::Expired
                    } else {
                        packet::ProgressFailureReason::WindowExhausted
                    }
                );
                assert_eq!(
                    failure.ledger_bytes,
                    if failure_kind == 0 { 144 } else { 2737 }
                );
                assert_eq!(
                    failure.total_check_bytes,
                    if failure_kind == 0 { 288 } else { 2881 }
                );
                assert_eq!(
                    failure.last_authenticated_control.unwrap().terminal,
                    Some(6)
                );
            } else {
                assert!(turn.port.snapshot().last_progress_failure.is_none());
                assert_eq!(turn.ledger.bytes(), 0);
                assert_eq!(turn.ledger.highest_committed(), Some(7));
            }
        }
    }

    #[test]
    fn future_terminal_frontier_requires_actual_receipts_before_credit() {
        for already_known in [false, true] {
            for kind in 0..5 {
                if kind == 4 && !already_known {
                    continue;
                }
                let mut turn = ControlTurn::new();
                if already_known {
                    turn.commit(7, 0);
                }
                let index = if already_known { 8 } else { 7 };
                let at_ms = if kind == 4 { 721 } else { 718 };
                let (notify, receipt) = oneshot::channel();
                let unresolved = if kind == 2 {
                    Some(notify)
                } else {
                    notify
                        .send(if kind == 3 {
                            Commit::Dropped
                        } else {
                            Commit::Committed {
                                framed_bytes: 144,
                                at: turn.at(at_ms),
                            }
                        })
                        .unwrap_or_else(|_| panic!("receipt closed"));
                    None
                };
                turn.commits.push_back(PendingCommit {
                    index,
                    timestamp: 0,
                    opus_bytes: 100,
                    submitted: turn.at(700),
                    source_ready: turn.at(700),
                    receipt,
                });
                let terminal = if kind == 1 { index + 1 } else { index };
                // A valid future ACK cannot predate its own Noise commitment.
                let queued_ms = if kind == 4 { 722 } else { 719 };
                let actor_ms = if kind == 4 { 723 } else { 721 };
                turn.queue_ack(Some(terminal), queued_ms);
                let result = turn.decide(actor_ms, None);
                if kind == 0 {
                    assert_eq!(result, Ok(true));
                    assert!(turn.commits.is_empty());
                    assert_eq!(turn.ledger.highest_committed(), Some(index));
                    assert_eq!(turn.ledger.bytes(), 0);
                    assert_eq!(turn.sent.packets, 1 + u32::from(already_known));
                    assert_eq!(turn.port.snapshot().terminal_feedback, 1);
                    assert_eq!(
                        turn.port.snapshot().max_feedback_cycle_ms,
                        if already_known { 721 } else { 3 }
                    );
                    assert_eq!(turn.sent.last_control.unwrap().terminal, Some(index));
                    assert!(turn.port.snapshot().last_progress_failure.is_none());
                } else {
                    assert_eq!(
                        result,
                        Err(if kind == 4 {
                            "remote media progress expired; retire generation"
                        } else {
                            "invalid remote terminal frontier"
                        }
                        .into())
                    );
                    assert_eq!(turn.port.snapshot().terminal_feedback, 0);
                    assert!(turn.sent.last_control.is_none());
                    if kind == 4 {
                        let failure = turn.port.snapshot().last_progress_failure.unwrap();
                        assert_eq!(failure.check_site, ProgressCheckSite::ReceiptCommit);
                        assert_eq!(failure.terminal_frontier, None);
                        assert_eq!(failure.oldest_index, Some(7));
                        assert_eq!(failure.oldest_age_us, Some(721000));
                    } else {
                        assert!(turn.port.snapshot().last_progress_failure.is_none());
                    }
                    if kind == 2 {
                        assert_eq!(turn.commits.len(), 1);
                        assert_eq!(turn.ledger.highest_committed(), already_known.then_some(7));
                    }
                    if kind == 3 {
                        assert_eq!(turn.port.snapshot().dropped_capture, 640);
                    }
                }
                drop(unresolved);
            }
        }
    }

    #[test]
    fn repeated_or_stale_prefix_never_rescues_uncovered_receipt_age() {
        for terminal in [Some(7), Some(6), None] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            turn.commit(8, 0);
            turn.queue_ack(Some(7), 199);
            assert_eq!(turn.decide(200, None), Ok(true));
            let prior_control = turn.sent.last_control;
            let (notify, receipt) = oneshot::channel();
            notify
                .send(Commit::Committed {
                    framed_bytes: 144,
                    at: turn.at(721),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            turn.commits.push_back(PendingCommit {
                index: 9,
                timestamp: 9 * 1920,
                opus_bytes: 100,
                submitted: turn.at(700),
                source_ready: turn.at(700),
                receipt,
            });
            turn.queue_ack(terminal, 719);
            assert_eq!(
                turn.decide(721, None),
                Err(if terminal == Some(6) {
                    "invalid remote terminal frontier"
                } else {
                    "remote media progress expired; retire generation"
                }
                .into())
            );
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(turn.ledger.highest_committed(), Some(8));
            assert_eq!(turn.sent.last_control, prior_control);
            assert_eq!(
                turn.port.snapshot().max_feedback_cycle_ms,
                200,
                "equal frontier adds no fresh cycle or credit"
            );
            assert_eq!(
                turn.port.snapshot().terminal_feedback,
                if terminal == Some(7) { 2 } else { 1 }
            );
            if terminal == Some(6) {
                assert!(turn.port.snapshot().last_progress_failure.is_none());
                assert!(
                    matches!(
                        turn.commits.front_mut().unwrap().receipt.try_recv(),
                        Ok(Commit::Committed { .. })
                    ),
                    "stale validation precedes unrelated receipts"
                );
            } else {
                let failure = turn.port.snapshot().last_progress_failure.unwrap();
                assert_eq!(failure.check_site, ProgressCheckSite::ReceiptCommit);
                assert_eq!(failure.oldest_index, Some(8));
                assert_eq!(failure.oldest_age_us, Some(721000));
                assert_eq!(failure.terminal_frontier, Some(7));
                assert_eq!(
                    failure.last_authenticated_control.unwrap().terminal,
                    Some(7)
                );
            }
        }
    }

    #[test]
    fn receipt_commit_failure_records_original_instant_before_queued_ack() {
        for actor_ms in [721, 999] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            let cipher = turn.sender.protect_rtcp(&turn.compound(Some(6))).unwrap();
            turn.queued
                .try_send(Ok(InboundFrame {
                    frame: (OP_RTCP, cipher),
                    body_complete: turn.at(100),
                    noise_done: turn.at(101),
                }))
                .unwrap_or_else(|_| panic!("control inbox full"));
            assert_eq!(turn.decide(200, None), Ok(true));
            let prior_control = turn.sent.last_control;
            let (outgoing, _requests) = mpsc::channel(1);
            let waiting = WaitingPacket::new(
                vec![0; 30],
                0,
                turn.at(680),
                turn.at(700),
                turn.at(700),
                LiveProfile::Ms40,
            );
            turn.admission
                .stage(
                    waiting,
                    &outgoing,
                    0,
                    turn.at(700),
                    LiveProfile::Ms40,
                    &turn.port,
                )
                .unwrap();
            let (notify, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 8,
                timestamp: 8 * 1920,
                opus_bytes: 100,
                submitted: turn.at(700),
                source_ready: turn.at(700),
                receipt,
            });
            notify
                .send(Commit::Committed {
                    framed_bytes: 144,
                    at: turn.at(721),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            let (_unresolved, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 9,
                timestamp: 9 * 1920,
                opus_bytes: 40,
                submitted: turn.at(710),
                source_ready: turn.at(710),
                receipt,
            });
            // A new protected equal prefix covers none of the old entry 7.
            // Receipt expiry must remain at its own instant, not actor time.
            turn.queue_ack(Some(6), 719);
            let reads = std::cell::Cell::new(0);
            let actor_at = turn.at(actor_ms);
            assert_eq!(
                control_progress(
                    &mut turn.incoming,
                    &mut turn.receiver,
                    &turn.fixture,
                    &mut turn.commits,
                    &mut turn.admission,
                    &mut turn.ledger,
                    &mut turn.sent,
                    &mut turn.received,
                    &mut turn.ready,
                    &turn.port,
                    None,
                    || {
                        reads.set(reads.get() + 1);
                        actor_at
                    },
                ),
                Err("remote media progress expired; retire generation".into())
            );
            assert_eq!(
                reads.get(),
                1,
                "only the valid equal prefix reads actor time"
            );
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(turn.commits.len(), 2);
            assert_eq!(turn.sent.packets, 1);
            assert_eq!(turn.port.snapshot().terminal_feedback, 2);
            assert_eq!(turn.port.snapshot().rx_bytes, 132);
            assert_eq!(turn.sent.last_control, prior_control);
            let failure = turn
                .port
                .snapshot()
                .last_progress_failure
                .expect("original receipt-commit failure must have an observation");
            assert_eq!(failure.oldest_index, Some(7));
            assert_eq!(failure.oldest_age_us, Some(721000));
            assert_eq!(
                serde_json::to_value(&failure).unwrap(),
                serde_json::json!({
                    "reason": "expired", "check_site": "receipt_commit",
                    "attempted_index": 8, "frame_bytes": 144,
                    "ledger_bytes": 144, "window_bytes": 2880, "entry_count": 1,
                    "oldest_index": 7, "oldest_age_us": 721000,
                    "highest_committed_index": 7, "terminal_frontier": 6, "max_age_us": 720000,
                    "waiting_bytes": 74, "pending_bytes": 84, "pending_receipt_count": 1,
                    "committed_receipt_count": 1, "check_extra_bytes": 144, "total_check_bytes": 288,
                    "last_authenticated_control": {
                        "terminal": 6, "sender_clock_present": false,
                        "body_to_noise_us": 1000, "noise_to_processed_us": 99000,
                        "since_processed_us": 521000, "since_noise_us": 620000, "since_body_us": 621000
                    }
                })
            );
            turn.port.closed.store(true, Ordering::Relaxed);
            turn.port.stats(|stats| stats.stopped = true);
            assert_eq!(turn.port.snapshot().last_progress_failure, Some(failure));
        }
    }

    #[test]
    fn receipt_commit_failure_observes_every_original_reaping_route_and_time() {
        for route in 0..5 {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            let (outgoing, _requests) = mpsc::channel(1);
            let waiting = WaitingPacket::new(
                vec![0; 30],
                0,
                turn.at(680),
                turn.at(700),
                turn.at(700),
                LiveProfile::Ms40,
            );
            turn.admission
                .stage(
                    waiting,
                    &outgoing,
                    0,
                    turn.at(700),
                    LiveProfile::Ms40,
                    &turn.port,
                )
                .unwrap();
            let taken = if route >= 3 {
                let mut budget = packet::Budget::new(50_000, 144, turn.at(719));
                Some(
                    turn.admission
                        .take_ready(&mut budget, turn.at(719), LiveProfile::Ms40, &turn.port)
                        .unwrap(),
                )
            } else {
                None
            };
            let waiting_bytes = taken.as_ref().map(|(waiting, _)| waiting.bytes());
            let (notify, receipt) = oneshot::channel();
            notify
                .send(Commit::Committed {
                    framed_bytes: 144,
                    at: turn.at(721),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            turn.commits.push_back(PendingCommit {
                index: 8,
                timestamp: 8 * 1920,
                opus_bytes: 100,
                submitted: turn.at(700),
                source_ready: turn.at(700),
                receipt,
            });
            if route == 1 || route == 4 {
                // Valid but non-covering prefix: do not forgive entry 7.
                turn.queue_ack(Some(6), 719);
            }
            let actor_at = turn.at(999);
            let result = if route == 0 {
                // The endpoint's top turn now routes through the bounded helper.
                turn.decide(999, None).map(|_| ())
            } else if route == 1 {
                // The select branch's control handler, without the top turn.
                let frame = turn.incoming.try_recv().unwrap();
                receive_control(
                    Some(frame),
                    &mut turn.receiver,
                    &turn.fixture,
                    &mut turn.commits,
                    &mut turn.admission,
                    &mut turn.ledger,
                    &mut turn.sent,
                    &mut turn.received,
                    &mut turn.ready,
                    &turn.port,
                    None,
                    || actor_at,
                )
            } else {
                // Empty-inbox top/media helper, or queued-control media helper.
                control_progress(
                    &mut turn.incoming,
                    &mut turn.receiver,
                    &turn.fixture,
                    &mut turn.commits,
                    &mut turn.admission,
                    &mut turn.ledger,
                    &mut turn.sent,
                    &mut turn.received,
                    &mut turn.ready,
                    &turn.port,
                    waiting_bytes,
                    || actor_at,
                )
                .map(|_| ())
            };
            assert_eq!(
                result,
                Err("remote media progress expired; retire generation".into())
            );
            let failure = turn.port.snapshot().last_progress_failure.unwrap();
            assert_eq!(failure.check_site, ProgressCheckSite::ReceiptCommit);
            assert_eq!(
                (failure.attempted_index, failure.frame_bytes),
                (Some(8), Some(144))
            );
            assert_eq!(failure.oldest_age_us, Some(721000));
            assert_eq!(failure.waiting_bytes, Some(74));
            assert_eq!(
                (failure.pending_bytes, failure.pending_receipt_count),
                (0, 0)
            );
            assert_eq!(
                (failure.check_extra_bytes, failure.total_check_bytes),
                (144, 288)
            );
            assert!(failure.last_authenticated_control.is_none());
            assert_eq!(
                turn.port.snapshot().terminal_feedback,
                u64::from(route == 1 || route == 4)
            );
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(turn.sent.packets, 1);
        }

        for queued_ack in [false, true] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            let (notify, receipt) = oneshot::channel();
            notify
                .send(Commit::Committed {
                    framed_bytes: 144,
                    at: turn.at(720),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            turn.commits.push_back(PendingCommit {
                index: 8,
                timestamp: 8 * 1920,
                opus_bytes: 100,
                submitted: turn.at(700),
                source_ready: turn.at(700),
                receipt,
            });
            if queued_ack {
                turn.queue_ack(Some(7), 719);
            }
            let actor_at = turn.at(999);
            let result = control_progress(
                &mut turn.incoming,
                &mut turn.receiver,
                &turn.fixture,
                &mut turn.commits,
                &mut turn.admission,
                &mut turn.ledger,
                &mut turn.sent,
                &mut turn.received,
                &mut turn.ready,
                &turn.port,
                None,
                || actor_at,
            );
            assert!(turn.commits.is_empty());
            assert_eq!(turn.sent.packets, 2);
            if queued_ack {
                assert_eq!(result, Ok(true));
                assert!(turn.port.snapshot().last_progress_failure.is_none());
                assert_eq!(turn.ledger.bytes(), 144);
            } else {
                assert_eq!(
                    result,
                    Err("remote media progress expired; retire generation".into())
                );
                let failure = turn.port.snapshot().last_progress_failure.unwrap();
                assert_eq!(failure.check_site, ProgressCheckSite::TopTurn);
                assert_eq!((failure.attempted_index, failure.frame_bytes), (None, None));
                assert_eq!(failure.oldest_age_us, Some(999000));
                assert_eq!(failure.highest_committed_index, Some(8));
                assert_eq!(failure.ledger_bytes, 288);
            }
        }
    }

    #[test]
    fn receipt_commit_failure_keeps_window_precedence_and_exact_boundary() {
        for commit_ms in [720, 721] {
            for one_byte_over in [false, true] {
                let mut turn = ControlTurn::new();
                for index in 7..=24 {
                    turn.commit(index, 0);
                }
                turn.ledger
                    .commit(25, 99 + usize::from(one_byte_over), turn.at(0))
                    .unwrap();
                turn.ledger.commit(26, 45, turn.at(0)).unwrap();
                let (notify, receipt) = oneshot::channel();
                notify
                    .send(Commit::Committed {
                        framed_bytes: 144,
                        at: turn.at(commit_ms),
                    })
                    .unwrap_or_else(|_| panic!("receipt closed"));
                turn.commits.push_back(PendingCommit {
                    index: 27,
                    timestamp: 27 * 1920,
                    opus_bytes: 100,
                    submitted: turn.at(700),
                    source_ready: turn.at(700),
                    receipt,
                });
                let result = reap_commits(
                    &mut turn.commits,
                    &mut turn.admission,
                    &mut turn.ledger,
                    &turn.port,
                    640,
                    &mut turn.sent,
                    None,
                );
                if commit_ms == 720 && !one_byte_over {
                    assert_eq!(result, Ok(()));
                    assert_eq!(turn.ledger.bytes(), 2880);
                    assert!(turn.commits.is_empty());
                    assert_eq!(turn.sent.packets, 19);
                    assert!(turn.port.snapshot().last_progress_failure.is_none());
                } else {
                    let reason = if one_byte_over {
                        packet::ProgressFailureReason::WindowExhausted
                    } else {
                        packet::ProgressFailureReason::Expired
                    };
                    assert_eq!(result, Err(reason.message().into()));
                    let failure = turn.port.snapshot().last_progress_failure.unwrap();
                    assert_eq!(failure.reason, reason);
                    assert_eq!(failure.check_site, ProgressCheckSite::ReceiptCommit);
                    assert_eq!(
                        (failure.attempted_index, failure.frame_bytes),
                        (Some(27), Some(144))
                    );
                    assert_eq!(failure.ledger_bytes, 2736 + u64::from(one_byte_over));
                    assert_eq!(failure.total_check_bytes, 2880 + u64::from(one_byte_over));
                    assert_eq!(failure.oldest_age_us, Some(commit_ms * 1000));
                    assert_eq!(failure.highest_committed_index, Some(26));
                    assert_eq!(failure.entry_count, 20);
                    assert_eq!(turn.sent.packets, 18);
                    assert_eq!(turn.commits.len(), 1);
                }
            }
        }
    }

    #[test]
    fn receipt_and_control_errors_never_invent_or_replace_progress_observations() {
        for frozen_failure in [false, true] {
            for invalid in 0..5 {
                let mut turn = ControlTurn::new();
                turn.commit(7, 0);
                let valid = turn.sender.protect_rtcp(&turn.compound(Some(6))).unwrap();
                turn.queue_cipher(valid.clone(), 199);
                assert_eq!(turn.decide(200, None), Ok(true));
                let last_control = turn.sent.last_control;
                if frozen_failure {
                    assert_eq!(
                        turn.decide(721, None),
                        Err("remote media progress expired; retire generation".into())
                    );
                }
                let before = turn.port.snapshot().last_progress_failure;
                let (notify, receipt) = oneshot::channel();
                let (index, commit_ms) = if invalid == 0 { (7, 721) } else { (8, 720) };
                turn.commits.push_back(PendingCommit {
                    index,
                    timestamp: index as u32 * 1920,
                    opus_bytes: 100,
                    submitted: turn.at(700),
                    source_ready: turn.at(700),
                    receipt,
                });
                if invalid == 1 {
                    drop(notify);
                } else {
                    notify
                        .send(Commit::Committed {
                            framed_bytes: 144,
                            at: turn.at(commit_ms),
                        })
                        .unwrap_or_else(|_| panic!("receipt closed"));
                }
                let actor_at = turn.at(721);
                let result = if invalid <= 1 {
                    reap_commits(
                        &mut turn.commits,
                        &mut turn.admission,
                        &mut turn.ledger,
                        &turn.port,
                        640,
                        &mut turn.sent,
                        None,
                    )
                } else {
                    let cipher = match invalid {
                        2 => {
                            let mut forged =
                                turn.sender.protect_rtcp(&turn.compound(Some(7))).unwrap();
                            *forged.last_mut().unwrap() ^= 1;
                            forged
                        }
                        3 => valid, // Exact replay must fail before receipt processing.
                        _ => turn.sender.protect_rtcp(&turn.compound(Some(9))).unwrap(),
                    };
                    turn.queue_cipher(cipher, 719);
                    let frame = turn.incoming.try_recv().unwrap();
                    receive_control(
                        Some(frame),
                        &mut turn.receiver,
                        &turn.fixture,
                        &mut turn.commits,
                        &mut turn.admission,
                        &mut turn.ledger,
                        &mut turn.sent,
                        &mut turn.received,
                        &mut turn.ready,
                        &turn.port,
                        None,
                        || actor_at,
                    )
                };
                match invalid {
                    0 => assert_eq!(result, Err("invalid committed media index".into())),
                    1 => assert_eq!(result, Err("probe commitment owner closed".into())),
                    2 => assert_eq!(result, Err("SRTP authentication failed".into())),
                    3 => assert!(result.is_err()),
                    _ => assert_eq!(result, Err("invalid remote terminal frontier".into())),
                }
                assert_eq!(turn.port.snapshot().last_progress_failure, before);
                assert_eq!(turn.sent.last_control, last_control);
                assert_eq!(turn.port.snapshot().terminal_feedback, 1);
            }
        }
    }

    #[test]
    fn progress_failure_records_exact_checked_state_and_last_authenticated_timing() {
        let mut turn = ControlTurn::new();
        assert!(turn.port.snapshot().last_progress_failure.is_none());
        assert!(
            serde_json::to_value(turn.port.snapshot()).unwrap()["last_progress_failure"].is_null()
        );
        turn.commit(7, 0);
        let plain = packet::compound(packet::Report {
            sender_ssrc: turn.peer.ssrc_tx,
            peer_ssrc: turn.peer.ssrc_rx,
            cname: &turn.peer.cname,
            sender: Some((1 << 32, 1920, 1, 100)),
            feedback: packet::Feedback {
                terminal: Some(6),
                ..packet::Feedback::default()
            },
        });
        let cipher = turn.sender.protect_rtcp(&plain).unwrap();
        turn.queued
            .try_send(Ok(InboundFrame {
                frame: (OP_RTCP, cipher),
                body_complete: turn.at(100),
                noise_done: turn.at(101),
            }))
            .unwrap_or_else(|_| panic!("control inbox full"));
        assert_eq!(turn.decide(200, None), Ok(true));
        turn.commit(8, 700);
        let (_unresolved, receipt) = oneshot::channel();
        turn.commits.push_back(PendingCommit {
            index: 9,
            timestamp: 9 * 1920,
            opus_bytes: 100,
            submitted: turn.at(710),
            source_ready: turn.at(710),
            receipt,
        });
        assert_eq!(turn.decide(720, Some(144)), Ok(false));
        assert!(turn.port.snapshot().last_progress_failure.is_none());
        let reads = std::cell::Cell::new(0);
        let at = turn.at(721);
        let later = turn.at(999);
        assert_eq!(
            control_progress(
                &mut turn.incoming,
                &mut turn.receiver,
                &turn.fixture,
                &mut turn.commits,
                &mut turn.admission,
                &mut turn.ledger,
                &mut turn.sent,
                &mut turn.received,
                &mut turn.ready,
                &turn.port,
                Some(144),
                || {
                    let count = reads.get();
                    reads.set(count + 1);
                    if count == 0 {
                        at
                    } else {
                        later
                    }
                },
            ),
            Err("remote media progress expired; retire generation".into())
        );
        assert_eq!(
            reads.get(),
            1,
            "observation must not read a later decision time"
        );
        let frozen = turn.port.snapshot().last_progress_failure.unwrap();
        assert_eq!(
            serde_json::to_value(&frozen).unwrap(),
            serde_json::json!({
                "reason": "expired",
                "check_site": "media_admission", "attempted_index": null, "frame_bytes": null,
                "ledger_bytes": 288, "window_bytes": 2880, "entry_count": 2,
                "oldest_index": 7, "oldest_age_us": 721000,
                "highest_committed_index": 8, "terminal_frontier": 6, "max_age_us": 720000,
                "waiting_bytes": 144, "pending_bytes": 144, "pending_receipt_count": 1,
                "committed_receipt_count": 2, "check_extra_bytes": 288, "total_check_bytes": 576,
                "last_authenticated_control": {
                    "terminal": 6, "sender_clock_present": true,
                    "body_to_noise_us": 1000, "noise_to_processed_us": 99000,
                    "since_processed_us": 521000, "since_noise_us": 620000, "since_body_us": 621000
                }
            })
        );
        // Retirement may change health, but it cannot replace the original cause.
        turn.port.closed.store(true, Ordering::Relaxed);
        turn.port.stats(|stats| {
            stats.stopped = true;
            stats.remote_clock_valid = false;
        });
        assert_eq!(turn.port.snapshot().last_progress_failure, Some(frozen));
        assert_eq!(turn.ledger.bytes(), 288);
        assert_eq!(turn.commits.len(), 1);
    }

    #[test]
    fn progress_failure_window_precedence_and_current_receipt_charges_are_exact() {
        for one_byte_over in [false, true] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            for index in 8..=24 {
                turn.commit(index, 700);
            }
            turn.ledger
                .commit(25, 99 + usize::from(one_byte_over), turn.at(700))
                .unwrap();
            let (notify, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 26,
                timestamp: 26 * 1920,
                opus_bytes: 1,
                submitted: turn.at(719),
                source_ready: turn.at(719),
                receipt,
            });
            notify
                .send(Commit::Committed {
                    framed_bytes: 45,
                    // This check used to expire the ACK-covered old entry 7.
                    at: turn.at(721),
                })
                .unwrap_or_else(|_| panic!("receipt closed"));
            let (_unresolved, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 27,
                timestamp: 27 * 1920,
                opus_bytes: 100,
                submitted: turn.at(719),
                source_ready: turn.at(719),
                receipt,
            });
            turn.queue_ack(Some(7), 719);
            let result = turn.decide(721, Some(144));
            assert_eq!(turn.ledger.bytes(), 2592 + usize::from(one_byte_over));
            assert_eq!(turn.commits.len(), 1);
            assert_eq!(turn.sent.packets, 19);
            if one_byte_over {
                assert_eq!(
                    result,
                    Err("remote media window exhausted; retire generation".into())
                );
                let failure = turn.port.snapshot().last_progress_failure.unwrap();
                assert_eq!(
                    failure.reason,
                    packet::ProgressFailureReason::WindowExhausted
                );
                assert_eq!(
                    (failure.check_extra_bytes, failure.total_check_bytes),
                    (288, 2881)
                );
                assert_eq!(
                    (failure.pending_bytes, failure.pending_receipt_count),
                    (144, 1)
                );
                assert_eq!(
                    (failure.oldest_index, failure.oldest_age_us),
                    (Some(8), Some(21000))
                );
                assert_eq!(failure.terminal_frontier, Some(7));
                assert_eq!(
                    failure
                        .last_authenticated_control
                        .unwrap()
                        .since_processed_us,
                    Some(0)
                );
            } else {
                assert_eq!(result, Ok(true));
                assert!(turn.port.snapshot().last_progress_failure.is_none());
            }
        }
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        for index in 8..=24 {
            turn.commit(index, 0);
        }
        assert_eq!(
            turn.decide(721, Some(289)),
            Err("remote media window exhausted; retire generation".into())
        );
        let failure = turn.port.snapshot().last_progress_failure.unwrap();
        assert_eq!(
            failure.reason,
            packet::ProgressFailureReason::WindowExhausted
        );
        assert_eq!(
            (failure.oldest_age_us, failure.total_check_bytes),
            (Some(721000), 2881)
        );
        assert!(failure.last_authenticated_control.is_none());
    }

    #[test]
    fn progress_failure_authentication_errors_never_replace_valid_control_or_reason() {
        for invalid in 0..5 {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            let valid = turn.sender.protect_rtcp(&turn.compound(Some(6))).unwrap();
            turn.queue_cipher(valid.clone(), 199);
            assert_eq!(turn.decide(200, None), Ok(true));
            let last = turn.sent.last_control;
            let cipher = match invalid {
                0 => {
                    let mut forged = valid.clone();
                    *forged.last_mut().unwrap() ^= 1;
                    forged
                }
                1 => valid, // Exact SRTCP replay, not a newly protected equal frontier.
                2 => turn.sender.protect_rtcp(&turn.compound(Some(8))).unwrap(),
                3 => turn.sender.protect_rtcp(&turn.compound(Some(5))).unwrap(),
                _ => {
                    let mut plain = turn.compound(Some(7));
                    plain[32 + 10] ^= 1;
                    turn.sender.protect_rtcp(&plain).unwrap()
                }
            };
            turn.queue_cipher(cipher, 719);
            assert!(turn.decide(721, None).is_err());
            assert_eq!(turn.sent.last_control, last);
            assert!(turn.port.snapshot().last_progress_failure.is_none());
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(
                turn.decide(721, None),
                Err("remote media progress expired; retire generation".into())
            );
            let failure = turn.port.snapshot().last_progress_failure.unwrap();
            assert_eq!(
                failure
                    .last_authenticated_control
                    .as_ref()
                    .unwrap()
                    .terminal,
                Some(6)
            );
            assert_eq!(
                failure
                    .last_authenticated_control
                    .as_ref()
                    .unwrap()
                    .since_processed_us,
                Some(521000)
            );
            let frozen = failure.clone();
            let invalid_plain = turn.compound(Some(8));
            let invalid_cipher = turn.sender.protect_rtcp(&invalid_plain).unwrap();
            turn.queue_cipher(invalid_cipher, 722);
            assert_eq!(
                turn.decide(723, None),
                Err("invalid remote terminal frontier".into())
            );
            assert_eq!(turn.port.snapshot().last_progress_failure, Some(frozen));
        }
    }

    #[test]
    fn progress_failure_top_turn_observes_waiting_slot_and_unknown_timing_without_charging_it() {
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        let (outgoing, _requests) = mpsc::channel(1);
        let waiting = WaitingPacket::new(
            vec![0; 100],
            0,
            turn.at(700),
            turn.at(700),
            turn.at(700),
            LiveProfile::Ms40,
        );
        turn.admission
            .stage(
                waiting,
                &outgoing,
                0,
                turn.at(700),
                LiveProfile::Ms40,
                &turn.port,
            )
            .unwrap();
        assert_eq!(
            turn.decide(721, None),
            Err("remote media progress expired; retire generation".into())
        );
        let frozen = turn.port.snapshot().last_progress_failure.unwrap();
        assert_eq!(
            (
                frozen.waiting_bytes,
                frozen.check_extra_bytes,
                frozen.total_check_bytes
            ),
            (Some(144), 0, 144)
        );
        assert!(frozen.last_authenticated_control.is_none());
        turn.queue_ack(Some(7), 722);
        assert_eq!(turn.decide(723, None), Ok(true));
        assert_eq!(
            turn.port.snapshot().last_progress_failure,
            Some(frozen.clone())
        );
        turn.commit(8, 723);
        assert_eq!(
            turn.decide(1444, None),
            Err("remote media progress expired; retire generation".into())
        );
        let replacement = turn.port.snapshot().last_progress_failure.unwrap();
        assert_eq!(replacement.oldest_index, Some(8));
        assert_eq!(frozen.oldest_index, Some(7));

        let metadata = AuthenticatedControl {
            terminal: None,
            sender_clock_present: false,
            body_complete: turn.at(10),
            noise_done: turn.at(9),
            processed: turn.at(8),
        }
        .observe(turn.at(7));
        assert_eq!(
            (
                metadata.body_to_noise_us,
                metadata.noise_to_processed_us,
                metadata.since_processed_us,
                metadata.since_noise_us,
                metadata.since_body_us
            ),
            (None, None, None, None, None)
        );
    }

    #[test]
    fn media_wake_processes_terminal_ack_queued_after_top_of_turn_check() {
        for queued_ack in [false, true] {
            let mut turn = ControlTurn::new();
            let profile = LiveProfile::Ms40;
            turn.commit(7, 0);
            assert_eq!(turn.decide(719, None), Ok(false));

            let (outgoing, _requests) = mpsc::channel(1);
            let mut budget = packet::Budget::new(50_000, 144, turn.at(719));
            // Existing credit is briefly short of the waiting frame. Both the
            // media wake and control inbox can be ready on the next select poll.
            assert!(budget.admit_media(7, turn.at(719)));
            let waiting = WaitingPacket::new(
                vec![0; 100],
                0,
                turn.at(679),
                turn.at(719),
                turn.at(719),
                profile,
            );
            turn.admission
                .stage(waiting, &outgoing, 0, turn.at(719), profile, &turn.port)
                .unwrap();
            let wake = turn
                .admission
                .wake_at(&mut budget, turn.at(719), profile)
                .unwrap();
            assert!(wake > turn.at(719) && wake <= turn.at(721));
            if queued_ack {
                turn.queue_ack(Some(7), 720); // After the empty top-of-turn check.
            }
            let (waiting, _permit) = turn
                .admission
                .take_ready(&mut budget, turn.at(721), profile, &turn.port)
                .unwrap();
            let pending_bytes: usize = turn
                .commits
                .iter()
                .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
                .sum();
            let extra_bytes = waiting.bytes() + pending_bytes;
            assert_eq!(extra_bytes, 144);
            // The original media-wake decision fails even with the protected
            // terminal ACK already queued. It has not authenticated that ACK.
            assert_eq!(
                turn.ledger.check(turn.at(721), extra_bytes),
                Err("remote media progress expired; retire generation".into())
            );
            let decision = turn.decide(721, Some(waiting.bytes()));
            if queued_ack {
                assert_eq!(decision, Ok(true));
                assert_eq!(turn.ledger.bytes(), 0);
                assert_eq!(turn.port.snapshot().terminal_feedback, 1);
                assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 721);
                assert_eq!(turn.port.snapshot().rx_bytes, 132);
            } else {
                assert_eq!(
                    decision,
                    Err("remote media progress expired; retire generation".into())
                );
                assert_eq!(turn.ledger.bytes(), 144);
                assert_eq!(turn.port.snapshot().terminal_feedback, 0);
            }
            assert!(turn.incoming.is_empty());
            assert_eq!(turn.port.snapshot().dropped_capture, 0);
        }
    }

    #[test]
    fn media_wake_valid_ack_keeps_waiting_and_pending_byte_window() {
        for one_byte_over in [false, true] {
            let mut turn = ControlTurn::new();
            let profile = LiveProfile::Ms40;
            let limit = 144 * ((600 + 120) / 40 + 2);
            assert_eq!(limit, 2880);
            turn.commit(7, 0);
            // A valid prefix ACK leaves fresh commitments. Legal framed packet
            // sizes put full waiting-plus-pending admission exactly at Wmax or
            // one byte above it, without involving the age guard.
            for index in 8..=24 {
                turn.commit(index, 700);
            }
            turn.ledger
                .commit(25, 99 + usize::from(one_byte_over), turn.at(700))
                .unwrap();
            turn.ledger.commit(26, 45, turn.at(700)).unwrap();
            assert_eq!(turn.decide(719, None), Ok(false));

            let (outgoing, _requests) = mpsc::channel(1);
            let waiting = WaitingPacket::new(
                vec![0; 100],
                0,
                turn.at(679),
                turn.at(719),
                turn.at(719),
                profile,
            );
            turn.admission
                .stage(waiting, &outgoing, 0, turn.at(719), profile, &turn.port)
                .unwrap();
            let (_notify, receipt) = oneshot::channel();
            turn.commits.push_back(PendingCommit {
                index: 27,
                timestamp: 0,
                opus_bytes: 100,
                submitted: turn.at(719),
                source_ready: turn.at(719),
                receipt,
            });
            turn.queue_ack(Some(7), 720);
            let mut budget = packet::Budget::new(50_000, 144, turn.at(719));
            let (waiting, _permit) = turn
                .admission
                .take_ready(&mut budget, turn.at(721), profile, &turn.port)
                .unwrap();
            let pending_bytes: usize = turn
                .commits
                .iter()
                .map(|pending| pending.opus_bytes + packet::MEDIA_OVERHEAD)
                .sum();
            let extra_bytes = waiting.bytes() + pending_bytes;
            assert_eq!(extra_bytes, 288);
            let decision = turn.decide(721, Some(waiting.bytes()));
            assert_eq!(
                decision,
                if one_byte_over {
                    Err("remote media window exhausted; retire generation".into())
                } else {
                    Ok(true)
                }
            );
            assert_eq!(turn.port.snapshot().terminal_feedback, 1);
            assert_eq!(
                turn.ledger.bytes() + extra_bytes,
                limit + usize::from(one_byte_over)
            );
            assert_eq!(turn.commits.len(), 1);
            assert!(turn.incoming.is_empty());
            // Omitting the uncommitted packet charge would incorrectly admit
            // even the over-window case after authenticating the valid ACK.
            assert!(turn.ledger.check(turn.at(721), waiting.bytes()).is_ok());
        }
    }

    #[test]
    fn queued_control_rejects_tampering_and_unexpected_ssrc_without_credit() {
        for unknown_author in [false, true] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            let mut plain = turn.compound(Some(7));
            let cipher = if unknown_author {
                let ssrc = turn.peer.ssrc_tx ^ 1;
                // Structurally valid compound protected under the same key but
                // an unrecognized author; the exact-SSRC receiver must reject.
                for offset in [4, 32 + 4, 32 + 20 + 4] {
                    plain[offset..offset + 4].copy_from_slice(&ssrc.to_be_bytes());
                }
                dmsg_srtp_sys::Sender::new(&turn.peer.media_tx, ssrc)
                    .unwrap()
                    .protect_rtcp(&plain)
                    .unwrap()
            } else {
                let mut cipher = turn.sender.protect_rtcp(&plain).unwrap();
                *cipher.last_mut().unwrap() ^= 1;
                cipher
            };
            turn.queue_cipher(cipher, 719);
            let error = turn.decide(721, None).unwrap_err();
            if unknown_author {
                assert_eq!(error, "SRTP unexpected SSRC");
            } else {
                assert_eq!(error, "SRTP authentication failed");
            }
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(turn.port.snapshot().terminal_feedback, 0);
            assert_eq!(turn.port.snapshot().rx_bytes, 0);
        }
    }

    #[test]
    fn queued_authenticated_control_keeps_whole_compound_and_frontier_checks() {
        for (offset, expected) in [
            (8, "invalid fixture RTCP peer"),
            (32 + 10, "invalid fixture RTCP CNAME"),
            (32 + 20 + 12, "invalid fixture RTCP APP"),
        ] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            let mut plain = turn.compound(Some(7));
            plain[offset] ^= 1;
            let cipher = turn.sender.protect_rtcp(&plain).unwrap();
            turn.queue_cipher(cipher, 719);
            assert_eq!(turn.decide(721, None), Err(expected.into()));
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(turn.port.snapshot().terminal_feedback, 0);
            assert_eq!(turn.port.snapshot().rx_bytes, 0);
        }
        for (terminal, expected) in [
            (8, "invalid remote terminal frontier"),
            (1 << 48, "invalid fixture RTCP cursor"),
        ] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            turn.queue_ack(Some(terminal), 719);
            assert_eq!(turn.decide(721, None), Err(expected.into()));
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(turn.port.snapshot().terminal_feedback, 0);
        }
    }

    #[test]
    fn queued_prefix_ack_cannot_hide_remaining_expired_commitment() {
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        turn.commit(8, 0);
        turn.queue_ack(Some(7), 719);
        assert_eq!(
            turn.decide(721, None),
            Err("remote media progress expired; retire generation".into())
        );
        assert_eq!(turn.ledger.bytes(), 144);
        assert_eq!(turn.port.snapshot().terminal_feedback, 1);
        assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 721);
    }

    #[test]
    fn queued_stale_and_nonprogress_control_cannot_reset_oldest_commitment_age() {
        for terminal in [Some(6), Some(7), None] {
            let mut turn = ControlTurn::new();
            turn.commit(7, 0);
            turn.queue_ack(Some(7), 199);
            assert_eq!(turn.decide(200, None), Ok(true));
            turn.commit(8, 200);
            turn.queue_ack(terminal, 919);
            let expected = if terminal == Some(6) {
                "invalid remote terminal frontier"
            } else {
                "remote media progress expired; retire generation"
            };
            assert_eq!(turn.decide(921, None), Err(expected.into()));
            assert_eq!(turn.ledger.bytes(), 144);
            assert_eq!(
                turn.port.snapshot().terminal_feedback,
                if terminal == Some(7) { 2 } else { 1 }
            );
            assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 200);
        }
        // A fully authenticated sender-clock observation is not a terminal ACK.
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        let plain = packet::compound(packet::Report {
            sender_ssrc: turn.peer.ssrc_tx,
            peer_ssrc: turn.peer.ssrc_rx,
            cname: &turn.peer.cname,
            sender: Some((1 << 32, 1920, 1, 100)),
            feedback: packet::Feedback::default(),
        });
        let cipher = turn.sender.protect_rtcp(&plain).unwrap();
        turn.queue_cipher(cipher, 719);
        assert_eq!(
            turn.decide(721, None),
            Err("remote media progress expired; retire generation".into())
        );
        assert_eq!(turn.ledger.bytes(), 144);
        assert_eq!(turn.port.snapshot().terminal_feedback, 0);
        assert_eq!(turn.port.snapshot().rx_bytes, 152);
        assert_eq!(turn.port.snapshot().remote_clock_rejected_reports, 0);
    }

    #[test]
    fn selected_control_reaps_pending_receipt_before_fast_ack_validation() {
        let mut turn = ControlTurn::new();
        let (notify, receipt) = oneshot::channel();
        notify
            .send(Commit::Committed {
                framed_bytes: 144,
                at: turn.at(0),
            })
            .unwrap_or_else(|_| panic!("receipt closed"));
        turn.commits.push_back(PendingCommit {
            index: 7,
            timestamp: 1920,
            opus_bytes: 100,
            submitted: turn.at(0),
            source_ready: turn.at(0),
            receipt,
        });
        turn.queue_ack(Some(7), 1);
        let frame = turn.incoming.try_recv().unwrap();
        let now = turn.at(2);
        // Same handler as select: unlike the top-of-turn path, its receipt was
        // completed while awaiting control and has not been reaped beforehand.
        receive_control(
            Some(frame),
            &mut turn.receiver,
            &turn.fixture,
            &mut turn.commits,
            &mut turn.admission,
            &mut turn.ledger,
            &mut turn.sent,
            &mut turn.received,
            &mut turn.ready,
            &turn.port,
            None,
            || now,
        )
        .unwrap();
        assert!(turn.commits.is_empty());
        assert_eq!(turn.sent.packets, 1);
        assert_eq!(turn.ledger.bytes(), 0);
        assert_eq!(turn.port.snapshot().terminal_feedback, 1);
        assert_eq!(turn.port.snapshot().max_feedback_cycle_ms, 2);
    }

    #[test]
    fn control_progress_processes_at_most_one_ready_frame() {
        let mut turn = ControlTurn::new();
        turn.commit(7, 0);
        let first = turn.sender.protect_rtcp(&turn.compound(None)).unwrap();
        let second = turn.sender.protect_rtcp(&turn.compound(Some(7))).unwrap();
        turn.queue_cipher(first, 719);
        let queued = turn.queued.clone();
        let at = turn.at(721);
        // Refill the freed capacity-one inbox precisely when the first handler
        // asks for its clock observation, without scheduling another actor.
        let refilled = std::cell::Cell::new(false);
        let second = std::cell::RefCell::new(Some(second));
        let result = control_progress(
            &mut turn.incoming,
            &mut turn.receiver,
            &turn.fixture,
            &mut turn.commits,
            &mut turn.admission,
            &mut turn.ledger,
            &mut turn.sent,
            &mut turn.received,
            &mut turn.ready,
            &turn.port,
            None,
            || {
                if !refilled.replace(true) {
                    queued
                        .try_send(Ok(InboundFrame {
                            frame: (OP_RTCP, second.borrow_mut().take().unwrap()),
                            body_complete: at,
                            noise_done: at,
                        }))
                        .unwrap_or_else(|_| panic!("control inbox full"));
                }
                at
            },
        );
        assert_eq!(
            result,
            Err("remote media progress expired; retire generation".into())
        );
        assert_eq!(turn.ledger.bytes(), 144);
        assert_eq!(turn.port.snapshot().terminal_feedback, 0);
        assert_eq!(turn.incoming.len(), 1);
    }

    fn held_codec_endpoint_progress(kind: super::super::codec::Kind) {
        use super::super::codec::Hold;
        use std::future::Future;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (a, b, relay_fixture, relay_key) = fixture::pair(PublicConfig {
            relay_addr: addr.clone(),
            domain: "m1v.fixture".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        })
        .unwrap();
        let (stop, stopped) = watch::channel(false);
        let (inject, mut injection) = mpsc::channel(1);
        let peer_stop = stopped.clone();
        let peer = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let relay_stop = peer_stop.clone();
                    let relay =
                        tokio::spawn(relay::serve(listener, relay_fixture, relay_key, relay_stop));
                    let mut media = relay::connect(&addr, &b, true).await.unwrap();
                    let mut control = relay::connect(&addr, &b, false).await.unwrap();
                    let mut sender = dmsg_srtp_sys::Sender::new(&b.media_tx, b.ssrc_tx).unwrap();
                    for index in 0..2u64 {
                        injection.recv().await.unwrap();
                        let plain = packet::compound(packet::Report {
                            sender_ssrc: b.ssrc_tx,
                            peer_ssrc: b.ssrc_rx,
                            cname: &b.cname,
                            sender: None,
                            feedback: packet::Feedback::default(),
                        });
                        control
                            .outgoing
                            .send(SendRequest {
                                opcode: OP_RTCP,
                                payload: sender.protect_rtcp(&plain).unwrap(),
                                deadline: None,
                                committed: None,
                            })
                            .await
                            .unwrap();
                        let plain = packet::rtp(
                            b.ssrc_tx,
                            u64::from(b.initial_sequence) + index,
                            b.initial_timestamp.wrapping_add(index as u32 * 1920),
                            &[0x50],
                        );
                        media
                            .outgoing
                            .send(SendRequest {
                                opcode: OP_RTP,
                                payload: sender.protect_rtp(&plain).unwrap(),
                                deadline: None,
                                committed: None,
                            })
                            .await
                            .unwrap();
                    }
                    let mut peer_stop = peer_stop;
                    cancelled(&mut peer_stop).await;
                    media.joined_close().await;
                    control.joined_close().await;
                    relay.abort();
                    let _ = relay.await;
                });
        });
        let (entered, entry) = std::sync::mpsc::sync_channel(1);
        let hold = Hold {
            kind,
            entered,
            release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            admitted: Arc::new(AtomicU64::new(0)),
            controlled: Arc::new(AtomicU64::new(0)),
        };
        let port = Arc::new(test_port());
        let (input, incoming) = mpsc::channel(4);
        let actor_port = port.clone();
        let observer_port = port.clone();
        let observer_input = input.clone();
        let actor_hold = hold.clone();
        let actor_stop = stopped.clone();
        let (connected, connection) = std::sync::mpsc::sync_channel(1);
        let (returned, returning) = std::sync::mpsc::sync_channel(1);
        let (observe, observation) = oneshot::channel::<u64>();
        let (observed, source_slots) = std::sync::mpsc::sync_channel(1);
        let actor = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let media = relay::connect(&a.relay_addr, &a, true).await.unwrap();
                    let control = relay::connect(&a.relay_addr, &a, false).await.unwrap();
                    connected.send(()).unwrap();
                    let endpoint = endpoint_owned(
                        a,
                        actor_port,
                        incoming,
                        media,
                        control,
                        actor_stop,
                        Some(actor_hold),
                    );
                    tokio::pin!(endpoint);
                    let outcome = tokio::select! {
                        outcome = &mut endpoint => outcome,
                        baseline = observation => {
                            match baseline {
                                Ok(baseline) => {
                                    // One bounded snapshot after the four sends,
                                    // with the producer quiescent. Poll the same
                                    // endpoint so decode gets a legal collection
                                    // turn instead of racing the supervisor's
                                    // immediate channel-capacity read.
                                    let slots = std::future::poll_fn(|cx| {
                                        if let std::task::Poll::Ready(outcome) = endpoint.as_mut().poll(cx) {
                                            return std::task::Poll::Ready(Err(outcome));
                                        }
                                        let queued = observer_input.max_capacity() - observer_input.capacity();
                                        if kind == super::super::codec::Kind::Decode && queued == 4 {
                                            return std::task::Poll::Pending;
                                        }
                                        let stats = observer_port.snapshot();
                                        std::task::Poll::Ready(Ok((
                                            queued,
                                            stats.dropped_capture.saturating_sub(baseline),
                                            stats.encoded_packets,
                                            stats.encode_duration_bins.iter().sum::<u64>(),
                                        )))
                                    }).await;
                                    match slots {
                                        Ok(slots) => {
                                            observed.send(slots).unwrap();
                                            endpoint.await
                                        }
                                        Err(outcome) => outcome,
                                    }
                                }
                                Err(_) => endpoint.await,
                            }
                        }
                    };
                    returned.send(()).unwrap();
                    outcome
                })
        });
        // This supervisor is outside the endpoint's runtime. Even an inline C
        // operation cannot suppress its release/deadlock guard.
        connection.recv_timeout(Duration::from_secs(3)).unwrap();
        inject.blocking_send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !port.snapshot().ready && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if kind == super::super::codec::Kind::Encode {
            for batch in 0..4 {
                let mut captured = Captured {
                    pcm: [0; 160],
                    len: 160,
                    position: batch * 160,
                    at: Instant::now(),
                    pushed_at: Instant::now(),
                    hardware_age: None,
                    capture_clock: None,
                };
                captured
                    .pcm
                    .copy_from_slice(&super::super::test_tone(batch as usize * 160, 160));
                input.blocking_send(captured).unwrap();
            }
        }
        let held = entry.recv_timeout(Duration::from_secs(3)).is_ok();
        let before_media = hold.admitted.load(Ordering::Relaxed);
        let before_control = hold.controlled.load(Ordering::Relaxed);
        inject.blocking_send(()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline
            && (hold.admitted.load(Ordering::Relaxed) <= before_media
                || hold.controlled.load(Ordering::Relaxed) <= before_control)
        {
            thread::sleep(Duration::from_millis(1));
        }
        let progressed = hold.admitted.load(Ordering::Relaxed) > before_media
            && hold.controlled.load(Ordering::Relaxed) > before_control;
        let position = if kind == super::super::codec::Kind::Encode {
            640
        } else {
            0
        };
        let source_baseline = port.snapshot().dropped_capture;
        let mut sent_all = true;
        for batch in 0..4 {
            input
                .try_send(Captured {
                    pcm: [0; 160],
                    len: 160,
                    position: position + batch * 160,
                    at: Instant::now(),
                    pushed_at: Instant::now(),
                    hardware_age: None,
                    capture_clock: None,
                })
                .unwrap_or_else(|_| sent_all = false);
        }
        observe.send(source_baseline).unwrap();
        let observed_slots = source_slots.recv_timeout(Duration::from_secs(3));
        let _ = stop.send(true);
        let returned_while_held = returning.recv_timeout(Duration::from_millis(20)).is_ok();
        hold.release();
        if !returned_while_held {
            returning.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        let outcome = actor.join().unwrap();
        peer.join().unwrap();
        assert!(
            held,
            "codec operation was not entered: {outcome:?}, ready={}, encoded={}, received={}",
            port.snapshot().ready,
            port.snapshot().encoded_packets,
            port.snapshot().received_rtp_packets
        );
        assert!(
            progressed,
            "authenticated endpoint media/control blocked until codec release: {outcome:?}"
        );
        assert!(
            !returned_while_held,
            "endpoint retired before joining the held codec owner"
        );
        assert!(sent_all, "the original ring must accept four batches");
        let (queued, discarded, encoded, encode_operations) =
            observed_slots.expect("source ownership was not observed while codec remained held");
        let samples = LiveProfile::Ms40.samples();
        assert_eq!(input.max_capacity(), 4);
        assert!(queued <= 4, "capture exceeded the original ring");
        assert_eq!(encoded, 0, "encoder output appeared before codec release");
        assert_eq!(
            encode_operations, 0,
            "encoder work completed before the held operation was released"
        );
        // At this endpoint poll boundary there is no dequeued-but-unprocessed
        // batch. While the sole codec operation is held, collected samples can
        // only remain in the assembled source slot or be accounted as loss.
        // Do not require loss-free 40ms scheduling under host CPU contention;
        // the controlled-Instant source-slot tests check age and deadlines.
        assert!(discarded <= samples as u64);
        let assembled = samples
            .checked_sub(queued * 160 + discarded as usize)
            .expect("capture escaped the ring/source/loss accounting");
        assert!(assembled <= samples, "assembled source frame grew");
        let source_owned = if kind == super::super::codec::Kind::Encode {
            assert_eq!(
                (queued, assembled, discarded),
                (4, 0, 0),
                "encode owns the source slot and must retain the extra ring"
            );
            samples
        } else {
            assert!(queued < 4, "decode prevented legal source collection");
            assembled
        };
        assert!(source_owned + queued * 160 <= samples + 4 * 160);
    }

    #[test]
    fn held_encoder_does_not_block_authenticated_endpoint_media_and_control() {
        held_codec_endpoint_progress(super::super::codec::Kind::Encode);
    }

    #[test]
    fn held_decoder_does_not_block_authenticated_endpoint_media_and_control() {
        held_codec_endpoint_progress(super::super::codec::Kind::Decode);
    }

    // Android's current-capacity fresh request; the existing batch is still <=160.
    fn renderer_fresh_pull_limit(depth: usize) -> usize {
        assert!(depth <= 640);
        (640 - depth).min(160)
    }

    #[tokio::test]
    async fn known_decode_compute_precedes_empty_sink_handoff_without_moving_presentation() {
        let profile = LiveProfile::Ms40;
        let mut results = Vec::new();
        for head_quantum in [160u64, 320] {
            for write_limit in [160usize, 80] {
                for stalled in [false, true] {
                    let origin = Instant::now();
                    let at = |ns| origin + Duration::from_nanos(ns);
                    let mut port = test_port();
                    let (input, mut incoming) = mpsc::channel(4);
                    port.input = input;
                    let port = Arc::new(port);
                    let audio = AudioPort { port: port.clone() };
                    let mut capture = CaptureFrame::new(640, port.clone());
                    let mut received = test_received();
                    let mut feedback = packet::Feedback::default();
                    received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
                    let mut owner = codec::Owner::start(profile, None).await.unwrap();
                    let mut flight: Option<CodecFlight> = None;
                    let mut reset = false;
                    let mut admission = MediaAdmission::default();
                    let mut budget = packet::Budget::new(50_000, 144, origin);
                    let (outgoing, mut requests) = mpsc::channel(1);
                    let mut finish = None;
                    let mut decode_started = None;
                    let mut published = None;
                    let mut first_write = None;
                    let mut first_presentation = None;
                    let mut hardware = VecDeque::new();
                    let (mut submitted, mut consumed) = (0u64, 0u64);
                    let mut consumption_remainder = 0u64;
                    let mut pending = VecDeque::new(); // The renderer's existing <=160 batch.
                    let mut next_sample = 0usize;
                    let mut max_native_hold = 0u64;
                    for ns in (0..=160_000_000u64).step_by(50_000) {
                        let now = at(ns);
                        // Actual consumption and independently quantized head readback.
                        // Tags preserve each sample's source presentation time.
                        if hardware.is_empty() {
                            consumption_remainder = 0;
                        } else {
                            consumption_remainder += 50_000 * 16_000;
                            while consumption_remainder >= 1_000_000_000 {
                                consumption_remainder -= 1_000_000_000;
                                let Some((sample, _pcm)) = hardware.pop_front() else {
                                    consumption_remainder = 0;
                                    break;
                                };
                                assert!(
                                    ns >= 80_000_000 + sample as u64 * SAMPLE_NS,
                                    "early actual presentation: ns={ns} sample={sample}"
                                );
                                consumed += 1;
                            }
                        }
                        let head = consumed / head_quantum * head_quantum;
                        audio.sink_queued((submitted - head) as usize);
                        let mut wake = ns % 10_000_000 == 0;
                        if ns <= 30_000_000 && ns % 10_000_000 == 0 {
                            assert!(audio.push_at(
                                &[1; 160],
                                ns / 10_000_000 * 160,
                                now,
                                now,
                                None,
                                None
                            ));
                            wake = true;
                        }
                        if finish.is_some_and(|due| ns >= due) {
                            let end = finish.take().unwrap();
                            let decode = matches!(
                                flight.as_ref().unwrap().source.as_ref(),
                                Some(CodecSource::Decode(_))
                            );
                            let mut completed = owner.completed().await.unwrap();
                            completed.finished = at(end);
                            completed.duration =
                                Duration::from_micros(if decode { 12_151 } else { 2_000 });
                            apply_codec_completion(
                                completed,
                                &mut flight,
                                &mut received,
                                &mut admission,
                                &mut feedback,
                                0,
                                now,
                                profile,
                                &port,
                            )
                            .unwrap();
                            if decode {
                                published = Some(ns);
                            }
                            wake = true;
                        }
                        if admission
                            .wake_at(&mut budget, now, profile)
                            .is_some_and(|due| now >= due)
                        {
                            if let Some((waiting, permit)) =
                                admission.take_ready(&mut budget, now, profile, &port)
                            {
                                permit.send(SendRequest {
                                    opcode: OP_RTP,
                                    payload: waiting.opus.to_vec(),
                                    deadline: Some(waiting.deadline),
                                    committed: None,
                                });
                                requests.try_recv().unwrap();
                                admission.committed(now, 640);
                            }
                            wake = true;
                        }
                        if wake {
                            // Declared finite input: no imaginary PLC after its last frame.
                            if received.playout.as_ref().unwrap().cursor < 1920 {
                                dispatch_codec(
                                    now,
                                    profile,
                                    &mut received,
                                    &mut capture,
                                    &admission,
                                    &mut feedback,
                                    &mut reset,
                                    &mut owner,
                                    &mut flight,
                                    &outgoing,
                                    0,
                                    &port,
                                )
                                .unwrap();
                            } else {
                                received.maintain_playout(now, profile, &mut feedback, &port);
                            }
                            while capture.can_ingest(
                                profile,
                                flight.as_ref(),
                                &admission,
                                &outgoing,
                                0,
                            ) {
                                let Ok(batch) = incoming.try_recv() else {
                                    break;
                                };
                                capture.ingest(batch, true, now, profile);
                                dispatch_codec(
                                    now,
                                    profile,
                                    &mut received,
                                    &mut capture,
                                    &admission,
                                    &mut feedback,
                                    &mut reset,
                                    &mut owner,
                                    &mut flight,
                                    &outgoing,
                                    0,
                                    &port,
                                )
                                .unwrap();
                            }
                        }
                        if flight.is_some() && finish.is_none() {
                            let decode = matches!(
                                flight.as_ref().unwrap().source.as_ref(),
                                Some(CodecSource::Decode(_))
                            );
                            if decode {
                                decode_started = Some(ns);
                            }
                            finish = Some(ns + if decode { 12_151_000 } else { 2_000_000 });
                        }
                        // Same bounded <=160 ownership through partial nonblocking writes.
                        // A controlled 24ms renderer stall is separate from codec service.
                        if ns % 2_000_000 == 0
                            && !(stalled && (98_000_000..122_000_000).contains(&ns))
                        {
                            let depth = (submitted - head) as usize;
                            let required = if pending.is_empty() {
                                renderer_fresh_pull_limit(depth)
                            } else {
                                pending.len()
                            };
                            if required != 0 && depth + required <= 640 {
                                if pending.is_empty() {
                                    let mut pcm = [0; 160];
                                    let count = audio.pull_at(now, &mut pcm[..required]);
                                    assert!(count <= required);
                                    if count != 0 {
                                        max_native_hold =
                                            max_native_hold.max(ns - published.unwrap());
                                        for value in &pcm[..count] {
                                            pending.push_back((next_sample, *value));
                                            next_sample += 1;
                                        }
                                    }
                                }
                                for _ in 0..pending.len().min(write_limit) {
                                    let sample = pending.pop_front().unwrap();
                                    if hardware.is_empty() {
                                        first_presentation.get_or_insert(ns);
                                        assert!(
                                            ns >= 80_000_000 + sample.0 as u64 * SAMPLE_NS,
                                            "early empty-sink write: ns={ns} sample={}",
                                            sample.0
                                        );
                                    }
                                    first_write.get_or_insert(ns);
                                    hardware.push_back(sample);
                                    submitted += 1;
                                }
                            }
                        }
                        assert!(incoming.len() <= 4 && capture.pcm.len() <= 640);
                        assert!(
                            hardware.len() <= 640
                                && submitted - head <= 640
                                && pending.len() <= 160
                        );
                        assert!(port.render.lock().unwrap().pcm.len() <= 640);
                        assert_eq!(owner.busy(), flight.is_some());
                        assert_eq!(received.first_playout, Some((0, at(80_000_000))));
                    }
                    let stats = port.snapshot();
                    assert_eq!(
                        (
                            stats.late_packets,
                            stats.plc_slots,
                            stats.future_rejected_packets
                        ),
                        (0, 0, 0)
                    );
                    assert_eq!(stats.dropped_capture, 0);
                    assert_eq!((stats.encoded_packets, stats.decoded_packets), (1, 1));
                    assert!(hardware.is_empty() && pending.is_empty());
                    assert_eq!(submitted, consumed);
                    assert_eq!(submitted + stats.expired_render_samples, 640);
                    results.push((
                        head_quantum,
                        write_limit,
                        stalled,
                        decode_started.unwrap(),
                        published.unwrap(),
                        first_write.unwrap(),
                        first_presentation.unwrap(),
                        stats.expired_render_samples,
                        max_native_hold,
                    ));
                    owner.close().unwrap();
                }
            }
        }
        assert!(
            results.iter().all(|row| row.3 == 60_000_000
                && row.4 == 72_200_000
                && row.5 == 80_000_000
                && row.6 == 80_000_000
                && row.7 == 0
                && row.8 == if row.1 == 160 { 13_800_000 } else { 19_800_000 }),
            "first source due must remain80ms, no active tail expiry: {results:?}"
        );
    }

    #[test]
    fn capacity_limited_renderer_conserves_pcm_under_controlled_head_readback() {
        // These are controlled legal readbacks/wakes, not a replay of a phone
        // trace: the physical trace does not establish earlier free capacity.
        // A one-sample later readback distinguishes prompt handoff from expiry.
        let mut outcomes = Vec::new();
        for reclaim_last in [false, true] {
            for write_limit in [160usize, 80] {
                for capacity_limited in [false, true] {
                    let profile = LiveProfile::Ms40;
                    let origin = Instant::now();
                    let at = |us| origin + Duration::from_micros(us);
                    let due = at(80_000);
                    let end = at(120_000);
                    let port = Arc::new(test_port());
                    let audio = AudioPort { port: port.clone() };
                    let mut received = test_received();
                    let mut feedback = packet::Feedback::default();
                    received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
                    let mut work = received
                        .prepare_decode(at(70_371), profile, &mut feedback, &port)
                        .unwrap();
                    let expected = LiveDecoder::new(profile)
                        .unwrap()
                        .decode(work.opus.take().as_ref().unwrap())
                        .unwrap();
                    received
                        .apply_decode(
                            work,
                            at(81_841),
                            Duration::from_micros(11_470),
                            Zeroizing::new(expected.clone()),
                            &port,
                        )
                        .unwrap();
                    assert!(audio.sink_rate(2_739));

                    // Previous-frame PCM accounts for the sink's initial lead.
                    // Its 320 already consumed samples are outside this frame.
                    let mut hardware: VecDeque<(Option<usize>, i16)> =
                        (0..320).map(|_| (None, 0)).collect();
                    let (mut submitted, mut consumed) = (640u64, 320u64);
                    let mut previous_head = 0;
                    let mut consumed_at = at(84_069);
                    let mut pcm = [77; 160]; // One existing renderer batch.
                    let (mut valid, mut offset) = (0, 0);
                    let mut transferred = Vec::new();
                    let mut played = Vec::new();
                    let mut first_tail_pull = None;
                    let tail_wake = if reclaim_last { 116_605 } else { 118_605 };
                    let mut wakes = vec![
                        (84_069, 160),
                        (94_069, 320),
                        (107_220, 480),
                        (tail_wake, 639),
                    ];
                    if reclaim_last {
                        wakes.push((118_605, 640));
                    }
                    wakes.push((120_605, if reclaim_last { 640 } else { 639 }));
                    for (us, head) in wakes {
                        let now = at(us);
                        while !hardware.is_empty()
                            && consumed_at + Duration::from_nanos(SAMPLE_NS) <= now
                        {
                            consumed_at += Duration::from_nanos(SAMPLE_NS);
                            let (sample, value) = hardware.pop_front().unwrap();
                            if let Some(sample) = sample {
                                assert!(
                                    consumed_at
                                        >= due
                                            + received
                                                .remote_clock
                                                .rate
                                                .duration(sample as u64 * 3)
                                );
                                assert_eq!(sample, played.len());
                                assert_eq!(value, expected[sample]);
                                played.push(value);
                            }
                            consumed += 1;
                        }
                        assert!(previous_head <= head && head <= consumed);
                        previous_head = head;
                        let depth = (submitted - head) as usize;
                        assert!(depth <= 640 && hardware.len() <= depth);
                        audio.sink_queued(depth);
                        received.maintain_playout(now, profile, &mut feedback, &port);
                        loop {
                            let depth = (submitted - head) as usize;
                            let pending = valid - offset;
                            let requested = if pending == 0 {
                                if capacity_limited {
                                    renderer_fresh_pull_limit(depth)
                                } else if depth <= 480 {
                                    160 // The previous fresh/full-batch policy.
                                } else {
                                    0
                                }
                            } else {
                                pending
                            };
                            if requested == 0 || depth + requested > 640 {
                                break;
                            }
                            if pending == 0 {
                                pcm.fill(77);
                                audio.sink_queued(depth);
                                valid = audio.pull_at(now, &mut pcm[..requested]);
                                offset = 0;
                                assert!(valid <= requested && requested <= 160);
                                assert!(pcm[valid..].iter().all(|value| *value == 77));
                                if valid == 0 {
                                    break;
                                }
                                let next = transferred.len();
                                assert!(
                                    now + clock::sink_lead(depth, 2_739)
                                        >= due
                                            + received.remote_clock.rate.duration(next as u64 * 3)
                                );
                                assert_eq!(&pcm[..valid], &expected[next..next + valid]);
                                if next == 480 {
                                    first_tail_pull = Some(us);
                                }
                                transferred.extend_from_slice(&pcm[..valid]);
                            }
                            // Finish the entire owned remainder through partial
                            // writes before overwriting the preallocated batch.
                            let count = (valid - offset).min(write_limit);
                            let first = transferred.len() - valid;
                            for index in offset..offset + count {
                                hardware.push_back((Some(first + index), pcm[index]));
                            }
                            offset += count;
                            submitted += count as u64;
                            assert!(submitted - head <= 640 && valid - offset <= 160);
                        }
                        let render = port.render.lock().unwrap();
                        if !render.pcm.is_empty() {
                            assert_eq!(render.start_due, Some(due));
                            assert_eq!(render.end_due, Some(end));
                            assert_eq!(render.source_end, Some(1920));
                        }
                        assert_eq!(
                            render.pcm.len()
                                + transferred.len()
                                + port.snapshot().expired_render_samples as usize,
                            640
                        );
                        assert_eq!(
                            hardware
                                .iter()
                                .filter(|(sample, _)| sample.is_some())
                                .count()
                                + played.len()
                                + valid
                                - offset,
                            transferred.len()
                        );
                        assert_eq!(received.first_playout, Some((0, due)));
                        assert_eq!(received.source_due(1920), end);
                    }
                    while let Some((sample, value)) = hardware.pop_front() {
                        consumed_at += Duration::from_nanos(SAMPLE_NS);
                        if let Some(sample) = sample {
                            assert!(
                                consumed_at
                                    >= due + received.remote_clock.rate.duration(sample as u64 * 3)
                            );
                            assert_eq!(sample, played.len());
                            played.push(value);
                        }
                        consumed += 1;
                    }
                    assert_eq!(submitted, consumed);
                    assert_eq!(valid, offset);
                    assert_eq!(played, transferred);
                    assert_eq!(played, expected[..played.len()]);
                    let stats = port.snapshot();
                    assert_eq!(
                        (
                            stats.decoded_packets,
                            stats.plc_slots,
                            stats.late_packets,
                            stats.future_rejected_packets
                        ),
                        (1, 0, 0, 0)
                    );
                    assert_eq!(stats.dropped_render, stats.expired_render_samples);
                    if !reclaim_last {
                        let trace = stats.last_render_expiry.unwrap();
                        assert_eq!(trace.expiry_vs_end_us, 605);
                        assert_eq!(trace.source_frame_duration_us, 40_000);
                        assert_eq!(trace.transferred_samples, played.len() as u64);
                    }
                    outcomes.push((
                        reclaim_last,
                        write_limit,
                        capacity_limited,
                        played.len(),
                        stats.expired_render_samples,
                        first_tail_pull,
                    ));
                    pcm.zeroize();
                }
            }
        }
        assert!(
            outcomes
                .iter()
                .all(|&(reclaim, _, limited, played, expired, tail)| {
                    if reclaim {
                        played == 640
                            && expired == 0
                            && tail == Some(if limited { 116_605 } else { 118_605 })
                    } else if limited {
                        played == 639 && expired == 1 && tail == Some(118_605)
                    } else {
                        played == 480 && expired == 160 && tail.is_none()
                    }
                }),
            "controlled capacity outcomes: {outcomes:?}"
        );
    }

    #[test]
    fn known_compute_headroom_never_becomes_a_missing_packet_reserve() {
        let origin = Instant::now();
        let due = origin + Duration::from_millis(80);
        let clock = PlayoutClock { cursor: 0, due };
        let early = due - KNOWN_PACKET_COMPUTE;
        assert!(!clock.can_prepare(early - Duration::from_nanos(1), 0, true, 0));
        assert!(clock.can_prepare(early, 0, true, 0));
        for samples in [0usize, 160, 640] {
            assert!(!clock.can_prepare(early, samples, false, 0));
            assert!(!clock.can_prepare(
                due - PREPARATION - Duration::from_nanos(1),
                samples,
                false,
                0
            ));
            assert_eq!(
                clock.can_prepare(due - PREPARATION, samples, false, 0),
                samples >= 160
            );
        }
        // Actual lead and compute headroom combine, but cannot change due.
        assert!(clock.can_prepare(due - Duration::from_millis(60), 640, true, 0));
        assert_eq!(clock.due, due);
    }

    #[test]
    fn computed_pcm_handoff_uses_remaining_source_samples_and_actual_sink_rate() {
        let profile = LiveProfile::Ms40;
        for ppm in [-500i64, -100, 0, 100, 500] {
            for sink_ppb in [-1_000_000i64, 0, 1_000_000] {
                let origin = Instant::now();
                let due = origin + Duration::from_millis(80);
                let port = Arc::new(test_port());
                let audio = AudioPort { port: port.clone() };
                let mut received = test_received();
                let mut feedback = packet::Feedback::default();
                received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
                let rate = Rate {
                    ticks: (48_000 * (1_000_000 + ppm)) as u64,
                    ns: 1_000_000_000_000_000,
                };
                received.remote_clock.rate = rate;
                received.remote_clock.calibrated = true;
                received.synchronize_clock(origin, &port);
                let mut work = received
                    .prepare_decode(due - KNOWN_PACKET_COMPUTE, profile, &mut feedback, &port)
                    .unwrap();
                let output = LiveDecoder::new(profile)
                    .unwrap()
                    .decode(work.opus.take().as_ref().unwrap())
                    .unwrap();
                received
                    .apply_decode(
                        work,
                        due - Duration::from_micros(7_849),
                        Duration::from_micros(12_151),
                        Zeroizing::new(output),
                        &port,
                    )
                    .unwrap();
                let end = due + rate.duration(1920);
                assert!(audio.sink_rate(sink_ppb));
                audio.sink_queued(80);
                let earliest = due - clock::sink_lead(80, sink_ppb);
                assert!(earliest >= due - Duration::from_micros(7_849));
                let mut pcm = [77; 160];
                assert_eq!(
                    audio.pull_at(earliest - Duration::from_nanos(1), &mut pcm),
                    0
                );
                assert_eq!(pcm, [77; 160]);
                assert_eq!(audio.pull_at(earliest, &mut pcm), 160);
                audio.sink_queued(0);
                for offset in [160u64, 320, 480] {
                    let next = due + rate.duration(offset * 3);
                    assert_eq!(audio.pull_at(next - Duration::from_nanos(1), &mut pcm), 0);
                    assert_eq!(port.render.lock().unwrap().end_due, Some(end));
                    assert_eq!(audio.pull_at(next, &mut pcm), 160);
                }
                assert!(port.render.lock().unwrap().pcm.is_empty());
                assert_eq!(audio.pull_at(end, &mut pcm), 0);
                assert_eq!(received.first_playout, Some((0, due)));
                let stats = port.snapshot();
                assert_eq!(
                    (
                        stats.dropped_render,
                        stats.expired_render_samples,
                        stats.plc_slots
                    ),
                    (0, 0, 0)
                );
                assert_eq!(stats.decoded_packets, 1);
            }
        }
    }

    #[test]
    fn early_computation_does_not_rescue_a_stalled_native_tail() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = Arc::new(test_port());
        let audio = AudioPort { port: port.clone() };
        let mut received = test_received();
        let mut feedback = packet::Feedback::default();
        received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
        let mut work = received
            .prepare_decode(at(60), profile, &mut feedback, &port)
            .unwrap();
        let output = LiveDecoder::new(profile)
            .unwrap()
            .decode(work.opus.take().as_ref().unwrap())
            .unwrap();
        received
            .apply_decode(
                work,
                at(73),
                Duration::from_micros(12_151),
                Zeroizing::new(output),
                &port,
            )
            .unwrap();
        let mut pcm = [77; 160];
        assert_eq!(audio.pull_at(at(79), &mut pcm), 0);
        assert_eq!(pcm, [77; 160]);
        assert_eq!(audio.pull_at(at(80), &mut pcm), 160);
        assert_eq!(port.render.lock().unwrap().end_due, Some(at(120)));
        // A real renderer stall still discards every untransferred stale sample.
        assert_eq!(audio.pull_at(at(120), &mut pcm), 0);
        assert_eq!(audio.pull_at(at(121), &mut pcm), 0);
        let stats = port.snapshot();
        assert_eq!(
            (stats.dropped_render, stats.expired_render_samples),
            (480, 480)
        );
        assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
        assert_eq!(received.first_playout, Some((0, at(80))));
    }

    #[test]
    fn render_expired_samples_is_nonblocking_read_only_and_unknown_when_closed() {
        let port = Arc::new(test_port());
        let audio = AudioPort { port: port.clone() };
        port.position.store(17, Ordering::Relaxed);
        audio.sink_queued(181);
        assert!(audio.sink_rate(500_000));
        let due = Instant::now();
        let mut render = port.render.lock().unwrap();
        render.enqueue_observed(due, vec![7; 640], None, None);
        // Holding the media/clock locks also proves the getter does not use them.
        let _clock = port.remote_clock_fresh_until.lock().unwrap();
        assert_eq!(audio.render_expired_samples(), Some(0));
        for counter in [95, 255, u64::MAX] {
            let mut stats = port.stats.lock().unwrap();
            stats.expired_render_samples = counter;
            let before = serde_json::to_value(&*stats).unwrap();
            // Same-thread ownership would deadlock a blocking lock(), not try_lock().
            assert_eq!(audio.render_expired_samples(), None);
            port.closed.store(true, Ordering::Relaxed);
            assert_eq!(audio.render_expired_samples(), None);
            drop(stats);
            assert_eq!(audio.render_expired_samples(), None);
            port.closed.store(false, Ordering::Relaxed);
            assert_eq!(audio.render_expired_samples(), Some(counter));
            assert_eq!(audio.render_expired_samples(), Some(counter));
            assert_eq!(
                serde_json::to_value(&*port.stats.lock().unwrap()).unwrap(),
                before
            );
        }
        assert_eq!(port.position.load(Ordering::Relaxed), 17);
        assert_eq!(port.sink_queue.load(Ordering::Relaxed), 181);
        assert_eq!(port.sink_rate.load(Ordering::Relaxed), 500_000);
        assert_eq!(render.pcm.iter().copied().collect::<Vec<_>>(), vec![7; 640]);
        assert_eq!(render.start_due, Some(due));
        assert_eq!(render.end_due, Some(due + Duration::from_millis(40)));
        assert_eq!(render.frame_samples, 640);
        assert!(render.first_pull.is_none() && render.last_pull.is_none());
        assert_eq!(render.successful_pull_calls, 0);
    }

    #[test]
    fn render_expiry_trace_records_partial_pulls_and_only_relative_frame_times() {
        let profile = LiveProfile::Ms40;
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = Arc::new(test_port());
        let audio = AudioPort { port: port.clone() };
        assert!(port.snapshot().last_render_expiry.is_none());
        assert!(serde_json::to_value(port.snapshot()).unwrap()["last_render_expiry"].is_null());
        let mut received = test_received();
        let mut feedback = packet::Feedback::default();
        received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
        let mut work = received
            .prepare_decode(at(60), profile, &mut feedback, &port)
            .unwrap();
        let output = LiveDecoder::new(profile)
            .unwrap()
            .decode(work.opus.take().as_ref().unwrap())
            .unwrap();
        received
            .apply_decode(
                work,
                at(73),
                Duration::from_micros(12_151),
                Zeroizing::new(output),
                &port,
            )
            .unwrap();
        let mut pcm = [77; 160];
        assert_eq!(audio.pull_at(at(79), &mut pcm), 0);
        assert_eq!(audio.pull_at(at(80), &mut pcm), 160);
        assert_eq!(audio.pull_at(at(89), &mut pcm), 0);
        assert_eq!(audio.pull_at(at(90), &mut pcm), 160);
        assert!(port.snapshot().last_render_expiry.is_none());
        audio.sink_queued(181);
        assert!(audio.sink_rate(500_000));
        pcm.fill(77);
        assert_eq!(audio.pull_at(at(123), &mut pcm), 0);
        assert_eq!(pcm, [77; 160]);
        let snapshot = port.snapshot();
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap()["last_render_expiry"],
            serde_json::json!({
                "kind": "queued",
                "publication_vs_start_us": -7_000,
                "codec_duration_us": 12_151,
                "first_pull_vs_start_us": 0,
                "last_pull_vs_start_us": 10_000,
                "successful_pull_calls": 2,
                "expiry_vs_end_us": 3_000,
                "initial_samples": 640,
                "transferred_samples": 320,
                "discarded_samples": 320,
                "source_frame_duration_us": 40_000,
                "sink_queue_samples": 181,
                "sink_rate_ppb": 500_000,
            })
        );
        assert_eq!(
            (snapshot.dropped_render, snapshot.expired_render_samples),
            (320, 320)
        );
        assert_eq!(snapshot.decoded_packets, 1);
        assert_eq!(received.first_playout, Some((0, at(80))));
        assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
        assert_eq!(audio.pull_at(at(130), &mut pcm), 0);
        let unchanged = port.snapshot();
        assert_eq!(unchanged.last_render_expiry, snapshot.last_render_expiry);
        assert_eq!(unchanged.expired_render_samples, 320);
    }

    #[test]
    fn render_expiry_trace_replaces_one_record_without_counting_valid_or_cleared_pcm() {
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let port = Arc::new(test_port());
        let audio = AudioPort { port: port.clone() };
        port.render.lock().unwrap().enqueue_observed(
            at(80),
            vec![7; 640],
            Some(at(73)),
            Some(Duration::from_millis(12)),
        );
        let mut pcm = [0; 160];
        assert_eq!(audio.pull_at(at(121), &mut pcm), 0);
        let frozen = port.snapshot();
        let first = frozen.last_render_expiry.as_ref().unwrap();
        assert_eq!(first.discarded_samples, 640);
        assert_eq!(first.expiry_vs_end_us, 1_000);
        assert_eq!(first.source_frame_duration_us, 40_000);

        port.render.lock().unwrap().enqueue_observed(
            at(160),
            vec![8; 640],
            Some(at(153)),
            Some(Duration::from_millis(12)),
        );
        for ms in [160, 170, 180, 190] {
            assert_eq!(audio.pull_at(at(ms), &mut pcm), 160);
        }
        assert_eq!(audio.pull_at(at(200), &mut pcm), 0);
        assert_eq!(
            port.snapshot().last_render_expiry,
            frozen.last_render_expiry
        );
        assert_eq!(port.snapshot().expired_render_samples, 640);

        port.render.lock().unwrap().enqueue_observed(
            at(240),
            vec![9; 640],
            Some(at(233)),
            Some(Duration::from_millis(12)),
        );
        assert_eq!(audio.pull_at(at(240), &mut pcm), 160);
        port.clear(); // Retirement clearing is not a newly observed expiry.
        assert_eq!(audio.pull_at(at(280), &mut pcm), 0);
        assert_eq!(
            port.snapshot().last_render_expiry,
            frozen.last_render_expiry
        );
        assert_eq!(port.snapshot().expired_render_samples, 640);

        let rate = Rate {
            ticks: 48_024,
            ns: 1_000_000_000,
        };
        let end = at(320) + rate.duration(1920);
        {
            let mut render = port.render.lock().unwrap();
            render.enqueue_observed(
                at(320),
                vec![10; 640],
                Some(at(313)),
                Some(Duration::from_millis(13)),
            );
            render.end_due = Some(end);
            render.rate = rate;
        }
        assert_eq!(audio.pull_at(end + Duration::from_millis(3), &mut pcm), 0);
        let snapshot = port.snapshot();
        let replacement = snapshot.last_render_expiry.as_ref().unwrap();
        assert_eq!(replacement.publication_vs_start_us, Some(-7_000));
        assert_eq!(replacement.codec_duration_us, Some(13_000));
        assert_eq!(replacement.source_frame_duration_us, 39_980);
        assert_eq!(replacement.expiry_vs_end_us, 3_000);
        assert_eq!(replacement.first_pull_vs_start_us, None);
        assert_eq!(replacement.last_pull_vs_start_us, None);
        assert_eq!(replacement.successful_pull_calls, 0);
        assert_ne!(snapshot.last_render_expiry, frozen.last_render_expiry);
        assert_eq!(
            frozen.last_render_expiry.as_ref().unwrap().expiry_vs_end_us,
            1_000
        );
        assert_eq!(
            (snapshot.dropped_render, snapshot.expired_render_samples),
            (1280, 1280)
        );
    }

    #[test]
    fn render_expiry_trace_distinguishes_completed_output_and_unknown_fixture_times() {
        let profile = LiveProfile::Ms40;
        for publish_ms in [119, 125] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let port = Arc::new(test_port());
            let audio = AudioPort { port: port.clone() };
            let mut received = test_received();
            let mut feedback = packet::Feedback::default();
            received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
            let mut work = received
                .prepare_decode(at(60), profile, &mut feedback, &port)
                .unwrap();
            let output = LiveDecoder::new(profile)
                .unwrap()
                .decode(work.opus.take().as_ref().unwrap())
                .unwrap();
            received
                .apply_decode(
                    work,
                    at(publish_ms),
                    Duration::from_millis(12),
                    Zeroizing::new(output),
                    &port,
                )
                .unwrap();
            if publish_ms == 119 {
                assert!(port.snapshot().last_render_expiry.is_none());
                assert_eq!(port.render.lock().unwrap().pcm.len(), 640);
                received.maintain_playout(at(120), profile, &mut feedback, &port);
            }
            let snapshot = port.snapshot();
            let trace = snapshot.last_render_expiry.as_ref().unwrap();
            assert_eq!(
                trace.kind,
                if publish_ms == 119 {
                    RenderExpiryKind::Queued
                } else {
                    RenderExpiryKind::Completion
                }
            );
            assert_eq!(
                trace.publication_vs_start_us,
                Some((publish_ms as i64 - 80) * 1000)
            );
            assert_eq!(
                trace.expiry_vs_end_us,
                if publish_ms == 119 { 0 } else { 5_000 }
            );
            assert_eq!(trace.codec_duration_us, Some(12_000));
            assert_eq!(trace.first_pull_vs_start_us, None);
            assert_eq!(trace.last_pull_vs_start_us, None);
            assert_eq!(trace.successful_pull_calls, 0);
            assert_eq!(
                (
                    trace.initial_samples,
                    trace.transferred_samples,
                    trace.discarded_samples
                ),
                (640, 0, 640)
            );
            assert_eq!(trace.source_frame_duration_us, 40_000);
            assert!(port.render.lock().unwrap().pcm.is_empty());
            assert_eq!(snapshot.decoded_packets, 1);
            assert_eq!(snapshot.expired_render_samples, 640);
            assert_eq!(snapshot.dropped_render, 640);
            assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);

            // Old synthetic enqueue fixtures have no claimed publication/C-call
            // observation. An expiry cannot turn those unknowns into a timestamp.
            port.render.lock().unwrap().enqueue(at(200), vec![7; 640]);
            let mut pcm = [77; 160];
            assert_eq!(audio.pull_at(at(240), &mut pcm), 0);
            assert_eq!(pcm, [77; 160]);
            let unknown = port.snapshot();
            let trace = unknown.last_render_expiry.as_ref().unwrap();
            assert_eq!(trace.kind, RenderExpiryKind::Queued);
            assert_eq!(trace.publication_vs_start_us, None);
            assert_eq!(trace.codec_duration_us, None);
            assert_eq!(trace.first_pull_vs_start_us, None);
            assert_eq!(trace.last_pull_vs_start_us, None);
            assert_eq!(unknown.expired_render_samples, 1280);
            assert_eq!(snapshot.expired_render_samples, 640);
        }
    }

    #[tokio::test]
    async fn held_decode_collects_physical_capture_before_native_wait_expires_it() {
        let profile = LiveProfile::Ms40;
        let mut results = Vec::new();
        for held_ms in [15u64, 25, 35] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let (audio, mut input) = test_audio();
            calibrate_audio(&audio, &[50; 20]);
            let port = audio.port.clone();
            let baseline = port.snapshot().dropped_capture;
            let mut capture = CaptureFrame::new(640, port.clone());
            let mut received = test_received();
            let mut feedback = packet::Feedback::default();
            received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
            let (entered, entry) = std::sync::mpsc::sync_channel(1);
            let hold = codec::Hold {
                kind: codec::Kind::Decode,
                entered,
                release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
                admitted: Arc::new(AtomicU64::new(0)),
                controlled: Arc::new(AtomicU64::new(0)),
            };
            let mut owner = codec::Owner::start(profile, Some(hold.clone()))
                .await
                .unwrap();
            let mut flight = None;
            let mut admission = MediaAdmission::default();
            let (outgoing, _requests) = mpsc::channel(1);
            let mut reset = false;
            dispatch_codec(
                at(80),
                profile,
                &mut received,
                &mut capture,
                &admission,
                &mut feedback,
                &mut reset,
                &mut owner,
                &mut flight,
                &outgoing,
                0,
                &port,
            )
            .unwrap();
            entry.recv_timeout(Duration::from_secs(3)).unwrap();
            // Logical service time is controlled; the actual owner supplies the
            // codec output. No real sleep or hardware/codec timing claim.
            let mut waiting_ms = 0;
            let mut collected_while_held = 0;
            for ms in (80..=115u64).step_by(5) {
                if ms == 80 + held_ms {
                    hold.release();
                    let mut completed = owner.completed().await.unwrap();
                    completed.finished = at(ms);
                    apply_codec_completion(
                        completed,
                        &mut flight,
                        &mut received,
                        &mut admission,
                        &mut feedback,
                        0,
                        at(ms),
                        profile,
                        &port,
                    )
                    .unwrap();
                }
                if ms <= 110 && ms % 10 == 0 {
                    let position = 3200 + (ms - 80) * 16;
                    assert!(audio.push_recorded(
                        &[1; 160],
                        aged_record_timestamp(position, 85),
                        at(ms)
                    ));
                }
                while capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0) {
                    let Ok(batch) = input.try_recv() else {
                        break;
                    };
                    waiting_ms = waiting_ms.max(at(ms).duration_since(batch.pushed_at).as_millis());
                    if flight.is_some() {
                        collected_while_held += batch.len;
                    }
                    capture.ingest(batch, true, at(ms), profile);
                }
                assert!(input.len() <= 4 && capture.pcm.len() <= 640);
                assert!(input.len() * 160 + capture.pcm.len() <= 1280);
                assert_eq!(owner.busy(), flight.is_some());
                dispatch_codec(
                    at(ms),
                    profile,
                    &mut received,
                    &mut capture,
                    &admission,
                    &mut feedback,
                    &mut reset,
                    &mut owner,
                    &mut flight,
                    &outgoing,
                    0,
                    &port,
                )
                .unwrap();
                if matches!(
                    flight.as_ref().and_then(|flight| flight.source.as_ref()),
                    Some(CodecSource::Encode(_))
                ) {
                    let mut completed = owner.completed().await.unwrap();
                    completed.finished = at(ms + 2);
                    apply_codec_completion(
                        completed,
                        &mut flight,
                        &mut received,
                        &mut admission,
                        &mut feedback,
                        0,
                        at(ms + 2),
                        profile,
                        &port,
                    )
                    .unwrap();
                    let waiting = &admission.pending.as_ref().unwrap().0;
                    assert_eq!(waiting.position, 3200);
                    assert_eq!(waiting.captured_at, at(45));
                    assert_eq!(waiting.deadline, at(125)); // Original 45+40+40.
                }
            }
            hold.release();
            let stats = port.snapshot();
            results.push((
                held_ms,
                stats.dropped_capture - baseline,
                stats.capture_age_rejected_batches,
                waiting_ms,
                stats.max_additional_capture_age_us,
                stats.encoded_packets,
                collected_while_held,
            ));
            admission.discard(profile, &port);
            drop(flight);
            owner.close().unwrap();
        }
        assert_eq!(
            results,
            vec![
                (15, 0, 0, 0, 35_000, 1, 320),
                (25, 0, 0, 0, 35_000, 1, 480),
                (35, 0, 0, 0, 35_000, 1, 640),
            ]
        );
    }

    #[tokio::test]
    async fn paced_codec_dispatch_serves_ready_decode_without_waiting_for_another_tick() {
        let profile = LiveProfile::Ms40;
        for decode_us in [8_000u64, 12_151] {
            for capture_phase_ns in [29_500_000u64, 14_500_000, 36_500_000, 0] {
                let origin = Instant::now();
                let at = |ns| origin + Duration::from_nanos(ns);
                let mut port = test_port();
                let (input, mut incoming) = mpsc::channel(4);
                port.input = input;
                let port = Arc::new(port);
                let audio = AudioPort { port: port.clone() };
                let mut capture = CaptureFrame::new(profile.samples(), port.clone());
                let mut received = test_received();
                let mut feedback = packet::Feedback::default();
                let mut reset = false;
                let mut owner = codec::Owner::start(profile, None).await.unwrap();
                let mut flight: Option<CodecFlight> = None;
                let mut admission = MediaAdmission::default();
                let mut budget = packet::Budget::new(50_000, 144, origin);
                let (outgoing, mut requests) = mpsc::channel(1);
                let opus = opus40();
                let (mut source, mut batch, mut queued, mut consumed, mut written) =
                    (0u64, 0u64, 0usize, 0u64, 0u64);
                let mut finish = None;
                let mut first_expiry = None;
                let mut operations = Vec::new();
                // Independent 10ms source/read and render clocks; the latter
                // deliberately runs just after the actor's 10ms timer. Operation
                // durations are controlled service times, not host CPU claims.
                for ns in (0..=1_080_000_000u64).step_by(50_000) {
                    let now = at(ns);
                    let mut wake = false;
                    if ns == source * 40_000_000 {
                        received.admit(source * 1920, &opus, now, profile, &mut feedback, &port);
                        source += 1;
                        wake = true;
                    }
                    if ns == capture_phase_ns + batch * 10_000_000 {
                        assert!(audio.push_at(&[1; 160], batch * 160, now, now, None, None));
                        batch += 1;
                        wake = true;
                    }
                    if finish.is_some_and(|end| ns >= end) {
                        let end = finish.take().unwrap();
                        let mut completed = owner.completed().await.unwrap();
                        completed.finished = at(end);
                        completed.duration = Duration::from_micros(
                            match flight.as_ref().unwrap().source.as_ref().unwrap() {
                                CodecSource::Encode(_) => 2_000,
                                CodecSource::Decode(_) => decode_us,
                            },
                        );
                        apply_codec_completion(
                            completed,
                            &mut flight,
                            &mut received,
                            &mut admission,
                            &mut feedback,
                            0,
                            now,
                            profile,
                            &port,
                        )
                        .unwrap();
                        wake = true;
                    }
                    if admission
                        .wake_at(&mut budget, now, profile)
                        .is_some_and(|due| now >= due)
                    {
                        if let Some((waiting, permit)) =
                            admission.take_ready(&mut budget, now, profile, &port)
                        {
                            // Consume the same capacity-one reservation and the
                            // unchanged integer byte credit; no service backlog.
                            permit.send(SendRequest {
                                opcode: OP_RTP,
                                payload: waiting.opus.to_vec(),
                                deadline: Some(waiting.deadline),
                                committed: None,
                            });
                            requests.try_recv().unwrap();
                            admission.committed(now, profile.samples());
                        }
                        wake = true;
                    }
                    let timer = ns % 10_000_000 == 0;
                    if wake || timer {
                        let decode_ready = flight.is_none() && received.decode_ready(now, &port);
                        dispatch_codec(
                            now,
                            profile,
                            &mut received,
                            &mut capture,
                            &admission,
                            &mut feedback,
                            &mut reset,
                            &mut owner,
                            &mut flight,
                            &outgoing,
                            0,
                            &port,
                        )
                        .unwrap();
                        assert!(!decode_ready || matches!(flight.as_ref().and_then(|flight| flight.source.as_ref()),
                            Some(CodecSource::Decode(_))),
                            "ready decode left idle or bypassed: ns={ns} timer={timer} decode_us={decode_us} capture_phase_ns={capture_phase_ns} operations={operations:?}");
                        while capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0)
                        {
                            let Ok(chunk) = incoming.try_recv() else {
                                break;
                            };
                            capture.ingest(chunk, true, now, profile);
                            dispatch_codec(
                                now,
                                profile,
                                &mut received,
                                &mut capture,
                                &admission,
                                &mut feedback,
                                &mut reset,
                                &mut owner,
                                &mut flight,
                                &outgoing,
                                0,
                                &port,
                            )
                            .unwrap();
                        }
                    }
                    if flight.is_some() && finish.is_none() {
                        let (kind, us, timestamp) =
                            match flight.as_ref().unwrap().source.as_ref().unwrap() {
                                CodecSource::Encode(source) => {
                                    ("encode", 2_000, source.position * 3)
                                }
                                CodecSource::Decode(work) => ("decode", decode_us, work.timestamp),
                            };
                        finish = Some(ns + us * 1_000);
                        operations.push((kind, ns, timestamp));
                    }
                    if ns >= 100_000 && (ns - 100_000) % 10_000_000 == 0 {
                        let drained = queued.min(160);
                        queued -= drained;
                        consumed += drained as u64;
                        let mut render = port.render.lock().unwrap();
                        let mut pcm = [0; 160];
                        loop {
                            port.sink_queue.store(queued as u64, Ordering::Relaxed);
                            if queued + 160 > 640 {
                                break;
                            }
                            let (count, expired) =
                                render.pull_with_lead(now, clock::sink_lead(queued, 0), &mut pcm);
                            port.expired_render(expired, None);
                            if expired != 0 {
                                first_expiry.get_or_insert((ns, expired));
                            }
                            if count == 0 {
                                break;
                            }
                            queued += count;
                            written += count as u64;
                        }
                    }
                    assert!(incoming.len() <= 4 && capture.pcm.len() <= 640 && queued <= 640);
                    assert!(
                        received.encoded.len() <= 6 && port.render.lock().unwrap().pcm.len() <= 640
                    );
                    assert_eq!(owner.busy(), flight.is_some());
                }
                let stats = port.snapshot();
                assert_eq!(
                    (
                        stats.late_packets,
                        stats.plc_slots,
                        stats.future_rejected_packets
                    ),
                    (0, 0, 0)
                );
                assert_eq!(stats.dropped_capture, 0);
                assert_eq!(stats.expired_render_samples, 0,
                    "decode_us={decode_us} capture_phase_ns={capture_phase_ns} first_expiry={first_expiry:?} operations={operations:?}");
                assert_eq!(stats.dropped_render, 0);
                assert!(stats.decoded_packets > 20 && stats.encoded_packets > 20);
                assert_eq!(written, consumed + queued as u64);
                assert_eq!(
                    stats.decoded_packets * 640,
                    written + port.render.lock().unwrap().pcm.len() as u64
                );
                // The measured finite interval precedes intentional retirement.
                drop(flight);
                admission.discard(profile, &port);
                owner.close().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn held_encode_retains_source_ownership_and_never_replays_aged_input() {
        let profile = LiveProfile::Ms40;
        for release_ms in [125u64, 145] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let (audio, mut input) = test_audio();
            calibrate_audio(&audio, &[50; 20]);
            let port = audio.port.clone();
            let baseline = port.snapshot().dropped_capture;
            let mut capture = CaptureFrame::new(640, port.clone());
            let mut admission = MediaAdmission::default();
            let (outgoing, mut requests) = mpsc::channel(1);
            for batch in 0..4u64 {
                let now = at(50 + batch * 10);
                assert!(audio.push_recorded(
                    &[1; 160],
                    aged_record_timestamp(3200 + batch * 160, 50),
                    now
                ));
                assert!(capture.can_ingest(profile, None, &admission, &outgoing, 0));
                capture.ingest(input.try_recv().unwrap(), true, now, profile);
            }
            let (entered, entry) = std::sync::mpsc::sync_channel(1);
            let hold = codec::Hold {
                kind: codec::Kind::Encode,
                entered,
                release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
                admitted: Arc::new(AtomicU64::new(0)),
                controlled: Arc::new(AtomicU64::new(0)),
            };
            let mut owner = codec::Owner::start(profile, Some(hold.clone()))
                .await
                .unwrap();
            let mut flight = None;
            let mut received = test_received();
            let mut feedback = packet::Feedback::default();
            let mut reset = false;
            dispatch_codec(
                at(80),
                profile,
                &mut received,
                &mut capture,
                &admission,
                &mut feedback,
                &mut reset,
                &mut owner,
                &mut flight,
                &outgoing,
                0,
                &port,
            )
            .unwrap();
            entry.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut kept_source = capture.pcm.is_empty() && outgoing.capacity() == 0;
            for batch in 0..4u64 {
                assert!(audio.push_recorded(
                    &[2; 160],
                    aged_record_timestamp(3840 + batch * 160, 85),
                    at(90 + batch * 10)
                ));
                kept_source &=
                    !capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0);
            }
            let kept_ring = input.len() == 4 && port.input.capacity() == 0;
            hold.release();
            assert!(kept_source && kept_ring && owner.busy());
            // A ready result still owns the same source slot until consumed.
            assert!(!capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0));
            let mut completed = owner.completed().await.unwrap();
            completed.finished = at(release_ms);
            apply_codec_completion(
                completed,
                &mut flight,
                &mut received,
                &mut admission,
                &mut feedback,
                0,
                at(release_ms),
                profile,
                &port,
            )
            .unwrap();
            if release_ms == 125 {
                assert!(!capture.can_ingest(profile, None, &admission, &outgoing, 0));
                assert_eq!(admission.pending.as_ref().unwrap().0.deadline, at(130));
                let mut budget = packet::Budget::new(50_000, 144, origin);
                let (waiting, permit) = admission
                    .take_ready(&mut budget, at(125), profile, &port)
                    .unwrap();
                permit.send(SendRequest {
                    opcode: OP_RTP,
                    payload: waiting.opus.to_vec(),
                    deadline: Some(waiting.deadline),
                    committed: None,
                });
                requests.try_recv().unwrap();
            } else {
                assert!(admission.pending.is_none());
                assert_eq!(port.snapshot().dropped_capture - baseline, 640);
            }
            let now = at(release_ms + 1);
            while capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0) {
                let Ok(batch) = input.try_recv() else {
                    break;
                };
                capture.ingest(batch, true, now, profile);
            }
            assert!(capture.pcm.is_empty() && input.is_empty());
            assert_eq!(port.snapshot().capture_age_rejected_batches, 4);
            assert_eq!(
                port.snapshot().dropped_capture - baseline,
                if release_ms == 125 { 640 } else { 1280 }
            );
            assert_eq!(port.position.load(Ordering::Relaxed), 4480);
            dispatch_codec(
                now,
                profile,
                &mut received,
                &mut capture,
                &admission,
                &mut feedback,
                &mut reset,
                &mut owner,
                &mut flight,
                &outgoing,
                0,
                &port,
            )
            .unwrap();
            assert!(flight.is_none() && !owner.busy()); // No old input replay.
            assert_eq!(port.snapshot().encoded_packets, 1);
            owner.close().unwrap();
        }
    }

    #[tokio::test]
    async fn held_decode_source_expiry_and_cancellation_keep_original_slot_accounting() {
        let profile = LiveProfile::Ms40;
        for expire in [false, true] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let (audio, mut input) = test_audio();
            calibrate_audio(&audio, &[50; 20]);
            let port = audio.port.clone();
            let baseline = port.snapshot().dropped_capture;
            let mut capture = CaptureFrame::new(640, port.clone());
            let mut received = test_received();
            let mut feedback = packet::Feedback::default();
            received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
            let (entered, entry) = std::sync::mpsc::sync_channel(1);
            let hold = codec::Hold {
                kind: codec::Kind::Decode,
                entered,
                release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
                admitted: Arc::new(AtomicU64::new(0)),
                controlled: Arc::new(AtomicU64::new(0)),
            };
            let mut owner = codec::Owner::start(profile, Some(hold.clone()))
                .await
                .unwrap();
            let mut flight = None;
            let admission = MediaAdmission::default();
            let (outgoing, _requests) = mpsc::channel(1);
            let mut reset = false;
            dispatch_codec(
                at(80),
                profile,
                &mut received,
                &mut capture,
                &admission,
                &mut feedback,
                &mut reset,
                &mut owner,
                &mut flight,
                &outgoing,
                0,
                &port,
            )
            .unwrap();
            entry.recv_timeout(Duration::from_secs(3)).unwrap();
            for batch in 0..4u64 {
                let now = at(80 + batch * 10);
                assert!(audio.push_recorded(
                    &[1; 160],
                    aged_record_timestamp(3200 + batch * 160, 85),
                    now
                ));
                if capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0) {
                    capture.ingest(input.try_recv().unwrap(), true, now, profile);
                }
            }
            let full_source = capture.full(profile) && input.is_empty();
            assert!(audio.push_recorded(&[2; 160], aged_record_timestamp(3840, 85), at(120)));
            let stops_when_full =
                !capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0);
            if expire {
                dispatch_codec(
                    at(125),
                    profile,
                    &mut received,
                    &mut capture,
                    &admission,
                    &mut feedback,
                    &mut reset,
                    &mut owner,
                    &mut flight,
                    &outgoing,
                    0,
                    &port,
                )
                .unwrap();
            }
            hold.release(); // Never leave the owned C operation held after a failing assertion.
            assert!(full_source && stops_when_full && owner.busy());
            if expire {
                assert!(capture.pcm.is_empty());
                assert_eq!(port.snapshot().dropped_capture - baseline, 640);
                assert!(capture.can_ingest(profile, flight.as_ref(), &admission, &outgoing, 0));
                // A different current batch at its exact 40ms bound can occupy
                // the released slot. The expired 3200..3840 frame is not replayed.
                capture.ingest(input.try_recv().unwrap(), true, at(125), profile);
                assert_eq!(capture.position, 3840);
                assert_eq!(capture.pcm.len(), 160);
                assert_eq!(capture.captured_at, at(85));
            }
            drop(flight);
            drop(capture);
            owner.close().unwrap();
            assert_eq!(
                port.snapshot().dropped_capture - baseline,
                if expire { 800 } else { 640 }
            );
            assert_eq!(port.snapshot().dropped_render, 640);
            assert_eq!(port.snapshot().encoded_packets, 0);
            assert_eq!(port.snapshot().capture_age_rejected_batches, 0);
        }
    }

    #[test]
    fn decode_source_collection_still_obeys_all_admission_and_readiness_guards() {
        let profile = LiveProfile::Ms40;
        let (audio, mut input) = test_audio();
        calibrate_audio(&audio, &[50; 20]);
        let port = audio.port.clone();
        let baseline = port.snapshot().dropped_capture;
        let mut capture = CaptureFrame::new(640, port.clone());
        let mut admission = MediaAdmission::default();
        let (outgoing, _requests) = mpsc::channel(1);
        let mut flight = CodecFlight {
            source: Some(CodecSource::Decode(DecodeWork {
                timestamp: 0,
                end: 1920,
                plc: true,
                opus: None,
            })),
            port: port.clone(),
            samples: 640,
        };
        assert!(capture.can_ingest(profile, Some(&flight), &admission, &outgoing, 1));
        assert!(!capture.can_ingest(profile, Some(&flight), &admission, &outgoing, 2));
        let source = flight.source.take();
        assert!(!capture.can_ingest(profile, Some(&flight), &admission, &outgoing, 0));
        flight.source = source;
        let permit = outgoing.clone().try_reserve_owned().unwrap();
        assert!(!capture.can_ingest(profile, Some(&flight), &admission, &outgoing, 0));
        let now = Instant::now();
        admission.pending = Some((
            WaitingPacket::new(vec![1], 0, now, now, now, profile),
            permit,
        ));
        assert!(!capture.can_ingest(profile, Some(&flight), &admission, &outgoing, 0));
        drop(admission.pending.take());
        assert!(capture.can_ingest(profile, Some(&flight), &admission, &outgoing, 0));
        assert!(audio.push_recorded(&[1; 160], aged_record_timestamp(3200, 85), now));
        capture.ingest(input.try_recv().unwrap(), false, now, profile);
        assert!(capture.pcm.is_empty());
        assert_eq!(port.snapshot().dropped_capture - baseline, 160);
        // Starting readiness later cannot recover the discarded earlier prefix.
        assert!(audio.push_recorded(
            &[2; 160],
            aged_record_timestamp(3360, 85),
            now + Duration::from_millis(10)
        ));
        capture.ingest(
            input.try_recv().unwrap(),
            true,
            now + Duration::from_millis(10),
            profile,
        );
        assert!(capture.pcm.is_empty());
        assert_eq!(capture.expected_position, 3520);
        assert_eq!(port.snapshot().dropped_capture - baseline, 320);
        flight.source.take(); // No real operation was dispatched in this guard test.
    }

    #[tokio::test]
    async fn ready_decode_holds_only_the_existing_full_source_frame_and_original_deadline() {
        let profile = LiveProfile::Ms40;
        for expire in [false, true] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let mut port = test_port();
            let (input, incoming) = mpsc::channel(4);
            port.input = input.clone();
            let port = Arc::new(port);
            let mut capture = CaptureFrame::new(640, port.clone());
            for batch in 0..4u64 {
                capture.ingest(
                    Captured {
                        pcm: [1; 160],
                        len: 160,
                        position: batch * 160,
                        at: at(50 + batch * 10),
                        pushed_at: at(50 + batch * 10),
                        hardware_age: None,
                        capture_clock: None,
                    },
                    true,
                    at(50 + batch * 10),
                    profile,
                );
            }
            let mut received = test_received();
            let mut feedback = packet::Feedback::default();
            received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
            let (entered, entry) = std::sync::mpsc::sync_channel(1);
            let hold = codec::Hold {
                kind: codec::Kind::Decode,
                entered,
                release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
                admitted: Arc::new(AtomicU64::new(0)),
                controlled: Arc::new(AtomicU64::new(0)),
            };
            let mut owner = codec::Owner::start(profile, Some(hold.clone()))
                .await
                .unwrap();
            let mut flight = None;
            let mut admission = MediaAdmission::default();
            let (outgoing, _requests) = mpsc::channel(1);
            let mut reset = false;
            dispatch_codec(
                at(80),
                profile,
                &mut received,
                &mut capture,
                &admission,
                &mut feedback,
                &mut reset,
                &mut owner,
                &mut flight,
                &outgoing,
                0,
                &port,
            )
            .unwrap();
            entry.recv_timeout(Duration::from_secs(3)).unwrap();
            let decode_selected = matches!(
                flight.as_ref().and_then(|flight| flight.source.as_ref()),
                Some(CodecSource::Decode(_))
            );
            let held_frame = capture.pcm.len() == 640 && outgoing.capacity() == 1;
            for batch in 0..4u64 {
                input
                    .try_send(Captured {
                        pcm: [2; 160],
                        len: 160,
                        position: 640 + batch * 160,
                        at: at(90 + batch * 10),
                        pushed_at: at(90 + batch * 10),
                        hardware_age: None,
                        capture_clock: None,
                    })
                    .unwrap();
            }
            let checked = if expire { 130 } else { 85 };
            let result = dispatch_codec(
                at(checked),
                profile,
                &mut received,
                &mut capture,
                &admission,
                &mut feedback,
                &mut reset,
                &mut owner,
                &mut flight,
                &outgoing,
                0,
                &port,
            );
            let kept_ring = incoming.len() == 4 && input.capacity() == 0;
            hold.release(); // Release even when an ordering assertion fails.
            result.unwrap();
            assert!(decode_selected && held_frame && kept_ring && owner.busy());
            assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
            assert_eq!(feedback.plc, 0);
            if expire {
                // Source age is still 50+40+40=130, not release/encode time.
                assert!(capture.pcm.is_empty());
                assert_eq!(port.snapshot().dropped_capture, 640);
                drop(flight);
                owner.close().unwrap();
                drop(capture);
                assert_eq!(port.snapshot().dropped_capture, 640);
            } else {
                let completed = owner.completed().await.unwrap();
                apply_codec_completion(
                    completed,
                    &mut flight,
                    &mut received,
                    &mut admission,
                    &mut feedback,
                    0,
                    at(85),
                    profile,
                    &port,
                )
                .unwrap();
                dispatch_codec(
                    at(85),
                    profile,
                    &mut received,
                    &mut capture,
                    &admission,
                    &mut feedback,
                    &mut reset,
                    &mut owner,
                    &mut flight,
                    &outgoing,
                    0,
                    &port,
                )
                .unwrap();
                assert!(capture.pcm.is_empty() && owner.busy());
                assert_eq!(outgoing.capacity(), 0); // The existing encode permit.
                let mut completed = owner.completed().await.unwrap();
                completed.finished = at(87);
                apply_codec_completion(
                    completed,
                    &mut flight,
                    &mut received,
                    &mut admission,
                    &mut feedback,
                    0,
                    at(87),
                    profile,
                    &port,
                )
                .unwrap();
                let waiting = &admission.pending.as_ref().unwrap().0;
                assert_eq!(waiting.captured_at, at(50));
                assert_eq!(waiting.encoded_at, at(87));
                assert_eq!(waiting.deadline, at(127));
                assert_eq!(port.snapshot().dropped_capture, 0);
                admission.discard(profile, &port);
                owner.close().unwrap();
            }
        }
    }

    #[test]
    fn codec_finish_time_does_not_extend_the_actual_pcm_publication_deadline() {
        let profile = LiveProfile::Ms40;
        for publish_ms in [119, 120] {
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let port = Arc::new(test_port());
            let mut received = test_received();
            let mut feedback = packet::Feedback::default();
            received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
            let mut work = received
                .prepare_decode(at(80), profile, &mut feedback, &port)
                .unwrap();
            let output = LiveDecoder::new(profile)
                .unwrap()
                .decode(work.opus.take().as_ref().unwrap())
                .unwrap();
            let mut flight = Some(CodecFlight {
                source: Some(CodecSource::Decode(work)),
                port: port.clone(),
                samples: 640,
            });
            let completed = codec::Completion {
                output: codec::Output::Decoded(Zeroizing::new(output)),
                duration: Duration::from_millis(12),
                finished: at(92),
            };
            apply_codec_completion(
                completed,
                &mut flight,
                &mut received,
                &mut MediaAdmission::default(),
                &mut feedback,
                0,
                at(publish_ms),
                profile,
                &port,
            )
            .unwrap();
            let mut render = port.render.lock().unwrap();
            let mut batch = [0; 160];
            if publish_ms == 119 {
                assert_eq!(render.end_due, Some(at(120)));
                assert_eq!(render.pull(at(119), &mut batch), (160, 0));
                assert_eq!(port.snapshot().expired_render_samples, 0);
                let expired = render.expire(at(120));
                port.expired_render(expired, None);
                assert_eq!(expired, 480);
            } else {
                // C finished on time, but the actor cannot publish stale PCM.
                assert_eq!(render.pull(at(120), &mut batch), (0, 0));
                assert_eq!(port.snapshot().expired_render_samples, 640);
            }
            assert!(flight.is_none());
            assert_eq!(received.playout.as_ref().unwrap().cursor, 1920);
            assert_eq!(port.snapshot().decoded_packets, 1);
        }
    }

    #[tokio::test]
    async fn ready_codec_completion_is_consumed_once_without_another_select_turn() {
        let mut owner = codec::Owner::start(LiveProfile::Ms40, None).await.unwrap();
        assert!(owner.try_completed().unwrap().is_none());
        owner
            .submit(codec::Operation::Encode(Zeroizing::new(vec![1; 640])))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let completed = loop {
            if let Some(completed) = owner.try_completed().unwrap() {
                break completed;
            }
            assert!(Instant::now() < deadline, "codec result did not arrive");
            tokio::task::yield_now().await;
        };
        assert!(matches!(completed.output, codec::Output::Encoded { .. }));
        assert!(!owner.busy());
        assert!(owner.try_completed().unwrap().is_none());
        owner.close().unwrap();
    }

    #[tokio::test]
    async fn codec_slot_is_exclusive_and_state_survives_encode_decode_and_plc() {
        let (entered, entry) = std::sync::mpsc::sync_channel(1);
        let hold = codec::Hold {
            kind: codec::Kind::Encode,
            entered,
            release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            admitted: Arc::new(AtomicU64::new(0)),
            controlled: Arc::new(AtomicU64::new(0)),
        };
        let profile = LiveProfile::Ms40;
        let mut owner = codec::Owner::start(profile, Some(hold.clone()))
            .await
            .unwrap();
        owner
            .submit(codec::Operation::Encode(Zeroizing::new(
                super::super::test_tone(0, 640),
            )))
            .unwrap();
        entry.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(owner.busy());
        assert!(owner
            .submit(codec::Operation::Decode {
                opus: None,
                reset: false
            })
            .is_err());
        hold.release();
        let result = owner.completed().await.unwrap();
        let codec::Output::Encoded { bytes, .. } = result.output else {
            panic!("encoded completion expected");
        };
        dmsg_opus_sys::live::validate_packet(profile, &bytes).unwrap();
        assert!(!owner.busy());
        owner
            .submit(codec::Operation::Decode {
                opus: Some(bytes),
                reset: false,
            })
            .unwrap();
        let result = owner.completed().await.unwrap();
        let codec::Output::Decoded(pcm) = result.output else {
            panic!("decoded completion expected");
        };
        assert_eq!(pcm.len(), 640);
        owner
            .submit(codec::Operation::Decode {
                opus: None,
                reset: false,
            })
            .unwrap();
        let result = owner.completed().await.unwrap();
        let codec::Output::Decoded(pcm) = result.output else {
            panic!("PLC completion expected");
        };
        assert_eq!(pcm.len(), 640);
        owner.close().unwrap();
        assert!(owner
            .submit(codec::Operation::Encode(Zeroizing::new(vec![0; 640])))
            .is_err());
    }

    #[test]
    fn cancelled_codec_owner_joins_held_work_and_discards_unobserved_result() {
        let (entered, entry) = std::sync::mpsc::sync_channel(1);
        let hold = codec::Hold {
            kind: codec::Kind::Encode,
            entered,
            release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            admitted: Arc::new(AtomicU64::new(0)),
            controlled: Arc::new(AtomicU64::new(0)),
        };
        let worker_hold = hold.clone();
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (returned, returning) = std::sync::mpsc::sync_channel(1);
        let endpoint = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let mut owner = runtime
                .block_on(codec::Owner::start(LiveProfile::Ms40, Some(worker_hold)))
                .unwrap();
            owner
                .submit(codec::Operation::Encode(Zeroizing::new(vec![0; 640])))
                .unwrap();
            cancelled.recv().unwrap();
            drop(owner); // The endpoint future's cancellation path; no receipt poll.
            returned.send(()).unwrap();
        });
        entry.recv_timeout(Duration::from_secs(3)).unwrap();
        cancel.send(()).unwrap();
        let returned_while_held = returning.recv_timeout(Duration::from_millis(20)).is_ok();
        hold.release();
        if !returned_while_held {
            returning.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        endpoint.join().unwrap();
        assert!(
            !returned_while_held,
            "replacement cannot overtake a live codec owner"
        );
    }

    #[tokio::test]
    async fn held_plc_retires_once_and_expired_completion_is_never_rendered() {
        let profile = LiveProfile::Ms40;
        let port = test_port();
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let mut received = test_received();
        let mut feedback = packet::Feedback::default();
        received.admit(0, &opus40(), origin, profile, &mut feedback, &port);
        let mut decoder = LiveDecoder::new(profile).unwrap();
        received
            .tick(at(80), profile, &mut decoder, &mut feedback, &port)
            .unwrap();
        port.clear();
        port.sink_queue.store(160, Ordering::Relaxed);
        let (entered, entry) = std::sync::mpsc::sync_channel(1);
        let hold = codec::Hold {
            kind: codec::Kind::Decode,
            entered,
            release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            admitted: Arc::new(AtomicU64::new(0)),
            controlled: Arc::new(AtomicU64::new(0)),
        };
        let mut owner = codec::Owner::start(profile, Some(hold.clone()))
            .await
            .unwrap();
        assert!(!received.maintain_playout(at(110), profile, &mut feedback, &port));
        let mut work = received
            .prepare_decode(at(110), profile, &mut feedback, &port)
            .unwrap();
        assert!(work.plc);
        owner
            .submit(codec::Operation::Decode {
                opus: work.opus.take(),
                reset: false,
            })
            .unwrap();
        entry.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(feedback.plc, 1);
        assert_eq!(feedback.playout, Some(3840));
        // A packet for the irrevocably concealed slot does not rewind it.
        received.admit(1920, &opus40(), at(115), profile, &mut feedback, &port);
        assert_eq!(port.snapshot().late_before_nominal_due_packets, 1);
        assert!(!received.maintain_playout(at(130), profile, &mut feedback, &port));
        assert!(received
            .prepare_decode(at(130), profile, &mut feedback, &port)
            .is_none());
        assert!(received.maintain_playout(at(200), profile, &mut feedback, &port));
        assert_eq!(feedback.playout, Some(5760));
        hold.release();
        let result = owner.completed().await.unwrap();
        let codec::Output::Decoded(output) = result.output else {
            panic!("PLC completion expected");
        };
        received
            .apply_decode(work, at(200), result.duration, output, &port)
            .unwrap();
        assert!(port.render.lock().unwrap().pcm.is_empty());
        assert_eq!(port.snapshot().expired_render_samples, 640);
        assert_eq!(port.snapshot().plc_slots, 1);
        assert_eq!(port.snapshot().skipped_playout_slots, 1);
        assert_eq!(received.playout.as_ref().unwrap().cursor, 5760);
        // Reset belongs to the same exclusive owner before its next current slot.
        let mut next = received
            .prepare_decode(at(200), profile, &mut feedback, &port)
            .unwrap();
        owner
            .submit(codec::Operation::Decode {
                opus: next.opus.take(),
                reset: true,
            })
            .unwrap();
        let _ = owner.completed().await.unwrap();
        assert_eq!(port.snapshot().plc_slots, 2);
        assert_eq!(received.playout.as_ref().unwrap().cursor, 7680);
        owner.close().unwrap();
    }

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

    #[test]
    fn timing_bins_are_bounded_and_keep_above_boundary_samples() {
        let mut bins = [0; 8];
        for duration in [
            Duration::ZERO,
            Duration::from_nanos(1_000_001),
            Duration::from_millis(81),
        ] {
            observe_duration(&mut bins, duration);
        }
        assert_eq!(bins, [1, 1, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn post_noise_deadline_crossing_is_counted_without_rebasing_admission() {
        let (local, peer, _, _) = fixture::pair(PublicConfig {
            relay_addr: "127.0.0.1:1".into(),
            domain: "timing.invalid".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        })
        .unwrap();
        let port = test_port();
        let origin = Instant::now();
        let due = origin - Duration::from_millis(10);
        let mut received = test_received();
        received.index = u64::from(local.peer_initial_sequence);
        received.timestamp = u64::from(local.peer_initial_timestamp);
        received.playout = Some(PlayoutClock {
            cursor: received.timestamp,
            due,
        });
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
        let mut feedback = packet::Feedback::default();
        received
            .receive_lane(
                InboundFrame {
                    frame: (OP_RTP, cipher.clone()),
                    body_complete: origin - Duration::from_millis(21),
                    noise_done: origin - Duration::from_millis(20),
                },
                &mut receiver,
                &local,
                &mut feedback,
                &port,
            )
            .unwrap();
        assert_eq!(received.playout.as_ref().unwrap().due, due);
        assert!(received.encoded.is_empty()); // Diagnostics cannot rescue late audio.
        let stats = port.stats.lock().unwrap();
        assert_eq!(stats.late_packets, 1);
        assert_eq!(stats.noise_after_nominal_due_packets, 0);
        assert_eq!(stats.noise_before_due_admitted_after_due_packets, 1);
        assert!(stats.max_rx_post_noise_wait_us >= 20_000);
        assert_eq!(stats.rx_post_noise_wait_bins.iter().sum::<u64>(), 1);
        drop(stats);
        // A replay still fails authentication before timing/receipt accounting.
        assert!(received
            .receive_lane(
                InboundFrame {
                    frame: (OP_RTP, cipher),
                    body_complete: origin,
                    noise_done: origin
                },
                &mut receiver,
                &local,
                &mut feedback,
                &port
            )
            .is_err());
        assert_eq!(
            port.stats
                .lock()
                .unwrap()
                .rx_post_noise_wait_bins
                .iter()
                .sum::<u64>(),
            1
        );
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
            source_ready: origin,
            receipt,
        }]);
        let mut ledger = packet::Ledger::new(50, 20, 600);
        let mut counters = SendCounters {
            timestamp: 0,
            packets: 0,
            octets: 0,
            since_report: false,
            last_control: None,
        };
        reap_commits(
            &mut commits,
            &mut admission,
            &mut ledger,
            &port,
            320,
            &mut counters,
            None,
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
        let (op, cipher) = received.0.unwrap().unwrap().frame;
        assert_eq!(op, OP_RTP);
        assert_eq!(a_receiver.unprotect_rtp(&cipher).unwrap(), rtp);
        for (frame, receiver, fixture) in [
            (received.1, &mut a_receiver, &a),
            (received.2, &mut b_receiver, &b),
        ] {
            let (op, cipher) = frame.unwrap().unwrap().frame;
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
        let stamp = Instant::now();
        tx.try_send(Ok(InboundFrame {
            frame: (OP_RTP, cipher),
            body_complete: stamp,
            noise_done: stamp,
        }))
        .unwrap_or_else(|_| panic!("timing fixture inbox closed"));
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
                first_timestamp + (slot + u64::from(slot != 4)) * 1920
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
            // Present packets can now be computed early; the empty-sink gate
            // and the original due still prevent an early first presentation.
            assert_eq!(
                port.render
                    .lock()
                    .unwrap()
                    .pull(due - Duration::from_nanos(1), &mut batch),
                (0, 0)
            );
            for queued in (0..640usize).step_by(160) {
                assert_eq!(
                    port.render.lock().unwrap().pull_with_lead(
                        due,
                        clock::sink_lead(queued, 0),
                        &mut batch
                    ),
                    (160, 0)
                );
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
                for queued in (0..640usize).step_by(160) {
                    assert_eq!(
                        port.render.lock().unwrap().pull_with_lead(
                            due(slot),
                            clock::sink_lead(queued, 0),
                            &mut batch
                        ),
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
                        let (count, expired) = render.pull_with_lead(
                            now,
                            clock::sink_lead(queued as usize, ppb),
                            &mut batch,
                        );
                        assert_eq!(
                            expired,
                            0,
                            "profile={} ppm={ppm} tick={tick}",
                            profile.duration_ms()
                        );
                        if count == 0 {
                            break;
                        }
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
        for queued in (0..640usize).step_by(160) {
            let (count, loss) = bounded_sink.pull_with_lead(
                due + Duration::from_millis(10),
                clock::sink_lead(queued, 0),
                &mut output,
            );
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

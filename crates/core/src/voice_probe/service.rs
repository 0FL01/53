//! Opt-in synthetic framed-application byte service, not measured DNS capacity.
//!
//! The two source roles have independent meters; media and feedback of one role
//! share a meter. Setup is excluded. No packets, ciphertext or keys live here.

use std::{
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::{net::tcp::OwnedReadHalf, sync::watch};

use super::packet::{FEEDBACK_FRAMED_MAX, MEDIA_OVERHEAD};

const BYTE_CREDIT: u128 = 8_000_000_000; // One byte in bit-nanoseconds.
const MAX_START_MS: u64 = 3_600_000;
const MAX_DURATION_MS: u64 = 5_000;

/// Nonsecret fixture configuration. Independent of the endpoint's 75% offered
/// admission envelope; neither setting is a measurement of DNS safe capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticServiceConfig {
    pub baseline_bps: u32,
    #[serde(default)]
    pub collapse: Option<SyntheticCollapse>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticCollapse {
    pub start_ms: u64,
    pub duration_ms: u64,
    pub bps: u32,
}

impl SyntheticServiceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.baseline_bps, 50_000 | 80_000) {
            return Err("synthetic service baseline must be 50000 or 80000 bps".into());
        }
        if let Some(collapse) = self.collapse {
            if collapse.start_ms > MAX_START_MS
                || !(1..=MAX_DURATION_MS).contains(&collapse.duration_ms)
                || collapse.bps > self.baseline_bps
                || collapse
                    .start_ms
                    .checked_add(collapse.duration_ms)
                    .is_none()
            {
                return Err("synthetic service collapse rejected".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyntheticRoleStats {
    /// Actual nonblocking reads: prefix + Noise tag + application header + SRTP/SRTCP.
    pub read_bytes: u64,
    pub authenticated_framed_bytes: u64,
    pub authenticated_frames: u64,
    pub live_partial_bytes: u64,
    pub retired_partial_bytes: u64,
    /// Sum over the two lanes, including active waits in snapshots. A wait starts
    /// only after socket readability and insufficient credit; reactor scheduling
    /// until the next read attempt is included, idle socket time is not.
    pub credit_wait_ns: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SyntheticServiceSnapshot {
    pub model: &'static str,
    pub config: SyntheticServiceConfig,
    pub profile_ms: u16,
    pub burst_bytes: usize,
    pub armed: bool,
    pub elapsed_ms: u64,
    pub current_bps: u32,
    pub roles: [SyntheticRoleStats; 2],
}

/// One immutable, single-generation model. Clone this handle for CLI observation;
/// only the relay arms it, after all four authenticated bindings are accepted.
#[derive(Clone)]
pub struct SyntheticServiceModel(Arc<Model>);

struct Model {
    config: SyntheticServiceConfig,
    profile_ms: u16,
    burst_bytes: usize,
    epoch: watch::Sender<Option<Instant>>,
    roles: [Mutex<RoleState>; 2],
}

struct RoleState {
    meter: ByteMeter,
    stats: SyntheticRoleStats,
    wait_since: [Option<Instant>; 2],
}

impl SyntheticServiceModel {
    pub fn new(config: SyntheticServiceConfig, profile_ms: u16) -> Result<Self, String> {
        config.validate()?;
        let opus_cap = match profile_ms {
            20 => 50,
            40 => 100,
            60 => 150,
            _ => return Err("synthetic service profile rejected".into()),
        };
        // RTP12 + SRTP10 + app4 + Noise16 + prefix2; largest feedback frame152.
        let burst_bytes = (opus_cap + MEDIA_OVERHEAD).max(FEEDBACK_FRAMED_MAX);
        let (epoch, _) = watch::channel(None);
        Ok(Self(Arc::new(Model {
            config,
            profile_ms,
            burst_bytes,
            epoch,
            roles: std::array::from_fn(|_| {
                Mutex::new(RoleState {
                    meter: ByteMeter::new(config, burst_bytes),
                    stats: SyntheticRoleStats::default(),
                    wait_since: [None; 2],
                })
            }),
        })))
    }

    pub fn snapshot(&self) -> Result<SyntheticServiceSnapshot, String> {
        let epoch = *self.0.epoch.borrow();
        let now = Instant::now();
        let mut roles = [SyntheticRoleStats::default(); 2];
        let mut current_bps = 0;
        for (index, mutex) in self.0.roles.iter().enumerate() {
            let role = mutex
                .lock()
                .map_err(|_| "synthetic service meter unavailable")?;
            roles[index] = role.stats;
            for since in role.wait_since.iter().flatten() {
                roles[index].credit_wait_ns = roles[index]
                    .credit_wait_ns
                    .saturating_add(nanos(now.saturating_duration_since(*since)));
            }
            if epoch.is_some() {
                current_bps = role.meter.rate(now);
            }
        }
        Ok(SyntheticServiceSnapshot {
            model: "synthetic-framed-application-service",
            config: self.0.config,
            profile_ms: self.0.profile_ms,
            burst_bytes: self.0.burst_bytes,
            armed: epoch.is_some(),
            elapsed_ms: epoch.map_or(0, |epoch| {
                u64::try_from(now.saturating_duration_since(epoch).as_millis()).unwrap_or(u64::MAX)
            }),
            current_bps,
            roles,
        })
    }

    pub(super) fn check_profile(&self, profile_ms: u16) -> Result<(), String> {
        if self.0.profile_ms != profile_ms || self.0.epoch.borrow().is_some() {
            return Err("synthetic service requires a fresh matching generation".into());
        }
        Ok(())
    }

    pub(super) fn arm(&self, epoch: Instant) -> Result<(), String> {
        // Fixed lock order, no awaits. Both roles receive the exact same epoch.
        let mut a = self.0.roles[0]
            .lock()
            .map_err(|_| "synthetic service meter unavailable")?;
        let mut b = self.0.roles[1]
            .lock()
            .map_err(|_| "synthetic service meter unavailable")?;
        a.meter.arm(epoch)?;
        b.meter.arm(epoch)?;
        self.0.epoch.send_replace(Some(epoch));
        Ok(())
    }

    pub(super) fn ingress(&self, role: usize, control: bool) -> IngressService {
        IngressService {
            model: self.clone(),
            role,
            slot: usize::from(control),
            armed: self.0.epoch.subscribe(),
            partial_bytes: 0,
        }
    }
}

/// Lane-local cancellation/retirement accounting, not another reader or queue.
pub(super) struct IngressService {
    model: SyntheticServiceModel,
    role: usize,
    slot: usize,
    armed: watch::Receiver<Option<Instant>>,
    partial_bytes: usize,
}

impl IngressService {
    pub(super) async fn read(
        &mut self,
        reader: &mut OwnedReadHalf,
        buffer: &mut [u8],
    ) -> io::Result<usize> {
        while self.armed.borrow_and_update().is_none() {
            self.armed
                .changed()
                .await
                .map_err(|_| io::Error::other("synthetic service activation unavailable"))?;
        }
        loop {
            // Do not accumulate credit-wait time while the socket is idle.
            reader.readable().await?;
            {
                let mut role = self.model.0.roles[self.role]
                    .lock()
                    .map_err(|_| io::Error::other("synthetic service meter unavailable"))?;
                let now = Instant::now();
                let allowed = role.meter.allowance(now).min(buffer.len());
                if allowed != 0 {
                    // Read and debit under the same short lock. No reservation,
                    // await or refundable charge: only actual returned bytes count.
                    match reader.try_read(&mut buffer[..allowed]) {
                        Ok(count) => {
                            role.finish_wait(self.slot, now);
                            role.meter.consume(count);
                            role.stats.read_bytes =
                                role.stats.read_bytes.saturating_add(count as u64);
                            role.stats.live_partial_bytes += count as u64;
                            self.partial_bytes += count;
                            return Ok(count);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            role.finish_wait(self.slot, now);
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            // Readiness may be a false positive or EOF. A non-consuming one-byte
            // peek outside the lock proves data is pending before counting delay.
            let mut pending = [0];
            if reader.peek(&mut pending).await? == 0 {
                return Ok(0);
            }
            let wait_until = {
                let mut role = self.model.0.roles[self.role]
                    .lock()
                    .map_err(|_| io::Error::other("synthetic service meter unavailable"))?;
                let now = Instant::now();
                if role.meter.allowance(now) != 0 {
                    continue;
                }
                role.wait_since[self.slot].get_or_insert(now);
                role.meter.ready_at(now)
            };
            // The next phase boundary is also a wakeup, including restoration
            // from zero service. All credit is recomputed after a select cancel.
            tokio::time::sleep_until(wait_until.into()).await;
        }
    }

    pub(super) fn authenticated(&mut self, framed_bytes: usize) -> Result<(), String> {
        let mut role = self.model.0.roles[self.role]
            .lock()
            .map_err(|_| "synthetic service meter unavailable")?;
        debug_assert_eq!(self.partial_bytes, framed_bytes);
        role.stats.authenticated_framed_bytes = role
            .stats
            .authenticated_framed_bytes
            .saturating_add(framed_bytes as u64);
        role.stats.authenticated_frames = role.stats.authenticated_frames.saturating_add(1);
        role.stats.live_partial_bytes -= framed_bytes as u64;
        self.partial_bytes = 0;
        Ok(())
    }
}

impl Drop for IngressService {
    fn drop(&mut self) {
        let mut role = self.model.0.roles[self.role]
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        role.finish_wait(self.slot, Instant::now());
        role.stats.live_partial_bytes -= self.partial_bytes as u64;
        role.stats.retired_partial_bytes = role
            .stats
            .retired_partial_bytes
            .saturating_add(self.partial_bytes as u64);
    }
}

impl RoleState {
    fn finish_wait(&mut self, slot: usize, now: Instant) {
        if let Some(since) = self.wait_since[slot].take() {
            self.stats.credit_wait_ns = self
                .stats
                .credit_wait_ns
                .saturating_add(nanos(now.saturating_duration_since(since)));
        }
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Exact integer byte-prefix service. A phase transition discards unused credit;
/// idle credit never exceeds one frame. No offered/admission rate is involved.
struct ByteMeter {
    config: SyntheticServiceConfig,
    burst: u128,
    credit: u128,
    at: Option<Instant>,
    collapse: Option<(Instant, Instant, u32)>,
}

impl ByteMeter {
    fn new(config: SyntheticServiceConfig, burst_bytes: usize) -> Self {
        Self {
            config,
            burst: burst_bytes as u128 * BYTE_CREDIT,
            credit: 0,
            at: None,
            collapse: None,
        }
    }

    fn arm(&mut self, epoch: Instant) -> Result<(), String> {
        if self.at.is_some() {
            return Err("synthetic service already armed".into());
        }
        if let Some(collapse) = self.config.collapse {
            let start = epoch
                .checked_add(Duration::from_millis(collapse.start_ms))
                .ok_or("synthetic service time rejected")?;
            let end = start
                .checked_add(Duration::from_millis(collapse.duration_ms))
                .ok_or("synthetic service time rejected")?;
            self.collapse = Some((start, end, collapse.bps));
        }
        self.at = Some(epoch);
        Ok(())
    }

    fn rate(&self, now: Instant) -> u32 {
        match self.collapse {
            Some((start, end, bps)) if start <= now && now < end => bps,
            _ => self.config.baseline_bps,
        }
    }

    fn allowance(&mut self, now: Instant) -> usize {
        let Some(mut at) = self.at else { return 0 };
        let now = now.max(at);
        if let Some((start, end, _)) = self.collapse {
            for boundary in [start, end] {
                if at < boundary && boundary <= now {
                    self.credit = 0;
                    at = boundary;
                }
            }
        }
        self.credit = self
            .credit
            .saturating_add(
                now.duration_since(at)
                    .as_nanos()
                    .saturating_mul(u128::from(self.rate(now))),
            )
            .min(self.burst);
        self.at = Some(now);
        (self.credit / BYTE_CREDIT) as usize
    }

    fn consume(&mut self, bytes: usize) {
        // The caller's nonblocking read buffer is capped by allowance, under lock.
        self.credit -= bytes as u128 * BYTE_CREDIT;
    }

    fn ready_at(&self, now: Instant) -> Instant {
        let now = now.max(self.at.expect("armed meter"));
        let boundary = self.collapse.and_then(|(start, end, _)| {
            if now < start {
                Some(start)
            } else if now < end {
                Some(end)
            } else {
                None
            }
        });
        let rate = u128::from(self.rate(now));
        if rate == 0 {
            return boundary.expect("zero service has a bounded restoration");
        }
        let deficit = BYTE_CREDIT.saturating_sub(self.credit);
        let delay = Duration::from_nanos(deficit.div_ceil(rate) as u64);
        let ready = now + delay; // At most one byte at a positive u32 rate.
        boundary.map_or(ready, |boundary| ready.min(boundary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(baseline_bps: u32) -> SyntheticServiceConfig {
        SyntheticServiceConfig {
            baseline_bps,
            collapse: None,
        }
    }

    #[test]
    fn fixture_is_strict_bounded_and_independent_of_offered_admission() {
        for baseline in [0, 37_500, 49_999, 60_000, 79_999, 80_001] {
            assert!(SyntheticServiceModel::new(config(baseline), 40).is_err());
        }
        for (profile, burst) in [(20, 152), (40, 152), (60, 194)] {
            let model = SyntheticServiceModel::new(config(50_000), profile).unwrap();
            let snapshot = model.snapshot().unwrap();
            assert_eq!(snapshot.burst_bytes, burst);
            assert!(!snapshot.armed);
            assert_eq!(snapshot.roles, [SyntheticRoleStats::default(); 2]);
        }
        assert!(SyntheticServiceModel::new(config(50_000), 30).is_err());
        for (start_ms, duration_ms, bps) in [
            (MAX_START_MS + 1, 1, 0),
            (0, 0, 0),
            (0, MAX_DURATION_MS + 1, 0),
            (0, 1, 50_001),
            (u64::MAX, u64::MAX, 0),
        ] {
            let value = SyntheticServiceConfig {
                baseline_bps: 50_000,
                collapse: Some(SyntheticCollapse {
                    start_ms,
                    duration_ms,
                    bps,
                }),
            };
            assert!(value.validate().is_err());
        }
        let maximum = SyntheticServiceConfig {
            baseline_bps: 80_000,
            collapse: Some(SyntheticCollapse {
                start_ms: MAX_START_MS,
                duration_ms: MAX_DURATION_MS,
                bps: 80_000,
            }),
        };
        maximum.validate().unwrap();
        assert!(serde_json::from_str::<SyntheticServiceConfig>(
            r#"{"baseline_bps":50000,"extra":1}"#,
        )
        .is_err());
        assert!(serde_json::from_str::<SyntheticServiceConfig>(
            r#"{"baseline_bps":50000,"collapse":{"start_ms":0,"duration_ms":1,"bps":0,"extra":1}}"#,
        )
        .is_err());
    }

    #[test]
    fn combined_media_feedback_has_one_role_allowance_and_two_independent_roles() {
        // Drive the actual role meters with a deterministic clock, consuming each
        // available byte prefix. No wall-clock equality or Tokio timer precision.
        for (bps, combined_ns) in [(50_000, 47_360_000), (80_000, 29_600_000)] {
            let model = SyntheticServiceModel::new(config(bps), 40).unwrap();
            let epoch = Instant::now();
            model.arm(epoch).unwrap();
            for role in 0..2 {
                let mut meter = model.0.roles[role].lock().unwrap();
                assert_eq!(meter.meter.allowance(epoch), 0);
                let mut now = epoch;
                for frame_bytes in [144, 152] {
                    for _ in 0..frame_bytes {
                        now = meter.meter.ready_at(now);
                        assert_eq!(meter.meter.allowance(now), 1);
                        meter.meter.consume(1);
                    }
                }
                assert_eq!(now.duration_since(epoch).as_nanos(), combined_ns);
                assert_eq!(meter.meter.allowance(now), 0);
            }
            // Reading all of A did not debit B, and control did not receive a
            // second burst or a separate 50/80k allowance within a role.
            let media = model.ingress(0, false);
            let feedback = model.ingress(0, true);
            assert!(Arc::ptr_eq(&media.model.0, &feedback.model.0));
            assert_eq!(media.role, feedback.role);
            assert_ne!(media.slot, feedback.slot);
            assert!(model.arm(epoch).is_err());
        }
    }

    #[test]
    fn collapse_resets_credit_bounds_low_service_and_wakes_zero_restoration() {
        for low_bps in [0, 20_000] {
            let mut cfg = config(50_000);
            cfg.collapse = Some(SyntheticCollapse {
                start_ms: 100,
                duration_ms: 300,
                bps: low_bps,
            });
            let mut meter = ByteMeter::new(cfg, 152);
            let epoch = Instant::now();
            meter.arm(epoch).unwrap();
            assert_eq!(meter.allowance(epoch + Duration::from_millis(99)), 152);
            let start = epoch + Duration::from_millis(100);
            let end = epoch + Duration::from_millis(400);
            assert_eq!(meter.allowance(start), 0);
            let mut bytes = 0;
            let mut now = start;
            loop {
                let next = meter.ready_at(now);
                if next >= end {
                    break;
                }
                now = next;
                let count = meter.allowance(now);
                meter.consume(count);
                bytes += count;
            }
            assert!(bytes <= 750);
            assert_eq!(bytes, if low_bps == 0 { 0 } else { 749 });
            // Credit is reset at restoration, even if some fractional/idle credit
            // was left. Crossing both boundaries in one refill also cannot bank it.
            assert_eq!(meter.allowance(end), 0);
            assert_eq!(meter.ready_at(end), end + Duration::from_micros(160));
            assert_eq!(meter.allowance(end + Duration::from_micros(160)), 1);
            let mut skipped = ByteMeter::new(cfg, 152);
            skipped.arm(epoch).unwrap();
            assert_eq!(skipped.allowance(end), 0);
            assert_eq!(skipped.allowance(end + Duration::from_secs(10)), 152);
        }
    }

    #[test]
    fn fractional_byte_credit_is_carried_without_per_read_rounding_drift() {
        for bps in [15_001, 79_999] {
            let mut cfg = config(80_000);
            cfg.collapse = Some(SyntheticCollapse {
                start_ms: 0,
                duration_ms: 1_000,
                bps,
            });
            let mut meter = ByteMeter::new(cfg, 152);
            let epoch = Instant::now();
            meter.arm(epoch).unwrap();
            let mut now = epoch;
            for _ in 0..100 {
                now = meter.ready_at(now);
                assert_eq!(meter.allowance(now), 1);
                meter.consume(1);
            }
            assert_eq!(
                now.duration_since(epoch).as_nanos(),
                (100 * BYTE_CREDIT).div_ceil(u128::from(bps)),
            );
            assert!(meter.credit < BYTE_CREDIT);
        }
    }
}

//! Fixture source clocks. Arrival time is a health check, never a rate sample.
use super::packet::SenderClock;
use std::time::{Duration, Instant};

const NTP_SECOND: u128 = 1 << 32;
const SECOND_NS: u128 = 1_000_000_000;
const WINDOW: Duration = Duration::from_secs(20);
const FRESH: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rate {
    pub ticks: u64,
    pub ns: u64,
}

impl Default for Rate {
    fn default() -> Self {
        Self {
            ticks: 48_000,
            ns: 1_000_000_000,
        }
    }
}

impl Rate {
    pub fn duration(self, ticks: u64) -> Duration {
        let ns = (u128::from(ticks) * u128::from(self.ns)).div_ceil(u128::from(self.ticks));
        Duration::new((ns / SECOND_NS) as u64, (ns % SECOND_NS) as u32)
    }

    pub fn ticks_at(self, elapsed: Duration) -> u64 {
        (elapsed.as_nanos() * u128::from(self.ticks) / u128::from(self.ns)) as u64
    }

    pub fn ppb(self) -> i64 {
        let denominator = i128::from(self.ns) * 48_000;
        ((i128::from(self.ticks) * 1_000_000_000 - denominator) * 1_000_000_000 / denominator)
            as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(ppm: i64) -> Rate {
        Rate {
            ticks: (48_000 * (1_000_000 + ppm)) as u64,
            ns: 1_000_000_000_000_000,
        }
    }

    fn clock_report(clock: &LocalClock, origin: Instant, millis: u64) -> SenderClock {
        clock
            .report(
                origin + Duration::from_millis(millis),
                millis as u32 / 40 + 1,
                123,
            )
            .unwrap()
    }

    #[test]
    fn source_report_pairs_current_clock_not_last_commit_or_wall_readback() {
        let origin = Instant::now();
        // Exercise NTP era and RTP rollover without another wall-clock read.
        let ntp = u64::MAX - (1 << 31);
        let mut local = LocalClock::new(origin, ntp, u32::MAX - 1_000);
        let measured = rate(500);
        local.observe(3_200, origin - Duration::from_millis(50), Some(measured));
        let before = local.report(origin, 7, 321).unwrap();
        let mut remote = RemoteClock::default();
        for millis in (0..=40_000).step_by(200) {
            let now = origin + Duration::from_millis(millis);
            // Callback/encoding stalls must not become clock-frequency samples.
            local.observe(
                3_200 + millis * 16,
                now - Duration::from_millis(3),
                Some(measured),
            );
            let report = clock_report(&local, origin, millis);
            assert_eq!(
                report.ntp,
                ntp.wrapping_add((u128::from(millis) * NTP_SECOND / 1000) as u64)
            );
            remote.observe(report, now);
        }
        let after = local
            .report(origin + Duration::from_secs(40), 7, 321)
            .unwrap();
        assert_eq!(after.rtp.wrapping_sub(before.rtp), 1_920_960);
        assert_eq!((after.packets, after.octets), (7, 321));
        assert!(remote.valid(origin + Duration::from_secs(40)));
        assert_eq!(remote.rate.ppb(), 500_000);
    }

    #[test]
    fn physical_source_readiness_keeps_reference_and_never_falls_back_to_nominal() {
        let origin = Instant::now();
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        assert!(local.report(origin, 7, 321).is_none());
        let captured = origin - Duration::from_millis(50);
        local.observe(
            3_200,
            captured,
            Some(Rate {
                ticks: 960_000,
                ns: 19_999_999_999,
            }),
        );
        assert!(local.report(origin, 7, 321).is_none());
        local.observe(3_360, origin, None);
        assert!(local.report(origin, 7, 321).is_none());
        let measured = Rate {
            ticks: 960_000,
            ns: 20_000_000_000,
        };
        local.observe(19_200, origin + Duration::from_secs(1), Some(measured));
        let report = local
            .report(origin + Duration::from_secs(1), 7, 321)
            .unwrap();
        assert_eq!(report.rtp, 7 + 3_200 * 3 + 50_400);
        assert_eq!((report.packets, report.octets), (7, 321));
        assert_eq!(local.source, Some((3_200, captured)));
        local.observe(19_360, origin + Duration::from_secs(1), None);
        assert!(local
            .report(origin + Duration::from_secs(1), 7, 321)
            .is_none());
        local.observe(19_520, origin + Duration::from_secs(1), Some(measured));
        let recovered = local
            .report(origin + Duration::from_secs(1), 7, 321)
            .unwrap();
        assert_eq!((recovered.ntp, recovered.rtp), (report.ntp, report.rtp));
        local.observe(
            19_680,
            origin + Duration::from_secs(1),
            Some(Rate {
                ticks: 0,
                ns: 20_000_000_000,
            }),
        );
        assert!(local
            .report(origin + Duration::from_secs(1), 7, 321)
            .is_none());

        let mut synthetic = LocalClock::new(origin, 1 << 32, 7);
        synthetic.observe(0, origin, None);
        assert_eq!(synthetic.report(origin, 7, 321).unwrap().rtp, 7);
    }

    #[test]
    fn delivery_delay_bursts_stale_and_invalid_progress_freeze_not_estimate_arrivals() {
        let origin = Instant::now();
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        local.observe(0, origin, Some(rate(-100)));
        let mut remote = RemoteClock::default();
        for millis in (0..=40_000).step_by(200) {
            // Alternating 0/100ms delay changes arrival slopes substantially.
            let delay = if millis % 400 == 0 { 100 } else { 0 };
            assert!(remote.observe(
                clock_report(&local, origin, millis),
                origin + Duration::from_millis(millis + delay)
            ));
        }
        let frozen = remote.rate;
        assert!(remote.calibrated && remote.valid(origin + Duration::from_millis(40_100)));
        assert!(!remote.valid(origin + Duration::from_millis(42_101)));
        // Ordered HOL catch-up: valid source pairs received in one burst.
        assert!(!remote.observe(
            clock_report(&local, origin, 40_200),
            origin + Duration::from_secs(44)
        ));
        assert!(!remote.observe(
            clock_report(&local, origin, 40_400),
            origin + Duration::from_secs(44)
        ));
        assert_eq!(remote.rate, frozen);
        assert!(!remote.valid(origin + Duration::from_secs(44)));
        for millis in (44_200..=66_000).step_by(200) {
            remote.observe(
                clock_report(&local, origin, millis),
                origin + Duration::from_millis(millis),
            );
        }
        assert!(remote.valid(origin + Duration::from_secs(66)));
        assert_eq!(remote.rate.ppb(), -100_000);
        let recovered = remote.rate;
        let normal = clock_report(&local, origin, 66_200);
        for bad in [
            SenderClock { ntp: 0, ..normal },
            SenderClock {
                ntp: 1 << 32,
                ..normal
            },
            SenderClock { rtp: 6, ..normal },
            SenderClock {
                packets: normal.packets - 100,
                ..normal
            },
        ] {
            assert!(!remote.observe(bad, origin + Duration::from_millis(66_200)));
            assert_eq!(remote.rate, recovered);
            assert!(!remote.valid(origin + Duration::from_millis(66_200)));
        }
        let mut unsupported = LocalClock::new(origin, 1 << 32, 7);
        unsupported.observe(0, origin, Some(rate(2_000)));
        let mut unknown = RemoteClock::default();
        for millis in (0..=25_000).step_by(200) {
            unknown.observe(
                clock_report(&unsupported, origin, millis),
                origin + Duration::from_millis(millis),
            );
        }
        assert!(!unknown.calibrated && !unknown.valid(origin + Duration::from_secs(25)));
        assert_eq!(unknown.rate, Rate::default());
    }

    #[test]
    fn hundred_three_hundred_and_thousand_ms_hol_do_not_become_frequency() {
        let origin = Instant::now();
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        local.observe(0, origin, Some(rate(100)));
        for delay in [100, 300, 1_000] {
            let mut remote = RemoteClock::default();
            for millis in (0..=40_000).step_by(200) {
                assert!(remote.observe(
                    clock_report(&local, origin, millis),
                    origin + Duration::from_millis(millis)
                ));
            }
            let frozen = remote.rate;
            let released = origin + Duration::from_millis(40_200 + delay);
            let first = remote.observe(clock_report(&local, origin, 40_200), released);
            assert_eq!(first, delay < 1_000);
            let second_arrival = released.max(origin + Duration::from_millis(40_400));
            remote.observe(clock_report(&local, origin, 40_400), second_arrival);
            assert_eq!(remote.rate, frozen);
            assert_eq!(remote.valid(second_arrival), delay == 100);
            for millis in (42_000..=64_000).step_by(200) {
                remote.observe(
                    clock_report(&local, origin, millis),
                    origin + Duration::from_millis(millis),
                );
            }
            assert!(remote.valid(origin + Duration::from_secs(64)));
            assert_eq!(remote.rate.ppb(), 100_000);
        }
    }

    #[test]
    fn continuous_supported_frequency_changes_have_bounded_window_updates() {
        let origin = Instant::now();
        let mut remote = RemoteClock::default();
        let mut last_rate = 0;
        for millis in (0..=120_000u64).step_by(200) {
            let extra = millis.saturating_sub(20_000) * 24 / 1_000;
            let report = SenderClock {
                ntp: (1u64 << 32).wrapping_add((u128::from(millis) * NTP_SECOND / 1000) as u64),
                rtp: (millis * 48 + extra) as u32,
                packets: (millis / 40 + 1) as u32,
                octets: 0,
            };
            assert!(remote.observe(report, origin + Duration::from_millis(millis)));
            let current = remote.rate.ppb();
            assert!(current.abs_diff(last_rate) <= 100_000);
            last_rate = current;
        }
        assert!(remote.valid(origin + Duration::from_secs(120)));
        assert!((416_660..=416_670).contains(&remote.rate.ppb()));
    }
}

pub(super) fn sink_lead(samples: usize, ppb: i64) -> Duration {
    let ns =
        (samples as u128 * SECOND_NS * SECOND_NS).div_ceil(16_000 * (1_000_000_000 + ppb) as u128);
    Duration::from_nanos(ns as u64)
}

pub(super) fn ntp_from_unix(unix: Duration) -> u64 {
    ((unix.as_secs().wrapping_add(2_208_988_800)) << 32)
        | ((u64::from(unix.subsec_nanos()) << 32) / 1_000_000_000)
}

pub(super) struct LocalClock {
    epoch: Instant,
    ntp: u64,
    initial_timestamp: u32,
    source: Option<(u64, Instant)>,
    rate: Option<Rate>,
    physical: bool,
}

impl LocalClock {
    pub fn new(epoch: Instant, ntp: u64, initial_timestamp: u32) -> Self {
        Self {
            epoch,
            ntp,
            initial_timestamp,
            source: None,
            rate: None,
            physical: false,
        }
    }

    pub fn observe(&mut self, position: u64, captured_at: Instant, rate: Option<Rate>) {
        // This is absolute sample time, not the application-age floor or
        // encoding/admission/Noise callback time. Later observations cannot
        // move the phase reference or contribute callback jitter to frequency.
        self.source.get_or_insert((position, captured_at));
        self.rate = match rate {
            Some(rate) => {
                self.physical = true;
                // ns is the validated hardware span, not callback uptime. A
                // short-span ratio can turn sub-ms timestamp quantization into
                // discontinuous projected RTP. Keep RR/APP feedback until the
                // source has a full long window; do not publish nominal timing.
                (rate.ticks != 0 && u128::from(rate.ns) >= WINDOW.as_nanos()).then_some(rate)
            }
            None if !self.physical => Some(Rate::default()), // Declared synthetic input.
            None => None, // Missing physical metadata is not a nominal clock.
        };
    }

    pub fn report(&self, now: Instant, packets: u32, octets: u32) -> Option<SenderClock> {
        let rate = self.rate?;
        let (position, captured_at) = self.source?;
        let elapsed = now.checked_duration_since(captured_at)?;
        let ntp_elapsed =
            now.checked_duration_since(self.epoch)?.as_nanos() * NTP_SECOND / SECOND_NS;
        Some(SenderClock {
            ntp: self.ntp.wrapping_add(ntp_elapsed as u64),
            rtp: self.initial_timestamp.wrapping_add(
                (position
                    .wrapping_mul(3)
                    .wrapping_add(rate.ticks_at(elapsed))) as u32,
            ),
            packets,
            octets,
        })
    }
}

#[derive(Clone, Copy)]
struct Observation {
    report: SenderClock,
    arrival: Instant,
    ticks: u64,
}

#[derive(Default)]
pub(super) struct RemoteClock {
    pub rate: Rate,
    pub calibrated: bool,
    first: Option<Observation>,
    latest: Option<Observation>,
    updated_ntp: Option<u64>,
    healthy: Option<Instant>,
}

impl RemoteClock {
    pub fn fresh_until(&self) -> Option<Instant> {
        self.calibrated
            .then_some(self.healthy)
            .flatten()?
            .checked_add(FRESH)
    }

    pub fn valid(&self, now: Instant) -> bool {
        self.calibrated
            && self.healthy.is_some_and(|at| {
                now.checked_duration_since(at)
                    .is_some_and(|age| age <= FRESH)
            })
    }

    fn freeze(&mut self) -> bool {
        // Keep the established rate and timeline. Recovery needs another full
        // healthy window; it never follows a delayed arrival baseline.
        self.first = None;
        self.healthy = None;
        self.updated_ntp = None;
        false
    }

    pub fn observe(&mut self, report: SenderClock, arrival: Instant) -> bool {
        if report.ntp == 0 {
            return self.freeze();
        }
        let ticks = if let Some(previous) = self.latest {
            let delta_ntp = report.ntp.wrapping_sub(previous.report.ntp);
            let ns = u128::from(delta_ntp) * SECOND_NS / NTP_SECOND;
            let delta_ticks = report.rtp.wrapping_sub(previous.report.rtp);
            let packet_progress = report.packets.wrapping_sub(previous.report.packets);
            let arrival_gap = arrival.checked_duration_since(previous.arrival);
            if ns < 50_000_000 || ns > FRESH.as_nanos()
                || delta_ticks == 0 || delta_ticks >= 1 << 31
                || packet_progress == 0 || packet_progress >= 1 << 31
                || arrival_gap.is_none_or(|gap| gap < Duration::from_millis(50) || gap > FRESH)
                || arrival_gap.is_some_and(|gap| {
                    gap.abs_diff(Duration::from_nanos(ns.min(u128::from(u64::MAX)) as u64))
                        > Duration::from_millis(500)
                })
                // Short intervals have one-tick quantization. This is solely a
                // discontinuity check, not the long-window frequency estimate.
                || (i128::from(delta_ticks) * 1_000_000_000 - ns as i128 * 48_000).abs()
                    * 1_000_000 > ns as i128 * 48_000 * 2_500
            {
                self.latest = Some(Observation {
                    report,
                    arrival,
                    ticks: u64::from(report.rtp),
                });
                return self.freeze();
            }
            previous.ticks + u64::from(delta_ticks)
        } else {
            u64::from(report.rtp)
        };
        let observation = Observation {
            report,
            arrival,
            ticks,
        };
        self.latest = Some(observation);
        let first = *self.first.get_or_insert(observation);
        let ns = u128::from(report.ntp.wrapping_sub(first.report.ntp)) * SECOND_NS / NTP_SECOND;
        if ns < WINDOW.as_nanos() {
            return true;
        }
        if self.updated_ntp.is_some_and(|ntp| {
            u128::from(report.ntp.wrapping_sub(ntp)) * SECOND_NS / NTP_SECOND < WINDOW.as_nanos()
        }) {
            self.healthy = Some(arrival);
            return true;
        }
        let candidate = Rate {
            ticks: ticks - first.ticks,
            ns: ns as u64,
        };
        // Tested +/-500ppm plus <=2ppm endpoint quantization at twenty seconds.
        // Reject, rather than clamp, a discontinuity or an unsupported clock.
        if candidate.ppb().abs() > 502_000 {
            return self.freeze();
        }
        self.rate = if self.calibrated && candidate.ppb().abs_diff(self.rate.ppb()) > 100_000 {
            // A supported continuous clock can change slowly. Limit correction
            // to 100ppm per complete window; do not turn an in-range target into
            // permanent invalidity, and never clamp an out-of-range report.
            let ppb =
                self.rate.ppb() + (candidate.ppb() - self.rate.ppb()).clamp(-100_000, 100_000);
            Rate {
                ticks: (48_000 * (1_000_000_000 + ppb)) as u64,
                ns: 1_000_000_000_000_000_000,
            }
        } else {
            candidate
        };
        self.calibrated = true;
        self.updated_ntp = Some(report.ntp);
        self.healthy = Some(arrival);
        true
    }
}

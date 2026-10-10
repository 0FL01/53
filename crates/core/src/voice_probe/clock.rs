//! Fixture source clocks. Arrival time is a health check, never a rate sample.
use super::packet::SenderClock;
use std::cell::Cell;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    use crate::voice_probe::packet;

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

    fn protected_report(sender: &mut dmsg_srtp_sys::Sender, source: SenderClock) -> Vec<u8> {
        let plain = packet::compound(packet::Report {
            sender_ssrc: 7,
            peer_ssrc: 8,
            cname: b"test0001",
            sender: Some((source.ntp, source.rtp, source.packets, source.octets)),
            feedback: packet::Feedback {
                terminal: Some(9),
                ..packet::Feedback::default()
            },
        });
        assert_eq!(plain.len(), 116);
        let cipher = sender.protect_rtcp(&plain).unwrap();
        assert_eq!(cipher.len() + packet::FRAMING_BYTES, 152);
        cipher
    }

    fn authenticated_report(receiver: &mut dmsg_srtp_sys::Receiver, cipher: &[u8]) -> SenderClock {
        let plain = receiver.unprotect_rtcp(cipher).unwrap();
        let parsed = packet::read_compound(&plain, 7, 8, b"test0001").unwrap();
        assert_eq!(parsed.terminal, Some(9));
        parsed.sender_clock.unwrap()
    }

    #[test]
    fn authenticated_debunched_reports_keep_source_frequency_and_health() {
        // R5's source/receiver decision separates authenticated NTP/RTP frequency
        // from transport jitter. A fresh, ordered burst has the same source pairs
        // as punctual delivery: it must not require twenty seconds of recovery.
        // At the nominal 200ms cadence, <=300ms backlog permits a two-report
        // burst. Three/four reports need 100ms source intervals (still >=50ms);
        // four nominal 200ms reports would necessarily exceed that backlog bound.
        for ppm in [-500, -100, 100, 500] {
            for (period_ms, burst_reports) in [(200u64, 2u64), (100, 3), (100, 4)] {
                // Leave >=50ms before the next unbunched source report too.
                let burst_delay_ms = if burst_reports == 3 { 230 } else { 300 };
                let origin = Instant::now();
                let mut local = LocalClock::new(origin, u64::MAX - (1 << 31), u32::MAX - 1_000);
                local.observe(0, origin, Some(rate(ppm)));
                let mut sender = dmsg_srtp_sys::Sender::new(&[46; 30], 7).unwrap();
                let mut receiver = dmsg_srtp_sys::Receiver::new(&[46; 30], 7).unwrap();
                let mut punctual = RemoteClock::default();
                let mut debunched = RemoteClock::default();
                let mut previous_arrival: Option<Instant> = None;
                let mut short_arrivals = 0;
                let mut max_backlog_ms = 0;
                for millis in (0..=120_000u64).step_by(period_ms as usize) {
                    let source_at = origin + Duration::from_millis(millis);
                    let source = clock_report(&local, origin, millis);
                    let cipher = protected_report(&mut sender, source);
                    let parsed = authenticated_report(&mut receiver, &cipher);
                    assert_eq!(parsed, source);
                    assert!(punctual.observe(parsed, source_at));
                    let delay_ms = if (40_001..=80_000).contains(&millis) {
                        let slot = ((millis - 40_000) / period_ms - 1) % (burst_reports + 2);
                        if slot < burst_reports {
                            burst_delay_ms - slot * (period_ms - 10)
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    max_backlog_ms = max_backlog_ms.max(delay_ms);
                    let arrival = source_at + Duration::from_millis(delay_ms);
                    let gap = previous_arrival.map(|previous| arrival - previous);
                    if let Some(gap) = gap {
                        assert!(!gap.is_zero() && gap <= FRESH);
                        assert!(
                            gap.abs_diff(Duration::from_millis(period_ms))
                                <= Duration::from_millis(300)
                        );
                        if gap < Duration::from_millis(50) {
                            short_arrivals += 1;
                        }
                    }
                    let accepted = debunched.observe(parsed, arrival);
                    assert!(
                        accepted,
                        "{ppm}ppm/{burst_reports}-report burst: source={millis}ms, backlog={delay_ms}ms, arrival_gap={gap:?}, mask={}, calibrated={}, valid={}",
                        debunched.rejection_mask,
                        debunched.calibrated,
                        debunched.valid(arrival),
                    );
                    // Both estimators see identical source pairs. Arrival phase
                    // cannot affect either the rate or the source calibration span.
                    assert_eq!(debunched.rate, punctual.rate);
                    assert_eq!(debunched.calibrated, millis >= 20_000);
                    assert_eq!(debunched.valid(arrival), millis >= 20_000);
                    if millis >= 20_000 {
                        assert_eq!(debunched.rate.ppb(), ppm * 1_000);
                    }
                    previous_arrival = Some(arrival);
                }
                assert!(short_arrivals > 0);
                assert_eq!(max_backlog_ms, burst_delay_ms);
                assert!(max_backlog_ms <= 300);
                assert_eq!(debunched.rejection_mask, 0);
                let end = origin + Duration::from_secs(120);
                assert_eq!(debunched.fresh_until(), Some(end + FRESH));
                assert!(debunched.valid(end + FRESH));
                assert!(!debunched.valid(end + FRESH + Duration::from_nanos(1)));
            }
        }
    }

    #[test]
    fn authenticated_burst_keeps_replay_source_and_stale_arrival_guards() {
        let origin = Instant::now();
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        local.observe(0, origin, Some(rate(100)));
        let before = clock_report(&local, origin, 40_000);
        let normal = clock_report(&local, origin, 40_200);
        let cases = [
            (SenderClock { ntp: 0, ..normal }, 200, Rejection::ZeroNtp),
            (
                clock_report(&local, origin, 40_049),
                200,
                Rejection::SourceInterval,
            ),
            (
                clock_report(&local, origin, 42_001),
                200,
                Rejection::SourceInterval,
            ),
            (before, 200, Rejection::SourceInterval),
            (
                clock_report(&local, origin, 39_800),
                200,
                Rejection::SourceInterval,
            ),
            (
                SenderClock {
                    rtp: before.rtp,
                    ..normal
                },
                200,
                Rejection::RtpProgress,
            ),
            (
                SenderClock {
                    rtp: before.rtp - 1,
                    ..normal
                },
                200,
                Rejection::RtpProgress,
            ),
            (
                SenderClock {
                    packets: before.packets,
                    ..normal
                },
                200,
                Rejection::PacketProgress,
            ),
            (
                SenderClock {
                    packets: before.packets - 1,
                    ..normal
                },
                200,
                Rejection::PacketProgress,
            ),
            (normal, 0, Rejection::ArrivalInterval),
            (normal, -1, Rejection::ArrivalInterval),
            (normal, 2_001, Rejection::ArrivalInterval),
            (normal, 701, Rejection::ArrivalSourceGap),
            (
                SenderClock {
                    rtp: normal.rtp + 48,
                    ..normal
                },
                10,
                Rejection::ShortRate,
            ),
        ];
        for (bad, arrival_ms, reason) in cases {
            let mut sender = dmsg_srtp_sys::Sender::new(&[47; 30], 7).unwrap();
            let mut receiver = dmsg_srtp_sys::Receiver::new(&[47; 30], 7).unwrap();
            let mut remote = RemoteClock::default();
            for millis in (0..=40_000).step_by(200) {
                let source = clock_report(&local, origin, millis);
                let cipher = protected_report(&mut sender, source);
                let parsed = authenticated_report(&mut receiver, &cipher);
                assert!(remote.observe(parsed, origin + Duration::from_millis(millis)));
            }
            let frozen = remote.rate;
            let fresh_until = remote.fresh_until();
            assert!(remote.valid(origin + Duration::from_secs(40)));
            let cipher = protected_report(&mut sender, bad);
            let mut forged = cipher.clone();
            forged[16] ^= 1; // A changed RTP clock must fail whole-compound authentication.
            assert!(receiver.unprotect_rtcp(&forged).is_err());
            assert_eq!(remote.rate, frozen);
            assert_eq!(remote.fresh_until(), fresh_until);
            let parsed = authenticated_report(&mut receiver, &cipher);
            assert_eq!(parsed, bad);
            assert!(receiver.unprotect_rtcp(&cipher).is_err()); // No replay reaches observe().
            assert_eq!(remote.fresh_until(), fresh_until);
            let arrival = origin + Duration::from_millis((40_000 + arrival_ms) as u64);
            assert!(!remote.observe(parsed, arrival));
            assert_eq!(remote.rejection_mask, reason.bit());
            assert_eq!(remote.rate, frozen);
            assert!(remote.calibrated);
            assert!(!remote.valid(arrival));
            assert_eq!(remote.fresh_until(), None);
            assert!(remote.first.is_none() && remote.updated_ntp.is_none());
        }
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
    fn authenticated_peer_epochs_and_wall_jumps_do_not_change_frequency() {
        // UTC offsets are deliberately included in the seeded wall epochs as
        // a stronger case than display-only timezones. No OS clock is changed.
        let cases = [
            (
                Duration::from_secs(31_536_000 - 12 * 3_600),
                Duration::from_secs(6_307_200_000 + 14 * 3_600),
                [-100, 100],
            ),
            (
                Duration::new(u64::from(u32::MAX) - 2_208_988_800, 500_000_001),
                Duration::from_secs(1_787_000_000 + 20_700),
                [-500, 500],
            ),
        ];
        for (wall_a, wall_b, ppm) in cases {
            let origin = Instant::now();
            let epochs = [ntp_from_unix(wall_a), ntp_from_unix(wall_b)];
            assert_ne!(epochs[0], epochs[1]);
            let mut locals = [
                LocalClock::new(origin, epochs[0], u32::MAX - 1_000),
                LocalClock::new(origin, epochs[1], u32::MAX - 2_000),
            ];
            let mut peers = [RemoteClock::default(), RemoteClock::default()];
            let keys = [[42; 30], [43; 30]];
            let mut senders = [
                dmsg_srtp_sys::Sender::new(&keys[0], 7).unwrap(),
                dmsg_srtp_sys::Sender::new(&keys[1], 8).unwrap(),
            ];
            let mut receivers = [
                dmsg_srtp_sys::Receiver::new(&keys[0], 7).unwrap(),
                dmsg_srtp_sys::Receiver::new(&keys[1], 8).unwrap(),
            ];
            let mut rejected = [0u64; 2];
            for role in 0..2 {
                locals[role].observe(0, origin, Some(rate(ppm[role])));
            }
            for millis in (0..=40_000u64).step_by(200) {
                let now = origin + Duration::from_millis(millis);
                for role in 0..2 {
                    // Model new SystemTime readbacks jumping backwards/forwards
                    // mid-session. LocalClock retains its single captured epoch;
                    // report() takes only monotonic time, not these readbacks.
                    let wall_readback = if millis < 20_000 {
                        [wall_a, wall_b][role] + Duration::from_millis(millis)
                    } else if millis < 30_000 {
                        Duration::from_secs(1 + role as u64 * 3_600)
                    } else {
                        Duration::from_secs(7_200_000_000 + role as u64 * 50_400)
                    };
                    locals[role].observe(
                        millis * 16,
                        now - Duration::from_millis(3),
                        Some(rate(ppm[role])),
                    );
                    let count = (u32::MAX - 10).wrapping_add(millis as u32 / 40 + 1);
                    let source = locals[role].report(now, count, 321).unwrap();
                    assert_eq!(
                        source.ntp,
                        epochs[role].wrapping_add((u128::from(millis) * NTP_SECOND / 1_000) as u64)
                    );
                    if millis >= 20_000 {
                        assert_ne!(source.ntp, ntp_from_unix(wall_readback));
                    }
                    let ssrc = 7 + role as u32;
                    let plain = packet::compound(packet::Report {
                        sender_ssrc: ssrc,
                        peer_ssrc: 15 - ssrc,
                        cname: b"test0001",
                        sender: Some((source.ntp, source.rtp, source.packets, source.octets)),
                        feedback: packet::Feedback {
                            terminal: Some(9),
                            ..packet::Feedback::default()
                        },
                    });
                    assert_eq!(plain.len(), 116);
                    let cipher = senders[role].protect_rtcp(&plain).unwrap();
                    assert_eq!(cipher.len() + packet::FRAMING_BYTES, 152);
                    let plain = receivers[role].unprotect_rtcp(&cipher).unwrap();
                    let parsed =
                        packet::read_compound(&plain, ssrc, 15 - ssrc, b"test0001").unwrap();
                    assert_eq!(parsed.terminal, Some(9));
                    assert_eq!(parsed.sender_clock, Some(source));
                    rejected[role] +=
                        u64::from(!peers[role].observe(parsed.sender_clock.unwrap(), now));
                    assert_eq!(locals[role].source, Some((0, origin)));
                }
            }
            assert_eq!(rejected, [0, 0]);
            for role in 0..2 {
                assert!(peers[role].valid(origin + Duration::from_secs(40)));
                assert_eq!(peers[role].rate.ppb(), ppm[role] * 1_000);
                assert_eq!(peers[role].rejection_mask, 0);
            }
        }
    }

    #[test]
    fn signed_system_wall_epochs_preserve_ntp_fraction_and_era() {
        let unix_ntp = 2_208_988_800u64 << 32;
        for duration in [
            Duration::ZERO,
            Duration::from_nanos(1),
            Duration::new(1, 999_999_999),
            Duration::new(u64::from(u32::MAX) - 2_208_988_800, 500_000_001),
        ] {
            assert_eq!(
                ntp_from_system_time(UNIX_EPOCH + duration),
                ntp_from_unix(duration)
            );
        }
        for (before, expected) in [
            (Duration::from_nanos(1), unix_ntp - 5),
            (Duration::from_millis(500), unix_ntp - (1 << 31)),
            (Duration::new(1, 500_000_000), unix_ntp - 3 * (1 << 31)),
            (Duration::new(0, 999_999_999), unix_ntp - (1 << 32) + 4),
            (Duration::from_secs(2_208_988_800), 0),
            (Duration::new(2_208_988_800, 1), u64::MAX - 4),
            (Duration::new(2_208_988_799, 999_999_999), 4),
            (Duration::from_secs(2_208_988_800 + (1 << 32)), 0),
        ] {
            assert_eq!(ntp_from_system_time(UNIX_EPOCH - before), expected);
        }
        let era = UNIX_EPOCH + Duration::from_secs((1 << 32) - 2_208_988_800);
        for (wall, expected) in [
            (era - Duration::from_nanos(1), u64::MAX - 4),
            (era, 0),
            (era + Duration::from_nanos(1), 4),
        ] {
            assert_eq!(ntp_from_system_time(wall), expected);
        }
    }

    #[test]
    fn authenticated_pre_unix_peers_calibrate_with_zero_epoch_rr() {
        let ntp_epoch = UNIX_EPOCH - Duration::from_secs(2_208_988_800);
        let previous_era = ntp_epoch - Duration::from_secs(1 << 32);
        let next_era = UNIX_EPOCH + Duration::from_secs((1 << 32) - 2_208_988_800);
        let cases = [
            (
                UNIX_EPOCH - Duration::from_nanos(1),
                UNIX_EPOCH + Duration::from_secs(1_787_000_000),
                [-100, 100],
            ),
            (
                UNIX_EPOCH - Duration::new(1, 500_000_000),
                UNIX_EPOCH - Duration::from_secs(31_536_000),
                [-500, 500],
            ),
            (ntp_epoch, next_era, [-500, 500]),
            (
                previous_era - Duration::from_nanos(1),
                ntp_epoch + Duration::from_nanos(1),
                [-100, 100],
            ),
        ];
        for (wall_a, wall_b, ppm) in cases {
            let origin = Instant::now();
            let epochs = [ntp_from_system_time(wall_a), ntp_from_system_time(wall_b)];
            let mut locals = [
                LocalClock::new(origin, epochs[0], u32::MAX - 1_000),
                LocalClock::new(origin, epochs[1], u32::MAX - 2_000),
            ];
            let mut peers = [RemoteClock::default(), RemoteClock::default()];
            let keys = [[44; 30], [45; 30]];
            let mut senders = [
                dmsg_srtp_sys::Sender::new(&keys[0], 7).unwrap(),
                dmsg_srtp_sys::Sender::new(&keys[1], 8).unwrap(),
            ];
            let mut receivers = [
                dmsg_srtp_sys::Receiver::new(&keys[0], 7).unwrap(),
                dmsg_srtp_sys::Receiver::new(&keys[1], 8).unwrap(),
            ];
            let mut first_sr = [None; 2];
            let mut first_valid = [None; 2];
            for role in 0..2 {
                locals[role].observe(0, origin, Some(rate(ppm[role])));
            }
            for millis in (0..=40_400u64).step_by(200) {
                let now = origin + Duration::from_millis(millis);
                for role in 0..2 {
                    let count = (u32::MAX - 10).wrapping_add(millis as u32 / 40 + 1);
                    let report = locals[role].report(now, count, 321);
                    let expected_ntp =
                        epochs[role].wrapping_add((u128::from(millis) * NTP_SECOND / 1_000) as u64);
                    assert_eq!(report.is_some(), expected_ntp != 0);
                    if let Some(report) = report {
                        assert_eq!(report.ntp, expected_ntp);
                        assert_eq!((report.packets, report.octets), (count, 321));
                        first_sr[role].get_or_insert(millis);
                    }
                    let ssrc = 7 + role as u32;
                    let plain = packet::compound(packet::Report {
                        sender_ssrc: ssrc,
                        peer_ssrc: 15 - ssrc,
                        cname: b"test0001",
                        sender: report.map(|r| (r.ntp, r.rtp, r.packets, r.octets)),
                        feedback: packet::Feedback {
                            terminal: Some(9),
                            ..packet::Feedback::default()
                        },
                    });
                    assert_eq!(plain.len(), if report.is_some() { 116 } else { 96 });
                    let cipher = senders[role].protect_rtcp(&plain).unwrap();
                    assert_eq!(
                        cipher.len() + packet::FRAMING_BYTES,
                        if report.is_some() { 152 } else { 132 }
                    );
                    let plain = receivers[role].unprotect_rtcp(&cipher).unwrap();
                    let parsed =
                        packet::read_compound(&plain, ssrc, 15 - ssrc, b"test0001").unwrap();
                    assert_eq!(parsed.terminal, Some(9));
                    assert_eq!(parsed.sender_clock, report);
                    if let Some(report) = parsed.sender_clock {
                        assert!(peers[role].observe(report, now));
                    }
                    if peers[role].valid(now) {
                        first_valid[role].get_or_insert(millis);
                    }
                    assert_eq!(locals[role].source, Some((0, origin)));
                }
            }
            for role in 0..2 {
                let delay = if epochs[role] == 0 { 200 } else { 0 };
                assert_eq!(first_sr[role], Some(delay));
                assert_eq!(first_valid[role], Some(delay + 20_000));
                assert!(peers[role].valid(origin + Duration::from_millis(40_400)));
                assert_eq!(peers[role].rate.ppb(), ppm[role] * 1_000);
                assert_eq!(peers[role].rejection_mask, 0);
            }
        }
    }

    #[test]
    fn zero_ntp_instant_keeps_rr_feedback_and_strict_receiver_guard() {
        let origin = Instant::now();
        let mut remote = RemoteClock::default();
        assert!(!remote.observe(
            SenderClock {
                ntp: 0,
                rtp: 7,
                packets: 1,
                octets: 321,
            },
            origin
        ));
        assert_eq!(remote.rejection_mask, Rejection::ZeroNtp.bit());
        assert!(!remote.calibrated);

        let mut local = LocalClock::new(origin, 0, 7);
        local.observe(0, origin, None);
        assert!(local.report(origin, 7, 321).is_none());
        let first = local
            .report(origin + Duration::from_nanos(1), 7, 321)
            .unwrap();
        assert_eq!(
            (first.ntp, first.rtp, first.packets, first.octets),
            (4, 7, 7, 321)
        );
        assert_eq!(local.source, Some((0, origin)));
        assert_eq!(local.ntp, 0);
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
    fn source_frequency_changes_integrate_without_rewriting_published_phase() {
        let origin = Instant::now();
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        local.observe(
            0,
            origin,
            Some(Rate {
                ticks: 960_000,
                ns: 20_000_000_000,
            }),
        );
        for millis in (20_200..40_000u64).step_by(200) {
            // A real continuous +500ppm clock after its first nominal 20s.
            // Hardware deltas, not callback/capture-delivery time, carry rate.
            let ticks = millis * 48 + (millis - 20_000) * 24 / 1_000;
            local.observe(
                ticks / 3,
                origin + Duration::from_millis(millis - 60),
                Some(Rate {
                    ticks,
                    ns: millis * 1_000_000,
                }),
            );
        }
        let now = origin + Duration::from_secs(40);
        let before = local.report(now, 7, 321).unwrap();
        let hardware = Rate {
            ticks: 1_920_480,
            ns: 40_000_000_000,
        };
        // The just-delivered oldest sample precedes an already published point.
        // A frequency change must not retroactively change that point or epoch.
        local.observe(640_160, now - Duration::from_millis(60), Some(hardware));
        assert_eq!(local.report(now, 7, 321), Some(before));
        assert_eq!(local.projection.as_ref().unwrap().rate.ppb(), 500_000);
        let after = local.report(now + Duration::from_secs(20), 7, 321).unwrap();
        assert_eq!(after.rtp.wrapping_sub(before.rtp), 960_480);
        assert_eq!((after.packets, after.octets), (7, 321));
        assert_eq!(local.source, Some((0, origin)));
        assert_eq!(local.ntp, 1 << 32);

        local.observe(640_320, now, None);
        assert!(local
            .report(now + Duration::from_secs(20), 7, 321)
            .is_none());
        local.observe(640_480, now, Some(hardware));
        assert_eq!(
            local.report(now + Duration::from_secs(20), 7, 321),
            Some(after)
        );
        assert_eq!(local.source, Some((0, origin)));
    }

    #[test]
    fn hardware_mean_windows_cancel_anchor_error_and_ignore_duplicate_callbacks() {
        for ppm in [-500i64, -100, 100, 500] {
            let mut hardware = HardwareWindow::default();
            let mut fits = Vec::new();
            for millis in (200..=60_200u64).step_by(200) {
                // Constant first-anchor error cancels in differences of means;
                // ongoing opposite quantization must not alias a lone endpoint.
                let error = if millis % 400 == 0 {
                    800_000i64
                } else {
                    -800_000
                };
                let point = Rate {
                    ticks: (u128::from(millis) * 48_000 * (1_000_000 + ppm) as u128 / 1_000_000_000)
                        as u64,
                    ns: (millis as i64 * 1_000_000 + error - 800_000) as u64,
                };
                if let Some(fit) = hardware.observe(point) {
                    fits.push(fit.ppb());
                }
                let counts = (hardware.early.count, hardware.late.count);
                // Twenty reads of the same hardware point are still one point.
                for _ in 0..20 {
                    assert!(hardware.observe(point).is_none());
                }
                assert_eq!((hardware.early.count, hardware.late.count), counts);
            }
            assert_eq!(fits.len(), 3);
            assert!(
                fits.iter().all(|ppb| ppb.abs_diff(ppm * 1_000) <= 2_000),
                "hardware mean fits at {ppm}ppm: {fits:?}"
            );
        }
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

    #[test]
    fn rejection_mask_identifies_existing_checks_without_changing_freeze() {
        let origin = Instant::now();
        let mut local = LocalClock::new(origin, 1 << 32, 7);
        local.observe(0, origin, None);
        let before = clock_report(&local, origin, 0);
        let normal = clock_report(&local, origin, 200);
        let cases = [
            (SenderClock { ntp: 0, ..normal }, 200, Rejection::ZeroNtp),
            (
                SenderClock {
                    ntp: before.ntp,
                    ..normal
                },
                200,
                Rejection::SourceInterval,
            ),
            (
                SenderClock {
                    rtp: before.rtp,
                    ..normal
                },
                200,
                Rejection::RtpProgress,
            ),
            (
                SenderClock {
                    packets: before.packets,
                    ..normal
                },
                200,
                Rejection::PacketProgress,
            ),
            // Debunching may shorten positive arrival intervals, but duplicate
            // monotonic observations remain invalid even with source progress.
            (normal, 0, Rejection::ArrivalInterval),
            (normal, 900, Rejection::ArrivalSourceGap),
            (
                SenderClock {
                    rtp: normal.rtp + 48,
                    ..normal
                },
                200,
                Rejection::ShortRate,
            ),
        ];
        let mut accumulated = 0;
        for (report, arrival_ms, reason) in cases {
            let mut remote = RemoteClock::default();
            assert!(remote.observe(before, origin));
            assert!(!remote.observe(report, origin + Duration::from_millis(arrival_ms)));
            assert_eq!(remote.rejection_mask, reason.bit());
            assert_eq!(remote.rate, Rate::default());
            assert!(!remote.calibrated && !remote.valid(origin));
            accumulated |= remote.rejection_mask;
        }
        let mut unsupported = LocalClock::new(origin, 1 << 32, 7);
        unsupported.observe(0, origin, Some(rate(2_000)));
        let mut remote = RemoteClock::default();
        for millis in (0..=20_000u64).step_by(200) {
            let accepted = remote.observe(
                clock_report(&unsupported, origin, millis),
                origin + Duration::from_millis(millis),
            );
            assert_eq!(accepted, millis < 20_000);
        }
        assert_eq!(remote.rejection_mask, Rejection::LongRate.bit());
        assert_eq!(remote.rate, Rate::default());
        assert!(!remote.calibrated);
        // Recovery clears health/window, not the bounded diagnostic history.
        for millis in (20_200..=40_200).step_by(200) {
            let report = clock_report(&local, origin, millis);
            // Return to nominal frequency continuously: retain the 1,920 RTP
            // ticks gained during the first twenty seconds at +2,000ppm.
            remote.observe(
                SenderClock {
                    rtp: report.rtp.wrapping_add(1_920),
                    ..report
                },
                origin + Duration::from_millis(millis),
            );
        }
        assert!(remote.valid(origin + Duration::from_millis(40_200)));
        assert_eq!(remote.rejection_mask, Rejection::LongRate.bit());
        assert_eq!(accumulated | remote.rejection_mask, 0xff);
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

pub(super) fn ntp_from_system_time(wall: SystemTime) -> u64 {
    match wall.duration_since(UNIX_EPOCH) {
        Ok(unix) => ntp_from_unix(unix),
        Err(before) => {
            // Floor the signed fixed-point timestamp: subtracting a negative
            // fractional Unix duration needs ceil, not truncation towards zero.
            // Wrapping preserves the standard 32-bit NTP seconds/era semantics.
            let ticks = (before.duration().as_nanos() * NTP_SECOND).div_ceil(SECOND_NS);
            ntp_from_unix(Duration::ZERO).wrapping_sub(ticks as u64)
        }
    }
}

#[derive(Default)]
struct HardwareMean {
    ticks: u128,
    ns: u128,
    count: u64,
}

impl HardwareMean {
    fn add(&mut self, point: Rate) {
        self.ticks += u128::from(point.ticks);
        self.ns += u128::from(point.ns);
        self.count += 1;
    }
}

#[derive(Default)]
struct HardwareWindow {
    start_ns: u64,
    early: HardwareMean,
    late: HardwareMean,
    latest: Option<Rate>,
}

impl HardwareWindow {
    fn observe(&mut self, point: Rate) -> Option<Rate> {
        // Repeated AudioRecord readbacks are one hardware point, regardless of
        // how many PCM callbacks drain it. Callback time never enters this fit.
        if self.latest == Some(point) {
            return None;
        }
        self.latest = Some(point);
        let window_ns = WINDOW.as_nanos() as u64;
        let mut fitted = None;
        if point.ns >= self.start_ns + window_ns {
            if self.early.count != 0 && self.late.count != 0 {
                // Difference of the two mean hardware points cancels the
                // first timestamp's constant error and averages quantization.
                let ticks = self.late.ticks * u128::from(self.early.count)
                    - self.early.ticks * u128::from(self.late.count);
                let ns = self.late.ns * u128::from(self.early.count)
                    - self.early.ns * u128::from(self.late.count);
                if ticks != 0 && ns != 0 {
                    let (mut a, mut b) = (ticks, ns);
                    while b != 0 {
                        (a, b) = (b, a % b);
                    }
                    fitted = u64::try_from(ticks / a)
                        .ok()
                        .zip(u64::try_from(ns / a).ok())
                        .map(|(ticks, ns)| Rate { ticks, ns });
                }
            }
            self.start_ns = point.ns / window_ns * window_ns;
            self.early = HardwareMean::default();
            self.late = HardwareMean::default();
        }
        if point.ns - self.start_ns < window_ns / 2 {
            self.early.add(point);
        } else {
            self.late.add(point);
        }
        fitted
    }
}

struct SourceProjection {
    at: Instant,
    // Fractional RTP ticks: carry sub-tick progress across frequency changes.
    phase: u128,
    rate: Rate,
}

impl SourceProjection {
    fn phase_at(&self, now: Instant) -> Option<u128> {
        let product = now
            .checked_duration_since(self.at)?
            .as_nanos()
            .checked_mul(u128::from(self.rate.ticks))?;
        let ns = u128::from(self.rate.ns);
        let whole = (product / ns).checked_mul(NTP_SECOND)?;
        self.phase
            .checked_add(whole.checked_add(product % ns * NTP_SECOND / ns)?)
    }

    fn set_rate(&mut self, at: Instant, rate: Rate) -> Option<()> {
        let phase = self.phase_at(at)?;
        self.at = at;
        self.phase = phase;
        self.rate = rate;
        Some(())
    }
}

pub(super) struct LocalClock {
    epoch: Instant,
    ntp: u64,
    initial_timestamp: u32,
    source: Option<(u64, Instant)>,
    projection: Option<SourceProjection>,
    hardware: HardwareWindow,
    available: bool,
    published: Cell<Option<Instant>>,
    physical: bool,
}

impl LocalClock {
    pub fn new(epoch: Instant, ntp: u64, initial_timestamp: u32) -> Self {
        Self {
            epoch,
            ntp,
            initial_timestamp,
            source: None,
            projection: None,
            hardware: HardwareWindow::default(),
            available: false,
            published: Cell::new(None),
            physical: false,
        }
    }

    pub fn observe(&mut self, position: u64, captured_at: Instant, rate: Option<Rate>) {
        // This is absolute sample time, not the application-age floor or
        // encoding/admission/Noise callback time. Later observations cannot
        // move the phase reference or contribute callback jitter to frequency.
        self.source.get_or_insert((position, captured_at));
        let qualified = match rate {
            Some(rate) => {
                self.physical = true;
                // ns is the validated hardware span, not callback uptime. A
                // short-span ratio can turn sub-ms timestamp quantization into
                // discontinuous projected RTP. Keep RR/APP feedback until the
                // source has a full long window; do not publish nominal timing.
                if rate.ticks == 0 || rate.ns == 0 {
                    self.available = false;
                    return;
                }
                let fitted = self.hardware.observe(rate);
                if u128::from(rate.ns) < WINDOW.as_nanos() {
                    self.available = false;
                    return;
                }
                // A first already-long span is usable without callback history.
                // Subsequently apply only complete hardware-window fits, not a
                // noisy endpoint ratio reprojected over the entire source age.
                fitted.or_else(|| self.projection.is_none().then_some(rate))
            }
            None if !self.physical => Some(Rate::default()), // Declared synthetic input.
            None => None, // Missing physical metadata is not a nominal clock.
        };
        self.available = !self.physical || rate.is_some();
        if let Some(rate) = qualified {
            if let Some(projection) = self.projection.as_mut() {
                if u128::from(rate.ticks) * u128::from(projection.rate.ns)
                    != u128::from(projection.rate.ticks) * u128::from(rate.ns)
                {
                    // Integrate forward at real capture time, never rewrite an
                    // already published point. Publication only bounds where a
                    // measured frequency takes effect; it does not measure it.
                    let at = captured_at
                        .max(projection.at)
                        .max(self.published.get().unwrap_or(projection.at));
                    if projection.set_rate(at, rate).is_none() {
                        self.available = false;
                    }
                }
            } else {
                self.projection = Some(SourceProjection {
                    at: self.source.unwrap().1,
                    phase: 0,
                    rate,
                });
            }
        }
    }

    pub fn report(&self, now: Instant, packets: u32, octets: u32) -> Option<SenderClock> {
        if !self.available {
            return None;
        }
        let (position, captured_at) = self.source?;
        now.checked_duration_since(captured_at)?;
        let phase = self.projection.as_ref()?.phase_at(now)?;
        let ntp_elapsed =
            now.checked_duration_since(self.epoch)?.as_nanos() * NTP_SECOND / SECOND_NS;
        let ntp = self.ntp.wrapping_add(ntp_elapsed as u64);
        // Zero means unavailable in this profile. Keep protected RR/APP at
        // that instant; do not shift the epoch or weaken the receiver's guard.
        if ntp == 0 {
            return None;
        }
        self.published
            .set(Some(self.published.get().map_or(now, |last| last.max(now))));
        Some(SenderClock {
            ntp,
            rtp: self.initial_timestamp.wrapping_add(
                (position
                    .wrapping_mul(3)
                    .wrapping_add((phase / NTP_SECOND) as u64)) as u32,
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

// Static diagnostic bits, not wire fields or an alternative acceptance policy.
// A rejection records the first failing check, matching the original OR order.
#[derive(Clone, Copy)]
#[repr(u8)]
enum Rejection {
    ZeroNtp = 0,
    SourceInterval = 1,
    RtpProgress = 2,
    PacketProgress = 3,
    ArrivalInterval = 4,
    ArrivalSourceGap = 5,
    ShortRate = 6,
    LongRate = 7,
}

impl Rejection {
    fn bit(self) -> u64 {
        1 << self as u8
    }
}

#[derive(Default)]
pub(super) struct RemoteClock {
    pub rate: Rate,
    pub calibrated: bool,
    pub rejection_mask: u64,
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

    fn freeze(&mut self, reason: Rejection) -> bool {
        // Keep the established rate and timeline. Recovery needs another full
        // healthy window; it never follows a delayed arrival baseline.
        self.first = None;
        self.healthy = None;
        self.updated_ntp = None;
        self.rejection_mask |= reason.bit();
        false
    }

    pub fn observe(&mut self, report: SenderClock, arrival: Instant) -> bool {
        if report.ntp == 0 {
            return self.freeze(Rejection::ZeroNtp);
        }
        let ticks = if let Some(previous) = self.latest {
            let delta_ntp = report.ntp.wrapping_sub(previous.report.ntp);
            let ns = u128::from(delta_ntp) * SECOND_NS / NTP_SECOND;
            let delta_ticks = report.rtp.wrapping_sub(previous.report.rtp);
            let packet_progress = report.packets.wrapping_sub(previous.report.packets);
            let arrival_gap = arrival.checked_duration_since(previous.arrival);
            // The final short-rate guard has one-tick quantization. It is a
            // discontinuity check, not the long-window frequency estimate.
            // Ordered transport can debunch fresh authenticated reports. The
            // >=50ms sampling interval belongs to source NTP, not arrival: all
            // frequency checks still use source NTP/RTP only. Authentication/
            // replay precedes observe(); arrival must still progress strictly
            // and retain the existing stale/divergence bounds.
            let rejected = if ns < 50_000_000 || ns > FRESH.as_nanos() {
                Some(Rejection::SourceInterval)
            } else if delta_ticks == 0 || delta_ticks >= 1 << 31 {
                Some(Rejection::RtpProgress)
            } else if packet_progress == 0 || packet_progress >= 1 << 31 {
                Some(Rejection::PacketProgress)
            } else if arrival_gap.is_none_or(|gap| gap.is_zero() || gap > FRESH) {
                Some(Rejection::ArrivalInterval)
            } else if arrival_gap.is_some_and(|gap| {
                gap.abs_diff(Duration::from_nanos(ns.min(u128::from(u64::MAX)) as u64))
                    > Duration::from_millis(500)
            }) {
                Some(Rejection::ArrivalSourceGap)
            } else if (i128::from(delta_ticks) * 1_000_000_000 - ns as i128 * 48_000).abs()
                * 1_000_000
                > ns as i128 * 48_000 * 2_500
            {
                Some(Rejection::ShortRate)
            } else {
                None
            };
            if let Some(reason) = rejected {
                self.latest = Some(Observation {
                    report,
                    arrival,
                    ticks: u64::from(report.rtp),
                });
                return self.freeze(reason);
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
            return self.freeze(Rejection::LongRate);
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

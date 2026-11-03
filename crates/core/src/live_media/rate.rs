use super::{config::RatePolicy, packet::LedgerObservation};
use std::time::{Duration, Instant};

/// Two codec rates, not a capacity estimate or an extension of packet deadlines.
pub(super) struct RateControl {
    adaptive: bool,
    bitrate: u32,
    healthy_since: Option<Instant>,
    last_loss: Option<(u32, u32)>,
}
impl RateControl {
    pub fn new(policy: RatePolicy) -> Self {
        Self {
            adaptive: matches!(policy, RatePolicy::Adaptive { .. }),
            bitrate: match policy {
                RatePolicy::Fixed => 12_000,
                RatePolicy::Adaptive { initial_bps } => initial_bps,
            },
            healthy_since: None,
            last_loss: None,
        }
    }
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }
    pub fn observe(
        &mut self,
        now: Instant,
        ledger: &LedgerObservation,
        control: Option<(Instant, Duration, u32, u32)>,
    ) -> u32 {
        if !self.adaptive {
            return self.bitrate;
        }
        let loss = control.map(|(_, _, late, plc)| (late, plc));
        let increased_loss = matches!((self.last_loss, loss), (Some(old), Some(new)) if old != new);
        self.last_loss = loss;
        let oldest = ledger.oldest_age.unwrap_or_default();
        if oldest >= Duration::from_millis(450)
            || ledger.bytes >= ledger.window / 2
            || increased_loss
        {
            self.bitrate = 8_000;
            self.healthy_since = None;
        } else if control.is_some_and(|(at, cycle, _, _)| {
            now.saturating_duration_since(at) < Duration::from_millis(400)
                && cycle < Duration::from_millis(350)
        }) && oldest < Duration::from_millis(350)
            && ledger.bytes < ledger.window / 4
        {
            let since = self.healthy_since.get_or_insert(now);
            if now.saturating_duration_since(*since) >= Duration::from_secs(10) {
                self.bitrate = 12_000;
            }
        } else {
            self.healthy_since = None;
        }
        self.bitrate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pressure_reduces_before_expiry_and_only_fresh_stable_feedback_restores() {
        let now = Instant::now();
        let mut ledger = super::super::packet::Ledger::new(100, 40, 600);
        ledger.commit(1, 80, now).unwrap();
        let mut rate = RateControl::new(RatePolicy::Adaptive {
            initial_bps: 12_000,
        });
        let pressure = now + Duration::from_millis(450);
        assert_eq!(
            rate.observe(pressure, &ledger.observe(pressure), None),
            8_000
        );
        ledger.acknowledge(1, pressure).unwrap();
        // Cached/stale feedback never restores quality.
        assert_eq!(
            rate.observe(
                now + Duration::from_secs(20),
                &ledger.observe(now),
                Some((pressure, Duration::from_millis(200), 0, 0))
            ),
            8_000
        );
        for sec in 21..=31 {
            let at = now + Duration::from_secs(sec);
            assert_eq!(
                rate.observe(
                    at,
                    &ledger.observe(at),
                    Some((at, Duration::from_millis(200), 0, 0))
                ),
                if sec == 31 { 12_000 } else { 8_000 }
            );
        }
        let at = now + Duration::from_secs(32);
        assert_eq!(
            rate.observe(
                at,
                &ledger.observe(at),
                Some((at, Duration::from_millis(200), 1, 0))
            ),
            8_000
        );
        assert_eq!(
            ledger.bytes(),
            0,
            "rate control never changes ledger credit"
        );
    }
    #[test]
    fn diagnostic_fixed_profile_does_not_adapt() {
        let now = Instant::now();
        let mut ledger = super::super::packet::Ledger::new(100, 40, 600);
        ledger.commit(1, 80, now).unwrap();
        let mut rate = RateControl::new(RatePolicy::Fixed);
        assert_eq!(
            rate.observe(
                now + Duration::from_secs(1),
                &ledger.observe(now + Duration::from_secs(1)),
                None
            ),
            12_000
        );
    }
}

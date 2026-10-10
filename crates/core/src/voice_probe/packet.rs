//! The pre-agreed M1V-RTCP-1 fixture profile, not generic RTP negotiation.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const FRAMING_BYTES: usize = 22;
pub const MEDIA_OVERHEAD: usize = 44;
pub const FEEDBACK_FRAMED_MAX: usize = 152;

pub fn rtp(ssrc: u32, index: u64, timestamp: u32, opus: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x80, 111];
    packet.extend_from_slice(&(index as u16).to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(opus);
    packet
}

pub fn read_rtp(packet: &[u8], ssrc: u32) -> Result<(u16, u32, &[u8]), String> {
    if packet.len() < 13
        || packet[0] != 0x80
        || packet[1] & 0x7f != 111
        || packet[8..12] != ssrc.to_be_bytes()
    {
        return Err("invalid fixture RTP header".into());
    }
    Ok((
        u16::from_be_bytes(packet[2..4].try_into().unwrap()),
        u32::from_be_bytes(packet[4..8].try_into().unwrap()),
        &packet[12..],
    ))
}

/// Single ordered source. Choose the nearest extended counter across rollover.
pub fn extend(previous: u64, low: u64, bits: u32) -> u64 {
    let modulus = 1u64 << bits;
    let mask = modulus - 1;
    let candidate = (previous & !mask) | low;
    if candidate.saturating_add(modulus / 2) < previous {
        candidate + modulus
    } else if candidate > previous.saturating_add(modulus / 2) && candidate >= modulus {
        candidate - modulus
    } else {
        candidate
    }
}

#[derive(Default, Clone, Copy)]
pub struct Feedback {
    // Internal parsed metadata, never another APP field. Read only after the
    // complete authenticated compound has passed all profile checks.
    pub sender_clock: Option<SenderClock>,
    pub terminal: Option<u64>,
    pub playout: Option<u64>,
    pub muted: bool,
    pub dtx: bool,
    pub late: u32,
    pub plc: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenderClock {
    pub ntp: u64,
    pub rtp: u32,
    pub packets: u32,
    pub octets: u32,
}

pub struct Report<'a> {
    pub sender_ssrc: u32,
    pub peer_ssrc: u32,
    pub cname: &'a [u8; 8],
    pub sender: Option<(u64, u32, u32, u32)>, // NTP, RTP time, packet count, octets
    pub feedback: Feedback,
}

pub fn compound(report: Report<'_>) -> Vec<u8> {
    let mut packet = vec![0x81, if report.sender.is_some() { 200 } else { 201 }];
    packet.extend_from_slice(&(if report.sender.is_some() { 12u16 } else { 7 }).to_be_bytes());
    packet.extend_from_slice(&report.sender_ssrc.to_be_bytes());
    if let Some((ntp, timestamp, count, bytes)) = report.sender {
        packet.extend_from_slice(&ntp.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&count.to_be_bytes());
        packet.extend_from_slice(&bytes.to_be_bytes());
    }
    // A full peer report block. Loss/jitter fields are not used as admission ACK.
    packet.extend_from_slice(&report.peer_ssrc.to_be_bytes());
    packet.extend_from_slice(&[0; 4]);
    packet.extend_from_slice(&(report.feedback.terminal.unwrap_or(0) as u32).to_be_bytes());
    packet.extend_from_slice(&[0; 12]);
    packet.extend_from_slice(&[0x81, 202, 0, 4]);
    packet.extend_from_slice(&report.sender_ssrc.to_be_bytes());
    packet.extend_from_slice(&[1, 8]);
    packet.extend_from_slice(report.cname);
    packet.extend_from_slice(&[0, 0]);
    packet.extend_from_slice(&[0x80, 204, 0, 10]);
    packet.extend_from_slice(&report.sender_ssrc.to_be_bytes());
    packet.extend_from_slice(b"M1VF");
    packet.push(1);
    packet.push(
        u8::from(report.feedback.terminal.is_some())
            | (u8::from(report.feedback.playout.is_some()) << 1)
            | (u8::from(report.feedback.muted) << 2)
            | (u8::from(report.feedback.dtx) << 3),
    );
    packet.extend_from_slice(&[0, 0]);
    packet.extend_from_slice(&report.peer_ssrc.to_be_bytes());
    packet.extend_from_slice(&report.feedback.terminal.unwrap_or(0).to_be_bytes());
    packet.extend_from_slice(&report.feedback.playout.unwrap_or(0).to_be_bytes());
    packet.extend_from_slice(&report.feedback.late.to_be_bytes());
    packet.extend_from_slice(&report.feedback.plc.to_be_bytes());
    packet
}

/// Call only AFTER whole-compound SRTCP authentication/replay checking.
pub fn read_compound(
    packet: &[u8],
    author: u32,
    reported: u32,
    cname: &[u8; 8],
) -> Result<Feedback, String> {
    let sr = packet.get(1) == Some(&200);
    let report_len = if sr { 52 } else { 32 };
    if packet.len() != report_len + 64
        || packet[0] != 0x81
        || packet[1] != if sr { 200 } else { 201 }
        || packet[2..4] != ((report_len / 4 - 1) as u16).to_be_bytes()
        || packet[4..8] != author.to_be_bytes()
    {
        return Err("invalid fixture RTCP report".into());
    }
    let block = if sr { 28 } else { 8 };
    if packet[block..block + 4] != reported.to_be_bytes() {
        return Err("invalid fixture RTCP peer".into());
    }
    let sdes = &packet[report_len..report_len + 20];
    if sdes[..4] != [0x81, 202, 0, 4]
        || sdes[4..8] != author.to_be_bytes()
        || sdes[8..10] != [1, 8]
        || sdes[10..18] != *cname
        || sdes[18..] != [0, 0]
    {
        return Err("invalid fixture RTCP CNAME".into());
    }
    let app = &packet[report_len + 20..];
    if app[..4] != [0x80, 204, 0, 10]
        || app[4..8] != author.to_be_bytes()
        || &app[8..12] != b"M1VF"
        || app[12] != 1
        || app[13] & !15 != 0
        || app[14..16] != [0, 0]
        || app[16..20] != reported.to_be_bytes()
    {
        return Err("invalid fixture RTCP APP".into());
    }
    let terminal = u64::from_be_bytes(app[20..28].try_into().unwrap());
    let playout = u64::from_be_bytes(app[28..36].try_into().unwrap());
    if terminal >> 48 != 0 || app[13] & 1 == 0 && terminal != 0 || app[13] & 2 == 0 && playout != 0
    {
        return Err("invalid fixture RTCP cursor".into());
    }
    Ok(Feedback {
        sender_clock: sr.then(|| SenderClock {
            ntp: u64::from_be_bytes(packet[8..16].try_into().unwrap()),
            rtp: u32::from_be_bytes(packet[16..20].try_into().unwrap()),
            packets: u32::from_be_bytes(packet[20..24].try_into().unwrap()),
            octets: u32::from_be_bytes(packet[24..28].try_into().unwrap()),
        }),
        terminal: (app[13] & 1 != 0).then_some(terminal),
        playout: (app[13] & 2 != 0).then_some(playout),
        muted: app[13] & 4 != 0,
        dtx: app[13] & 8 != 0,
        late: u32::from_be_bytes(app[36..40].try_into().unwrap()),
        plc: u32::from_be_bytes(app[40..44].try_into().unwrap()),
    })
}

struct Entry {
    index: u64,
    bytes: usize,
    at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressFailureReason {
    Expired,
    WindowExhausted,
}

impl ProgressFailureReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::Expired => "remote media progress expired; retire generation",
            Self::WindowExhausted => "remote media window exhausted; retire generation",
        }
    }
}

/// Read-only state at the original check instant; no new decision or clock read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerObservation {
    pub bytes: usize,
    pub window: usize,
    pub entries: usize,
    pub oldest_index: Option<u64>,
    pub oldest_age: Option<Duration>,
    pub highest: Option<u64>,
    pub terminal: Option<u64>,
    pub max_age: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerFailure {
    pub reason: ProgressFailureReason,
    pub observation: LedgerObservation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerCommitFailure {
    InvalidIndex,
    Progress(LedgerFailure),
}

impl LedgerCommitFailure {
    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidIndex => "invalid committed media index",
            Self::Progress(failure) => failure.reason.message(),
        }
    }
}

pub struct Ledger {
    entries: VecDeque<Entry>,
    bytes: usize,
    limit: usize,
    max_age: Duration,
    highest: Option<u64>,
    terminal: Option<u64>,
}

impl Ledger {
    pub fn new(packet_cap: usize, duration_ms: u16, healthy_cycle_ms: u32) -> Self {
        let maximum = packet_cap + MEDIA_OVERHEAD;
        let limit =
            maximum * (healthy_cycle_ms as usize + 120) / usize::from(duration_ms) + 2 * maximum;
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            limit,
            max_age: Duration::from_millis(u64::from(healthy_cycle_ms) + 120),
            highest: None,
            terminal: None,
        }
    }

    // Retain the original string API for callers of the standalone ledger.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn check(&self, now: Instant, next: usize) -> Result<(), String> {
        self.check_observed(now, next)
            .map_err(|failure| failure.reason.message().into())
    }

    pub fn check_observed(&self, now: Instant, next: usize) -> Result<(), LedgerFailure> {
        if self.bytes + next > self.limit {
            return Err(LedgerFailure {
                reason: ProgressFailureReason::WindowExhausted,
                observation: self.observe(now),
            });
        }
        if self
            .entries
            .front()
            .is_some_and(|e| now.saturating_duration_since(e.at) > self.max_age)
        {
            return Err(LedgerFailure {
                reason: ProgressFailureReason::Expired,
                observation: self.observe(now),
            });
        }
        Ok(())
    }

    fn observe(&self, now: Instant) -> LedgerObservation {
        LedgerObservation {
            bytes: self.bytes,
            window: self.limit,
            entries: self.entries.len(),
            oldest_index: self.entries.front().map(|entry| entry.index),
            oldest_age: self
                .entries
                .front()
                .map(|entry| now.saturating_duration_since(entry.at)),
            highest: self.highest,
            terminal: self.terminal,
            max_age: self.max_age,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn commit(&mut self, index: u64, bytes: usize, at: Instant) -> Result<(), String> {
        self.commit_observed(index, bytes, at)
            .map_err(|failure| failure.message().into())
    }

    pub fn commit_observed(
        &mut self,
        index: u64,
        bytes: usize,
        at: Instant,
    ) -> Result<(), LedgerCommitFailure> {
        if index >> 48 != 0 || self.highest.is_some_and(|old| index <= old) {
            return Err(LedgerCommitFailure::InvalidIndex);
        }
        self.check_observed(at, bytes)
            .map_err(LedgerCommitFailure::Progress)?;
        self.entries.push_back(Entry { index, bytes, at });
        self.bytes += bytes;
        self.highest = Some(index);
        Ok(())
    }

    /// Authenticated terminal frontier, never local writes or playout cursor.
    pub fn acknowledge(&mut self, index: u64, now: Instant) -> Result<Duration, String> {
        if self.highest.is_none_or(|high| index > high)
            || self.terminal.is_some_and(|old| index < old)
        {
            return Err("invalid remote terminal frontier".into());
        }
        let mut cycle = Duration::ZERO;
        while self.entries.front().is_some_and(|e| e.index <= index) {
            let e = self.entries.pop_front().unwrap();
            cycle = cycle.max(now.saturating_duration_since(e.at));
            self.bytes -= e.bytes;
        }
        self.terminal = Some(index);
        Ok(cycle)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Already registered commitments; pending receipts do not extend this frontier.
    pub fn highest_committed(&self) -> Option<u64> {
        self.highest
    }
}

pub struct Budget {
    media_rate_bps: u128,
    media_credit: u128,
    media_burst: u128,
    feedback_credit: u128,
    at: Instant,
}

impl Budget {
    // Bit-nanosecond credit: one byte costs exactly this many units. Integral
    // credit avoids an exact 200ms report becoming 151.999999... bytes after
    // repeated small timer refills and being incorrectly denied.
    const BYTE_CREDIT: u128 = 8_000_000_000;
    const FEEDBACK_RATE_BPS: u128 = FEEDBACK_FRAMED_MAX as u128 * 8 * 5;
    const FEEDBACK_BURST: u128 = FEEDBACK_FRAMED_MAX as u128 * Self::BYTE_CREDIT;

    pub fn new(capacity_bps: u32, maximum_frame: usize, now: Instant) -> Self {
        // The fixed 200ms protected feedback cadence has priority over media.
        // Total refill and burst remain the original 75% admission envelope;
        // media cannot consume the bytes reserved for authenticated progress.
        let media_burst = maximum_frame as u128 * Self::BYTE_CREDIT;
        Self {
            media_rate_bps: (u128::from(capacity_bps) * 3 / 4)
                .saturating_sub(Self::FEEDBACK_RATE_BPS),
            media_credit: media_burst,
            media_burst,
            feedback_credit: Self::FEEDBACK_BURST,
            at: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.at).as_nanos();
        self.media_credit = self
            .media_credit
            .saturating_add(elapsed.saturating_mul(self.media_rate_bps))
            .min(self.media_burst);
        self.feedback_credit = self
            .feedback_credit
            .saturating_add(elapsed.saturating_mul(Self::FEEDBACK_RATE_BPS))
            .min(Self::FEEDBACK_BURST);
        self.at = self.at.max(now);
    }

    pub fn admit_media(&mut self, bytes: usize, now: Instant) -> bool {
        self.refill(now);
        let cost = bytes as u128 * Self::BYTE_CREDIT;
        if self.media_credit < cost {
            return false;
        }
        self.media_credit -= cost;
        true
    }

    /// Earliest credit availability for the single waiting media packet. This
    /// only refills existing credit; it neither reserves bytes nor grows burst.
    pub fn media_ready_at(&mut self, bytes: usize, now: Instant) -> Option<Instant> {
        self.refill(now);
        let cost = bytes as u128 * Self::BYTE_CREDIT;
        if cost > self.media_burst {
            return None;
        }
        if cost <= self.media_credit {
            return Some(now);
        }
        if self.media_rate_bps == 0 {
            return None;
        }
        let deficit = cost - self.media_credit;
        let nanos = deficit.div_ceil(self.media_rate_bps);
        now.checked_add(Duration::from_nanos(u64::try_from(nanos).ok()?))
    }

    pub fn admit_feedback(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.feedback_credit < Self::FEEDBACK_BURST {
            return false;
        }
        self.feedback_credit -= Self::FEEDBACK_BURST;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_compounds_and_authenticated_credit_semantics() {
        let mut sender = dmsg_srtp_sys::Sender::new(&[42; 30], 7).unwrap();
        let mut receiver = dmsg_srtp_sys::Receiver::new(&[42; 30], 7).unwrap();
        for (sr, expected) in [(true, 152), (false, 132)] {
            let plain = compound(Report {
                sender_ssrc: 7,
                peer_ssrc: 8,
                cname: b"test0001",
                sender: sr.then_some((0, 0, 1, 60)),
                feedback: Feedback {
                    terminal: Some(65537),
                    playout: Some(0x1_0000_0010),
                    ..Feedback::default()
                },
            });
            let protected = sender.protect_rtcp(&plain).unwrap();
            assert_eq!(protected.len() + FRAMING_BYTES, expected);
            let decoded = receiver.unprotect_rtcp(&protected).unwrap();
            let report = read_compound(&decoded, 7, 8, b"test0001").unwrap();
            assert_eq!(report.terminal, Some(65537));
            assert_eq!(report.playout, Some(0x1_0000_0010));
            assert_eq!(
                report.sender_clock,
                sr.then_some(SenderClock {
                    ntp: 0,
                    rtp: 0,
                    packets: 1,
                    octets: 60
                })
            );
            assert!(read_compound(&decoded, 7, 9, b"test0001").is_err());
            for len in 0..decoded.len() {
                assert!(read_compound(&decoded[..len], 7, 8, b"test0001").is_err());
            }
        }
        let now = Instant::now();
        let mut ledger = Ledger::new(100, 40, 600);
        ledger.commit(65535, 144, now).unwrap();
        ledger.commit(65536, 104, now).unwrap();
        assert_eq!(ledger.bytes(), 248); // local commit never releases credit
        assert!(ledger.acknowledge(65537, now).is_err());
        ledger
            .acknowledge(65536, now + Duration::from_millis(600))
            .unwrap();
        assert_eq!(ledger.bytes(), 0); // one ordered path also retires middle drops
        ledger.commit(65537, 104, now).unwrap();
        assert!(ledger.check(now + Duration::from_millis(721), 0).is_err());
        assert!(ledger.acknowledge(65535, now).is_err());
    }

    #[test]
    fn sender_clock_metadata_requires_authenticated_whole_profile() {
        let mut sender = dmsg_srtp_sys::Sender::new(&[42; 30], 7).unwrap();
        let mut receiver = dmsg_srtp_sys::Receiver::new(&[42; 30], 7).unwrap();
        let plain = compound(Report {
            sender_ssrc: 7,
            peer_ssrc: 8,
            cname: b"test0001",
            sender: Some((1 << 32, 960_000, 500, 30_000)),
            feedback: Feedback::default(),
        });
        let cipher = sender.protect_rtcp(&plain).unwrap();
        let mut corrupt = cipher.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(receiver.unprotect_rtcp(&corrupt).is_err());
        let decoded = receiver.unprotect_rtcp(&cipher).unwrap();
        assert_eq!(
            read_compound(&decoded, 7, 8, b"test0001")
                .unwrap()
                .sender_clock
                .unwrap()
                .rtp,
            960_000
        );
        // Even a correctly authenticated SR prefix is insufficient: CNAME and
        // APP must pass before any typed sender timing metadata is returned.
        for offset in [52 + 10, 52 + 20 + 12, 52 + 20 + 16] {
            let mut malformed = plain.clone();
            malformed[offset] ^= 1;
            let cipher = sender.protect_rtcp(&malformed).unwrap();
            let decoded = receiver.unprotect_rtcp(&cipher).unwrap();
            assert!(read_compound(&decoded, 7, 8, b"test0001").is_err());
        }
    }

    #[test]
    fn bounded_window_budget_and_both_rollovers() {
        let now = Instant::now();
        let mut ledger = Ledger::new(100, 40, 600);
        assert_eq!(ledger.limit, 2880);
        for index in 0..20 {
            ledger.commit(index, 144, now).unwrap();
        }
        assert!(ledger.commit(20, 1, now).is_err());
        let mut budget = Budget::new(50_000, 144, now);
        assert!(budget.admit_feedback(now));
        assert!(budget.admit_media(144, now));
        assert!(!budget.admit_media(1, now));
        assert!(!budget.admit_feedback(now));
        assert!(budget.admit_media(144, now + Duration::from_secs(10)));
        assert!(!budget.admit_media(153, now + Duration::from_secs(10))); // no accumulated seconds of credit
        assert_eq!(extend(65535, 0, 16), 65536);
        assert_eq!(extend(65536, 65535, 16), 65535);
        assert_eq!(extend(u64::from(u32::MAX), 1919, 32), (1 << 32) + 1919);
        assert_eq!(
            extend((1 << 32) + 1919, u64::from(u32::MAX), 32),
            u64::from(u32::MAX)
        );
    }

    #[test]
    fn ledger_failure_observation_is_read_only_and_preserves_check_precedence() {
        let now = Instant::now();
        let mut ledger = Ledger::new(100, 40, 600);
        ledger.commit(7, 144, now).unwrap();
        ledger
            .acknowledge(6, now + Duration::from_millis(100))
            .unwrap();
        ledger
            .commit(8, 100, now + Duration::from_millis(700))
            .unwrap();
        assert!(ledger
            .check_observed(now + Duration::from_millis(720), 0)
            .is_ok());
        let failed_at = now + Duration::from_millis(721);
        let expired = ledger.check_observed(failed_at, 0).unwrap_err();
        assert_eq!(expired.reason, ProgressFailureReason::Expired);
        assert_eq!(
            expired.observation,
            LedgerObservation {
                bytes: 244,
                window: 2880,
                entries: 2,
                oldest_index: Some(7),
                oldest_age: Some(Duration::from_millis(721)),
                highest: Some(8),
                terminal: Some(6),
                max_age: Duration::from_millis(720),
            }
        );
        assert_eq!(
            ledger.check(failed_at, 0),
            Err(expired.reason.message().into())
        );
        let window = ledger.check_observed(failed_at, 2637).unwrap_err();
        assert_eq!(window.reason, ProgressFailureReason::WindowExhausted);
        assert_eq!(window.observation, expired.observation);
        assert_eq!(
            ledger.check(failed_at, 2637),
            Err(window.reason.message().into())
        );
        assert_eq!(
            (
                ledger.bytes,
                ledger.entries.len(),
                ledger.highest,
                ledger.terminal
            ),
            (244, 2, Some(8), Some(6))
        );
        ledger.acknowledge(7, failed_at).unwrap();
        assert!(ledger.check_observed(failed_at, 0).is_ok());
        assert_eq!(expired.observation.oldest_index, Some(7));
        assert_eq!((ledger.bytes, ledger.entries.len()), (100, 1));
    }

    #[test]
    fn ledger_commit_observation_preserves_index_priority_and_original_instant() {
        let origin = Instant::now();
        let at = |ms| origin + Duration::from_millis(ms);
        let mut ledger = Ledger::new(100, 40, 600);
        ledger.commit(7, 144, origin).unwrap();
        ledger.acknowledge(6, at(200)).unwrap();
        assert_eq!(ledger.commit_observed(8, 144, at(720)), Ok(()));
        let expired = ledger.commit_observed(9, 144, at(721)).unwrap_err();
        let LedgerCommitFailure::Progress(failure) = &expired else {
            panic!("expected the original age failure");
        };
        assert_eq!(failure.reason, ProgressFailureReason::Expired);
        assert_eq!(failure.observation.oldest_index, Some(7));
        assert_eq!(
            failure.observation.oldest_age,
            Some(Duration::from_millis(721))
        );
        assert_eq!(failure.observation.bytes, 288);
        assert_eq!(failure.observation.highest, Some(8));
        assert_eq!(failure.observation.terminal, Some(6));
        assert_eq!(
            ledger.commit(9, 144, at(721)),
            Err(expired.message().into())
        );
        for invalid_index in [8, 1 << 48] {
            assert_eq!(
                ledger.commit_observed(invalid_index, 3000, at(721)),
                Err(LedgerCommitFailure::InvalidIndex)
            );
            assert_eq!(
                ledger.commit(invalid_index, 3000, at(721)),
                Err("invalid committed media index".into())
            );
        }
        let window = ledger.commit_observed(9, 2593, at(721)).unwrap_err();
        let LedgerCommitFailure::Progress(window_failure) = &window else {
            panic!("expected the original byte failure");
        };
        assert_eq!(
            window_failure.reason,
            ProgressFailureReason::WindowExhausted
        );
        assert_eq!(window_failure.observation, failure.observation);
        assert_eq!(
            ledger.commit(9, 2593, at(721)),
            Err(window.message().into())
        );
        assert_eq!(
            (
                ledger.bytes,
                ledger.entries.len(),
                ledger.highest,
                ledger.terminal
            ),
            (288, 2, Some(8), Some(6))
        );
        ledger.acknowledge(7, at(721)).unwrap();
        assert_eq!(ledger.commit_observed(9, 144, at(721)), Ok(()));
        assert_eq!(failure.observation.oldest_index, Some(7));
        assert_eq!(ledger.highest, Some(9));
    }

    #[test]
    fn feedback_cannot_be_starved_by_continuous_media_admission() {
        let start = Instant::now();
        let mut budget = Budget::new(50_000, 94, start);
        for millis in (0..2_000).step_by(20) {
            let now = start + Duration::from_millis(millis);
            // Media may be dropped under an over-budget peak envelope; the
            // authenticated progress report must retain its priority instead.
            let _ = budget.admit_media(94, now);
            if millis % 200 == 0 {
                assert!(budget.admit_feedback(now), "feedback starved at {millis}ms");
            }
        }
    }

    #[test]
    fn feedback_reservation_preserves_total_rate_burst_and_profile_budget() {
        for (capacity, frame_ms, size) in [(50_000, 40, 144), (50_000, 60, 194), (80_000, 20, 94)] {
            let start = Instant::now();
            let mut budget = Budget::new(capacity, size, start);
            let mut admitted = 0usize;
            for millis in (0..2_000).step_by(10) {
                let now = start + Duration::from_millis(millis);
                if millis % frame_ms == 0 {
                    assert!(budget.admit_media(size, now));
                    admitted += size;
                }
                if millis % 200 == 0 {
                    assert!(budget.admit_feedback(now));
                    admitted += FEEDBACK_FRAMED_MAX;
                }
                let allowance = (size + FEEDBACK_FRAMED_MAX) as f64
                    + f64::from(capacity) * 0.75 / 8.0 * millis as f64 / 1_000.0;
                assert!(admitted as f64 <= allowance);
            }
        }
    }

    #[test]
    fn timely_twenty_ms_burst_needs_credit_wait_not_a_larger_bucket() {
        let start = Instant::now();
        let mut budget = Budget::new(50_000, 94, start);
        // 30-byte Opus + 44 framing: 29.6kbit/s media + 6.08 feedback,
        // less than 37.5kbit/s. A lawful batched handoff is not sustained excess.
        assert_eq!(74 * 8 * 50 + 152 * 8 * 5, 35_680);
        assert!(budget.admit_feedback(start));
        assert!(budget.admit_media(74, start));
        let burst = start + Duration::from_millis(1);
        assert!(!budget.admit_media(74, burst)); // Existing endpoint dropped here.
        let ready = budget.media_ready_at(74, burst).unwrap();
        assert_eq!(ready, start + Duration::from_nanos(13_749_205));
        assert!(!budget.admit_media(74, ready - Duration::from_nanos(1)));
        assert!(budget.admit_media(74, ready));
        assert_eq!(budget.media_ready_at(95, ready), None);
        assert!(budget.admit_feedback(start + Duration::from_millis(200)));
    }
}

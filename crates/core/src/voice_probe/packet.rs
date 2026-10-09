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
    pub terminal: Option<u64>,
    pub playout: Option<u64>,
    pub muted: bool,
    pub dtx: bool,
    pub late: u32,
    pub plc: u32,
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

    pub fn check(&self, now: Instant, next: usize) -> Result<(), String> {
        if self.bytes + next > self.limit {
            return Err("remote media window exhausted; retire generation".into());
        }
        if self
            .entries
            .front()
            .is_some_and(|e| now.saturating_duration_since(e.at) > self.max_age)
        {
            return Err("remote media progress expired; retire generation".into());
        }
        Ok(())
    }

    pub fn commit(&mut self, index: u64, bytes: usize, at: Instant) -> Result<(), String> {
        if index >> 48 != 0 || self.highest.is_some_and(|old| index <= old) {
            return Err("invalid committed media index".into());
        }
        self.check(at, bytes)?;
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
}

pub struct Budget {
    rate_bytes: f64,
    tokens: f64,
    burst: f64,
    at: Instant,
}

impl Budget {
    pub fn new(capacity_bps: u32, maximum_frame: usize, now: Instant) -> Self {
        let burst = (maximum_frame + FEEDBACK_FRAMED_MAX) as f64;
        Self {
            rate_bytes: f64::from(capacity_bps) * 0.75 / 8.0,
            tokens: burst,
            burst,
            at: now,
        }
    }

    pub fn admit(&mut self, bytes: usize, now: Instant) -> bool {
        self.tokens = (self.tokens
            + now.saturating_duration_since(self.at).as_secs_f64() * self.rate_bytes)
            .min(self.burst);
        self.at = now;
        if self.tokens < bytes as f64 {
            return false;
        }
        self.tokens -= bytes as f64;
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
    fn bounded_window_budget_and_both_rollovers() {
        let now = Instant::now();
        let mut ledger = Ledger::new(100, 40, 600);
        assert_eq!(ledger.limit, 2880);
        for index in 0..20 {
            ledger.commit(index, 144, now).unwrap();
        }
        assert!(ledger.commit(20, 1, now).is_err());
        let mut budget = Budget::new(50_000, 144, now);
        assert!(budget.admit(152, now));
        assert!(budget.admit(144, now));
        assert!(!budget.admit(1, now));
        assert!(budget.admit(144, now + Duration::from_secs(10)));
        assert!(!budget.admit(153, now + Duration::from_secs(10))); // no accumulated seconds of credit
        assert_eq!(extend(65535, 0, 16), 65536);
        assert_eq!(extend(65536, 65535, 16), 65535);
        assert_eq!(extend(u64::from(u32::MAX), 1919, 32), (1 << 32) + 1919);
        assert_eq!(
            extend((1 << 32) + 1919, u64::from(u32::MAX), 32),
            u64::from(u32::MAX)
        );
    }
}

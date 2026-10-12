//! Volatile account-authenticated calls. Keys exist only inside an Olm envelope.
use zeroize::Zeroize;

pub const VERSION: u8 = 1;
pub const PROFILE: u8 = 1; // mono16k,12kbit,40ms,100B Opus cap
pub const CIPHERTEXT_MAX: usize = 2048;
pub const RTP_CAP: usize = 122;
pub const SRTCP_CAP: usize = 130;
pub const OP_RTP: u8 = 242;
pub const OP_SRTCP: u8 = 243;
pub const RING_SECS: u64 = 30;
pub const ATTACH_SECS: u64 = 10;
pub const HEARTBEAT_SECS: u64 = 10;
pub const LIVENESS_SECS: u64 = 30;
pub const MAX_CALLS: usize = 2;
pub const MAX_LISTENERS: usize = 16;
pub const EVENT_CAP: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Action {
    Invite = 1,
    Ringing = 2,
    Accept = 3,
    Reject = 4,
    End = 5,
    Heartbeat = 6,
}
impl Action {
    fn parse(value: u8) -> Result<Self, &'static str> {
        match value {
            1 => Ok(Self::Invite),
            2 => Ok(Self::Ringing),
            3 => Ok(Self::Accept),
            4 => Ok(Self::Reject),
            5 => Ok(Self::End),
            6 => Ok(Self::Heartbeat),
            _ => Err("invalid call action"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Reason {
    Ok = 0,
    Unavailable = 1,
    Busy = 2,
    Expired = 3,
    Rejected = 4,
    LocalEnded = 5,
    RemoteEnded = 6,
    ConnectionLost = 7,
    Invalid = 8,
    Revoked = 9,
    AudioUnavailable = 10,
}
impl Reason {
    fn parse(value: u8) -> Result<Self, &'static str> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Unavailable),
            2 => Ok(Self::Busy),
            3 => Ok(Self::Expired),
            4 => Ok(Self::Rejected),
            5 => Ok(Self::LocalEnded),
            6 => Ok(Self::RemoteEnded),
            7 => Ok(Self::ConnectionLost),
            8 => Ok(Self::Invalid),
            9 => Ok(Self::Revoked),
            10 => Ok(Self::AudioUnavailable),
            _ => Err("invalid call reason"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub request_id: u32,
    pub action: Action,
    pub call_id: [u8; 16],
    pub epoch: u64,
    pub recipient: [u8; 16],
    pub ciphertext: Vec<u8>,
}
impl Command {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request_id == 0 || self.ciphertext.len() > CIPHERTEXT_MAX {
            return Err("invalid call command");
        }
        if self.action == Action::Heartbeat {
            return if self.call_id == [0; 16]
                && self.epoch == 0
                && self.recipient == [0; 16]
                && self.ciphertext.is_empty()
            {
                Ok(())
            } else {
                Err("invalid call heartbeat")
            };
        }
        identity(&self.call_id, self.epoch)?;
        let encrypted = matches!(self.action, Action::Invite | Action::Accept);
        if encrypted == self.ciphertext.is_empty()
            || ((self.action == Action::Invite) == (self.recipient == [0; 16]))
        {
            return Err("invalid call command body");
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let mut p = Vec::with_capacity(47 + self.ciphertext.len());
        p.extend_from_slice(&self.request_id.to_be_bytes());
        p.push(self.action as u8);
        p.extend_from_slice(&self.call_id);
        p.extend_from_slice(&self.epoch.to_be_bytes());
        p.extend_from_slice(&self.recipient);
        bytes(&mut p, &self.ciphertext);
        Ok(p)
    }
    pub fn decode(p: &[u8]) -> Result<Self, &'static str> {
        let mut r = Reader(p);
        let value = Self {
            request_id: r.u32()?,
            action: Action::parse(r.byte()?)?,
            call_id: r.array()?,
            epoch: r.u64()?,
            recipient: r.array()?,
            ciphertext: r.ciphertext()?,
        };
        r.end()?;
        value.validate()?;
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub request_id: u32,
    pub call_id: [u8; 16],
    pub epoch: u64,
    pub reason: Reason,
}
impl Ack {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(29);
        p.extend_from_slice(&self.request_id.to_be_bytes());
        p.extend_from_slice(&self.call_id);
        p.extend_from_slice(&self.epoch.to_be_bytes());
        p.push(self.reason as u8);
        p
    }
    pub fn decode(p: &[u8]) -> Result<Self, &'static str> {
        let mut r = Reader(p);
        let value = Self {
            request_id: r.u32()?,
            call_id: r.array()?,
            epoch: r.u64()?,
            reason: Reason::parse(r.byte()?)?,
        };
        r.end()?;
        if value.request_id == 0 || (value.call_id == [0; 16] && value.epoch != 0) {
            return Err("invalid call ack");
        }
        if value.call_id != [0; 16] {
            identity(&value.call_id, value.epoch)?;
        }
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EventKind {
    Invite = 1,
    Ringing = 2,
    Accepted = 3,
    MediaReady = 4,
    Ended = 5,
}
impl EventKind {
    fn parse(value: u8) -> Result<Self, &'static str> {
        match value {
            1 => Ok(Self::Invite),
            2 => Ok(Self::Ringing),
            3 => Ok(Self::Accepted),
            4 => Ok(Self::MediaReady),
            5 => Ok(Self::Ended),
            _ => Err("invalid call event"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub call_id: [u8; 16],
    pub epoch: u64,
    pub peer_user: [u8; 16],
    pub peer_device: [u8; 32],
    pub remaining_ms: u32,
    pub reason: Reason,
    pub ciphertext: Vec<u8>,
}
impl Event {
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let mut p = Vec::with_capacity(80 + self.ciphertext.len());
        p.push(self.kind as u8);
        p.extend_from_slice(&self.call_id);
        p.extend_from_slice(&self.epoch.to_be_bytes());
        p.extend_from_slice(&self.peer_user);
        p.extend_from_slice(&self.peer_device);
        p.extend_from_slice(&self.remaining_ms.to_be_bytes());
        p.push(self.reason as u8);
        bytes(&mut p, &self.ciphertext);
        Ok(p)
    }
    fn validate(&self) -> Result<(), &'static str> {
        identity(&self.call_id, self.epoch)?;
        if self.peer_user == [0; 16]
            || self.peer_device == [0; 32]
            || self.remaining_ms > 30_000
            || self.ciphertext.len() > CIPHERTEXT_MAX
        {
            return Err("invalid call event");
        }
        if matches!(self.kind, EventKind::Invite | EventKind::Accepted)
            == self.ciphertext.is_empty()
            || (self.kind != EventKind::Ended && self.reason != Reason::Ok)
        {
            return Err("invalid call event body");
        }
        Ok(())
    }
    pub fn decode(p: &[u8]) -> Result<Self, &'static str> {
        let mut r = Reader(p);
        let value = Self {
            kind: EventKind::parse(r.byte()?)?,
            call_id: r.array()?,
            epoch: r.u64()?,
            peer_user: r.array()?,
            peer_device: r.array()?,
            remaining_ms: r.u32()?,
            reason: Reason::parse(r.byte()?)?,
            ciphertext: r.ciphertext()?,
        };
        r.end()?;
        value.validate()?;
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LaneKind {
    Rtp = 0,
    Srtcp = 1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attach {
    pub call_id: [u8; 16],
    pub epoch: u64,
    pub lane: LaneKind,
}
impl Attach {
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        identity(&self.call_id, self.epoch)?;
        let mut p = Vec::with_capacity(25);
        p.extend_from_slice(&self.call_id);
        p.extend_from_slice(&self.epoch.to_be_bytes());
        p.push(self.lane as u8);
        Ok(p)
    }
    pub fn decode(p: &[u8]) -> Result<Self, &'static str> {
        let mut r = Reader(p);
        let call_id = r.array()?;
        let epoch = r.u64()?;
        let lane = match r.byte()? {
            0 => LaneKind::Rtp,
            1 => LaneKind::Srtcp,
            _ => return Err("invalid call lane"),
        };
        r.end()?;
        identity(&call_id, epoch)?;
        Ok(Self {
            call_id,
            epoch,
            lane,
        })
    }
}

/// Only the peer decrypts this strict E2Ev2 kind5 envelope. Never Debug/Serialize keys.
pub struct KeyEnvelope {
    pub call_id: [u8; 16],
    pub sender_ed: [u8; 32],
    pub action: Action,
    pub epoch: u64,
    pub sender_binding: [u8; 32],
    pub recipient_binding: [u8; 32],
    pub key: [u8; 30],
    pub ssrc: u32,
    pub cname: [u8; 8],
    pub sequence: u16,
    pub timestamp: u32,
}
impl Drop for KeyEnvelope {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
impl KeyEnvelope {
    fn validate(&self) -> Result<(), &'static str> {
        identity(&self.call_id, self.epoch)?;
        if !matches!(self.action, Action::Invite | Action::Accept)
            || self.sender_ed == [0; 32]
            || self.sender_binding == [0; 32]
            || self.recipient_binding == [0; 32]
            || self.sender_binding == self.recipient_binding
            || self.key == [0; 30]
            || self.ssrc == 0
            || !self.cname.iter().all(u8::is_ascii_alphanumeric)
        {
            return Err("invalid call key envelope");
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let mut p = Vec::with_capacity(173);
        p.extend_from_slice(&[2, 5]);
        p.extend_from_slice(&self.call_id);
        p.extend_from_slice(&self.sender_ed);
        p.push(0); // no Reply
        p.push(self.action as u8);
        p.extend_from_slice(&self.epoch.to_be_bytes());
        p.extend_from_slice(&self.sender_binding);
        p.extend_from_slice(&self.recipient_binding);
        p.extend_from_slice(&self.key);
        p.extend_from_slice(&self.ssrc.to_be_bytes());
        p.extend_from_slice(&self.cname);
        p.extend_from_slice(&self.sequence.to_be_bytes());
        p.extend_from_slice(&self.timestamp.to_be_bytes());
        p.push(PROFILE);
        Ok(p)
    }
    pub fn decode(p: &[u8]) -> Result<Self, &'static str> {
        let mut r = Reader(p);
        if r.array::<2>()? != [2, 5] {
            return Err("invalid call envelope version/kind");
        }
        let call_id = r.array()?;
        let sender_ed = r.array()?;
        if r.byte()? != 0 {
            return Err("call envelope cannot reply");
        }
        let value = Self {
            call_id,
            sender_ed,
            action: Action::parse(r.byte()?)?,
            epoch: r.u64()?,
            sender_binding: r.array()?,
            recipient_binding: r.array()?,
            key: r.array()?,
            ssrc: r.u32()?,
            cname: r.array()?,
            sequence: u16::from_be_bytes(r.array()?),
            timestamp: r.u32()?,
        };
        if r.byte()? != PROFILE {
            return Err("unsupported call profile");
        }
        r.end()?;
        value.validate()?;
        Ok(value)
    }
}

fn identity(id: &[u8; 16], epoch: u64) -> Result<(), &'static str> {
    if *id == [0; 16] || epoch != 1 {
        Err("invalid call identity/epoch")
    } else {
        Ok(())
    }
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        let value = self
            .0
            .get(..N)
            .ok_or("short call payload")?
            .try_into()
            .map_err(|_| "short call payload")?;
        self.0 = &self.0[N..];
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.array::<1>()?[0])
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn ciphertext(&mut self) -> Result<Vec<u8>, &'static str> {
        let len = u16::from_be_bytes(self.array()?) as usize;
        if len > CIPHERTEXT_MAX || self.0.len() != len {
            return Err("invalid call ciphertext length");
        }
        let value = self.0.to_vec();
        self.0 = &[];
        Ok(value)
    }
    fn end(self) -> Result<(), &'static str> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("trailing call payload")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_signal_and_attach_vectors() {
        let command = Command {
            request_id: 7,
            action: Action::Invite,
            call_id: [1; 16],
            epoch: 1,
            recipient: [2; 16],
            ciphertext: vec![3; 32],
        };
        let p = command.encode().unwrap();
        assert_eq!(&p[..5], &[0, 0, 0, 7, 1]);
        assert_eq!(Command::decode(&p).unwrap(), command);
        for n in 0..p.len() {
            assert!(Command::decode(&p[..n]).is_err());
        }
        let mut extra = p;
        extra.push(0);
        assert!(Command::decode(&extra).is_err());
        let event = Event {
            kind: EventKind::Accepted,
            call_id: [1; 16],
            epoch: 1,
            peer_user: [2; 16],
            peer_device: [3; 32],
            remaining_ms: 10_000,
            reason: Reason::Ok,
            ciphertext: vec![4],
        };
        assert_eq!(Event::decode(&event.encode().unwrap()).unwrap(), event);
        let attach = Attach {
            call_id: [1; 16],
            epoch: 1,
            lane: LaneKind::Srtcp,
        };
        assert_eq!(Attach::decode(&attach.encode().unwrap()).unwrap(), attach);
        let ack = Ack {
            request_id: 1,
            call_id: [1; 16],
            epoch: 1,
            reason: Reason::Busy,
        };
        assert_eq!(Ack::decode(&ack.encode()).unwrap(), ack);
        let heartbeat = Command {
            request_id: 1,
            action: Action::Heartbeat,
            call_id: [0; 16],
            epoch: 0,
            recipient: [0; 16],
            ciphertext: vec![],
        };
        assert_eq!(
            Command::decode(&heartbeat.encode().unwrap()).unwrap(),
            heartbeat
        );
    }
    #[test]
    fn keys_are_separate_from_message_history_and_profile_is_strict() {
        let envelope = KeyEnvelope {
            call_id: [1; 16],
            sender_ed: [2; 32],
            action: Action::Invite,
            epoch: 1,
            sender_binding: [3; 32],
            recipient_binding: [4; 32],
            key: [5; 30],
            ssrc: 6,
            cname: *b"abcdefgh",
            sequence: 7,
            timestamp: 8,
        };
        let p = envelope.encode().unwrap();
        assert_eq!(p.len(), 173);
        let decoded = KeyEnvelope::decode(&p).unwrap();
        assert_eq!(decoded.key, [5; 30]);
        assert!(crate::e2e::decode(&p).is_err());
        for n in 0..p.len() {
            assert!(KeyEnvelope::decode(&p[..n]).is_err());
        }
        let mut bad = p;
        *bad.last_mut().unwrap() = 2;
        assert!(KeyEnvelope::decode(&bad).is_err());
    }
}

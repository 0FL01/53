//! Bounded volatile call hub. Only peer-encrypted signal bodies and SRTP packets.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dmsg_protocol::{
    call::*, ERR_BUSY, ERR_EXPIRED, ERR_REVOKED, OP_CALL_ACK, OP_CALL_ATTACHED, OP_CALL_COMMAND,
    OP_CALL_EVENT, OP_CALL_READY, OP_ERROR,
};
use rusqlite::{Connection, OptionalExtension};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, OwnedSemaphorePermit};

use crate::call_lane::{self, Frame, Stop};
use crate::{SessionGuard, State};

const HOP: Duration = Duration::from_millis(20);

#[derive(Default)]
struct RelayCounters {
    queued: u64,
    full: u64,
    route_expired: u64,
    last_srtcp_full_index: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub user: [u8; 16],
    pub device: [u8; 32],
}

struct Listener {
    who: Endpoint,
    serial: u64,
    events: mpsc::Sender<Frame>,
    stop: Arc<Stop>,
    last: Instant,
}

struct Slot {
    serial: u64,
    outgoing: mpsc::Sender<Frame>,
    stop: Arc<Stop>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Invited,
    Ringing,
    Attaching,
    Active,
}

struct Call {
    epoch: u64,
    peers: [Endpoint; 2],
    stage: Stage,
    deadline: Option<Instant>,
    recovery_deadline: Option<Instant>,
    rekeys: [Option<Vec<u8>>; 2],
    recovered: [bool; 2],
    ready_at: Option<Instant>,
    lanes: [Option<Slot>; 4],
}

impl Call {
    fn side(&self, who: Endpoint) -> Option<usize> {
        self.peers.iter().position(|p| *p == who)
    }

    fn event(
        &self,
        id: [u8; 16],
        side: usize,
        kind: EventKind,
        reason: Reason,
        ciphertext: Vec<u8>,
        now: Instant,
    ) -> Event {
        let peer = self.peers[1 - side];
        Event {
            kind,
            call_id: id,
            epoch: self.epoch,
            peer_user: peer.user,
            peer_device: peer.device,
            remaining_ms: self
                .recovery_deadline
                .or(self.deadline)
                .map(|d| d.saturating_duration_since(now).as_millis().min(30_000) as u32)
                .unwrap_or(0),
            reason,
            ciphertext,
        }
    }
}

#[derive(Default)]
pub struct Hub {
    listeners: HashMap<[u8; 32], Listener>,
    calls: HashMap<[u8; 16], Call>,
    next_serial: u64,
}

impl Hub {
    fn serial(&mut self) -> u64 {
        self.next_serial += 1;
        self.next_serial
    }

    pub fn busy(&self, device: &[u8; 32]) -> bool {
        self.calls
            .values()
            .any(|c| c.peers.iter().any(|p| &p.device == device))
    }

    fn listen(
        &mut self,
        who: Endpoint,
        events: mpsc::Sender<Frame>,
        stop: Arc<Stop>,
        now: Instant,
    ) -> Result<u64, u8> {
        self.expire(now);
        if self.listeners.len() >= MAX_LISTENERS || self.listeners.contains_key(&who.device) {
            return Err(ERR_BUSY);
        }
        let serial = self.serial();
        self.listeners.insert(
            who.device,
            Listener {
                who,
                serial,
                events,
                stop,
                last: now,
            },
        );
        Ok(serial)
    }

    fn live(&self, who: Endpoint, serial: u64) -> bool {
        self.listeners
            .get(&who.device)
            .is_some_and(|l| l.who == who && l.serial == serial && !l.stop.is_closed())
    }

    fn open(
        &mut self,
        db: &Connection,
        who: Endpoint,
        open: Open,
        events: mpsc::Sender<Frame>,
        stop: Arc<Stop>,
        now: Instant,
    ) -> Result<u64, u8> {
        self.expire(now);
        if !current(db, who) {
            self.close_device(&who.device, Reason::Revoked);
            return Err(ERR_REVOKED);
        }
        let Some((id, epoch)) = open.resume else {
            return self.listen(who, events, stop, now);
        };
        let call = self.calls.get(&id).ok_or(ERR_EXPIRED)?;
        let side = call
            .side(who)
            .filter(|_| epoch > 0 && epoch <= call.epoch)
            .ok_or(ERR_BUSY)?;
        if !allowed(db, call.peers[0], call.peers[1]) {
            self.end(&id, Reason::Unavailable, None, now);
            return Err(ERR_EXPIRED);
        }
        if !matches!(call.stage, Stage::Attaching | Stage::Active) {
            self.end(&id, Reason::ConnectionLost, None, now);
            return Err(ERR_EXPIRED);
        }
        let begin = call.recovery_deadline.is_none()
            || (epoch == call.epoch
                && (call.stage == Stage::Active || call.lanes.iter().any(Option::is_some)));
        // Stop/remove before inserting: the old owner's Drop cannot detach this
        // replacement, even when it runs after the new stream starts receiving.
        self.remove_listener(&who.device);
        let serial = self.listen(who, events, stop, now)?;
        if begin {
            self.reconnect(id, now);
        } else {
            let call = self.calls.get(&id).expect("retained call");
            let mut sync =
                vec![call.event(id, side, EventKind::Reconnecting, Reason::Ok, vec![], now)];
            if let Some(cipher) = &call.rekeys[1 - side] {
                sync.push(call.event(
                    id,
                    side,
                    if side == 1 {
                        EventKind::RekeyOffer
                    } else {
                        EventKind::RekeyAnswer
                    },
                    Reason::Ok,
                    cipher.clone(),
                    now,
                ));
            }
            if call.stage == Stage::Active {
                sync.push(call.event(id, side, EventKind::MediaReady, Reason::Ok, vec![], now));
            }
            for event in sync {
                if !self.emit(who.device, event, now) {
                    return Err(ERR_BUSY);
                }
            }
        }
        if self.live(who, serial) && self.calls.contains_key(&id) {
            Ok(serial)
        } else {
            Err(ERR_EXPIRED)
        }
    }

    fn try_emit(&self, device: [u8; 32], event: Event) -> bool {
        self.listeners.get(&device).is_some_and(|l| {
            !l.stop.is_closed()
                && event
                    .encode()
                    .ok()
                    .is_some_and(|p| l.events.try_send(Frame::control(OP_CALL_EVENT, p)).is_ok())
        })
    }

    // Queue loss is transient, not a terminal close_device. Reconnecting emits
    // use try_emit directly, so two full queues cannot recursively advance epochs.
    fn emit(&mut self, device: [u8; 32], event: Event, now: Instant) -> bool {
        let sent = self.try_emit(device, event);
        if !sent {
            self.detach_device(&device, now);
        }
        sent
    }

    fn remove_listener(&mut self, device: &[u8; 32]) {
        if let Some(listener) = self.listeners.remove(device) {
            listener.stop.close();
        }
    }

    fn detach_device(&mut self, device: &[u8; 32], now: Instant) {
        self.remove_listener(device);
        let ids: Vec<_> = self
            .calls
            .iter()
            .filter(|(_, c)| c.peers.iter().any(|p| &p.device == device))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let call = self.calls.get(&id).expect("selected call");
            if matches!(call.stage, Stage::Invited | Stage::Ringing) {
                self.end(&id, Reason::ConnectionLost, None, now);
            } else if call.recovery_deadline.is_none()
                || call.stage == Stage::Active
                || call.lanes.iter().any(Option::is_some)
            {
                self.reconnect(id, now);
            }
        }
    }

    fn reconnect(&mut self, id: [u8; 16], now: Instant) {
        let Some(call) = self.calls.get_mut(&id) else {
            return;
        };
        if !matches!(call.stage, Stage::Attaching | Stage::Active) {
            return;
        }
        let deadline = *call
            .recovery_deadline
            .get_or_insert(now + Duration::from_secs(RECOVERY_SECS));
        let Some(epoch) = call.epoch.checked_add(1).filter(|_| now < deadline) else {
            self.end(&id, Reason::Expired, None, now);
            return;
        };
        for slot in &mut call.lanes {
            if let Some(slot) = slot.take() {
                slot.stop.close();
            }
        }
        call.epoch = epoch;
        call.stage = Stage::Attaching;
        call.deadline = Some((now + Duration::from_secs(ATTACH_SECS)).min(deadline));
        call.rekeys = [None, None];
        call.recovered = [false; 2];
        call.ready_at = None;
        let events = std::array::from_fn::<_, 2, _>(|side| {
            (
                call.peers[side].device,
                call.event(id, side, EventKind::Reconnecting, Reason::Ok, vec![], now),
            )
        });
        for (device, event) in events {
            if !self.try_emit(device, event) {
                self.remove_listener(&device);
            }
        }
    }

    fn detach_media(&mut self, attach: Attach, index: usize, serial: u64, now: Instant) {
        if self.calls.get(&attach.call_id).is_some_and(|call| {
            call.epoch == attach.epoch
                && call.lanes[index]
                    .as_ref()
                    .is_some_and(|s| s.serial == serial)
        }) {
            self.reconnect(attach.call_id, now);
        }
    }

    fn end(&mut self, id: &[u8; 16], reason: Reason, local: Option<Endpoint>, now: Instant) {
        if let Some(call) = self.calls.remove(id) {
            for slot in call.lanes.iter().flatten() {
                slot.stop.close();
            }
            for side in 0..2 {
                let why = if reason == Reason::LocalEnded && local != Some(call.peers[side]) {
                    Reason::RemoteEnded
                } else {
                    reason
                };
                self.emit(
                    call.peers[side].device,
                    call.event(*id, side, EventKind::Ended, why, vec![], now),
                    now,
                );
            }
        }
    }

    pub fn close_device(&mut self, device: &[u8], reason: Reason) {
        if let Some(listener) = self.listeners.remove(device) {
            listener.stop.close();
        }
        let ids: Vec<_> = self
            .calls
            .iter()
            .filter(|(_, c)| c.peers.iter().any(|p| p.device.as_slice() == device))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.end(&id, reason, None, Instant::now());
        }
    }

    pub fn close_pair(&mut self, a: &[u8; 16], b: &[u8; 16]) {
        let ids: Vec<_> = self
            .calls
            .iter()
            .filter(|(_, c)| {
                (c.peers[0].user == *a && c.peers[1].user == *b)
                    || (c.peers[0].user == *b && c.peers[1].user == *a)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.end(&id, Reason::Unavailable, None, Instant::now());
        }
    }

    pub fn expire(&mut self, now: Instant) {
        let devices: Vec<_> = self
            .listeners
            .iter()
            .filter(|(_, l)| {
                now.saturating_duration_since(l.last) >= Duration::from_secs(LIVENESS_SECS)
            })
            .map(|(d, _)| *d)
            .collect();
        for device in devices {
            self.detach_device(&device, now);
        }
        let ids: Vec<_> = self
            .calls
            .iter()
            .filter(|(_, c)| {
                c.recovery_deadline.is_some_and(|d| now >= d)
                    || c.deadline.is_some_and(|d| now >= d)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if self
                .calls
                .get(&id)
                .is_some_and(|c| c.recovery_deadline.is_some_and(|d| now < d))
            {
                self.reconnect(id, now);
            } else {
                self.end(&id, Reason::Expired, None, now);
            }
        }
    }

    fn command(
        &mut self,
        db: &Connection,
        who: Endpoint,
        serial: u64,
        cmd: Command,
        now: Instant,
    ) -> Ack {
        self.expire(now);
        let reason = self.command_reason(db, who, serial, &cmd, now);
        Ack {
            request_id: cmd.request_id,
            call_id: cmd.call_id,
            epoch: cmd.epoch,
            reason,
        }
    }

    fn command_reason(
        &mut self,
        db: &Connection,
        who: Endpoint,
        serial: u64,
        cmd: &Command,
        now: Instant,
    ) -> Reason {
        if !self.live(who, serial) {
            return Reason::ConnectionLost;
        }
        if cmd.validate().is_err() {
            return Reason::Invalid;
        }
        if !current(db, who) {
            self.close_device(&who.device, Reason::Revoked);
            return Reason::Revoked;
        }
        self.listeners
            .get_mut(&who.device)
            .expect("checked listener")
            .last = now;
        if cmd.action == Action::Heartbeat {
            return Reason::Ok;
        }
        if cmd.action == Action::Invite {
            // Permission, missing account, stale binding and offline recipient all
            // share Unavailable; never turn this into a user-existence oracle.
            let peer = match binding(db, cmd.recipient) {
                Some(peer)
                    if peer.user != who.user
                        && allowed(db, who, peer)
                        && self
                            .listeners
                            .get(&peer.device)
                            .is_some_and(|l| l.who == peer && !l.stop.is_closed()) =>
                {
                    peer
                }
                _ => return Reason::Unavailable,
            };
            if self.busy(&who.device) || self.busy(&peer.device) || self.calls.len() >= MAX_CALLS {
                return Reason::Busy;
            }
            if self.calls.contains_key(&cmd.call_id) {
                return Reason::Invalid;
            }
            let call = Call {
                epoch: cmd.epoch,
                peers: [who, peer],
                stage: Stage::Invited,
                deadline: Some(now + Duration::from_secs(RING_SECS)),
                recovery_deadline: None,
                rekeys: [None, None],
                recovered: [false; 2],
                ready_at: None,
                lanes: std::array::from_fn(|_| None),
            };
            let event = call.event(
                cmd.call_id,
                1,
                EventKind::Invite,
                Reason::Ok,
                cmd.ciphertext.clone(),
                now,
            );
            self.calls.insert(cmd.call_id, call);
            return if self.emit(peer.device, event, now) {
                Reason::Ok
            } else {
                Reason::Unavailable
            };
        }
        let Some(call) = self.calls.get(&cmd.call_id) else {
            return if cmd.action == Action::End {
                Reason::Ok
            } else {
                Reason::Expired
            };
        };
        let Some(side) = call
            .side(who)
            .filter(|_| cmd.epoch > 0 && cmd.epoch <= call.epoch)
        else {
            return Reason::Invalid;
        };
        if cmd.action == Action::End {
            self.end(&cmd.call_id, Reason::LocalEnded, Some(who), now);
            return Reason::Ok;
        }
        if !allowed(db, call.peers[0], call.peers[1]) {
            self.end(&cmd.call_id, Reason::Unavailable, None, now);
            return Reason::Unavailable;
        }
        if cmd.action == Action::Recover {
            if !matches!(call.stage, Stage::Attaching | Stage::Active) {
                return Reason::Invalid;
            }
            if cmd.epoch == call.epoch {
                self.reconnect(cmd.call_id, now);
            }
            return if self.calls.contains_key(&cmd.call_id) {
                Reason::Ok
            } else {
                Reason::Expired
            };
        }
        if cmd.epoch != call.epoch {
            return Reason::Invalid;
        }
        match cmd.action {
            Action::Ringing
                if side == 1 && matches!(call.stage, Stage::Invited | Stage::Ringing) =>
            {
                let device = call.peers[0].device;
                let event = call.event(cmd.call_id, 0, EventKind::Ringing, Reason::Ok, vec![], now);
                self.calls
                    .get_mut(&cmd.call_id)
                    .expect("checked call")
                    .stage = Stage::Ringing;
                if self.emit(device, event, now) {
                    Reason::Ok
                } else {
                    Reason::ConnectionLost
                }
            }
            Action::Accept if side == 1 && call.stage == Stage::Ringing => {
                if !allowed(db, call.peers[0], call.peers[1]) {
                    self.end(&cmd.call_id, Reason::Unavailable, None, now);
                    return Reason::Unavailable;
                }
                let call = self.calls.get_mut(&cmd.call_id).expect("checked call");
                call.stage = Stage::Attaching;
                call.deadline = Some(now + Duration::from_secs(ATTACH_SECS));
                let device = call.peers[0].device;
                let event = call.event(
                    cmd.call_id,
                    0,
                    EventKind::Accepted,
                    Reason::Ok,
                    cmd.ciphertext.clone(),
                    now,
                );
                if self.emit(device, event, now) {
                    Reason::Ok
                } else {
                    Reason::ConnectionLost
                }
            }
            Action::Reject
                if side == 1 && matches!(call.stage, Stage::Invited | Stage::Ringing) =>
            {
                self.end(&cmd.call_id, Reason::Rejected, None, now);
                Reason::Ok
            }
            Action::RekeyOffer | Action::RekeyAnswer
                if cmd.epoch >= 2
                    && ((cmd.action == Action::RekeyOffer && side == 0)
                        || (cmd.action == Action::RekeyAnswer && side == 1)) =>
            {
                if let Some(cipher) = &call.rekeys[side] {
                    return if *cipher == cmd.ciphertext {
                        Reason::Ok
                    } else {
                        Reason::Invalid
                    };
                }
                if call.recovery_deadline.is_none() {
                    return Reason::Invalid;
                }
                if side == 1 && call.rekeys[0].is_none() {
                    return Reason::Invalid;
                }
                let call = self.calls.get_mut(&cmd.call_id).expect("checked call");
                call.rekeys[side] = Some(cmd.ciphertext.clone());
                let device = call.peers[1 - side].device;
                let event = call.event(
                    cmd.call_id,
                    1 - side,
                    if side == 0 {
                        EventKind::RekeyOffer
                    } else {
                        EventKind::RekeyAnswer
                    },
                    Reason::Ok,
                    cmd.ciphertext.clone(),
                    now,
                );
                // Cached before relay: a lost ACK/event is replayed on resume.
                self.emit(device, event, now);
                Reason::Ok
            }
            Action::Recovered
                if call.stage == Stage::Active
                    && cmd.epoch >= 2
                    && call.ready_at.is_some_and(|ready| {
                        now.saturating_duration_since(ready)
                            >= Duration::from_secs(RECOVERY_STABLE_SECS)
                    }) =>
            {
                let call = self.calls.get_mut(&cmd.call_id).expect("checked call");
                if call.recovery_deadline.is_none() {
                    return Reason::Ok;
                }
                if !call.recovered[side] {
                    eprintln!(
                        concat!(
                            "{{\"event\":\"call_recovery_confirmation\",",
                            "\"epoch\":{},\"side\":{},\"peer_confirmed\":{},\"remaining_ms\":{}}}"
                        ),
                        call.epoch,
                        side,
                        call.recovered[1 - side],
                        call.recovery_deadline.map_or(0, |deadline| deadline
                            .saturating_duration_since(now)
                            .as_millis()
                            .min(30_000)
                            as u64),
                    );
                    call.recovered[side] = true;
                }
                if call.recovered.iter().all(|r| *r) {
                    call.recovery_deadline = None;
                    let events = std::array::from_fn::<_, 2, _>(|side| {
                        (
                            call.peers[side].device,
                            call.event(
                                cmd.call_id,
                                side,
                                EventKind::Recovered,
                                Reason::Ok,
                                vec![],
                                now,
                            ),
                        )
                    });
                    for (device, event) in events {
                        if !self.emit(device, event, now) {
                            break;
                        }
                    }
                }
                Reason::Ok
            }
            _ => Reason::Invalid,
        }
    }

    fn attach(
        &mut self,
        db: &Connection,
        who: Endpoint,
        attach: Attach,
        outgoing: mpsc::Sender<Frame>,
        stop: Arc<Stop>,
        now: Instant,
    ) -> Result<(u64, usize), u8> {
        self.expire(now);
        if !current(db, who) {
            return Err(ERR_REVOKED);
        }
        let call = self.calls.get(&attach.call_id).ok_or(ERR_BUSY)?;
        let side = call
            .side(who)
            .filter(|_| call.epoch == attach.epoch)
            .ok_or(ERR_BUSY)?;
        if call.stage != Stage::Attaching
            || !allowed(db, call.peers[0], call.peers[1])
            || (call.recovery_deadline.is_some() && call.rekeys.iter().any(Option::is_none))
        {
            return Err(ERR_BUSY);
        }
        let index = side * 2 + attach.lane as usize;
        if call.lanes[index].is_some() {
            return Err(ERR_BUSY);
        }
        let serial = self.serial();
        let call = self.calls.get_mut(&attach.call_id).expect("checked call");
        call.lanes[index] = Some(Slot {
            serial,
            outgoing,
            stop,
        });
        if call.lanes.iter().all(Option::is_some) {
            call.stage = Stage::Active;
            call.deadline = None;
            call.ready_at = Some(now);
            let events = [
                call.event(
                    attach.call_id,
                    0,
                    EventKind::MediaReady,
                    Reason::Ok,
                    vec![],
                    now,
                ),
                call.event(
                    attach.call_id,
                    1,
                    EventKind::MediaReady,
                    Reason::Ok,
                    vec![],
                    now,
                ),
            ];
            let devices = [call.peers[0].device, call.peers[1].device];
            for (device, event) in devices.into_iter().zip(events) {
                if !self.emit(device, event, now) {
                    return Err(ERR_BUSY);
                }
            }
        }
        Ok((serial, index))
    }

    fn relay(
        &mut self,
        attach: Attach,
        index: usize,
        serial: u64,
        opcode: u8,
        payload: &[u8],
        completed: Instant,
        counters: &mut RelayCounters,
    ) -> Result<(), call_lane::Error> {
        let (expected, cap) = media_shape(attach.lane);
        if opcode != expected || payload.is_empty() || payload.len() > cap {
            return Err(call_lane::Error::Invalid);
        }
        let call = self
            .calls
            .get(&attach.call_id)
            .ok_or(call_lane::Error::Invalid)?;
        if call.epoch != attach.epoch
            || call.stage != Stage::Active
            || !call.lanes[index]
                .as_ref()
                .is_some_and(|s| s.serial == serial && !s.stop.is_closed())
        {
            return Err(call_lane::Error::Invalid);
        }
        if call.recovery_deadline.is_some_and(|d| Instant::now() >= d) {
            self.end(&attach.call_id, Reason::Expired, None, Instant::now());
            return Err(call_lane::Error::Lost);
        }
        // Pair same-kind lanes across endpoints: 0<->2 RTP, 1<->3 SRTCP.
        let peer = call.lanes[index ^ 2]
            .as_ref()
            .ok_or(call_lane::Error::Lost)?;
        if peer.stop.is_closed() {
            return Err(call_lane::Error::Lost);
        }
        if completed + HOP <= Instant::now() {
            counters.route_expired = counters.route_expired.saturating_add(1);
            return Ok(());
        }
        match peer.outgoing.try_send(Frame {
            opcode,
            payload: payload.to_vec(),
            deadline: Some(completed + HOP),
        }) {
            Ok(()) => {
                counters.queued = counters.queued.saturating_add(1);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                counters.full = counters.full.saturating_add(1);
                if let Some(index) = call_lane::srtcp_index(opcode, payload) {
                    counters.last_srtcp_full_index = Some(index);
                }
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(call_lane::Error::Lost),
        }
    }
}

fn binding(db: &Connection, user: [u8; 16]) -> Option<Endpoint> {
    crate::prekey::binding(db, &user)
        .ok()
        .flatten()
        .map(|(device, _, _)| Endpoint { user, device })
}

fn current(db: &Connection, who: Endpoint) -> bool {
    binding(db, who.user).is_some_and(|p| p == who)
}

fn allowed(db: &Connection, caller: Endpoint, recipient: Endpoint) -> bool {
    if !current(db, caller) || !current(db, recipient) || caller.user == recipient.user {
        return false;
    }
    let state = |a: &[u8; 16], b: &[u8; 16]| -> rusqlite::Result<Option<String>> {
        db.query_row(
            "SELECT state FROM contact_permissions WHERE user_id=?1 AND peer_user_id=?2",
            rusqlite::params![a.as_slice(), b.as_slice()],
            |r| r.get(0),
        )
        .optional()
    };
    let (Ok(a), Ok(b)) = (
        state(&caller.user, &recipient.user),
        state(&recipient.user, &caller.user),
    ) else {
        return false;
    };
    // A single QR creates one accepted permission; no reverse scan is required.
    a.as_deref() != Some("blocked")
        && b.as_deref() != Some("blocked")
        && (a.as_deref() == Some("accepted") || b.as_deref() == Some("accepted"))
}

fn media_shape(kind: LaneKind) -> (u8, usize) {
    match kind {
        LaneKind::Rtp => (OP_RTP, RTP_CAP),
        LaneKind::Srtcp => (OP_SRTCP, SRTCP_CAP),
    }
}

pub enum Handoff {
    Signal(Open),
    Media(Attach),
}

enum Registration {
    Signal(Endpoint, u64),
    Media(Attach, usize, u64),
}
struct Registered {
    hub: Arc<Mutex<Hub>>,
    registration: Registration,
}
impl Registered {
    fn invalid(&self, db: &Connection, now: Instant) {
        let mut hub = self.hub.lock().expect("call hub");
        match self.registration {
            Registration::Signal(who, serial) if hub.live(who, serial) => {
                let reason = if current(db, who) {
                    Reason::Invalid
                } else {
                    Reason::Revoked
                };
                hub.close_device(&who.device, reason);
            }
            Registration::Media(attach, index, serial)
                if hub.calls.get(&attach.call_id).is_some_and(|call| {
                    call.epoch == attach.epoch
                        && call.lanes[index]
                            .as_ref()
                            .is_some_and(|s| s.serial == serial && !s.stop.is_closed())
                }) =>
            {
                hub.end(&attach.call_id, Reason::Invalid, None, now);
            }
            _ => (),
        }
    }
}
impl Drop for Registered {
    fn drop(&mut self) {
        let mut hub = self.hub.lock().expect("call hub");
        match self.registration {
            Registration::Signal(who, serial) if hub.live(who, serial) => {
                hub.detach_device(&who.device, Instant::now())
            }
            Registration::Media(attach, index, serial) => {
                hub.detach_media(attach, index, serial, Instant::now())
            }
            _ => (),
        }
    }
}

/// Consume the socket/Noise and retain both auth revocation and capacity ownership.
pub async fn serve(
    stream: TcpStream,
    noise: snow::TransportState,
    st: Arc<State>,
    who: Endpoint,
    handoff: Handoff,
    session: SessionGuard,
    _post: OwnedSemaphorePermit,
) {
    let revoked = session.notify.clone();
    let stop = Arc::new(Stop::default());
    let capacity = match handoff {
        Handoff::Signal(_) => EVENT_CAP,
        Handoff::Media(_) => 1,
    };
    let (outgoing, requests) = mpsc::channel(capacity);
    let registration = {
        // Consistent DB->hub order; active check and registration are atomic wrt
        // replacement/block. Neither lock survives the network await.
        let db = st.db.lock().expect("db");
        let mut hub = st.calls.lock().expect("call hub");
        if !current(&db, who) {
            Err(ERR_REVOKED)
        } else {
            match handoff {
                Handoff::Signal(open) => hub
                    .open(&db, who, open, outgoing, stop.clone(), Instant::now())
                    .map(|serial| Registration::Signal(who, serial)),
                Handoff::Media(attach) => hub
                    .attach(&db, who, attach, outgoing, stop.clone(), Instant::now())
                    .map(|(serial, index)| Registration::Media(attach, index, serial)),
            }
        }
    };
    let (registration, initial, inbound_cap) = match registration {
        Ok(registration) => {
            let (initial, cap) = match registration {
                Registration::Signal(_, _) => (
                    Frame::control(OP_CALL_READY, vec![VERSION, PROFILE]),
                    47 + CIPHERTEXT_MAX,
                ),
                Registration::Media(attach, _, _) => (
                    Frame::control(OP_CALL_ATTACHED, attach.encode().expect("validated attach")),
                    media_shape(attach.lane).1,
                ),
            };
            (registration, initial, cap)
        }
        Err(code) => {
            // Invalid/duplicate attachment retires only this unregistered stream.
            let mut stream = stream;
            let mut noise = noise;
            let inner = dmsg_protocol::encode_frame(OP_ERROR, &[code]).expect("fits");
            let mut wire = vec![0; inner.len() + 16];
            if let Ok(n) = noise.write_message(&inner, &mut wire) {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    crate::write_hs_msg(&mut stream, &wire[..n]),
                )
                .await;
            }
            return;
        }
    };
    let registered = Registered {
        hub: st.calls.clone(),
        registration,
    };
    let mut media_counters = call_lane::MediaCounters::default();
    let mut relay_counters = RelayCounters::default();
    let result = call_lane::run(
        stream,
        noise,
        requests,
        stop,
        revoked,
        inbound_cap,
        initial,
        &mut media_counters,
        |opcode, payload, completed| match registered.registration {
            Registration::Signal(who, serial) => {
                if opcode != OP_CALL_COMMAND {
                    return Err(call_lane::Error::Invalid);
                }
                let cmd = Command::decode(payload).map_err(|_| call_lane::Error::Invalid)?;
                let ack = {
                    let db = st.db.lock().expect("db");
                    st.calls.lock().expect("call hub").command(
                        &db,
                        who,
                        serial,
                        cmd,
                        Instant::now(),
                    )
                };
                Ok(Some(Frame::control(OP_CALL_ACK, ack.encode())))
            }
            Registration::Media(attach, index, serial) => {
                st.calls.lock().expect("call hub").relay(
                    attach,
                    index,
                    serial,
                    opcode,
                    payload,
                    completed,
                    &mut relay_counters,
                )?;
                Ok(None)
            }
        },
    )
    .await;
    if result == Err(call_lane::Error::Invalid) {
        // Same DB->hub order as registration/revocation. Only this live owner
        // can terminate; an invalid retired lane cannot affect its replacement.
        let db = st.db.lock().expect("db");
        registered.invalid(&db, Instant::now());
    }
    if let Registration::Media(attach, index, _) = registered.registration {
        // Fixed schema, numeric/null metadata only; no identity or packet material.
        // Receive/relay describe this side's ingress; writes/expiry its egress.
        eprintln!(
            concat!(
                "{{\"event\":\"call_lane_summary\",\"side\":{},\"lane\":{},",
                "\"authenticated_media_rx\":{},\"complete_tcp_media_writes\":{},",
                "\"expired_plaintext_before_noise\":{},\"relay_queued\":{},",
                "\"relay_full\":{},\"relay_route_expired\":{},",
                "\"last_srtcp_rx_index\":{},\"last_srtcp_tcp_write_index\":{},",
                "\"last_srtcp_full_index\":{}}}"
            ),
            index / 2,
            attach.lane as u8,
            media_counters.authenticated_media_rx,
            media_counters.complete_tcp_media_writes,
            media_counters.expired_plaintext_before_noise,
            relay_counters.queued,
            relay_counters.full,
            relay_counters.route_expired,
            media_counters
                .last_srtcp_rx_index
                .map_or_else(|| "null".to_owned(), |index| index.to_string()),
            media_counters
                .last_srtcp_tcp_write_index
                .map_or_else(|| "null".to_owned(), |index| index.to_string()),
            relay_counters
                .last_srtcp_full_index
                .map_or_else(|| "null".to_owned(), |index| index.to_string()),
        );
    }
    // RAII also handles task cancellation before/inside the lane future.
    drop(registered);
    drop(session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmsg_protocol::call::VERSION;
    use dmsg_protocol::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Semaphore;
    use tokio::time::timeout;

    fn database() -> (Connection, [Endpoint; 6]) {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let peers = std::array::from_fn(|i| {
            let n = i as u8 + 1;
            let who = Endpoint {
                user: [n; 16],
                device: crate::noise::pubkey_of(&[n + 40; 32]),
            };
            db.execute(
                "INSERT INTO users VALUES(?1,?2,?3,'$argon2id$test',1)",
                rusqlite::params![
                    who.user.as_slice(),
                    format!("{:012}", n),
                    format!("peer{n}")
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
                rusqlite::params![who.device.as_slice(), who.user.as_slice()],
            )
            .unwrap();
            db.execute(
                "INSERT INTO device_identities VALUES(?1,?2,?3,?4)",
                rusqlite::params![
                    who.device.as_slice(),
                    who.user.as_slice(),
                    [n + 60; 32].as_slice(),
                    [n + 80; 32].as_slice()
                ],
            )
            .unwrap();
            who
        });
        for pair in peers.chunks_exact(2) {
            db.execute(
                "INSERT INTO contact_permissions VALUES(?1,?2,'accepted')",
                rusqlite::params![pair[1].user.as_slice(), pair[0].user.as_slice()],
            )
            .unwrap();
        }
        (db, peers)
    }

    fn listener(
        hub: &mut Hub,
        who: Endpoint,
        now: Instant,
    ) -> (u64, mpsc::Receiver<Frame>, Arc<Stop>) {
        let (tx, rx) = mpsc::channel(EVENT_CAP);
        let stop = Arc::new(Stop::default());
        let serial = hub.listen(who, tx, stop.clone(), now).unwrap();
        (serial, rx, stop)
    }

    fn command(action: Action, id: [u8; 16], recipient: [u8; 16]) -> Command {
        let cmd = Command {
            request_id: 1,
            action,
            call_id: id,
            epoch: 1,
            recipient,
            ciphertext: if matches!(action, Action::Invite | Action::Accept) {
                vec![123; 48]
            } else {
                vec![]
            },
        };
        cmd.validate().unwrap();
        cmd
    }

    fn event(rx: &mut mpsc::Receiver<Frame>) -> Event {
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame.opcode, OP_CALL_EVENT);
        Event::decode(&frame.payload).unwrap()
    }

    fn invite(
        hub: &mut Hub,
        db: &Connection,
        peers: [Endpoint; 2],
        serial: u64,
        id: [u8; 16],
        now: Instant,
    ) -> Reason {
        hub.command(
            db,
            peers[0],
            serial,
            command(Action::Invite, id, peers[1].user),
            now,
        )
        .reason
    }

    fn accepted(
        db: &Connection,
        peers: [Endpoint; 2],
        id: [u8; 16],
        now: Instant,
    ) -> (Hub, [u64; 2], [mpsc::Receiver<Frame>; 2]) {
        let mut hub = Hub::default();
        let (a, mut arx, _) = listener(&mut hub, peers[0], now);
        let (b, mut brx, _) = listener(&mut hub, peers[1], now);
        assert_eq!(invite(&mut hub, db, peers, a, id, now), Reason::Ok);
        assert_eq!(event(&mut brx).kind, EventKind::Invite);
        for action in [Action::Ringing, Action::Accept] {
            assert_eq!(
                hub.command(db, peers[1], b, command(action, id, [0; 16]), now)
                    .reason,
                Reason::Ok
            );
            event(&mut arx);
        }
        (hub, [a, b], [arx, brx])
    }

    fn recovery_command(action: Action, id: [u8; 16], epoch: u64) -> Command {
        Command {
            request_id: 2,
            action,
            call_id: id,
            epoch,
            recipient: [0; 16],
            ciphertext: if matches!(action, Action::RekeyOffer | Action::RekeyAnswer) {
                vec![action as u8; 48]
            } else {
                vec![]
            },
        }
    }

    fn attach_four(
        hub: &mut Hub,
        db: &Connection,
        peers: [Endpoint; 2],
        id: [u8; 16],
        epoch: u64,
        now: Instant,
    ) -> Vec<(Attach, u64, mpsc::Receiver<Frame>, Arc<Stop>)> {
        (0..4)
            .map(|index| {
                let attach = Attach {
                    call_id: id,
                    epoch,
                    lane: if index % 2 == 0 {
                        LaneKind::Rtp
                    } else {
                        LaneKind::Srtcp
                    },
                };
                let (tx, rx) = mpsc::channel(1);
                let stop = Arc::new(Stop::default());
                let (serial, slot) = hub
                    .attach(db, peers[index / 2], attach, tx, stop.clone(), now)
                    .unwrap();
                assert_eq!(slot, index);
                (attach, serial, rx, stop)
            })
            .collect()
    }

    #[test]
    fn calls_recover_same_identity_fresh_routes_stable_handshake_and_stale_end() {
        let (db, peers) = database();
        let peers = [peers[0], peers[1]];
        let now = Instant::now();
        let id = [31; 16];
        let (mut hub, [a, b], [mut arx, mut brx]) = accepted(&db, peers, id, now);
        let old = attach_four(&mut hub, &db, peers, id, 1, now);
        assert_eq!(event(&mut arx).kind, EventKind::MediaReady);
        assert_eq!(event(&mut brx).kind, EventKind::MediaReady);
        hub.detach_media(old[0].0, 0, old[0].1, now);
        let deadline = now + Duration::from_secs(RECOVERY_SECS);
        assert_eq!(hub.calls[&id].epoch, 2);
        assert_eq!(hub.calls[&id].recovery_deadline, Some(deadline));
        assert!(hub.busy(&peers[0].device) && hub.busy(&peers[1].device));
        for (_, _, _, stop) in &old {
            assert!(stop.is_closed());
        }
        for rx in [&mut arx, &mut brx] {
            let e = event(rx);
            assert_eq!(
                (e.kind, e.call_id, e.epoch, e.remaining_ms),
                (EventKind::Reconnecting, id, 2, 30_000)
            );
        }
        for (index, (attach, serial, _, _)) in old.iter().enumerate() {
            hub.detach_media(*attach, index, *serial, now);
        }
        assert_eq!(
            hub.command(
                &db,
                peers[0],
                a,
                recovery_command(Action::Recover, id, 1),
                now
            )
            .reason,
            Reason::Ok
        );
        assert_eq!(hub.calls[&id].epoch, 2, "same failed epoch advances once");
        let (tx, _rx) = mpsc::channel(1);
        assert!(hub
            .attach(
                &db,
                peers[0],
                Attach {
                    epoch: 2,
                    ..old[0].0
                },
                tx,
                Arc::new(Stop::default()),
                now
            )
            .is_err());
        let offer = recovery_command(Action::RekeyOffer, id, 2);
        let answer = recovery_command(Action::RekeyAnswer, id, 2);
        assert_eq!(
            hub.command(&db, peers[1], b, offer.clone(), now).reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(&db, peers[1], b, answer.clone(), now).reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(&db, peers[0], a, offer.clone(), now).reason,
            Reason::Ok
        );
        assert_eq!(event(&mut brx).ciphertext, offer.ciphertext);
        assert_eq!(
            hub.command(&db, peers[0], a, offer.clone(), now).reason,
            Reason::Ok
        );
        assert!(
            brx.try_recv().is_err(),
            "exact retry does not enqueue duplicate event"
        );
        let mut different = offer.clone();
        different.ciphertext[0] ^= 1;
        assert_eq!(
            hub.command(&db, peers[0], a, different, now).reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(&db, peers[1], b, answer, now).reason,
            Reason::Ok
        );
        assert_eq!(event(&mut arx).kind, EventKind::RekeyAnswer);
        let ready = now + Duration::from_secs(2);
        let mut new = attach_four(&mut hub, &db, peers, id, 2, ready);
        for rx in [&mut arx, &mut brx] {
            let e = event(rx);
            assert_eq!(
                (e.kind, e.epoch, e.remaining_ms),
                (EventKind::MediaReady, 2, 28_000)
            );
        }
        assert_eq!(
            hub.calls[&id].recovery_deadline,
            Some(deadline),
            "four lanes do not clear episode"
        );
        let mut counters = RelayCounters::default();
        assert!(hub
            .relay(
                old[0].0,
                0,
                old[0].1,
                OP_RTP,
                &[1],
                Instant::now(),
                &mut counters
            )
            .is_err());
        hub.detach_media(
            Attach {
                epoch: 1,
                ..new[0].0
            },
            0,
            new[0].1,
            ready,
        );
        hub.detach_media(new[0].0, 0, old[0].1, ready);
        assert_eq!(
            hub.calls[&id].epoch, 2,
            "both epoch and serial guard cleanup"
        );
        hub.relay(
            new[0].0,
            0,
            new[0].1,
            OP_RTP,
            &[7],
            Instant::now(),
            &mut counters,
        )
        .unwrap();
        assert_eq!(new[2].2.try_recv().unwrap().payload, [7]);
        let recovered = recovery_command(Action::Recovered, id, 2);
        assert_eq!(
            hub.command(&db, peers[0], a, recovered.clone(), ready)
                .reason,
            Reason::Invalid
        );
        let stable = ready + Duration::from_secs(RECOVERY_STABLE_SECS);
        assert_eq!(
            hub.command(&db, peers[0], a, recovered.clone(), stable)
                .reason,
            Reason::Ok
        );
        assert_eq!(hub.calls[&id].recovery_deadline, Some(deadline));
        assert_eq!(
            hub.command(
                &db,
                peers[1],
                b,
                recovery_command(Action::Recovered, id, 1),
                stable
            )
            .reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(&db, peers[1], b, recovered.clone(), stable)
                .reason,
            Reason::Ok
        );
        assert_eq!(hub.calls[&id].recovery_deadline, None);
        assert_eq!(event(&mut arx).kind, EventKind::Recovered);
        assert_eq!(event(&mut brx).kind, EventKind::Recovered);
        assert_eq!(
            hub.command(&db, peers[0], a, recovered, stable).reason,
            Reason::Ok
        );
        assert_eq!(
            hub.command(&db, peers[0], a, offer, stable).reason,
            Reason::Ok
        );
        assert!(brx.try_recv().is_err());
        hub.detach_media(new[0].0, 0, new[0].1, stable);
        assert_eq!(hub.calls[&id].epoch, 3);
        assert_eq!(
            hub.calls[&id].recovery_deadline,
            Some(stable + Duration::from_secs(RECOVERY_SECS))
        );
        event(&mut arx);
        event(&mut brx);
        assert_eq!(
            hub.command(
                &db,
                peers[0],
                a,
                recovery_command(Action::End, id, 4),
                stable
            )
            .reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(&db, peers[0], a, command(Action::End, id, [0; 16]), stable)
                .reason,
            Reason::Ok
        );
        assert_eq!(event(&mut arx).reason, Reason::LocalEnded);
        assert_eq!(event(&mut brx).reason, Reason::RemoteEnded);
        assert!(hub.calls.is_empty());
        assert_eq!(
            hub.command(&db, peers[0], a, command(Action::End, id, [0; 16]), stable)
                .reason,
            Reason::Ok
        );
    }

    #[test]
    fn calls_resume_replays_bounded_peer_cache_and_old_signal_drop_is_inert() {
        let (db, peers) = database();
        let now = Instant::now();
        let id = [32; 16];
        let pair = [peers[0], peers[1]];
        let (mut hub, [a, b], [mut arx, mut brx]) = accepted(&db, pair, id, now);
        let old_stop = hub.listeners[&peers[1].device].stop.clone();
        let (tx, mut resumed) = mpsc::channel(EVENT_CAP);
        let serial = hub
            .open(
                &db,
                peers[1],
                Open {
                    resume: Some((id, 1)),
                },
                tx,
                Arc::new(Stop::default()),
                now,
            )
            .unwrap();
        assert_ne!(serial, b);
        assert!(old_stop.is_closed());
        assert_eq!(event(&mut resumed).kind, EventKind::Reconnecting);
        assert_eq!(event(&mut arx).epoch, 2);
        assert!(brx.try_recv().is_err());
        let offer = recovery_command(Action::RekeyOffer, id, 2);
        assert_eq!(
            hub.command(&db, peers[0], a, offer.clone(), now).reason,
            Reason::Ok
        );
        assert_eq!(event(&mut resumed).ciphertext, offer.ciphertext);
        assert_eq!(
            hub.command(
                &db,
                peers[1],
                serial,
                recovery_command(Action::RekeyAnswer, id, 2),
                now
            )
            .reason,
            Reason::Ok
        );
        event(&mut arx);
        let lanes = attach_four(&mut hub, &db, pair, id, 2, now);
        event(&mut arx);
        event(&mut resumed);
        let (tx, mut replay) = mpsc::channel(EVENT_CAP);
        let newest = hub
            .open(
                &db,
                peers[1],
                Open {
                    resume: Some((id, 1)),
                },
                tx,
                Arc::new(Stop::default()),
                now,
            )
            .unwrap();
        assert_eq!(hub.calls[&id].epoch, 2);
        assert_eq!(event(&mut replay).kind, EventKind::Reconnecting);
        assert_eq!(event(&mut replay).ciphertext, offer.ciphertext);
        assert_eq!(event(&mut replay).kind, EventKind::MediaReady);
        assert!(replay.try_recv().is_err());
        for (who, open, expected) in [
            (peers[1], Open { resume: None }, ERR_BUSY),
            (
                peers[1],
                Open {
                    resume: Some(([99; 16], 1)),
                },
                ERR_EXPIRED,
            ),
            (
                peers[1],
                Open {
                    resume: Some((id, 3)),
                },
                ERR_BUSY,
            ),
            (
                peers[2],
                Open {
                    resume: Some((id, 2)),
                },
                ERR_BUSY,
            ),
        ] {
            let (tx, _rx) = mpsc::channel(EVENT_CAP);
            assert_eq!(
                hub.open(&db, who, open, tx, Arc::new(Stop::default()), now),
                Err(expected)
            );
            assert!(hub.live(peers[1], newest));
        }
        let deadline = hub.calls[&id].recovery_deadline;
        let old_serial = newest;
        let (tx, mut current) = mpsc::channel(EVENT_CAP);
        let newest = hub
            .open(
                &db,
                peers[1],
                Open {
                    resume: Some((id, 2)),
                },
                tx,
                Arc::new(Stop::default()),
                now + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(hub.calls[&id].epoch, 3);
        assert_eq!(hub.calls[&id].recovery_deadline, deadline);
        assert!(hub.calls[&id].rekeys.iter().all(Option::is_none));
        assert!(hub.calls[&id].lanes.iter().all(Option::is_none));
        assert_eq!(event(&mut current).remaining_ms, 29_000);
        for (index, (attach, serial, _, stop)) in lanes.iter().enumerate() {
            assert!(stop.is_closed());
            hub.detach_media(*attach, index, *serial, now + Duration::from_secs(1));
        }
        assert_eq!(hub.calls[&id].epoch, 3);
        for (side, action) in [(0, Action::RekeyOffer), (1, Action::RekeyAnswer)] {
            assert_eq!(
                hub.command(
                    &db,
                    peers[side],
                    [a, newest][side],
                    recovery_command(action, id, 3),
                    now + Duration::from_secs(1)
                )
                .reason,
                Reason::Ok
            );
        }
        let partial = Attach {
            call_id: id,
            epoch: 3,
            lane: LaneKind::Rtp,
        };
        let partial_stop = Arc::new(Stop::default());
        let (tx, _rx) = mpsc::channel(1);
        let (partial_serial, index) = hub
            .attach(
                &db,
                peers[0],
                partial,
                tx,
                partial_stop.clone(),
                now + Duration::from_secs(1),
            )
            .unwrap();
        assert!(hub.calls[&id].stage == Stage::Attaching);
        let partial_signal = newest;
        let (tx, mut current) = mpsc::channel(EVENT_CAP);
        let newest = hub
            .open(
                &db,
                peers[1],
                Open {
                    resume: Some((id, 3)),
                },
                tx,
                Arc::new(Stop::default()),
                now + Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(hub.calls[&id].epoch, 4);
        assert_eq!(hub.calls[&id].recovery_deadline, deadline);
        assert_eq!(event(&mut current).remaining_ms, 28_000);
        assert!(partial_stop.is_closed());
        assert!(hub.calls[&id].lanes.iter().all(Option::is_none));
        hub.detach_media(partial, index, partial_serial, now + Duration::from_secs(2));
        let hub = Arc::new(Mutex::new(hub));
        for registration in [
            Registration::Signal(peers[1], b),
            Registration::Signal(peers[1], serial),
            Registration::Signal(peers[1], old_serial),
            Registration::Signal(peers[1], partial_signal),
            Registration::Media(partial, index, partial_serial),
        ] {
            let old = Registered {
                hub: hub.clone(),
                registration,
            };
            old.invalid(&db, now + Duration::from_secs(2));
            drop(old);
        }
        assert!(hub.lock().unwrap().live(peers[1], newest));
        assert_eq!(hub.lock().unwrap().calls[&id].epoch, 4);
        db.execute("UPDATE contact_permissions SET state='blocked'", [])
            .unwrap();
        let (tx, _rx) = mpsc::channel(EVENT_CAP);
        assert_eq!(
            hub.lock().unwrap().open(
                &db,
                peers[1],
                Open {
                    resume: Some((id, 2))
                },
                tx,
                Arc::new(Stop::default()),
                now
            ),
            Err(ERR_EXPIRED)
        );
        assert!(hub.lock().unwrap().calls.is_empty());
    }

    #[test]
    fn calls_invalid_media_cleanup_requires_both_epoch_and_serial() {
        let (db, peers) = database();
        let pair = [peers[0], peers[1]];
        let now = Instant::now();
        let id = [36; 16];
        let (mut hub, [a, _], [mut arx, mut brx]) = accepted(&db, pair, id, now);
        let lanes = attach_four(&mut hub, &db, pair, id, 1, now);
        event(&mut arx);
        event(&mut brx);
        let hub = Arc::new(Mutex::new(hub));
        let (attach, serial, _, _) = &lanes[0];
        for registration in [
            Registration::Signal(peers[0], a + 100),
            Registration::Media(
                Attach {
                    epoch: 2,
                    ..*attach
                },
                0,
                *serial,
            ),
            Registration::Media(*attach, 0, serial + 100),
        ] {
            let stale = Registered {
                hub: hub.clone(),
                registration,
            };
            stale.invalid(&db, now);
            drop(stale);
            assert_eq!(hub.lock().unwrap().calls[&id].epoch, 1);
            assert!(hub.lock().unwrap().calls[&id].stage == Stage::Active);
        }
        let current = Registered {
            hub: hub.clone(),
            registration: Registration::Media(*attach, 0, *serial),
        };
        current.invalid(&db, now);
        drop(current);
        assert!(hub.lock().unwrap().calls.is_empty());
        assert_eq!(event(&mut arx).reason, Reason::Invalid);
        assert_eq!(event(&mut brx).reason, Reason::Invalid);
    }

    #[test]
    fn calls_recovery_retries_never_extend_episode_and_epoch_never_wraps() {
        let (db, peers) = database();
        let now = Instant::now();
        let id = [33; 16];
        let (mut hub, [a, _], [mut arx, mut brx]) = accepted(&db, [peers[0], peers[1]], id, now);
        assert_eq!(
            hub.command(
                &db,
                peers[0],
                a,
                recovery_command(Action::Recover, id, 1),
                now
            )
            .reason,
            Reason::Ok
        );
        event(&mut arx);
        event(&mut brx);
        let deadline = now + Duration::from_secs(RECOVERY_SECS);
        for (seconds, epoch, remaining) in [(10, 3, 20_000), (20, 4, 10_000)] {
            let at = now + Duration::from_secs(seconds);
            hub.calls.get_mut(&id).unwrap().rekeys = [Some(vec![1]), Some(vec![2])];
            for listener in hub.listeners.values_mut() {
                listener.last = at;
            }
            hub.expire(at);
            assert_eq!(hub.calls[&id].epoch, epoch);
            assert_eq!(hub.calls[&id].recovery_deadline, Some(deadline));
            assert!(hub.calls[&id].rekeys.iter().all(Option::is_none));
            for rx in [&mut arx, &mut brx] {
                assert_eq!(event(rx).remaining_ms, remaining);
            }
        }
        for listener in hub.listeners.values_mut() {
            listener.last = deadline;
        }
        hub.expire(deadline);
        assert!(hub.calls.is_empty());
        assert_eq!(event(&mut arx).reason, Reason::Expired);
        assert_eq!(event(&mut brx).reason, Reason::Expired);
        let (mut hub, [a, _], [mut arx, mut brx]) = accepted(&db, [peers[0], peers[1]], id, now);
        hub.calls.get_mut(&id).unwrap().epoch = u64::MAX;
        assert_eq!(
            hub.command(
                &db,
                peers[0],
                a,
                recovery_command(Action::Recover, id, u64::MAX),
                now
            )
            .reason,
            Reason::Expired
        );
        assert!(hub.calls.is_empty());
        assert_eq!(event(&mut arx).epoch, u64::MAX);
        assert_eq!(event(&mut brx).reason, Reason::Expired);
    }

    #[test]
    fn calls_transient_queue_and_liveness_loss_keep_accepted_call_without_storms() {
        let (db, peers) = database();
        let now = Instant::now();
        let id = [34; 16];
        let (mut hub, _, [mut arx, mut brx]) = accepted(&db, [peers[0], peers[1]], id, now);
        let _lanes = attach_four(&mut hub, &db, [peers[0], peers[1]], id, 1, now);
        let ready = event(&mut arx);
        event(&mut brx);
        for _ in 0..EVENT_CAP {
            assert!(hub.try_emit(peers[0].device, ready.clone()));
            assert!(hub.try_emit(peers[1].device, ready.clone()));
        }
        assert!(!hub.emit(peers[0].device, ready, now));
        assert!(hub.listeners.is_empty());
        assert_eq!(
            hub.calls[&id].epoch, 2,
            "both overflowing listeners reserve only one epoch"
        );
        assert!(hub.calls[&id].lanes.iter().all(Option::is_none));
        let (mut hub, _, [mut arx, mut brx]) = accepted(&db, [peers[0], peers[1]], id, now);
        hub.expire(now + Duration::from_secs(LIVENESS_SECS));
        assert_eq!(hub.calls[&id].epoch, 2);
        assert!(hub.listeners.is_empty());
        let mut reconnects = 0;
        for rx in [&mut arx, &mut brx] {
            while let Ok(frame) = rx.try_recv() {
                assert_eq!(
                    Event::decode(&frame.payload).unwrap().kind,
                    EventKind::Reconnecting
                );
                reconnects += 1;
            }
        }
        assert_eq!(
            reconnects, 1,
            "first expired listener notifies only the other"
        );
    }

    #[test]
    fn calls_lifecycle_membership_duplicate_attach_and_opaque_pairing() {
        let (db, peers) = database();
        let now = Instant::now();
        let mut hub = Hub::default();
        let (a, mut arx, _) = listener(&mut hub, peers[0], now);
        let (b, mut brx, _) = listener(&mut hub, peers[1], now);
        let (stranger, _, _) = listener(&mut hub, peers[2], now);
        let id = [11; 16];
        assert_eq!(
            invite(&mut hub, &db, [peers[0], peers[1]], a, id, now),
            Reason::Ok
        );
        let received = event(&mut brx);
        assert_eq!(received.kind, EventKind::Invite);
        assert_eq!(received.ciphertext, vec![123; 48]);
        assert_eq!(received.peer_device, peers[0].device);
        assert!(
            arx.try_recv().is_err(),
            "Ringing must wait for recipient confirmation"
        );
        assert_eq!(
            hub.command(&db, peers[1], b, command(Action::Accept, id, [0; 16]), now)
                .reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(
                &db,
                peers[2],
                stranger,
                command(Action::End, id, [0; 16]),
                now
            )
            .reason,
            Reason::Invalid
        );
        assert_eq!(
            hub.command(&db, peers[1], b, command(Action::Ringing, id, [0; 16]), now)
                .reason,
            Reason::Ok
        );
        assert_eq!(event(&mut arx).kind, EventKind::Ringing);
        assert_eq!(
            hub.command(&db, peers[1], b, command(Action::Accept, id, [0; 16]), now)
                .reason,
            Reason::Ok
        );
        assert_eq!(event(&mut arx).kind, EventKind::Accepted);
        let mut slots = vec![];
        for index in 0..4 {
            let attach = Attach {
                call_id: id,
                epoch: 1,
                lane: if index % 2 == 0 {
                    LaneKind::Rtp
                } else {
                    LaneKind::Srtcp
                },
            };
            let (tx, rx) = mpsc::channel(1);
            let stop = Arc::new(Stop::default());
            let (serial, slot) = hub
                .attach(&db, peers[index / 2], attach, tx.clone(), stop.clone(), now)
                .unwrap();
            assert_eq!(slot, index);
            assert!(hub
                .attach(
                    &db,
                    peers[index / 2],
                    attach,
                    tx,
                    Arc::new(Stop::default()),
                    now
                )
                .is_err());
            slots.push((serial, rx, stop));
            if index < 3 {
                assert!(arx.try_recv().is_err());
                assert!(brx.try_recv().is_err());
            }
        }
        assert_eq!(event(&mut arx).kind, EventKind::MediaReady);
        assert_eq!(event(&mut brx).kind, EventKind::MediaReady);
        let attach = Attach {
            call_id: id,
            epoch: 1,
            lane: LaneKind::Rtp,
        };
        let mut rtp_counters = RelayCounters::default();
        hub.relay(
            attach,
            0,
            slots[0].0,
            OP_RTP,
            &[9; RTP_CAP],
            Instant::now(),
            &mut rtp_counters,
        )
        .unwrap();
        // Full is an intentional bounded drop; expiry never queues plaintext.
        hub.relay(
            attach,
            0,
            slots[0].0,
            OP_RTP,
            &[7],
            Instant::now(),
            &mut rtp_counters,
        )
        .unwrap();
        hub.relay(
            attach,
            0,
            slots[0].0,
            OP_RTP,
            &[6],
            Instant::now() - HOP,
            &mut rtp_counters,
        )
        .unwrap();
        assert_eq!(rtp_counters.queued, 1);
        assert_eq!(rtp_counters.full, 1);
        assert_eq!(rtp_counters.route_expired, 1);
        assert_eq!(rtp_counters.last_srtcp_full_index, None);
        let relayed = slots[2].1.try_recv().unwrap();
        assert_eq!(relayed.payload, vec![9; RTP_CAP]);
        assert!(relayed.deadline.is_some());
        assert!(slots[1].1.try_recv().is_err());
        assert!(hub
            .relay(
                attach,
                0,
                slots[0].0,
                OP_SRTCP,
                &[1],
                Instant::now(),
                &mut rtp_counters
            )
            .is_err());
        assert!(hub
            .relay(
                attach,
                0,
                slots[0].0,
                OP_RTP,
                &[1; RTP_CAP + 1],
                Instant::now(),
                &mut rtp_counters,
            )
            .is_err());
        assert_eq!(rtp_counters.queued, 1);
        assert_eq!(rtp_counters.full, 1);
        assert_eq!(rtp_counters.route_expired, 1);
        let rtcp = Attach {
            lane: LaneKind::Srtcp,
            ..attach
        };
        let mut rtcp_counters = RelayCounters::default();
        hub.relay(
            rtcp,
            3,
            slots[3].0,
            OP_SRTCP,
            &[8; SRTCP_CAP],
            Instant::now(),
            &mut rtcp_counters,
        )
        .unwrap();
        assert_eq!(rtcp_counters.last_srtcp_full_index, None);
        // Dummy fixed-profile trailer markers, not authenticated media.
        let mut marker = [0xa5; 22];
        marker[8..12].copy_from_slice(&0x8000_0009_u32.to_be_bytes());
        hub.relay(
            rtcp,
            3,
            slots[3].0,
            OP_SRTCP,
            &marker,
            Instant::now(),
            &mut rtcp_counters,
        )
        .unwrap();
        assert_eq!(rtcp_counters.last_srtcp_full_index, Some(9));
        marker[8..12].copy_from_slice(&0x8000_000a_u32.to_be_bytes());
        hub.relay(
            rtcp,
            3,
            slots[3].0,
            OP_SRTCP,
            &marker,
            Instant::now() - HOP,
            &mut rtcp_counters,
        )
        .unwrap();
        assert_eq!(rtcp_counters.last_srtcp_full_index, Some(9));
        assert_eq!(slots[1].1.try_recv().unwrap().payload, vec![8; SRTCP_CAP]);
        hub.relay(
            rtcp,
            3,
            slots[3].0,
            OP_SRTCP,
            &marker,
            Instant::now(),
            &mut rtcp_counters,
        )
        .unwrap();
        assert_eq!(slots[1].1.try_recv().unwrap().payload, marker);
        assert_eq!(rtcp_counters.queued, 2);
        assert_eq!(rtcp_counters.full, 1);
        assert_eq!(rtcp_counters.route_expired, 1);
        assert_eq!(rtcp_counters.last_srtcp_full_index, Some(9));
        assert_eq!(
            hub.command(&db, peers[0], a, command(Action::End, id, [0; 16]), now)
                .reason,
            Reason::Ok
        );
        assert_eq!(event(&mut arx).reason, Reason::LocalEnded);
        assert_eq!(event(&mut brx).reason, Reason::RemoteEnded);
        assert!(hub.calls.is_empty());
        for (_, _, stop) in slots {
            assert!(stop.is_closed());
        }
        assert!(!hub.busy(&peers[0].device));
        assert_eq!(
            hub.command(&db, peers[0], a, command(Action::End, id, [0; 16]), now)
                .reason,
            Reason::Ok
        );
    }

    #[test]
    fn calls_ttl_ring_attach_and_idle_listener_use_monotonic_time() {
        let (db, peers) = database();
        let now = Instant::now();
        let mut hub = Hub::default();
        let (a, mut arx, astop) = listener(&mut hub, peers[0], now);
        let (b, mut brx, bstop) = listener(&mut hub, peers[1], now);
        let id = [12; 16];
        assert_eq!(
            invite(&mut hub, &db, [peers[0], peers[1]], a, id, now),
            Reason::Ok
        );
        event(&mut brx);
        // Keep signaling live while the independent ringing deadline expires.
        for (who, serial) in [(peers[0], a), (peers[1], b)] {
            let heartbeat = Command {
                request_id: 2,
                action: Action::Heartbeat,
                call_id: [0; 16],
                epoch: 0,
                recipient: [0; 16],
                ciphertext: vec![],
            };
            assert_eq!(
                hub.command(
                    &db,
                    who,
                    serial,
                    heartbeat,
                    now + Duration::from_secs(HEARTBEAT_SECS * 2)
                )
                .reason,
                Reason::Ok
            );
        }
        hub.expire(now + Duration::from_secs(RING_SECS));
        assert_eq!(event(&mut arx).reason, Reason::Expired);
        assert_eq!(event(&mut brx).reason, Reason::Expired);
        assert!(hub.calls.is_empty());
        let next = now + Duration::from_secs(31);
        assert_eq!(
            invite(&mut hub, &db, [peers[0], peers[1]], a, id, next),
            Reason::Ok
        );
        event(&mut brx);
        hub.command(
            &db,
            peers[1],
            b,
            command(Action::Ringing, id, [0; 16]),
            next,
        );
        event(&mut arx);
        hub.command(&db, peers[1], b, command(Action::Accept, id, [0; 16]), next);
        event(&mut arx);
        let (tx, _rx) = mpsc::channel(1);
        let stop = Arc::new(Stop::default());
        hub.attach(
            &db,
            peers[0],
            Attach {
                call_id: id,
                epoch: 1,
                lane: LaneKind::Rtp,
            },
            tx,
            stop.clone(),
            next,
        )
        .unwrap();
        hub.expire(next + Duration::from_secs(ATTACH_SECS));
        assert_eq!(event(&mut arx).reason, Reason::Expired);
        assert_eq!(event(&mut brx).reason, Reason::Expired);
        assert!(stop.is_closed());
        hub.expire(next + Duration::from_secs(LIVENESS_SECS));
        assert!(hub.listeners.is_empty());
        assert!(astop.is_closed() && bstop.is_closed());
    }

    #[test]
    fn calls_busy_capacity_permissions_current_binding_and_unavailable_privacy() {
        let (db, peers) = database();
        let now = Instant::now();
        let mut hub = Hub::default();
        let mut listeners = vec![];
        for peer in peers {
            listeners.push(listener(&mut hub, peer, now));
        }
        for i in 0..2 {
            assert_eq!(
                invite(
                    &mut hub,
                    &db,
                    [peers[i * 2], peers[i * 2 + 1]],
                    listeners[i * 2].0,
                    [i as u8 + 1; 16],
                    now
                ),
                Reason::Ok
            );
            event(&mut listeners[i * 2 + 1].1);
        }
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[4], peers[5]],
                listeners[4].0,
                [3; 16],
                now
            ),
            Reason::Busy
        );
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[0], peers[1]],
                listeners[0].0,
                [4; 16],
                now
            ),
            Reason::Busy
        );
        let missing = Endpoint {
            user: [99; 16],
            device: [99; 32],
        };
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[4], missing],
                listeners[4].0,
                [5; 16],
                now
            ),
            Reason::Unavailable
        );
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[4], peers[0]],
                listeners[4].0,
                [5; 16],
                now
            ),
            Reason::Unavailable
        );
        hub.close_device(&peers[0].device, Reason::Revoked);
        hub.close_device(&peers[2].device, Reason::ConnectionLost);
        // Reverse call on the same accepted one-QR edge needs no reverse row.
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[5], peers[4]],
                listeners[5].0,
                [6; 16],
                now
            ),
            Reason::Ok
        );
        hub.close_pair(&peers[4].user, &peers[5].user);
        db.execute(
            "UPDATE contact_permissions SET state='requested' WHERE user_id=?1",
            [peers[5].user.as_slice()],
        )
        .unwrap();
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[4], peers[5]],
                listeners[4].0,
                [7; 16],
                now
            ),
            Reason::Unavailable
        );
        db.execute(
            "UPDATE contact_permissions SET state='accepted' WHERE user_id=?1",
            [peers[5].user.as_slice()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contact_permissions VALUES(?1,?2,'blocked')",
            rusqlite::params![peers[4].user.as_slice(), peers[5].user.as_slice()],
        )
        .unwrap();
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[4], peers[5]],
                listeners[4].0,
                [7; 16],
                now
            ),
            Reason::Unavailable
        );
        assert!(!current(
            &db,
            Endpoint {
                user: peers[4].user,
                device: peers[5].device
            }
        ));
        db.execute(
            "UPDATE devices SET revoked=1 WHERE device_key=?1",
            [peers[4].device.as_slice()],
        )
        .unwrap();
        assert_eq!(
            invite(
                &mut hub,
                &db,
                [peers[4], peers[5]],
                listeners[4].0,
                [7; 16],
                now
            ),
            Reason::Revoked
        );
        assert!(listeners[4].2.is_closed());
        let mut hub = Hub::default();
        let mut receivers = vec![];
        for i in 1..=MAX_LISTENERS {
            receivers.push(listener(
                &mut hub,
                Endpoint {
                    user: [i as u8; 16],
                    device: [i as u8; 32],
                },
                now,
            ));
        }
        let (tx, _rx) = mpsc::channel(EVENT_CAP);
        assert_eq!(
            hub.listen(missing, tx, Arc::new(Stop::default()), now),
            Err(ERR_BUSY)
        );
    }

    #[test]
    fn calls_event_overflow_closes_signal_and_scoped_routes() {
        let (db, peers) = database();
        let now = Instant::now();
        let mut hub = Hub::default();
        let (a, mut arx, _) = listener(&mut hub, peers[0], now);
        let (_b, _brx, bstop) = listener(&mut hub, peers[1], now);
        let (other, _other_rx, other_stop) = listener(&mut hub, peers[2], now);
        let id = [14; 16];
        for _ in 0..EVENT_CAP {
            let dummy = Event {
                kind: EventKind::Ended,
                call_id: id,
                epoch: 1,
                peer_user: peers[0].user,
                peer_device: peers[0].device,
                remaining_ms: 0,
                reason: Reason::RemoteEnded,
                ciphertext: vec![],
            };
            assert!(hub.emit(peers[1].device, dummy, now));
        }
        assert_eq!(
            invite(&mut hub, &db, [peers[0], peers[1]], a, id, now),
            Reason::Unavailable
        );
        assert!(bstop.is_closed());
        assert!(!hub.listeners.contains_key(&peers[1].device));
        assert!(hub.calls.is_empty());
        assert_eq!(event(&mut arx).kind, EventKind::Ended);
        assert!(hub.live(peers[2], other));
        assert!(!other_stop.is_closed());
    }

    #[test]
    fn calls_accept_and_attach_recheck_permissions_and_exact_device_membership() {
        let (db, peers) = database();
        let now = Instant::now();
        let mut hub = Hub::default();
        let (a, mut arx, _) = listener(&mut hub, peers[0], now);
        let (b, mut brx, _) = listener(&mut hub, peers[1], now);
        let id = [16; 16];
        assert_eq!(
            invite(&mut hub, &db, [peers[0], peers[1]], a, id, now),
            Reason::Ok
        );
        event(&mut brx);
        hub.command(&db, peers[1], b, command(Action::Ringing, id, [0; 16]), now);
        event(&mut arx);
        db.execute(
            "UPDATE contact_permissions SET state='blocked' WHERE user_id=?1",
            [peers[1].user.as_slice()],
        )
        .unwrap();
        assert_eq!(
            hub.command(&db, peers[1], b, command(Action::Accept, id, [0; 16]), now)
                .reason,
            Reason::Unavailable
        );
        assert!(hub.calls.is_empty());
        event(&mut arx);
        event(&mut brx);
        db.execute(
            "UPDATE contact_permissions SET state='accepted' WHERE user_id=?1",
            [peers[1].user.as_slice()],
        )
        .unwrap();
        assert_eq!(
            invite(&mut hub, &db, [peers[0], peers[1]], a, id, now),
            Reason::Ok
        );
        event(&mut brx);
        hub.command(&db, peers[1], b, command(Action::Ringing, id, [0; 16]), now);
        event(&mut arx);
        hub.command(&db, peers[1], b, command(Action::Accept, id, [0; 16]), now);
        event(&mut arx);
        let attach = Attach {
            call_id: id,
            epoch: 1,
            lane: LaneKind::Rtp,
        };
        let (tx, _rx) = mpsc::channel(1);
        assert!(hub
            .attach(
                &db,
                peers[2],
                attach,
                tx.clone(),
                Arc::new(Stop::default()),
                now
            )
            .is_err());
        assert_eq!(
            hub.attach(
                &db,
                Endpoint {
                    user: peers[0].user,
                    device: peers[1].device
                },
                attach,
                tx.clone(),
                Arc::new(Stop::default()),
                now
            ),
            Err(ERR_REVOKED)
        );
        db.execute(
            "UPDATE devices SET revoked=1 WHERE device_key=?1",
            [peers[1].device.as_slice()],
        )
        .unwrap();
        assert_eq!(
            hub.attach(&db, peers[0], attach, tx, Arc::new(Stop::default()), now),
            Err(ERR_BUSY)
        );
        hub.close_device(&peers[1].device, Reason::Revoked);
        assert!(hub.calls.is_empty());
        assert_eq!(event(&mut arx).reason, Reason::Revoked);
    }

    async fn state() -> (Arc<State>, [Endpoint; 6]) {
        let (db, peers) = database();
        let st = State {
            auth: crate::auth::Engine::new().await.unwrap(),
            cfg: Arc::new(crate::Config {
                domain: "call.test".into(),
                listen: String::new(),
                data_dir: std::path::PathBuf::new(),
                blobs_dir: std::path::PathBuf::new(),
                msgctl_sock: std::path::PathBuf::new(),
                schema_version: crate::db::SCHEMA_VERSION,
                noise_key_file: std::path::PathBuf::new(),
                noise_private: [71; 32],
                carrier_cert_file: std::path::PathBuf::new(),
            }),
            counters: Arc::new(crate::Counters::default()),
            db: Arc::new(Mutex::new(db)),
            blob_io: Arc::new(Semaphore::new(1)),
            calls: Arc::new(Mutex::new(Hub::default())),
            live: Arc::new(Mutex::new(HashMap::new())),
            pre: Arc::new(Mutex::new(HashMap::new())),
        };
        (Arc::new(st), peers)
    }

    #[tokio::test]
    async fn calls_server_busy_keeps_durable_mailbox_and_block_revoke_close_scoped_calls() {
        let (st, peers) = state().await;
        let mut send = vec![];
        send.extend_from_slice(&peers[1].user);
        send.extend_from_slice(&[27; 16]);
        send.extend_from_slice(&[1, 2, 3]);
        assert_eq!(
            decode_frame(&crate::send_reply(&st, &st.counters, &peers[0].device, &send).unwrap())
                .unwrap()
                .1,
            OP_SEND_ACK
        );
        let now = Instant::now();
        let (a, mut arx, _) = listener(&mut st.calls.lock().unwrap(), peers[0], now);
        let (_b, mut brx, _) = listener(&mut st.calls.lock().unwrap(), peers[1], now);
        {
            let db = st.db.lock().unwrap();
            assert_eq!(
                invite(
                    &mut st.calls.lock().unwrap(),
                    &db,
                    [peers[0], peers[1]],
                    a,
                    [18; 16],
                    now
                ),
                Reason::Ok
            );
        }
        event(&mut brx);
        for frame in [
            crate::send_reply(&st, &st.counters, &peers[0].device, &send).unwrap(),
            crate::fetch_reply(&st, &st.counters, &peers[1].user, &peers[1].device, &[]).unwrap(),
            crate::blob_reply(&st, &peers[1].user, &peers[1].device, OP_BLOB_GET, &[0; 20]).await,
        ] {
            let (_, opcode, payload, _) = decode_frame(&frame).unwrap();
            assert_eq!((opcode, payload), (OP_ERROR, &[ERR_BUSY][..]));
        }
        assert_eq!(
            st.db
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM mailbox_events", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let mut block = peers[0].user.to_vec();
        block.push(2);
        assert_eq!(
            decode_frame(&crate::contact_reply(
                &st,
                &peers[1].user,
                &peers[1].device,
                OP_CONTACT_DECIDE,
                &block
            ))
            .unwrap()
            .1,
            OP_CONTACT_OK
        );
        assert!(st.calls.lock().unwrap().calls.is_empty());
        assert_eq!(event(&mut arx).reason, Reason::Unavailable);
        assert_eq!(event(&mut brx).reason, Reason::Unavailable);
        assert_eq!(
            decode_frame(
                &crate::fetch_reply(&st, &st.counters, &peers[1].user, &peers[1].device, &[])
                    .unwrap()
            )
            .unwrap()
            .1,
            OP_FETCH_RESP
        );
        // Administrative revoke closes the registered signal after its DB commit.
        crate::msgctl_block(&st, &peers[0].device).unwrap();
        assert!(!st
            .calls
            .lock()
            .unwrap()
            .listeners
            .contains_key(&peers[0].device));
    }

    struct Client {
        socket: TcpStream,
        noise: snow::TransportState,
    }
    impl Client {
        fn encrypted(&mut self, opcode: u8, payload: &[u8]) -> Vec<u8> {
            let inner = encode_frame(opcode, payload).unwrap();
            let mut wire = vec![0; inner.len() + 18];
            let n = self.noise.write_message(&inner, &mut wire[2..]).unwrap();
            wire[..2].copy_from_slice(&(n as u16).to_be_bytes());
            wire.truncate(n + 2);
            wire
        }
        async fn send(&mut self, opcode: u8, payload: &[u8]) {
            let wire = self.encrypted(opcode, payload);
            self.socket.write_all(&wire).await.unwrap();
        }
        async fn recv(&mut self) -> (u8, Vec<u8>) {
            let cipher = timeout(Duration::from_secs(2), crate::read_hs_msg(&mut self.socket))
                .await
                .unwrap()
                .unwrap();
            let mut plain = vec![0; MAX_FRAME];
            let n = self.noise.read_message(&cipher, &mut plain).unwrap();
            let (_, op, p, total) = decode_frame(&plain[..n]).unwrap();
            assert_eq!(total, n);
            (op, p.to_vec())
        }
    }

    async fn connect(st: Arc<State>, who: usize, post: Arc<Semaphore>) -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut socket = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let server_st = st.clone();
        tokio::spawn(crate::handle_conn(server, server_st, permit, post));
        let mut hs = snow::Builder::new(crate::noise::PATTERN.parse().unwrap())
            .local_private_key(&[who as u8 + 41; 32])
            .unwrap()
            .remote_public_key(&crate::noise::pubkey_of(&st.cfg.noise_private))
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut out = [0; 256];
        let n = hs.write_message(&[], &mut out).unwrap();
        crate::write_hs_msg(&mut socket, &out[..n]).await.unwrap();
        let response = crate::read_hs_msg(&mut socket).await.unwrap();
        hs.read_message(&response, &mut out).unwrap();
        let mut client = Client {
            socket,
            noise: hs.into_transport_mode().unwrap(),
        };
        client.send(OP_AUTH_DOMAIN, st.cfg.domain.as_bytes()).await;
        assert_eq!(client.recv().await.0, OP_WELCOME);
        client.send(OP_RESUME, &[]).await;
        assert_eq!(client.recv().await.0, OP_AUTHENTICATED);
        client
    }

    async fn signal(st: Arc<State>, who: usize, post: Arc<Semaphore>) -> Client {
        let mut c = connect(st, who, post).await;
        c.send(OP_CALL_OPEN, &[VERSION]).await;
        assert_eq!(c.recv().await, (OP_CALL_READY, vec![VERSION, PROFILE]));
        c
    }

    async fn ack(c: &mut Client, cmd: Command) -> Ack {
        c.send(OP_CALL_COMMAND, &cmd.encode().unwrap()).await;
        let (op, p) = c.recv().await;
        assert_eq!(op, OP_CALL_ACK);
        Ack::decode(&p).unwrap()
    }

    async fn pushed(c: &mut Client, kind: EventKind) -> Event {
        let (op, p) = c.recv().await;
        assert_eq!(op, OP_CALL_EVENT);
        let e = Event::decode(&p).unwrap();
        assert_eq!(e.kind, kind);
        e
    }

    #[tokio::test]
    async fn calls_actual_tcp_invalid_signal_and_media_are_terminal_not_reconnecting() {
        // Both source owners: corrupt Noise, authenticated trailing frame data,
        // and a correctly encrypted but invalid media callback payload.
        for case in 0..4 {
            let (st, peers) = state().await;
            let post = Arc::new(Semaphore::new(8));
            let id = [37; 16];
            let mut a = signal(st.clone(), 0, post.clone()).await;
            let mut b = signal(st.clone(), 1, post.clone()).await;
            assert_eq!(
                ack(&mut a, command(Action::Invite, id, peers[1].user))
                    .await
                    .reason,
                Reason::Ok
            );
            pushed(&mut b, EventKind::Invite).await;
            for (action, kind) in [
                (Action::Ringing, EventKind::Ringing),
                (Action::Accept, EventKind::Accepted),
            ] {
                assert_eq!(
                    ack(&mut b, command(action, id, [0; 16])).await.reason,
                    Reason::Ok
                );
                pushed(&mut a, kind).await;
            }
            let mut media = Vec::new();
            for index in 0..4 {
                let attach = Attach {
                    call_id: id,
                    epoch: 1,
                    lane: if index % 2 == 0 {
                        LaneKind::Rtp
                    } else {
                        LaneKind::Srtcp
                    },
                };
                let mut c = connect(st.clone(), index / 2, post.clone()).await;
                c.send(OP_CALL_ATTACH, &attach.encode().unwrap()).await;
                assert_eq!(c.recv().await, (OP_CALL_ATTACHED, attach.encode().unwrap()));
                media.push(c);
            }
            pushed(&mut a, EventKind::MediaReady).await;
            pushed(&mut b, EventKind::MediaReady).await;
            let source = if case < 2 { &mut a } else { &mut media[0] };
            let wire = match case {
                0 | 2 => {
                    let mut wire = source.encrypted(OP_RTP, &[1]);
                    *wire.last_mut().unwrap() ^= 1;
                    wire
                }
                1 => {
                    let mut inner = encode_frame(
                        OP_CALL_COMMAND,
                        &command(Action::End, id, [0; 16]).encode().unwrap(),
                    )
                    .unwrap();
                    inner.push(0);
                    let mut wire = vec![0; inner.len() + 18];
                    let n = source.noise.write_message(&inner, &mut wire[2..]).unwrap();
                    wire[..2].copy_from_slice(&(n as u16).to_be_bytes());
                    wire.truncate(n + 2);
                    wire
                }
                3 => source.encrypted(OP_RTP, &[]),
                _ => unreachable!(),
            };
            source.socket.write_all(&wire).await.unwrap();
            let ended = pushed(&mut b, EventKind::Ended).await;
            assert_eq!(
                (ended.call_id, ended.epoch, ended.reason),
                (id, 1, Reason::Invalid)
            );
            assert!(st.calls.lock().unwrap().calls.is_empty());
            drop(a);
            drop(b);
            drop(media);
            timeout(Duration::from_secs(2), async {
                while post.available_permits() != 8 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn calls_actual_tcp_resume_replays_rekey_and_relay_uses_same_call_new_epoch() {
        let (st, peers) = state().await;
        let post = Arc::new(Semaphore::new(8));
        let id = [35; 16];
        let mut unknown = connect(st.clone(), 0, post.clone()).await;
        unknown
            .send(
                OP_CALL_OPEN,
                &Open {
                    resume: Some((id, 1)),
                }
                .encode()
                .unwrap(),
            )
            .await;
        assert_eq!(unknown.recv().await, (OP_ERROR, vec![ERR_EXPIRED]));
        assert!(st.calls.lock().unwrap().calls.is_empty());
        drop(unknown);
        let mut a = signal(st.clone(), 0, post.clone()).await;
        let mut b = signal(st.clone(), 1, post.clone()).await;
        assert_eq!(
            ack(&mut a, command(Action::Invite, id, peers[1].user))
                .await
                .reason,
            Reason::Ok
        );
        pushed(&mut b, EventKind::Invite).await;
        for (action, kind) in [
            (Action::Ringing, EventKind::Ringing),
            (Action::Accept, EventKind::Accepted),
        ] {
            assert_eq!(
                ack(&mut b, command(action, id, [0; 16])).await.reason,
                Reason::Ok
            );
            pushed(&mut a, kind).await;
        }
        // Already accepted, but not yet attached, is also recoverable.
        drop(a);
        assert_eq!(pushed(&mut b, EventKind::Reconnecting).await.epoch, 2);
        let mut a = connect(st.clone(), 0, post.clone()).await;
        a.send(
            OP_CALL_OPEN,
            &Open {
                resume: Some((id, 1)),
            }
            .encode()
            .unwrap(),
        )
        .await;
        assert_eq!(a.recv().await, (OP_CALL_READY, vec![VERSION, PROFILE]));
        assert_eq!(pushed(&mut a, EventKind::Reconnecting).await.epoch, 2);
        let offer = recovery_command(Action::RekeyOffer, id, 2);
        assert_eq!(ack(&mut a, offer.clone()).await.reason, Reason::Ok);
        // Lose the offer event and resume the listener: server replays the exact
        // stored cipher, without another epoch or another encrypt/ratchet step.
        drop(b);
        let mut b = connect(st.clone(), 1, post.clone()).await;
        b.send(
            OP_CALL_OPEN,
            &Open {
                resume: Some((id, 1)),
            }
            .encode()
            .unwrap(),
        )
        .await;
        assert_eq!(b.recv().await, (OP_CALL_READY, vec![VERSION, PROFILE]));
        assert_eq!(pushed(&mut b, EventKind::Reconnecting).await.epoch, 2);
        assert_eq!(
            pushed(&mut b, EventKind::RekeyOffer).await.ciphertext,
            offer.ciphertext
        );
        assert_eq!(
            ack(&mut b, recovery_command(Action::RekeyAnswer, id, 2))
                .await
                .reason,
            Reason::Ok
        );
        assert_eq!(pushed(&mut a, EventKind::RekeyAnswer).await.epoch, 2);
        assert_eq!(ack(&mut a, offer).await.reason, Reason::Ok);
        let mut media = vec![];
        for index in 0..4 {
            let attach = Attach {
                call_id: id,
                epoch: 2,
                lane: if index % 2 == 0 {
                    LaneKind::Rtp
                } else {
                    LaneKind::Srtcp
                },
            };
            let mut c = connect(st.clone(), index / 2, post.clone()).await;
            c.send(OP_CALL_ATTACH, &attach.encode().unwrap()).await;
            assert_eq!(c.recv().await, (OP_CALL_ATTACHED, attach.encode().unwrap()));
            media.push(c);
        }
        assert_eq!(pushed(&mut a, EventKind::MediaReady).await.epoch, 2);
        assert_eq!(pushed(&mut b, EventKind::MediaReady).await.epoch, 2);
        media[0].send(OP_RTP, &[7; RTP_CAP]).await;
        assert_eq!(media[2].recv().await, (OP_RTP, vec![7; RTP_CAP]));
        media[3].send(OP_SRTCP, &[8; SRTCP_CAP]).await;
        assert_eq!(media[1].recv().await, (OP_SRTCP, vec![8; SRTCP_CAP]));
        assert!(st.calls.lock().unwrap().calls[&id]
            .recovery_deadline
            .is_some());
        tokio::time::sleep(Duration::from_secs(RECOVERY_STABLE_SECS)).await;
        for c in [&mut a, &mut b] {
            assert_eq!(
                ack(c, recovery_command(Action::Recovered, id, 2))
                    .await
                    .reason,
                Reason::Ok
            );
        }
        assert_eq!(pushed(&mut a, EventKind::Recovered).await.epoch, 2);
        assert_eq!(pushed(&mut b, EventKind::Recovered).await.epoch, 2);
        assert!(st.calls.lock().unwrap().calls[&id]
            .recovery_deadline
            .is_none());
        drop(media.remove(0));
        assert_eq!(pushed(&mut a, EventKind::Reconnecting).await.epoch, 3);
        assert_eq!(pushed(&mut b, EventKind::Reconnecting).await.epoch, 3);
        assert_eq!(
            ack(&mut a, command(Action::End, id, [0; 16])).await.reason,
            Reason::Ok
        );
        assert_eq!(
            pushed(&mut a, EventKind::Ended).await.reason,
            Reason::LocalEnded
        );
        assert_eq!(
            pushed(&mut b, EventKind::Ended).await.reason,
            Reason::RemoteEnded
        );
        assert!(st.calls.lock().unwrap().calls.is_empty());
        drop(a);
        drop(b);
        drop(media);
        timeout(Duration::from_secs(2), async {
            while post.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn calls_actual_tcp_push_relay_partial_io_revoke_and_permit_cleanup() {
        let (st, peers) = state().await;
        let post = Arc::new(Semaphore::new(8));
        let mut a = signal(st.clone(), 0, post.clone()).await;
        let mut b = signal(st.clone(), 1, post.clone()).await;
        let id = [21; 16];
        assert_eq!(
            ack(&mut a, command(Action::Invite, id, peers[1].user))
                .await
                .reason,
            Reason::Ok
        );
        assert_eq!(
            pushed(&mut b, EventKind::Invite).await.ciphertext,
            vec![123; 48]
        );
        assert_eq!(
            ack(&mut b, command(Action::Ringing, id, [0; 16]))
                .await
                .reason,
            Reason::Ok
        );
        pushed(&mut a, EventKind::Ringing).await;
        assert_eq!(
            ack(&mut b, command(Action::Accept, id, [0; 16]))
                .await
                .reason,
            Reason::Ok
        );
        pushed(&mut a, EventKind::Accepted).await;
        let mut media = vec![];
        for index in 0..4 {
            let attach = Attach {
                call_id: id,
                epoch: 1,
                lane: if index % 2 == 0 {
                    LaneKind::Rtp
                } else {
                    LaneKind::Srtcp
                },
            };
            let mut c = connect(st.clone(), index / 2, post.clone()).await;
            c.send(OP_CALL_ATTACH, &attach.encode().unwrap()).await;
            assert_eq!(c.recv().await, (OP_CALL_ATTACHED, attach.encode().unwrap()));
            media.push(c);
        }
        pushed(&mut a, EventKind::MediaReady).await;
        pushed(&mut b, EventKind::MediaReady).await;
        assert_eq!(
            post.available_permits(),
            2,
            "handoff must retain six post-auth slots"
        );
        // A partial Noise header/body survives reverse media and pushed signaling.
        let wire = media[0].encrypted(OP_RTP, &[7; RTP_CAP]);
        media[0].socket.write_all(&wire[..1]).await.unwrap();
        media[2].send(OP_RTP, &[8; RTP_CAP]).await;
        assert_eq!(media[0].recv().await, (OP_RTP, vec![8; RTP_CAP]));
        media[0].socket.write_all(&wire[1..10]).await.unwrap();
        media[2].send(OP_RTP, &[9; RTP_CAP]).await;
        assert_eq!(media[0].recv().await, (OP_RTP, vec![9; RTP_CAP]));
        media[0].socket.write_all(&wire[10..]).await.unwrap();
        assert_eq!(media[2].recv().await, (OP_RTP, vec![7; RTP_CAP]));
        media[1].send(OP_SRTCP, &[6; SRTCP_CAP]).await;
        assert_eq!(media[3].recv().await, (OP_SRTCP, vec![6; SRTCP_CAP]));
        // Retire a half-read frame plus queued relay work; revocation is scoped.
        let abandoned = media[0].encrypted(OP_RTP, &[4; RTP_CAP]);
        media[0].socket.write_all(&abandoned[..1]).await.unwrap();
        crate::msgctl_block(&st, &peers[0].device).unwrap();
        assert_eq!(
            pushed(&mut b, EventKind::Ended).await.reason,
            Reason::Revoked
        );
        timeout(Duration::from_secs(2), async {
            while post.available_permits() != 7 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(st.calls.lock().unwrap().calls.is_empty());
        assert!(!st
            .live
            .lock()
            .unwrap()
            .contains_key(peers[0].device.as_slice()));
        for c in &mut media {
            let mut byte = [0; 1];
            assert_eq!(
                timeout(Duration::from_secs(1), c.socket.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
        drop(b);
        timeout(Duration::from_secs(2), async {
            while post.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(st.calls.lock().unwrap().listeners.is_empty());
    }
}

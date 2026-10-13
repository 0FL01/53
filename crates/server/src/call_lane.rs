//! One call stream, one Noise owner. Partial IO offsets survive select cancellation.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dmsg_protocol::{
    call::{OP_RTP, OP_SRTCP},
    decode_frame, encode_frame, HEADER_LEN, MAX_FRAME,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Notify};
use zeroize::Zeroizing;

pub struct Frame {
    pub opcode: u8,
    pub payload: Vec<u8>,
    pub deadline: Option<Instant>,
}

impl Frame {
    pub fn control(opcode: u8, payload: Vec<u8>) -> Self {
        Self {
            opcode,
            payload,
            deadline: None,
        }
    }
}

#[derive(Default)]
pub struct Stop {
    closed: AtomicBool,
    notify: Notify,
}

impl Stop {
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // One actor per Stop; preserve a permit before its first poll.
        self.notify.notify_one();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        if !self.is_closed() {
            self.notify.notified().await;
        }
    }
}

/// Numeric metadata owned by the lane task, independent of cancellation/route state.
#[derive(Default)]
pub(super) struct MediaCounters {
    pub(super) authenticated_media_rx: u64,
    pub(super) complete_tcp_media_writes: u64,
    pub(super) expired_plaintext_before_noise: u64,
    pub(super) last_srtcp_rx_index: Option<u32>,
    pub(super) last_srtcp_tcp_write_index: Option<u32>,
}

/// Fixed AES_CM_128_HMAC_SHA1_80, no MKI: RTCP header, index, then 10-byte tag.
/// Diagnostic metadata only; this does not authenticate or validate media.
pub(super) fn srtcp_index(opcode: u8, payload: &[u8]) -> Option<u32> {
    if opcode != OP_SRTCP || payload.len() < 8 + 4 + 10 {
        return None;
    }
    let start = payload.len() - 4 - 10;
    Some(u32::from_be_bytes(payload[start..start + 4].try_into().ok()?) & 0x7fff_ffff)
}

/// The callback never awaits: ACKs and pushed events share this Noise owner.
/// The request channel is the sole queued plaintext slot for media (capacity 1).
pub(super) async fn run(
    stream: TcpStream,
    mut noise: snow::TransportState,
    mut requests: mpsc::Receiver<Frame>,
    stop: Arc<Stop>,
    revoked: Arc<Notify>,
    inbound_cap: usize,
    initial: Frame,
    counters: &mut MediaCounters,
    mut receive: impl FnMut(u8, &[u8], Instant) -> Result<Option<Frame>, ()>,
) -> Result<(), ()> {
    let (mut reader, mut writer) = stream.into_split();
    let bound = inbound_cap + HEADER_LEN + 16;
    let mut cipher = vec![0; bound + 2];
    let mut filled = 0;
    let mut wanted = 2;
    let mut plain = Zeroizing::new(vec![0; inbound_cap + HEADER_LEN]);
    let mut reply = Some(initial);
    let mut wire = Vec::new();
    let mut written = 0;
    let mut writing_media = false;
    let mut writing_srtcp_index = None;

    loop {
        if stop.is_closed() {
            return Err(());
        }
        // Commitment is a synchronous operation, never a cancellable future.
        if wire.is_empty() {
            if let Some(frame) = reply.take() {
                if !frame.deadline.is_some_and(|d| d <= Instant::now()) {
                    let inner =
                        Zeroizing::new(encode_frame(frame.opcode, &frame.payload).map_err(|_| ())?);
                    if inner.len() > MAX_FRAME {
                        return Err(());
                    }
                    wire.resize(inner.len() + 18, 0);
                    // Last deadline check before advancing Noise, not after it.
                    if frame.deadline.is_some_and(|d| d <= Instant::now()) {
                        counters.expired_plaintext_before_noise =
                            counters.expired_plaintext_before_noise.saturating_add(1);
                        wire.clear();
                        continue;
                    }
                    writing_srtcp_index = srtcp_index(frame.opcode, &frame.payload);
                    let length = noise
                        .write_message(&inner, &mut wire[2..])
                        .map_err(|_| ())?;
                    wire[..2].copy_from_slice(&(length as u16).to_be_bytes());
                    wire.truncate(length + 2);
                    writing_media = matches!(frame.opcode, OP_RTP | OP_SRTCP);
                } else {
                    counters.expired_plaintext_before_noise =
                        counters.expired_plaintext_before_noise.saturating_add(1);
                }
            }
        }
        tokio::select! {
            _ = stop.cancelled() => return Err(()),
            _ = revoked.notified() => return Err(()),
            read = reader.read(&mut cipher[filled..wanted]), if reply.is_none() => {
                let count = read.map_err(|_| ())?;
                if count == 0 { return Err(()); }
                filled += count;
                if filled != wanted { continue; }
                if wanted == 2 {
                    let length = usize::from(u16::from_be_bytes([cipher[0], cipher[1]]));
                    if !(HEADER_LEN + 16..=bound).contains(&length) { return Err(()); }
                    wanted = length + 2;
                } else {
                    let completed = Instant::now();
                    let length = noise.read_message(&cipher[2..wanted], &mut plain).map_err(|_| ())?;
                    let (_, opcode, payload, consumed) = decode_frame(&plain[..length]).map_err(|_| ())?;
                    if consumed != length || payload.len() > inbound_cap { return Err(()); }
                    if matches!(opcode, OP_RTP | OP_SRTCP) {
                        counters.authenticated_media_rx = counters.authenticated_media_rx.saturating_add(1);
                    }
                    if let Some(index) = srtcp_index(opcode, payload) {
                        counters.last_srtcp_rx_index = Some(index);
                    }
                    reply = receive(opcode, payload, completed)?;
                    plain.fill(0);
                    cipher[..wanted].fill(0);
                    filled = 0;
                    wanted = 2;
                    if matches!(opcode, OP_RTP | OP_SRTCP) {
                        // Let a ready peer owner drain its slot before more hot ingress.
                        tokio::task::yield_now().await;
                    }
                }
            }
            write = writer.write(&wire[written..]), if !wire.is_empty() => {
                let count = write.map_err(|_| ())?;
                if count == 0 { return Err(()); }
                written += count;
                if written == wire.len() {
                    // Complete framed TCP write, not a Noise commitment or peer delivery.
                    if writing_media {
                        counters.complete_tcp_media_writes = counters.complete_tcp_media_writes.saturating_add(1);
                    }
                    if let Some(index) = writing_srtcp_index.take() {
                        counters.last_srtcp_tcp_write_index = Some(index);
                    }
                    wire.clear();
                    written = 0;
                }
            }
            request = requests.recv(), if wire.is_empty() && reply.is_none() => {
                reply = Some(request.ok_or(())?);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmsg_protocol::call::{OP_RTP, RTP_CAP};
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    #[test]
    fn calls_srtcp_index_is_length_checked_fixed_profile_metadata() {
        // Dummy trailer markers are not media-authentication fixtures.
        let mut payload = [0xa5; 22];
        for length in 0..payload.len() {
            assert_eq!(srtcp_index(OP_SRTCP, &payload[..length]), None);
        }
        for (wire_index, expected) in [
            (0_u32, 0),
            (0x8000_0000, 0),
            (0x8123_4567, 0x0123_4567),
            (u32::MAX, 0x7fff_ffff),
        ] {
            payload[8..12].copy_from_slice(&wire_index.to_be_bytes());
            assert_eq!(srtcp_index(OP_SRTCP, &payload), Some(expected));
            assert_eq!(srtcp_index(OP_RTP, &payload), None);
        }
    }

    async fn paired() -> (
        TcpStream,
        snow::TransportState,
        TcpStream,
        snow::TransportState,
    ) {
        let private = [71; 32];
        let mut responder = snow::Builder::new(crate::noise::PATTERN.parse().unwrap())
            .local_private_key(&private)
            .unwrap()
            .build_responder()
            .unwrap();
        let mut initiator = snow::Builder::new(crate::noise::PATTERN.parse().unwrap())
            .local_private_key(&[72; 32])
            .unwrap()
            .remote_public_key(&crate::noise::pubkey_of(&private))
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut wire = [0; 256];
        let mut scratch = [0; 256];
        let n = initiator.write_message(&[], &mut wire).unwrap();
        responder.read_message(&wire[..n], &mut scratch).unwrap();
        let n = responder.write_message(&[], &mut wire).unwrap();
        initiator.read_message(&wire[..n], &mut scratch).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (
            server,
            responder.into_transport_mode().unwrap(),
            client,
            initiator.into_transport_mode().unwrap(),
        )
    }

    async fn packet(socket: &mut TcpStream, noise: &mut snow::TransportState) -> Vec<u8> {
        let cipher = timeout(Duration::from_secs(2), crate::read_hs_msg(socket))
            .await
            .unwrap()
            .unwrap();
        let mut plain = vec![0; MAX_FRAME];
        let n = noise.read_message(&cipher, &mut plain).unwrap();
        let (_, op, p, total) = decode_frame(&plain[..n]).unwrap();
        assert_eq!(total, n);
        assert_eq!(op, OP_RTP);
        p.to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn calls_lane_ready_burst_reaches_writable_peer_without_full() {
        const BURST: usize = 4;
        let hop = Duration::from_millis(20);
        let mut observations = Vec::new();
        // Each iteration is a same-kind pair, as in the actual media routes.
        for opcode in [OP_RTP, OP_SRTCP] {
            let (source, source_noise, mut ingress, mut ingress_noise) = paired().await;
            let (destination, destination_noise, mut peer, mut peer_noise) = paired().await;
            let (_source_tx, source_requests) = mpsc::channel(1);
            let (peer_tx, peer_requests) = mpsc::channel(1);
            let source_stop = Arc::new(Stop::default());
            let destination_stop = Arc::new(Stop::default());
            let actor_stop = destination_stop.clone();
            let destination_actor = tokio::spawn(async move {
                let mut counters = MediaCounters::default();
                let result = run(
                    destination,
                    destination_noise,
                    peer_requests,
                    actor_stop,
                    Arc::new(Notify::new()),
                    RTP_CAP,
                    Frame::control(OP_RTP, vec![0]),
                    &mut counters,
                    |_, _, _| Err(()),
                )
                .await;
                (result, counters)
            });
            let outgoing = peer_tx.clone();
            let actor_stop = source_stop.clone();
            let source_actor = tokio::spawn(async move {
                let mut counters = MediaCounters::default();
                let (mut queued, mut full, mut route_expired) = (0, 0, 0);
                let result = run(
                    source,
                    source_noise,
                    source_requests,
                    actor_stop,
                    Arc::new(Notify::new()),
                    RTP_CAP,
                    Frame::control(OP_RTP, vec![0]),
                    &mut counters,
                    |op, payload, completed| {
                        if op != opcode || payload.is_empty() || payload.len() > RTP_CAP {
                            return Err(());
                        }
                        if completed + hop <= Instant::now() {
                            route_expired += 1;
                            return Ok(None);
                        }
                        match outgoing.try_send(Frame {
                            opcode: op,
                            payload: payload.to_vec(),
                            deadline: Some(completed + hop),
                        }) {
                            Ok(()) => queued += 1,
                            Err(mpsc::error::TrySendError::Full(_)) => full += 1,
                            Err(mpsc::error::TrySendError::Closed(_)) => return Err(()),
                        }
                        Ok(None)
                    },
                )
                .await;
                (result, counters, queued, full, route_expired)
            });
            let (warmed, ready) = tokio::sync::oneshot::channel();
            let reader = tokio::spawn(async move {
                assert_eq!(packet(&mut peer, &mut peer_noise).await, vec![0]);
                warmed.send(()).unwrap();
                let mut packets = Vec::new();
                for _ in 0..BURST {
                    let Ok(cipher) =
                        timeout(Duration::from_millis(200), crate::read_hs_msg(&mut peer)).await
                    else {
                        break;
                    };
                    let cipher = cipher.unwrap();
                    let mut plain = vec![0; MAX_FRAME];
                    let length = peer_noise.read_message(&cipher, &mut plain).unwrap();
                    let (_, op, payload, consumed) = decode_frame(&plain[..length]).unwrap();
                    assert_eq!(consumed, length);
                    packets.push((op, payload.to_vec()));
                }
                packets
            });
            // Both initial replies are drained; the downstream reader is polling
            // real TCP and the peer owner's sole plaintext slot is empty.
            assert_eq!(packet(&mut ingress, &mut ingress_noise).await, vec![0]);
            ready.await.unwrap();
            assert_eq!(peer_tx.capacity(), 1);

            let mut expected = Vec::new();
            let mut burst = Vec::new();
            for sequence in 1..=BURST {
                // Opaque media fixtures: realistic sizes/headers and unique
                // sequence/content, authenticated by Noise, not by SRTP here.
                let mut payload = vec![sequence as u8; if opcode == OP_RTP { RTP_CAP } else { 22 }];
                if opcode == OP_RTP {
                    payload[..2].copy_from_slice(&[0x80, 111]);
                    payload[2..4].copy_from_slice(&(sequence as u16).to_be_bytes());
                    payload[4..8].copy_from_slice(&(sequence as u32 * 640).to_be_bytes());
                    payload[8..12].copy_from_slice(&1_u32.to_be_bytes());
                } else {
                    payload[..4].copy_from_slice(&[0x80, 201, 0, 1]);
                    payload[4..8].copy_from_slice(&1_u32.to_be_bytes());
                    payload[8..12].copy_from_slice(&(0x8000_0000 | sequence as u32).to_be_bytes());
                }
                let inner = encode_frame(opcode, &payload).unwrap();
                let mut wire = vec![0; inner.len() + 18];
                let length = ingress_noise.write_message(&inner, &mut wire[2..]).unwrap();
                wire[..2].copy_from_slice(&(length as u16).to_be_bytes());
                burst.extend_from_slice(&wire[..length + 2]);
                expected.push((opcode, payload));
            }
            // One small, supported-input TCP burst, with no blocked consumer,
            // oversized output, owner suspension, or relay retry.
            ingress.write_all(&burst).await.unwrap();
            let packets = reader.await.unwrap();
            source_stop.close();
            destination_stop.close();
            let (result, source_counters, queued, full, route_expired) =
                source_actor.await.unwrap();
            assert!(result.is_err());
            let (result, destination_counters) = destination_actor.await.unwrap();
            assert!(result.is_err());
            eprintln!(
                "ready burst opcode={opcode}: ingress={} queued={queued} Full={full} route_expired={route_expired} peer_writes={} peer_authenticated={} pre_noise_expired={}",
                source_counters.authenticated_media_rx,
                destination_counters.complete_tcp_media_writes - 1,
                packets.len(),
                destination_counters.expired_plaintext_before_noise,
            );
            observations.push((
                opcode,
                source_counters,
                destination_counters,
                queued,
                full,
                route_expired,
                packets,
                expected,
            ));
        }
        for (opcode, source, destination, queued, full, route_expired, packets, expected) in
            observations
        {
            assert_eq!(source.authenticated_media_rx, BURST as u64);
            assert_eq!(full, 0, "writable peer lost media to Full, opcode={opcode}");
            assert_eq!(route_expired, 0);
            assert_eq!(queued, BURST);
            assert_eq!(packets, expected);
            assert_eq!(source.complete_tcp_media_writes, 1);
            assert_eq!(destination.complete_tcp_media_writes, BURST as u64 + 1);
            assert_eq!(source.expired_plaintext_before_noise, 0);
            assert_eq!(destination.expired_plaintext_before_noise, 0);
            assert_eq!(destination.authenticated_media_rx, 0);
            let last_index = (opcode == OP_SRTCP).then_some(BURST as u32);
            assert_eq!(source.last_srtcp_rx_index, last_index);
            assert_eq!(destination.last_srtcp_tcp_write_index, last_index);
        }
    }

    #[tokio::test]
    async fn calls_lane_expired_plaintext_never_advances_noise_and_stop_drops_queue() {
        let (server, server_noise, mut peer, mut peer_noise) = paired().await;
        let (tx, requests) = mpsc::channel(1);
        let stop = Arc::new(Stop::default());
        let actor_stop = stop.clone();
        let actor = tokio::spawn(async move {
            let mut counters = MediaCounters::default();
            let result = run(
                server,
                server_noise,
                requests,
                actor_stop,
                Arc::new(Notify::new()),
                RTP_CAP,
                Frame::control(OP_RTP, vec![1]),
                &mut counters,
                |_, _, _| Ok(None),
            )
            .await;
            (result, counters)
        });
        assert_eq!(packet(&mut peer, &mut peer_noise).await, vec![1]);
        tx.send(Frame {
            opcode: OP_RTP,
            payload: vec![2],
            deadline: Some(Instant::now()),
        })
        .await
        .unwrap();
        tx.send(Frame::control(OP_RTP, vec![3])).await.unwrap();
        // If the expired packet used a nonce, the peer's next read cannot authenticate.
        assert_eq!(packet(&mut peer, &mut peer_noise).await, vec![3]);
        peer.write_all(&[0]).await.unwrap();
        tx.send(Frame {
            opcode: OP_RTP,
            payload: vec![4],
            deadline: Some(Instant::now()),
        })
        .await
        .unwrap();
        stop.close();
        let (result, counters) = timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(counters.authenticated_media_rx, 0);
        assert_eq!(counters.complete_tcp_media_writes, 2);
        assert_eq!(counters.expired_plaintext_before_noise, 1);
        assert_eq!(counters.last_srtcp_rx_index, None);
        assert_eq!(counters.last_srtcp_tcp_write_index, None);
        assert!(tx.is_closed());
        let mut byte = [0];
        assert_eq!(
            timeout(Duration::from_secs(1), peer.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn calls_lane_inbound_still_progresses_under_actual_tcp_write_backpressure() {
        let (server, server_noise, mut peer, mut peer_noise) = paired().await;
        let (tx, requests) = mpsc::channel(1);
        let stop = Arc::new(Stop::default());
        let (received, mut incoming) = mpsc::channel(1);
        let actor_stop = stop.clone();
        let actor = tokio::spawn(async move {
            let mut counters = MediaCounters::default();
            let result = run(
                server,
                server_noise,
                requests,
                actor_stop,
                Arc::new(Notify::new()),
                RTP_CAP,
                Frame::control(OP_RTP, vec![1]),
                &mut counters,
                move |op, p, _| {
                    received.try_send((op, p.to_vec())).map_err(|_| ())?;
                    Ok(None)
                },
            )
            .await;
            (result, counters)
        });
        // Exercise the actor's cancellation-safe write offsets without changing
        // production media caps. Large output is solely a backpressure fixture.
        // Dummy SRTCP trailers test indices, not media authentication.
        let mut blocked = false;
        let mut enqueued = 0;
        for _ in 0..1024 {
            let mut payload = vec![9; dmsg_protocol::MAX_PAYLOAD];
            let start = payload.len() - 14;
            payload[start..start + 4]
                .copy_from_slice(&(0x8000_0000 | (enqueued + 1) as u32).to_be_bytes());
            if timeout(
                Duration::from_millis(20),
                tx.send(Frame::control(OP_SRTCP, payload)),
            )
            .await
            .is_err()
            {
                blocked = true;
                break;
            }
            enqueued += 1;
        }
        assert!(blocked, "socket never reached transmit backpressure");
        let mut inbound = vec![7; 22];
        inbound[8..12].copy_from_slice(&0x8000_0007_u32.to_be_bytes());
        let inner = encode_frame(OP_SRTCP, &inbound).unwrap();
        let mut wire = vec![0; inner.len() + 18];
        let n = peer_noise.write_message(&inner, &mut wire[2..]).unwrap();
        wire[..2].copy_from_slice(&(n as u16).to_be_bytes());
        peer.write_all(&wire[..n + 2]).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), incoming.recv())
                .await
                .unwrap()
                .unwrap(),
            (OP_SRTCP, inbound)
        );
        stop.close();
        let (result, counters) = timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(counters.authenticated_media_rx, 1);
        assert_eq!(counters.expired_plaintext_before_noise, 0);
        assert!(counters.complete_tcp_media_writes > 0);
        // Initial + enqueued frames, minus one queued and one committed/blocked.
        assert!(counters.complete_tcp_media_writes < enqueued);
        assert_eq!(counters.last_srtcp_rx_index, Some(7));
        // Initial RTP has no index; only whole writes advance the SRTCP marker.
        assert_eq!(
            counters.last_srtcp_tcp_write_index,
            Some((counters.complete_tcp_media_writes - 1) as u32)
        );
        assert!(tx.is_closed());
    }
}

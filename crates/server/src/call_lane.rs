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

/// Scalars owned by the lane task, independent of cancellation/route state.
#[derive(Default)]
pub(super) struct MediaCounters {
    pub(super) authenticated_media_rx: u64,
    pub(super) complete_tcp_media_writes: u64,
    pub(super) expired_plaintext_before_noise: u64,
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
                    reply = receive(opcode, payload, completed)?;
                    plain.fill(0);
                    cipher[..wanted].fill(0);
                    filled = 0;
                    wanted = 2;
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
        let mut blocked = false;
        let mut enqueued = 0;
        for _ in 0..1024 {
            if timeout(
                Duration::from_millis(20),
                tx.send(Frame::control(OP_RTP, vec![9; dmsg_protocol::MAX_PAYLOAD])),
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
        let inner = encode_frame(OP_RTP, &[7]).unwrap();
        let mut wire = vec![0; inner.len() + 18];
        let n = peer_noise.write_message(&inner, &mut wire[2..]).unwrap();
        wire[..2].copy_from_slice(&(n as u16).to_be_bytes());
        peer.write_all(&wire[..n + 2]).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), incoming.recv())
                .await
                .unwrap()
                .unwrap(),
            (OP_RTP, vec![7])
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
        assert!(tx.is_closed());
    }
}

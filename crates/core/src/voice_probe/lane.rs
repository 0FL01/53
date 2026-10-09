//! One private probe lane: a single Noise owner, bounded queues, cancellable IO.

use std::time::Instant;

use dmsg_protocol::{decode_frame, encode_frame, HEADER_LEN, MAX_FRAME};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

pub const OP_BIND: u8 = 240;
pub const OP_BOUND: u8 = 241;
pub const OP_RTP: u8 = 242;
pub const OP_RTCP: u8 = 243;

pub struct SendRequest {
    pub opcode: u8,
    pub payload: Vec<u8>,
    pub deadline: Option<Instant>,
    pub committed: Option<oneshot::Sender<Commit>>,
}

/// Commitment is nonce advancement, not TCP completion or a remote ACK.
pub enum Commit {
    Committed { framed_bytes: usize, at: Instant },
    Dropped,
}

pub struct Lane {
    pub outgoing: mpsc::Sender<SendRequest>,
    pub incoming: mpsc::Receiver<Result<(u8, Vec<u8>), String>>,
    actor: Option<JoinHandle<()>>,
}

impl Lane {
    /// Abandon the entire ordered stream, including any committed partial frame.
    pub fn close(&mut self) {
        if let Some(actor) = &self.actor {
            actor.abort();
        }
        self.incoming.close();
        while self.incoming.try_recv().is_ok() {}
    }

    /// Relay retirement joins cancellation before a replacement can be started.
    pub(super) async fn joined_close(&mut self) {
        self.close();
        if let Some(actor) = self.actor.take() {
            let _ = actor.await;
        }
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn spawn(stream: TcpStream, noise: snow::TransportState) -> Result<Lane, String> {
    stream
        .set_nodelay(true)
        .map_err(|_| "probe socket configuration failed".to_string())?;
    let (outgoing, mut requests) = mpsc::channel(1);
    let (received, incoming) = mpsc::channel(1);
    let actor = tokio::spawn(async move {
        if let Err(error) = run(stream, noise, &mut requests, &received).await {
            // Never wait for an unresponsive consumer during teardown.
            let _ = received.try_send(Err(error));
        }
        requests.close();
        while let Ok(request) = requests.try_recv() {
            dropped(request);
        }
    });
    Ok(Lane {
        outgoing,
        incoming,
        actor: Some(actor),
    })
}

fn dropped(request: SendRequest) {
    if let Some(notify) = request.committed {
        let _ = notify.send(Commit::Dropped);
    }
}

async fn run(
    stream: TcpStream,
    mut noise: snow::TransportState,
    requests: &mut mpsc::Receiver<SendRequest>,
    received: &mpsc::Sender<Result<(u8, Vec<u8>), String>>,
) -> Result<(), String> {
    let (mut reader, mut writer) = stream.into_split();
    // Offsets live outside select futures: cancelled read/write calls lose no bytes.
    let mut cipher = vec![0; MAX_FRAME + 16 + 2];
    let mut filled = 0;
    let mut wanted = 2;
    let mut plain = Zeroizing::new(vec![0; MAX_FRAME]);
    let mut inbound = None;
    let mut wire = Vec::new();
    let mut written = 0;

    loop {
        tokio::select! {
            _ = received.closed() => return Err("probe consumer closed".into()),
            read = reader.read(&mut cipher[filled..wanted]), if inbound.is_none() => {
                let count = read.map_err(|_| "probe read failed".to_string())?;
                if count == 0 {
                    return Err("probe lane closed".into());
                }
                filled += count;
                if filled != wanted {
                    continue;
                }
                if wanted == 2 {
                    let length = usize::from(u16::from_be_bytes([cipher[0], cipher[1]]));
                    if !(HEADER_LEN + 16..=MAX_FRAME + 16).contains(&length) {
                        return Err("probe cipher length rejected".into());
                    }
                    wanted = length + 2;
                } else {
                    let length = noise.read_message(&cipher[2..wanted], &mut plain)
                        .map_err(|_| "probe authentication failed".to_string())?;
                    let (_, opcode, payload, consumed) = decode_frame(&plain[..length])
                        .map_err(|_| "probe frame rejected".to_string())?;
                    if consumed != length {
                        return Err("probe trailing frame bytes".into());
                    }
                    inbound = Some(Ok((opcode, payload.to_vec())));
                    filled = 0;
                    wanted = 2;
                }
            }
            permit = received.reserve(), if inbound.is_some() => {
                let permit = permit.map_err(|_| "probe consumer closed".to_string())?;
                permit.send(inbound.take().expect("guarded pending frame"));
            }
            write = writer.write(&wire[written..]), if !wire.is_empty() => {
                let count = write.map_err(|_| "probe write failed".to_string())?;
                if count == 0 {
                    return Err("probe write closed".into());
                }
                written += count;
                if written == wire.len() {
                    wire.clear();
                    written = 0;
                }
            }
            request = requests.recv(), if wire.is_empty() => {
                let request = request.ok_or_else(|| "probe producer closed".to_string())?;
                let inner = Zeroizing::new(encode_frame(request.opcode, &request.payload)
                    .map_err(|_| "probe outgoing frame rejected".to_string())?);
                wire.resize(inner.len() + 16 + 2, 0);
                // Last check before advancing Noise. The channel is the sole pending
                // plaintext slot; it is not drained while a committed frame exists.
                if request.deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    wire.clear();
                    dropped(request);
                    continue;
                }
                let length = noise.write_message(&inner, &mut wire[2..])
                    .map_err(|_| "probe encryption failed".to_string())?;
                let prefix = u16::try_from(length)
                    .map_err(|_| "probe outgoing length rejected".to_string())?;
                wire[..2].copy_from_slice(&prefix.to_be_bytes());
                wire.truncate(length + 2);
                if let Some(notify) = request.committed {
                    let _ = notify.send(Commit::Committed { framed_bytes: wire.len(), at: Instant::now() });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    async fn paired() -> (Lane, TcpStream, snow::TransportState) {
        // Public deterministic fixture keys, never application identities.
        let private = [71; 32];
        let mut responder = snow::Builder::new(crate::transport::PATTERN.parse().unwrap())
            .local_private_key(&private)
            .unwrap()
            .build_responder()
            .unwrap();
        let mut initiator = snow::Builder::new(crate::transport::PATTERN.parse().unwrap())
            .local_private_key(&[72; 32])
            .unwrap()
            .remote_public_key(&crate::olm::device_pubkey(&private))
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut buffer = [0; 256];
        let mut scratch = [0; 256];
        let length = initiator.write_message(&[], &mut buffer).unwrap();
        responder
            .read_message(&buffer[..length], &mut scratch)
            .unwrap();
        let length = responder.write_message(&[], &mut buffer).unwrap();
        initiator
            .read_message(&buffer[..length], &mut scratch)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        (
            spawn(client, initiator.into_transport_mode().unwrap()).unwrap(),
            peer,
            responder.into_transport_mode().unwrap(),
        )
    }

    fn encrypted(noise: &mut snow::TransportState, inner: &[u8]) -> Vec<u8> {
        let mut wire = vec![0; inner.len() + 18];
        let length = noise.write_message(inner, &mut wire[2..]).unwrap();
        wire[..2].copy_from_slice(&u16::try_from(length).unwrap().to_be_bytes());
        wire.truncate(length + 2);
        wire
    }

    async fn send(lane: &Lane, payload: Vec<u8>, deadline: Option<Instant>) -> Commit {
        let (committed, receipt) = oneshot::channel();
        lane.outgoing
            .send(SendRequest {
                opcode: OP_RTP,
                payload,
                deadline,
                committed: Some(committed),
            })
            .await
            .unwrap();
        timeout(Duration::from_secs(2), receipt)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn expired_request_does_not_advance_noise_and_counts_real_framing() {
        let (mut lane, peer, noise) = paired().await;
        let mut other = spawn(peer, noise).unwrap();
        assert!(matches!(
            send(&lane, vec![1], Some(Instant::now())).await,
            Commit::Dropped
        ));
        let before = Instant::now();
        match send(&lane, vec![2; 100], None).await {
            Commit::Committed { framed_bytes, at } => {
                assert_eq!(framed_bytes, 122);
                assert!(at >= before);
            }
            Commit::Dropped => panic!("valid request dropped"),
        }
        assert_eq!(
            other.incoming.recv().await.unwrap().unwrap(),
            (OP_RTP, vec![2; 100])
        );
        send(&other, vec![3], None).await;
        assert_eq!(
            lane.incoming.recv().await.unwrap().unwrap(),
            (OP_RTP, vec![3])
        );
        lane.joined_close().await;
        other.joined_close().await;
    }

    #[tokio::test]
    async fn inbound_progress_survives_real_tcp_transmit_backpressure() {
        let (mut lane, mut peer, mut noise) = paired().await;
        let mut blocked = false;
        for _ in 0..1024 {
            let (committed, receipt) = oneshot::channel();
            timeout(
                Duration::from_secs(2),
                lane.outgoing.send(SendRequest {
                    opcode: OP_RTP,
                    payload: vec![9; dmsg_protocol::MAX_PAYLOAD],
                    deadline: None,
                    committed: Some(committed),
                }),
            )
            .await
            .unwrap()
            .unwrap();
            if timeout(Duration::from_millis(30), receipt).await.is_err() {
                blocked = true;
                break;
            }
        }
        assert!(blocked, "socket never reached transmit backpressure");
        let wire = encrypted(&mut noise, &encode_frame(OP_RTCP, &[7]).unwrap());
        peer.write_all(&wire).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(2), lane.incoming.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            (OP_RTCP, vec![7])
        );
        lane.joined_close().await;
        assert!(lane.outgoing.is_closed());
    }

    #[tokio::test]
    async fn partial_header_and_body_survive_other_select_branches() {
        let (mut lane, mut peer, mut noise) = paired().await;
        let wire = encrypted(&mut noise, &encode_frame(OP_RTP, &[4; 64]).unwrap());
        peer.write_all(&wire[..1]).await.unwrap();
        send(&lane, vec![8], None).await;
        let mut reverse = [0; 23];
        peer.read_exact(&mut reverse).await.unwrap();
        let mut scratch = [0; 64];
        let length = noise.read_message(&reverse[2..], &mut scratch).unwrap();
        assert_eq!(decode_frame(&scratch[..length]).unwrap().2, &[8]);
        peer.write_all(&wire[1..10]).await.unwrap();
        assert!(timeout(Duration::from_millis(20), lane.incoming.recv())
            .await
            .is_err());
        send(&lane, vec![8], None).await;
        peer.read_exact(&mut reverse).await.unwrap();
        noise.read_message(&reverse[2..], &mut scratch).unwrap();
        peer.write_all(&wire[10..]).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(2), lane.incoming.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            (OP_RTP, vec![4; 64])
        );
        lane.joined_close().await;
    }

    #[tokio::test]
    async fn close_and_drop_cancel_partial_read_without_a_child_reader() {
        for use_drop in [false, true] {
            let (mut lane, mut peer, _) = paired().await;
            peer.write_all(&[0]).await.unwrap();
            tokio::task::yield_now().await;
            let outgoing = lane.outgoing.clone();
            if use_drop {
                drop(lane);
            } else {
                lane.joined_close().await;
                assert!(lane.incoming.try_recv().is_err());
            }
            let mut byte = [0];
            assert_eq!(
                timeout(Duration::from_secs(2), peer.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            assert!(outgoing.is_closed());
        }
    }

    #[tokio::test]
    async fn trailing_frames_unknown_version_and_cipher_bound_are_rejected() {
        for kind in 0..3 {
            let (mut lane, mut peer, mut noise) = paired().await;
            let mut inner = encode_frame(OP_RTP, &[1]).unwrap();
            let wire = match kind {
                0 => {
                    inner.extend_from_slice(&encode_frame(OP_RTP, &[2]).unwrap());
                    encrypted(&mut noise, &inner)
                }
                1 => {
                    inner[0] = 1;
                    encrypted(&mut noise, &inner)
                }
                _ => u16::try_from(MAX_FRAME + 17)
                    .unwrap()
                    .to_be_bytes()
                    .to_vec(),
            };
            peer.write_all(&wire).await.unwrap();
            assert!(timeout(Duration::from_secs(2), lane.incoming.recv())
                .await
                .unwrap()
                .unwrap()
                .is_err());
            lane.joined_close().await;
            assert!(lane.outgoing.is_closed());
        }
    }
}

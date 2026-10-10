//! Separate fixture backend. It authenticates Noise peers and routes opaque SRTP.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use zeroize::Zeroizing;

use super::fixture::{EndpointFixture, RelayFixture};
use super::lane::{
    self, Commit, InboundFrame, Lane, SendRequest, OP_BIND, OP_BOUND, OP_RTCP, OP_RTP,
};
use super::service::SyntheticServiceModel;

const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const HOP_DEADLINE: Duration = Duration::from_millis(20);

struct Bound {
    index: usize,
    body: Vec<u8>,
    lane: Lane,
}

pub async fn connect(addr: &str, fixture: &EndpointFixture, media: bool) -> Result<Lane, String> {
    tokio::time::timeout(SETUP_TIMEOUT, async {
        if fixture.role > 1 {
            return Err("probe role rejected".into());
        }
        let transport = crate::transport::initiate_with_key(
            addr,
            &fixture.relay_public,
            fixture.domain.as_bytes(),
            &fixture.noise_private,
        )
        .await
        .map_err(|_| "probe connection failed".to_string())?;
        let (stream, noise) = transport
            .into_probe_parts()
            .map_err(|_| "probe transport unavailable".to_string())?;
        let mut lane = lane::spawn(stream, noise)?;
        let body = binding(
            &fixture.generation,
            fixture.role,
            !media,
            fixture.profile_ms,
        );
        if let Err(error) = send_setup(&lane, OP_BIND, body.clone()).await {
            lane.joined_close().await;
            return Err(error);
        }
        let bound = lane.incoming.recv().await;
        if !matches!(bound, Some(Ok(InboundFrame { frame: (OP_BOUND, ref payload), .. })) if *payload == body) {
            lane.joined_close().await;
            return Err("probe binding rejected".into());
        }
        Ok(lane)
    })
    .await
    .map_err(|_| "probe setup timed out".to_string())?
}

fn binding(generation: &[u8; 16], role: u8, control: bool, profile_ms: u16) -> Vec<u8> {
    let mut body = Vec::with_capacity(20);
    body.extend_from_slice(generation);
    body.push(role);
    body.push(u8::from(control));
    body.extend_from_slice(&profile_ms.to_be_bytes());
    body
}

fn media_cap(profile_ms: u16) -> Result<usize, String> {
    match profile_ms {
        20 => Ok(50 + 12 + 10),
        40 => Ok(100 + 12 + 10),
        60 => Ok(150 + 12 + 10),
        _ => Err("probe profile rejected".into()),
    }
}

/// One immutable generation, at most four sockets (including setup sockets).
pub async fn serve(
    listener: TcpListener,
    fixture: RelayFixture,
    private: Zeroizing<[u8; 32]>,
    stop: watch::Receiver<bool>,
) -> Result<(), String> {
    serve_inner(listener, fixture, private, stop, None).await
}

/// Synthetic 50/80k framed-application service, not measured DNS capacity. Clone
/// the model before this call to observe progress. Default `serve` is ungated.
pub async fn serve_with_service_model(
    listener: TcpListener,
    fixture: RelayFixture,
    private: Zeroizing<[u8; 32]>,
    stop: watch::Receiver<bool>,
    service: SyntheticServiceModel,
) -> Result<(), String> {
    serve_inner(listener, fixture, private, stop, Some(service)).await
}

async fn serve_inner(
    listener: TcpListener,
    fixture: RelayFixture,
    private: Zeroizing<[u8; 32]>,
    mut stop: watch::Receiver<bool>,
    service: Option<SyntheticServiceModel>,
) -> Result<(), String> {
    if fixture.version != 1
        || fixture.domain.is_empty()
        || fixture.domain.len() > dmsg_protocol::DOMAIN_MAX
    {
        return Err("probe relay fixture rejected".into());
    }
    let cap = media_cap(fixture.profile_ms)?;
    if let Some(service) = &service {
        service.check_profile(fixture.profile_ms)?;
    }
    let private = Arc::new(private);
    let mut handlers: JoinSet<Result<Bound, String>> = JoinSet::new();
    let mut lanes: [Option<Lane>; 4] = std::array::from_fn(|_| None);
    let setup_deadline = tokio::time::sleep(SETUP_TIMEOUT);
    tokio::pin!(setup_deadline);

    let setup = loop {
        if *stop.borrow() {
            break Ok(false);
        }
        if lanes.iter().all(Option::is_some) {
            break Ok(true);
        }
        tokio::select! {
            biased;
            _ = stopped(&mut stop) => break Ok(false),
            _ = &mut setup_deadline => break Err("probe relay setup timed out".to_string()),
            completed = handlers.join_next(), if !handlers.is_empty() => {
                if let Some(Ok(Ok(mut bound))) = completed {
                    if lanes[bound.index].is_some() {
                        bound.lane.joined_close().await;
                        continue;
                    }
                    // Only the generation owner can acknowledge a unique role/lane.
                    if bound.lane.outgoing.try_send(SendRequest {
                        opcode: OP_BOUND,
                        payload: bound.body,
                        deadline: None,
                        committed: None,
                    }).is_err() {
                        bound.lane.joined_close().await;
                        continue;
                    }
                    lanes[bound.index] = Some(bound.lane);
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => break Err("probe relay accept failed".to_string()),
                };
                if handlers.len() + lanes.iter().filter(|lane| lane.is_some()).count() < 4 {
                    handlers.spawn(setup_peer(
                        stream, fixture.clone(), Arc::clone(&private), service.clone(),
                    ));
                }
                // An excess socket is dropped here, without another handler.
            }
        }
    };

    let result = match setup {
        Ok(true) => {
            async {
                if let Some(service) = &service {
                    service.arm(Instant::now())?;
                }
                // Four concrete directions, no routing registry or forwarding workers.
                let [a_media, a_control, b_media, b_control] = &mut lanes;
                let a_media = a_media.as_mut().expect("all four bound");
                let a_control = a_control.as_mut().expect("all four bound");
                let b_media = b_media.as_mut().expect("all four bound");
                let b_control = b_control.as_mut().expect("all four bound");
                route(
                    &listener, a_media, a_control, b_media, b_control, cap, &mut stop,
                )
                .await
            }
            .await
        }
        Ok(false) => Ok(()),
        Err(error) => Err(error),
    };

    // Stop/error retires every socket, pending request and committed frame.
    for lane in lanes.iter_mut().flatten() {
        lane.close();
    }
    handlers.abort_all();
    while let Some(completed) = handlers.join_next().await {
        if let Ok(Ok(mut bound)) = completed {
            bound.lane.joined_close().await;
        }
    }
    for lane in lanes.iter_mut().flatten() {
        lane.joined_close().await;
    }
    result
}

async fn route(
    listener: &TcpListener,
    a_media: &mut Lane,
    a_control: &mut Lane,
    b_media: &mut Lane,
    b_control: &mut Lane,
    cap: usize,
    stop: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
            _ = stopped(stop) => return Ok(()),
            packet = a_media.incoming.recv() => forward(packet, &b_media.outgoing, OP_RTP, cap)?,
            packet = b_media.incoming.recv() => forward(packet, &a_media.outgoing, OP_RTP, cap)?,
            packet = a_control.incoming.recv() => forward(packet, &b_control.outgoing, OP_RTCP, 130)?,
            packet = b_control.incoming.recv() => forward(packet, &a_control.outgoing, OP_RTCP, 130)?,
            accepted = listener.accept() => {
                // All four slots are owned for this generation; reject newcomers.
                let (stream, _) = accepted.map_err(|_| "probe relay accept failed".to_string())?;
                drop(stream);
            }
        }
    }
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

fn forward(
    packet: Option<Result<InboundFrame, String>>,
    destination: &mpsc::Sender<SendRequest>,
    opcode: u8,
    cap: usize,
) -> Result<(), String> {
    let (received_opcode, payload) = packet
        .ok_or_else(|| "probe relay lane closed".to_string())?
        .map_err(|_| "probe relay lane failed".to_string())?
        .frame;
    if received_opcode != opcode || payload.is_empty() || payload.len() > cap {
        return Err("probe relay packet rejected".into());
    }
    match destination.try_send(SendRequest {
        opcode,
        payload,
        deadline: Some(Instant::now() + HOP_DEADLINE),
        committed: None,
    }) {
        Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
        Err(mpsc::error::TrySendError::Closed(_)) => Err("probe relay destination closed".into()),
    }
}

async fn send_setup(lane: &Lane, opcode: u8, payload: Vec<u8>) -> Result<(), String> {
    let (committed, receipt) = oneshot::channel();
    lane.outgoing
        .send(SendRequest {
            opcode,
            payload,
            deadline: None,
            committed: Some(committed),
        })
        .await
        .map_err(|_| "probe setup send failed".to_string())?;
    match receipt.await {
        Ok(Commit::Committed { .. }) => Ok(()),
        _ => Err("probe setup commitment failed".into()),
    }
}

async fn setup_peer(
    mut stream: TcpStream,
    fixture: RelayFixture,
    private: Arc<Zeroizing<[u8; 32]>>,
    service: Option<SyntheticServiceModel>,
) -> Result<Bound, String> {
    stream
        .set_nodelay(true)
        .map_err(|_| "probe socket configuration failed".to_string())?;
    let mut handshake = snow::Builder::new(
        crate::transport::PATTERN
            .parse()
            .map_err(|_| "probe Noise pattern failed".to_string())?,
    )
    .local_private_key(private.as_ref().as_ref())
    .map_err(|_| "probe Noise key failed".to_string())?
    .build_responder()
    .map_err(|_| "probe Noise responder failed".to_string())?;
    let mut scratch = Zeroizing::new(vec![0; dmsg_protocol::MAX_FRAME + 16]);
    let first = read_handshake(&mut stream).await?;
    if handshake
        .read_message(&first, &mut scratch)
        .map_err(|_| "probe Noise handshake rejected".to_string())?
        != 0
    {
        return Err("probe Noise handshake payload rejected".into());
    }
    let remote: [u8; 32] = handshake
        .get_remote_static()
        .ok_or_else(|| "probe Noise identity missing".to_string())?
        .try_into()
        .map_err(|_| "probe Noise identity rejected".to_string())?;
    let length = handshake
        .write_message(&[], &mut scratch)
        .map_err(|_| "probe Noise handshake failed".to_string())?;
    let prefix = u16::try_from(length).map_err(|_| "probe Noise length failed".to_string())?;
    stream
        .write_all(&prefix.to_be_bytes())
        .await
        .map_err(|_| "probe handshake write failed".to_string())?;
    stream
        .write_all(&scratch[..length])
        .await
        .map_err(|_| "probe handshake write failed".to_string())?;
    let mut noise = handshake
        .into_transport_mode()
        .map_err(|_| "probe Noise transport failed".to_string())?;
    let (opcode, domain) = read_setup_frame(&mut stream, &mut noise, &mut scratch).await?;
    if opcode != dmsg_protocol::OP_AUTH_DOMAIN || domain != fixture.domain.as_bytes() {
        return Err("probe relay domain rejected".into());
    }
    let welcome = Zeroizing::new(
        dmsg_protocol::encode_frame(dmsg_protocol::OP_WELCOME, fixture.domain.as_bytes())
            .map_err(|_| "probe relay welcome failed".to_string())?,
    );
    let length = noise
        .write_message(&welcome, &mut scratch)
        .map_err(|_| "probe relay welcome encryption failed".to_string())?;
    let prefix =
        u16::try_from(length).map_err(|_| "probe relay welcome length failed".to_string())?;
    stream
        .write_all(&prefix.to_be_bytes())
        .await
        .map_err(|_| "probe welcome write failed".to_string())?;
    stream
        .write_all(&scratch[..length])
        .await
        .map_err(|_| "probe welcome write failed".to_string())?;
    let (opcode, body) = read_setup_frame(&mut stream, &mut noise, &mut scratch).await?;
    if opcode != OP_BIND
        || body.len() != 20
        || body[..16] != fixture.generation
        || body[16] > 1
        || body[17] > 1
        || u16::from_be_bytes([body[18], body[19]]) != fixture.profile_ms
    {
        return Err("probe relay bind rejected".into());
    }
    let role = usize::from(body[16]);
    if remote != fixture.device_public[role] {
        return Err("probe relay identity rejected".into());
    }
    // Handshake, AUTH_DOMAIN and BIND have already been read/authenticated.
    // Further reads wait for the generation owner's common four-lane epoch.
    let lane = match service {
        Some(service) => {
            lane::spawn_with_ingress_service(stream, noise, service.ingress(role, body[17] == 1))?
        }
        None => lane::spawn(stream, noise)?,
    };
    Ok(Bound {
        index: role * 2 + usize::from(body[17]),
        body,
        lane,
    })
}

async fn read_setup_frame(
    stream: &mut TcpStream,
    noise: &mut snow::TransportState,
    scratch: &mut [u8],
) -> Result<(u8, Vec<u8>), String> {
    let cipher = read_handshake(stream).await?;
    let length = noise
        .read_message(&cipher, scratch)
        .map_err(|_| "probe setup authentication failed".to_string())?;
    let (_, opcode, payload, consumed) = dmsg_protocol::decode_frame(&scratch[..length])
        .map_err(|_| "probe setup frame rejected".to_string())?;
    if consumed != length {
        return Err("probe setup trailing bytes".into());
    }
    Ok((opcode, payload.to_vec()))
}

async fn read_handshake(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut prefix = [0; 2];
    stream
        .read_exact(&mut prefix)
        .await
        .map_err(|_| "probe handshake read failed".to_string())?;
    let length = usize::from(u16::from_be_bytes(prefix));
    if length == 0 || length > dmsg_protocol::MAX_FRAME + 16 {
        return Err("probe handshake length rejected".into());
    }
    let mut body = vec![0; length];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|_| "probe handshake read failed".to_string())?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    const RELAY_PRIVATE: [u8; 32] = [81; 32];
    const DEVICE_PRIVATE: [[u8; 32]; 2] = [[82; 32], [83; 32]];
    const TEST_TIMEOUT: Duration = Duration::from_secs(3);

    fn fixture(generation: u8) -> RelayFixture {
        RelayFixture {
            version: 1,
            generation: [generation; 16],
            domain: "voice-probe.test".into(),
            device_public: DEVICE_PRIVATE.map(|private| crate::olm::device_pubkey(&private)),
            profile_ms: 40,
        }
    }

    async fn start(
        fixture: RelayFixture,
    ) -> (String, watch::Sender<bool>, JoinHandle<Result<(), String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (stop, receiver) = watch::channel(false);
        let job = tokio::spawn(serve(
            listener,
            fixture,
            Zeroizing::new(RELAY_PRIVATE),
            receiver,
        ));
        (addr, stop, job)
    }

    // Minimal independent Noise client: no EndpointFixture constructors or
    // assumptions about its additional private fields are needed by these tests.
    async fn binder(
        addr: &str,
        domain: &str,
        private: &[u8; 32],
        body: Vec<u8>,
    ) -> Result<Lane, String> {
        let relay_public = crate::olm::device_pubkey(&RELAY_PRIVATE);
        let mut handshake = snow::Builder::new(crate::transport::PATTERN.parse().unwrap())
            .local_private_key(private)
            .map_err(|_| "test private key rejected".to_string())?
            .remote_public_key(&relay_public)
            .map_err(|_| "test relay key rejected".to_string())?
            .build_initiator()
            .map_err(|_| "test initiator rejected".to_string())?;
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|_| "test socket failed".to_string())?;
        let mut scratch = vec![0; dmsg_protocol::MAX_FRAME + 16];
        let length = handshake
            .write_message(&[], &mut scratch)
            .map_err(|_| "test handshake write failed".to_string())?;
        let prefix = u16::try_from(length).unwrap().to_be_bytes();
        stream
            .write_all(&prefix)
            .await
            .map_err(|_| "test prefix write failed".to_string())?;
        stream
            .write_all(&scratch[..length])
            .await
            .map_err(|_| "test handshake send failed".to_string())?;
        let reply = read_handshake(&mut stream).await?;
        if handshake
            .read_message(&reply, &mut scratch)
            .map_err(|_| "test handshake read failed".to_string())?
            != 0
        {
            return Err("test handshake payload rejected".into());
        }
        let noise = handshake
            .into_transport_mode()
            .map_err(|_| "test Noise transport failed".to_string())?;
        let mut lane = lane::spawn(stream, noise)?;
        if let Err(error) = send_setup(
            &lane,
            dmsg_protocol::OP_AUTH_DOMAIN,
            domain.as_bytes().to_vec(),
        )
        .await
        {
            lane.joined_close().await;
            return Err(error);
        }
        if !matches!(lane.incoming.recv().await, Some(Ok(InboundFrame { frame: (dmsg_protocol::OP_WELCOME, payload), .. })) if payload == domain.as_bytes())
        {
            lane.joined_close().await;
            return Err("test domain rejected".into());
        }
        if let Err(error) = send_setup(&lane, OP_BIND, body.clone()).await {
            lane.joined_close().await;
            return Err(error);
        }
        if !matches!(lane.incoming.recv().await, Some(Ok(InboundFrame { frame: (OP_BOUND, payload), .. })) if payload == body)
        {
            lane.joined_close().await;
            return Err("test binding rejected".into());
        }
        Ok(lane)
    }

    async fn valid(addr: &str, fixture: &RelayFixture, role: u8, control: bool) -> Lane {
        timeout(
            TEST_TIMEOUT,
            binder(
                addr,
                &fixture.domain,
                &DEVICE_PRIVATE[usize::from(role)],
                binding(&fixture.generation, role, control, fixture.profile_ms),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn all(addr: &str, fixture: &RelayFixture) -> [Lane; 4] {
        [
            valid(addr, fixture, 0, false).await,
            valid(addr, fixture, 0, true).await,
            valid(addr, fixture, 1, false).await,
            valid(addr, fixture, 1, true).await,
        ]
    }

    async fn receive(lane: &mut Lane) -> (u8, Vec<u8>) {
        timeout(TEST_TIMEOUT, lane.incoming.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .frame
    }

    async fn retire(stop: watch::Sender<bool>, job: JoinHandle<Result<(), String>>) {
        stop.send(true).unwrap();
        timeout(TEST_TIMEOUT, job).await.unwrap().unwrap().unwrap();
    }

    async fn close_all(lanes: &mut [Lane; 4]) {
        for lane in lanes {
            lane.joined_close().await;
        }
    }

    #[tokio::test]
    async fn four_lanes_route_only_to_other_role_same_lane_in_fifo_order() {
        let fixture = fixture(1);
        let (addr, stop, job) = start(fixture.clone()).await;
        let mut lanes = all(&addr, &fixture).await;
        for index in 0..4 {
            let other = (index + 2) % 4;
            let opcode = if index % 2 == 0 { OP_RTP } else { OP_RTCP };
            let cap = if index % 2 == 0 { 122 } else { 130 };
            for ordinal in 0..4 {
                let opaque = vec![u8::try_from(index * 4 + ordinal).unwrap(); cap];
                timeout(
                    TEST_TIMEOUT,
                    send_setup(&lanes[index], opcode, opaque.clone()),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(receive(&mut lanes[other]).await, (opcode, opaque));
            }
        }
        for lane in &mut lanes {
            assert!(lane.incoming.try_recv().is_err());
        }
        retire(stop, job).await;
        close_all(&mut lanes).await;
    }

    #[tokio::test]
    async fn synthetic_service_arms_after_four_bindings_and_counts_both_role_lanes() {
        use super::super::service::{SyntheticCollapse, SyntheticServiceConfig};

        let fixture = fixture(7);
        let model = SyntheticServiceModel::new(
            SyntheticServiceConfig {
                baseline_bps: 80_000,
                collapse: Some(SyntheticCollapse {
                    start_ms: 0,
                    duration_ms: 100,
                    bps: 0,
                }),
            },
            fixture.profile_ms,
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (stop, receiver) = watch::channel(false);
        let job = tokio::spawn(serve_with_service_model(
            listener,
            fixture.clone(),
            Zeroizing::new(RELAY_PRIVATE),
            receiver,
            model.clone(),
        ));
        let mut a_media = valid(&addr, &fixture, 0, false).await;
        send_setup(&a_media, OP_RTP, vec![1; 122]).await.unwrap();
        let mut a_control = valid(&addr, &fixture, 0, true).await;
        send_setup(&a_control, OP_RTCP, vec![2; 130]).await.unwrap();
        let mut b_media = valid(&addr, &fixture, 1, false).await;
        let unarmed = model.snapshot().unwrap();
        assert!(!unarmed.armed);
        assert_eq!(unarmed.roles[0].read_bytes, 0);
        assert_eq!(unarmed.roles[1].read_bytes, 0);
        let mut b_control = valid(&addr, &fixture, 1, true).await;
        assert!(model.snapshot().unwrap().armed);
        send_setup(&b_media, OP_RTP, vec![3; 122]).await.unwrap();
        send_setup(&b_control, OP_RTCP, vec![4; 130]).await.unwrap();
        assert_eq!(receive(&mut b_media).await, (OP_RTP, vec![1; 122]));
        assert_eq!(receive(&mut b_control).await, (OP_RTCP, vec![2; 130]));
        assert_eq!(receive(&mut a_media).await, (OP_RTP, vec![3; 122]));
        assert_eq!(receive(&mut a_control).await, (OP_RTCP, vec![4; 130]));
        retire(stop, job).await;
        let final_stats = model.snapshot().unwrap();
        for role in final_stats.roles {
            assert_eq!(role.read_bytes, 296); // media144 + feedback152, no setup bytes.
            assert_eq!(role.authenticated_framed_bytes, 296);
            assert_eq!(role.authenticated_frames, 2);
            assert_eq!(role.live_partial_bytes, 0);
            assert_eq!(role.retired_partial_bytes, 0);
        }
        close_all(&mut [a_media, a_control, b_media, b_control]).await;
    }

    #[tokio::test]
    async fn wrong_identity_domain_generation_profile_and_duplicate_are_rejected() {
        let fixture = fixture(2);
        let (addr, stop, job) = start(fixture.clone()).await;
        let mut original = valid(&addr, &fixture, 0, false).await;
        let valid_body = binding(&fixture.generation, 0, false, fixture.profile_ms);
        for kind in 0..7 {
            let mut body = valid_body.clone();
            let mut private = DEVICE_PRIVATE[0];
            let domain = if kind == 1 {
                "wrong.test"
            } else {
                &fixture.domain
            };
            match kind {
                0 => private = [84; 32],
                2 => body[0] ^= 1,
                3 => body[19] = 20,
                4 => body[17] = 2,
                5 => body.push(0),
                // kind 6 is the exact duplicate, authenticated by the right key.
                _ => {}
            }
            assert!(timeout(TEST_TIMEOUT, binder(&addr, domain, &private, body))
                .await
                .unwrap()
                .is_err());
        }
        let mut a_control = valid(&addr, &fixture, 0, true).await;
        let mut other = valid(&addr, &fixture, 1, false).await;
        let mut b_control = valid(&addr, &fixture, 1, true).await;
        // Rejections leave the original authenticated role/lane intact.
        send_setup(&other, OP_RTP, vec![19; 32]).await.unwrap();
        assert_eq!(receive(&mut original).await, (OP_RTP, vec![19; 32]));
        assert!(timeout(
            TEST_TIMEOUT,
            binder(&addr, &fixture.domain, &DEVICE_PRIVATE[0], valid_body)
        )
        .await
        .unwrap()
        .is_err());
        retire(stop, job).await;
        original.joined_close().await;
        other.joined_close().await;
        a_control.joined_close().await;
        b_control.joined_close().await;
    }

    #[tokio::test]
    async fn unexpected_opcode_or_oversize_ends_the_entire_generation() {
        for (index, opcode, length) in [(0, OP_RTCP, 20), (0, OP_RTP, 123), (1, OP_RTCP, 131)] {
            let fixture = fixture(3);
            let (addr, _stop, job) = start(fixture.clone()).await;
            let mut lanes = all(&addr, &fixture).await;
            send_setup(&lanes[index], opcode, vec![1; length])
                .await
                .unwrap();
            assert!(timeout(TEST_TIMEOUT, job).await.unwrap().unwrap().is_err());
            for lane in &mut lanes {
                assert!(matches!(
                    timeout(TEST_TIMEOUT, lane.incoming.recv()).await.unwrap(),
                    None | Some(Err(_))
                ));
            }
            close_all(&mut lanes).await;
        }
    }

    #[tokio::test]
    async fn full_pending_hop_drops_plaintext_without_reordering_survivors() {
        let (outgoing, mut requests) = mpsc::channel(1);
        let inbound = |ordinal| {
            Some(Ok(InboundFrame {
                frame: (OP_RTP, vec![ordinal]),
                body_complete: Instant::now(),
                noise_done: Instant::now(),
            }))
        };
        forward(inbound(1), &outgoing, OP_RTP, 122).unwrap();
        forward(inbound(2), &outgoing, OP_RTP, 122).unwrap();
        let first = requests.recv().await.unwrap();
        assert_eq!(first.payload, [1]);
        let deadline = first.deadline.unwrap();
        assert!(deadline <= Instant::now() + HOP_DEADLINE);
        forward(inbound(3), &outgoing, OP_RTP, 122).unwrap();
        assert_eq!(requests.recv().await.unwrap().payload, [3]);
        assert!(requests.try_recv().is_err());
        assert_eq!(media_cap(20).unwrap(), 72);
        assert_eq!(media_cap(40).unwrap(), 122);
        assert_eq!(media_cap(60).unwrap(), 172);
    }

    #[tokio::test]
    async fn stop_cancels_partial_startup_and_limits_live_handlers_to_four() {
        let (addr, stop, job) = start(fixture(4)).await;
        let mut sockets = Vec::new();
        for _ in 0..4 {
            let mut stream = TcpStream::connect(&addr).await.unwrap();
            stream.write_all(&[0]).await.unwrap();
            sockets.push(stream);
        }
        let mut excess = TcpStream::connect(&addr).await.unwrap();
        let mut byte = [0];
        assert_eq!(
            timeout(TEST_TIMEOUT, excess.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        retire(stop, job).await;
        for mut stream in sockets {
            // An unread partial header can cause reset rather than FIN on Linux.
            assert!(
                !matches!(timeout(TEST_TIMEOUT, stream.read(&mut byte)).await.unwrap(), Ok(count) if count > 0)
            );
        }
    }

    #[tokio::test]
    async fn retirement_joins_old_handlers_before_fresh_generation_on_same_address() {
        let old_fixture = fixture(5);
        let (addr, stop, job) = start(old_fixture.clone()).await;
        let mut old = all(&addr, &old_fixture).await;
        send_setup(&old[0], OP_RTP, vec![33]).await.unwrap();
        assert_eq!(receive(&mut old[2]).await, (OP_RTP, vec![33]));
        retire(stop, job).await;

        let fresh_fixture = fixture(6);
        let listener = TcpListener::bind(&addr).await.unwrap();
        let (stop, receiver) = watch::channel(false);
        let job = tokio::spawn(serve(
            listener,
            fresh_fixture.clone(),
            Zeroizing::new(RELAY_PRIVATE),
            receiver,
        ));
        assert!(timeout(
            TEST_TIMEOUT,
            binder(
                &addr,
                &fresh_fixture.domain,
                &DEVICE_PRIVATE[0],
                binding(&old_fixture.generation, 0, false, old_fixture.profile_ms),
            )
        )
        .await
        .unwrap()
        .is_err());
        let mut fresh = all(&addr, &fresh_fixture).await;
        // Old sockets have no live relay handlers, and cannot enter replacements.
        for lane in &mut old {
            assert!(matches!(
                timeout(TEST_TIMEOUT, lane.incoming.recv()).await.unwrap(),
                None | Some(Err(_))
            ));
            lane.joined_close().await;
            assert!(lane.outgoing.is_closed());
        }
        send_setup(&fresh[0], OP_RTP, vec![44]).await.unwrap();
        assert_eq!(receive(&mut fresh[2]).await, (OP_RTP, vec![44]));
        for lane in &mut fresh {
            assert!(lane.incoming.try_recv().is_err());
        }
        retire(stop, job).await;
        close_all(&mut fresh).await;
    }
}

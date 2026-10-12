use super::{
    fixture::{self, EndpointFixture, PublicConfig},
    relay,
};
use crate::live_media::{
    session::{cancelled, endpoint, new_port, Captured, Port},
    AudioPort, Stats,
};
use std::{
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

/// One explicit fixture owner. Its worker and carrier must join before reuse.
pub struct Probe {
    port: Arc<Port>,
    stop: watch::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Probe {
    pub fn start_local() -> Result<Self, String> {
        Self::start(None)
    }

    pub fn start_dns(path: &Path) -> Result<Self, String> {
        let fixture = EndpointFixture::load(path)?;
        if fixture.carrier.is_none() {
            return Err("DNS probe requires carrier fixture".into());
        }
        Self::start(Some(fixture))
    }

    /// Diagnostic host path; direct TCP is never labelled DNS acceptance.
    pub fn start_fixture(fixture: EndpointFixture) -> Result<Self, String> {
        fixture.validate()?;
        Self::start(Some(fixture))
    }

    fn start(fixture: Option<EndpointFixture>) -> Result<Self, String> {
        let (port, incoming) = new_port();
        let (stop, cancelled) = watch::channel(false);
        let shared = port.clone();
        let worker = thread::Builder::new()
            .name("m1v-media".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "probe runtime failed".to_string())
                    .and_then(|runtime| {
                        runtime.block_on(run(fixture, shared.clone(), incoming, cancelled))
                    });
                shared.finish(result);
            })
            .map_err(|_| "probe worker failed")?;
        Ok(Self {
            port,
            stop,
            worker: Some(worker),
        })
    }

    /// Nonblocking 10ms PCM transfer. Dropped positions still advance source time.
    pub fn push(&self, pcm: &[i16]) -> bool {
        self.audio().push(pcm)
    }

    pub fn pull(&self, pcm: &mut [i16]) -> usize {
        self.audio().pull(pcm)
    }

    pub fn sink_queued(&self, samples: usize) {
        self.audio().sink_queued(samples)
    }

    pub fn sink_rate(&self, ppb: i64) -> bool {
        self.audio().sink_rate(ppb)
    }

    pub(crate) fn audio(&self) -> AudioPort {
        self.port.audio()
    }

    pub fn snapshot(&self) -> Stats {
        self.port.snapshot()
    }

    pub fn stop(&mut self) -> Stats {
        let at = Instant::now();
        self.port.close();
        let _ = self.stop.send(true);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                self.port.stats(|stats| {
                    stats.failed = true;
                    stats.error = "probe worker panicked".into();
                });
            }
            self.port
                .stats(|stats| stats.stop_ms = at.elapsed().as_millis() as u64);
        }
        self.port.clear();
        self.snapshot()
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run(
    fixture: Option<EndpointFixture>,
    port: Arc<Port>,
    incoming: mpsc::Receiver<Captured>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    if let Some(fixture) = fixture {
        let mut native = if let Some(carrier) = &fixture.carrier {
            let certificate = fixture::read_private(Path::new(&carrier.certificate_path), 65536)?;
            let resolvers = carrier
                .resolvers
                .iter()
                .map(|address| {
                    address
                        .parse()
                        .map_err(|_| "invalid numeric fixture resolver".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut config = slipstream_sys::Config::new(
                carrier.domain.clone(),
                resolvers,
                certificate.to_vec(),
            );
            config.congestion_control = carrier.congestion_control.native();
            Some(
                slipstream_sys::NativeClient::start(config)
                    .map_err(|_| "probe carrier unavailable or already owned".to_string())?,
            )
        } else {
            None
        };
        port.stats(|stats| stats.dns_carrier = native.is_some());
        let endpoint_stop = stop.clone();
        let result = async {
            let addr = if let Some(native) = &native {
                let bootstrap = async {
                    loop {
                        match native.status() {
                            slipstream_sys::Status::Ready(endpoint) => {
                                return Ok(endpoint.to_string())
                            }
                            slipstream_sys::Status::Failed(_) | slipstream_sys::Status::Stopped => {
                                return Err("probe carrier bootstrap failed".to_string())
                            }
                            _ => tokio::time::sleep(Duration::from_millis(10)).await,
                        }
                    }
                };
                tokio::time::timeout(Duration::from_secs(30), bootstrap)
                    .await
                    .map_err(|_| "probe carrier bootstrap timed out")??
            } else {
                fixture.relay_addr.clone()
            };
            let media = relay::connect(&addr, &fixture, true).await?;
            let control = relay::connect(&addr, &fixture, false).await?;
            endpoint(
                fixture.media_config(),
                port.clone(),
                incoming,
                media,
                control,
                endpoint_stop,
            )
            .await
        };
        let outcome =
            tokio::select! { _ = cancelled(&mut stop) => Ok(()), result = result => result };
        if let Some(native) = &mut native {
            let at = Instant::now();
            native.stop().map_err(|_| "probe carrier join failed")?;
            port.stats(|stats| stats.native_stop_ms = at.elapsed().as_millis() as u64);
        }
        outcome
    } else {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "probe local listener failed")?;
        let config = PublicConfig {
            relay_addr: listener
                .local_addr()
                .map_err(|_| "probe local address failed")?
                .to_string(),
            domain: "m1v.fixture".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        };
        let (a, b, relay_fixture, relay_key) = fixture::pair(config)?;
        let addr = a.relay_addr.clone();
        let relay_owner = tokio::spawn(relay::serve(
            listener,
            relay_fixture,
            relay_key,
            stop.clone(),
        ));
        let (peer, peer_incoming) = new_port();
        let peer_shared = peer.clone();
        let peer_addr = addr.clone();
        let peer_stop = stop.clone();
        let peer_owner = tokio::spawn(async move {
            let media = relay::connect(&peer_addr, &b, true).await?;
            let control = relay::connect(&peer_addr, &b, false).await?;
            endpoint(
                b.media_config(),
                peer_shared,
                peer_incoming,
                media,
                control,
                peer_stop,
            )
            .await
        });
        let tone_peer = peer.clone();
        let observed = port.clone();
        let mut tone_stop = stop.clone();
        let tone = tokio::spawn(async move {
            let origin = tokio::time::Instant::now();
            let mut timer = tokio::time::interval_at(origin, Duration::from_millis(10));
            // Simulate continuously sampled hardware, not a timer-based VAD.
            // A late callback can still drain recent 10ms capture slots into the
            // bounded 40ms ring. The endpoint rejects aged PCM before encoding;
            // its media admission never flushes a queued audio burst.
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
            loop {
                tokio::select! {
                    _ = cancelled(&mut tone_stop) => break,
                    scheduled = timer.tick() => {
                        // The simulated microphone clock must not slow down when
                        // this runtime misses a tick. Use the exact scheduled slot:
                        // flooring wall-clock jitter can repeat a source position.
                        let position = (scheduled.duration_since(origin).as_micros() as usize / 10_000) * 160;
                        let pushed_at = Instant::now();
                        let mut pcm = [0; 160];
                        pcm.copy_from_slice(&super::test_tone(position, 160));
                        tone_peer.push_synthetic(&pcm, position as u64, scheduled.into_std(), pushed_at);
                        tone_peer.clear(); // no fake remote hardware presentation claim
                        let evidence = tone_peer.snapshot();
                        observed.stats(|stats| {
                            stats.fixture_peer_encoded_packets = evidence.encoded_packets;
                            stats.fixture_peer_dropped_capture = evidence.dropped_capture;
                            stats.fixture_peer_capture_gap_batches = evidence.capture_gap_batches;
                        });
                    }
                }
            }
        });
        let endpoint_stop = stop.clone();
        let result = async {
            let media = relay::connect(&addr, &a, true).await?;
            let control = relay::connect(&addr, &a, false).await?;
            endpoint(
                a.media_config(),
                port,
                incoming,
                media,
                control,
                endpoint_stop,
            )
            .await
        };
        let outcome =
            tokio::select! { _ = cancelled(&mut stop) => Ok(()), result = result => result };
        tone.abort();
        peer_owner.abort();
        relay_owner.abort();
        let _ = tone.await;
        let _ = peer_owner.await;
        let _ = relay_owner.await;
        peer.clear();
        outcome
    }
}

//! Fixture authority / opaque private relay / synthetic endpoint. No msgd DB.
use dmsg_core::voice_probe::{
    fixture, relay,
    service::{SyntheticServiceConfig, SyntheticServiceModel},
    test_tone, Probe,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

/// A bounded virtual sink, not hardware playback or acoustic evidence.
/// Match the Android40ms submitted queue; throwing away only one batch per tick
/// leaves valid decoded tails waiting until their source-frame end.
#[derive(Default)]
struct FixturePlayback {
    consumed: u128,
    queued: usize,
}

impl FixturePlayback {
    fn advance(&mut self, elapsed: Duration) {
        let consumed = elapsed.as_nanos() / 62_500; // exact nominal16k source clock
        let delta = consumed.saturating_sub(self.consumed);
        self.consumed = consumed;
        self.queued = self
            .queued
            .saturating_sub(delta.min(usize::MAX as u128) as usize);
    }

    fn render(&mut self, elapsed: Duration, probe: &Probe, output: &mut [i16; 160]) {
        self.advance(elapsed);
        probe.sink_queued(self.queued);
        for _ in 0..4 {
            let capacity = (640 - self.queued).min(output.len());
            if capacity == 0 {
                break;
            }
            let count = probe.pull(&mut output[..capacity]);
            if count == 0 {
                break;
            }
            self.queued += count;
            probe.sink_queued(self.queued);
        }
    }
}

fn write_pair(dir: &Path, pair: fixture::FixturePair) -> Result<(), String> {
    let (a, b, route, key) = pair;
    for (name, bytes) in [
        (
            "endpoint-a.json",
            Zeroizing::new(serde_json::to_vec(&a).map_err(|_| "fixture serialization failed")?),
        ),
        (
            "endpoint-b.json",
            Zeroizing::new(serde_json::to_vec(&b).map_err(|_| "fixture serialization failed")?),
        ),
        (
            "relay.json",
            Zeroizing::new(serde_json::to_vec(&route).map_err(|_| "fixture serialization failed")?),
        ),
    ] {
        fixture::write_new(&dir.join(name), &bytes)?;
    }
    fixture::write_new(&dir.join("relay.key"), key.as_ref())
}

fn main() {
    if execute().is_err() {
        // Do not print serde/IO errors that could include fixtures or private paths.
        eprintln!("M1-V probe command failed; inspect sanitized run result");
        std::process::exit(1);
    }
}

fn execute() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("create-fixtures") if args.len() == 4 => {
            let dir = Path::new(&args[2]);
            if !dir.is_dir() {
                return Err("fixture parent directory required".into());
            }
            let public = fixture::read_private(Path::new(&args[3]), 16384)?;
            let config =
                serde_json::from_slice(&public).map_err(|_| "invalid public probe config")?;
            write_pair(dir, fixture::pair(config)?)?;
            println!("fresh owner-read-only fixture created; relay contains no media keys");
        }
        Some("rotate-fixtures") if args.len() == 4 => {
            let old = Path::new(&args[2]);
            let next = Path::new(&args[3]);
            if !next.is_dir() {
                return Err("fresh fixture parent directory required".into());
            }
            let a = fixture::EndpointFixture::load(&old.join("endpoint-a.json"))?;
            let b = fixture::EndpointFixture::load(&old.join("endpoint-b.json"))?;
            let key = fixture::read_private(&old.join("relay.key"), 32)?;
            let key = Zeroizing::new(
                key.as_slice()
                    .try_into()
                    .map_err(|_| "invalid relay key size")?,
            );
            write_pair(next, fixture::rotate(a, b, key)?)?;
            println!("fresh epoch created with unchanged fixture trust identities");
        }
        Some("relay") if matches!(args.len(), 4 | 5) => {
            let dir = Path::new(&args[2]);
            let route = fixture::read_private(&dir.join("relay.json"), 4096)?;
            let route: fixture::RelayFixture =
                serde_json::from_slice(&route).map_err(|_| "invalid relay fixture")?;
            let service = args
                .get(4)
                .map(|path| {
                    let bytes = fixture::read_private(Path::new(path), 4096)?;
                    let config: SyntheticServiceConfig = serde_json::from_slice(&bytes)
                        .map_err(|_| "invalid synthetic service fixture")?;
                    SyntheticServiceModel::new(config, route.profile_ms)
                })
                .transpose()?;
            let key = fixture::read_private(&dir.join("relay.key"), 32)?;
            let key: [u8; 32] = key
                .as_slice()
                .try_into()
                .map_err(|_| "invalid relay key size")?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "relay runtime failed")?;
            runtime.block_on(async {
                let listener = tokio::net::TcpListener::bind(&args[3])
                    .await
                    .map_err(|_| "relay bind failed")?;
                let (_stop, stopped) = tokio::sync::watch::channel(false);
                if let Some(service) = service {
                    let served = relay::serve_with_service_model(
                        listener,
                        route,
                        Zeroizing::new(key),
                        stopped,
                        service.clone(),
                    );
                    tokio::pin!(served);
                    let mut reports = tokio::time::interval(Duration::from_secs(10));
                    reports.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            result = &mut served => {
                                println!("{}", serde_json::json!({
                                    "kind": "probe_service_final", "service": service.snapshot()?,
                                }));
                                break result;
                            }
                            _ = reports.tick() => {
                                println!("{}", serde_json::json!({
                                    "kind": "probe_service_progress", "service": service.snapshot()?,
                                }));
                            }
                        }
                    }
                } else {
                    relay::serve(listener, route, Zeroizing::new(key), stopped).await
                }
            })?;
        }
        Some("endpoint") if args.len() == 4 => {
            let fixture = fixture::EndpointFixture::load(Path::new(&args[2]))?;
            let seconds: u64 = args[3].parse().map_err(|_| "invalid probe duration")?;
            if !(2..=3600).contains(&seconds) {
                return Err("invalid probe duration".into());
            }
            let mut probe = Probe::start_fixture(fixture)?;
            let mut output = [0; 160];
            let mut position = 0;
            let start = Instant::now();
            let mut playback = FixturePlayback::default();
            let mut ready_observed = false;
            let mut next_progress = start;
            while start.elapsed() < Duration::from_secs(seconds) {
                probe.push(&test_tone(position, 160));
                playback.render(start.elapsed(), &probe, &mut output);
                position += 160;
                let state = probe.snapshot();
                if state.failed {
                    break;
                }
                if state.ready
                    && state.encoded_packets > 0
                    && state.decoded_packets > 0
                    && (!ready_observed || Instant::now() >= next_progress)
                {
                    println!(
                        "{}",
                        serde_json::json!({
                            "kind": if ready_observed { "probe_progress" } else { "probe_ready" },
                            "elapsed_ms": start.elapsed().as_millis(),
                            "stats": state,
                        })
                    );
                    ready_observed = true;
                    next_progress = Instant::now() + Duration::from_secs(10);
                }
                let due = start + Duration::from_micros(position as u64 * 1_000_000 / 16000);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            probe.sink_queued(0);
            output.zeroize();
            let result = probe.stop();
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|_| "probe summary failed")?
            );
            if result.failed || !result.ready {
                return Err("probe failed or never became ready".into());
            }
        }
        Some("local") if args.len() == 2 => {
            let mut probe = Probe::start_local()?;
            let mut output = [0; 160];
            let mut playback = FixturePlayback::default();
            let start = Instant::now();
            for i in 0..300 {
                probe.push(&test_tone(i * 160, 160));
                playback.render(start.elapsed(), &probe, &mut output);
                let due = start + Duration::from_millis((i + 1) as u64 * 10);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            probe.sink_queued(0);
            output.zeroize();
            let result = probe.stop();
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|_| "probe summary failed")?
            );
            if result.failed {
                return Err("probe failed".into());
            }
        }
        _ => {
            eprintln!("usage: call_media_probe create-fixtures DIR PUBLIC_CONFIG | rotate-fixtures OLD_DIR NEW_DIR | relay DIR BIND [SYNTHETIC_SERVICE_CONFIG] | endpoint FIXTURE SECONDS | local");
            return Err("invalid probe command".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_sink_keeps_fractional_clock_progress_and_bounds_waiting_pcm() {
        let mut sink = FixturePlayback {
            consumed: 0,
            queued: 640,
        };
        sink.advance(Duration::from_micros(10_001));
        assert_eq!(sink.queued, 480);
        sink.advance(Duration::from_micros(10_020));
        assert_eq!(sink.queued, 480);
        sink.advance(Duration::from_micros(10_063));
        assert_eq!(sink.queued, 479);
        sink.advance(Duration::from_millis(40));
        assert_eq!(sink.queued, 0);
        sink.advance(Duration::from_secs(4));
        assert_eq!(sink.queued, 0); // a stall cannot accumulate historical output
        assert_eq!(sink.consumed, 64_000);
    }
}

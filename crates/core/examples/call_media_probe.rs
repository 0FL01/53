//! Fixture authority / opaque private relay / synthetic endpoint. No msgd DB.
use dmsg_core::voice_probe::{fixture, relay, test_tone, Probe};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

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
        Some("relay") if args.len() == 4 => {
            let dir = Path::new(&args[2]);
            let route = fixture::read_private(&dir.join("relay.json"), 4096)?;
            let route = serde_json::from_slice(&route).map_err(|_| "invalid relay fixture")?;
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
                relay::serve(listener, route, Zeroizing::new(key), stopped).await
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
            while start.elapsed() < Duration::from_secs(seconds) {
                probe.push(&test_tone(position, 160));
                probe.pull(&mut output);
                position += 160;
                if probe.snapshot().failed {
                    break;
                }
                let due = start + Duration::from_micros(position as u64 * 1_000_000 / 16000);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
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
            for i in 0..300 {
                probe.push(&test_tone(i * 160, 160));
                probe.pull(&mut output);
                std::thread::sleep(Duration::from_millis(10));
            }
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
            eprintln!("usage: call_media_probe create-fixtures DIR PUBLIC_CONFIG | rotate-fixtures OLD_DIR NEW_DIR | relay DIR BIND | endpoint FIXTURE SECONDS | local");
            return Err("invalid probe command".into());
        }
    }
    Ok(())
}

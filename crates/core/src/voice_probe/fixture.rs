//! Private M1-V files, not account credentials or production call negotiation.
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};
use zeroize::{Zeroize, Zeroizing};

/// Existing pinned carrier choices; this fixture never replaces the algorithm.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CarrierCongestion {
    #[default]
    Dcubic,
    Bbr,
}

impl CarrierCongestion {
    pub fn native(self) -> slipstream_sys::CongestionControl {
        match self {
            Self::Dcubic => slipstream_sys::CongestionControl::Dcubic,
            Self::Bbr => slipstream_sys::CongestionControl::Bbr,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarrierFixture {
    pub domain: String,
    pub resolvers: Vec<String>,
    pub certificate_path: String,
    #[serde(default)]
    pub congestion_control: CarrierCongestion,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointFixture {
    pub version: u8,
    pub generation: [u8; 16],
    pub profile_ms: u16,
    pub role: u8,
    pub domain: String,
    pub relay_addr: String,
    pub noise_private: [u8; 32],
    pub relay_public: [u8; 32],
    pub media_tx: [u8; 30],
    pub media_rx: [u8; 30],
    pub ssrc_tx: u32,
    pub ssrc_rx: u32,
    pub cname: [u8; 8],
    pub peer_cname: [u8; 8],
    pub initial_sequence: u16,
    pub peer_initial_sequence: u16,
    pub initial_timestamp: u32,
    pub peer_initial_timestamp: u32,
    /// Measured healthy commit-to-terminal-feedback cycle; frozen per run.
    pub healthy_cycle_ms: u32,
    /// Independently measured useful tunnel capacity, not local write rate.
    pub capacity_bps: u32,
    pub carrier: Option<CarrierFixture>,
}

impl Drop for EndpointFixture {
    fn drop(&mut self) {
        self.noise_private.zeroize();
        self.media_tx.zeroize();
        self.media_rx.zeroize();
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayFixture {
    pub version: u8,
    pub generation: [u8; 16],
    pub domain: String,
    pub device_public: [[u8; 32]; 2],
    pub profile_ms: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicConfig {
    pub relay_addr: String,
    pub domain: String,
    pub profile_ms: u16,
    pub healthy_cycle_ms: u32,
    pub capacity_bps: u32,
    pub carriers: [Option<CarrierFixture>; 2],
}

pub type FixturePair = (
    EndpointFixture,
    EndpointFixture,
    RelayFixture,
    Zeroizing<[u8; 32]>,
);

pub fn profile(ms: u16) -> Result<dmsg_opus_sys::live::LiveProfile, String> {
    use dmsg_opus_sys::live::LiveProfile;
    match ms {
        20 => Ok(LiveProfile::Ms20),
        40 => Ok(LiveProfile::Ms40),
        60 => Ok(LiveProfile::Ms60),
        _ => Err("invalid probe duration".into()),
    }
}

impl EndpointFixture {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = read_private(path, 16 * 1024)?;
        let fixture: Self = serde_json::from_slice(&raw).map_err(|_| "invalid endpoint fixture")?;
        fixture.validate()?;
        Ok(fixture)
    }

    pub fn validate(&self) -> Result<(), String> {
        profile(self.profile_ms)?;
        if self.version != 1
            || self.role > 1
            || self.generation == [0; 16]
            || self.domain.is_empty()
            || self.domain.len() > dmsg_protocol::DOMAIN_MAX
            || self.relay_addr.is_empty()
            || self.ssrc_tx == 0
            || self.ssrc_rx == 0
            || self.ssrc_tx == self.ssrc_rx
            || self.media_tx == self.media_rx
            || self.media_tx == [0; 30]
            || self.media_rx == [0; 30]
            || self.noise_private == [0; 32]
            || self.relay_public == [0; 32]
            || !(200..=5000).contains(&self.healthy_cycle_ms)
            || !(50_000..=80_000).contains(&self.capacity_bps)
            || self.cname == self.peer_cname
            || !self
                .cname
                .iter()
                .chain(&self.peer_cname)
                .all(|b| b.is_ascii_graphic())
        {
            return Err("invalid endpoint fixture binding".into());
        }
        Ok(())
    }
}

/// Symlinks, writable/group-readable files and unrelated owners are forbidden.
pub fn read_private(path: &Path, limit: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let before = fs::symlink_metadata(path).map_err(|_| "fixture unavailable")?;
    if !before.is_file()
        || before.mode() & 0o777 != 0o400
        || before.uid() != unsafe { geteuid() }
        || before.len() > limit as u64
    {
        return Err("fixture must be owner-only read-only regular file".into());
    }
    let file = fs::File::open(path).map_err(|_| "fixture unavailable")?;
    let after = file.metadata().map_err(|_| "fixture unavailable")?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mode() != after.mode()
        || before.uid() != after.uid()
        || before.len() != after.len()
    {
        return Err("fixture changed during open".into());
    }
    use std::io::Read;
    let mut raw = Zeroizing::new(Vec::new());
    file.take(limit as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "fixture read failed")?;
    if raw.len() > limit {
        return Err("fixture exceeds bound".into());
    }
    Ok(raw)
}

pub fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| "fixture random generation failed")?;
    Ok(bytes)
}

pub fn public_key(private: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*private)).to_bytes()
}

/// Test authority only: returns two secrets and a media-key-free relay binding.
pub fn pair(config: PublicConfig) -> Result<FixturePair, String> {
    let generation = random()?;
    let relay_key = Zeroizing::new(random()?);
    let device_a = Zeroizing::new(random()?);
    let device_b = Zeroizing::new(random()?);
    let ab = Zeroizing::new(random()?);
    let ba = Zeroizing::new(random()?);
    let ssrc_a = u32::from_be_bytes(random()?);
    let ssrc_b = u32::from_be_bytes(random()?);
    let sequence_a = u16::from_be_bytes(random()?);
    let sequence_b = u16::from_be_bytes(random()?);
    let timestamp_a = u32::from_be_bytes(random()?);
    let timestamp_b = u32::from_be_bytes(random()?);
    let cname_a = random::<8>()?.map(|b| b"0123456789abcdef"[usize::from(b & 15)]);
    let cname_b = random::<8>()?.map(|b| b"0123456789abcdef"[usize::from(b & 15)]);
    let make = |role: u8| EndpointFixture {
        version: 1,
        generation,
        profile_ms: config.profile_ms,
        role,
        domain: config.domain.clone(),
        relay_addr: config.relay_addr.clone(),
        noise_private: if role == 0 { *device_a } else { *device_b },
        relay_public: public_key(&relay_key),
        media_tx: if role == 0 { *ab } else { *ba },
        media_rx: if role == 0 { *ba } else { *ab },
        ssrc_tx: if role == 0 { ssrc_a } else { ssrc_b },
        ssrc_rx: if role == 0 { ssrc_b } else { ssrc_a },
        cname: if role == 0 { cname_a } else { cname_b },
        peer_cname: if role == 0 { cname_b } else { cname_a },
        initial_sequence: if role == 0 { sequence_a } else { sequence_b },
        peer_initial_sequence: if role == 0 { sequence_b } else { sequence_a },
        initial_timestamp: if role == 0 { timestamp_a } else { timestamp_b },
        peer_initial_timestamp: if role == 0 { timestamp_b } else { timestamp_a },
        healthy_cycle_ms: config.healthy_cycle_ms,
        capacity_bps: config.capacity_bps,
        carrier: config.carriers[usize::from(role)].clone(),
    };
    let a = make(0);
    let b = make(1);
    a.validate()?;
    b.validate()?;
    let relay = RelayFixture {
        version: 1,
        generation,
        domain: config.domain,
        device_public: [public_key(&device_a), public_key(&device_b)],
        profile_ms: config.profile_ms,
    };
    Ok((a, b, relay, relay_key))
}

/// New epoch, same fixture trust identities. Never resets counters under old keys.
pub fn rotate(
    a: EndpointFixture,
    b: EndpointFixture,
    relay_key: Zeroizing<[u8; 32]>,
) -> Result<FixturePair, String> {
    a.validate()?;
    b.validate()?;
    if a.role != 0
        || b.role != 1
        || a.generation != b.generation
        || a.media_tx != b.media_rx
        || a.media_rx != b.media_tx
        || a.ssrc_tx != b.ssrc_rx
        || a.ssrc_rx != b.ssrc_tx
        || a.relay_public != public_key(&relay_key)
        || b.relay_public != a.relay_public
        || a.profile_ms != b.profile_ms
        || a.domain != b.domain
    {
        return Err("inconsistent prior fixture pair".into());
    }
    let config = PublicConfig {
        relay_addr: a.relay_addr.clone(),
        domain: a.domain.clone(),
        profile_ms: a.profile_ms,
        healthy_cycle_ms: a.healthy_cycle_ms.max(b.healthy_cycle_ms),
        capacity_bps: a.capacity_bps.min(b.capacity_bps),
        carriers: [a.carrier.clone(), b.carrier.clone()],
    };
    let (mut fresh_a, mut fresh_b, mut relay, generated_key) = pair(config)?;
    drop(generated_key);
    fresh_a.noise_private.zeroize();
    fresh_b.noise_private.zeroize();
    fresh_a.noise_private = a.noise_private;
    fresh_b.noise_private = b.noise_private;
    fresh_a.relay_public = a.relay_public;
    fresh_b.relay_public = b.relay_public;
    relay.device_public = [public_key(&a.noise_private), public_key(&b.noise_private)];
    Ok((fresh_a, fresh_b, relay, relay_key))
}

/// Create new 0400 fixture files; never overwrite identities or existing runs.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "cannot create fresh fixture file")?;
    file.write_all(bytes).map_err(|_| "fixture write failed")?;
    file.sync_all().map_err(|_| "fixture sync failed")?;
    file.set_permissions(fs::Permissions::from_mode(0o400))
        .map_err(|_| "fixture permissions failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_selects_only_existing_carrier_algorithms_and_preserves_default() {
        let carrier = |algorithm: Option<&str>| {
            let mut value = serde_json::json!({
                "domain": "fixture.test",
                "resolvers": ["127.0.0.1:5353"],
                "certificate_path": "private-cert.pem"
            });
            if let Some(algorithm) = algorithm {
                value["congestion_control"] = algorithm.into();
            }
            serde_json::from_value::<CarrierFixture>(value)
        };
        assert_eq!(
            carrier(None).unwrap().congestion_control.native(),
            slipstream_sys::CongestionControl::Dcubic
        );
        assert_eq!(
            carrier(Some("bbr")).unwrap().congestion_control.native(),
            slipstream_sys::CongestionControl::Bbr
        );
        assert_eq!(
            carrier(Some("dcubic")).unwrap().congestion_control.native(),
            slipstream_sys::CongestionControl::Dcubic
        );
        assert!(carrier(Some("newreno")).is_err());
        assert!(carrier(Some("BBR")).is_err());
    }

    #[test]
    fn fresh_epoch_keeps_pins_but_never_reuses_media_keys() {
        let config = PublicConfig {
            relay_addr: "127.0.0.1:1".into(),
            domain: "fixture.test".into(),
            profile_ms: 40,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
            carriers: [None, None],
        };
        let (a, b, before, key) = pair(config).unwrap();
        let old_key = Zeroizing::new(a.media_tx);
        let old_pin = a.relay_public;
        let old_a = Zeroizing::new(a.noise_private);
        let (a, b, after, key) = rotate(a, b, key).unwrap();
        assert_ne!(before.generation, after.generation);
        assert_eq!(before.device_public, after.device_public);
        assert_eq!(a.noise_private, *old_a);
        assert_eq!(a.relay_public, old_pin);
        assert_eq!(public_key(&key), old_pin);
        assert_ne!(a.media_tx, *old_key);
        assert_eq!(a.media_tx, b.media_rx);
        assert_eq!(a.ssrc_tx, b.ssrc_rx);
        let raw = serde_json::to_vec(&after).unwrap();
        assert!(!String::from_utf8(raw).unwrap().contains("media_"));
    }
}

//! Bounded authenticated incoming contact requests; no login directory.
use crate::auth::{self, DeviceBinding};

pub const REQUESTS_MAX: usize = 32;
pub const PROFILE_LEN: usize = 124;

#[derive(Debug, PartialEq, Eq)]
pub struct PeerProfile {
    pub contact_id: String,
    pub binding: DeviceBinding,
}

pub fn build_requests(profiles: &[PeerProfile]) -> Option<Vec<u8>> {
    if profiles.len() > REQUESTS_MAX {
        return None;
    }
    let mut out = vec![profiles.len() as u8];
    for p in profiles {
        auth::build_authenticated(&p.binding.user_id, &p.contact_id).ok()?;
        out.extend_from_slice(p.contact_id.as_bytes());
        out.extend_from_slice(&auth::build_binding(
            &p.binding.user_id,
            &p.binding.device_key,
            &p.binding.ed25519,
            &p.binding.curve25519,
        ));
    }
    Some(out)
}

pub fn parse_requests(payload: &[u8]) -> Option<Vec<PeerProfile>> {
    let count = usize::from(*payload.first()?);
    if count > REQUESTS_MAX || payload.len() != 1 + count * PROFILE_LEN {
        return None;
    }
    payload[1..]
        .chunks_exact(PROFILE_LEN)
        .map(|raw| {
            let binding = auth::parse_binding(&raw[12..]).ok()?;
            let contact_id = std::str::from_utf8(&raw[..12]).ok()?.to_owned();
            auth::build_authenticated(&binding.user_id, &contact_id).ok()?;
            Some(PeerProfile {
                contact_id,
                binding,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bounded_profiles() {
        let p = PeerProfile {
            contact_id: "0123456789AB".into(),
            binding: DeviceBinding {
                user_id: [1; 16],
                device_key: [2; 32],
                ed25519: [3; 32],
                curve25519: [4; 32],
            },
        };
        let bytes = build_requests(&[p]).unwrap();
        assert_eq!(bytes.len(), 125);
        assert_eq!(
            parse_requests(&bytes).unwrap()[0].binding.device_key,
            [2; 32]
        );
        assert!(parse_requests(&bytes[..124]).is_none());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(parse_requests(&trailing).is_none());
        assert!(parse_requests(&[33]).is_none());
        assert_eq!(parse_requests(&[0]), Some(vec![]));
    }
}

//! Public server profile: `dmsg://server/<base64url-no-pad(versioned-binary)>`.
//! Layout: version 1 + domain_len u8 + domain + cert_len u16 BE + full cert DER
//! + Noise public key 32. There is no secret or invitation in this profile.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

pub const URI_PREFIX: &str = "dmsg://server/";
pub const PROFILE_VERSION: u8 = 1;
/// Maximum raw profile size. Checked before allocating in the builder.
pub const PROFILE_MAX: usize = 4 * 1024;
/// Maximum unpadded base64 length, checked before decoding any input.
pub const B64_LEN_MAX: usize = (PROFILE_MAX * 4 + 2) / 3;

#[derive(Debug, PartialEq, Eq)]
pub struct ServerProfile {
    pub domain: Vec<u8>,
    pub cert_der: Vec<u8>,
    pub noise_pubkey: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProfileError {
    BadPrefix,
    BadEncoding,
    /// Truncated, trailing bytes, or size limit exceeded.
    Truncated,
    BadVersion(u8),
    BadDomain,
    BadCert,
}

/// ASCII LDH labels of 1..=63 bytes, total 1..=DOMAIN_MAX bytes, with no empty
/// labels, trailing dot, or leading/trailing label hyphens.
pub fn valid_domain(domain: &[u8]) -> bool {
    if domain.is_empty() || domain.len() > super::DOMAIN_MAX || !domain.is_ascii() {
        return false;
    }
    domain.split(|&b| b == b'.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with(b"-")
            && !label.ends_with(b"-")
            && label
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    })
}

/// Minimal DER check only; carrier TLS consumes and verifies the full pin.
fn valid_cert_der(cert: &[u8]) -> bool {
    cert.len() >= 2 && cert[0] == 0x30
}

pub fn build(
    domain: &[u8],
    cert_der: &[u8],
    noise_pubkey: &[u8; 32],
) -> Result<String, ProfileError> {
    if !valid_domain(domain) {
        return Err(ProfileError::BadDomain);
    }
    if !valid_cert_der(cert_der) {
        return Err(ProfileError::BadCert);
    }
    let raw_len = (1usize + 1 + 2 + 32)
        .checked_add(domain.len())
        .and_then(|len| len.checked_add(cert_der.len()))
        .filter(|&len| len <= PROFILE_MAX)
        .ok_or(ProfileError::Truncated)?;
    let dlen = u8::try_from(domain.len()).map_err(|_| ProfileError::BadDomain)?;
    let clen = u16::try_from(cert_der.len()).map_err(|_| ProfileError::Truncated)?;
    let mut raw = Vec::with_capacity(raw_len);
    raw.extend_from_slice(&[PROFILE_VERSION, dlen]);
    raw.extend_from_slice(domain);
    raw.extend_from_slice(&clen.to_be_bytes());
    raw.extend_from_slice(cert_der);
    raw.extend_from_slice(noise_pubkey);
    Ok(format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw)))
}

/// Bounded, canonical base64url decoding and exact raw consumption. Old join
/// URIs and any appended secret bytes are rejected.
pub fn parse(uri: &str) -> Result<ServerProfile, ProfileError> {
    let b64 = uri
        .strip_prefix(URI_PREFIX)
        .ok_or(ProfileError::BadPrefix)?;
    if b64.len() > B64_LEN_MAX {
        return Err(ProfileError::Truncated);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ProfileError::BadEncoding)?;
    if raw.len() > PROFILE_MAX || raw.len() < 1 + 1 + 2 + 32 {
        return Err(ProfileError::Truncated);
    }
    if raw[0] != PROFILE_VERSION {
        return Err(ProfileError::BadVersion(raw[0]));
    }
    let dlen = usize::from(raw[1]);
    if dlen == 0 || dlen > super::DOMAIN_MAX {
        return Err(ProfileError::BadDomain);
    }
    let mut off = 2;
    let domain = raw.get(off..off + dlen).ok_or(ProfileError::Truncated)?;
    if !valid_domain(domain) {
        return Err(ProfileError::BadDomain);
    }
    off += dlen;
    let clen = usize::from(u16::from_be_bytes(
        raw.get(off..off + 2)
            .ok_or(ProfileError::Truncated)?
            .try_into()
            .map_err(|_| ProfileError::Truncated)?,
    ));
    off += 2;
    let cert_der = raw.get(off..off + clen).ok_or(ProfileError::Truncated)?;
    if !valid_cert_der(cert_der) {
        return Err(ProfileError::BadCert);
    }
    off += clen;
    let noise_pubkey = raw
        .get(off..off + 32)
        .ok_or(ProfileError::Truncated)?
        .try_into()
        .map_err(|_| ProfileError::Truncated)?;
    off += 32;
    if off != raw.len() {
        return Err(ProfileError::Truncated);
    }
    Ok(ServerProfile {
        domain: domain.to_vec(),
        cert_der: cert_der.to_vec(),
        noise_pubkey,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<u8>, Vec<u8>, [u8; 32]) {
        (b"msg.example.com".to_vec(), vec![0x30; 1024], [9; 32])
    }

    fn uri_from_raw(raw: &[u8]) -> String {
        format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(raw))
    }

    fn raw_profile(domain: &[u8], cert: &[u8]) -> Vec<u8> {
        let mut raw = vec![PROFILE_VERSION, u8::try_from(domain.len()).unwrap()];
        raw.extend_from_slice(domain);
        raw.extend_from_slice(&u16::try_from(cert.len()).unwrap().to_be_bytes());
        raw.extend_from_slice(cert);
        raw.extend_from_slice(&[9; 32]);
        raw
    }

    #[test]
    fn roundtrip() {
        let (d, c, n) = sample();
        let uri = build(&d, &c, &n).unwrap();
        assert!(uri.starts_with(URI_PREFIX));
        let profile = parse(&uri).unwrap();
        assert_eq!(
            (profile.domain, profile.cert_der, profile.noise_pubkey),
            (d, c, n)
        );
    }

    #[test]
    fn exact_public_layout_vector() {
        let uri = build(b"x", &[0x30, 0], &[9; 32]).unwrap();
        let raw = URL_SAFE_NO_PAD.decode(&uri[URI_PREFIX.len()..]).unwrap();
        assert_eq!(&raw[..7], &[1, 1, b'x', 0, 2, 0x30, 0]);
        assert_eq!(&raw[7..], &[9; 32]);
        assert_eq!(raw.len(), 39);
    }

    #[test]
    fn oversize_der_rejected_and_exact_cap_allowed() {
        let (d, _, n) = sample();
        let mut big = vec![0x30; PROFILE_MAX];
        big[1] = 0x82;
        assert_eq!(build(&d, &big, &n), Err(ProfileError::Truncated));
        let cert = vec![0x30; PROFILE_MAX - (36 + d.len())];
        let uri = build(&d, &cert, &n).unwrap();
        assert_eq!(uri.len() - URI_PREFIX.len(), B64_LEN_MAX);
        assert_eq!(parse(&uri).unwrap().cert_der, cert);
        assert_eq!(
            parse(&uri_from_raw(&raw_profile(&d, &big))),
            Err(ProfileError::Truncated)
        );
    }

    #[test]
    fn truncation_rejected() {
        let (d, c, n) = sample();
        let uri = build(&d, &c, &n).unwrap();
        assert_eq!(parse(&uri[..uri.len() - 4]), Err(ProfileError::Truncated));
        assert_eq!(parse("https://x/"), Err(ProfileError::BadPrefix));
        assert_eq!(parse("dmsg://server/!!!"), Err(ProfileError::BadEncoding));
        let raw = raw_profile(&d, &c);
        for end in 0..raw.len() {
            assert!(
                parse(&uri_from_raw(&raw[..end])).is_err(),
                "prefix length {end}"
            );
        }
    }

    #[test]
    fn bad_version_rejected() {
        let (d, c, _) = sample();
        let mut raw = raw_profile(&d, &c);
        raw[0] = 9;
        assert_eq!(parse(&uri_from_raw(&raw)), Err(ProfileError::BadVersion(9)));
    }

    #[test]
    fn b64_len_precheck_rejects_before_decode() {
        let big = format!("{URI_PREFIX}{}", "A".repeat(B64_LEN_MAX + 1024));
        assert_eq!(parse(&big), Err(ProfileError::Truncated));
        let invalid_big = format!("{URI_PREFIX}{}", "!".repeat(B64_LEN_MAX + 1));
        assert_eq!(parse(&invalid_big), Err(ProfileError::Truncated));
        let (d, c, n) = sample();
        let uri = build(&d, &c, &n).unwrap();
        assert!(uri.len() - URI_PREFIX.len() <= B64_LEN_MAX);
        assert!(parse(&uri).is_ok());
    }

    #[test]
    fn der_minimum_rejected() {
        let (d, _, n) = sample();
        for cert in [&[0x31, 0x82, 0x01, 0x00][..], &[][..], &[0x30][..]] {
            assert_eq!(build(&d, cert, &n), Err(ProfileError::BadCert));
            assert_eq!(
                parse(&uri_from_raw(&raw_profile(&d, cert))),
                Err(ProfileError::BadCert)
            );
        }
        assert!(parse(&uri_from_raw(&raw_profile(
            &d,
            &[0x30, 0x03, 0x01, 0x01, 0x00]
        )))
        .is_ok());
    }

    #[test]
    fn ldh_domain_vectors() {
        let cert = [0x30, 0x03, 0x01, 0x01, 0x00];
        for bad in [
            &b"-lead.com"[..],
            &b"trail-.com"[..],
            &b"a..b"[..],
            &b".lead"[..],
            &b"trail."[..],
            &b"under_score.com"[..],
            &b""[..],
            &b"a\x7fb.com"[..],
            &b"a\xffb.com"[..],
            &b"a/b.com"[..],
        ] {
            assert_eq!(
                build(bad, &cert, &[9; 32]),
                Err(ProfileError::BadDomain),
                "build {bad:?}"
            );
            assert_eq!(
                parse(&uri_from_raw(&raw_profile(bad, &cert))),
                Err(ProfileError::BadDomain),
                "parse {bad:?}"
            );
        }
        assert!(parse(&uri_from_raw(&raw_profile(b"a-b.x1--y.com", &cert))).is_ok());
        assert!(valid_domain(&[b'a'; 63]));
        assert!(!valid_domain(&[b'a'; 64]));
        let max = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert_eq!(max.len(), super::super::DOMAIN_MAX);
        assert!(build(max.as_bytes(), &cert, &[9; 32]).is_ok());
        let over = format!("{max}d");
        assert_eq!(
            build(over.as_bytes(), &cert, &[9; 32]),
            Err(ProfileError::BadDomain)
        );
        assert_eq!(
            parse(&uri_from_raw(&raw_profile(over.as_bytes(), &cert))),
            Err(ProfileError::BadDomain)
        );
    }

    #[test]
    fn trailing_and_huge_clen_rejected() {
        let (d, c, _) = sample();
        let mut raw = raw_profile(&d, &c);
        raw.push(0xFF);
        assert_eq!(parse(&uri_from_raw(&raw)), Err(ProfileError::Truncated));
        let mut raw2 = vec![PROFILE_VERSION, 1, b'x', 0xFF, 0xFF];
        raw2.extend_from_slice(&[0; 32]);
        assert_eq!(parse(&uri_from_raw(&raw2)), Err(ProfileError::Truncated));
    }

    #[test]
    fn old_join_and_trailing_secret_rejected() {
        let mut old = raw_profile(b"msg.example.com", &[0x30, 0]);
        old.extend_from_slice(&[7; 32]);
        assert_eq!(
            parse(&format!("dmsg://join/{}", URL_SAFE_NO_PAD.encode(&old))),
            Err(ProfileError::BadPrefix)
        );
        assert_eq!(parse(&uri_from_raw(&old)), Err(ProfileError::Truncated));
    }

    #[test]
    fn base64_padding_and_noncanonical_tail_rejected() {
        let uri = build(b"xx", &[0x30, 0], &[0; 32]).unwrap();
        assert_eq!(parse(&format!("{uri}==")), Err(ProfileError::BadEncoding));
        // 40 raw bytes: the last sextet has four unused bits, which must be zero.
        assert!(uri.ends_with('A'));
        let bad = format!("{}B", &uri[..uri.len() - 1]);
        assert_eq!(parse(&bad), Err(ProfileError::BadEncoding));
    }
}

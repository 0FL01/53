//! Bootstrap invite P3: `dmsg://join/<base64url(versioned-binary)>`.
//! Layout: `[version:u8=1][domain_len:u8][domain][cert_len:u16 BE][cert DER]`
//! `[noise_pubkey:32][token:32]`. Только пабкеи и bearer-token — приватных
//! ключей нет. QR — тот же payload. Bounded: сырые байты ≤ BOOTSTRAP_MAX.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

/// Префикс URI приглашения.
pub const URI_PREFIX: &str = "dmsg://join/";
/// Версия bootstrap-формата.
pub const BOOTSTRAP_VERSION: u8 = 1;
/// Кап сырых байт bootstrap (DER ~1 KiB влезает с запасом).
pub const BOOTSTRAP_MAX: usize = 4 * 1024;

/// Разобранный bootstrap.
#[derive(Debug, PartialEq, Eq)]
pub struct Bootstrap {
    /// Туннельный домен.
    pub domain: Vec<u8>,
    /// Полный pinned carrier cert DER.
    pub cert_der: Vec<u8>,
    /// Noise static-публичный ключ msgd.
    pub noise_pubkey: [u8; 32],
    /// Bearer-token приглашения (256 бит).
    pub token: [u8; 32],
}

/// Ошибка парсинга/сборки.
#[derive(Debug, PartialEq, Eq)]
pub enum BootstrapError {
    /// Неверный префикс URI.
    BadPrefix,
    /// Не base64url.
    BadEncoding,
    /// Обрезано или кап превышен.
    Truncated,
    /// Неизвестная версия формата.
    BadVersion(u8),
    /// Домен пуст/слишком длинен/не ascii.
    BadDomain,
}

/// Собрать URI из частей.
pub fn build(domain: &[u8], cert_der: &[u8], noise_pubkey: &[u8; 32], token: &[u8; 32]) -> Result<String, BootstrapError> {
    if domain.is_empty() || domain.len() > super::DOMAIN_MAX || !domain.is_ascii() {
        return Err(BootstrapError::BadDomain);
    }
    let mut raw = Vec::with_capacity(1 + 1 + domain.len() + 2 + cert_der.len() + 64);
    raw.push(BOOTSTRAP_VERSION);
    raw.push(domain.len() as u8);
    raw.extend_from_slice(domain);
    raw.extend_from_slice(&(cert_der.len() as u16).to_be_bytes());
    raw.extend_from_slice(cert_der);
    raw.extend_from_slice(noise_pubkey);
    raw.extend_from_slice(token);
    if raw.len() > BOOTSTRAP_MAX {
        return Err(BootstrapError::Truncated);
    }
    Ok(format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw)))
}

/// Разобрать URI в части. Строго bounded, без аллокаций сверх результата.
pub fn parse(uri: &str) -> Result<Bootstrap, BootstrapError> {
    let b64 = uri.strip_prefix(URI_PREFIX).ok_or(BootstrapError::BadPrefix)?;
    let raw = URL_SAFE_NO_PAD.decode(b64).map_err(|_| BootstrapError::BadEncoding)?;
    if raw.len() > BOOTSTRAP_MAX || raw.len() < 1 + 1 + 2 + 32 + 32 {
        return Err(BootstrapError::Truncated);
    }
    if raw[0] != BOOTSTRAP_VERSION {
        return Err(BootstrapError::BadVersion(raw[0]));
    }
    let dlen = raw[1] as usize;
    if dlen == 0 || dlen > super::DOMAIN_MAX {
        return Err(BootstrapError::BadDomain);
    }
    let mut off = 2;
    let domain = raw.get(off..off + dlen).ok_or(BootstrapError::Truncated)?.to_vec();
    if !domain.is_ascii() {
        return Err(BootstrapError::BadDomain);
    }
    off += dlen;
    let clen = u16::from_be_bytes(raw.get(off..off + 2).ok_or(BootstrapError::Truncated)?.try_into().unwrap()) as usize;
    off += 2;
    let cert_der = raw.get(off..off + clen).ok_or(BootstrapError::Truncated)?.to_vec();
    off += clen;
    let noise_pubkey: [u8; 32] = raw.get(off..off + 32).ok_or(BootstrapError::Truncated)?.try_into().unwrap();
    off += 32;
    let token: [u8; 32] = raw.get(off..off + 32).ok_or(BootstrapError::Truncated)?.try_into().unwrap();
    off += 32;
    if off != raw.len() {
        return Err(BootstrapError::Truncated);
    }
    Ok(Bootstrap { domain, cert_der, noise_pubkey, token })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<u8>, Vec<u8>, [u8; 32], [u8; 32]) {
        (b"msg.example.com".to_vec(), vec![0x30u8; 1024], [9u8; 32], [7u8; 32])
    }

    #[test]
    fn roundtrip() {
        let (d, c, n, t) = sample();
        let uri = build(&d, &c, &n, &t).unwrap();
        assert!(uri.starts_with(URI_PREFIX));
        let b = parse(&uri).unwrap();
        assert_eq!((b.domain, b.cert_der, b.noise_pubkey, b.token), (d, c, n, t));
    }

    #[test]
    fn oversize_der_rejected() {
        let (d, _, n, t) = sample();
        assert_eq!(build(&d, &vec![0u8; BOOTSTRAP_MAX], &n, &t), Err(BootstrapError::Truncated));
    }

    #[test]
    fn truncation_rejected() {
        let (d, c, n, t) = sample();
        let uri = build(&d, &c, &n, &t).unwrap();
        assert_eq!(parse(&uri[..uri.len() - 4]), Err(BootstrapError::Truncated));
        assert_eq!(parse("https://x/"), Err(BootstrapError::BadPrefix));
        assert_eq!(parse("dmsg://join/!!!"), Err(BootstrapError::BadEncoding));
    }

    #[test]
    fn bad_version_rejected() {
        let (d, c, n, t) = sample();
        let uri = build(&d, &c, &n, &t).unwrap();
        let mut raw = URL_SAFE_NO_PAD.decode(&uri[URI_PREFIX.len()..]).unwrap();
        raw[0] = 9;
        assert_eq!(parse(&format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw))), Err(BootstrapError::BadVersion(9)));
    }
}

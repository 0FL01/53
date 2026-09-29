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
/// Кап длины base64url-части ДО decode (BOOTSTRAP_MAX сырых байт — не длиннее
/// BOOTSTRAP_MAX*4/3+4 символов). Пре-чек режет гигантский ввод без аллокаций
/// по заявленной длине: decode вызывается только на короткую строку.
/// Контракт: любой b64 длиннее — Truncated, ложных срабатываний нет
/// (валидный raw ≤ BOOTSTRAP_MAX кодируется короче капа).
pub const B64_LEN_MAX: usize = BOOTSTRAP_MAX * 4 / 3 + 4;

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
    /// Домен пуст/не LDH/слишком длинен/не ascii.
    BadDomain,
    /// Сертификат пуст или не DER-SEQUENCE (минимум: 0x30 + байт длины).
    BadCert,
}

/// LDH-домен: метки `[A-Za-z0-9-]`, 1–63 байта, без ведущих/конечных дефисов,
/// без пустых меток (`..`, ведущая/конечная точка), без control.
/// Контракт: проверяется и в build (конфиг оператора, fail-closed),
/// и в parse (сетевые байты). IP-литералы и trailing-dot не принимаем
/// (v1 — только DNS-имена). No panic: только split/iter по реальным байтам.
pub fn valid_domain(d: &[u8]) -> bool {
    if d.is_empty() || d.len() > super::DOMAIN_MAX || !d.is_ascii() {
        return false;
    }
    for label in d.split(|&b| b == b'.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if label.starts_with(b"-") || label.ends_with(b"-") {
            return false;
        }
        if !label.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'-') {
            return false;
        }
    }
    true
}

/// DER-минимум pinned carrier-серта: непустой, тег SEQUENCE (0x30) + байт длины.
/// Контракт: полная X.509-валидация — non-goal; парсер ловит только
/// truncation/мусор, подлинность — pin-сверкой на клиенте (ARCH §6).
fn valid_cert_der(c: &[u8]) -> bool {
    c.len() >= 2 && c[0] == 0x30
}

/// Собрать URI из частей. Fail-closed: домен — LDH, серт — DER-минимум,
// иначе downstream-парсер всё равно отвергнет (roundtrip-когерентность).
pub fn build(domain: &[u8], cert_der: &[u8], noise_pubkey: &[u8; 32], token: &[u8; 32]) -> Result<String, BootstrapError> {
    if !valid_domain(domain) {
        return Err(BootstrapError::BadDomain);
    }
    if !valid_cert_der(cert_der) {
        return Err(BootstrapError::BadCert);
    }
    let clen = u16::try_from(cert_der.len()).map_err(|_| BootstrapError::Truncated)?;
    let mut raw = Vec::with_capacity(1 + 1 + domain.len() + 2 + cert_der.len() + 64);
    raw.push(BOOTSTRAP_VERSION);
    raw.push(domain.len() as u8);
    raw.extend_from_slice(domain);
    raw.extend_from_slice(&clen.to_be_bytes());
    raw.extend_from_slice(cert_der);
    raw.extend_from_slice(noise_pubkey);
    raw.extend_from_slice(token);
    if raw.len() > BOOTSTRAP_MAX {
        return Err(BootstrapError::Truncated);
    }
    Ok(format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw)))
}

/// Разобрать URI в части. Строго bounded, без аллокаций сверх результата.
/// No panic на сетевых байтах: индекс raw[0]/raw[1] — после проверки длины,
/// остальные срезы — через get()+map_err (без unwrap), длины из сети — через
/// try_from/usize::from (без as). Trailing-байты — Truncated.
pub fn parse(uri: &str) -> Result<Bootstrap, BootstrapError> {
    let b64 = uri.strip_prefix(URI_PREFIX).ok_or(BootstrapError::BadPrefix)?;
    if b64.len() > B64_LEN_MAX {
        return Err(BootstrapError::Truncated);
    }
    let raw = URL_SAFE_NO_PAD.decode(b64).map_err(|_| BootstrapError::BadEncoding)?;
    if raw.len() > BOOTSTRAP_MAX || raw.len() < 1 + 1 + 2 + 32 + 32 {
        return Err(BootstrapError::Truncated);
    }
    if raw[0] != BOOTSTRAP_VERSION {
        return Err(BootstrapError::BadVersion(raw[0]));
    }
    let dlen = usize::from(raw[1]);
    if dlen == 0 || dlen > super::DOMAIN_MAX {
        return Err(BootstrapError::BadDomain);
    }
    let mut off = 2;
    let domain = raw.get(off..off + dlen).ok_or(BootstrapError::Truncated)?.to_vec();
    if !valid_domain(&domain) {
        return Err(BootstrapError::BadDomain);
    }
    off += dlen;
    let clen = usize::from(u16::from_be_bytes(
        raw.get(off..off + 2).ok_or(BootstrapError::Truncated)?.try_into().map_err(|_| BootstrapError::Truncated)?,
    ));
    off += 2;
    let cert_der = raw.get(off..off + clen).ok_or(BootstrapError::Truncated)?.to_vec();
    if !valid_cert_der(&cert_der) {
        return Err(BootstrapError::BadCert);
    }
    off += clen;
    let noise_pubkey: [u8; 32] = raw.get(off..off + 32).ok_or(BootstrapError::Truncated)?.try_into().map_err(|_| BootstrapError::Truncated)?;
    off += 32;
    let token: [u8; 32] = raw.get(off..off + 32).ok_or(BootstrapError::Truncated)?.try_into().map_err(|_| BootstrapError::Truncated)?;
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
        // DER-минимум пройден (0x30..), но raw превышает кап — Truncated.
        let mut big = vec![0x30u8; BOOTSTRAP_MAX];
        big[1] = 0x82;
        assert_eq!(build(&d, &big, &n, &t), Err(BootstrapError::Truncated));
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

    /// Собрать raw вручную (мимо build-валидации): произвольные домен/сеrt.
    fn raw_uri(domain: &[u8], cert: &[u8]) -> String {
        let mut raw = vec![BOOTSTRAP_VERSION, domain.len() as u8];
        raw.extend_from_slice(domain);
        raw.extend_from_slice(&(cert.len() as u16).to_be_bytes());
        raw.extend_from_slice(cert);
        raw.extend_from_slice(&[9u8; 32]);
        raw.extend_from_slice(&[7u8; 32]);
        format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw))
    }

    #[test]
    fn b64_len_precheck_rejects_before_decode() {
        // Валидные символы, но длиннее капа: Truncated без декодирования гиганта.
        let big = format!("{URI_PREFIX}{}", "A".repeat(B64_LEN_MAX + 1024));
        assert_eq!(parse(&big), Err(BootstrapError::Truncated));
        // Граница: ровно кап валидного b64 короткого raw — декодируется нормально.
        let (d, c, n, t) = sample();
        let uri = build(&d, &c, &n, &t).unwrap();
        assert!(uri[URI_PREFIX.len()..].len() <= B64_LEN_MAX);
        assert!(parse(&uri).is_ok());
    }

    #[test]
    fn der_minimum_rejected() {
        // build fail-closed на мусоре вместо DER.
        let (d, _, n, t) = sample();
        assert_eq!(build(&d, &[0x31, 0x82, 0x01, 0x00], &n, &t), Err(BootstrapError::BadCert));
        assert_eq!(build(&d, &[], &n, &t), Err(BootstrapError::BadCert));
        assert_eq!(build(&d, &[0x30], &n, &t), Err(BootstrapError::BadCert));
        // parse: пустой серт (clen=0) и не-SEQUENCE — BadCert, не Truncated.
        assert_eq!(parse(&raw_uri(b"msg.example.com", &[])), Err(BootstrapError::BadCert));
        assert_eq!(parse(&raw_uri(b"msg.example.com", &[0x31, 0x82, 0x01, 0x00])), Err(BootstrapError::BadCert));
        // Минимум 0x30 + длина проходит парсер.
        assert!(parse(&raw_uri(b"msg.example.com", &[0x30, 0x03, 0x01, 0x01, 0x00])).is_ok());
    }

    #[test]
    fn ldh_domain_vectors() {
        let cert = [0x30u8, 0x03, 0x01, 0x01, 0x00];
        // build fail-closed.
        for bad in [&b"-lead.com"[..], &b"trail-.com"[..], &b"a..b"[..], &b".lead"[..], &b"trail."[..], &b"under_score.com"[..], &b""[..]] {
            assert_eq!(build(bad, &cert, &[9u8; 32], &[7u8; 32]), Err(BootstrapError::BadDomain), "build {bad:?}");
        }
        // parse тех же байтов с сети — BadDomain.
        for bad in [&b"-lead.com"[..], &b"trail-.com"[..], &b"a..b"[..], &b".lead"[..], &b"trail."[..], &b"under_score.com"[..]] {
            assert_eq!(parse(&raw_uri(bad, &cert)), Err(BootstrapError::BadDomain), "parse {bad:?}");
        }
        // Control-байт в домене — BadDomain.
        assert_eq!(parse(&raw_uri(b"a\x7fb.com", &cert)), Err(BootstrapError::BadDomain));
        // Валидный дефис внутри метки — Ok.
        assert!(parse(&raw_uri(b"a-b.x1--y.com", &cert)).is_ok());
    }

    #[test]
    fn trailing_and_huge_clen_rejected() {
        let (d, c, n, t) = sample();
        let uri = build(&d, &c, &n, &t).unwrap();
        let mut raw = URL_SAFE_NO_PAD.decode(&uri[URI_PREFIX.len()..]).unwrap();
        raw.push(0xFF);
        assert_eq!(parse(&format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw))), Err(BootstrapError::Truncated));
        // clen=0xFFFF при коротком буфере — Truncated, не паника и не чтение за краем.
        let mut raw2 = vec![BOOTSTRAP_VERSION, 1, b'x', 0xFF, 0xFF];
        raw2.extend_from_slice(&[0u8; 64]);
        assert_eq!(parse(&format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(&raw2))), Err(BootstrapError::Truncated));
    }
}

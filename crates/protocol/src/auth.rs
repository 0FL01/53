//! Unified account authentication payloads, inside the completed Noise channel.
//! Credentials: login_len u8 + canonical login + password_len u16 BE + password
//! + optional flag u8 (0, or 1 followed by exactly 32 bytes).
//! Parsers borrow secrets, consume the whole payload, and never format them.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

pub const LOGIN_MIN: usize = 3;
pub const LOGIN_MAX: usize = 32;
pub const PASSWORD_MIN: usize = 8;
pub const PASSWORD_MAX: usize = 128;
pub const AUTH_PAYLOAD_MAX: usize = 1 + LOGIN_MAX + 2 + PASSWORD_MAX + 1 + 32;
pub const CONTACT_ID_LEN: usize = 12;
pub const AUTHENTICATED_LEN: usize = 16 + CONTACT_ID_LEN;
pub const BINDING_LEN: usize = 16 + 32 + 32 + 32;
const CONTACT_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RegistrationMode {
    InviteOnly = 0,
    Open = 1,
}

impl RegistrationMode {
    pub fn parse(payload: &[u8]) -> Option<Self> {
        match payload {
            [0] => Some(Self::InviteOnly),
            [1] => Some(Self::Open),
            _ => None,
        }
    }

    pub fn encode(self) -> [u8; 1] {
        [self as u8]
    }
}

/// Field-safe errors: no credentials, invitations, or input bytes are included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    BadLogin,
    BadPassword,
    BadFlag,
    BadContactId,
    BadInvitation,
    Truncated,
    TrailingData,
    Oversize,
}

/// Deliberately no Debug implementation on any credential-bearing type.
#[derive(PartialEq, Eq)]
pub struct Credentials<'a> {
    pub login: &'a str,
    pub password: &'a str,
}

#[derive(PartialEq, Eq)]
pub struct Signup<'a> {
    pub credentials: Credentials<'a>,
    pub invitation: Option<&'a [u8; 32]>,
}

#[derive(PartialEq, Eq)]
pub struct Login<'a> {
    pub credentials: Credentials<'a>,
    /// Explicitly confirmed device to replace; absent on the first login attempt.
    pub replace: Option<&'a [u8; 32]>,
}

fn valid_login(login: &str) -> bool {
    (LOGIN_MIN..=LOGIN_MAX).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

/// ASCII lowercase only, with no whitespace trimming or Unicode folding.
/// Builders use this; parsers require an already canonical wire login.
pub fn normalize_login(login: &str) -> Result<String, AuthError> {
    if !(LOGIN_MIN..=LOGIN_MAX).contains(&login.len()) || !login.is_ascii() {
        return Err(AuthError::BadLogin);
    }
    let normalized = login.to_ascii_lowercase();
    if !valid_login(&normalized) {
        return Err(AuthError::BadLogin);
    }
    Ok(normalized)
}

fn valid_password(password: &str) -> bool {
    (PASSWORD_MIN..=PASSWORD_MAX).contains(&password.len())
        && !password.chars().any(char::is_control)
}

fn parse_credentials(payload: &[u8]) -> Result<(Credentials<'_>, &[u8]), AuthError> {
    if payload.len() > AUTH_PAYLOAD_MAX {
        return Err(AuthError::Oversize);
    }
    let (&login_len, rest) = payload.split_first().ok_or(AuthError::Truncated)?;
    let login_len = usize::from(login_len);
    if !(LOGIN_MIN..=LOGIN_MAX).contains(&login_len) {
        return Err(AuthError::BadLogin);
    }
    let login_bytes = rest.get(..login_len).ok_or(AuthError::Truncated)?;
    let login = std::str::from_utf8(login_bytes).map_err(|_| AuthError::BadLogin)?;
    if !valid_login(login) {
        return Err(AuthError::BadLogin);
    }
    let rest = &rest[login_len..];
    let password_len = usize::from(u16::from_be_bytes(
        rest.get(..2)
            .ok_or(AuthError::Truncated)?
            .try_into()
            .map_err(|_| AuthError::Truncated)?,
    ));
    if !(PASSWORD_MIN..=PASSWORD_MAX).contains(&password_len) {
        return Err(AuthError::BadPassword);
    }
    let rest = &rest[2..];
    let password_bytes = rest.get(..password_len).ok_or(AuthError::Truncated)?;
    let password = std::str::from_utf8(password_bytes).map_err(|_| AuthError::BadPassword)?;
    if !valid_password(password) {
        return Err(AuthError::BadPassword);
    }
    Ok((Credentials { login, password }, &rest[password_len..]))
}

fn parse_optional(payload: &[u8]) -> Result<Option<&[u8; 32]>, AuthError> {
    let (&flag, rest) = payload.split_first().ok_or(AuthError::Truncated)?;
    match flag {
        0 if rest.is_empty() => Ok(None),
        0 => Err(AuthError::TrailingData),
        1 if rest.len() < 32 => Err(AuthError::Truncated),
        1 if rest.len() > 32 => Err(AuthError::TrailingData),
        1 => Ok(Some(rest.try_into().map_err(|_| AuthError::Truncated)?)),
        _ => Err(AuthError::BadFlag),
    }
}

pub fn parse_signup(payload: &[u8]) -> Result<Signup<'_>, AuthError> {
    let (credentials, rest) = parse_credentials(payload)?;
    Ok(Signup {
        credentials,
        invitation: parse_optional(rest)?,
    })
}

pub fn parse_login(payload: &[u8]) -> Result<Login<'_>, AuthError> {
    let (credentials, rest) = parse_credentials(payload)?;
    Ok(Login {
        credentials,
        replace: parse_optional(rest)?,
    })
}

fn build_credentials(
    login: &str,
    password: &str,
    optional: Option<&[u8; 32]>,
) -> Result<Vec<u8>, AuthError> {
    let login = normalize_login(login)?;
    if !valid_password(password) {
        return Err(AuthError::BadPassword);
    }
    let login_len = u8::try_from(login.len()).map_err(|_| AuthError::BadLogin)?;
    let password_len = u16::try_from(password.len()).map_err(|_| AuthError::BadPassword)?;
    let mut payload =
        Vec::with_capacity(1 + login.len() + 2 + password.len() + 1 + optional.map_or(0, |_| 32));
    payload.push(login_len);
    payload.extend_from_slice(login.as_bytes());
    payload.extend_from_slice(&password_len.to_be_bytes());
    payload.extend_from_slice(password.as_bytes());
    match optional {
        None => payload.push(0),
        Some(value) => {
            payload.push(1);
            payload.extend_from_slice(value);
        }
    }
    Ok(payload)
}

pub fn build_signup(
    login: &str,
    password: &str,
    invitation: Option<&[u8; 32]>,
) -> Result<Vec<u8>, AuthError> {
    build_credentials(login, password, invitation)
}

pub fn build_login(
    login: &str,
    password: &str,
    replace: Option<&[u8; 32]>,
) -> Result<Vec<u8>, AuthError> {
    build_credentials(login, password, replace)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Authenticated {
    pub user_id: [u8; 16],
    pub contact_id: String,
}

fn valid_contact_id(contact_id: &[u8]) -> bool {
    contact_id.len() == CONTACT_ID_LEN && contact_id.iter().all(|b| CONTACT_ALPHABET.contains(b))
}

fn check_exact_len(payload: &[u8], len: usize) -> Result<(), AuthError> {
    if payload.len() < len {
        return Err(AuthError::Truncated);
    }
    if payload.len() > len {
        return Err(AuthError::TrailingData);
    }
    Ok(())
}

pub fn parse_authenticated(payload: &[u8]) -> Result<Authenticated, AuthError> {
    check_exact_len(payload, AUTHENTICATED_LEN)?;
    let user_id = payload[..16].try_into().map_err(|_| AuthError::Truncated)?;
    let contact_id = &payload[16..];
    if !valid_contact_id(contact_id) {
        return Err(AuthError::BadContactId);
    }
    let contact_id = std::str::from_utf8(contact_id)
        .map_err(|_| AuthError::BadContactId)?
        .to_owned();
    Ok(Authenticated {
        user_id,
        contact_id,
    })
}

pub fn build_authenticated(user_id: &[u8; 16], contact_id: &str) -> Result<Vec<u8>, AuthError> {
    if !valid_contact_id(contact_id.as_bytes()) {
        return Err(AuthError::BadContactId);
    }
    let mut payload = Vec::with_capacity(AUTHENTICATED_LEN);
    payload.extend_from_slice(user_id);
    payload.extend_from_slice(contact_id.as_bytes());
    Ok(payload)
}

/// A secret invitation is a standalone canonical 43-character base64url string.
/// It is never a URI or part of a public server profile.
pub fn build_invitation(invitation: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(invitation)
}

pub fn parse_invitation(invitation: &str) -> Result<[u8; 32], AuthError> {
    if invitation.len() != 43
        || !invitation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(AuthError::BadInvitation);
    }
    let mut decoded = [0; 32];
    let len = URL_SAFE_NO_PAD
        .decode_slice(invitation, &mut decoded)
        .map_err(|_| AuthError::BadInvitation)?;
    if len != decoded.len() {
        return Err(AuthError::BadInvitation);
    }
    Ok(decoded)
}

/// Authenticated peer-binding response. It does not establish local peer trust;
/// the core must pin/confirm identity changes before consuming peer messages.
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceBinding {
    pub user_id: [u8; 16],
    pub device_key: [u8; 32],
    pub ed25519: [u8; 32],
    pub curve25519: [u8; 32],
}

pub fn build_binding(
    user_id: &[u8; 16],
    device_key: &[u8; 32],
    ed25519: &[u8; 32],
    curve25519: &[u8; 32],
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(BINDING_LEN);
    payload.extend_from_slice(user_id);
    payload.extend_from_slice(device_key);
    payload.extend_from_slice(ed25519);
    payload.extend_from_slice(curve25519);
    payload
}

pub fn parse_binding(payload: &[u8]) -> Result<DeviceBinding, AuthError> {
    check_exact_len(payload, BINDING_LEN)?;
    Ok(DeviceBinding {
        user_id: payload[..16].try_into().map_err(|_| AuthError::Truncated)?,
        device_key: payload[16..48]
            .try_into()
            .map_err(|_| AuthError::Truncated)?,
        ed25519: payload[48..80]
            .try_into()
            .map_err(|_| AuthError::Truncated)?,
        curve25519: payload[80..112]
            .try_into()
            .map_err(|_| AuthError::Truncated)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_credentials(login: &[u8], password: &[u8], optional: &[u8]) -> Vec<u8> {
        let mut payload = vec![u8::try_from(login.len()).unwrap()];
        payload.extend_from_slice(login);
        payload.extend_from_slice(&u16::try_from(password.len()).unwrap().to_be_bytes());
        payload.extend_from_slice(password);
        payload.extend_from_slice(optional);
        payload
    }

    #[test]
    fn registration_mode_exact_vectors() {
        for mode in [RegistrationMode::InviteOnly, RegistrationMode::Open] {
            assert_eq!(RegistrationMode::parse(&mode.encode()), Some(mode));
        }
        assert_eq!(RegistrationMode::InviteOnly.encode(), [0]);
        assert_eq!(RegistrationMode::Open.encode(), [1]);
        for bad in [&[][..], &[2][..], &[255][..], &[0, 0][..], &[1, 1][..]] {
            assert_eq!(RegistrationMode::parse(bad), None);
        }
    }

    #[test]
    fn signup_and_login_exact_vectors_and_borrowed_secrets() {
        let password = "  Pass wörd  ";
        let optional = [0xAB; 32];
        for value in [None, Some(&optional)] {
            let expected = raw_credentials(
                b"alice_.-1",
                password.as_bytes(),
                &value.map_or_else(|| vec![0], |v| [&[1][..], &v[..]].concat()),
            );
            let payload = build_signup("ALIce_.-1", password, value).unwrap();
            assert_eq!(payload, expected);
            let signup = parse_signup(&payload).unwrap();
            assert_eq!(signup.credentials.login, "alice_.-1");
            assert_eq!(signup.credentials.password, password);
            assert_eq!(signup.credentials.password.as_ptr(), payload[12..].as_ptr());
            assert_eq!(signup.invitation, value);
            let login_payload = build_login("ALIce_.-1", password, value).unwrap();
            assert_eq!(login_payload, expected);
            let login = parse_login(&login_payload).unwrap();
            assert_eq!(login.credentials.login, "alice_.-1");
            assert_eq!(login.credentials.password, password);
            assert_eq!(login.replace, value);
        }
        assert_eq!(
            build_login("abc", "12345678", None).unwrap(),
            b"\x03abc\x00\x0812345678\x00"
        );
    }

    #[test]
    fn login_grammar_normalization_and_wire_canonicality() {
        assert_eq!(normalize_login("ABC._-012"), Ok("abc._-012".to_owned()));
        for bad in [
            "", "ab", "a b", " abc", "abc ", "a/b", "a@b", "äbc", "a\0b", "a\nb",
        ] {
            assert_eq!(normalize_login(bad), Err(AuthError::BadLogin));
            assert_eq!(
                build_signup(bad, "12345678", None),
                Err(AuthError::BadLogin)
            );
            assert_eq!(
                parse_signup(&raw_credentials(bad.as_bytes(), b"12345678", &[0])).err(),
                Some(AuthError::BadLogin)
            );
        }
        for noncanonical in ["ABC", "aBc"] {
            let payload = raw_credentials(noncanonical.as_bytes(), b"12345678", &[0]);
            assert_eq!(parse_login(&payload).err(), Some(AuthError::BadLogin));
        }
        assert!(normalize_login(&"A".repeat(LOGIN_MAX)).is_ok());
        assert_eq!(
            normalize_login(&"a".repeat(LOGIN_MAX + 1)),
            Err(AuthError::BadLogin)
        );
        let invalid_utf8 = raw_credentials(&[b'a', 0xFF, b'b'], b"12345678", &[0]);
        assert_eq!(parse_login(&invalid_utf8).err(), Some(AuthError::BadLogin));
    }

    #[test]
    fn password_byte_limits_utf8_controls_and_no_normalization() {
        for good in [
            "12345678".to_owned(),
            "x".repeat(PASSWORD_MAX),
            "é".repeat(64),
            "        ".to_owned(),
            " éPass  ".to_owned(),
        ] {
            let payload = build_login("abc", &good, None).unwrap();
            assert_eq!(parse_login(&payload).unwrap().credentials.password, good);
        }
        for bad in [
            "1234567".to_owned(),
            "x".repeat(PASSWORD_MAX + 1),
            "é".repeat(65),
            "1234\0abc".to_owned(),
            "1234\tabc".to_owned(),
            "1234\nabc".to_owned(),
            "1234\u{7f}abc".to_owned(),
            "1234\u{85}abc".to_owned(),
        ] {
            assert_eq!(build_login("abc", &bad, None), Err(AuthError::BadPassword));
            assert_eq!(
                parse_login(&raw_credentials(b"abc", bad.as_bytes(), &[0])).err(),
                Some(AuthError::BadPassword)
            );
        }
        assert_eq!(
            parse_signup(&raw_credentials(b"abc", &[0xFF; 8], &[0])).err(),
            Some(AuthError::BadPassword)
        );
        let decomposed = "1234567e\u{301}";
        let payload = build_login("abc", decomposed, None).unwrap();
        assert_eq!(
            parse_login(&payload).unwrap().credentials.password,
            decomposed
        );
    }

    #[test]
    fn optional_flag_exact_consumption_and_truncations() {
        let optional = [7; 32];
        for value in [None, Some(&optional)] {
            let payload = build_signup("abc", "12345678", value).unwrap();
            for end in 0..payload.len() {
                assert!(
                    parse_signup(&payload[..end]).is_err(),
                    "signup prefix {end}"
                );
                assert!(parse_login(&payload[..end]).is_err(), "login prefix {end}");
            }
            let mut trailing = payload.clone();
            trailing.push(0);
            assert_eq!(parse_signup(&trailing).err(), Some(AuthError::TrailingData));
            assert_eq!(parse_login(&trailing).err(), Some(AuthError::TrailingData));
        }
        for flag in [2, 255] {
            let payload = raw_credentials(b"abc", b"12345678", &[flag]);
            assert_eq!(parse_signup(&payload).err(), Some(AuthError::BadFlag));
            assert_eq!(parse_login(&payload).err(), Some(AuthError::BadFlag));
        }
        let payload = build_login(
            &"a".repeat(LOGIN_MAX),
            &"x".repeat(PASSWORD_MAX),
            Some(&optional),
        )
        .unwrap();
        assert_eq!(payload.len(), AUTH_PAYLOAD_MAX);
        assert!(parse_login(&payload).is_ok());
        let mut oversized = payload;
        oversized.push(0);
        assert_eq!(parse_login(&oversized).err(), Some(AuthError::Oversize));
        let huge_login = [255];
        assert_eq!(parse_login(&huge_login).err(), Some(AuthError::BadLogin));
        let huge_password = b"\x03abc\xff\xff";
        assert_eq!(
            parse_signup(huge_password).err(),
            Some(AuthError::BadPassword)
        );
    }

    #[test]
    fn authenticated_exact_shape_and_contact_alphabet() {
        let user_id = [0xAB; 16];
        let contact_id = "7K3MP9TX4V2N";
        let payload = build_authenticated(&user_id, contact_id).unwrap();
        assert_eq!(payload.len(), 28);
        assert_eq!(&payload[..16], &user_id);
        assert_eq!(&payload[16..], contact_id.as_bytes());
        assert_eq!(
            parse_authenticated(&payload).unwrap(),
            Authenticated {
                user_id,
                contact_id: contact_id.to_owned()
            }
        );
        for end in 0..payload.len() {
            assert_eq!(
                parse_authenticated(&payload[..end]),
                Err(AuthError::Truncated)
            );
        }
        let mut trailing = payload;
        trailing.push(0);
        assert_eq!(parse_authenticated(&trailing), Err(AuthError::TrailingData));
        for bad in [
            "7k3mp9tx4v2n",
            "7K3M-P9TX-4V2N",
            "ABCDEFGHIJKL",
            "0123456789OU",
            "0123456789Z",
            "0123456789ZZZ",
        ] {
            assert_eq!(
                build_authenticated(&user_id, bad),
                Err(AuthError::BadContactId)
            );
        }
        for invalid in [b'I', b'L', b'O', b'U', b'a', b'-', 0, 0xFF] {
            let mut payload = vec![0; 16];
            payload.extend_from_slice(b"0123456789AZ");
            payload[16] = invalid;
            assert_eq!(parse_authenticated(&payload), Err(AuthError::BadContactId));
        }
        for &valid in CONTACT_ALPHABET {
            let contact = String::from_utf8(vec![valid; 12]).unwrap();
            assert!(build_authenticated(&user_id, &contact).is_ok());
        }
    }

    #[test]
    fn invitation_strict_base64_no_uri_or_padding() {
        for secret in [[0; 32], [0xFF; 32], [7; 32]] {
            let invitation = build_invitation(&secret);
            assert_eq!(invitation.len(), 43);
            assert_eq!(parse_invitation(&invitation), Ok(secret));
            assert_eq!(
                parse_invitation(&format!("{invitation}=")),
                Err(AuthError::BadInvitation)
            );
            assert_eq!(
                parse_invitation(&invitation[..42]),
                Err(AuthError::BadInvitation)
            );
            assert_eq!(
                parse_invitation(&format!("dmsg://join/{invitation}")),
                Err(AuthError::BadInvitation)
            );
        }
        assert_eq!(
            parse_invitation(&"A".repeat(10_000)),
            Err(AuthError::BadInvitation)
        );
        assert_eq!(
            parse_invitation(&format!("{}!", "A".repeat(42))),
            Err(AuthError::BadInvitation)
        );
        assert_eq!(
            parse_invitation(&format!("{}B", "A".repeat(42))),
            Err(AuthError::BadInvitation)
        );
        assert_eq!(
            parse_invitation(&format!("{}\n", "A".repeat(42))),
            Err(AuthError::BadInvitation)
        );
    }

    #[test]
    fn binding_exact_layout_and_length() {
        let payload = build_binding(&[0x11; 16], &[0x22; 32], &[0x33; 32], &[0x44; 32]);
        assert_eq!(payload.len(), 112);
        assert_eq!(
            parse_binding(&payload).unwrap(),
            DeviceBinding {
                user_id: [0x11; 16],
                device_key: [0x22; 32],
                ed25519: [0x33; 32],
                curve25519: [0x44; 32],
            }
        );
        for end in 0..payload.len() {
            assert_eq!(parse_binding(&payload[..end]), Err(AuthError::Truncated));
        }
        let mut trailing = payload;
        trailing.push(0);
        assert_eq!(parse_binding(&trailing), Err(AuthError::TrailingData));
    }

    #[test]
    fn arbitrary_bounded_payloads_do_not_panic() {
        // Exhaustive single-byte prefixes and adversarial declared lengths.
        for byte in 0..=u8::MAX {
            for len in [
                0,
                1,
                2,
                3,
                4,
                8,
                16,
                28,
                32,
                112,
                AUTH_PAYLOAD_MAX,
                AUTH_PAYLOAD_MAX + 1,
            ] {
                let payload = vec![byte; len];
                let _ = parse_signup(&payload);
                let _ = parse_login(&payload);
                let _ = parse_authenticated(&payload);
                let _ = parse_binding(&payload);
            }
        }
    }
}

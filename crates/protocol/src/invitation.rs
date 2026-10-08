//! Single-secret self-service invitations. QR contains the existing raw43 token.
use crate::auth::{self, AuthError};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_ACTIVE: usize = 8;
pub const TTL_SECONDS: i64 = 86400;
pub const MAX_INPUT_BYTES: usize = 256;
pub const WORD_COUNT: usize = 7776;
const WORDS: &[&str; WORD_COUNT] = &diceware_wordlists::EFF_LONG_WORDLIST;

pub fn word(index: usize) -> Option<&'static str> {
    WORDS.get(index).copied()
}

/// Bound before allocation; lowercase Latin words and ASCII whitespace only.
pub fn canonical_phrase(input: &str) -> Result<String, AuthError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(AuthError::Oversize);
    }
    if !input
        .bytes()
        .all(|b| b.is_ascii_alphabetic() || b == b'-' || b.is_ascii_whitespace())
    {
        return Err(AuthError::BadInvitation);
    }
    let normalized = Zeroizing::new(input.to_ascii_lowercase());
    let mut words = normalized.split_ascii_whitespace();
    let mut selected = [""; 6];
    for w in &mut selected {
        *w = words.next().ok_or(AuthError::BadInvitation)?;
        if WORDS.binary_search(w).is_err() {
            return Err(AuthError::BadInvitation);
        }
    }
    if words.next().is_some() {
        return Err(AuthError::BadInvitation);
    }
    Ok(selected.join(" "))
}

pub fn parse_invitation_input(input: &str) -> Result<[u8; 32], AuthError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(AuthError::Oversize);
    }
    if let Ok(token) = auth::parse_invitation(input) {
        return Ok(token);
    }
    let phrase = Zeroizing::new(canonical_phrase(input)?);
    let mut hash = Sha256::new();
    hash.update(b"dmsg signup phrase v1\0");
    hash.update(phrase.as_bytes());
    Ok(hash.finalize().into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Active = 0,
    Used = 1,
    Revoked = 2,
    Expired = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub issue_id: [u8; 16],
    pub created_at: i64,
    pub expires_at: i64,
}

pub struct Issued {
    pub server_now: i64,
    pub invitation: Metadata,
    pub state: State,
    pub phrase: Option<String>,
}

impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issued")
            .field("server_now", &self.server_now)
            .field("invitation", &self.invitation)
            .field("state", &self.state)
            .field("phrase", &self.phrase.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

// Callers can guard the complete owned response without preventing field moves.
impl Zeroize for Issued {
    fn zeroize(&mut self) {
        if let Some(phrase) = &mut self.phrase {
            phrase.zeroize();
        }
        self.phrase = None;
    }
}

#[derive(Debug)]
pub struct List {
    pub server_now: i64,
    pub invitations: Vec<Metadata>,
}

pub fn parse_issue_id(payload: &[u8]) -> Result<[u8; 16], AuthError> {
    payload.try_into().map_err(|_| {
        if payload.len() < 16 {
            AuthError::Truncated
        } else {
            AuthError::TrailingData
        }
    })
}

fn validate_metadata(m: &Metadata, now: i64) -> Result<(), AuthError> {
    if now < 0
        || m.created_at < 0
        || m.created_at > now
        || m.created_at.checked_add(TTL_SECONDS) != Some(m.expires_at)
    {
        return Err(AuthError::BadInvitation);
    }
    Ok(())
}

fn validate_issued(value: &Issued) -> Result<(), AuthError> {
    validate_metadata(&value.invitation, value.server_now)?;
    match (value.state, value.phrase.as_deref()) {
        (State::Active, Some(phrase)) if value.invitation.expires_at > value.server_now => {
            let canonical = Zeroizing::new(canonical_phrase(phrase)?);
            if canonical.as_str() != phrase {
                return Err(AuthError::BadInvitation);
            }
        }
        (State::Expired, None) if value.invitation.expires_at <= value.server_now => {}
        (State::Used | State::Revoked, None) => {}
        _ => return Err(AuthError::BadInvitation),
    }
    Ok(())
}

fn append_metadata(out: &mut Vec<u8>, m: &Metadata) {
    out.extend_from_slice(&m.issue_id);
    out.extend_from_slice(&m.created_at.to_be_bytes());
    out.extend_from_slice(&m.expires_at.to_be_bytes());
}

fn i64_at(payload: &[u8], offset: usize) -> Result<i64, AuthError> {
    Ok(i64::from_be_bytes(
        payload
            .get(offset..offset + 8)
            .ok_or(AuthError::Truncated)?
            .try_into()
            .map_err(|_| AuthError::Truncated)?,
    ))
}

fn metadata_at(payload: &[u8], offset: usize) -> Result<Metadata, AuthError> {
    Ok(Metadata {
        issue_id: parse_issue_id(
            payload
                .get(offset..offset + 16)
                .ok_or(AuthError::Truncated)?,
        )?,
        created_at: i64_at(payload, offset + 16)?,
        expires_at: i64_at(payload, offset + 24)?,
    })
}

pub fn build_issued(value: &Issued) -> Result<Vec<u8>, AuthError> {
    validate_issued(value)?;
    let mut out = Vec::with_capacity(42 + value.phrase.as_ref().map_or(0, String::len));
    out.extend_from_slice(&value.server_now.to_be_bytes());
    append_metadata(&mut out, &value.invitation);
    out.push(value.state as u8);
    if let Some(phrase) = &value.phrase {
        out.push(u8::try_from(phrase.len()).map_err(|_| AuthError::Oversize)?);
        out.extend_from_slice(phrase.as_bytes());
    }
    Ok(out)
}

pub fn parse_issued(payload: &[u8]) -> Result<Issued, AuthError> {
    if payload.len() > 42 + 255 {
        return Err(AuthError::Oversize);
    }
    let server_now = i64_at(payload, 0)?;
    let invitation = metadata_at(payload, 8)?;
    let state = match payload.get(40).ok_or(AuthError::Truncated)? {
        0 => State::Active,
        1 => State::Used,
        2 => State::Revoked,
        3 => State::Expired,
        _ => return Err(AuthError::BadFlag),
    };
    let phrase = if state == State::Active {
        let len = usize::from(*payload.get(41).ok_or(AuthError::Truncated)?);
        if payload.len() < 42 + len {
            return Err(AuthError::Truncated);
        }
        if payload.len() != 42 + len {
            return Err(AuthError::TrailingData);
        }
        Some(
            std::str::from_utf8(&payload[42..])
                .map_err(|_| AuthError::BadInvitation)?
                .to_owned(),
        )
    } else {
        if payload.len() != 41 {
            return Err(AuthError::TrailingData);
        }
        None
    };
    let mut value = Issued {
        server_now,
        invitation,
        state,
        phrase,
    };
    if let Err(error) = validate_issued(&value) {
        value.zeroize();
        return Err(error);
    }
    Ok(value)
}

fn validate_list(value: &List) -> Result<(), AuthError> {
    if value.server_now < 0 || value.invitations.len() > MAX_ACTIVE {
        return Err(AuthError::BadInvitation);
    }
    for (i, m) in value.invitations.iter().enumerate() {
        validate_metadata(m, value.server_now)?;
        if m.expires_at <= value.server_now
            || value.invitations[..i]
                .iter()
                .any(|other| other.issue_id == m.issue_id)
        {
            return Err(AuthError::BadInvitation);
        }
    }
    Ok(())
}

pub fn build_list(value: &List) -> Result<Vec<u8>, AuthError> {
    validate_list(value)?;
    let mut out = Vec::with_capacity(9 + value.invitations.len() * 32);
    out.extend_from_slice(&value.server_now.to_be_bytes());
    out.push(value.invitations.len() as u8);
    for m in &value.invitations {
        append_metadata(&mut out, m);
    }
    Ok(out)
}

pub fn parse_list(payload: &[u8]) -> Result<List, AuthError> {
    let server_now = i64_at(payload, 0)?;
    let count = usize::from(*payload.get(8).ok_or(AuthError::Truncated)?);
    if count > MAX_ACTIVE {
        return Err(AuthError::Oversize);
    }
    if payload.len() < 9 + count * 32 {
        return Err(AuthError::Truncated);
    }
    if payload.len() != 9 + count * 32 {
        return Err(AuthError::TrailingData);
    }
    let mut invitations = Vec::with_capacity(count);
    for i in 0..count {
        invitations.push(metadata_at(payload, 9 + i * 32)?);
    }
    let value = List {
        server_now,
        invitations,
    };
    validate_list(&value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    const PHRASE: &str = "panoramic nectar precut smith banana handclap";

    #[test]
    fn pinned_dictionary_full_source_fingerprint_and_all_words_parse() {
        assert_eq!(WORDS.len(), 7776);
        assert_eq!(word(0), Some("abacus"));
        assert_eq!(word(7775), Some("zoom"));
        assert_eq!(word(7776), None);
        let mut fingerprint = Sha256::new();
        for (i, w) in WORDS.iter().enumerate() {
            assert!(w.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'));
            if i > 0 {
                assert!(WORDS[i - 1] < *w);
            }
            fingerprint.update(w.as_bytes());
            fingerprint.update(b"\n");
            let repeated = [*w; 6].join(" ");
            assert_eq!(canonical_phrase(&repeated).unwrap(), repeated);
        }
        assert_eq!(
            format!("{:x}", fingerprint.finalize()),
            "6d557f0693958fb5e650b68b5bee585eb82cf4da32965505c789e924743bc522"
        );
    }

    #[test]
    fn phrase_raw43_equivalence_domain_separation_and_strict_grammar() {
        let expected = "hYxoPRNle8nngeRVVXTPgr6XMXaNSAIqZMFsTqyyri8";
        let token = parse_invitation_input(PHRASE).unwrap();
        assert_eq!(auth::build_invitation(&token), expected);
        assert_eq!(parse_invitation_input(expected).unwrap(), token);
        assert_eq!(
            parse_invitation_input("\tpanoramic  nectar\nprecut\rsmith\x0cbanana handclap ")
                .unwrap(),
            token
        );
        assert_ne!(token, <[u8; 32]>::from(Sha256::digest(PHRASE.as_bytes())));
        assert_eq!(
            parse_invitation_input(&PHRASE.to_ascii_uppercase()).unwrap(),
            token
        );
        for bad in [
            "panoramic nectar precut smith banana",
            "panoramic nectar precut smith banana handclap zoom",
            "panoramic nectar precut smith banana handclapp",
            "panoramic\u{a0}nectar precut smith banana handclap",
            "панорама nectar precut smith banana handclap",
            "dmsg://join/abacus abacus abacus abacus abacus abacus",
            "abacus, abacus abacus abacus abacus abacus",
            "abacus\0 abacus abacus abacus abacus abacus",
        ] {
            assert!(parse_invitation_input(bad).is_err());
        }
        let bounded = format!("{}{}", " ".repeat(256 - PHRASE.len()), PHRASE);
        assert_eq!(parse_invitation_input(&bounded).unwrap(), token);
        assert_eq!(
            parse_invitation_input(&(bounded + " ")),
            Err(AuthError::Oversize)
        );
        for token in [[0; 32], [255; 32], [7; 32]] {
            assert_eq!(
                parse_invitation_input(&auth::build_invitation(&token)),
                Ok(token)
            );
        }
    }

    fn fixture(state: State) -> Issued {
        Issued {
            server_now: if state == State::Expired { 86401 } else { 2 },
            invitation: Metadata {
                issue_id: [9; 16],
                created_at: 1,
                expires_at: 86401,
            },
            state,
            phrase: if state == State::Active {
                Some(PHRASE.into())
            } else {
                None
            },
        }
    }

    #[test]
    fn exact_issued_vectors_state_secrets_timestamps_and_consumption() {
        assert_eq!(
            [
                crate::OP_INVITE_ISSUE,
                crate::OP_INVITE_ISSUED,
                crate::OP_INVITE_REVOKE,
                crate::OP_INVITE_REVOKED,
                crate::OP_INVITE_LIST,
                crate::OP_INVITE_LISTED
            ],
            [46, 47, 48, 49, 50, 51]
        );
        assert_eq!(crate::ERR_INVITE_LIMIT, 14);
        for state in [State::Active, State::Used, State::Revoked, State::Expired] {
            let f = fixture(state);
            let bytes = build_issued(&f).unwrap();
            assert_eq!(
                bytes.len(),
                if state == State::Active {
                    42 + PHRASE.len()
                } else {
                    41
                }
            );
            assert_eq!(&bytes[..8], &f.server_now.to_be_bytes());
            assert_eq!(&bytes[8..24], &[9; 16]);
            assert_eq!(&bytes[24..32], &1i64.to_be_bytes());
            assert_eq!(&bytes[32..40], &86401i64.to_be_bytes());
            assert_eq!(bytes[40], state as u8);
            let parsed = parse_issued(&bytes).unwrap();
            assert_eq!(parsed.invitation, f.invitation);
            assert_eq!(parsed.state, state);
            assert_eq!(parsed.phrase, f.phrase);
            assert!(!format!("{parsed:?}").contains(PHRASE));
            for end in 0..bytes.len() {
                assert!(parse_issued(&bytes[..end]).is_err());
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(parse_issued(&trailing).is_err());
            let mut invalid = bytes;
            invalid[40] = 4;
            assert!(parse_issued(&invalid).is_err());
        }
        let mut f = fixture(State::Active);
        f.server_now = f.invitation.expires_at;
        assert!(build_issued(&f).is_err());
        f = fixture(State::Expired);
        f.server_now = 2;
        assert!(build_issued(&f).is_err());
        f = fixture(State::Used);
        f.phrase = Some(PHRASE.into());
        assert!(build_issued(&f).is_err());
        f = fixture(State::Active);
        f.invitation.expires_at -= 1;
        assert!(build_issued(&f).is_err());
        f = fixture(State::Active);
        f.invitation.created_at = 3;
        assert!(build_issued(&f).is_err());
        f = fixture(State::Active);
        f.phrase = Some(format!(" {PHRASE}"));
        assert!(build_issued(&f).is_err());
    }

    #[test]
    fn list_is_bounded_active_metadata_only_exact_and_unique() {
        for count in 0..=MAX_ACTIVE {
            let invitations = (0..count)
                .map(|i| Metadata {
                    issue_id: [i as u8; 16],
                    created_at: 1,
                    expires_at: 86401,
                })
                .collect();
            let l = List {
                server_now: 2,
                invitations,
            };
            let bytes = build_list(&l).unwrap();
            assert_eq!(bytes.len(), 9 + 32 * count);
            assert_eq!(parse_list(&bytes).unwrap().invitations, l.invitations);
            for end in 0..bytes.len() {
                assert!(parse_list(&bytes[..end]).is_err());
            }
            let mut trailing = bytes;
            trailing.push(0);
            assert!(parse_list(&trailing).is_err());
        }
        let m = fixture(State::Active).invitation;
        assert!(build_list(&List {
            server_now: 2,
            invitations: vec![m, m]
        })
        .is_err());
        assert!(build_list(&List {
            server_now: 86401,
            invitations: vec![m]
        })
        .is_err());
        assert!(build_list(&List {
            server_now: 2,
            invitations: vec![m; 9]
        })
        .is_err());
        let mut bytes = build_list(&List {
            server_now: 2,
            invitations: vec![m],
        })
        .unwrap();
        bytes[8] = 2;
        bytes.extend_from_slice(&bytes[9..41].to_vec());
        assert!(parse_list(&bytes).is_err());
        bytes[8] = 9;
        assert!(parse_list(&bytes).is_err());
        assert!(parse_issue_id(&[0; 15]).is_err());
        assert!(parse_issue_id(&[0; 17]).is_err());
        assert_eq!(parse_issue_id(&[0; 16]).unwrap(), [0; 16]);
    }

    #[test]
    fn arbitrary_payloads_do_not_panic() {
        for b in 0..=255 {
            for len in [0, 1, 8, 9, 16, 40, 41, 42, 100, 265, 297, 298] {
                let bytes = vec![b; len];
                let _ = parse_issued(&bytes);
                let _ = parse_list(&bytes);
            }
        }
    }
}

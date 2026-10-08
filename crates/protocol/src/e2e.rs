//! Strict E2E plaintext v1: `[version:u8][kind:u8][message_id:16][sender_ed:32][body]`.
//! Text is the remaining UTF-8 bytes; controls start with target16 + revision8 BE.

use crate::{CIPHERTEXT_MAX, TEXT_CHAR_MAX, TEXT_UTF8_MAX};

const VERSION: u8 = 1;
const HEADER_LEN: usize = 1 + 1 + 16 + 32;
const CONTROL_LEN: usize = 16 + 8;
const ENVELOPE_MAX: usize = HEADER_LEN + CONTROL_LEN + TEXT_UTF8_MAX;

pub const VOICE_MANIFEST_LEN: usize = 166;
pub const VOICE_SAMPLE_MAX: u32 = 960_000;
/// Version1/profile1 = fixed mono16k,20ms,Opus10k speech container.
/// Keys are E2E-only and must never be sent to msgd outside Olm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceManifest {
    pub blob_id: [u8; 16],
    pub key: [u8; 32],
    pub nonce_prefix: [u8; 8],
    pub recipient_binding: [u8; 32],
    pub plain_len: u32,
    pub byte_len: u32,
    pub sample_count: u32,
    pub waveform: Vec<u8>,
}

impl VoiceManifest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sample_count == 0
            || self.sample_count > VOICE_SAMPLE_MAX
            || self.waveform.len() != 64
        {
            return Err("invalid voice samples/waveform");
        }
        if self.plain_len == 0 || self.byte_len as usize > crate::blob::VOICE_MAX {
            return Err("invalid voice length");
        }
        let count = self.plain_len.div_ceil(crate::blob::CHUNK_PLAIN_MAX as u32);
        if self
            .plain_len
            .checked_add(count.checked_mul(16).ok_or("invalid voice geometry")?)
            != Some(self.byte_len)
        {
            return Err("invalid voice geometry");
        }
        if crate::blob::chunk_count(self.byte_len) != u16::try_from(count).ok() {
            return Err("invalid voice geometry");
        }
        Ok(())
    }
    pub fn chunk_count(&self) -> Result<u16, &'static str> {
        self.validate()?;
        crate::blob::chunk_count(self.byte_len).ok_or("invalid voice geometry")
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let mut out = Vec::with_capacity(VOICE_MANIFEST_LEN);
        out.extend_from_slice(&[1, 1]);
        out.extend_from_slice(&self.blob_id);
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.nonce_prefix);
        out.extend_from_slice(&self.recipient_binding);
        out.extend_from_slice(&self.plain_len.to_be_bytes());
        out.extend_from_slice(&self.byte_len.to_be_bytes());
        out.extend_from_slice(&self.sample_count.to_be_bytes());
        out.extend_from_slice(&self.waveform);
        Ok(out)
    }
    pub fn decode(p: &[u8]) -> Result<Self, &'static str> {
        if p.len() != VOICE_MANIFEST_LEN || p[..2] != [1, 1] {
            return Err("invalid voice manifest version/profile/length");
        }
        let m = Self {
            blob_id: p[2..18].try_into().map_err(|_| "invalid voice manifest")?,
            key: p[18..50].try_into().map_err(|_| "invalid voice manifest")?,
            nonce_prefix: p[50..58].try_into().map_err(|_| "invalid voice manifest")?,
            recipient_binding: p[58..90].try_into().map_err(|_| "invalid voice manifest")?,
            plain_len: u32::from_be_bytes(
                p[90..94].try_into().map_err(|_| "invalid voice manifest")?,
            ),
            byte_len: u32::from_be_bytes(
                p[94..98].try_into().map_err(|_| "invalid voice manifest")?,
            ),
            sample_count: u32::from_be_bytes(
                p[98..102]
                    .try_into()
                    .map_err(|_| "invalid voice manifest")?,
            ),
            waveform: p[102..].to_vec(),
        };
        m.validate()?;
        Ok(m)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Text = 1,
    Edit = 2,
    Delete = 3,
    Voice = 4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub message_id: [u8; 16],
    pub sender_ed: [u8; 32],
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Text(String),
    Voice(VoiceManifest),
    Edit {
        target: [u8; 16],
        revision: u64,
        text: String,
    },
    Delete {
        target: [u8; 16],
        revision: u64,
    },
}

impl Event {
    pub fn kind(&self) -> Kind {
        match &self.body {
            Body::Text(_) => Kind::Text,
            Body::Voice(_) => Kind::Voice,
            Body::Edit { .. } => Kind::Edit,
            Body::Delete { .. } => Kind::Delete,
        }
    }
}

/// Validate the complete event before allocating its exact wire envelope.
pub fn encode(event: &Event) -> Result<Vec<u8>, &'static str> {
    let body_len = match &event.body {
        Body::Text(text) => {
            validate_text(text)?;
            text.len()
        }
        Body::Voice(manifest) => {
            manifest.validate()?;
            VOICE_MANIFEST_LEN
        }
        Body::Edit { revision, text, .. } => {
            validate_revision(*revision)?;
            validate_text(text)?;
            CONTROL_LEN + text.len()
        }
        Body::Delete { revision, .. } => {
            validate_revision(*revision)?;
            CONTROL_LEN
        }
    };
    let total = HEADER_LEN + body_len;
    validate_envelope_len(total)?;
    let mut out = Vec::with_capacity(total);
    out.push(VERSION);
    out.push(event.kind() as u8);
    out.extend_from_slice(&event.message_id);
    out.extend_from_slice(&event.sender_ed);
    match &event.body {
        Body::Text(text) => out.extend_from_slice(text.as_bytes()),
        Body::Voice(manifest) => out.extend_from_slice(&manifest.encode()?),
        Body::Edit {
            target,
            revision,
            text,
        } => {
            out.extend_from_slice(target);
            out.extend_from_slice(&revision.to_be_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        Body::Delete { target, revision } => {
            out.extend_from_slice(target);
            out.extend_from_slice(&revision.to_be_bytes());
        }
    }
    Ok(out)
}

/// Decode one complete envelope, checking lengths before slices or allocation.
pub fn decode(bytes: &[u8]) -> Result<Event, &'static str> {
    validate_envelope_len(bytes.len())?;
    if bytes[0] != VERSION {
        return Err("unknown E2E version");
    }
    let body_bytes = &bytes[HEADER_LEN..];
    let body = match bytes[1] {
        1 => Body::Text(decode_text(body_bytes)?),
        4 => Body::Voice(VoiceManifest::decode(body_bytes)?),
        2 => {
            let (target, revision) = decode_control(body_bytes)?;
            Body::Edit {
                target,
                revision,
                text: decode_text(&body_bytes[CONTROL_LEN..])?,
            }
        }
        3 => {
            if body_bytes.len() != CONTROL_LEN {
                return Err("invalid E2E delete length");
            }
            let (target, revision) = decode_control(body_bytes)?;
            Body::Delete { target, revision }
        }
        _ => return Err("unknown E2E kind"),
    };
    Ok(Event {
        message_id: bytes[2..18]
            .try_into()
            .map_err(|_| "truncated E2E header")?,
        sender_ed: bytes[18..HEADER_LEN]
            .try_into()
            .map_err(|_| "truncated E2E header")?,
        body,
    })
}

fn validate_envelope_len(len: usize) -> Result<(), &'static str> {
    if len < HEADER_LEN {
        return Err("truncated E2E header");
    }
    if len > ENVELOPE_MAX || len > CIPHERTEXT_MAX {
        return Err("E2E envelope too large");
    }
    Ok(())
}

fn validate_text_bytes(len: usize) -> Result<(), &'static str> {
    if len == 0 || len > TEXT_UTF8_MAX {
        return Err("invalid E2E text length");
    }
    Ok(())
}

/// New TEXT/EDIT source is 1..=4000 Unicode scalar values. Validate only:
/// spaces, newlines and Markdown markers count; never trim or normalize source.
pub fn validate_text(text: &str) -> Result<(), &'static str> {
    validate_text_bytes(text.len())?;
    if text.chars().count() > TEXT_CHAR_MAX {
        return Err("invalid E2E text length");
    }
    Ok(())
}

fn validate_revision(revision: u64) -> Result<(), &'static str> {
    if revision == 0 || i64::try_from(revision).is_err() {
        return Err("invalid E2E revision");
    }
    Ok(())
}

fn decode_text(bytes: &[u8]) -> Result<String, &'static str> {
    validate_text_bytes(bytes.len())?;
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid E2E UTF-8")?;
    validate_text(text)?;
    Ok(text.to_owned())
}

fn decode_control(bytes: &[u8]) -> Result<([u8; 16], u64), &'static str> {
    if bytes.len() < CONTROL_LEN {
        return Err("truncated E2E control");
    }
    let target = bytes[..16]
        .try_into()
        .map_err(|_| "truncated E2E control")?;
    let revision = u64::from_be_bytes(
        bytes[16..CONTROL_LEN]
            .try_into()
            .map_err(|_| "truncated E2E control")?,
    );
    validate_revision(revision)?;
    Ok((target, revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MID: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];
    const SENDER: [u8; 32] = [
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
        0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d,
        0x3e, 0x3f,
    ];
    const TARGET: [u8; 16] = [
        0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e,
        0x4f,
    ];
    const REVISION: u64 = 0x0102_0304_0506_0708;
    const TEXT_VECTOR: &[u8] = &[
        1, 1, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c,
        0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b,
        0x3c, 0x3d, 0x3e, 0x3f, 0x68, 0xc3, 0xa9,
    ];
    const EDIT_VECTOR: &[u8] = &[
        1, 2, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c,
        0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b,
        0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a,
        0x4b, 0x4c, 0x4d, 0x4e, 0x4f, 1, 2, 3, 4, 5, 6, 7, 8, 0xe4, 0xbf, 0xae,
    ];
    const DELETE_VECTOR: &[u8] = &[
        1, 3, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c,
        0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b,
        0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a,
        0x4b, 0x4c, 0x4d, 0x4e, 0x4f, 1, 2, 3, 4, 5, 6, 7, 8,
    ];

    fn event(body: Body) -> Event {
        Event {
            message_id: MID,
            sender_ed: SENDER,
            body,
        }
    }

    fn wire(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = TEXT_VECTOR[..HEADER_LEN].to_vec();
        bytes[1] = kind;
        bytes.extend_from_slice(body);
        bytes
    }

    fn assert_vector(event: Event, kind: Kind, bytes: &[u8]) {
        assert_eq!(event.kind(), kind);
        assert_eq!(encode(&event).unwrap(), bytes);
        assert_eq!(encode(&event).unwrap(), encode(&event).unwrap());
        let decoded = decode(bytes).unwrap();
        assert_eq!(decoded, event);
        assert_eq!(encode(&decoded).unwrap(), bytes);
    }

    #[test]
    fn text_fixed_vector_and_deterministic_roundtrip() {
        assert_vector(event(Body::Text("hé".into())), Kind::Text, TEXT_VECTOR);
    }

    #[test]
    fn edit_fixed_vector_and_deterministic_roundtrip() {
        assert_vector(
            event(Body::Edit {
                target: TARGET,
                revision: REVISION,
                text: "修".into(),
            }),
            Kind::Edit,
            EDIT_VECTOR,
        );
    }

    #[test]
    fn delete_fixed_vector_and_deterministic_roundtrip() {
        assert_vector(
            event(Body::Delete {
                target: TARGET,
                revision: REVISION,
            }),
            Kind::Delete,
            DELETE_VECTOR,
        );
    }

    #[test]
    fn text_and_edit_unicode_scalar_bounds_and_exact_source() {
        assert_eq!(TEXT_CHAR_MAX, 4000);
        assert_eq!(TEXT_UTF8_MAX, 16000);
        assert_eq!(ENVELOPE_MAX, 16074);
        for text in [
            "x".to_owned(),
            " \0\n".to_owned(),
            " \n".repeat(TEXT_CHAR_MAX / 2),
            "a".repeat(TEXT_CHAR_MAX),
            "я".repeat(TEXT_CHAR_MAX),
            "修".repeat(TEXT_CHAR_MAX),
            "🦀".repeat(TEXT_CHAR_MAX),
            "e\u{301}".repeat(TEXT_CHAR_MAX / 2),
            format!("{}é", "a".repeat(TEXT_CHAR_MAX - 1)),
            "  # heading\r\n**bold** _italic_ ~~strike~~ `code`\n> quote\n- list\n```\na < b\n```\n[link](https://example.test)\n![alt](image) <b>literal</b>  \n".into(),
        ] {
            assert_eq!(validate_text(&text), Ok(()));
            for body in [
                Body::Text(text.clone()),
                Body::Edit {
                    target: TARGET,
                    revision: 1,
                    text: text.clone(),
                },
            ] {
                let event = event(body);
                let bytes = encode(&event).unwrap();
                let control_len = if event.kind() == Kind::Edit {
                    CONTROL_LEN
                } else {
                    0
                };
                assert_eq!(bytes.len(), HEADER_LEN + control_len + text.len());
                assert_eq!(&bytes[HEADER_LEN + control_len..], text.as_bytes());
                assert!(bytes.len() <= ENVELOPE_MAX && bytes.len() <= CIPHERTEXT_MAX);
                assert_eq!(decode(&bytes).unwrap(), event);
            }
        }
    }

    #[test]
    fn empty_and_oversize_text_rejected_by_both_directions() {
        for text in [
            String::new(),
            "a".repeat(TEXT_CHAR_MAX + 1),
            "я".repeat(TEXT_CHAR_MAX + 1),
            "修".repeat(TEXT_CHAR_MAX + 1),
            "🦀".repeat(TEXT_CHAR_MAX + 1),
            format!("{}é", "a".repeat(TEXT_CHAR_MAX)),
        ] {
            assert_eq!(validate_text(&text), Err("invalid E2E text length"));
            assert!(encode(&event(Body::Text(text.clone()))).is_err());
            assert!(decode(&wire(1, text.as_bytes())).is_err());
            assert!(encode(&event(Body::Edit {
                target: TARGET,
                revision: 1,
                text: text.clone(),
            }))
            .is_err());
            let mut body = DELETE_VECTOR[HEADER_LEN..].to_vec();
            body.extend_from_slice(text.as_bytes());
            assert!(decode(&wire(2, &body)).is_err());
        }
        // Scalar overflow must be rejected even though its byte length fits.
        let ascii = "a".repeat(TEXT_CHAR_MAX + 1);
        assert!(ascii.len() < TEXT_UTF8_MAX);
        assert_eq!(
            decode(&wire(1, ascii.as_bytes())),
            Err("invalid E2E text length")
        );
    }

    #[test]
    fn decode_byte_bound_precedes_utf8_and_edit_revision_precedes_text() {
        assert_eq!(
            decode(&wire(1, &vec![0xff; TEXT_UTF8_MAX + 1])),
            Err("invalid E2E text length")
        );
        let body = vec![0xff; TEXT_UTF8_MAX];
        assert_eq!(decode(&wire(1, &body)), Err("invalid E2E UTF-8"));
        let mut body = TARGET.to_vec();
        body.extend_from_slice(&0u64.to_be_bytes());
        body.push(0xff);
        assert_eq!(decode(&wire(2, &body)), Err("invalid E2E revision"));
        assert_eq!(
            encode(&event(Body::Edit {
                target: TARGET,
                revision: 0,
                text: "a".repeat(TEXT_CHAR_MAX + 1),
            })),
            Err("invalid E2E revision")
        );
    }

    #[test]
    fn malformed_utf8_rejected() {
        for text in [
            &[0xff][..],
            &[0xc3][..],
            &[0xc0, 0x80][..],
            &[0xed, 0xa0, 0x80][..],
            &[0xf4, 0x90, 0x80, 0x80][..],
        ] {
            assert_eq!(decode(&wire(1, text)), Err("invalid E2E UTF-8"));
            let mut body = DELETE_VECTOR[HEADER_LEN..].to_vec();
            body.extend_from_slice(text);
            assert_eq!(decode(&wire(2, &body)), Err("invalid E2E UTF-8"));
        }
    }

    #[test]
    fn truncated_headers_and_controls_rejected() {
        for end in 0..=HEADER_LEN {
            assert!(decode(&TEXT_VECTOR[..end]).is_err());
        }
        for end in 0..EDIT_VECTOR.len() {
            assert!(decode(&EDIT_VECTOR[..end]).is_err());
        }
        for end in 0..DELETE_VECTOR.len() {
            assert!(decode(&DELETE_VECTOR[..end]).is_err());
        }
    }

    #[test]
    fn unknown_version_and_kind_rejected() {
        for vector in [TEXT_VECTOR, EDIT_VECTOR, DELETE_VECTOR] {
            for version in [0, 2, u8::MAX] {
                let mut bytes = vector.to_vec();
                bytes[0] = version;
                assert_eq!(decode(&bytes), Err("unknown E2E version"));
            }
            for kind in [0, 5, u8::MAX] {
                let mut bytes = vector.to_vec();
                bytes[1] = kind;
                assert_eq!(decode(&bytes), Err("unknown E2E kind"));
            }
        }
    }

    #[test]
    fn revision_boundaries_validated_by_both_directions() {
        for revision in [1, u64::try_from(i64::MAX).unwrap(), 0, 1 << 63, u64::MAX] {
            for (kind, body) in [
                (
                    2,
                    Body::Edit {
                        target: TARGET,
                        revision,
                        text: "edit".into(),
                    },
                ),
                (
                    3,
                    Body::Delete {
                        target: TARGET,
                        revision,
                    },
                ),
            ] {
                let event = event(body);
                let mut bytes = if kind == 2 {
                    EDIT_VECTOR.to_vec()
                } else {
                    DELETE_VECTOR.to_vec()
                };
                bytes[HEADER_LEN + 16..HEADER_LEN + CONTROL_LEN]
                    .copy_from_slice(&revision.to_be_bytes());
                if revision > 0 && i64::try_from(revision).is_ok() {
                    assert_eq!(decode(&encode(&event).unwrap()).unwrap(), event);
                    assert!(decode(&bytes).is_ok());
                } else {
                    assert_eq!(encode(&event), Err("invalid E2E revision"));
                    assert_eq!(decode(&bytes), Err("invalid E2E revision"));
                }
            }
        }
    }

    #[test]
    fn delete_trailing_data_rejected() {
        for trailing in [1, TEXT_UTF8_MAX] {
            let mut bytes = DELETE_VECTOR.to_vec();
            bytes.resize(bytes.len() + trailing, 0);
            assert_eq!(decode(&bytes), Err("invalid E2E delete length"));
        }
    }

    #[test]
    fn whole_envelope_cap_rejected() {
        for len in [ENVELOPE_MAX + 1, CIPHERTEXT_MAX + 1] {
            let mut bytes = EDIT_VECTOR.to_vec();
            bytes.resize(len, b'x');
            assert_eq!(decode(&bytes), Err("E2E envelope too large"));
        }
    }

    #[test]
    fn voice_fixed_layout_and_geometry() {
        let mut m = VoiceManifest {
            blob_id: [1; 16],
            key: [2; 32],
            nonce_prefix: [3; 8],
            recipient_binding: [4; 32],
            plain_len: 8177,
            byte_len: 8209,
            sample_count: 960000,
            waveform: vec![5; 64],
        };
        let e = event(Body::Voice(m.clone()));
        let p = encode(&e).unwrap();
        assert_eq!(p.len(), HEADER_LEN + 166);
        assert_eq!(&p[HEADER_LEN..HEADER_LEN + 2], &[1, 1]);
        assert_eq!(p[1], 4);
        assert_eq!(decode(&p).unwrap(), e);
        for n in 0..166 {
            assert!(VoiceManifest::decode(&p[HEADER_LEN..HEADER_LEN + n]).is_err());
        }
        let mut bad = m.encode().unwrap();
        bad.push(0);
        assert!(VoiceManifest::decode(&bad).is_err());
        for field in [0, 1] {
            let mut bad = m.encode().unwrap();
            bad[field] = 2;
            assert!(VoiceManifest::decode(&bad).is_err());
        }
        m.waveform.pop();
        assert!(m.validate().is_err());
        m.waveform.push(5);
        for sample in [0, 960001, u32::MAX] {
            m.sample_count = sample;
            assert!(m.validate().is_err());
        }
        m.sample_count = 1;
        for (plain, cipher) in [(1, 17), (8176, 8192), (8177, 8209), (130816, 131072)] {
            m.plain_len = plain;
            m.byte_len = cipher;
            assert!(m.validate().is_ok());
        }
        for (plain, cipher) in [
            (0, 16),
            (8177, 8193),
            (130817, 131089),
            (u32::MAX, u32::MAX),
        ] {
            m.plain_len = plain;
            m.byte_len = cipher;
            assert!(m.validate().is_err());
        }
    }
}

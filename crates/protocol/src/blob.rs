//! Wire2 opaque blob payloads. All integers are big endian. Builders return
//! payloads (use `encode_frame` separately); parsers reject trailing bytes.
//! STATUS_RESP = id16 + byte_len4 + state1 + received_bitmap8 (bit index).
//! PUT/DATA = id16 + index2 + length2 + bytes; PUT_ACK = id16 + index2.
//! FINISH/FINISH_ACK/STATUS = id16; GET = id16 + index2.
//! SEND_MEDIA = recipient16 + expected_device32 + MID16 + blob16 + Olm wire.

use crate::{BLOB_SIZE_MAX, CIPHERTEXT_MAX, MAX_PAYLOAD};

pub const CHUNK_MAX: usize = 8192;
pub const CHUNK_PLAIN_MAX: usize = 8176;
pub const VOICE_MAX: usize = 131072;
pub const CHUNKS_MAX: u16 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Reserved = 0,
    Complete = 1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub blob_id: [u8; 16],
    pub byte_len: u32,
    pub state: State,
    pub bitmap: u64,
}

/// Generic blob geometry is ciphertext-sized; voice manifest separately binds
/// plaintext geometry, including each 16-byte AEAD tag.
pub fn chunk_count(byte_len: u32) -> Option<u16> {
    if byte_len == 0 || byte_len > BLOB_SIZE_MAX {
        return None;
    }
    u16::try_from(byte_len.div_ceil(CHUNK_MAX as u32)).ok()
}

pub fn chunk_len(byte_len: u32, index: u16) -> Option<usize> {
    let count = chunk_count(byte_len)?;
    if index >= count {
        return None;
    }
    Some((byte_len as usize - usize::from(index) * CHUNK_MAX).min(CHUNK_MAX))
}

pub fn full_bitmap(byte_len: u32) -> Option<u64> {
    let count = chunk_count(byte_len)?;
    Some(if count == 64 {
        u64::MAX
    } else {
        (1u64 << count) - 1
    })
}

pub fn build_reserve(blob_id: &[u8; 16], byte_len: u32) -> Option<Vec<u8>> {
    chunk_count(byte_len)?;
    let mut p = blob_id.to_vec();
    p.extend_from_slice(&byte_len.to_be_bytes());
    Some(p)
}
pub fn parse_reserve(p: &[u8]) -> Option<([u8; 16], u32)> {
    let (id, size) = crate::mailbox::parse_reserve(p)?;
    Some((id.try_into().ok()?, size))
}
pub fn parse_reserved(p: &[u8]) -> Option<[u8; 16]> {
    p.try_into().ok()
}
pub fn build_status(blob_id: &[u8; 16]) -> Vec<u8> {
    blob_id.to_vec()
}
pub fn parse_status(p: &[u8]) -> Option<[u8; 16]> {
    p.try_into().ok()
}
pub fn build_status_response(s: &Status) -> Option<Vec<u8>> {
    let all = full_bitmap(s.byte_len)?;
    if s.bitmap & !all != 0 || (s.state == State::Complete && s.bitmap != all) {
        return None;
    }
    let mut p = s.blob_id.to_vec();
    p.extend_from_slice(&s.byte_len.to_be_bytes());
    p.push(s.state as u8);
    p.extend_from_slice(&s.bitmap.to_be_bytes());
    Some(p)
}
pub fn parse_status_response(p: &[u8]) -> Option<Status> {
    if p.len() != 29 {
        return None;
    }
    let s = Status {
        blob_id: p[..16].try_into().ok()?,
        byte_len: u32::from_be_bytes(p[16..20].try_into().ok()?),
        state: match p[20] {
            0 => State::Reserved,
            1 => State::Complete,
            _ => return None,
        },
        bitmap: u64::from_be_bytes(p[21..].try_into().ok()?),
    };
    build_status_response(&s)?;
    Some(s)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub blob_id: &'a [u8; 16],
    pub index: u16,
    pub bytes: &'a [u8],
}
pub fn build_put(blob_id: &[u8; 16], index: u16, bytes: &[u8]) -> Option<Vec<u8>> {
    if index >= CHUNKS_MAX || bytes.is_empty() || bytes.len() > CHUNK_MAX {
        return None;
    }
    let mut p = build_get(blob_id, index)?;
    p.extend_from_slice(&u16::try_from(bytes.len()).ok()?.to_be_bytes());
    p.extend_from_slice(bytes);
    Some(p)
}
pub fn parse_put(p: &[u8]) -> Option<Chunk<'_>> {
    if p.len() < 21 || p.len() > 20 + CHUNK_MAX {
        return None;
    }
    let index = u16::from_be_bytes(p[16..18].try_into().ok()?);
    let len = usize::from(u16::from_be_bytes(p[18..20].try_into().ok()?));
    if index >= CHUNKS_MAX || len == 0 || len != p.len() - 20 {
        return None;
    }
    Some(Chunk {
        blob_id: p[..16].try_into().ok()?,
        index,
        bytes: &p[20..],
    })
}
pub fn build_put_ack(blob_id: &[u8; 16], index: u16) -> Option<Vec<u8>> {
    build_get(blob_id, index)
}
pub fn parse_put_ack(p: &[u8]) -> Option<([u8; 16], u16)> {
    parse_get(p)
}
pub fn build_finish(blob_id: &[u8; 16]) -> Vec<u8> {
    blob_id.to_vec()
}
pub fn parse_finish(p: &[u8]) -> Option<[u8; 16]> {
    p.try_into().ok()
}
pub fn build_finish_ack(blob_id: &[u8; 16]) -> Vec<u8> {
    blob_id.to_vec()
}
pub fn parse_finish_ack(p: &[u8]) -> Option<[u8; 16]> {
    p.try_into().ok()
}
pub fn build_get(blob_id: &[u8; 16], index: u16) -> Option<Vec<u8>> {
    if index >= CHUNKS_MAX {
        return None;
    }
    let mut p = blob_id.to_vec();
    p.extend_from_slice(&index.to_be_bytes());
    Some(p)
}
pub fn parse_get(p: &[u8]) -> Option<([u8; 16], u16)> {
    if p.len() != 18 {
        return None;
    }
    let index = u16::from_be_bytes(p[16..].try_into().ok()?);
    if index >= CHUNKS_MAX {
        return None;
    }
    Some((p[..16].try_into().ok()?, index))
}
pub fn build_data(blob_id: &[u8; 16], index: u16, bytes: &[u8]) -> Option<Vec<u8>> {
    build_put(blob_id, index, bytes)
}
pub fn parse_data(p: &[u8]) -> Option<Chunk<'_>> {
    parse_put(p)
}

#[derive(Debug, PartialEq, Eq)]
pub struct SendMedia<'a> {
    pub recipient: &'a [u8; 16],
    pub recipient_device: &'a [u8; 32],
    pub message_id: &'a [u8; 16],
    pub blob_id: &'a [u8; 16],
    pub ciphertext: &'a [u8],
}
pub fn build_send_media(
    recipient: &[u8; 16],
    recipient_device: &[u8; 32],
    message_id: &[u8; 16],
    blob_id: &[u8; 16],
    ciphertext: &[u8],
) -> Option<Vec<u8>> {
    if ciphertext.is_empty()
        || ciphertext.len() > CIPHERTEXT_MAX
        || 80 + ciphertext.len() > MAX_PAYLOAD
    {
        return None;
    }
    let mut p = Vec::with_capacity(80 + ciphertext.len());
    p.extend_from_slice(recipient);
    p.extend_from_slice(recipient_device);
    p.extend_from_slice(message_id);
    p.extend_from_slice(blob_id);
    p.extend_from_slice(ciphertext);
    Some(p)
}
pub fn parse_send_media(p: &[u8]) -> Option<SendMedia<'_>> {
    if p.len() <= 80 || p.len() > MAX_PAYLOAD || p.len() - 80 > CIPHERTEXT_MAX {
        return None;
    }
    Some(SendMedia {
        recipient: p[..16].try_into().ok()?,
        recipient_device: p[16..48].try_into().ok()?,
        message_id: p[48..64].try_into().ok()?,
        blob_id: p[64..80].try_into().ok()?,
        ciphertext: &p[80..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_layout_bounds_and_trailing_rejection() {
        let id = [0xaa; 16];
        let put = build_put(&id, 3, b"abc").unwrap();
        assert_eq!(&put[16..], &[0, 3, 0, 3, b'a', b'b', b'c']);
        assert_eq!(
            parse_data(&put).unwrap(),
            Chunk {
                blob_id: &id,
                index: 3,
                bytes: b"abc"
            }
        );
        for n in 0..put.len() {
            assert!(parse_put(&put[..n]).is_none());
        }
        let mut bad = put.clone();
        bad.push(0);
        assert!(parse_put(&bad).is_none());
        assert!(build_put(&id, 64, b"a").is_none());
        assert!(build_put(&id, 0, &vec![0; 8193]).is_none());
        let s = Status {
            blob_id: id,
            byte_len: BLOB_SIZE_MAX,
            state: State::Complete,
            bitmap: u64::MAX,
        };
        let p = build_status_response(&s).unwrap();
        assert_eq!(parse_status_response(&p), Some(s));
        assert_eq!(chunk_len(8193, 1), Some(1));
        assert_eq!(chunk_len(8193, 2), None);
        let mut p = build_status_response(&Status {
            blob_id: id,
            byte_len: 1,
            state: State::Reserved,
            bitmap: 0,
        })
        .unwrap();
        p[28] = 2;
        assert!(parse_status_response(&p).is_none());
        p[28] = 0;
        p[20] = 1;
        assert!(parse_status_response(&p).is_none());
        let media = build_send_media(&id, &[2; 32], &[3; 16], &[4; 16], b"olm").unwrap();
        assert_eq!(parse_send_media(&media).unwrap().ciphertext, b"olm");
        assert!(parse_send_media(&media[..80]).is_none());
        assert!(build_send_media(&id, &[2; 32], &id, &id, &vec![0; MAX_PAYLOAD - 79]).is_none());
        assert_eq!(
            [
                crate::OP_BLOB_STATUS,
                crate::OP_BLOB_STATUS_RESP,
                crate::OP_BLOB_PUT,
                crate::OP_BLOB_PUT_ACK,
                crate::OP_BLOB_FINISH,
                crate::OP_BLOB_FINISH_ACK,
                crate::OP_BLOB_GET,
                crate::OP_BLOB_DATA,
                crate::OP_SEND_MEDIA
            ],
            [37, 38, 39, 40, 41, 42, 43, 44, 45]
        );
    }
}

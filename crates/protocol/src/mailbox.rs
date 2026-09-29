//! Layout валидных payload mailbox-опкодов P4 (фиксированные поля, BE).
//! Парсинг строгий: неверная длина → Err, без паник. Wire-совместимость — тесты ниже.

use super::*;

/// SEND: recipient 16 + message_id 16 + ciphertext.
#[derive(Debug, PartialEq, Eq)]
pub struct Send<'a> {
    pub recipient: &'a [u8],
    pub message_id: &'a [u8],
    pub ciphertext: &'a [u8],
}

pub fn parse_send(p: &[u8]) -> Option<Send<'_>> {
    if p.len() < 32 || p.len() - 32 > CIPHERTEXT_MAX {
        return None;
    }
    Some(Send { recipient: &p[..16], message_id: &p[16..32], ciphertext: &p[32..] })
}

/// SEND_ACK: message_id 16 + status u8.
pub fn parse_send_ack(p: &[u8]) -> Option<(&[u8], u8)> {
    if p.len() != 17 {
        return None;
    }
    Some((&p[..16], p[16]))
}

/// Одна запись FETCH_RESP.
#[derive(Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub seq: u64,
    pub sender: &'a [u8],
    pub message_id: &'a [u8],
    pub ciphertext: &'a [u8],
}

/// FETCH_RESP: count u16 + записи (seq8 + sender32 + msgid16 + ctlen2 + ct).
pub fn parse_fetch_resp(p: &[u8]) -> Option<Vec<Event<'_>>> {
    if p.len() < 2 {
        return None;
    }
    let n = u16::from_be_bytes([p[0], p[1]]) as usize;
    let mut out = Vec::with_capacity(n.min(FETCH_BATCH_MAX));
    let mut cur = &p[2..];
    for _ in 0..n {
        if cur.len() < 8 + 32 + 16 + 2 {
            return None;
        }
        let seq = u64::from_be_bytes(cur[..8].try_into().ok()?);
        let sender = &cur[8..40];
        let message_id = &cur[40..56];
        let ctlen = u16::from_be_bytes(cur[56..58].try_into().ok()?) as usize;
        cur = &cur[58..];
        if cur.len() < ctlen {
            return None;
        }
        let (ct, rest) = cur.split_at(ctlen);
        out.push(Event { seq, sender, message_id, ciphertext: ct });
        cur = rest;
    }
    if !cur.is_empty() {
        return None;
    }
    Some(out)
}

/// DELIVERY_ACK: count u16 + seq u64*.
pub fn parse_delivery_ack(p: &[u8]) -> Option<Vec<u64>> {
    if p.len() < 2 || (p.len() - 2) % 8 != 0 {
        return None;
    }
    let n = u16::from_be_bytes([p[0], p[1]]) as usize;
    if 2 + n * 8 != p.len() {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(u64::from_be_bytes(p[2 + 8 * i..10 + 8 * i].try_into().ok()?));
    }
    Some(out)
}

/// Одна prekey-запись: key_id + one_time + pubkey 32 + sig 64 = 101 байт.
pub const PREKEY_ENTRY_LEN: usize = 4 + 1 + 32 + 64;

/// UPLOAD_PREKEYS: identity 32 + count u16 + записи.
pub fn parse_upload(p: &[u8]) -> Option<(&[u8], Vec<(u32, u8, &[u8], &[u8])>)> {
    if p.len() < 34 {
        return None;
    }
    let n = u16::from_be_bytes([p[32], p[33]]) as usize;
    if 34 + n * PREKEY_ENTRY_LEN != p.len() {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = &p[34 + i * PREKEY_ENTRY_LEN..34 + (i + 1) * PREKEY_ENTRY_LEN];
        out.push((
            u32::from_be_bytes(e[..4].try_into().ok()?),
            e[4],
            &e[5..37],
            &e[37..101],
        ));
    }
    Some((&p[..32], out))
}

/// CLAIM / COUNT: device_key 32.
pub fn parse_device(p: &[u8]) -> Option<&[u8]> {
    if p.len() != 32 {
        return None;
    }
    Some(p)
}

/// BLOB_RESERVE: blob_id 16 + size u32 BE.
pub fn parse_reserve(p: &[u8]) -> Option<(&[u8], u32)> {
    if p.len() != 20 {
        return None;
    }
    Some((&p[..16], u32::from_be_bytes(p[16..20].try_into().ok()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_roundtrip_and_bounds() {
        let mut p = vec![0u8; 32 + 10];
        p[..16].fill(0xAA);
        p[16..32].fill(0xBB);
        let s = parse_send(&p).unwrap();
        assert_eq!((s.recipient[0], s.message_id[0], s.ciphertext.len()), (0xAA, 0xBB, 10));
        assert!(parse_send(&p[..31]).is_none());
        assert!(parse_send(&vec![0u8; 33 + CIPHERTEXT_MAX]).is_none());
    }

    #[test]
    fn fetch_resp_roundtrip_and_strict() {
        // count=1, seq=7, sender=0x11, msgid=0x22, ctlen=3, ct=abc
        let mut p = vec![0u8, 1];
        p.extend_from_slice(&7u64.to_be_bytes());
        p.extend_from_slice(&[0x11u8; 32]);
        p.extend_from_slice(&[0x22u8; 16]);
        p.extend_from_slice(&[0u8, 3]);
        p.extend_from_slice(b"abc");
        let ev = parse_fetch_resp(&p).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].seq, ev[0].ciphertext), (7, b"abc".as_slice()));
        let mut bad = p.clone();
        bad.push(0);
        assert!(parse_fetch_resp(&bad).is_none());
        assert!(parse_fetch_resp(&p[..10]).is_none());
    }

    #[test]
    fn delivery_ack_shape() {
        let mut p = vec![0u8, 2];
        p.extend_from_slice(&5u64.to_be_bytes());
        p.extend_from_slice(&9u64.to_be_bytes());
        assert_eq!(parse_delivery_ack(&p).unwrap(), vec![5, 9]);
        assert!(parse_delivery_ack(&[0, 2, 1, 2, 3]).is_none());
    }

    #[test]
    fn upload_and_reserve_shapes() {
        let mut p = vec![0xCCu8; 32];
        p.extend_from_slice(&[0u8, 1]);
        p.extend_from_slice(&1u32.to_be_bytes());
        p.push(1);
        p.extend_from_slice(&[0xDDu8; 32]);
        p.extend_from_slice(&[0xEEu8; 64]);
        let (id, entries) = parse_upload(&p).unwrap();
        assert_eq!((id[0], entries.len(), entries[0].0, entries[0].1), (0xCC, 1, 1, 1));
        assert!(parse_upload(&p[..40]).is_none());
        let mut r = vec![0x11u8; 16];
        r.extend_from_slice(&512u32.to_be_bytes());
        let (bid, size) = parse_reserve(&r).unwrap();
        assert_eq!((bid[0], size), (0x11, 512));
        assert!(parse_device(&[0u8; 31]).is_none());
        assert!(parse_device(&[0u8; 32]).is_some());
    }
}

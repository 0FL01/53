//! Layout валидных payload mailbox-опкодов P4 (фиксированные поля, BE).
//! Парсинг строгий: неверная длина → Err, без паник. Wire-совместимость — тесты ниже.
//!
//! Контракты парсеров (зафиксированы векторами):
//! - no panic на сетевых байтах: только срезы после проверок длины,
//!   try_into()+ok()? вместо unwrap, длины из сети — через usize::try_from (без as);
//! - no аллокаций по заявленной длине: Vec::with_capacity — только от min(n, CAP)
//!   либо после проверки точного равенства длины (n ограничен реальным вводом);
//! - trailing-байты после записей — reject (None);
//! - count FETCH_RESP > FETCH_BATCH_MAX парсер НЕ режет (влезло в кадр —
//!   вернётся всё); кап 32 — при сборке ответа сервером (main fetch_reply);
//! - one_time байт сохраняется как есть (семантика 0/1 — серверная,
//!   prekey хранит байт, claim/count смотрят one_time=1);
//! - дубли key_id сохраняются порядком (upsert — серверный ON CONFLICT).

use super::*;

/// SEND: recipient 16 + message_id 16 + ciphertext.
#[derive(Debug, PartialEq, Eq)]
pub struct Send<'a> {
    pub recipient: &'a [u8],
    pub message_id: &'a [u8],
    pub ciphertext: &'a [u8],
}

/// SEND: recipient 16 + message_id 16 + НЕПУСТОЙ ciphertext.
/// Пустой SEND (len==32) отвергается: E2E-ciphertext не бывает пустым.
pub fn parse_send(p: &[u8]) -> Option<Send<'_>> {
    if p.len() <= 32 || p.len() - 32 > CIPHERTEXT_MAX {
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
/// Аллокация — min(n, FETCH_BATCH_MAX): заявленный count не раздувает Vec.
pub fn parse_fetch_resp(p: &[u8]) -> Option<Vec<Event<'_>>> {
    if p.len() < 2 {
        return None;
    }
    let n = usize::try_from(u16::from_be_bytes([p[0], p[1]])).ok()?;
    let mut out = Vec::with_capacity(n.min(FETCH_BATCH_MAX));
    let mut cur = &p[2..];
    for _ in 0..n {
        if cur.len() < 8 + 32 + 16 + 2 {
            return None;
        }
        let seq = u64::from_be_bytes(cur[..8].try_into().ok()?);
        let sender = &cur[8..40];
        let message_id = &cur[40..56];
        let ctlen = usize::try_from(u16::from_be_bytes(cur[56..58].try_into().ok()?)).ok()?;
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
/// Точное равенство 2+n*8==len проверяется ДО аллокации: заявленный count
/// ограничен реальным вводом.
pub fn parse_delivery_ack(p: &[u8]) -> Option<Vec<u64>> {
    if p.len() < 2 || (p.len() - 2) % 8 != 0 {
        return None;
    }
    let n = usize::try_from(u16::from_be_bytes([p[0], p[1]])).ok()?;
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
/// Точное равенство 34+n*ENTRY_LEN==len проверяется ДО аллокации.
/// one_time байт и дубли key_id сохраняются как есть (см. контракты модуля).
pub fn parse_upload(p: &[u8]) -> Option<(&[u8], Vec<(u32, u8, &[u8], &[u8])>)> {
    if p.len() < 34 {
        return None;
    }
    let n = usize::try_from(u16::from_be_bytes([p[32], p[33]])).ok()?;
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

/// PREKEY: ответ сервера на CLAIM: key_id u32 BE + pubkey 32 (ровно 36).
/// Формат зафиксирован `claim_reply` (main.rs): сначала id, затем ключ.
pub fn parse_prekey(p: &[u8]) -> Option<(u32, [u8; 32])> {
    if p.len() != 36 {
        return None;
    }
    let id = u32::from_be_bytes(p[..4].try_into().ok()?);
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&p[4..]);
    Some((id, pubkey))
}

/// BLOB_RESERVE: blob_id 16 + size u32 BE.
/// size==0 и size>BLOB_SIZE_MAX отвергаются парсером (зеркало blob::reserve,
// раньше — только сервером после decrypt).
pub fn parse_reserve(p: &[u8]) -> Option<(&[u8], u32)> {
    if p.len() != 20 {
        return None;
    }
    let size = u32::from_be_bytes(p[16..20].try_into().ok()?);
    if size == 0 || size > super::BLOB_SIZE_MAX {
        return None;
    }
    Some((&p[..16], size))
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
    fn send_empty_rejected() {
        // len==32 (пустой ciphertext) — reject; 1 байт — уже валидный SEND.
        assert!(parse_send(&vec![0u8; 32]).is_none());
        assert!(parse_send(&[]).is_none());
        assert_eq!(parse_send(&vec![0xAAu8; 33]).unwrap().ciphertext.len(), 1);
    }

    #[test]
    fn reserve_zero_and_huge_rejected() {
        fn payload(size: u32) -> Vec<u8> {
            let mut r = vec![0x11u8; 16];
            r.extend_from_slice(&size.to_be_bytes());
            r
        }
        assert!(parse_reserve(&payload(0)).is_none());
        assert!(parse_reserve(&payload(BLOB_SIZE_MAX + 1)).is_none());
        assert!(parse_reserve(&payload(u32::MAX)).is_none());
        assert_eq!(parse_reserve(&payload(1)).unwrap().1, 1);
        assert_eq!(parse_reserve(&payload(BLOB_SIZE_MAX)).unwrap().1, BLOB_SIZE_MAX);
        assert!(parse_reserve(&vec![0u8; 19]).is_none());
    }

    #[test]
    fn fetch_resp_over_batch_parses_uncapped() {
        // Контракт: count>32 парсер не режет (кап — при сборке ответа).
        // 33 минимальные записи (ctlen=0) влезают в кадр.
        let n = FETCH_BATCH_MAX + 1;
        let mut p = vec![(n >> 8) as u8, n as u8];
        for i in 0..n {
            p.extend_from_slice(&(i as u64).to_be_bytes());
            p.extend_from_slice(&[0x11u8; 32]);
            p.extend_from_slice(&[0x22u8; 16]);
            p.extend_from_slice(&[0u8, 0]);
        }
        let ev = parse_fetch_resp(&p).unwrap();
        assert_eq!(ev.len(), n);
        assert_eq!(ev[n - 1].seq, (n - 1) as u64);
    }

    #[test]
    fn upload_raw_one_time_and_dup_key_id() {
        // Контракт: one_time сохраняется как есть, дубли key_id — порядком.
        let mut p = vec![0xCCu8; 32];
        p.extend_from_slice(&[0u8, 2]);
        for (kid, ot) in [(7u32, 7u8), (7u32, 1u8)] {
            p.extend_from_slice(&kid.to_be_bytes());
            p.push(ot);
            p.extend_from_slice(&[0xDDu8; 32]);
            p.extend_from_slice(&[0xEEu8; 64]);
        }
        let (_, entries) = parse_upload(&p).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].0, entries[0].1), (7, 7));
        assert_eq!((entries[1].0, entries[1].1), (7, 1));
        // trailing после записей — reject.
        let mut bad = p.clone();
        bad.push(0);
        assert!(parse_upload(&bad).is_none());
    }

    #[test]
    fn prekey_reply_shape() {
        // Ответ claim_reply: key_id BE + pubkey 32 = 36 байт ровно.
        let mut p = 7u32.to_be_bytes().to_vec();
        p.extend_from_slice(&[0xDDu8; 32]);
        assert_eq!(parse_prekey(&p).unwrap(), (7, [0xDDu8; 32]));
        assert!(parse_prekey(&p[..35]).is_none());
        assert!(parse_prekey(&[]).is_none());
    }

    #[test]
    fn delivery_ack_trailing_rejected() {
        let mut p = vec![0u8, 1];
        p.extend_from_slice(&5u64.to_be_bytes());
        let mut bad = p.clone();
        bad.extend_from_slice(&[0u8; 4]); // ломает кратность 8
        assert!(parse_delivery_ack(&bad).is_none());
        assert_eq!(parse_delivery_ack(&p).unwrap(), vec![5]);
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

    #[test]
    fn err_busy_code_vector() {
        // Payload-код ERR_BUSY=7 (ERROR=6 — opcode кадра, другое пространство имён).
        assert_eq!(ERR_BUSY, 7);
        assert_ne!(ERR_BUSY, OP_ERROR);
        for c in [ERR_BAD, ERR_EXPIRED, ERR_REVOKED, ERR_BOUND_OTHER, ERR_NO_PREKEY, ERR_QUOTA] {
            assert_ne!(c, ERR_BUSY);
        }
    }
}

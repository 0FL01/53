use dmsg_srtp_sys::{
    Receiver, Sender, AUTH_TAG_BYTES, KEY_MATERIAL_BYTES, MAX_PLAINTEXT_BYTES, MAX_SRTCP_BYTES,
    MAX_SRTP_BYTES, MIN_RTCP_BYTES, MIN_RTP_BYTES, SRTCP_TRAILER_BYTES, SRTP_TRAILER_BYTES,
};

// Public deterministic test fixtures, never production key material.
const KEY: [u8; KEY_MATERIAL_BYTES] = [0x42; KEY_MATERIAL_BYTES];
const NEXT_KEY: [u8; KEY_MATERIAL_BYTES] = [0xa5; KEY_MATERIAL_BYTES];
const SSRC: u32 = 0x1234_abcd;

fn rtp(sequence: u16) -> Vec<u8> {
    let mut packet = vec![0x80, 111];
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(&(u32::from(sequence) * 1920).to_be_bytes());
    packet.extend_from_slice(&SSRC.to_be_bytes());
    packet.extend_from_slice(&[0x78, 0x12, 0x34, 0x56, 0x90]);
    packet
}

fn report(sender_report: bool) -> Vec<u8> {
    // One full reception report block; SR includes the complete sender info.
    let size = if sender_report { 52 } else { 32 };
    let mut packet = vec![0x81, if sender_report { 200 } else { 201 }];
    packet.extend_from_slice(&((size / 4 - 1) as u16).to_be_bytes());
    packet.extend_from_slice(&SSRC.to_be_bytes());
    if sender_report {
        packet.extend_from_slice(&[0x21; 20]);
    }
    packet.extend_from_slice(&[0x56; 24]);
    assert_eq!(packet.len(), size);
    packet
}

fn compound(sender_report: bool) -> Vec<u8> {
    let mut packet = report(sender_report);
    // SDES: one SSRC, one 8-octet printable CNAME, END, word padding (20B).
    packet.extend_from_slice(&[0x81, 202, 0, 4]);
    packet.extend_from_slice(&SSRC.to_be_bytes());
    packet.extend_from_slice(&[1, 8]);
    packet.extend_from_slice(b"dmsgtest");
    packet.extend_from_slice(&[0, 0]);
    // APP: 4B header + SSRC + name + 32B body (44B), protected with SR/RR+SDES.
    packet.extend_from_slice(&[0x80, 204, 0, 10]);
    packet.extend_from_slice(&SSRC.to_be_bytes());
    packet.extend_from_slice(b"M1V0");
    packet.extend_from_slice(&[0x76; 32]);
    packet
}

#[test]
fn rtp_roundtrip_has_exact_80_bit_auth_trailer() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let plaintext = rtp(9);
    let protected = tx.protect_rtp(&plaintext).unwrap();
    assert_eq!(AUTH_TAG_BYTES, 10);
    assert_eq!(protected.len(), plaintext.len() + SRTP_TRAILER_BYTES);
    assert_eq!(&protected[..12], &plaintext[..12]);
    assert_ne!(&protected[12..plaintext.len()], &plaintext[12..]);
    assert_eq!(rx.unprotect_rtp(&protected).unwrap(), plaintext);
}

#[test]
fn srtcp_full_sr_and_rr_sdes_app_compounds_protected_once() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    for (sender_report, index) in [(true, 1u32), (false, 2)] {
        let plaintext = compound(sender_report);
        let protected = tx.protect_rtcp(&plaintext).unwrap();
        assert_eq!(protected.len(), plaintext.len() + SRTCP_TRAILER_BYTES);
        assert_eq!(&protected[..8], &plaintext[..8]);
        assert_ne!(&protected[8..plaintext.len()], &plaintext[8..]);
        let trailer = u32::from_be_bytes(
            protected[plaintext.len()..plaintext.len() + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(trailer, 0x8000_0000 | index);
        assert_eq!(rx.unprotect_rtcp(&protected).unwrap(), plaintext);
    }
}

#[test]
fn rtp_auth_tamper_and_replay_rejected_without_poisoning_valid_packet() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let plaintext = rtp(3);
    let protected = tx.protect_rtp(&plaintext).unwrap();
    for offset in [4, 12, protected.len() - 1] {
        let mut tampered = protected.clone();
        tampered[offset] ^= 1;
        assert_eq!(
            rx.unprotect_rtp(&tampered).unwrap_err(),
            "SRTP authentication failed"
        );
    }
    assert_eq!(rx.unprotect_rtp(&protected).unwrap(), plaintext);
    assert_eq!(
        rx.unprotect_rtp(&protected).unwrap_err(),
        "SRTP replay rejected"
    );
    assert_eq!(
        tx.protect_rtp(&plaintext).unwrap_err(),
        "SRTP replay rejected"
    );
}

#[test]
fn srtcp_auth_tamper_and_replay_rejected_without_poisoning_valid_packet() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let plaintext = compound(true);
    let protected = tx.protect_rtcp(&plaintext).unwrap();
    // Tamper the encrypted SR, SDES, APP, encryption index, and auth tag.
    for offset in [8, 55, 99, plaintext.len() + 3, protected.len() - 1] {
        let mut tampered = protected.clone();
        tampered[offset] ^= 1;
        assert_eq!(
            rx.unprotect_rtcp(&tampered).unwrap_err(),
            "SRTP authentication failed"
        );
    }
    assert_eq!(rx.unprotect_rtcp(&protected).unwrap(), plaintext);
    assert_eq!(
        rx.unprotect_rtcp(&protected).unwrap_err(),
        "SRTP replay rejected"
    );
    // Repeated RTCP contents get distinct library-owned indices, not RTP seq.
    let next = tx.protect_rtcp(&plaintext).unwrap();
    assert_ne!(next, protected);
    assert_eq!(rx.unprotect_rtcp(&next).unwrap(), plaintext);
}

#[test]
fn exact_ssrc_rejected_in_both_directions_for_rtp_and_rtcp() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut other_rx = Receiver::new(&KEY, SSRC ^ 1).unwrap();
    let media = rtp(1);
    let feedback = compound(false);
    assert_eq!(
        other_rx
            .unprotect_rtp(&tx.protect_rtp(&media).unwrap())
            .unwrap_err(),
        "SRTP unexpected SSRC"
    );
    assert_eq!(
        other_rx
            .unprotect_rtcp(&tx.protect_rtcp(&feedback).unwrap())
            .unwrap_err(),
        "SRTP unexpected SSRC"
    );
    let mut wrong_media = media;
    wrong_media[8] ^= 1;
    let mut wrong_feedback = feedback;
    wrong_feedback[4] ^= 1;
    assert_eq!(
        tx.protect_rtp(&wrong_media).unwrap_err(),
        "SRTP unexpected SSRC"
    );
    assert_eq!(
        tx.protect_rtcp(&wrong_feedback).unwrap_err(),
        "SRTP unexpected SSRC"
    );
}

#[test]
fn sequence_rollover_uses_roc_and_rejects_duplicates_from_both_sides() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let mut packets = Vec::new();
    for sequence in [65533, 65534, 65535, 0, 1, 2] {
        let plain = rtp(sequence);
        let protected = tx.protect_rtp(&plain).unwrap();
        assert_eq!(rx.unprotect_rtp(&protected).unwrap(), plain);
        packets.push(protected);
    }
    for protected in packets {
        assert_eq!(
            rx.unprotect_rtp(&protected).unwrap_err(),
            "SRTP replay rejected"
        );
    }
    // Same key/seq/header/payload, but ROC=1 ciphertext differs from ROC=0.
    let mut initial_roc_tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rollover_tx = Sender::new(&KEY, SSRC).unwrap();
    rollover_tx.protect_rtp(&rtp(65535)).unwrap();
    assert_ne!(
        initial_roc_tx.protect_rtp(&rtp(0)).unwrap(),
        rollover_tx.protect_rtp(&rtp(0)).unwrap()
    );
}

#[test]
fn replay_window_accepts_unseen_reordering_then_rejects_old_packet() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let old = tx.protect_rtp(&rtp(1)).unwrap();
    let two = tx.protect_rtp(&rtp(2)).unwrap();
    let three = tx.protect_rtp(&rtp(3)).unwrap();
    assert_eq!(rx.unprotect_rtp(&three).unwrap(), rtp(3));
    assert_eq!(rx.unprotect_rtp(&two).unwrap(), rtp(2));
    for sequence in 4..=140 {
        let packet = tx.protect_rtp(&rtp(sequence)).unwrap();
        assert_eq!(rx.unprotect_rtp(&packet).unwrap(), rtp(sequence));
    }
    assert_eq!(rx.unprotect_rtp(&old).unwrap_err(), "SRTP replay too old");
}

#[test]
fn fresh_generation_key_context_rejects_old_rtp_and_srtcp() {
    let mut old_tx = Sender::new(&KEY, SSRC).unwrap();
    let old_rtp = old_tx.protect_rtp(&rtp(1)).unwrap();
    let old_rtcp = old_tx.protect_rtcp(&compound(false)).unwrap();
    let mut fresh_rx = Receiver::new(&NEXT_KEY, SSRC).unwrap();
    assert_eq!(
        fresh_rx.unprotect_rtp(&old_rtp).unwrap_err(),
        "SRTP authentication failed"
    );
    assert_eq!(
        fresh_rx.unprotect_rtcp(&old_rtcp).unwrap_err(),
        "SRTP authentication failed"
    );
    let mut fresh_tx = Sender::new(&NEXT_KEY, SSRC).unwrap();
    assert_eq!(
        fresh_rx
            .unprotect_rtp(&fresh_tx.protect_rtp(&rtp(1)).unwrap())
            .unwrap(),
        rtp(1)
    );
    assert_eq!(
        fresh_rx
            .unprotect_rtcp(&fresh_tx.protect_rtcp(&compound(false)).unwrap())
            .unwrap(),
        compound(false)
    );
    // Independently rotating SSRC also rejects the retired generation.
    let mut next_ssrc_rx = Receiver::new(&NEXT_KEY, SSRC ^ 1).unwrap();
    assert!(next_ssrc_rx.unprotect_rtp(&old_rtp).is_err());
    assert!(next_ssrc_rx.unprotect_rtcp(&old_rtcp).is_err());
}

#[test]
fn maximum_lengths_and_unaligned_input_slices_roundtrip() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let mut media = rtp(1);
    media.resize(MAX_PLAINTEXT_BYTES, 0x52);
    let mut unaligned = vec![0];
    unaligned.extend_from_slice(&media);
    let protected = tx.protect_rtp(&unaligned[1..]).unwrap();
    assert_eq!(protected.len(), MAX_SRTP_BYTES);
    let mut protected_unaligned = vec![0];
    protected_unaligned.extend_from_slice(&protected);
    assert_eq!(rx.unprotect_rtp(&protected_unaligned[1..]).unwrap(), media);

    let mut feedback = compound(false);
    let app_offset = 32 + 20;
    feedback.resize(MAX_PLAINTEXT_BYTES, 0x6a);
    let app_words_minus_one = ((MAX_PLAINTEXT_BYTES - app_offset) / 4 - 1) as u16;
    feedback[app_offset + 2..app_offset + 4].copy_from_slice(&app_words_minus_one.to_be_bytes());
    let protected = tx.protect_rtcp(&feedback).unwrap();
    assert_eq!(protected.len(), MAX_SRTCP_BYTES);
    assert_eq!(rx.unprotect_rtcp(&protected).unwrap(), feedback);
    media.push(0);
    feedback.extend_from_slice(&[0; 4]);
    assert!(tx.protect_rtp(&media).is_err());
    assert!(tx.protect_rtcp(&feedback).is_err());
    assert!(rx.unprotect_rtp(&vec![0; MAX_SRTP_BYTES + 1]).is_err());
    assert!(rx.unprotect_rtcp(&vec![0; MAX_SRTCP_BYTES + 1]).is_err());
}

#[test]
fn csrc_extensions_and_encrypted_padding_roundtrip() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    let mut media = rtp(1);
    media[0] = 0xb1; // version 2, padding, extension, one CSRC.
    media.splice(12..12, [0, 1, 2, 3, 0xbe, 0xde, 0, 1, 0x12, 0x45, 0, 0]);
    media.extend_from_slice(&[0, 0, 0, 4]);
    let protected = tx.protect_rtp(&media).unwrap();
    assert_eq!(rx.unprotect_rtp(&protected).unwrap(), media);

    let mut feedback = compound(true);
    let app_offset = 52 + 20;
    feedback[app_offset] |= 0x20;
    feedback[app_offset + 3] = 11;
    feedback.extend_from_slice(&[0, 0, 0, 4]);
    let protected = tx.protect_rtcp(&feedback).unwrap();
    assert_eq!(rx.unprotect_rtcp(&protected).unwrap(), feedback);
}

#[test]
fn invalid_lengths_headers_extensions_and_compounds_do_not_panic() {
    let mut tx = Sender::new(&KEY, SSRC).unwrap();
    let mut rx = Receiver::new(&KEY, SSRC).unwrap();
    for length in 0..MIN_RTP_BYTES {
        assert!(tx.protect_rtp(&vec![0; length]).is_err());
    }
    for length in 0..MIN_RTP_BYTES + SRTP_TRAILER_BYTES {
        assert!(rx.unprotect_rtp(&vec![0; length]).is_err());
    }
    for length in 0..MIN_RTCP_BYTES {
        assert!(tx.protect_rtcp(&vec![0; length]).is_err());
    }
    for length in 0..MIN_RTCP_BYTES + SRTCP_TRAILER_BYTES {
        assert!(rx.unprotect_rtcp(&vec![0; length]).is_err());
    }
    for header in [0, 0x40, 0xc0, 0x8f, 0x90, 0xa0] {
        let mut media = rtp(1);
        media[0] = header;
        assert!(tx.protect_rtp(&media).is_err());
    }
    let mut extension = rtp(1);
    extension[0] = 0x90;
    extension[14..16].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(tx.protect_rtp(&extension).is_err());
    for offset in [0, 1, 2, 3, 32, 33, 34, 35, 52, 53, 54, 55] {
        let mut feedback = compound(false);
        feedback[offset] = 0xff;
        assert!(
            tx.protect_rtcp(&feedback).is_err(),
            "malformed at offset {offset}"
        );
    }
    let feedback = compound(false);
    for length in 8..feedback.len() {
        if length == 32 || length == 52 {
            continue; // Complete SR/RR or SR/RR+SDES structural boundaries.
        }
        assert!(tx.protect_rtcp(&feedback[..length]).is_err());
    }
    // Truncated ciphertext headers cannot use auth trailer bytes as extension.
    let protected = tx.protect_rtp(&rtp(1)).unwrap();
    let mut invalid_csrc = protected.clone();
    invalid_csrc[0] = 0x8f;
    assert!(rx.unprotect_rtp(&invalid_csrc).is_err());
    let mut invalid_extension = protected;
    invalid_extension[0] = 0x90;
    invalid_extension[14..16].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(rx.unprotect_rtp(&invalid_extension).is_err());
}

#[test]
fn independent_sessions_initialize_and_move_across_threads() {
    let start = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|offset| {
            let ssrc = SSRC + offset;
            let start = start.clone();
            // Constructors race behind OnceLock; per-session operations stay exclusive.
            std::thread::spawn(move || {
                start.wait();
                let mut tx = Sender::new(&KEY, ssrc).unwrap();
                let rx = Receiver::new(&KEY, ssrc).unwrap();
                let mut plain = rtp(1);
                plain[8..12].copy_from_slice(&ssrc.to_be_bytes());
                let encrypted = tx.protect_rtp(&plain).unwrap();
                std::thread::spawn(move || {
                    let mut rx = rx;
                    assert_eq!(rx.unprotect_rtp(&encrypted).unwrap(), plain);
                    drop(tx); // Ownership/destruction may also cross thread boundaries.
                })
                .join()
                .unwrap();
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

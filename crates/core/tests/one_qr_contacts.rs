//! Real server, only A scans B. Consent/deferred delivery/restart/reverse text.
mod support;
use dmsg_core::history::DeliveryState;
use dmsg_core::{contacts, Core};
use support::*;

fn qr(c: &Core) -> String {
    let (u, id) = c.my_account().unwrap();
    let (e, v) = c.identity_keys();
    contacts::build_qr(&id, &u, &c.device_pub(), &e, &v).unwrap()
}

#[tokio::test]
async fn blocked_id_without_prior_qr_remains_terminal_and_acknowledges_drop() {
    let srv = LiveMsgd::start("one-qr-preblock", "preblock.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let ea = srv.signup(&da, "alice").await;
    let eb = srv.signup(&db, "bobby").await;
    let mut a = Core::open(&da).unwrap();
    let mut b = Core::open(&db).unwrap();
    let mut ta = srv.connect(&da).await;
    let mut tb = srv.connect(&db).await;
    b.block_contact(&ea.contact_id).unwrap();
    b.on_reconnect(&mut tb).await.unwrap();
    contacts::invite_from_qr(&dmsg_core::store::open(&da).unwrap(), &qr(&b)).unwrap();
    a.on_reconnect(&mut ta).await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    let mid = a
        .send_text(&mut ta, &eb.contact_id, "explicitly blocked")
        .await
        .unwrap();
    let dropped = b.fetch_and_decrypt(&mut tb).await.unwrap();
    assert!(dropped.received.is_empty());
    assert_eq!(dropped.skipped_blocked, 1);
    assert_eq!(dropped.skipped_unknown, 0);
    let c = contacts::get(&dmsg_core::store::open(&db).unwrap(), &ea.contact_id)
        .unwrap()
        .unwrap();
    assert_eq!(c.state, "blocked");
    assert!(c.user_id.is_some() && c.ed_identity.is_none() && c.curve_identity.is_none());
    assert!(b.accept_contact(&ea.contact_id).is_err());
    a.retry_queued(&mut ta).await.unwrap();
    let mid = mid.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        a.message_status(&mid).unwrap(),
        Some(DeliveryState::Delivered)
    );
}

#[tokio::test]
async fn simultaneous_first_sends_and_reopen_preserve_both_ratchets() {
    let srv = LiveMsgd::start("one-qr-race", "race.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let ea = srv.signup(&da, "alice").await;
    let eb = srv.signup(&db, "bobby").await;
    let mut a = Core::open(&da).unwrap();
    let mut b = Core::open(&db).unwrap();
    let mut ta = srv.connect(&da).await;
    let mut tb = srv.connect(&db).await;
    b.on_reconnect(&mut tb).await.unwrap();
    contacts::invite_from_qr(&dmsg_core::store::open(&da).unwrap(), &qr(&b)).unwrap();
    a.on_reconnect(&mut ta).await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    b.accept_contact(&ea.contact_id).unwrap();
    // Neither receiver has yet established an inbound session.
    let am = a
        .send_text(&mut ta, &eb.contact_id, "simultaneous A")
        .await
        .unwrap();
    let bm = b
        .send_text(&mut tb, &ea.contact_id, "simultaneous B")
        .await
        .unwrap();
    let ac = a.outbox_ciphertext(&am).unwrap();
    let bc = b.outbox_ciphertext(&bm).unwrap();
    assert_eq!(
        a.fetch_and_decrypt(&mut ta).await.unwrap().received.len(),
        1
    );
    assert_eq!(
        b.fetch_and_decrypt(&mut tb).await.unwrap().received.len(),
        1
    );
    drop(a);
    drop(b);
    a = Core::open(&da).unwrap();
    b = Core::open(&db).unwrap();
    // Both retained sessions survive reopen, and the storage bound fails closed.
    let conn = dmsg_core::store::open(&da).unwrap();
    let pickle = dmsg_core::store::load_session(&conn, &eb.contact_id)
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(dmsg_core::olm::unpickle_sessions(&pickle).unwrap().len(), 2);
    let mut invalid: serde_json::Value = serde_json::from_str(&pickle).unwrap();
    let extra = invalid["dmsg_receiving_sessions"].as_array_mut().unwrap();
    extra.push(extra[0].clone());
    assert!(dmsg_core::olm::unpickle_sessions(&invalid.to_string()).is_err());
    for round in 0..3 {
        let next_a = a
            .send_text(&mut ta, &eb.contact_id, "next A")
            .await
            .unwrap();
        let next_b = b
            .send_text(&mut tb, &ea.contact_id, "next B")
            .await
            .unwrap();
        if round > 0 {
            // Deterministic convergence avoids sending prekeys indefinitely.
            for wire in [
                a.outbox_ciphertext(&next_a).unwrap(),
                b.outbox_ciphertext(&next_b).unwrap(),
            ] {
                assert!(matches!(
                    dmsg_core::olm::decode_wire(&wire).unwrap(),
                    vodozemac::olm::OlmMessage::Normal(_)
                ));
            }
        }
        assert_eq!(
            a.fetch_and_decrypt(&mut ta).await.unwrap().received.len(),
            1
        );
        assert_eq!(
            b.fetch_and_decrypt(&mut tb).await.unwrap().received.len(),
            1
        );
    }
    a.retry_queued(&mut ta).await.unwrap();
    b.retry_queued(&mut tb).await.unwrap();
    assert_eq!(a.outbox_ciphertext(&am).unwrap(), ac);
    assert_eq!(b.outbox_ciphertext(&bm).unwrap(), bc);
    assert!(a
        .fetch_and_decrypt(&mut ta)
        .await
        .unwrap()
        .received
        .is_empty());
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
}

#[tokio::test]
async fn one_qr_request_survives_reopen_and_first_text_waits_for_acceptance() {
    let srv = LiveMsgd::start("one-qr", "one-qr.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let ea = srv.signup(&da, "alice").await;
    let eb = srv.signup(&db, "bobby").await;
    let mut a = Core::open(&da).unwrap();
    let mut b = Core::open(&db).unwrap();
    contacts::invite_from_qr(&dmsg_core::store::open(&da).unwrap(), &qr(&b)).unwrap();
    drop(a);
    a = Core::open(&da).unwrap();
    let mut ta = srv.connect(&da).await;
    let mut tb = srv.connect(&db).await;
    a.on_reconnect(&mut ta).await.unwrap();
    assert_eq!(
        contacts::get(&dmsg_core::store::open(&da).unwrap(), &eb.contact_id)
            .unwrap()
            .unwrap()
            .state,
        "accepted"
    );
    // A newly registered receiver can show its QR before its first poll.
    // The request must reach it already, but text still requires published
    // prekeys and may not be stored as a plaintext/offline first message.
    assert_eq!(
        a.send_text(&mut ta, &eb.contact_id, "not queued without prekeys")
            .await,
        Err(dmsg_core::olm::OlmError::NoPeerPrekeys)
    );
    assert!(a
        .history_page(&eb.contact_id, None, 100)
        .unwrap()
        .rows
        .is_empty());
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
    let incoming = contacts::get(&dmsg_core::store::open(&db).unwrap(), &ea.contact_id)
        .unwrap()
        .unwrap();
    assert_eq!(incoming.state, "incoming");
    assert!(contacts::sendable(&incoming).is_err());
    let mid = a
        .send_text(&mut ta, &eb.contact_id, "first text before consent")
        .await
        .unwrap();
    let cipher = a.outbox_ciphertext(&mid).unwrap();
    let midhex = mid.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let pending = b.fetch_and_decrypt(&mut tb).await.unwrap();
    assert!(pending.received.is_empty());
    assert_eq!(pending.skipped_unknown, 1);
    assert_eq!(pending.cursor, 0);
    a.retry_queued(&mut ta).await.unwrap();
    assert_eq!(
        a.message_status(&midhex).unwrap(),
        Some(DeliveryState::Accepted)
    );
    drop(b);
    b = Core::open(&db).unwrap();
    b.accept_contact(&ea.contact_id).unwrap();
    assert_eq!(
        contacts::get(&dmsg_core::store::open(&db).unwrap(), &ea.contact_id)
            .unwrap()
            .unwrap()
            .state,
        "accepted_server"
    );
    let delivered = b.fetch_and_decrypt(&mut tb).await.unwrap();
    assert_eq!(delivered.received.len(), 1);
    assert_eq!(delivered.received[0].text, "first text before consent");
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
    a.retry_queued(&mut ta).await.unwrap();
    assert_eq!(
        a.message_status(&midhex).unwrap(),
        Some(DeliveryState::Delivered)
    );
    assert_eq!(a.outbox_ciphertext(&mid).unwrap(), cipher);
    b.send_text(&mut tb, &ea.contact_id, "reply without reverse QR")
        .await
        .unwrap();
    let r = a.fetch_and_decrypt(&mut ta).await.unwrap();
    assert_eq!(r.received.len(), 1);
    assert_eq!(r.received[0].text, "reply without reverse QR");
}

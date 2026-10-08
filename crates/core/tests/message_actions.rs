//! Fresh encrypted cores, the real msgd, and the same durable actions used by Android.
mod support;

use dmsg_core::history::{self, DeleteScope, DeliveryState, MessageDirection, ReplyTargetState};
use dmsg_core::transport::{Transport, TransportError};
use dmsg_core::{contacts, Core};
use support::LiveMsgd;

fn qr(core: &Core) -> String {
    let (user, id) = core.my_account().unwrap();
    let (ed, curve) = core.identity_keys();
    contacts::build_qr(&id, &user, &core.device_pub(), &ed, &curve).unwrap()
}

struct Observe<'a> {
    inner: &'a mut dmsg_core::DirectTcp,
    sends: Vec<Vec<u8>>,
}
impl Transport for Observe<'_> {
    async fn connect(&mut self) -> Result<(), TransportError> {
        self.inner.connect().await
    }
    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), TransportError> {
        if opcode == dmsg_protocol::OP_SEND {
            self.sends.push(payload.to_vec());
        }
        self.inner.send_frame(opcode, payload).await
    }
    async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
        self.inner.recv_frame().await
    }
    async fn close(&mut self) {
        self.inner.close().await
    }
    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }
}

#[tokio::test]
async fn replied_text_is_shallow_current_and_immutable_across_controls_reopen_and_live_retry() {
    let srv = LiveMsgd::start("message-replies", "replies.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let aa = srv.signup_keyed(&da, "alice", Some(&[17; 32])).await;
    let ba = srv.signup_keyed(&db, "bobby", Some(&[18; 32])).await;
    let mut a = Core::open_encrypted(&da, &[17; 32]).unwrap();
    let mut b = Core::open_encrypted(&db, &[18; 32]).unwrap();
    a.add_contact_qr(&qr(&b)).unwrap();
    b.add_contact_qr(&qr(&a)).unwrap();
    a.accept_contact(&ba.contact_id).unwrap();
    b.accept_contact(&aa.contact_id).unwrap();
    let mut ta = srv.connect_keyed(&da, Some(&[17; 32])).await;
    let mut tb = srv.connect_keyed(&db, Some(&[18; 32])).await;
    a.on_reconnect(&mut ta).await.unwrap();
    b.on_reconnect(&mut tb).await.unwrap();
    let original = a
        .send_text(&mut ta, &ba.contact_id, "grandparent body", None)
        .await
        .unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    let incoming = b.history_page(&aa.contact_id, None, 1).unwrap().rows[0].clone();
    let target_mid = b
        .send_text(
            &mut tb,
            &aa.contact_id,
            "  **parent body**\r\n",
            Some(incoming.local_id),
        )
        .await
        .unwrap();
    a.fetch_and_decrypt(&mut ta).await.unwrap();
    let target_a = a.history_page(&ba.contact_id, None, 1).unwrap().rows[0].clone();
    let target_b = b.history_page(&aa.contact_id, None, 1).unwrap().rows[0].clone();
    assert_eq!(target_a.reply.as_ref().unwrap().preview, "grandparent body");
    let mid = a
        .send_text(
            &mut ta,
            &ba.contact_id,
            "**reply body**",
            Some(target_a.local_id),
        )
        .await
        .unwrap();
    let source = a.history_page(&ba.contact_id, None, 1).unwrap().rows[0].clone();
    let quote = source.reply.as_ref().unwrap();
    assert_eq!(quote.direction, Some(MessageDirection::Incoming));
    assert_eq!(quote.preview, "  **parent body**\r\n");
    let ac = dmsg_core::store::open_encrypted(&da, &[17; 32]).unwrap();
    let ref_bytes: Vec<u8> = ac
        .query_row(
            "SELECT reply_ref FROM core_messages WHERE local_id=?1",
            [source.local_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        ref_bytes,
        dmsg_protocol::e2e::ReplyRef {
            sender_device: b.device_pub(),
            message_id: target_mid
        }
        .encode()
    );
    let cipher = a.outbox_ciphertext(&mid).unwrap();
    drop(a);
    let mut a = Core::open_encrypted(&da, &[17; 32]).unwrap();
    assert_eq!(
        history::history_message(&ac, &ba.contact_id, source.local_id).unwrap(),
        source
    );
    let mut observed = Observe {
        inner: &mut ta,
        sends: vec![],
    };
    a.retry_queued(&mut observed).await.unwrap();
    a.retry_queued(&mut observed).await.unwrap();
    let mut payload = ba.user_id.to_vec();
    payload.extend_from_slice(&mid);
    payload.extend_from_slice(&cipher);
    assert!(observed.sends.iter().any(|p| p == &payload));
    assert_eq!(
        observed
            .sends
            .iter()
            .filter(|p| p[16..32] == mid)
            .collect::<Vec<_>>(),
        vec![&payload, &payload]
    );
    drop(observed);
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    a.retry_queued(&mut ta).await.unwrap();
    let peer_source = b.history_page(&aa.contact_id, None, 1).unwrap().rows[0].clone();
    assert_eq!(
        peer_source.reply.as_ref().unwrap().direction,
        Some(MessageDirection::Outgoing)
    );
    b.edit_message(
        &aa.contact_id,
        target_b.local_id,
        0,
        "  ## current parent\n`raw`  ",
    )
    .unwrap();
    b.retry_queued(&mut tb).await.unwrap();
    a.fetch_and_decrypt(&mut ta).await.unwrap();
    let updated = history::history_message(&ac, &ba.contact_id, source.local_id).unwrap();
    assert_eq!(updated.reply.as_ref().unwrap().target_revision, Some(1));
    assert_eq!(
        updated.reply.as_ref().unwrap().preview,
        "  ## current parent\n`raw`  "
    );
    a.edit_message(
        &ba.contact_id,
        source.local_id,
        0,
        "edited reply's own body",
    )
    .unwrap();
    a.retry_queued(&mut ta).await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    let bc = dmsg_core::store::open_encrypted(&db, &[18; 32]).unwrap();
    assert_eq!(
        history::history_message(&bc, &aa.contact_id, peer_source.local_id)
            .unwrap()
            .reply
            .as_ref()
            .unwrap()
            .target_local_id,
        Some(target_b.local_id)
    );
    a.delete_message(&ba.contact_id, source.local_id, DeleteScope::Everyone)
        .unwrap();
    a.retry_queued(&mut ta).await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    b.delete_message(&aa.contact_id, target_b.local_id, DeleteScope::Everyone)
        .unwrap();
    b.retry_queued(&mut tb).await.unwrap();
    a.fetch_and_decrypt(&mut ta).await.unwrap();
    for (conn, cid, id, target_id) in [
        (&ac, &ba.contact_id, source.local_id, target_a.local_id),
        (&bc, &aa.contact_id, peer_source.local_id, target_b.local_id),
    ] {
        let row = history::history_message(conn, cid, id).unwrap();
        assert!(row.deleted_all);
        assert!(row.text.is_empty());
        let q = row.reply.unwrap();
        assert_eq!(q.state, ReplyTargetState::Deleted);
        assert_eq!(q.target_local_id, Some(target_id));
        assert_eq!(q.target_revision, Some(2));
        assert!(q.preview.is_empty());
        assert_eq!(q.voice_duration_ms, None);
        assert_eq!(
            conn.query_row::<Vec<u8>, _, _>(
                "SELECT reply_ref FROM core_messages WHERE local_id=?1",
                [id],
                |r| r.get(0)
            )
            .unwrap(),
            ref_bytes
        );
        assert_eq!(conn.query_row::<i64,_,_>("SELECT count(*) FROM core_messages WHERE kind IN ('edit','delete') AND reply_ref IS NOT NULL",[],|r|r.get(0)).unwrap(),0);
    }
    assert_eq!(a.outbox_ciphertext(&mid).unwrap(), cipher);
    assert!(!a.outbox_ciphertext(&original).unwrap().is_empty());
    drop(a);
    drop(b);
    let a = Core::open_encrypted(&da, &[17; 32]).unwrap();
    let b = Core::open_encrypted(&db, &[18; 32]).unwrap();
    assert_eq!(
        a.history_page(&ba.contact_id, None, 100)
            .unwrap()
            .rows
            .len(),
        3
    );
    assert_eq!(
        b.history_page(&aa.contact_id, None, 100)
            .unwrap()
            .rows
            .len(),
        3
    );
    assert_eq!(
        history::history_message(&ac, &ba.contact_id, source.local_id)
            .unwrap()
            .reply
            .unwrap()
            .state,
        ReplyTargetState::Deleted
    );
}

#[tokio::test]
async fn text_edit_self_hide_and_everyone_delete_retry_through_live_msgd() {
    let srv = LiveMsgd::start("message-actions", "actions.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let aa = srv.signup_keyed(&da, "alice", Some(&[17; 32])).await;
    let ba = srv.signup_keyed(&db, "bobby", Some(&[18; 32])).await;
    let mut a = Core::open_encrypted(&da, &[17; 32]).unwrap();
    let mut b = Core::open_encrypted(&db, &[18; 32]).unwrap();
    a.add_contact_qr(&qr(&b)).unwrap();
    b.add_contact_qr(&qr(&a)).unwrap();
    a.accept_contact(&ba.contact_id).unwrap();
    b.accept_contact(&aa.contact_id).unwrap();
    let mut ta = srv.connect_keyed(&da, Some(&[17; 32])).await;
    let mut tb = srv.connect_keyed(&db, Some(&[18; 32])).await;
    a.on_reconnect(&mut ta).await.unwrap();
    b.on_reconnect(&mut tb).await.unwrap();

    let boundary_source = |prefix: &str, scalar: &str| {
        format!(
            "{prefix}{}",
            scalar.repeat(dmsg_protocol::TEXT_CHAR_MAX - prefix.chars().count())
        )
    };
    let source = boundary_source(
        "  # original\r\n**bold** _italic_ ~~strike~~ `e\u{301}`\n> quote\n- list\n```\na < b\n```\n[link](https://example.test)\n![alt](image) <b>literal</b>  \n",
        "🦀",
    );
    let replacement = boundary_source(
        "  ## edited\n**raw** `é`\n> сохранённый исходник\n- item\n```\n  spaces\n```\n[link](https://example.test/edit)\n  ",
        "я",
    );
    assert_eq!(source.chars().count(), 4000);
    assert_eq!(replacement.chars().count(), 4000);
    let mid = a
        .send_text(&mut ta, &ba.contact_id, &source, None)
        .await
        .unwrap();
    let ciphertext = a.outbox_ciphertext(&mid).unwrap();
    assert_eq!(
        b.fetch_and_decrypt(&mut tb).await.unwrap().received[0].text,
        source
    );
    a.retry_queued(&mut ta).await.unwrap();
    let before = a
        .history_page(&ba.contact_id, None, 100)
        .unwrap()
        .rows
        .remove(0);
    let received = b
        .history_page(&aa.contact_id, None, 100)
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(before.delivery_state, Some(DeliveryState::Delivered));
    assert_eq!(before.text, source);
    assert_eq!(received.text, source);
    let edit = a
        .edit_message(&ba.contact_id, before.local_id, 0, &replacement)
        .unwrap();
    assert_eq!(edit.text, replacement);
    assert_eq!(edit.change_delivery_state, Some(DeliveryState::Queued));
    assert_eq!(a.outbox_ciphertext(&mid).unwrap(), ciphertext);
    assert_eq!(
        (
            edit.local_id,
            &edit.message_id_hex,
            edit.local_timestamp_ms,
            edit.server_seq,
            edit.server_timestamp_ms
        ),
        (
            before.local_id,
            &before.message_id_hex,
            before.local_timestamp_ms,
            before.server_seq,
            before.server_timestamp_ms
        )
    );
    let ac = dmsg_core::store::open_encrypted(&da, &[17; 32]).unwrap();
    let controls = dmsg_core::store::outbox_queued(&ac, 0, 100).unwrap().0;
    assert_eq!(controls.len(), 1);
    let control_mid = controls[0].1;
    let control_ciphertext = controls[0].3.clone();
    drop(a);
    let mut a = Core::open_encrypted(&da, &[17; 32]).unwrap();
    assert_eq!(
        a.history_page(&ba.contact_id, None, 100).unwrap().rows[0],
        edit
    );
    // Observe the actual live SEND payload on both retries after reopen.
    let mut observed = Observe {
        inner: &mut ta,
        sends: Vec::new(),
    };
    a.retry_queued(&mut observed).await.unwrap();
    a.retry_queued(&mut observed).await.unwrap();
    let mut expected_send = ba.user_id.to_vec();
    expected_send.extend_from_slice(&control_mid);
    expected_send.extend_from_slice(&control_ciphertext);
    assert_eq!(observed.sends, vec![expected_send.clone(), expected_send]);
    drop(observed);
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
    a.retry_queued(&mut ta).await.unwrap();
    assert_eq!(
        a.outbox_ciphertext(&control_mid).unwrap(),
        control_ciphertext
    );
    drop(b);
    let mut b = Core::open_encrypted(&db, &[18; 32]).unwrap();
    let bc = dmsg_core::store::open_encrypted(&db, &[18; 32]).unwrap();
    let current = history::history_message(&ac, &ba.contact_id, before.local_id).unwrap();
    assert_eq!(current.text, replacement);
    assert_eq!(current.message_id_hex, before.message_id_hex);
    assert_eq!(a.outbox_ciphertext(&mid).unwrap(), ciphertext);
    assert_eq!(
        current.change_delivery_state,
        Some(DeliveryState::Delivered)
    );
    let peer = history::history_message(&bc, &aa.contact_id, received.local_id).unwrap();
    assert_eq!(peer.text, replacement);
    assert_eq!(
        (
            peer.local_id,
            &peer.message_id_hex,
            peer.local_timestamp_ms,
            peer.server_seq,
            peer.server_timestamp_ms
        ),
        (
            received.local_id,
            &received.message_id_hex,
            received.local_timestamp_ms,
            received.server_seq,
            received.server_timestamp_ms
        )
    );

    a.delete_message(&ba.contact_id, before.local_id, DeleteScope::SelfOnly)
        .unwrap();
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
    assert_eq!(
        history::history_message(&bc, &aa.contact_id, received.local_id)
            .unwrap()
            .text,
        replacement
    );
    assert!(
        history::history_message(&ac, &ba.contact_id, before.local_id)
            .unwrap()
            .hidden_self
    );
    let second = a
        .send_text(&mut ta, &ba.contact_id, "remove both", None)
        .await
        .unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    let target = a
        .history_page(&ba.contact_id, None, 1)
        .unwrap()
        .rows
        .remove(0);
    let original = a.outbox_ciphertext(&second).unwrap();
    let tombstone = a
        .delete_message(&ba.contact_id, target.local_id, DeleteScope::Everyone)
        .unwrap();
    assert!(tombstone.deleted_all && tombstone.text.is_empty());
    assert_eq!(tombstone.change_delivery_state, Some(DeliveryState::Queued));
    drop(a);
    let mut a = Core::open_encrypted(&da, &[17; 32]).unwrap();
    a.retry_queued(&mut ta).await.unwrap();
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
    a.retry_queued(&mut ta).await.unwrap();
    let row = b
        .history_page(&aa.contact_id, None, 1)
        .unwrap()
        .rows
        .remove(0);
    assert!(row.deleted_all && row.text.is_empty());
    assert_eq!(row.message_id_hex, target.message_id_hex);
    assert_eq!(a.outbox_ciphertext(&second).unwrap(), original);
    assert_eq!(
        history::history_message(&ac, &ba.contact_id, target.local_id)
            .unwrap()
            .change_delivery_state,
        Some(DeliveryState::Delivered)
    );
    assert!(dmsg_core::store::inbox_list(&bc, 0, 100)
        .unwrap()
        .0
        .iter()
        .all(|(_, _, text)| text != "remove both"));
    assert_eq!(
        a.history_page(&ba.contact_id, None, 100)
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        b.history_page(&aa.contact_id, None, 100)
            .unwrap()
            .rows
            .len(),
        2
    );
}

//! Fresh encrypted cores, the real msgd, and the same durable actions used by Android.
mod support;

use dmsg_core::history::{self, DeleteScope, DeliveryState};
use dmsg_core::{contacts, Core};
use support::LiveMsgd;

fn qr(core: &Core) -> String {
    let (user, id) = core.my_account().unwrap();
    let (ed, curve) = core.identity_keys();
    contacts::build_qr(&id, &user, &core.device_pub(), &ed, &curve).unwrap()
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

    let mid = a
        .send_text(&mut ta, &ba.contact_id, "original")
        .await
        .unwrap();
    let ciphertext = a.outbox_ciphertext(&mid).unwrap();
    assert_eq!(
        b.fetch_and_decrypt(&mut tb).await.unwrap().received[0].text,
        "original"
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
    let edit = a
        .edit_message(&ba.contact_id, before.local_id, 0, "edited")
        .unwrap();
    assert_eq!(edit.change_delivery_state, Some(DeliveryState::Queued));
    assert_eq!(a.outbox_ciphertext(&mid).unwrap(), ciphertext);
    assert_eq!(
        (edit.local_id, edit.server_seq, edit.server_timestamp_ms),
        (
            before.local_id,
            before.server_seq,
            before.server_timestamp_ms
        )
    );
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
    let ac = dmsg_core::store::open_encrypted(&da, &[17; 32]).unwrap();
    let bc = dmsg_core::store::open_encrypted(&db, &[18; 32]).unwrap();
    let current = history::history_message(&ac, &ba.contact_id, before.local_id).unwrap();
    assert_eq!(
        current.change_delivery_state,
        Some(DeliveryState::Delivered)
    );
    let peer = history::history_message(&bc, &aa.contact_id, received.local_id).unwrap();
    assert_eq!(peer.text, "edited");
    assert_eq!(
        (peer.local_id, peer.server_seq, peer.server_timestamp_ms),
        (
            received.local_id,
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
        "edited"
    );
    assert!(
        history::history_message(&ac, &ba.contact_id, before.local_id)
            .unwrap()
            .hidden_self
    );
    let second = a
        .send_text(&mut ta, &ba.contact_id, "remove both")
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

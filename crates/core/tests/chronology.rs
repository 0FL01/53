//! Confirmed order/time is immutable mailbox acceptance, not queue/decrypt time.
mod support;
use dmsg_core::transport::Transport;
use dmsg_core::{contacts, history, store, Core};
use dmsg_protocol::{chronology as wire, *};
use support::*;
fn qr(c: &Core) -> String {
    let (u, id) = c.my_account().unwrap();
    let (e, v) = c.identity_keys();
    contacts::build_qr(&id, &u, &c.device_pub(), &e, &v).unwrap()
}
fn timeline(path: &std::path::Path, peer: &str) -> Vec<history::HistoryMessage> {
    history::timeline_page(&store::open(path).unwrap(), peer, None, 100)
        .unwrap()
        .rows
        .into_iter()
        .rev()
        .collect()
}
#[tokio::test]
async fn offline_pending_then_server_acceptance_late_receive_retry_and_restart() {
    let srv = LiveMsgd::start("chronology", "chronology.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let dc = srv.dir.join("c.db");
    let aa = srv.signup(&da, "alice").await;
    let bb = srv.signup(&db, "bobby").await;
    srv.signup(&dc, "carol").await;
    let mut a = Core::open(&da).unwrap();
    let mut b = Core::open(&db).unwrap();
    let mut ta = srv.connect(&da).await;
    let mut tb = srv.connect(&db).await;
    b.on_reconnect(&mut tb).await.unwrap();
    contacts::invite_from_qr(&store::open(&da).unwrap(), &qr(&b)).unwrap();
    a.on_reconnect(&mut ta).await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    b.accept_contact(&aa.contact_id).unwrap();
    let seed = a.send_text(&mut ta, &bb.contact_id, "seed").await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    let offline = a
        .queue_text_existing_session(&bb.contact_id, "queued earlier accepted later")
        .unwrap()
        .unwrap();
    let original = a.outbox_ciphertext(&offline).unwrap();
    let other = b
        .send_text(&mut tb, &aa.contact_id, "accepted before offline A")
        .await
        .unwrap();
    a.fetch_and_decrypt(&mut ta).await.unwrap();
    let pending = timeline(&da, &bb.contact_id);
    let hex = |m: &[u8; 16]| m.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(pending.last().unwrap().message_id_hex, hex(&offline));
    assert!(pending.last().unwrap().server_seq.is_none());
    // Device clocks/late decryption dates cannot determine the confirmed order.
    store::open(&da)
        .unwrap()
        .execute(
            "UPDATE core_messages SET local_timestamp_ms=9000000000000",
            [],
        )
        .unwrap();
    store::open(&db)
        .unwrap()
        .execute("UPDATE core_messages SET local_timestamp_ms=1", [])
        .unwrap();
    drop(a);
    a = Core::open(&da).unwrap();
    a.retry_queued(&mut ta).await.unwrap();
    assert_eq!(a.outbox_ciphertext(&offline).unwrap(), original);
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    a.retry_queued(&mut ta).await.unwrap();
    let ah = timeline(&da, &bb.contact_id);
    let bh = timeline(&db, &aa.contact_id);
    let order = |rows: &[history::HistoryMessage]| {
        rows.iter()
            .map(|r| {
                (
                    r.message_id_hex.clone(),
                    r.server_seq.unwrap(),
                    r.server_timestamp_ms.unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&ah), order(&bh));
    assert_eq!(
        ah.iter()
            .map(|r| r.message_id_hex.clone())
            .collect::<Vec<_>>(),
        [hex(&seed), hex(&other), hex(&offline)]
    );
    let before = order(&ah);
    a.retry_queued(&mut ta).await.unwrap();
    b.fetch_and_decrypt(&mut tb).await.unwrap();
    drop(b);
    b = Core::open(&db).unwrap();
    assert_eq!(before, order(&timeline(&da, &bb.contact_id)));
    assert_eq!(before, order(&timeline(&db, &aa.contact_id)));
    assert!(b
        .fetch_and_decrypt(&mut tb)
        .await
        .unwrap()
        .received
        .is_empty());
    // Owner/recipient get identical metadata, a third authenticated account gets
    // exactly the same zero record as a nonexistent MID; malformed lists reject.
    let keys = [(a.device_pub(), offline)];
    let request = wire::build_keys(&keys).unwrap();
    ta.send_frame(OP_MESSAGE_METADATA, &request).await.unwrap();
    let (_, p) = ta.recv_frame().await.unwrap();
    let own = wire::parse_orders(&p, 1).unwrap();
    tb.send_frame(OP_MESSAGE_METADATA, &request).await.unwrap();
    let (_, p) = tb.recv_frame().await.unwrap();
    assert_eq!(wire::parse_orders(&p, 1).unwrap(), own);
    let mut tc = srv.connect(&dc).await;
    Core::open(&dc)
        .unwrap()
        .on_reconnect(&mut tc)
        .await
        .unwrap();
    tc.send_frame(OP_MESSAGE_METADATA, &request).await.unwrap();
    let (op, p) = tc.recv_frame().await.unwrap();
    assert_eq!(op, OP_MESSAGE_METADATA_RESP);
    assert_eq!(wire::parse_orders(&p, 1).unwrap(), vec![None]);
    let p = wire::build_keys(&[(a.device_pub(), [255; 16])]).unwrap();
    tc.send_frame(OP_MESSAGE_METADATA, &p).await.unwrap();
    let (_, p) = tc.recv_frame().await.unwrap();
    assert_eq!(wire::parse_orders(&p, 1).unwrap(), vec![None]);
    tc.send_frame(OP_MESSAGE_METADATA, &[0]).await.unwrap();
    assert_eq!(tc.recv_frame().await.unwrap().0, OP_ERROR);
}

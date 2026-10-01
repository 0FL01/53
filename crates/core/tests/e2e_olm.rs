//! Actual v2 live-server E2E, durable retry/dedup, and peer replacement gates.
mod support;
use dmsg_core::history::{DeliveryState, MessageDirection};
use dmsg_core::{contacts, Core, OlmError};
use support::*;
fn qr(core: &Core) -> String {
    let (u, id) = core.my_account().unwrap();
    let (ed, curve) = core.identity_keys();
    contacts::build_qr(&id, &u, &core.device_pub(), &ed, &curve).unwrap()
}
fn link(a: &Core, b: &Core) {
    a.add_contact_qr(&qr(b)).unwrap();
    b.add_contact_qr(&qr(a)).unwrap();
    a.accept_contact(&b.my_account().unwrap().1).unwrap();
    b.accept_contact(&a.my_account().unwrap().1).unwrap();
}
#[tokio::test]
async fn two_cores_talk_e2e_through_live_msgd() {
    let srv = LiveMsgd::start("e2e", "e2e.test");
    let da = srv.dir.join("a.db");
    let db = srv.dir.join("b.db");
    let ea = srv.signup(&da, "alice").await;
    let eb = srv.signup(&db, "bobby").await;
    let mut a = Core::open(&da).unwrap();
    let mut b = Core::open(&db).unwrap();
    link(&a, &b);
    let mut ta = srv.connect(&da).await;
    let mut tb = srv.connect(&db).await;
    assert_eq!(a.on_reconnect(&mut ta).await.unwrap(), 16);
    assert_eq!(b.on_reconnect(&mut tb).await.unwrap(), 16);
    let mid = a
        .send_text(&mut ta, &eb.contact_id, "hello bob")
        .await
        .unwrap();
    let saved = a.outbox_ciphertext(&mid).unwrap();
    let mid_hex = mid.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        a.message_status(&mid_hex).unwrap(),
        Some(DeliveryState::Accepted)
    );
    assert!(!saved.windows(9).any(|w| w == b"hello bob"));
    let r = b.fetch_and_decrypt(&mut tb).await.unwrap();
    assert_eq!(r.received.len(), 1);
    assert_eq!(r.received[0].text, "hello bob");
    assert_eq!(
        (
            r.skipped_unknown,
            r.skipped_blocked,
            r.skipped_mismatch,
            r.skipped_undecryptable
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(b.on_reconnect(&mut tb).await.unwrap(), 15);
    let stats = a.retry_queued(&mut ta).await.unwrap();
    assert_eq!((stats.resent, stats.delivered), (1, 1));
    assert_eq!(
        a.message_status(&mid_hex).unwrap(),
        Some(DeliveryState::Delivered)
    );
    assert_eq!(b.message_status(&mid_hex).unwrap(), None);
    assert_eq!(saved, a.outbox_ciphertext(&mid).unwrap());
    let empty = b.fetch_and_decrypt(&mut tb).await.unwrap();
    assert!(empty.received.is_empty());
    assert!(empty.cursor >= r.cursor);
    for (text, reverse) in [
        ("hi alice", true),
        ("after reverse gap", false),
        ("after forward gap", true),
    ] {
        let (sender, receiver, send_t, recv_t, id) = if reverse {
            (&mut b, &mut a, &mut tb, &mut ta, &ea.contact_id)
        } else {
            (&mut a, &mut b, &mut ta, &mut tb, &eb.contact_id)
        };
        sender.send_text(send_t, id, text).await.unwrap();
        let r = receiver.fetch_and_decrypt(recv_t).await.unwrap();
        assert_eq!(r.received.len(), 1);
        assert_eq!(r.received[0].text, text);
        assert_eq!(r.skipped_undecryptable, 0);
        assert!(receiver
            .fetch_and_decrypt(recv_t)
            .await
            .unwrap()
            .received
            .is_empty());
    }
    let ah = a.history_page(&eb.contact_id, None, 100).unwrap();
    let bh = b.history_page(&ea.contact_id, None, 100).unwrap();
    for page in [&ah, &bh] {
        assert_eq!(page.rows.len(), 4);
        assert_eq!(
            page.rows
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "after forward gap",
                "after reverse gap",
                "hi alice",
                "hello bob"
            ]
        );
        assert!(page
            .rows
            .windows(2)
            .all(|pair| pair[0].local_id > pair[1].local_id));
        assert!(page.rows.iter().all(|r| r.local_timestamp_ms > 0));
    }
    assert_eq!(ah.rows[0].direction, MessageDirection::Incoming);
    assert_eq!(bh.rows[0].direction, MessageDirection::Outgoing);
    assert_eq!(a.dialogs_page(None, 100).unwrap().rows[0].local_unread, 2);
    assert_eq!(b.dialogs_page(None, 100).unwrap().rows[0].local_unread, 2);
    a.set_contact_alias(&eb.contact_id, Some("Local Bobby"))
        .unwrap();
    a.mark_read(&eb.contact_id, ah.rows[2].local_id).unwrap();
    assert_eq!(a.dialogs_page(None, 100).unwrap().rows[0].local_unread, 1);
    drop(a);
    drop(b);
    // Rust-only facade seam mirrors DNS dispatch without exporting DirectTCP.
    let addr = srv.addr.clone();
    let key = srv.server_pub.to_vec();
    let domain = srv.domain.clone();
    tokio::task::spawn_blocking(move || {
        let a = dmsg_core::ffi::DmsgClient::open(da.to_string_lossy().into());
        let b = dmsg_core::ffi::DmsgClient::open(db.to_string_lossy().into());
        let summary = a.dialogs_page(None, 100).unwrap().rows.remove(0);
        assert_eq!(summary.local_alias.as_deref(), Some("Local Bobby"));
        assert_eq!(summary.local_unread, 1);
        assert_eq!(
            a.history_page(eb.contact_id.clone(), None, 100)
                .unwrap()
                .rows
                .len(),
            4
        );
        assert_eq!(
            b.history_page(ea.contact_id.clone(), None, 100)
                .unwrap()
                .rows
                .len(),
            4
        );
        for _ in 0..2 {
            assert!(
                a.reconnect(addr.clone(), key.clone(), domain.clone())
                    .unwrap()
                    >= 8
            );
            assert!(
                b.reconnect(addr.clone(), key.clone(), domain.clone())
                    .unwrap()
                    >= 8
            );
        }
        a.send_text(
            addr.clone(),
            key.clone(),
            domain.clone(),
            eb.contact_id.clone(),
            "ffi roundtrip".into(),
        )
        .unwrap();
        assert_eq!(
            b.fetch(addr.clone(), key.clone(), domain.clone())
                .unwrap()
                .received[0]
                .text,
            "ffi roundtrip"
        );
        assert!(
            a.retry_queued(addr.clone(), key.clone(), domain.clone())
                .unwrap()
                .delivered
                >= 1
        );
        assert!(b
            .fetch(addr.clone(), key.clone(), domain.clone())
            .unwrap()
            .received
            .is_empty());
        assert_eq!(
            a.account_info().unwrap().contact_id,
            Some(ea.contact_id.clone())
        );
        drop(a);
        drop(b);
        let a =
            dmsg_core::ffi::DmsgClient::open_encrypted(da.to_string_lossy().into(), vec![19; 32])
                .unwrap();
        let b =
            dmsg_core::ffi::DmsgClient::open_encrypted(db.to_string_lossy().into(), vec![20; 32])
                .unwrap();
        a.reconnect(addr.clone(), key.clone(), domain.clone())
            .unwrap();
        let sealed_mid = a
            .send_text(
                addr.clone(),
                key.clone(),
                domain.clone(),
                eb.contact_id.clone(),
                "sealed ffi message".into(),
            )
            .unwrap();
        assert_eq!(
            a.message_status(sealed_mid.clone()).unwrap(),
            Some(DeliveryState::Accepted)
        );
        assert_eq!(
            b.fetch(addr.clone(), key.clone(), domain.clone())
                .unwrap()
                .received[0]
                .text,
            "sealed ffi message"
        );
        assert!(
            a.retry_queued(addr.clone(), key.clone(), domain.clone())
                .unwrap()
                .delivered
                >= 1
        );
        assert!(b
            .inbox_page(0, 100)
            .unwrap()
            .rows
            .iter()
            .any(|r| r.text == "sealed ffi message"));
        assert_eq!(
            a.message_status(sealed_mid).unwrap(),
            Some(DeliveryState::Delivered)
        );
        let count = a.outbox_page(0, 100).unwrap().rows.len();
        assert!(matches!(
            a.send_text(
                addr.clone(),
                vec![42; 32],
                domain.clone(),
                eb.contact_id.clone(),
                "must not queue".into()
            ),
            Err(dmsg_core::ffi::FfiError::Transport(_))
        ));
        assert_eq!(a.outbox_page(0, 100).unwrap().rows.len(), count);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let offline = listener.local_addr().unwrap().to_string();
        drop(listener);
        let mid = a
            .send_text(
                offline,
                key.clone(),
                domain.clone(),
                eb.contact_id.clone(),
                "offline-to-online".into(),
            )
            .unwrap();
        assert_eq!(
            a.message_status(mid.clone()).unwrap(),
            Some(DeliveryState::Queued)
        );
        let ciphertext = || {
            let conn = dmsg_core::store::open_encrypted(&da, &[19; 32]).unwrap();
            dmsg_core::store::outbox_queued(&conn, 0, 100)
                .unwrap()
                .0
                .into_iter()
                .find(|(_, id, _, _, _)| {
                    id.iter().map(|b| format!("{b:02x}")).collect::<String>() == mid
                })
                .unwrap()
                .3
        };
        let before = ciphertext();
        assert!(!before.windows(17).any(|w| w == b"offline-to-online"));
        assert!(b
            .fetch(addr.clone(), key.clone(), domain.clone())
            .unwrap()
            .received
            .is_empty());
        assert!(
            a.retry_queued(addr.clone(), key.clone(), domain.clone())
                .unwrap()
                .resent
                >= 1
        );
        assert_eq!(before, ciphertext());
        assert_eq!(
            a.message_status(mid.clone()).unwrap(),
            Some(DeliveryState::Accepted)
        );
        let r = b.fetch(addr.clone(), key.clone(), domain.clone()).unwrap();
        assert_eq!(r.received.len(), 1);
        assert_eq!(r.received[0].message_id_hex, mid);
        assert!(b
            .fetch(addr.clone(), key.clone(), domain.clone())
            .unwrap()
            .received
            .is_empty());
        a.retry_queued(addr.clone(), key.clone(), domain.clone())
            .unwrap();
        assert_eq!(
            a.message_status(mid.clone()).unwrap(),
            Some(DeliveryState::Delivered)
        );
        assert_eq!(b.message_status(mid).unwrap(), None);
        drop(a);
        drop(b);
        let a =
            dmsg_core::ffi::DmsgClient::open_encrypted(da.to_string_lossy().into(), vec![19; 32])
                .unwrap();
        let b =
            dmsg_core::ffi::DmsgClient::open_encrypted(db.to_string_lossy().into(), vec![20; 32])
                .unwrap();
        let ah = a.history_page(eb.contact_id.clone(), None, 100).unwrap();
        let bh = b.history_page(ea.contact_id.clone(), None, 100).unwrap();
        assert_eq!((ah.rows.len(), bh.rows.len()), (7, 7));
        assert_eq!(ah.rows[0].text, "offline-to-online");
        assert_eq!(bh.rows[0].text, "offline-to-online");
        assert_eq!(
            ah.rows
                .iter()
                .filter(|r| r.direction == MessageDirection::Incoming)
                .count(),
            2
        );
        assert_eq!(
            bh.rows
                .iter()
                .filter(|r| r.direction == MessageDirection::Outgoing)
                .count(),
            2
        );
        for path in [&da, &db] {
            for file in [
                path.clone(),
                std::path::PathBuf::from(format!("{}-wal", path.display())),
            ] {
                if let Ok(bytes) = std::fs::read(file) {
                    for secret in [
                        "hello bob",
                        "hi alice",
                        "after reverse gap",
                        "after forward gap",
                        "ffi roundtrip",
                        "sealed ffi message",
                        "offline-to-online",
                        "Local Bobby",
                    ] {
                        assert!(
                            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                            "plaintext text or alias at rest"
                        );
                    }
                }
            }
        }
        assert!(
            dmsg_core::ffi::DmsgClient::open(da.to_string_lossy().into())
                .account_info()
                .is_err()
        );
        assert!(dmsg_core::ffi::DmsgClient::open_encrypted(
            da.to_string_lossy().into(),
            vec![21; 32]
        )
        .is_err());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn replacement_warns_two_peers_retains_message_and_delivers_once_after_confirm() {
    use dmsg_core::auth::LoginOutcome;
    let srv = LiveMsgd::start("peer-replace", "peer.test");
    let old_db = srv.dir.join("old.db");
    let new_db = srv.dir.join("new.db");
    let p_db = srv.dir.join("peer.db");
    let q_db = srv.dir.join("peer2.db");
    let old_account = srv.signup(&old_db, "alice").await;
    let pa = srv.signup(&p_db, "bobby").await;
    let qa = srv.signup(&q_db, "carol").await;
    let mut old = Core::open(&old_db).unwrap();
    let mut p = Core::open(&p_db).unwrap();
    let mut q = Core::open(&q_db).unwrap();
    link(&old, &p);
    link(&old, &q);
    let mut old_t = srv.connect(&old_db).await;
    let mut pt = srv.connect(&p_db).await;
    let mut qt = srv.connect(&q_db).await;
    old.on_reconnect(&mut old_t).await.unwrap();
    p.on_reconnect(&mut pt).await.unwrap();
    q.on_reconnect(&mut qt).await.unwrap();
    for (peer, t, id) in [
        (&mut p, &mut pt, &pa.contact_id),
        (&mut q, &mut qt, &qa.contact_id),
    ] {
        old.send_text(&mut old_t, id, "old device message")
            .await
            .unwrap();
        assert_eq!(peer.fetch_and_decrypt(t).await.unwrap().received.len(), 1);
    }
    let expected = match dmsg_core::login_direct(
        &srv.code,
        &srv.addr,
        &new_db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        None,
    )
    .await
    .unwrap()
    {
        LoginOutcome::ReplacementRequired(key) => key,
        _ => panic!("confirmation required"),
    };
    old.login(&mut old_t).await.unwrap(); // Cancellation retains old access.
    assert_eq!(
        dmsg_core::login_direct(
            &srv.code,
            &srv.addr,
            &new_db,
            Some(DER),
            None,
            "alice",
            PASSWORD,
            Some(&expected)
        )
        .await
        .unwrap(),
        LoginOutcome::Authenticated(old_account.clone())
    );
    assert!(
        old.login(&mut old_t).await.is_err(),
        "old already-open session revoked"
    );
    let mut new = Core::open(&new_db).unwrap();
    assert_ne!(new.identity_keys(), old.identity_keys());
    assert_ne!(new.device_pub(), old.device_pub());
    new.add_contact_qr(&qr(&p)).unwrap();
    new.accept_contact(&pa.contact_id).unwrap();
    new.add_contact_qr(&qr(&q)).unwrap();
    new.accept_contact(&qa.contact_id).unwrap();
    let mut nt = srv.connect(&new_db).await;
    new.on_reconnect(&mut nt).await.unwrap();
    let mid = new
        .send_text(&mut nt, &pa.contact_id, "retained until confirm")
        .await
        .unwrap();
    let ciphertext = new.outbox_ciphertext(&mid).unwrap();
    let before = dmsg_core::store::load_session(
        &dmsg_core::store::open(&p_db).unwrap(),
        &old_account.contact_id,
    )
    .unwrap();
    let warning = p.fetch_and_decrypt(&mut pt).await.unwrap();
    assert!(warning.received.is_empty());
    assert_eq!(warning.skipped_mismatch, 1);
    assert_eq!(warning.skipped_unknown, 0);
    assert_eq!(
        p.history_page(&old_account.contact_id, None, 100)
            .unwrap()
            .rows
            .len(),
        1,
        "unconfirmed replacement cannot enter history"
    );
    assert!(p.dialogs_page(None, 100).unwrap().rows[0].identity_mismatch);
    assert_eq!(
        before,
        dmsg_core::store::load_session(
            &dmsg_core::store::open(&p_db).unwrap(),
            &old_account.contact_id
        )
        .unwrap(),
        "warning cannot advance ratchet"
    );
    assert_eq!(
        p.send_text(&mut pt, &old_account.contact_id, "stop").await,
        Err(OlmError::IdentityMismatch)
    );
    assert_eq!(
        q.send_text(
            &mut qt,
            &old_account.contact_id,
            "stop by directory refresh"
        )
        .await,
        Err(OlmError::IdentityMismatch)
    );
    // Warning is durable across restart; candidate routing keys were not pinned.
    drop(p);
    p = Core::open(&p_db).unwrap();
    let c = contacts::get(
        &dmsg_core::store::open(&p_db).unwrap(),
        &old_account.contact_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(c.device_key, Some(old.device_pub()));
    assert_eq!(c.seen_device, Some(new.device_pub()));
    assert_eq!(
        p.fetch_and_decrypt(&mut pt).await.unwrap().skipped_mismatch,
        1
    );
    p.confirm_contact(&old_account.contact_id).unwrap();
    q.confirm_contact(&old_account.contact_id).unwrap();
    assert!(dmsg_core::store::load_session(
        &dmsg_core::store::open(&p_db).unwrap(),
        &old_account.contact_id
    )
    .unwrap()
    .is_none());
    let r = p.fetch_and_decrypt(&mut pt).await.unwrap();
    assert_eq!(r.received.len(), 1);
    assert_eq!(r.received[0].text, "retained until confirm");
    assert_eq!(
        p.history_page(&old_account.contact_id, None, 100)
            .unwrap()
            .rows
            .len(),
        2
    );
    assert!(p
        .fetch_and_decrypt(&mut pt)
        .await
        .unwrap()
        .received
        .is_empty());
    new.retry_queued(&mut nt).await.unwrap();
    assert_eq!(ciphertext, new.outbox_ciphertext(&mid).unwrap());
    assert!(p
        .fetch_and_decrypt(&mut pt)
        .await
        .unwrap()
        .received
        .is_empty());
    p.send_text(&mut pt, &old_account.contact_id, "response new device")
        .await
        .unwrap();
    q.send_text(&mut qt, &old_account.contact_id, "second peer new session")
        .await
        .unwrap();
    let r = new.fetch_and_decrypt(&mut nt).await.unwrap();
    assert_eq!(r.received.len(), 2);
    assert_eq!(r.skipped_mismatch, 0);
    assert!(new
        .fetch_and_decrypt(&mut nt)
        .await
        .unwrap()
        .received
        .is_empty());
    new.send_text(&mut nt, &qa.contact_id, "new message peer two")
        .await
        .unwrap();
    assert_eq!(
        q.fetch_and_decrypt(&mut qt).await.unwrap().received.len(),
        1
    );
    assert!(q
        .fetch_and_decrypt(&mut qt)
        .await
        .unwrap()
        .received
        .is_empty());
    assert_eq!(
        dmsg_core::store::inbox_count(&dmsg_core::store::open(&new_db).unwrap()).unwrap(),
        2,
        "old history unavailable"
    );
}

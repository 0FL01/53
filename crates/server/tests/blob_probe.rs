//! Live fresh-schema6 Noise tests: durable resume, exact-device ACL, atomic
//! mailbox/ACL dedup, independent control and coherent backup references.
mod common;
use common::*;
use dmsg_protocol::{blob as bp, mailbox as mp, *};
use rusqlite::params;

async fn account(s: &Server, key: u8, name: &str) -> (Client, [u8; 16]) {
    let mut c = Client::connect(s, [key; 32]).await;
    let (op, p) = c.signup(name, None).await;
    assert_eq!(op, OP_AUTHENTICATED);
    (c, p[..16].try_into().unwrap())
}
async fn resume(s: &Server, key: u8) -> Client {
    let mut c = Client::connect(s, [key; 32]).await;
    assert_eq!(c.exchange(OP_RESUME, &[]).await.0, OP_AUTHENTICATED);
    c
}
async fn error(c: &mut Client, op: u8, p: &[u8], code: u8) {
    assert_eq!(
        c.exchange(op, p).await,
        (OP_ERROR, vec![code]),
        "operation {op}"
    );
}
async fn reserve(c: &mut Client, id: &[u8; 16], len: u32) {
    assert_eq!(
        c.exchange(OP_BLOB_RESERVE, &bp::build_reserve(id, len).unwrap())
            .await,
        (OP_BLOB_RESERVED, id.to_vec())
    );
}
async fn put(c: &mut Client, id: &[u8; 16], index: u16, bytes: &[u8]) {
    let (op, p) = c
        .exchange(OP_BLOB_PUT, &bp::build_put(id, index, bytes).unwrap())
        .await;
    assert_eq!(op, OP_BLOB_PUT_ACK);
    assert_eq!(bp::parse_put_ack(&p), Some((*id, index)));
}
async fn status(c: &mut Client, id: &[u8; 16]) -> bp::Status {
    let (op, p) = c.exchange(OP_BLOB_STATUS, &bp::build_status(id)).await;
    assert_eq!(op, OP_BLOB_STATUS_RESP);
    bp::parse_status_response(&p).unwrap()
}
async fn finish(c: &mut Client, id: &[u8; 16]) {
    let (op, p) = c.exchange(OP_BLOB_FINISH, &bp::build_finish(id)).await;
    assert_eq!(op, OP_BLOB_FINISH_ACK);
    assert_eq!(bp::parse_finish_ack(&p), Some(*id));
}
async fn data(c: &mut Client, id: &[u8; 16], index: u16) -> Vec<u8> {
    let (op, p) = c
        .exchange(OP_BLOB_GET, &bp::build_get(id, index).unwrap())
        .await;
    assert_eq!(op, OP_BLOB_DATA);
    let c = bp::parse_data(&p).unwrap();
    assert_eq!((*c.blob_id, c.index), (*id, index));
    c.bytes.to_vec()
}
async fn accept(a: &mut Client, b: &mut Client, au: &[u8; 16], bu: &[u8; 16]) {
    assert_eq!(a.exchange(OP_CONTACT_REQUEST, bu).await.0, OP_CONTACT_OK);
    let mut p = au.to_vec();
    p.push(1);
    assert_eq!(b.exchange(OP_CONTACT_DECIDE, &p).await.0, OP_CONTACT_OK);
}

#[tokio::test]
async fn durable_resume_exact_device_acl_and_retry_never_reroutes() {
    let mut s = Server::new("blob-resume");
    s.ctl(&["registration-mode", "open"]);
    assert_eq!(s.ctl(&["dbversion"]), "6\n");
    let (mut a, au) = account(&s, 11, "alice").await;
    let (mut b, bu) = account(&s, 12, "bob").await;
    let (mut e, _) = account(&s, 13, "eve").await;
    let id = [21; 16];
    let len = 16389;
    reserve(&mut a, &id, len).await;
    reserve(&mut a, &id, len).await;
    error(
        &mut a,
        OP_BLOB_RESERVE,
        &bp::build_reserve(&id, len + 1).unwrap(),
        ERR_BAD,
    )
    .await;
    error(
        &mut e,
        OP_BLOB_RESERVE,
        &bp::build_reserve(&id, len).unwrap(),
        ERR_BAD,
    )
    .await;
    error(&mut e, OP_BLOB_STATUS, &id, ERR_BAD).await;
    error(
        &mut b,
        OP_BLOB_GET,
        &bp::build_get(&id, 0).unwrap(),
        ERR_BAD,
    )
    .await;
    error(
        &mut a,
        OP_BLOB_PUT,
        &bp::build_put(&id, 0, b"short").unwrap(),
        ERR_BAD,
    )
    .await;
    put(&mut a, &id, 0, &vec![31; 8192]).await;
    put(&mut a, &id, 0, &vec![31; 8192]).await;
    error(
        &mut a,
        OP_BLOB_PUT,
        &bp::build_put(&id, 0, &vec![32; 8192]).unwrap(),
        ERR_BAD,
    )
    .await;
    error(&mut a, OP_BLOB_FINISH, &id, ERR_BAD).await;
    assert_eq!(status(&mut a, &id).await.bitmap, 1);
    // Process death after acknowledged receipt: reconnect only sends missing chunks.
    drop((a, b, e));
    s.restart();
    let mut a = resume(&s, 11).await;
    let mut b = resume(&s, 12).await;
    let mut e = resume(&s, 13).await;
    assert_eq!(status(&mut a, &id).await.bitmap, 1);
    put(&mut a, &id, 1, &vec![33; 8192]).await;
    put(&mut a, &id, 2, b"last!").await;
    finish(&mut a, &id).await;
    let expiry: i64 = s
        .db()
        .query_row(
            "SELECT expires_at FROM blob_meta WHERE blob_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    finish(&mut a, &id).await;
    assert_eq!(
        s.db()
            .query_row(
                "SELECT expires_at FROM blob_meta WHERE blob_id=?1",
                [&id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        expiry
    );
    assert_eq!(status(&mut a, &id).await.state, bp::State::Complete);
    let mid = [41; 16];
    let send = bp::build_send_media(&bu, &b.device, &mid, &id, b"opaque-olm-manifest").unwrap();
    error(&mut a, OP_SEND_MEDIA, &send, ERR_BAD).await; // contact consent required
    accept(&mut a, &mut b, &au, &bu).await;
    let wrong = bp::build_send_media(&bu, &e.device, &mid, &id, b"opaque-olm-manifest").unwrap();
    error(&mut a, OP_SEND_MEDIA, &wrong, ERR_REVOKED).await;
    s.db().execute_batch("CREATE TRIGGER fail_acl BEFORE INSERT ON blob_acl BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    error(&mut a, OP_SEND_MEDIA, &send, ERR_BAD).await;
    assert_eq!(
        s.db()
            .query_row("SELECT COUNT(*) FROM mailbox_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    s.db().execute_batch("DROP TRIGGER fail_acl").unwrap();
    assert_eq!(
        a.exchange(OP_SEND_MEDIA, &send).await,
        (OP_SEND_ACK, [mid.as_slice(), &[ST_ACCEPTED]].concat())
    );
    assert_eq!(data(&mut b, &id, 0).await, vec![31; 8192]);
    assert_eq!(data(&mut b, &id, 2).await, b"last!");
    error(
        &mut e,
        OP_BLOB_GET,
        &bp::build_get(&id, 0).unwrap(),
        ERR_BAD,
    )
    .await;
    error(
        &mut b,
        OP_BLOB_PUT,
        &bp::build_put(&id, 2, b"last!").unwrap(),
        ERR_BAD,
    )
    .await;
    let conflicting = bp::build_send_media(&bu, &b.device, &mid, &id, b"different").unwrap();
    error(&mut a, OP_SEND_MEDIA, &conflicting, ERR_BAD).await;
    let different_blob =
        bp::build_send_media(&bu, &b.device, &mid, &[22; 16], b"opaque-olm-manifest").unwrap();
    error(&mut a, OP_SEND_MEDIA, &different_blob, ERR_BAD).await;
    let (op, fetch) = b.exchange(OP_FETCH, &[]).await;
    assert_eq!(op, OP_FETCH_RESP);
    let rows = mp::parse_fetch_resp(&fetch).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ciphertext, b"opaque-olm-manifest");
    let seq = rows[0].seq;
    let mut ack = vec![0, 1];
    ack.extend_from_slice(&seq.to_be_bytes());
    assert_eq!(b.exchange(OP_DELIVERY_ACK, &ack).await.0, OP_DELIVERY_ACK);
    assert_eq!(a.exchange(OP_SEND_MEDIA, &send).await.1[16], ST_DELIVERED);
    // A plain SEND collision must not bypass the stricter media dedup contract.
    error(
        &mut a,
        OP_SEND,
        &[bu.as_slice(), mid.as_slice(), b"different"].concat(),
        ERR_BAD,
    )
    .await;
    let mut new_b = Client::connect(&s, [14; 32]).await;
    assert_eq!(
        new_b.login("bob", Some(&b.device)).await.0,
        OP_AUTHENTICATED
    );
    b.closed().await;
    error(&mut new_b, OP_BLOB_STATUS, &id, ERR_BAD).await;
    error(
        &mut new_b,
        OP_BLOB_GET,
        &bp::build_get(&id, 0).unwrap(),
        ERR_BAD,
    )
    .await;
    assert!(mp::parse_fetch_resp(&new_b.exchange(OP_FETCH, &[]).await.1)
        .unwrap()
        .is_empty());
    assert_eq!(a.exchange(OP_SEND_MEDIA, &send).await.1[16], ST_DELIVERED);
    let reroute =
        bp::build_send_media(&bu, &new_b.device, &mid, &id, b"opaque-olm-manifest").unwrap();
    error(&mut a, OP_SEND_MEDIA, &reroute, ERR_BAD).await;
    assert_eq!(
        s.db()
            .query_row("SELECT COUNT(*) FROM blob_acl", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        s.db()
            .query_row("SELECT COUNT(*) FROM mailbox_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop((a, b, e, new_b));
    s.restart();
    let mut a = resume(&s, 11).await;
    assert_eq!(a.exchange(OP_SEND_MEDIA, &send).await.1[16], ST_DELIVERED);
    assert_eq!(
        s.db()
            .query_row("SELECT COUNT(*) FROM blob_acl", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    // Exact owner identity: account replacement cannot resume another device's upload.
    let mut new_a = Client::connect(&s, [15; 32]).await;
    assert_eq!(
        new_a.login("alice", Some(&a.device)).await.0,
        OP_AUTHENTICATED
    );
    a.closed().await;
    error(&mut new_a, OP_BLOB_STATUS, &id, ERR_BAD).await;
    error(
        &mut new_a,
        OP_BLOB_RESERVE,
        &bp::build_reserve(&id, len).unwrap(),
        ERR_BAD,
    )
    .await;
}

#[tokio::test]
async fn blob_receipt_failure_control_progress_backup_and_gc() {
    let s = Server::new("blob-boundary");
    s.ctl(&["registration-mode", "open"]);
    let (mut a, au) = account(&s, 51, "alice").await;
    let (mut b, bu) = account(&s, 52, "bob").await;
    accept(&mut a, &mut b, &au, &bu).await;
    let id = [53; 16];
    reserve(&mut a, &id, 8193).await;
    // SQL commit boundary failure after durable rename: no ACK and no receipt.
    s.db().execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON blob_chunks BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    error(
        &mut a,
        OP_BLOB_PUT,
        &bp::build_put(&id, 0, &vec![54; 8192]).unwrap(),
        ERR_BAD,
    )
    .await;
    assert_eq!(status(&mut a, &id).await.bitmap, 0);
    let path = s.dir.join("blobs").join("35".repeat(16)).join("0.chunk");
    assert_eq!(std::fs::read(&path).unwrap(), vec![54; 8192]);
    s.db().execute_batch("DROP TRIGGER fail_receipt").unwrap();
    put(&mut a, &id, 0, &vec![54; 8192]).await;
    put(&mut a, &id, 1, b"z").await;
    finish(&mut a, &id).await;
    let send = bp::build_send_media(&bu, &b.device, &[55; 16], &id, b"manifest").unwrap();
    assert_eq!(a.exchange(OP_SEND_MEDIA, &send).await.0, OP_SEND_ACK);
    // Large upload on its own Noise stream leaves TEXT/FETCH on control usable.
    let mut bulk = resume(&s, 51).await;
    let large = [56; 16];
    reserve(&mut bulk, &large, BLOB_SIZE_MAX).await;
    let upload = async {
        for i in 0..64 {
            put(&mut bulk, &large, i, &vec![57; 8192]).await;
        }
    };
    let control = async {
        for i in 0..8u8 {
            let p = [bu.as_slice(), [i; 16].as_slice(), b"control"].concat();
            assert_eq!(a.exchange(OP_SEND, &p).await.0, OP_SEND_ACK);
            assert_eq!(b.exchange(OP_FETCH, &[]).await.0, OP_FETCH_RESP);
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(upload, control);
    })
    .await
    .unwrap();
    // Incomplete reservations and their durable receipts also survive backup.
    let orphan = s.dir.join("blobs").join("ff".repeat(16));
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("0.part"), b"unused").unwrap();
    let rep = s.ctl(&["backup"]);
    let snap = rep
        .split_whitespace()
        .find_map(|p| p.strip_prefix("path="))
        .unwrap();
    let snapshot = rusqlite::Connection::open(std::path::Path::new(snap).join("msgd.db")).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT COUNT(*) FROM blob_chunks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        66
    );
    assert_eq!(
        std::fs::read(
            std::path::Path::new(snap)
                .join("blobs")
                .join("35".repeat(16))
                .join("0.chunk")
        )
        .unwrap(),
        vec![54; 8192]
    );
    let restored = std::path::Path::new(snap).join("blobs");
    assert_eq!(
        std::fs::read(restored.join("38".repeat(16)).join("63.chunk")).unwrap(),
        vec![57; 8192]
    );
    // The same backup can be taken twice in one second without overwriting it.
    assert!(s.ctl(&["backup"]).starts_with("backup path="));
    assert!(!std::path::Path::new(snap)
        .join("blobs")
        .join("ff".repeat(16))
        .exists());
    // Block either participant immediately prevents old ACL operations.
    let mut block = au.to_vec();
    block.push(2);
    assert_eq!(b.exchange(OP_CONTACT_DECIDE, &block).await.0, OP_CONTACT_OK);
    error(
        &mut b,
        OP_BLOB_GET,
        &bp::build_get(&id, 0).unwrap(),
        ERR_BAD,
    )
    .await;
    // Expired metadata is removed before files; orphan chunks are swept.
    s.db()
        .execute(
            "UPDATE blob_meta SET expires_at=0 WHERE blob_id=?1",
            [large],
        )
        .unwrap();
    assert!(s.ctl(&["gc"]).starts_with("gc blobs=1"));
    assert!(!orphan.exists());
    assert!(data(&mut a, &id, 0).await.len() == 8192);
    // Broken acknowledged reference is an explicit backup failure, never a
    // supposedly successful backup lacking a referenced ciphertext chunk.
    std::fs::remove_file(path).unwrap();
    assert_eq!(s.raw("backup"), "err\n");
    assert_eq!(
        s.db()
            .query_row(
                "SELECT COUNT(*) FROM blob_meta WHERE blob_id=?1",
                params![id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

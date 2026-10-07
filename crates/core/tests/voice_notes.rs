//! Fresh core9/server6, actual Opus notes, and the opaque API used by Android.
//! Direct TCP is a host integration seam, not DNS/device acceptance evidence.
mod support;

use dmsg_core::{
    contacts,
    ffi::{DmsgClient, FfiError, VoiceTransfer},
    history::{DeleteScope, DeliveryState, MessageKind},
    voice_codec::{VoiceDecoder, VoiceEncoder},
    Core,
};
use std::sync::Arc;
use support::LiveMsgd;

const KA: [u8; 32] = [71; 32];
const KB: [u8; 32] = [72; 32];

struct Pair {
    srv: LiveMsgd,
    da: std::path::PathBuf,
    db: std::path::PathBuf,
    a: Arc<DmsgClient>,
    b: Arc<DmsgClient>,
    aid: String,
    bid: String,
}

impl Pair {
    fn new(tag: &str) -> Self {
        let srv = LiveMsgd::start(tag, "voice.test");
        let da = srv.dir.join("a.db");
        let db = srv.dir.join("b.db");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (aid, bid) = rt.block_on(async {
            let aa = srv.signup_keyed(&da, "alice", Some(&KA)).await;
            let ba = srv.signup_keyed(&db, "bobby", Some(&KB)).await;
            let mut a = Core::open_encrypted(&da, &KA).unwrap();
            let mut b = Core::open_encrypted(&db, &KB).unwrap();
            let qr = |core: &Core| {
                let (uid, id) = core.my_account().unwrap();
                let (ed, curve) = core.identity_keys();
                contacts::build_qr(&id, &uid, &core.device_pub(), &ed, &curve).unwrap()
            };
            contacts::invite_from_qr(
                &dmsg_core::store::open_encrypted(&da, &KA).unwrap(),
                &qr(&b),
            )
            .unwrap();
            contacts::invite_from_qr(
                &dmsg_core::store::open_encrypted(&db, &KB).unwrap(),
                &qr(&a),
            )
            .unwrap();
            let mut ta = srv.connect_keyed(&da, Some(&KA)).await;
            let mut tb = srv.connect_keyed(&db, Some(&KB)).await;
            a.on_reconnect(&mut ta).await.unwrap();
            b.on_reconnect(&mut tb).await.unwrap();
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
            (aa.contact_id, ba.contact_id)
        });
        let a = DmsgClient::open_encrypted(da.to_string_lossy().into(), KA.to_vec()).unwrap();
        let b = DmsgClient::open_encrypted(db.to_string_lossy().into(), KB.to_vec()).unwrap();
        let server_db = rusqlite::Connection::open_with_flags(
            srv.dir.join("data/msgd.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            server_db
                .query_row::<String, _, _>(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            "6"
        );
        assert!(
            server_db
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM contact_permissions WHERE state='accepted'",
                    [],
                    |r| r.get(0)
                )
                .unwrap()
                > 0,
            "real contact permission must be accepted"
        );
        Self {
            srv,
            da,
            db,
            a,
            b,
            aid,
            bid,
        }
    }
    fn prime(&self) {
        self.a
            .prime_voice_session(
                self.srv.addr.clone(),
                self.srv.server_pub.to_vec(),
                self.srv.domain.clone(),
                self.bid.clone(),
            )
            .unwrap();
    }
    fn transfer(
        &self,
        client: &DmsgClient,
        cid: &str,
        id: i64,
        download: bool,
    ) -> Arc<VoiceTransfer> {
        client
            .prepare_voice_transfer_direct(
                self.srv.addr.clone(),
                self.srv.server_pub.to_vec(),
                self.srv.domain.clone(),
                cid.into(),
                id,
                download,
            )
            .unwrap()
    }
    fn retry(&self) -> dmsg_core::ffi::RetryReport {
        self.a
            .retry_queued(
                self.srv.addr.clone(),
                self.srv.server_pub.to_vec(),
                self.srv.domain.clone(),
            )
            .unwrap()
    }
    fn fetch(&self) -> dmsg_core::ffi::FetchReport {
        self.b
            .fetch(
                self.srv.addr.clone(),
                self.srv.server_pub.to_vec(),
                self.srv.domain.clone(),
            )
            .unwrap()
    }
    fn reopen(&mut self) {
        self.a = DmsgClient::open_encrypted(self.da.to_string_lossy().into(), KA.to_vec()).unwrap();
        self.b = DmsgClient::open_encrypted(self.db.to_string_lossy().into(), KB.to_vec()).unwrap();
    }
}

fn note(seconds: u32) -> Vec<u8> {
    let mut encoder = VoiceEncoder::new().unwrap();
    for batch in 0..seconds * 10 {
        let pcm: Vec<i16> = (0..1600)
            .map(|i| {
                let t = (batch * 1600 + i) as f64 / 16_000.;
                ((t * std::f64::consts::TAU * 180.).sin() * 8000.
                    + (t * std::f64::consts::TAU * 360.).sin() * 2000.) as i16
            })
            .collect();
        encoder.push(&pcm).unwrap();
    }
    encoder.finish().unwrap().bytes
}

fn finish(client: &DmsgClient, transfer: Arc<VoiceTransfer>) {
    for _ in 0..40 {
        let network = transfer.advance().unwrap();
        let durable = client.commit_voice_transfer(transfer.clone()).unwrap();
        assert_eq!(network, durable);
        if durable.complete {
            return;
        }
    }
    panic!("bounded note transfer did not finish");
}

#[test]
fn first_voice_atomic_sealed_queue_upload_and_manual_download_resume_after_both_restarts() {
    let mut p = Pair::new("voice-first-resume");
    let encoded = note(12);
    let mid = "01010101010101010101010101010101".to_string();
    assert!(!p.a.voice_session_ready(p.bid.clone()).unwrap());
    assert_eq!(
        p.a.queue_voice(p.bid.clone(), mid.clone(), encoded.clone()),
        Err(FfiError::VoiceSessionRequired)
    );
    assert!(p
        .a
        .history_page(p.bid.clone(), None, 100)
        .unwrap()
        .rows
        .is_empty());
    p.prime();
    assert!(p.a.voice_session_ready(p.bid.clone()).unwrap());
    let before = dmsg_core::store::open_encrypted(&p.da, &KA).unwrap();
    let ratchet: Vec<u8> = before
        .query_row("SELECT pickle FROM core_sessions", [], |r| r.get(0))
        .unwrap();
    before.execute_batch("CREATE TRIGGER fail_voice_queue AFTER UPDATE ON core_sessions BEGIN SELECT RAISE(ABORT,'test rollback'); END;").unwrap();
    assert!(matches!(
        p.a.queue_voice(p.bid.clone(), mid.clone(), encoded.clone()),
        Err(FfiError::Store(_))
    ));
    for table in ["core_messages", "core_blob_transfers", "core_blob_chunks"] {
        assert_eq!(
            before
                .query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        before
            .query_row::<Vec<u8>, _, _>("SELECT pickle FROM core_sessions", [], |r| r.get(0))
            .unwrap(),
        ratchet
    );
    before
        .execute_batch("DROP TRIGGER fail_voice_queue;")
        .unwrap();
    drop(before);
    let queued =
        p.a.queue_voice(p.bid.clone(), mid.clone(), encoded.clone())
            .unwrap();
    assert_eq!(queued.kind, MessageKind::Voice);
    assert!(queued.text.is_empty());
    assert_eq!(queued.voice.as_ref().unwrap().sample_count, 12 * 16_000);
    assert_eq!(queued.voice.as_ref().unwrap().waveform.len(), 64);
    assert!(queued.voice.as_ref().unwrap().downloaded);
    assert_eq!(queued.delivery_state, Some(DeliveryState::Queued));
    // Stable attempt reconciliation does not parse/re-encrypt a retry payload.
    assert_eq!(
        p.a.queue_voice(p.bid.clone(), mid.clone(), vec![]).unwrap(),
        queued
    );
    assert_eq!(
        p.a.queue_voice(p.aid.clone(), mid.clone(), vec![]),
        Err(FfiError::MessageUnavailable)
    );
    assert_eq!(
        p.a.history_message_by_mid(p.bid.clone(), mid.clone())
            .unwrap(),
        Some(queued.clone())
    );
    let ac = dmsg_core::store::open_encrypted(&p.da, &KA).unwrap();
    let sealed: Vec<u8> = ac
        .query_row("SELECT media_manifest FROM core_messages", [], |r| r.get(0))
        .unwrap();
    assert!(sealed.starts_with(b"DMSG-S1"));
    let manifest: Vec<u8> = ac
        .query_row(
            "SELECT dmsg_unseal('voice_manifest',media_manifest) FROM core_messages",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let event = dmsg_protocol::e2e::decode(&manifest).unwrap();
    let dmsg_protocol::e2e::Body::Voice(m) = event.body else {
        panic!("typed voice")
    };
    let original: Vec<u8> = ac
        .query_row("SELECT ciphertext FROM core_messages", [], |r| r.get(0))
        .unwrap();
    let chunks: Vec<Vec<u8>> = ac
        .prepare("SELECT ciphertext FROM core_blob_chunks ORDER BY chunk_index")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(chunks.len() > 1);
    assert!(ac
        .execute(
            "UPDATE core_messages SET media_manifest=?1",
            [manifest.as_slice()]
        )
        .is_err());
    assert!(ac
        .query_row::<Vec<u8>, _, _>(
            "SELECT dmsg_unseal('message_text',media_manifest) FROM core_messages",
            [],
            |r| r.get(0)
        )
        .is_err());
    for entry in std::fs::read_dir(&p.srv.dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file()
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("a.db")
        {
            let bytes = std::fs::read(path).unwrap();
            for secret in [
                encoded.as_slice(),
                m.key.as_slice(),
                m.nonce_prefix.as_slice(),
            ] {
                assert!(
                    !bytes.windows(secret.len()).any(|w| w == secret),
                    "audio/key in SQLite/WAL"
                );
            }
        }
    }
    // Clear cache preserves every queued byte; no media auto-download on fetch.
    p.a.clear_voice_cache().unwrap();
    assert_eq!(
        p.a.voice_data(p.bid.clone(), queued.local_id).unwrap(),
        encoded
    );
    let upload = p.transfer(&p.a, &p.bid, queued.local_id, false);
    let mut progressed = false;
    for _ in 0..5 {
        let progress = upload.advance().unwrap();
        p.a.commit_voice_transfer(upload.clone()).unwrap();
        if progress.transferred > 0 {
            assert!(!progress.complete);
            progressed = true;
            break;
        }
    }
    assert!(progressed);
    upload.cancel();
    drop(upload);
    drop(ac);
    p.srv.restart();
    p.reopen();
    let upload = p.transfer(&p.a, &p.bid, queued.local_id, false);
    let status = upload.advance().unwrap();
    assert!(status.transferred > 0, "server bitmap survived restart");
    p.a.commit_voice_transfer(upload.clone()).unwrap();
    finish(&p.a, upload);
    assert!(p.a.pending_voice_upload().unwrap().is_none());
    p.retry();
    let report = p.fetch();
    assert_eq!(report.received.len(), 1);
    assert_eq!(report.received[0].kind, MessageKind::Voice);
    assert!(report.received[0].text.is_empty());
    assert!(!report.received[0].voice.as_ref().unwrap().downloaded);
    let incoming =
        p.b.history_page(p.aid.clone(), None, 100)
            .unwrap()
            .rows
            .remove(0);
    assert_eq!(incoming.message_id_hex, mid);
    assert!(p.b.voice_data(p.aid.clone(), incoming.local_id).is_err());
    let summary = p.b.dialogs_page(None, 100).unwrap().rows.remove(0);
    assert_eq!(summary.preview, None);
    assert_eq!(summary.preview_kind, Some(MessageKind::Voice));
    assert_eq!(summary.voice_duration_ms, Some(12_000));
    assert_eq!(summary.local_unread, 1);
    assert_eq!(
        p.b.inbox_page(0, 100).unwrap().rows[0].kind,
        MessageKind::Voice
    );
    let download = p.transfer(&p.b, &p.aid, incoming.local_id, true);
    download.advance().unwrap();
    p.b.commit_voice_transfer(download.clone()).unwrap(); // status
    let progress = download.advance().unwrap();
    assert!(progress.transferred > 0 && !progress.complete);
    p.b.commit_voice_transfer(download.clone()).unwrap();
    download.cancel();
    drop(download);
    p.reopen();
    finish(&p.b, p.transfer(&p.b, &p.aid, incoming.local_id, true));
    assert_eq!(
        p.b.voice_data(p.aid.clone(), incoming.local_id).unwrap(),
        encoded
    );
    let mut decoder = VoiceDecoder::new(&encoded).unwrap();
    let mut samples = 0;
    loop {
        let pcm = decoder.read(1600).unwrap();
        if pcm.is_empty() {
            break;
        }
        samples += pcm.len();
    }
    assert_eq!(samples, 12 * 16_000);
    p.retry();
    let accepted = p.a.history_message(p.bid.clone(), queued.local_id).unwrap();
    assert_eq!(accepted.delivery_state, Some(DeliveryState::Delivered));
    assert_eq!(accepted.server_seq, incoming.server_seq);
    assert_eq!(accepted.server_timestamp_ms, incoming.server_timestamp_ms);
    let ac = dmsg_core::store::open_encrypted(&p.da, &KA).unwrap();
    assert_eq!(
        ac.query_row::<Vec<u8>, _, _>("SELECT ciphertext FROM core_messages", [], |r| r.get(0))
            .unwrap(),
        original
    );
    assert_eq!(
        ac.prepare("SELECT ciphertext FROM core_blob_chunks ORDER BY chunk_index")
            .unwrap()
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        chunks
    );
    p.b.clear_voice_cache().unwrap();
    assert!(
        !p.b.history_message(p.aid.clone(), incoming.local_id)
            .unwrap()
            .voice
            .unwrap()
            .downloaded
    );
    assert!(p.b.voice_data(p.aid.clone(), incoming.local_id).is_err());
    // Receiver-side acceptance has its distinct persisted contact state. Its
    // already cached inbound Olm session must support an offline VOICE reply.
    let bc = dmsg_core::store::open_encrypted(&p.db, &KB).unwrap();
    bc.execute(
        "UPDATE core_contacts SET state='accepted_server' WHERE contact_id=?1",
        [&p.aid],
    )
    .unwrap();
    let reply =
        p.b.queue_voice(
            p.aid.clone(),
            "07070707070707070707070707070707".into(),
            note(1),
        )
        .unwrap();
    assert_eq!(p.b.pending_voice_upload().unwrap(), Some(reply));
}

#[test]
fn pending_upload_does_not_block_text_delete_and_hidden_queue_still_sends() {
    let p = Pair::new("voice-control-fairness");
    p.prime();
    let first =
        p.a.queue_voice(
            p.bid.clone(),
            "02020202020202020202020202020202".into(),
            note(1),
        )
        .unwrap();
    finish(&p.a, p.transfer(&p.a, &p.bid, first.local_id, false));
    p.retry();
    p.fetch();
    let pending =
        p.a.queue_voice(
            p.bid.clone(),
            "03030303030303030303030303030303".into(),
            note(12),
        )
        .unwrap();
    assert_eq!(
        p.a.delete_message(p.bid.clone(), pending.local_id, DeleteScope::Everyone),
        Err(FfiError::MessageUnavailable)
    );
    assert_eq!(
        p.a.edit_message(p.bid.clone(), first.local_id, 0, "bad edit".into()),
        Err(FfiError::MessageUnavailable)
    );
    let hidden =
        p.a.delete_message(p.bid.clone(), pending.local_id, DeleteScope::SelfOnly)
            .unwrap();
    assert!(hidden.hidden_self && !hidden.deleted_all);
    assert_eq!(
        p.a.queue_voice(p.bid.clone(), hidden.message_id_hex.clone(), vec![])
            .unwrap(),
        hidden
    );
    let deletion =
        p.a.delete_message(p.bid.clone(), first.local_id, DeleteScope::Everyone)
            .unwrap();
    assert!(deletion.deleted_all && deletion.voice.is_none());
    let text =
        p.a.send_text(
            p.srv.addr.clone(),
            p.srv.server_pub.to_vec(),
            p.srv.domain.clone(),
            p.bid.clone(),
            "text bypasses bulk".into(),
        )
        .unwrap();
    assert_eq!(
        p.a.queue_voice(p.bid.clone(), text.clone(), vec![]),
        Err(FfiError::MessageUnavailable)
    );
    let retried = p.retry();
    assert!(retried.skipped >= 1 && retried.resent >= 2);
    assert_eq!(
        p.a.message_status(text).unwrap(),
        Some(DeliveryState::Accepted)
    );
    let report = p.fetch();
    assert_eq!(report.received.len(), 1);
    assert_eq!(report.received[0].text, "text bypasses bulk");
    let tombstone =
        p.b.history_message_by_mid(p.aid.clone(), first.message_id_hex.clone())
            .unwrap()
            .unwrap();
    assert!(tombstone.deleted_all && tombstone.voice.is_none());
    assert!(p.b.voice_data(p.aid.clone(), tombstone.local_id).is_err());
    p.retry();
    assert_eq!(
        p.a.message_status(first.message_id_hex.clone()).unwrap(),
        Some(DeliveryState::Delivered)
    );
    finish(&p.a, p.transfer(&p.a, &p.bid, pending.local_id, false));
    p.retry();
    let report = p.fetch();
    assert_eq!(report.received.len(), 1);
    assert_eq!(report.received[0].kind, MessageKind::Voice);
    let hidden =
        p.a.history_message(p.bid.clone(), pending.local_id)
            .unwrap();
    assert!(hidden.hidden_self && hidden.voice.is_none());
    assert_eq!(hidden.delivery_state, Some(DeliveryState::Accepted));
    assert!(p.a.voice_data(p.bid.clone(), pending.local_id).is_err());
    // SelfOnly does not affect the peer's base row.
    assert!(
        !p.b.history_message_by_mid(p.aid.clone(), pending.message_id_hex)
            .unwrap()
            .unwrap()
            .deleted_all
    );
}

#[test]
fn late_download_receipt_cannot_resurrect_everyone_deleted_voice_or_changed_epoch() {
    let p = Pair::new("voice-late-delete");
    p.prime();
    let row =
        p.a.queue_voice(
            p.bid.clone(),
            "04040404040404040404040404040404".into(),
            note(12),
        )
        .unwrap();
    finish(&p.a, p.transfer(&p.a, &p.bid, row.local_id, false));
    p.retry();
    p.fetch();
    let incoming =
        p.b.history_message_by_mid(p.aid.clone(), row.message_id_hex.clone())
            .unwrap()
            .unwrap();
    let transfer = p.transfer(&p.b, &p.aid, incoming.local_id, true);
    transfer.advance().unwrap();
    p.b.commit_voice_transfer(transfer.clone()).unwrap();
    transfer.advance().unwrap(); // opaque uncommitted GET receipt
    p.a.delete_message(p.bid.clone(), row.local_id, DeleteScope::Everyone)
        .unwrap();
    p.retry();
    p.fetch();
    assert_eq!(
        p.b.commit_voice_transfer(transfer.clone()),
        Err(FfiError::MessageUnavailable)
    );
    transfer.cancel();
    assert!(
        p.b.history_message(p.aid.clone(), incoming.local_id)
            .unwrap()
            .deleted_all
    );
    let c = dmsg_core::store::open_encrypted(&p.db, &KB).unwrap();
    assert_eq!(
        c.query_row::<i64, _, _>("SELECT count(*) FROM core_blob_chunks", [], |r| r.get(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row::<i64, _, _>("SELECT count(*) FROM core_blob_transfers", [], |r| r.get(0))
            .unwrap(),
        0
    );
    let another =
        p.a.queue_voice(
            p.bid.clone(),
            "05050505050505050505050505050505".into(),
            note(1),
        )
        .unwrap();
    let transfer = p.transfer(&p.a, &p.bid, another.local_id, false);
    transfer.advance().unwrap();
    p.a.contact_block(p.bid.clone()).unwrap();
    assert_eq!(
        p.a.commit_voice_transfer(transfer.clone()),
        Err(FfiError::Blocked)
    );
    transfer.cancel();
}

#[test]
fn cancellation_closes_only_the_bulk_stream_without_holding_the_store() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};
    fn frame(s: &mut TcpStream) -> Vec<u8> {
        let mut hdr = [0; 2];
        s.read_exact(&mut hdr).unwrap();
        let mut bytes = vec![0; usize::from(u16::from_be_bytes(hdr)) + 2];
        bytes[..2].copy_from_slice(&hdr);
        s.read_exact(&mut bytes[2..]).unwrap();
        bytes
    }
    let p = Pair::new("voice-cancel-independent");
    p.prime();
    let queued =
        p.a.queue_voice(
            p.bid.clone(),
            "08080808080808080808080808080808".into(),
            note(1),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let upstream = p.srv.addr.clone();
    let (stalled, ready) = std::sync::mpsc::channel();
    let proxy = std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut server = TcpStream::connect(upstream).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Relay actual Noise IK, AUTH_DOMAIN/WELCOME and RESUME/AUTHENTICATED.
        // Stall the first blob request without decoding any encrypted frame.
        for _ in 0..3 {
            server.write_all(&frame(&mut client)).unwrap();
            client.write_all(&frame(&mut server)).unwrap();
        }
        let _ = frame(&mut client);
        stalled.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(
            client.read(&mut byte).unwrap(),
            0,
            "cancel must close its own TCP stream"
        );
    });
    let transfer =
        p.a.prepare_voice_transfer_direct(
            address,
            p.srv.server_pub.to_vec(),
            p.srv.domain.clone(),
            p.bid.clone(),
            queued.local_id,
            false,
        )
        .unwrap();
    let advancing = transfer.clone();
    let step = std::thread::spawn(move || advancing.advance());
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let text =
        p.a.send_text(
            p.srv.addr.clone(),
            p.srv.server_pub.to_vec(),
            p.srv.domain.clone(),
            p.bid.clone(),
            "control while bulk is stalled".into(),
        )
        .unwrap();
    assert_eq!(
        p.a.message_status(text).unwrap(),
        Some(DeliveryState::Accepted)
    );
    assert_eq!(p.fetch().received.len(), 1);
    let start = Instant::now();
    transfer.cancel();
    assert_eq!(step.join().unwrap(), Err(FfiError::MessageUnavailable));
    assert!(start.elapsed() < Duration::from_secs(1));
    proxy.join().unwrap();
    assert_eq!(
        p.a.commit_voice_transfer(transfer),
        Err(FfiError::MessageUnavailable)
    );
    assert_eq!(p.a.pending_voice_upload().unwrap(), Some(queued));
    let report = p.retry();
    assert!(report.skipped >= 1 && report.resent >= 1);
}

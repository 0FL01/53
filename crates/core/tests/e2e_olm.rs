//! K3 evidence: два core-инстанса друг другу через живой msgd.
//!
//! Схема: живой msgd на localhost (как K2-харнес) → enrol A и B двумя
//! invite → обмен contact-QR → ensure_prekeys (refill) → A→B текст →
//! E2E-проверка «сервер видит только ciphertext» (сохранённый ciphertext
//! из outbox A отличается от plaintext и не содержит его; расшифровка у B
//! успешна) → claim съел один ключ B (COUNT 15) → retry тем же ciphertext
//! после доставки даёт ST_DELIVERED без дубликата (дедуп сервера) →
//! обратный текст B→A по тем же сессиям.
//!
//! Секреты (token, ключи) — только файлами/памятью, в вывод не печатаются.

use dmsg_core::{contacts, enrol_from_qr, Core};
use dmsg_protocol::bootstrap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

const FAKE_DER: &[u8] = &[0x30, 0x03, 0x01, 0x01, 0x00];
const DOMAIN: &str = "k3.test";

fn msgd_bin() -> PathBuf {
    if let Ok(p) = std::env::var("MSGD_BIN") {
        let p = PathBuf::from(p);
        assert!(p.exists(), "MSGD_BIN points nowhere: {}", p.display());
        return p;
    }
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cand = here.join("../../target/debug/msgd");
    assert!(
        cand.exists(),
        "msgd binary not found at {}; run `cargo build -p msgd` first",
        cand.display()
    );
    cand
}

struct LiveMsgd {
    child: Child,
    dir: PathBuf,
    addr: String,
}

impl LiveMsgd {
    fn start() -> Self {
        let bin = msgd_bin();
        let dir = std::env::temp_dir().join(format!("dmsg-k3e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let key = dir.join("noise_key");
        let out = Command::new(&bin).args(["keygen", "--out"]).arg(&key).output().expect("keygen");
        assert!(out.status.success(), "keygen failed: {out:?}");
        std::fs::write(dir.join("carrier.der"), FAKE_DER).expect("carrier");
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            l.local_addr().unwrap().port()
        };
        let child = Command::new(&bin)
            .env("DMSG_DOMAIN", DOMAIN)
            .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
            .env("MSGD_DATA_DIR", dir.join("data"))
            .env("MSGD_BLOBS_DIR", dir.join("blobs"))
            .env("MSGCTL_SOCK", dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", &key)
            .env("CARRIER_CERT_FILE", dir.join("carrier.der"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("msgd spawn");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "msgd not ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
        Self { child, dir, addr: format!("127.0.0.1:{port}") }
    }

    fn issue(&self, name: &str) -> String {
        let out_file = self.dir.join(name);
        let out = Command::new(msgd_bin())
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .arg("msgctl")
            .arg("invite-issue")
            .arg("--out-file")
            .arg(&out_file)
            .arg("3600")
            .output()
            .expect("invite-issue");
        assert!(out.status.success(), "invite-issue failed: {out:?}");
        std::fs::read_to_string(&out_file).expect("read uri").trim().to_string()
    }
}

impl Drop for LiveMsgd {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Подключить транспорт ядра (Noise static из store, server key из invite).
async fn connect(db: &std::path::Path, uri: &str, addr: &str) -> dmsg_core::DirectTcp {
    let b = bootstrap::parse(uri).expect("uri parses");
    let conn = dmsg_core::store::open(db).expect("open");
    let privk = dmsg_core::store::load_identity(&conn).expect("id").expect("enrolled");
    drop(conn);
    dmsg_core::initiate_with_key(addr, &b.noise_pubkey, &b.domain, &privk)
        .await
        .expect("connect")
}

#[tokio::test]
async fn two_cores_talk_e2e_through_live_msgd() {
    let srv = LiveMsgd::start();
    let uri_a = srv.issue("invite-a");
    let uri_b = srv.issue("invite-b");
    let db_a = srv.dir.join("a-core.db");
    let db_b = srv.dir.join("b-core.db");
    let ea = enrol_from_qr(&uri_a, &srv.addr, &db_a, Some(FAKE_DER)).await.expect("enrol a");
    let eb = enrol_from_qr(&uri_b, &srv.addr, &db_b, Some(FAKE_DER)).await.expect("enrol b");

    let mut a = Core::open(&db_a).expect("core a");
    let mut b = Core::open(&db_b).expect("core b");

    // Обмен contact-QR тем же bootstrap-конвертом (другой type) + accept.
    let (aed, acurve) = a.identity_keys();
    let (bed, bcurve) = b.identity_keys();
    let aqr = contacts::build_qr(&ea.contact_id, &ea.user_id, &a.device_pub(), &aed, &acurve)
        .expect("aqr");
    let bqr = contacts::build_qr(&eb.contact_id, &eb.user_id, &b.device_pub(), &bed, &bcurve)
        .expect("bqr");
    assert_eq!(a.add_contact_qr(&bqr), Ok(contacts::QrResult::Added));
    assert_eq!(b.add_contact_qr(&aqr), Ok(contacts::QrResult::Added));
    a.accept_contact(&eb.contact_id).expect("a accept");
    b.accept_contact(&ea.contact_id).expect("b accept");

    let mut ta = connect(&db_a, &uri_a, &srv.addr).await;
    let mut tb = connect(&db_b, &uri_b, &srv.addr).await;

    // Refill-побудка reconnect: пустой запас → догрузка до TARGET.
    let left_a = a.on_reconnect(&mut ta).await.expect("refill a");
    let left_b = b.on_reconnect(&mut tb).await.expect("refill b");
    assert_eq!((left_a, left_b), (16, 16), "fresh refill must hit target");

    // A→B: первое сообщение (prekey для B).
    let mid1 = a.send_text(&mut ta, &eb.contact_id, "hello bob").await.expect("send1");
    // E2E: сервер видел только ciphertext — сохранённые байты отличаются
    // от plaintext и не содержат его.
    let ct1 = a.outbox_ciphertext(&mid1).expect("ct1");
    assert!(!ct1.is_empty() && ct1 != b"hello bob");
    assert!(
        ct1.windows(b"hello bob".len()).all(|w| w != b"hello bob"),
        "ciphertext must not contain plaintext"
    );

    // B забирает и расшифровывает.
    let r1 = b.fetch_and_decrypt(&mut tb).await.expect("fetch1");
    assert_eq!(r1.received.len(), 1);
    assert_eq!(r1.received[0].text, "hello bob");
    assert_eq!(r1.received[0].contact_id, ea.contact_id);
    assert_eq!((r1.skipped_unknown, r1.skipped_blocked, r1.skipped_undecryptable, r1.skipped_mismatch), (0, 0, 0, 0));

    // Claim A съел один one-time B: COUNT 15 (атомарный consume).
    let left_b2 = b.on_reconnect(&mut tb).await.expect("count b");
    assert_eq!(left_b2, 15, "one claimed key must be consumed");

    // Ретрай тем же ciphertext после доставки: ST_DELIVERED, без дубликата.
    let stats = a.retry_queued(&mut ta).await.expect("retry");
    assert_eq!((stats.resent, stats.delivered), (1, 1));
    let r_empty = b.fetch_and_decrypt(&mut tb).await.expect("fetch-empty");
    assert!(r_empty.received.is_empty(), "server dedup: no duplicate event");
    assert!(r_empty.cursor >= r1.cursor, "an empty fetch must retain the durable server cursor");

    // Обратное направление по установленным сессиям (normal-сообщение).
    let mid2 = b.send_text(&mut tb, &ea.contact_id, "hi alice").await.expect("send2");
    let ct2 = b.outbox_ciphertext(&mid2).expect("ct2");
    assert!(ct2.windows(b"hi alice".len()).all(|w| w != b"hi alice"));
    let r2 = a.fetch_and_decrypt(&mut ta).await.expect("fetch2");
    assert_eq!(r2.received.len(), 1);
    assert_eq!((r2.received[0].text.as_str(), r2.received[0].contact_id.as_str()), ("hi alice", eb.contact_id.as_str()));

    // Global mailbox seq now alternates recipients. Both directions must drain
    // despite those gaps, with no replay counted as an undecryptable event.
    let a_empty = a.fetch_and_decrypt(&mut ta).await.expect("a second fetch");
    assert!(a_empty.received.is_empty());
    a.send_text(&mut ta, &eb.contact_id, "after reverse gap")
        .await
        .expect("send3");
    let r3 = b.fetch_and_decrypt(&mut tb).await.expect("fetch3");
    assert_eq!(r3.received.len(), 1);
    assert_eq!(r3.received[0].text, "after reverse gap");
    let b_empty = b
        .fetch_and_decrypt(&mut tb)
        .await
        .expect("b second fetch after gap");
    assert!(b_empty.received.is_empty());
    b.send_text(&mut tb, &ea.contact_id, "after forward gap")
        .await
        .expect("send4");
    let r4 = a.fetch_and_decrypt(&mut ta).await.expect("fetch4");
    assert_eq!(r4.received.len(), 1);
    assert_eq!(r4.received[0].text, "after forward gap");
    let a_empty2 = a
        .fetch_and_decrypt(&mut ta)
        .await
        .expect("a second fetch after new gap");
    assert!(a_empty2.received.is_empty());
    let skips: Vec<_> = [&r2, &a_empty, &r3, &b_empty, &r4, &a_empty2]
        .iter()
        .map(|r| {
            (
                r.skipped_unknown,
                r.skipped_blocked,
                r.skipped_undecryptable,
                r.skipped_mismatch,
            )
        })
        .collect();
    assert_eq!(
        skips,
        vec![(0, 0, 0, 0); 6],
        "bidirectional fetches must have zero skipped events"
    );

    // Android's synchronous facade opens a fresh transport for EVERY command.
    // Each one must authenticate with the persisted Noise static, not a new key.
    drop(a);
    drop(b);
    let server_pub = bootstrap::parse(&uri_a).expect("bootstrap").noise_pubkey.to_vec();
    let addr = srv.addr.clone();
    tokio::task::spawn_blocking(move || {
        let a = dmsg_core::ffi::DmsgClient::open(db_a.to_string_lossy().into_owned());
        let b = dmsg_core::ffi::DmsgClient::open(db_b.to_string_lossy().into_owned());
        for _ in 0..2 {
            assert!(a.reconnect(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("ffi reconnect a") >= 8);
            assert!(b.reconnect(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("ffi reconnect b") >= 8);
        }
        let mid = a.send_text(addr.clone(), server_pub.clone(), DOMAIN.into(), eb.contact_id.clone(),
            "ffi roundtrip".into()).expect("ffi send");
        assert_eq!(mid.len(), 32);
        let fetched = b.fetch(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("ffi fetch");
        assert_eq!(fetched.received.len(), 1);
        assert_eq!(fetched.received[0].text, "ffi roundtrip");
        let retry = a.retry_queued(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("ffi retry");
        assert!(retry.delivered >= 1);
        assert!(b.fetch(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("ffi dedup fetch").received.is_empty());
        assert_eq!(a.account_info().expect("ffi account").contact_id, Some(ea.contact_id));

        drop(a);
        drop(b);
        // Migrate live plaintext identities; every subsequent FFI operation
        // must take the encrypted path through store, enrol and chat.
        let a = dmsg_core::ffi::DmsgClient::open_encrypted(db_a.to_string_lossy().into_owned(), vec![19;32])
            .expect("sealed a");
        let b = dmsg_core::ffi::DmsgClient::open_encrypted(db_b.to_string_lossy().into_owned(), vec![20;32])
            .expect("sealed b");
        a.reconnect(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("sealed reconnect");
        let mid = a.send_text(addr.clone(), server_pub.clone(), DOMAIN.into(), eb.contact_id.clone(),
            "sealed ffi message".into()).expect("sealed send");
        assert_eq!(mid.len(), 32);
        let report = b.fetch(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("sealed fetch");
        assert_eq!(report.received[0].text, "sealed ffi message");
        assert!(a.retry_queued(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("sealed retry").delivered >= 1);
        assert!(b.inbox_page(0, 100).expect("sealed inbox page").rows.iter().any(|r| r.text == "sealed ffi message"));

        let before_bad_key = a.outbox_page(0, 100).expect("before bad key").rows.len();
        assert!(matches!(a.send_text(addr.clone(), vec![42; 32], DOMAIN.into(),
            eb.contact_id.clone(), "must not queue".into()), Err(dmsg_core::ffi::FfiError::Transport(_))));
        assert_eq!(a.outbox_page(0, 100).expect("after bad key").rows.len(), before_bad_key,
            "a failed Noise handshake cannot enqueue offline ciphertext");

        // FFI dispatch with no network uses the already persisted Olm session;
        // retry after restoration sends the exact ciphertext committed offline.
        let offline_addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            let addr = listener.local_addr().expect("addr").to_string();
            drop(listener);
            addr
        };
        let offline_mid = a.send_text(offline_addr.clone(), server_pub.clone(), DOMAIN.into(),
            eb.contact_id.clone(), "offline-to-online".into()).expect("offline queued");
        let queued = a.outbox_page(0, 100).expect("queued page");
        assert_eq!(queued.rows.iter().filter(|r| r.message_id_hex == offline_mid && r.status == "queued").count(), 1);
        let saved = dmsg_core::store::outbox_queued(
            &dmsg_core::store::open_encrypted(&db_a, &[19; 32]).expect("sealed db"), 0, 100
        ).expect("saved rows").0.into_iter().find(|(_, mid, _, _, _)| {
            mid.iter().map(|b| format!("{b:02x}")).collect::<String>() == offline_mid
        }).expect("offline row").3;
        assert!(saved.windows(b"offline-to-online".len()).all(|w| w != b"offline-to-online"));
        assert!(b.fetch(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("no premature send").received.is_empty());
        let stats = a.retry_queued(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("restored retry");
        assert!(stats.resent >= 1);
        let after = dmsg_core::store::outbox_queued(
            &dmsg_core::store::open_encrypted(&db_a, &[19; 32]).expect("sealed db"), 0, 100
        ).expect("saved rows").0.into_iter().find(|(_, mid, _, _, _)| {
            mid.iter().map(|b| format!("{b:02x}")).collect::<String>() == offline_mid
        }).expect("restored row").3;
        assert_eq!(saved, after, "retry must use the byte-identical ciphertext from offline queue");
        let restored = b.fetch(addr.clone(), server_pub.clone(), DOMAIN.into()).expect("restored fetch");
        assert_eq!(restored.received.len(), 1);
        assert_eq!(restored.received[0].text, "offline-to-online");
        assert_eq!(restored.received[0].message_id_hex, offline_mid);

        assert!(dmsg_core::ffi::DmsgClient::open(db_a.to_string_lossy().into_owned()).account_info().is_err());
        assert!(dmsg_core::ffi::DmsgClient::open_encrypted(db_a.to_string_lossy().into_owned(), vec![21;32]).is_err());
    }).await.expect("ffi worker");
}

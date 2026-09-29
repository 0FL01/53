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

    // Обратное направление по установленным сессиям (normal-сообщение).
    let mid2 = b.send_text(&mut tb, &ea.contact_id, "hi alice").await.expect("send2");
    let ct2 = b.outbox_ciphertext(&mid2).expect("ct2");
    assert!(ct2.windows(b"hi alice".len()).all(|w| w != b"hi alice"));
    let r2 = a.fetch_and_decrypt(&mut ta).await.expect("fetch2");
    assert_eq!(r2.received.len(), 1);
    assert_eq!((r2.received[0].text.as_str(), r2.received[0].contact_id.as_str()), ("hi alice", eb.contact_id.as_str()));
}

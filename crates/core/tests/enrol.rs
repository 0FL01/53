//! K2 evidence: offline-enrol против живого msgd на localhost.
//!
//! Бинарь msgd собирается из crates/server (`cargo build -p msgd` перед
//! тестом). Invite выпускается через `msgctl invite-issue --out-file` (URI с
//! token — только в файл 0600, не в stdout/логи). Carrier-cert — минимальный
//! DER-блоб (direct-TCP не предъявляет живой серт; полный DER из QR сверяется
//! побайтово с ожидаемым pin до сети).
//! Кейсы: happy+reconnect тем же ключом (replay), used-token вторым ключом →
//! BoundOther, неверный pin (до сети), expired, revoked.

use dmsg_core::{enrol_from_qr, EnrolError};
use dmsg_protocol::bootstrap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Минимальный валидный DER для bootstrap (парсер требует 0x30 + длина).
const FAKE_DER: &[u8] = &[0x30, 0x03, 0x01, 0x01, 0x00];

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

fn hex_to32(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64);
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    b
}

struct LiveMsgd {
    child: Child,
    dir: PathBuf,
    addr: String,
}

impl LiveMsgd {
    fn start(tag: &str, domain: &str) -> Self {
        let bin = msgd_bin();
        let dir = std::env::temp_dir().join(format!("dmsg-k2-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let key = dir.join("noise_key");
        let out = Command::new(&bin).args(["keygen", "--out"]).arg(&key).output().expect("keygen");
        assert!(out.status.success(), "keygen failed: {out:?}");
        let _server_pub = hex_to32(String::from_utf8(out.stdout).unwrap().trim());
        std::fs::write(dir.join("carrier.der"), FAKE_DER).expect("carrier");

        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            l.local_addr().unwrap().port()
        };
        let child = Command::new(&bin)
            .env("DMSG_DOMAIN", domain)
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

    /// Выпустить invite; URI читается из файла 0600 (token не в stdout).
    fn issue(&self, name: &str, ttl: &str) -> String {
        let out_file = self.dir.join(name);
        let out = Command::new(msgd_bin())
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .arg("msgctl")
            .arg("invite-issue")
            .arg("--out-file")
            .arg(&out_file)
            .arg(ttl)
            .output()
            .expect("invite-issue");
        assert!(out.status.success(), "invite-issue failed: {out:?}");
        std::fs::read_to_string(&out_file).expect("read uri").trim().to_string()
    }

    fn revoke(&self, token_hex_path: &std::path::Path) {
        let out = Command::new(msgd_bin())
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .arg("msgctl")
            .arg("invite-revoke")
            .arg("--file")
            .arg(token_hex_path)
            .output()
            .expect("invite-revoke");
        assert!(out.status.success(), "invite-revoke failed: {out:?}");
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "ok");
    }
}

impl Drop for LiveMsgd {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn token_hex_file(srv: &LiveMsgd, name: &str, uri: &str) -> PathBuf {
    let b = bootstrap::parse(uri).expect("uri parses");
    let hex: String = b.token.iter().map(|x| format!("{x:02x}")).collect();
    let p = srv.dir.join(name);
    std::fs::write(&p, format!("{hex}\n")).expect("hex file");
    p
}

#[tokio::test]
async fn happy_reconnect_and_second_key_refused() {
    let srv = LiveMsgd::start("happy", "k2.test");
    let uri = srv.issue("invite1", "3600");
    // Preview без сети: домен + fingerprint полного DER.
    let pv = dmsg_core::preview(&uri).expect("preview");
    assert_eq!(pv.domain, "k2.test");
    assert_eq!(pv.pin_fingerprint, dmsg_core::enrol::pin_fingerprint(FAKE_DER));

    let db_a = srv.dir.join("a-core.db");
    let first = enrol_from_qr(&uri, &srv.addr, &db_a, Some(FAKE_DER)).await.expect("enrol");
    assert_eq!(first.contact_id.len(), 12);
    // Store: static + account персистентны, файл 0600.
    let conn = dmsg_core::store::open(&db_a).expect("reopen");
    assert!(dmsg_core::store::load_identity(&conn).expect("identity").is_some());
    let (uid, cid) = dmsg_core::store::load_account(&conn).expect("account").expect("enrolled");
    assert_eq!((uid, cid), (first.user_id, first.contact_id.clone()));
    drop(conn);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&db_a).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "core db must be 0600");
    }
    // Reconnect тем же ключом (тот же store): серверный replay, тот же итог.
    let second = enrol_from_qr(&uri, &srv.addr, &db_a, Some(FAKE_DER)).await.expect("reconnect");
    assert_eq!(first, second);
    // Used-token вторым ключом (другой store): отказ BoundOther, без клонирования.
    let db_b = srv.dir.join("b-core.db");
    let r = enrol_from_qr(&uri, &srv.addr, &db_b, Some(FAKE_DER)).await;
    assert_eq!(r, Err(EnrolError::BoundOther));
}

#[tokio::test]
async fn wrong_pin_refused_without_network() {
    let srv = LiveMsgd::start("pin", "k2.test");
    let uri = srv.issue("invite1", "3600");
    let b = bootstrap::parse(&uri).expect("parse");
    let mut tampered = b.cert_der.clone();
    tampered[2] ^= 0xFF;
    let bad_uri =
        bootstrap::build(&b.domain, &tampered, &b.noise_pubkey, &b.token).expect("rebuild");
    // Pin не сошёлся — отказ до сети (сервер жив, но dial не нужен для отказа).
    let r =
        enrol_from_qr(&bad_uri, &srv.addr, &srv.dir.join("core.db"), Some(FAKE_DER)).await;
    assert_eq!(r, Err(EnrolError::PinMismatch));
}

#[tokio::test]
async fn expired_and_revoked_refused() {
    let srv = LiveMsgd::start("exp", "k2.test");
    // Короткий TTL: ждём истечения, затем ENROL → Expired.
    let uri = srv.issue("short", "2");
    std::thread::sleep(Duration::from_secs(3));
    let r = enrol_from_qr(&uri, &srv.addr, &srv.dir.join("s-core.db"), Some(FAKE_DER)).await;
    assert_eq!(r, Err(EnrolError::Expired));
    // Revoke живого invite → Revoked.
    let uri2 = srv.issue("long", "3600");
    let hexf = token_hex_file(&srv, "tok.hex", &uri2);
    srv.revoke(&hexf);
    let r2 = enrol_from_qr(&uri2, &srv.addr, &srv.dir.join("r-core.db"), Some(FAKE_DER)).await;
    assert_eq!(r2, Err(EnrolError::Revoked));
}

#[tokio::test]
async fn wrong_noise_key_refused() {
    let srv = LiveMsgd::start("key", "k2.test");
    let uri = srv.issue("invite1", "3600");
    // Подменяем pinned Noise-публичник в QR: handshake обязан не сойтись.
    let b = bootstrap::parse(&uri).expect("parse");
    let mut pubkey = b.noise_pubkey;
    pubkey[0] ^= 0xFF;
    let bad_uri = bootstrap::build(&b.domain, &b.cert_der, &pubkey, &b.token).expect("rebuild");
    let r =
        enrol_from_qr(&bad_uri, &srv.addr, &srv.dir.join("core.db"), Some(FAKE_DER)).await;
    assert!(matches!(r, Err(EnrolError::KeyMismatch(_))), "got {r:?}");
}

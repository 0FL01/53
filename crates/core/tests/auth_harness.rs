//! K1 evidence: AUTH→WELCOME против живого msgd на localhost.
//!
//! Бинарь собирается из crates/server (`cargo build -p msgd` перед тестом;
//! вложенный cargo из-под `cargo test` не зовём — target-lock). Тест поднимает
//! msgd на свободном порту, делает [`dmsg_core::initiate`] и требует WELCOME.
//! Секреты (noise-приватник) — только файлами, в тест-вывод не печатаются.

use dmsg_core::Transport;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

const DOMAIN: &str = "k1.test";

/// Найти бинарь msgd: MSGD_BIN или workspace target/debug/msgd.
fn msgd_bin() -> PathBuf {
    if let Ok(p) = std::env::var("MSGD_BIN") {
        let p = PathBuf::from(p);
        assert!(p.exists(), "MSGD_BIN points nowhere: {}", p.display());
        return p;
    }
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/core/../../target/debug/msgd == <root>/target/debug/msgd
    let cand = here.join("../../target/debug/msgd");
    assert!(
        cand.exists(),
        "msgd binary not found at {}; run `cargo build -p msgd` first",
        cand.display()
    );
    cand
}

fn hex_to32(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64, "pubkey hex must be 64 chars");
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    b
}

/// Живой msgd на свободном localhost-порту. Drop — kill + чистка tmpdir.
struct LiveMsgd {
    child: Child,
    dir: PathBuf,
    addr: String,
    server_pub: [u8; 32],
    domain: String,
}

impl LiveMsgd {
    fn start(tag: &str, domain: &str) -> Self {
        let bin = msgd_bin();
        let dir = std::env::temp_dir().join(format!("dmsg-k1-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let key = dir.join("noise_key");
        let _ = std::fs::remove_file(&key);

        // keygen печатает public hex в stdout (приватник — только в файл).
        let out = Command::new(&bin).args(["keygen", "--out"]).arg(&key).output().expect("keygen");
        assert!(out.status.success(), "keygen failed: {out:?}");
        let server_pub = hex_to32(String::from_utf8(out.stdout).unwrap().trim());

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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("msgd spawn");

        // Ждём готовности TCP (до 5s).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "msgd not ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { child, dir, addr: format!("127.0.0.1:{port}"), server_pub, domain: domain.into() }
    }
}

impl Drop for LiveMsgd {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

#[tokio::test]
async fn auth_welcome_against_live_msgd() {
    let srv = LiveMsgd::start("happy", DOMAIN);
    let ch = dmsg_core::initiate(&srv.addr, &srv.server_pub, srv.domain.as_bytes())
        .await
        .expect("AUTH→WELCOME against live msgd");
    assert!(ch.is_connected(), "channel must be connected after WELCOME");
}

#[tokio::test]
async fn wrong_domain_gets_no_welcome() {
    let srv = LiveMsgd::start("wrongdom", DOMAIN);
    let r = dmsg_core::initiate(&srv.addr, &srv.server_pub, b"evil.example").await;
    assert!(r.is_err(), "чужой домен обязан не получить WELCOME");
}

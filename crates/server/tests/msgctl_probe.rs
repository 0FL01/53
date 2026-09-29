//! P5.1 evidence: msgctl quotas/user-list/device-unblock — форматы и ошибки.
//! Гоняется на свежем сервере; детектор регрессий формата ответов.

use std::process::{Child, Command};
use std::time::Duration;

struct Server {
    child: Child,
    dir: std::path::PathBuf,
}

fn start(port: u16) -> Server {
    let dir = std::env::temp_dir().join(format!("msgd-mc-{port}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_msgd");
    let key = dir.join("noise_key");
    let out = Command::new(bin)
        .args(["keygen", "--out"])
        .arg(&key)
        .output()
        .expect("keygen");
    assert!(out.status.success(), "keygen failed");
    let child = Command::new(bin)
        .env("DMSG_DOMAIN", "ctl.test")
        .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
        .env("MSGD_DATA_DIR", dir.join("data"))
        .env("MSGD_BLOBS_DIR", dir.join("blobs"))
        .env("MSGCTL_SOCK", dir.join("ctl.sock"))
        .env("NOISE_KEY_FILE", &key)
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
    Server { child, dir }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn ctl(srv: &Server, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_msgd"))
        .env("MSGCTL_SOCK", srv.dir.join("ctl.sock"))
        .arg("msgctl")
        .args(args)
        .output()
        .expect("msgctl");
    assert!(out.status.success(), "msgctl failed: {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn empty_db_formats() {
    let srv = start(17201);
    assert_eq!(ctl(&srv, &["user-list"]), "ok\n");
    assert_eq!(
        ctl(&srv, &["quotas"]),
        "limits events=512 bytes=33554432\nok\n"
    );
}

#[test]
fn bad_inputs_err() {
    let srv = start(17202);
    assert_eq!(ctl(&srv, &["quotas", "%"]), "err\n");
    assert_eq!(ctl(&srv, &["quotas", "way-too-long-filter"]), "err\n");
    assert_eq!(ctl(&srv, &["device-unblock", "zz"]), "err\n");
    assert_eq!(ctl(&srv, &["device-unblock"]), "err\n");
}

#[test]
fn unblock_roundtrip() {
    let srv = start(17203);
    // несуществующий ключ: UPDATE трогает 0 строк, но это ok (идемпотентно)
    let zero = "00".repeat(32);
    assert_eq!(ctl(&srv, &["device-unblock", &zero]), "ok\n");
    assert_eq!(ctl(&srv, &["user-list"]), "ok\n");
}

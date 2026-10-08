//! P5.2 evidence: backup → restore → start.
//! Доказывает только: копия консистентна, свежий сервер стартует,
//! dbversion и invite-list совпадают. НЕ доказывает смерть диска,
//! power-loss, гонку blobs (зафиксировано в runbook).

use std::process::{Child, Command};
use std::time::Duration;

struct Server {
    child: Child,
    port: u16,
}

fn spawn(port: u16, dir: &std::path::Path, key: &std::path::Path) -> Child {
    let _ = port; // готовность — по msgctl pong, порт нужен вызывающему для TCP
    let bin = env!("CARGO_BIN_EXE_msgd");
    let cert = dir.parent().expect("base").join("carrier.pem");
    if !cert.exists() {
        let csr = Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=x",
                "-keyout",
            ])
            .arg(dir.parent().expect("base").join("k.pem"))
            .arg("-out")
            .arg(&cert)
            .output();
        assert!(
            csr.map(|o| o.status.success()).unwrap_or(false),
            "openssl req"
        );
    }
    let child = Command::new(bin)
        .env("DMSG_DOMAIN", "bak.test")
        .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
        .env("MSGD_DATA_DIR", dir)
        .env("MSGD_BLOBS_DIR", dir.join("blobs"))
        .env("MSGCTL_SOCK", dir.join("ctl.sock"))
        .env("NOISE_KEY_FILE", key)
        .env("CARRIER_CERT_FILE", &cert)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("msgd spawn");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let pong = Command::new(env!("CARGO_BIN_EXE_msgd"))
            .env("MSGCTL_SOCK", dir.join("ctl.sock"))
            .args(["msgctl", "ping"])
            .output()
            .map(|o| o.stdout == b"pong\n")
            .unwrap_or(false);
        if pong {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "msgd not ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}

fn ctl(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_msgd"))
        .env("MSGCTL_SOCK", dir.join("ctl.sock"))
        .arg("msgctl")
        .args(args)
        .output()
        .expect("msgctl");
    assert!(out.status.success(), "msgctl failed: {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn backup_restore_start() {
    let base = std::env::temp_dir().join(format!("msgd-bk-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let bin = env!("CARGO_BIN_EXE_msgd");
    let key = base.join("noise_key");
    let out = Command::new(bin)
        .args(["keygen", "--out"])
        .arg(&key)
        .output()
        .expect("keygen");
    assert!(out.status.success());
    let data = base.join("data");
    // Порты от pid: зависший процесс прошлого прогона не должен ронять новый.
    let p0 = 17300 + (std::process::id() % 400) as u16;
    let mut srv = Server {
        child: spawn(p0, &data, &key),
        port: p0,
    };
    let invite = base.join("invite.txt");
    assert_eq!(
        ctl(
            &data,
            &["invite-issue", "--out-file", invite.to_str().unwrap()]
        ),
        "ok\n"
    );
    assert_eq!(ctl(&data, &["registration-mode", "open"]), "open\n");
    let conn = rusqlite::Connection::open(data.join("msgd.db")).unwrap();
    use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
    let salt = SaltString::encode_b64(&[19; 16]).unwrap();
    let hash = Argon2::default()
        .hash_password(b"backup-test-password", &salt)
        .unwrap()
        .to_string();
    conn.execute(
        "INSERT INTO users VALUES(?1,'0123456789AB','backup',?2,1)",
        rusqlite::params![[19u8; 16].as_slice(), hash],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
        rusqlite::params![[20u8; 32].as_slice(), [19u8; 16].as_slice()],
    )
    .unwrap();
    drop(conn);
    let before = ctl(&data, &["invite-list"]);
    let rep = ctl(&data, &["backup"]);
    assert!(rep.starts_with("backup path="), "bad reply: {rep}");
    let snap = rep
        .strip_prefix("backup ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|p| p.strip_prefix("path="))
        .expect("path in reply");
    assert!(std::path::Path::new(snap).join("msgd.db").exists());
    srv.child.kill().ok();
    let _ = srv.child.wait();
    // Рестор на отдельную копию: подмена db (+ удалить wal/shm, если есть).
    let rest = base.join("restored");
    std::fs::create_dir_all(&rest).unwrap();
    std::fs::copy(
        std::path::Path::new(snap).join("msgd.db"),
        rest.join("msgd.db"),
    )
    .unwrap();
    for suf in ["-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(rest.join(format!("msgd.db{suf}")));
    }
    let mut srv2 = Server {
        child: spawn(p0 + 1, &rest, &key),
        port: p0 + 1,
    };
    assert_eq!(ctl(&rest, &["dbversion"]), "7\n");
    assert_eq!(ctl(&rest, &["invite-list"]), before);
    assert_eq!(ctl(&rest, &["registration-mode"]), "open\n");
    let conn = rusqlite::Connection::open(rest.join("msgd.db")).unwrap();
    let restored_hash: String = conn
        .query_row(
            "SELECT password_hash FROM users WHERE login='backup'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(restored_hash, hash);
    use argon2::{password_hash::PasswordHash, PasswordVerifier};
    assert!(Argon2::default()
        .verify_password(
            b"backup-test-password",
            &PasswordHash::new(&restored_hash).unwrap()
        )
        .is_ok());
    drop(conn);
    srv2.child.kill().ok();
    std::fs::remove_dir_all(&base).ok();
    let _ = srv.port;
}

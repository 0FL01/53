//! P5.1 evidence: msgctl quotas/user-list/device-unblock — форматы и ошибки.
//! R3: секретные команды только через --file (argv-hex удалён),
//! invite-issue --out-file 0600 refuse-if-exists, строгие args, line-too-long.

use std::os::unix::net::UnixStream;
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
    // CARRIER_CERT_FILE: самоподписанный серт для сборки URI (DER целиком).
    let cert = dir.join("carrier.pem");
    let csr = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
               "-subj", "/CN=x", "-keyout"])
        .arg(dir.join("k.pem"))
        .arg("-out")
        .arg(&cert)
        .output();
    assert!(csr.map(|o| o.status.success()).unwrap_or(false), "openssl req");
    let child = Command::new(bin)
        .env("DMSG_DOMAIN", "ctl.test")
        .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
        .env("MSGD_DATA_DIR", dir.join("data"))
        .env("MSGD_BLOBS_DIR", dir.join("blobs"))
        .env("MSGCTL_SOCK", dir.join("ctl.sock"))
        .env("NOISE_KEY_FILE", &key)
        .env("CARRIER_CERT_FILE", &cert)
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
    let out = ctl_full(srv, args);
    assert!(out.status.success(), "msgctl failed: {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn ctl_full(srv: &Server, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_msgd"))
        .env("MSGCTL_SOCK", srv.dir.join("ctl.sock"))
        .arg("msgctl")
        .args(args)
        .output()
        .expect("msgctl")
}

/// Сырая строка прямо в сокет (мимо CLI-клиента): серверная строгость.
fn raw(srv: &Server, line: &str) -> String {
    use std::io::{Read, Write};
    let mut s = UnixStream::connect(srv.dir.join("ctl.sock")).unwrap();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn write_file(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

fn seed_rebind_account(srv: &Server) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(srv.dir.join("data/msgd.db")).unwrap();
    db.execute("INSERT INTO users VALUES(?1,'0123456789AB',1)", [[31u8; 16].as_slice()]).unwrap();
    db.execute("INSERT INTO devices VALUES(?1,?2,1,0)",
        rusqlite::params![[32u8; 32].as_slice(), [31u8; 16].as_slice()],
    ).unwrap();
    db.execute("INSERT INTO invites(token,created_at,expires_at,bound_device_key) VALUES(?1,1,2,?2)",
        rusqlite::params![[33u8; 32].as_slice(), [32u8; 32].as_slice()],
    ).unwrap();
    db
}

fn rebind_counts(db: &rusqlite::Connection) -> (i64, i64, i64) {
    db.query_row("SELECT (SELECT COUNT(*) FROM invites),
        (SELECT COUNT(*) FROM invites WHERE revoked=0),
        (SELECT COUNT(*) FROM devices WHERE revoked=0)", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap()
}

#[test]
fn rebind_requires_public_key_file_and_secret_output_file() {
    let srv = start(17418);
    let db = seed_rebind_account(&srv);
    let source = write_file(&srv.dir, "public.hex", &format!("{}\n", "20".repeat(32)));
    let dest = srv.dir.join("rebind.txt");
    let source = source.to_str().unwrap();
    let dest = dest.to_str().unwrap();
    for args in [
        vec!["invite-rebind"],
        vec!["invite-rebind", "--file", source],
        vec!["invite-rebind", "--out-file", dest],
        vec!["invite-rebind", "--file", source, "--out-file", dest, "--file", source],
        vec!["invite-rebind", "--file", source, "--out-file", dest, "bad"],
    ] {
        let out = ctl_full(&srv, &args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert_eq!(rebind_counts(&db), (1, 1, 1));
    }
    let out = ctl_full(&srv, &["invite-rebind", "--file", source, "--out-file", dest, "3600"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ok\n");
    assert!(out.stderr.is_empty());
    let body = std::fs::read_to_string(dest).unwrap();
    let invite = dmsg_protocol::bootstrap::parse(body.trim()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dest).unwrap().permissions().mode() & 0o777, 0o600);
    let target: Vec<u8> = db.query_row("SELECT rebind_user_id FROM invites WHERE token=?1",
        [invite.token.as_slice()], |r| r.get(0),
    ).unwrap();
    assert_eq!(target, [31u8; 16]);
    assert_eq!(rebind_counts(&db), (2, 1, 0));
    // Existing output must fail BEFORE a second rebind request.
    let out = ctl_full(&srv, &["invite-rebind", "--file", source, "--out-file", dest]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read_to_string(dest).unwrap(), body);
    assert_eq!(rebind_counts(&db), (2, 1, 0));
    // A retry with a new destination supersedes the unreachable/unused invite.
    let retry = srv.dir.join("retry.txt");
    assert_eq!(ctl(&srv, &["invite-rebind", "--file", source, "--out-file", retry.to_str().unwrap()]), "ok\n");
    assert_eq!(rebind_counts(&db), (3, 1, 0));
    let revoked: i64 = db.query_row("SELECT revoked FROM invites WHERE token=?1",
        [invite.token.as_slice()], |r| r.get(0),
    ).unwrap();
    assert_eq!(revoked, 1);
}

#[test]
fn rebind_failures_are_static_and_have_no_db_side_effects() {
    let srv = start(17419);
    let db = seed_rebind_account(&srv);
    let source = write_file(&srv.dir, "public.hex", &"20".repeat(32));
    let bad = write_file(&srv.dir, "bad.hex", &"é".repeat(32));
    let unknown = write_file(&srv.dir, "unknown.hex", &"00".repeat(32));
    let dest = srv.dir.join("rebind.txt");
    let missing_parent = srv.dir.join("missing/rebind.txt");
    for (source, output, ttl) in [
        (&bad, &dest, "3600"), (&unknown, &dest, "3600"),
        (&source, &missing_parent, "3600"), (&source, &dest, "9223372036854775807"),
    ] {
        let out = ctl_full(&srv, &["invite-rebind", "--file", source.to_str().unwrap(),
            "--out-file", output.to_str().unwrap(), ttl]);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(matches!(err.as_str(), "invite-rebind: invalid-source\n" |
            "invite-rebind: rejected\n" | "invite-rebind: output-unavailable\n"));
        assert!(!output.exists());
        assert_eq!(rebind_counts(&db), (1, 1, 1));
    }
    std::fs::remove_file(srv.dir.join("carrier.pem")).unwrap();
    let out = ctl_full(&srv, &["invite-rebind", "--file", source.to_str().unwrap(),
        "--out-file", dest.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stderr, b"invite-rebind: rejected\n");
    assert_eq!(rebind_counts(&db), (1, 1, 1));
    assert!(!dest.exists());
    let key = "20".repeat(32);
    for command in [format!("invite-rebind {key} garbage"),
        format!("invite-rebind {key} 10 extra"), format!("invite-rebind {}", "é".repeat(32))] {
        assert_eq!(raw(&srv, &command), "err\n");
    }
}

#[test]
fn rebind_post_commit_output_failure_retires_token_without_unblocking_old() {
    let srv = start(17420);
    let db = seed_rebind_account(&srv);
    let source = write_file(&srv.dir, "public.hex", &"20".repeat(32));
    let dest = srv.dir.join("rebind.txt");
    // POSIX sh file-size limit = one 512-byte block; the full certificate URI
    // exceeds this. Ignore SIGXFSZ so write_all observes EFBIG and cleanup runs.
    let out = Command::new("sh")
        .args(["-c", "trap '' XFSZ; ulimit -f 1; exec \"$@\"", "rebind-cli"])
        .arg(env!("CARGO_BIN_EXE_msgd"))
        .arg("msgctl")
        .args(["invite-rebind", "--file"])
        .arg(&source)
        .arg("--out-file")
        .arg(&dest)
        .env("MSGCTL_SOCK", srv.dir.join("ctl.sock"))
        .output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"invite-rebind: output-failed\n");
    assert!(!dest.exists(), "partial secret output must be removed");
    assert_eq!(rebind_counts(&db), (2, 0, 0));
    // The same public key can safely issue a new invitation after the failure.
    let retry = srv.dir.join("retry.txt");
    assert_eq!(ctl(&srv, &["invite-rebind", "--file", source.to_str().unwrap(),
        "--out-file", retry.to_str().unwrap()]), "ok\n");
    assert_eq!(rebind_counts(&db), (3, 1, 0));
}

#[test]
fn empty_db_formats() {
    let srv = start(17411);
    assert_eq!(ctl(&srv, &["user-list"]), "ok\n");
    assert_eq!(
        ctl(&srv, &["quotas"]),
        "limits events=512 bytes=33554432\nok\n"
    );
}

#[test]
fn bad_inputs_err() {
    let srv = start(17412);
    assert_eq!(ctl(&srv, &["quotas", "%"]), "err\n");
    assert_eq!(ctl(&srv, &["quotas", "way-too-long-filter"]), "err\n");
    // argv-hex удалён: без --file — usage/exit 2, не сокет-err.
    assert!(!ctl_full(&srv, &["device-unblock", "zz"]).status.success());
    assert!(!ctl_full(&srv, &["device-unblock"]).status.success());
    assert!(!ctl_full(&srv, &["device-unblock", &"00".repeat(32)]).status.success());
    // --file с мусором — usage/exit 2.
    let bad = write_file(&srv.dir, "bad.hex", "zz\n");
    assert!(!ctl_full(&srv, &["device-unblock", "--file", bad.to_str().unwrap()]).status.success());
    // --file на несуществующий — exit 1.
    let missing = srv.dir.join("nope.hex");
    assert!(!ctl_full(&srv, &["device-unblock", "--file", missing.to_str().unwrap()]).status.success());
}

#[test]
fn unblock_roundtrip_via_file() {
    let srv = start(17413);
    // несуществующий ключ: UPDATE трогает 0 строк, но это ok (идемпотентно)
    let f = write_file(&srv.dir, "zero.hex", &format!("{}\n", "00".repeat(32)));
    assert_eq!(ctl(&srv, &["device-unblock", "--file", f.to_str().unwrap()]), "ok\n");
    assert_eq!(ctl(&srv, &["user-list"]), "ok\n");
}

#[test]
fn strict_extra_args_err() {
    let srv = start(17414);
    // Команды без параметров: любой лишний аргумент — err.
    for cmd in ["ping x", "stats x", "user-list x", "invite-list x", "gc x", "backup x", "dbversion x", "domain x"] {
        assert_eq!(raw(&srv, cmd), "err\n", "{cmd}");
    }
    // Команды с параметрами: лишние тоже err; мусор вместо ttl — err.
    assert_eq!(raw(&srv, "quotas A B"), "err\n");
    assert_eq!(raw(&srv, "invite-issue 10 20"), "err\n");
    assert_eq!(raw(&srv, "invite-issue garbage"), "err\n");
    let zero = "00".repeat(32);
    assert_eq!(raw(&srv, &format!("invite-revoke {zero} extra")), "err\n");
    assert_eq!(raw(&srv, &format!("device-block {zero} extra")), "err\n");
    assert_eq!(raw(&srv, &format!("device-unblock {zero} extra")), "err\n");
    // Без лишних — как раньше.
    assert_eq!(raw(&srv, "ping"), "pong\n");
    assert_eq!(raw(&srv, "domain"), "ctl.test\n");
    assert_eq!(raw(&srv, &format!("device-unblock {zero}")), "ok\n");
}

#[test]
fn line_too_long_err() {
    let srv = start(17415);
    // 256 байт — граница капа: неизвестная команда → обычный err.
    let at_cap = format!("q{}", "x".repeat(255));
    assert_eq!(at_cap.len(), 256);
    assert_eq!(raw(&srv, &at_cap), "err\n");
    // 257+ — err line-too-long вместо молчаливой резки.
    let over = format!("ping {}", "x".repeat(300));
    assert_eq!(raw(&srv, &over), "err line-too-long\n");
    // Сервер жив после overlong.
    assert_eq!(raw(&srv, "ping"), "pong\n");
}

#[test]
fn invite_issue_out_file_flow() {
    let srv = start(17416);
    // Без флага: URI в stdout + warning в stderr.
    let out = ctl_full(&srv, &["invite-issue"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("dmsg://join/"), "uri in stdout: {stdout:?}");
    assert!(!out.stderr.is_empty(), "warning expected");
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning"));
    // С флагом: URI в файл 0600, в stdout — только ok.
    let dest = srv.dir.join("invite.txt");
    let out = ctl_full(&srv, &["invite-issue", "--out-file", dest.to_str().unwrap()]);
    assert!(out.status.success(), "out-file failed: {out:?}");
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "ok\n");
    let body = std::fs::read_to_string(&dest).unwrap();
    assert!(body.starts_with("dmsg://join/"), "uri in file: {body:?}");
    assert!(dmsg_protocol::bootstrap::parse(body.trim()).is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Refuse-if-exists: второй выпуск в тот же путь — неуспех, файл цел.
    let out = ctl_full(&srv, &["invite-issue", "--out-file", dest.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(std::fs::read_to_string(&dest).unwrap().starts_with("dmsg://join/"));
    // --out-file без пути — usage/exit 2.
    assert!(!ctl_full(&srv, &["invite-issue", "--out-file"]).status.success());
}

#[test]
fn invite_revoke_via_file_roundtrip() {
    let srv = start(17417);
    let uri = ctl(&srv, &["invite-issue", "3600"]);
    let b = dmsg_protocol::bootstrap::parse(uri.trim()).expect("uri parses");
    let hex: String = b.token.iter().map(|x| format!("{x:02x}")).collect();
    let f = write_file(&srv.dir, "tok.hex", &format!("{hex}\n"));
    assert_eq!(ctl(&srv, &["invite-revoke", "--file", f.to_str().unwrap()]), "ok\n");
    let list = ctl(&srv, &["invite-list"]);
    assert!(list.contains("revoked=1"), "revoked flag: {list:?}");
    assert!(list.ends_with("ok\n"));
}

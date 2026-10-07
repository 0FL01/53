//! Local control contract: public profile, persisted policy, file-only invites.
mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn direct_control_and_qr_terminal_contract() {
    let srv = Server::new("qr-control");
    let executable = srv.dir.join("53ctl");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_msgd"), &executable).unwrap();
    let cli = |args: &[&str]| {
        std::process::Command::new(&executable)
            .env("MSGCTL_SOCK", srv.dir.join("ctl.sock"))
            .env("MSGD_DATA_DIR", srv.dir.join("data"))
            .args(args)
            .output()
            .unwrap()
    };
    assert_eq!(cli(&["ping"]).stdout, b"pong\n");
    let out = cli(&["qr-invite"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!srv.dir.join("data/invites").exists());
    assert_eq!(
        srv.db()
            .query_row("SELECT COUNT(*) FROM invites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    // util-linux script gives the child its own PTY; no real token is printed by
    // the test runner, and the terminal capture stays in memory.
    let pty = |args: &str| {
        std::process::Command::new("script")
            .args([
                "--quiet",
                "--return",
                "--command",
                &format!("exec '{}' qr-invite {args}", executable.display()),
                "/dev/null",
            ])
            .env("MSGCTL_SOCK", srv.dir.join("ctl.sock"))
            .env("MSGD_DATA_DIR", srv.dir.join("data"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let issued = pty("");
    assert!(issued.status.success());
    let dir = srv.dir.join("data/invites");
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let file = &files[0];
    let token = std::fs::read_to_string(file).unwrap();
    dmsg_protocol::auth::parse_invitation(token.trim()).unwrap();
    assert_eq!(
        std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!String::from_utf8_lossy(&issued.stdout).contains(token.trim()));
    let ttl: i64 = srv
        .db()
        .query_row("SELECT expires_at-created_at FROM invites", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(ttl, 86400);
    let shown = pty(&format!("--file '{}'", file.display()));
    assert!(shown.status.success());
    assert_eq!(shown.stdout, issued.stdout);
    assert_eq!(
        srv.db()
            .query_row("SELECT COUNT(*) FROM invites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(pty("--ttl 3600").status.success());
    assert_eq!(
        srv.db()
            .query_row(
                "SELECT COUNT(*) FROM invites WHERE expires_at-created_at=3600",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        pty(&format!("--file '{}'", file.display())).status.code(),
        Some(1)
    );
}

#[test]
fn empty_db_formats_and_public_profile() {
    let srv = Server::new("control-formats");
    assert_eq!(srv.ctl(&["user-list"]), "ok\n");
    assert_eq!(
        srv.ctl(&["quotas"]),
        "limits events=512 bytes=33554432\nok\n"
    );
    assert_eq!(srv.ctl(&["dbversion"]), "6\n");
    let profile = srv.ctl(&["server-code"]);
    let profile = dmsg_protocol::profile::parse(profile.trim()).unwrap();
    assert_eq!(profile.domain, DOMAIN.as_bytes());
    assert_eq!(profile.noise_pubkey, srv.server_pub);
    assert!(!profile.cert_der.is_empty());
}

#[test]
fn registration_policy_is_persisted_and_strict() {
    let mut srv = Server::new("control-policy");
    assert_eq!(srv.ctl(&["registration-mode"]), "invite_only\n");
    assert_eq!(srv.ctl(&["registration-mode", "open"]), "open\n");
    srv.restart();
    assert_eq!(srv.ctl(&["registration-mode"]), "open\n");
    for command in [
        "registration-mode OPEN",
        "registration-mode other",
        "registration-mode open extra",
    ] {
        assert_eq!(srv.raw(command), "err\n");
    }
    assert_eq!(
        srv.ctl(&["registration-mode", "invite_only"]),
        "invite_only\n"
    );
}

#[test]
fn bad_inputs_err_and_permissions_enforced() {
    let srv = Server::new("control-bad");
    for prefix in ["%", "way-too-long-filter"] {
        assert_eq!(srv.ctl(&["quotas", prefix]), "err\n");
    }
    for args in [
        vec!["device-unblock", "zz"],
        vec!["device-unblock"],
        vec!["device-unblock", &"00".repeat(32)],
    ] {
        assert_eq!(srv.cli(&args).status.code(), Some(2));
    }
    let bad = srv.file("bad.hex", b"zz\n");
    assert_eq!(
        srv.cli(&["device-unblock", "--file", bad.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );
    let missing = srv.dir.join("nope.hex");
    assert_eq!(
        srv.cli(&["device-unblock", "--file", missing.to_str().unwrap()])
            .status
            .code(),
        Some(1)
    );
    let public = srv.public_file("public.hex", &[0; 32]);
    std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        srv.cli(&["device-unblock", "--file", public.to_str().unwrap()])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn unblock_roundtrip_cannot_revive_retired_key() {
    let srv = Server::new("control-unblock");
    let path = srv.public_file("zero.hex", &[0; 32]);
    assert_eq!(
        srv.ctl(&["device-unblock", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    let db = srv.db();
    db.execute(
        "INSERT INTO users VALUES(?1,'0123456789AB','alice','$argon2id$test',1)",
        [[1u8; 16].as_slice()],
    )
    .unwrap();
    for (key, revoked) in [([0u8; 32], 1), ([2u8; 32], 0)] {
        db.execute(
            "INSERT INTO devices(device_key,user_id,created_at,revoked) VALUES(?1,?2,1,?3)",
            rusqlite::params![key.as_slice(), [1u8; 16].as_slice(), revoked],
        )
        .unwrap();
    }
    assert_eq!(
        srv.ctl(&["device-unblock", "--file", path.to_str().unwrap()]),
        "err\n"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM devices WHERE revoked=0", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let active = srv.public_file("active.hex", &[2; 32]);
    assert_eq!(
        srv.ctl(&["device-block", "--file", active.to_str().unwrap()]),
        "ok\n"
    );
    assert_eq!(
        srv.ctl(&["device-unblock", "--file", active.to_str().unwrap()]),
        "ok\n"
    );
    assert_eq!(
        db.query_row(
            "SELECT blocked FROM devices WHERE device_key=?1",
            [[2u8; 32].as_slice()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn strict_extra_args_err() {
    let srv = Server::new("control-strict");
    for command in [
        "ping x",
        "stats x",
        "user-list x",
        "invite-list x",
        "gc x",
        "backup x",
        "dbversion x",
        "domain x",
        "server-code x",
        "quotas A B",
        "invite-issue 10 20",
        "invite-issue garbage",
        "invite-issue 0",
        "invite-issue -1",
        "invite-issue 9223372036854775807",
    ] {
        assert_eq!(srv.raw(command), "err\n", "{command}");
    }
    let zero = "00".repeat(32);
    for op in ["invite-revoke", "device-block", "device-unblock"] {
        assert_eq!(srv.raw(&format!("{op} {zero} extra")), "err\n");
    }
    assert_eq!(srv.raw("invite-rebind"), "err\n");
    assert_eq!(srv.raw("ping"), "pong\n");
    assert_eq!(srv.raw("domain"), format!("{DOMAIN}\n"));
}

#[test]
fn line_too_long_err() {
    let srv = Server::new("control-line");
    assert_eq!(srv.raw(&format!("q{}", "x".repeat(255))), "err\n");
    assert_eq!(
        srv.raw(&format!("ping {}", "x".repeat(300))),
        "err line-too-long\n"
    );
    assert_eq!(srv.raw("ping"), "pong\n");
}

#[test]
fn invite_issue_out_file_flow_redacts_token() {
    let srv = Server::new("control-issue");
    assert_eq!(srv.cli(&["invite-issue"]).status.code(), Some(2));
    let (token, path) = srv.issue();
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body.trim().len(), 43);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let out = srv.cli(&["invite-issue", "--out-file", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    assert_eq!(
        srv.cli(&["invite-issue", "--out-file"]).status.code(),
        Some(2)
    );
    let list = srv.ctl(&["invite-list"]);
    assert!(!list.contains(body.trim()));
    let hex: String = token.iter().map(|b| format!("{b:02x}")).collect();
    assert!(!list.contains(&hex));
    assert!(list.contains("revoked=0 used=no"));
    assert!(!srv.ctl(&["server-code"]).contains(body.trim()));
    assert_eq!(
        srv.db()
            .query_row("SELECT COUNT(*) FROM invites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn invite_revoke_via_file_roundtrip() {
    let srv = Server::new("control-revoke");
    let (_, path) = srv.issue();
    assert_eq!(
        srv.ctl(&["invite-revoke", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    assert_eq!(
        srv.ctl(&["invite-revoke", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    let list = srv.ctl(&["invite-list"]);
    assert!(list.contains("revoked=1"));
    assert!(list.ends_with("ok\n"));
}

#[test]
fn issue_invalid_args_and_output_failure_have_no_side_effects() {
    let srv = Server::new("control-failures");
    let dest = srv.dir.join("invite.txt");
    let path = dest.to_str().unwrap();
    for args in [
        vec!["invite-issue"],
        vec!["invite-issue", "--out-file", path, "bad"],
        vec!["invite-issue", "--out-file", path, "1\nping"],
        vec!["invite-issue", "--out-file", path, "0"],
        vec!["invite-issue", "--out-file", path, "10", "extra"],
        vec!["invite-issue", "--out-file", path, "--out-file", path],
    ] {
        assert_eq!(srv.cli(&args).status.code(), Some(2));
        assert!(!dest.exists());
    }
    let missing = srv.dir.join("missing/invite.txt");
    assert_eq!(
        srv.cli(&["invite-issue", "--out-file", missing.to_str().unwrap()])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        srv.db()
            .query_row("SELECT COUNT(*) FROM invites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn post_commit_output_failure_retires_unusable_invite() {
    let srv = Server::new("control-write-failure");
    let dest = srv.dir.join("invite.txt");
    let out = std::process::Command::new("sh")
        .args(["-c", "trap '' XFSZ; ulimit -f 0; exec \"$@\"", "issue-cli"])
        .arg(env!("CARGO_BIN_EXE_msgd"))
        .args(["msgctl", "invite-issue", "--out-file"])
        .arg(&dest)
        .env("MSGCTL_SOCK", srv.dir.join("ctl.sock"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"invite-issue: output-failed\n");
    assert!(!dest.exists());
    assert_eq!(
        srv.db()
            .query_row("SELECT COUNT(*) FROM invites WHERE revoked=1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let (_, path) = srv.issue();
    assert!(path.exists());
}

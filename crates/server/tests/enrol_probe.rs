//! P3 evidence: полный цикл enrol через TCP+Noise против живого msgd.
//! Кейсы: issue→ENROLLED, replay, второй ключ→4, мусор→1, revoke→3+close,
//! device-block→3+close, kill/restart→durable replay, короткий TTL→2.

use dmsg_protocol::{
    bootstrap, decode_frame, encode_frame, OP_AUTH_DOMAIN, OP_ENROL,
};
use std::process::{Child, Command};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const DOMAIN: &str = "enrol.test";

struct Server {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
    server_pub: [u8; 32],
}

fn hex_to32(s: &str) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    b
}

fn start(port: u16) -> Server {
    let dir = std::env::temp_dir().join(format!("msgd-ep-{port}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_msgd");
    let key = dir.join("noise_key");
    let out = Command::new(bin).args(["keygen", "--out"]).arg(&key).output().unwrap();
    assert!(out.status.success());
    let server_pub = hex_to32(String::from_utf8(out.stdout).unwrap().trim());
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
    let mut child = Command::new(bin)
        .env("DMSG_DOMAIN", DOMAIN)
        .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
        .env("MSGD_DATA_DIR", dir.join("data"))
        .env("MSGD_BLOBS_DIR", dir.join("blobs"))
        .env("MSGCTL_SOCK", dir.join("ctl.sock"))
        .env("NOISE_KEY_FILE", &key)
        .env("CARRIER_CERT_FILE", &cert)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "msgd not ready");
        // readiness-probe съедает слот pre-auth и падает на hs-read: ок, кап 8.
        std::thread::sleep(Duration::from_millis(50));
    }
    // дать readiness-коннектам отвалиться по eof, чтобы не жрать кап
    std::thread::sleep(Duration::from_millis(300));
    let _ = &mut child;
    Server { child, port, dir, server_pub }
}

impl Server {
    fn msgctl(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_msgd"))
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .arg("msgctl")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "msgctl {args:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn kill_restart(&mut self) {
        self.child.kill().ok();
        let bin = env!("CARGO_BIN_EXE_msgd");
        self.child = Command::new(bin)
            .env("DMSG_DOMAIN", DOMAIN)
            .env("MSGD_LISTEN", format!("127.0.0.1:{}", self.port))
            .env("MSGD_DATA_DIR", self.dir.join("data"))
            .env("MSGD_BLOBS_DIR", self.dir.join("blobs"))
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", self.dir.join("noise_key"))
            .env("CARRIER_CERT_FILE", self.dir.join("carrier.pem"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(format!("127.0.0.1:{}", self.port)).is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "msgd not ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn wlen(s: &mut TcpStream, m: &[u8]) {
    s.write_all(&(m.len() as u16).to_be_bytes()).await.unwrap();
    s.write_all(m).await.unwrap();
}

async fn rlen(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).await.ok()?;
    let n = u16::from_be_bytes(h) as usize;
    if n == 0 || n > 65535 {
        return None;
    }
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).await.ok()?;
    Some(b)
}

/// Полный клиентский путь: handshake → AUTH_DOMAIN → WELCOME. Возвращает транспорт.
async fn connect_auth(srv: &Server) -> Option<(snow::TransportState, TcpStream)> {
    let params: snow::params::NoiseParams = PATTERN.parse().ok()?;
    let kp = snow::Builder::new(params).generate_keypair().ok()?;
    connect_auth_with(srv, &kp.private).await
}

async fn connect_auth_with(
    srv: &Server,
    initiator_priv: &[u8],
) -> Option<(snow::TransportState, TcpStream)> {
    let params: snow::params::NoiseParams = PATTERN.parse().ok()?;
    let mut hs = snow::Builder::new(params)
        .local_private_key(initiator_priv)
        .ok()?
        .remote_public_key(&srv.server_pub)
        .ok()?
        .build_initiator()
        .ok()?;
    let mut buf = vec![0u8; 65535];
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", srv.port)).await.ok()?;
    let n = hs.write_message(&[], &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await;
    let m2 = rlen(&mut s).await?;
    hs.read_message(&m2, &mut buf).ok()?;
    let mut t = hs.into_transport_mode().ok()?;
    let inner = encode_frame(OP_AUTH_DOMAIN, DOMAIN.as_bytes()).ok()?;
    let n = t.write_message(&inner, &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await;
    let c = rlen(&mut s).await?;
    let n = t.read_message(&c, &mut buf).ok()?;
    let (_, op, _, _) = decode_frame(&buf[..n]).ok()?;
    assert_eq!(op, 2); // WELCOME
    Some((t, s))
}

/// ENROL поверх готового транспорта. Возвращает сырой payload ответа (op, bytes).
async fn enrol(
    t: &mut snow::TransportState,
    s: &mut TcpStream,
    token: &[u8],
) -> Option<(u8, Vec<u8>)> {
    let mut buf = vec![0u8; 65535];
    let inner = encode_frame(OP_ENROL, token).ok()?;
    let n = t.write_message(&inner, &mut buf).ok()?;
    wlen(s, &buf[..n]).await;
    let c = rlen(s).await?;
    let n = t.read_message(&c, &mut buf).ok()?;
    let (_, op, p, _) = decode_frame(&buf[..n]).ok()?;
    Some((op, p.to_vec()))
}

fn issue_token(srv: &Server, ttl: &str) -> [u8; 32] {
    let uri = srv.msgctl(&["invite-issue", ttl]);
    let b = bootstrap::parse(uri.trim()).expect("uri parses");
    assert_eq!(b.domain, DOMAIN.as_bytes());
    b.token
}

/// Put the fixture deadline in the past without waiting for wall-clock TTL.
fn expire_token(srv: &Server, token: &[u8]) {
    let conn = rusqlite::Connection::open(srv.dir.join("data/msgd.db")).unwrap();
    assert_eq!(conn.execute(
        "UPDATE invites SET expires_at=0 WHERE token=?1", [token],
    ).unwrap(), 1);
}

/// Hex в файл 600 для --file команд (argv-hex удалён, R3/C1).
fn write_hex(srv: &Server, name: &str, hex: &str) -> String {
    let p = srv.dir.join(name);
    std::fs::write(&p, format!("{hex}\n")).unwrap();
    p.to_str().unwrap().to_string()
}

#[tokio::test]
async fn rebind_preserves_account_closes_old_and_replays_after_expiry() {
    use x25519_dalek::{PublicKey, StaticSecret};
    let mut srv = start(17207);
    let old_private = [71u8; 32];
    let new_private = [72u8; 32];
    let old_public = PublicKey::from(&StaticSecret::from(old_private));
    let old_hex: String = old_public.as_bytes().iter().map(|x| format!("{x:02x}")).collect();
    let key_file = write_hex(&srv, "old-public.hex", &old_hex);
    let old_token = issue_token(&srv, "3600");
    let (mut old_t, mut old_s) = connect_auth_with(&srv, &old_private).await.unwrap();
    let original = enrol(&mut old_t, &mut old_s, &old_token).await.unwrap();
    assert_eq!(original.0, 5);
    // Also close a pre-enrol stream for the old Noise key.
    let (_, mut pre_s) = connect_auth_with(&srv, &old_private).await.unwrap();
    let dest = srv.dir.join("rebind.txt");
    assert_eq!(srv.msgctl(&["invite-rebind", "--file", &key_file,
        "--out-file", dest.to_str().unwrap(), "3600"]), "ok\n");
    let body = std::fs::read_to_string(&dest).unwrap();
    let token = bootstrap::parse(body.trim()).unwrap().token;
    for stream in [&mut old_s, &mut pre_s] {
        let mut tmp = [0u8; 8];
        let r = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut tmp)).await;
        assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "old stream must close");
    }
    let (mut t, mut s) = connect_auth_with(&srv, &old_private).await.unwrap();
    assert_eq!(enrol(&mut t, &mut s, &old_token).await.unwrap(), (6, vec![3]));
    assert_eq!(enrol(&mut t, &mut s, &token).await.unwrap(), (6, vec![4]));
    let (mut t, mut s) = connect_auth_with(&srv, &new_private).await.unwrap();
    assert_eq!(enrol(&mut t, &mut s, &token).await.unwrap(), original);
    expire_token(&srv, &token);
    drop((t, s));
    srv.kill_restart();
    let (mut t, mut s) = connect_auth_with(&srv, &new_private).await.unwrap();
    assert_eq!(enrol(&mut t, &mut s, &token).await.unwrap(), original);
    assert_eq!(srv.msgctl(&["device-unblock", "--file", &key_file]), "err\n");
    let db = rusqlite::Connection::open(srv.dir.join("data/msgd.db")).unwrap();
    let counts: (i64, i64, i64) = db.query_row(
        "SELECT (SELECT COUNT(*) FROM users), (SELECT COUNT(*) FROM devices),
                (SELECT COUNT(*) FROM devices WHERE revoked=0)", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(counts, (1, 2, 1));
}

#[tokio::test]
async fn issue_enrol_replay() {
    let srv = start(17201);
    let token = issue_token(&srv, "3600");
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Builder::new(params).generate_keypair().unwrap();
    let priv32: [u8; 32] = kp.private[..32].try_into().unwrap();
    let (mut t, mut s) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (op, p) = enrol(&mut t, &mut s, &token).await.unwrap();
    assert_eq!(op, 5, "ENROLLED");
    assert_eq!(p.len(), 28);
    assert_eq!(p[16..].len(), 12);
    // Replay after the invite deadline: the same Noise static keeps its identity.
    expire_token(&srv, &token);
    let (mut t2, mut s2) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (op2, p2) = enrol(&mut t2, &mut s2, &token).await.unwrap();
    assert_eq!((op2, &p2), (5, &p));
    let (mut t3, mut s3) = connect_auth(&srv).await.unwrap();
    let (op3, p3) = enrol(&mut t3, &mut s3, &token).await.unwrap();
    assert_eq!((op3, p3.as_slice()), (6, &[2u8][..])); // Expired, different key
}

#[tokio::test]
async fn second_key_refused_bogus_bad() {
    let srv = start(17202);
    let token = issue_token(&srv, "3600");
    let (mut t, mut s) = connect_auth(&srv).await.unwrap();
    assert_eq!(enrol(&mut t, &mut s, &token).await.unwrap().0, 5);
    let (mut t2, mut s2) = connect_auth(&srv).await.unwrap();
    let (op, p) = enrol(&mut t2, &mut s2, &token).await.unwrap();
    assert_eq!((op, p.as_slice()), (6, &[4u8][..])); // BoundOther
    let (mut t3, mut s3) = connect_auth(&srv).await.unwrap();
    let (op, p) = enrol(&mut t3, &mut s3, &[9u8; 32]).await.unwrap();
    assert_eq!((op, p.as_slice()), (6, &[1u8][..])); // Bad
}

#[tokio::test]
async fn revoke_closes_and_rejects() {
    let srv = start(17203);
    let token = issue_token(&srv, "3600");
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Builder::new(params).generate_keypair().unwrap();
    let priv32: [u8; 32] = kp.private[..32].try_into().unwrap();
    let (mut t, mut s) = connect_auth_with(&srv, &priv32).await.unwrap();
    assert_eq!(enrol(&mut t, &mut s, &token).await.unwrap().0, 5);
    let hex: String = token.iter().map(|x| format!("{x:02x}")).collect();
    let f = write_hex(&srv, "tok.hex", &hex);
    expire_token(&srv, &token);
    assert_eq!(srv.msgctl(&["invite-revoke", "--file", &f]).trim(), "ok");
    // живая сессия закрыта
    let mut tmp = [0u8; 8];
    let r = tokio::time::timeout(Duration::from_secs(3), s.read(&mut tmp)).await;
    assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "session must close");
    // replay тем же ключом после revoke → 3
    let (mut t2, mut s2) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (op, p) = enrol(&mut t2, &mut s2, &token).await.unwrap();
    assert_eq!((op, p.as_slice()), (6, &[3u8][..])); // Revoked
}

#[tokio::test]
async fn block_closes_and_rejects_replay() {
    use x25519_dalek::{PublicKey, StaticSecret};
    let srv = start(17204);
    let token = issue_token(&srv, "3600");
    // Фиксированный static инициатора = известный device_key.
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Builder::new(params).generate_keypair().unwrap();
    let priv32: [u8; 32] = kp.private[..32].try_into().unwrap();
    let dev_pub = PublicKey::from(&StaticSecret::from(priv32));
    let dev_hex: String = dev_pub.as_bytes().iter().map(|x| format!("{x:02x}")).collect();
    let f = write_hex(&srv, "dev.hex", &dev_hex);
    let (mut t, mut s) = connect_auth_with(&srv, &priv32).await.unwrap();
    assert_eq!(enrol(&mut t, &mut s, &token).await.unwrap().0, 5);
    expire_token(&srv, &token);
    assert_eq!(srv.msgctl(&["device-block", "--file", &f]).trim(), "ok");
    // живая сессия закрыта
    let mut tmp = [0u8; 8];
    let r = tokio::time::timeout(Duration::from_secs(3), s.read(&mut tmp)).await;
    assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "session must close");
    // replay тем же ключом после block → 3 (revoked device)
    let (mut t2, mut s2) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (op, p) = enrol(&mut t2, &mut s2, &token).await.unwrap();
    assert_eq!((op, p.as_slice()), (6, &[3u8][..])); // Revoked
}

#[tokio::test]
async fn kill_restart_durable() {
    let mut srv = start(17205);
    let token = issue_token(&srv, "3600");
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Builder::new(params).generate_keypair().unwrap();
    let priv32: [u8; 32] = kp.private[..32].try_into().unwrap();
    let (mut t, mut s) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (_, p1) = enrol(&mut t, &mut s, &token).await.unwrap();
    drop((t, s));
    expire_token(&srv, &token);
    srv.kill_restart();
    let (mut t2, mut s2) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (op, p2) = enrol(&mut t2, &mut s2, &token).await.unwrap();
    assert_eq!((op, &p2), (5, &p1)); // durable replay после рестарта
}

#[tokio::test]
async fn short_ttl_expires() {
    let srv = start(17206);
    let token = issue_token(&srv, "2");
    expire_token(&srv, &token);
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Builder::new(params).generate_keypair().unwrap();
    let priv32: [u8; 32] = kp.private[..32].try_into().unwrap();
    let (mut t, mut s) = connect_auth_with(&srv, &priv32).await.unwrap();
    let (op, p) = enrol(&mut t, &mut s, &token).await.unwrap();
    assert_eq!((op, p.as_slice()), (6, &[2u8][..])); // Unbound invite: Expired
}

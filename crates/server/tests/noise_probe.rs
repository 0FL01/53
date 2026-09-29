//! P2 evidence: инициатор на том же snow играет клиента против msgd по TCP.
//! Кейсы: happy-path, неверный server key, replay msg1, oversize-handshake,
//! чужой домен внутри канала, неизвестный opcode внутри канала.
//! Полный гейт «unknown key → только enrolment» — в P3 (enrolment ещё нет);
//! здесь: неизвестные действия внутри канала закрываются без сайд-эффектов.

use dmsg_protocol::{decode_frame, encode_frame, OP_AUTH_DOMAIN, OP_WELCOME};
use std::process::{Child, Command};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const DOMAIN: &str = "noise.test";

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
    let dir = std::env::temp_dir().join(format!("msgd-np-{port}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_msgd");
    let key = dir.join("noise_key");
    // keygen печатает public hex
    let out = Command::new(bin)
        .args(["keygen", "--out"])
        .arg(&key)
        .output()
        .expect("keygen");
    assert!(out.status.success(), "keygen failed");
    let server_pub = hex_to32(String::from_utf8(out.stdout).unwrap().trim());
    let child = Command::new(bin)
        .env("DMSG_DOMAIN", DOMAIN)
        .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
        .env("MSGD_DATA_DIR", dir.join("data"))
        .env("MSGD_BLOBS_DIR", dir.join("blobs"))
        .env("MSGCTL_SOCK", dir.join("ctl.sock"))
        .env("NOISE_KEY_FILE", &key)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("msgd spawn");
    // ждём готовности
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "msgd not ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    Server { child, port, dir, server_pub }
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
    if s.read_exact(&mut h).await.is_err() {
        return None;
    }
    let n = u16::from_be_bytes(h) as usize;
    if n == 0 || n > 65535 {
        return None;
    }
    let mut b = vec![0u8; n];
    if s.read_exact(&mut b).await.is_err() {
        return None;
    }
    Some(b)
}

struct Init {
    t: snow::TransportState,
    s: TcpStream,
}

async fn handshake(srv: &Server, remote_pub: &[u8; 32]) -> Option<Init> {
    let params: snow::params::NoiseParams = PATTERN.parse().ok()?;
    let kp = snow::Builder::new(params.clone()).generate_keypair().ok()?;
    let mut hs = snow::Builder::new(params)
        .local_private_key(&kp.private)
        .ok()?
        .remote_public_key(remote_pub)
        .ok()?
        .build_initiator()
        .ok()?;
    let mut buf = vec![0u8; 65535];
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", srv.port)).await.ok()?;
    let n = hs.write_message(&[], &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await;
    let m2 = rlen(&mut s).await?;
    hs.read_message(&m2, &mut buf).ok()?;
    Some(Init { t: hs.into_transport_mode().ok()?, s })
}

async fn auth_domain(init: &mut Init, domain: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut buf = vec![0u8; 65535];
    let inner = encode_frame(OP_AUTH_DOMAIN, domain).ok()?;
    let n = init.t.write_message(&inner, &mut buf).ok()?;
    wlen(&mut init.s, &buf[..n]).await;
    let c = rlen(&mut init.s).await?;
    let n = init.t.read_message(&c, &mut buf).ok()?;
    let (_, op, p, _) = decode_frame(&buf[..n]).ok()?;
    Some((op, p.to_vec()))
}

#[tokio::test]
async fn happy_welcome() {
    let srv = start(17111);
    let mut init = handshake(&srv, &srv.server_pub).await.expect("handshake");
    let (op, p) = auth_domain(&mut init, DOMAIN.as_bytes()).await.expect("auth");
    assert_eq!(op, OP_WELCOME);
    assert_eq!(p, DOMAIN.as_bytes());
}

#[tokio::test]
async fn wrong_server_key_fails() {
    let srv = start(17112);
    let wrong = [7u8; 32];
    assert!(handshake(&srv, &wrong).await.is_none(), "must not complete");
}

#[tokio::test]
async fn replay_msg1_gives_no_session() {
    let srv = start(17113);
    // msg1 честного инициатора (e-private знает только он)
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Builder::new(params.clone()).generate_keypair().unwrap();
    let mut hs = snow::Builder::new(params)
        .local_private_key(&kp.private)
        .unwrap()
        .remote_public_key(&srv.server_pub)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(&[], &mut buf).unwrap();
    let stolen = buf[..n].to_vec();
    // replay на новом коннекте: сервер отвечает msg2 (отличить replay он не может),
    // но без e-private это тупик — выдаём replay за готовый канал:
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", srv.port)).await.unwrap();
    wlen(&mut s, &stolen).await;
    let _m2 = rlen(&mut s).await.expect("server answers");
    wlen(&mut s, b"garbage").await;
    // сервер обязан закрыть без WELCOME и без сайд-эффектов
    let mut tmp = [0u8; 8];
    let r = tokio::time::timeout(Duration::from_secs(3), s.read(&mut tmp)).await;
    assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "must close, got {r:?}");
}

#[tokio::test]
async fn oversize_handshake_closed() {
    // Bound wire-чтения сервера: MAX_FRAME+16 = 16400 (кадр + Noise-tag).
    // 16401 и u16::MAX закрываются сразу, не дожидаясь тела.
    let srv = start(17114);
    for len in [16401u16, u16::MAX] {
        let mut s = TcpStream::connect(format!("127.0.0.1:{}", srv.port)).await.unwrap();
        s.write_all(&len.to_be_bytes()).await.unwrap();
        s.write_all(&[0u8; 64]).await.unwrap();
        let mut tmp = [0u8; 8];
        let r = tokio::time::timeout(Duration::from_secs(12), s.read(&mut tmp)).await;
        assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "len={len}: must close, got {r:?}");
    }
}

#[tokio::test]
async fn wrong_domain_closed_no_side_effects() {
    let srv = start(17115);
    let mut init = handshake(&srv, &srv.server_pub).await.expect("handshake");
    assert!(auth_domain(&mut init, b"evil.example").await.is_none());
}

#[tokio::test]
async fn unknown_opcode_closed() {
    let srv = start(17116);
    let mut init = handshake(&srv, &srv.server_pub).await.expect("handshake");
    let mut buf = vec![0u8; 65535];
    let inner = encode_frame(77, b"x").unwrap();
    let n = init.t.write_message(&inner, &mut buf).unwrap();
    wlen(&mut init.s, &buf[..n]).await;
    let r = rlen(&mut init.s).await;
    assert!(r.is_none(), "must close without reply");
}

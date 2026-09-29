//! P4.4 chaos-матрица mailbox (loopback TCP; DNS-прогон отдельно).
//! Кейсы: flow+split ACK, replay-dedup+reorder, kill-restart-durable,
//! corrupt-passthrough, oversize-close, TTL+gc, tmpfs disk-full (graceful skip),
//! identity-change, unknown-version.

use dmsg_protocol::{
    decode_frame, encode_frame, OP_AUTH_DOMAIN, OP_DELIVERY_ACK, OP_ENROL, OP_ENROLLED, OP_ERROR,
    OP_FETCH, OP_FETCH_RESP, OP_SEND, OP_SEND_ACK, OP_UPLOAD_PREKEYS, OP_WELCOME,
};
use std::process::{Child, Command};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const DOMAIN: &str = "mbox.test";

struct Srv {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
    server_pub: [u8; 32],
}

fn start(port: u16, dir: std::path::PathBuf) -> Srv {
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_msgd");
    let key = dir.join("noise_key");
    if !key.exists() {
        let out = Command::new(bin).args(["keygen", "--out"]).arg(&key).output().unwrap();
        assert!(out.status.success());
    }
    let out = Command::new(bin).args(["pubkey", "--key"]).arg(&key).output().unwrap();
    let mut sp = [0u8; 32];
    let t = String::from_utf8(out.stdout).unwrap();
    for (i, c) in sp.iter_mut().enumerate() {
        *c = u8::from_str_radix(&t[2 * i..2 * i + 2], 16).unwrap();
    }
    let cert = dir.join("c.pem");
    if !cert.exists() {
        Command::new("openssl")
            .arg("req")
            .args(["-x509", "-newkey", "rsa:2048", "-keyout"])
            .arg(dir.join("k.pem"))
            .args(["-out"])
            .arg(&cert)
            .args(["-days", "1", "-nodes", "-subj", "/CN=x"])
            .output()
            .unwrap();
    }
    let child = Command::new(bin)
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
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    Srv { child, port, dir, server_pub: sp }
}

impl Srv {
    fn msgctl(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_msgd"))
            .arg("msgctl")
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }
    fn issue(&self) -> [u8; 32] {
        let uri = self.msgctl(&["invite-issue"]);
        let b64 = uri.trim().rsplit("/join/").next().unwrap().to_string();
        let raw = base64_url_decode(&b64);
        raw[raw.len() - 32..].try_into().unwrap()
    }
}

impl Drop for Srv {
    fn drop(&mut self) {
        self.child.kill().ok();
    }
}

fn base64_url_decode(s: &str) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut n = 0;
    for &c in s.as_bytes() {
        let v = val(c).unwrap() as u32;
        acc = (acc << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
        }
    }
    out
}

/// Пара static-ключей устройства: private случайный, public деривирован (для reconnect).
fn device_pair() -> ([u8; 32], [u8; 32]) {
    let mut privk = [0u8; 32];
    getrandom::fill(&mut privk).unwrap();
    let secret = x25519_dalek::StaticSecret::from(privk);
    (privk, *x25519_dalek::PublicKey::from(&secret).as_bytes())
}

struct Cli {
    t: snow::TransportState,
    s: TcpStream,
    device: [u8; 32],
}

async fn connect(srv: &Srv, pair: Option<([u8; 32], [u8; 32])>) -> (Cli, ([u8; 32], [u8; 32])) {
    let (privk, pubk) = pair.unwrap_or_else(device_pair);
    let params: snow::params::NoiseParams = PATTERN.parse().unwrap();
    let kp = snow::Keypair { private: privk.to_vec(), public: pubk.to_vec() };
    let mut hs = snow::Builder::new(params)
        .local_private_key(&kp.private)
        .unwrap()
        .remote_public_key(&srv.server_pub)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut buf = vec![0u8; 65535];
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", srv.port)).await.unwrap();
    let n = hs.write_message(&[], &mut buf).unwrap();
    wlen(&mut s, &buf[..n]).await;
    let m2 = rlen(&mut s).await.unwrap();
    hs.read_message(&m2, &mut buf).unwrap();
    (Cli { t: hs.into_transport_mode().unwrap(), s, device: pubk }, (privk, pubk))
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

impl Cli {
    fn device_static(&self) -> [u8; 32] {
        self.device
    }
    async fn xchg(&mut self, op: u8, payload: &[u8]) -> Option<(u8, Vec<u8>)> {
        let mut buf = vec![0u8; 65535];
        let inner = encode_frame(op, payload).ok()?;
        let n = self.t.write_message(&inner, &mut buf).ok()?;
        wlen(&mut self.s, &buf[..n]).await;
        let c = rlen(&mut self.s).await?;
        let n = self.t.read_message(&c, &mut buf).ok()?;
        let (_, o, p, _) = decode_frame(&buf[..n]).ok()?;
        Some((o, p.to_vec()))
    }
    async fn auth(&mut self) {
        let (op, _) = self.xchg(OP_AUTH_DOMAIN, DOMAIN.as_bytes()).await.unwrap();
        assert_eq!(op, OP_WELCOME);
    }
    async fn enrol(&mut self, token: &[u8; 32]) -> [u8; 16] {
        let (op, p) = self.xchg(OP_ENROL, token).await.unwrap();
        assert_eq!(op, OP_ENROLLED, "enrol failed op={op}");
        p[..16].try_into().unwrap()
    }
    /// Разобрать FETCH_RESP в (seq, ciphertext).
    async fn fetch_all(&mut self) -> Vec<(u64, Vec<u8>)> {
        let (op, fp) = self.xchg(OP_FETCH, &[]).await.unwrap();
        assert_eq!(op, OP_FETCH_RESP);
        let n = u16::from_be_bytes([fp[0], fp[1]]) as usize;
        let mut off = 2;
        let mut out = vec![];
        for _ in 0..n {
            let s = u64::from_be_bytes(fp[off..off + 8].try_into().unwrap());
            let ctlen = u16::from_be_bytes([fp[off + 56], fp[off + 57]]) as usize;
            out.push((s, fp[off + 58..off + 58 + ctlen].to_vec()));
            off += 58 + ctlen;
        }
        out
    }
    async fn ack(&mut self, seqs: &[u64]) -> u64 {
        let mut p = vec![(seqs.len() >> 8) as u8, seqs.len() as u8];
        for s in seqs {
            p.extend_from_slice(&s.to_be_bytes());
        }
        let (op, cp) = self.xchg(OP_DELIVERY_ACK, &p).await.unwrap();
        assert_eq!(op, OP_DELIVERY_ACK);
        u64::from_be_bytes(cp[..8].try_into().unwrap())
    }
}

fn tmpdir(name: &str, port: u16) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("msgd-mb-{name}-{port}-{}", std::process::id()))
}

fn send_frame(user: &[u8; 16], msgid: &[u8; 16], ct: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(user);
    p.extend_from_slice(msgid);
    p.extend_from_slice(ct);
    p
}

#[tokio::test]
async fn flow_split_ack() {
    let srv = start(17211, tmpdir("flow", 17211));
    let tok_b = srv.issue();
    let (mut b, _) = connect(&srv, None).await;
    b.auth().await;
    let user_b = b.enrol(&tok_b).await;
    let tok_a = srv.issue();
    let (mut a, _) = connect(&srv, None).await;
    a.auth().await;
    a.enrol(&tok_a).await;
    let msgid = [1u8; 16];
    let (op, rp) = a.xchg(OP_SEND, &send_frame(&user_b, &msgid, b"hello-b")).await.unwrap();
    assert_eq!(op, OP_SEND_ACK);
    assert_eq!((&rp[..16], rp[16]), (&msgid[..], 1));
    // ACCEPTED ≠ DELIVERED: событие уже видно, но cursor не двинут.
    assert_eq!(b.fetch_all().await.len(), 1);
    assert_eq!(b.fetch_all().await.len(), 1);
    let ev = b.fetch_all().await;
    assert_eq!(b.ack(&[ev[0].0]).await, ev[0].0);
    assert!(b.fetch_all().await.is_empty());
    // Повтор после доставки: тот же message_id → ST_DELIVERED, без нового события.
    let (op, rp) = a.xchg(OP_SEND, &send_frame(&user_b, &msgid, b"hello-b")).await.unwrap();
    assert_eq!(op, OP_SEND_ACK);
    assert_eq!((&rp[..16], rp[16]), (&msgid[..], 2));
    assert!(b.fetch_all().await.is_empty());
}

#[tokio::test]
async fn replay_dedup_and_reorder() {
    let srv = start(17212, tmpdir("replay", 17212));
    let tok_b = srv.issue();
    let (mut b, _) = connect(&srv, None).await;
    b.auth().await;
    let user_b = b.enrol(&tok_b).await;
    let tok_a = srv.issue();
    let (mut a, _) = connect(&srv, None).await;
    a.auth().await;
    a.enrol(&tok_a).await;
    for i in 0..3u8 {
        let (op, _) = a.xchg(OP_SEND, &send_frame(&user_b, &[i; 16], b"d")).await.unwrap();
        assert_eq!(op, OP_SEND_ACK);
    }
    // Повтор msgid=1 другим текстом — прежний accept, дубля нет.
    let (op, _) = a.xchg(OP_SEND, &send_frame(&user_b, &[1u8; 16], b"other")).await.unwrap();
    assert_eq!(op, OP_SEND_ACK);
    let ev = b.fetch_all().await;
    assert_eq!(ev.len(), 3);
    assert_eq!(ev[1].1, b"d"); // оригинал, не "other"
    // ACK среднего: cursor стоит (гэп).
    assert_eq!(b.ack(&[ev[1].0]).await, 0);
    // ACK первого: cursor прыгает по непрерывному (1,2).
    assert_eq!(b.ack(&[ev[0].0]).await, ev[1].0);
    // Остался только третий.
    let rest = b.fetch_all().await;
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].0, ev[2].0);
}

#[tokio::test]
async fn interleaved_recipient_ack_crosses_global_and_ttl_gaps_only() {
    let srv = start(17221, tmpdir("interleaved", 17221));
    let tok_a = srv.issue();
    let tok_b = srv.issue();
    let (mut a, _) = connect(&srv, None).await;
    let (mut b, _) = connect(&srv, None).await;
    a.auth().await;
    b.auth().await;
    let user_a = a.enrol(&tok_a).await;
    let user_b = b.enrol(&tok_b).await;

    // GC removes a global sequence before either recipient's first live event.
    let db = rusqlite::Connection::open(srv.dir.join("data/msgd.db")).unwrap();
    db.execute(
        "INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at)
         VALUES(?1,?2,?3,?4,0)",
        rusqlite::params![
            user_b.as_slice(),
            a.device_static().as_slice(),
            [99u8; 16],
            b"expired"
        ],
    )
    .unwrap();
    assert_eq!(srv.msgctl(&["gc"]).trim(), "gc blobs=0 events=1");

    for i in 0..3u8 {
        assert_eq!(
            a.xchg(OP_SEND, &send_frame(&user_b, &[i; 16], b"to-b"))
                .await
                .unwrap()
                .0,
            OP_SEND_ACK
        );
        if i < 2 {
            assert_eq!(
                b.xchg(OP_SEND, &send_frame(&user_a, &[i; 16], b"to-a"))
                    .await
                    .unwrap()
                    .0,
                OP_SEND_ACK
            );
        }
    }
    let be = b.fetch_all().await;
    let ae = a.fetch_all().await;
    assert_eq!((be.len(), ae.len()), (3, 2));
    assert!(be[0].0 > 1 && be[1].0 > be[0].0 + 1);
    // Mark the last event first: the earliest undelivered event still blocks.
    assert_eq!(b.ack(&[be[2].0]).await, 0);
    assert_eq!(b.fetch_all().await.len(), 3);
    assert_eq!(b.ack(&[be[0].0]).await, be[0].0);
    assert_eq!(b.fetch_all().await, be[1..]);
    // Closing the recipient-local gap also crosses its already delivered tail.
    assert_eq!(b.ack(&[be[1].0]).await, be[2].0);
    assert!(b.fetch_all().await.is_empty());
    assert_eq!(b.ack(&[be[2].0]).await, be[2].0, "ACK replay is idempotent");
    assert_eq!(a.ack(&[ae[1].0]).await, 0);
    assert_eq!(a.ack(&[ae[0].0]).await, ae[1].0);
    assert!(a.fetch_all().await.is_empty());
}

#[tokio::test]
async fn kill_restart_durable() {
    let dir = tmpdir("restart", 17213);
    let (pair_b, tok_b, msgid) = {
        let srv = start(17213, dir.clone());
        let tok_b = srv.issue();
        let (mut b, pair_b) = connect(&srv, None).await;
        b.auth().await;
        let user_b = b.enrol(&tok_b).await;
        let tok_a = srv.issue();
        let (mut a, _) = connect(&srv, None).await;
        a.auth().await;
        a.enrol(&tok_a).await;
        let msgid = [9u8; 16];
        let (op, _) = a.xchg(OP_SEND, &send_frame(&user_b, &msgid, b"durable")).await.unwrap();
        assert_eq!(op, OP_SEND_ACK);
        (pair_b, tok_b, msgid) // srv kill здесь (Drop): kill до FETCH
    };
    // Рестарт: то же устройство B (тот же static) replay'ит СВОЙ token → ENROLLED,
    // забирает событие с cursor. Так работает и настоящий reconnect.
    let srv = start(17214, dir);
    let (mut b, _) = connect(&srv, Some(pair_b)).await;
    b.auth().await;
    b.enrol(&tok_b).await; // replay: прежний ответ, не BoundOther
    let ev = b.fetch_all().await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].1, b"durable");
    assert_eq!(b.ack(&[ev[0].0]).await, ev[0].0);
    assert!(b.fetch_all().await.is_empty());
    let _ = msgid;
}

#[tokio::test]
async fn corrupt_passthrough_and_unknown_version() {
    let srv = start(17215, tmpdir("corrupt", 17215));
    let tok_b = srv.issue();
    let (mut b, _) = connect(&srv, None).await;
    b.auth().await;
    let user_b = b.enrol(&tok_b).await;
    let tok_a = srv.issue();
    let (mut a, _) = connect(&srv, None).await;
    a.auth().await;
    a.enrol(&tok_a).await;
    // Сервер opaque: мусорный ciphertext принимается как есть.
    let (op, _) = a
        .xchg(OP_SEND, &send_frame(&user_b, &[2u8; 16], &[0xFFu8; 64]))
        .await
        .unwrap();
    assert_eq!(op, OP_SEND_ACK);
    let ev = b.fetch_all().await;
    assert_eq!(ev[0].1, vec![0xFFu8; 64]);
    // Неизвестная версия wire → close без ответа.
    let mut raw = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", srv.port)).await.unwrap();
    wlen(&mut raw, &[9u8, OP_SEND, 0, 1, 0xFF]).await;
    let mut tmp = [0u8; 8];
    let r = tokio::time::timeout(Duration::from_secs(12), raw.read(&mut tmp)).await;
    assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "must close, got {r:?}");
}

#[tokio::test]
async fn oversize_close() {
    let srv = start(17216, tmpdir("oversize", 17216));
    // 1) Transport-bound: length-prefix 16401 (> MAX_FRAME+16=16400) → close без ответа.
    {
        let tok = srv.issue();
        let (mut c, _) = connect(&srv, None).await;
        c.auth().await;
        c.enrol(&tok).await;
        {
            use tokio::io::AsyncWriteExt;
            c.s.write_all(&16401u16.to_be_bytes()).await.unwrap();
            c.s.write_all(&[0xBBu8; 64]).await.unwrap();
        }
        let mut tmp = [0u8; 8];
        let r = tokio::time::timeout(Duration::from_secs(12), c.s.read(&mut tmp)).await;
        assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "must close, got {r:?}");
    }
    // 2) Кадр больше MAX_FRAME целиком → close. Строим вручную (encode_frame отказал бы).
    //    Plaintext-кадр 16388 → шифртекст 16404 > bound 16400: close уже на transport-read.
    let tok = srv.issue();
    let (mut c, _) = connect(&srv, None).await;
    c.auth().await;
    c.enrol(&tok).await;
    // Кадр больше MAX_FRAME целиком → close. Строим вручную (encode_frame отказал бы).
    let big = vec![0xAAu8; dmsg_protocol::MAX_FRAME];
    let mut raw_frame = vec![1u8, OP_SEND];
    raw_frame.extend_from_slice(&(big.len() as u16).to_be_bytes());
    raw_frame.extend_from_slice(&big);
    // u16 не вмещает >65535, но MAX_FRAME=16384 — шлём ровно лимит+1 через Noise.
    // (bound transport-чтения 16400 тоже превышен: см. часть 1 выше).
    let mut buf = vec![0u8; 70000];
    let n = c.t.write_message(&raw_frame, &mut buf).unwrap();
    {
        use tokio::io::AsyncWriteExt;
        c.s.write_all(&(n as u16).to_be_bytes()).await.unwrap();
        c.s.write_all(&buf[..n]).await.unwrap();
    }
    let mut tmp = [0u8; 8];
    let r = tokio::time::timeout(Duration::from_secs(12), c.s.read(&mut tmp)).await;
    assert!(matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))), "must close, got {r:?}");
}

#[tokio::test]
async fn ttl_gc_cleans() {
    use rusqlite::Connection;
    let dir = tmpdir("ttl", 17217);
    let srv = start(17217, dir.clone());
    let tok_b = srv.issue();
    let (mut b, _) = connect(&srv, None).await;
    b.auth().await;
    let user_b = b.enrol(&tok_b).await;
    // Crafted-expired событие напрямую в SQL (без sleep-флаков).
    let db = Connection::open(dir.join("data").join("msgd.db")).unwrap();
    db.execute(
        "INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at)
         VALUES(?1,?2,?3,?4,0)",
        rusqlite::params![&user_b[..], [8u8; 32], [8u8; 16], vec![0u8; 4]],
    )
    .unwrap();
    drop(db);
    assert_eq!(srv.msgctl(&["gc"]).trim(), "gc blobs=0 events=1");
    assert!(b.fetch_all().await.is_empty());
}

#[tokio::test]
async fn disk_full_tmpfs() {
    // tmpfs 1M под data: SQLITE_FULL при SEND → ERROR, не паника/зависание.
    let base = tmpdir("diskfull", 17218);
    let mnt = base.join("mnt");
    std::fs::create_dir_all(&mnt).unwrap();
    let mounted = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=1M", "tmpfs"])
        .arg(&mnt)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !mounted {
        eprintln!("SKIP disk_full: mount tmpfs not permitted");
        return;
    }
    async {
        let srv = start(17219, mnt.clone());
        let tok_b = srv.issue();
        let (mut b, _) = connect(&srv, None).await;
        b.auth().await;
        let user_b = b.enrol(&tok_b).await;
        let tok_a = srv.issue();
        let (mut a, _) = connect(&srv, None).await;
        a.auth().await;
        a.enrol(&tok_a).await;
        // Забиваем крошечный диск событиями, пока не получим ERROR/quota или close.
        let mut got_err = false;
        for i in 0..200u8 {
            let ct = vec![i; 4096];
            match a.xchg(OP_SEND, &send_frame(&user_b, &[i; 16], &ct)).await {
                Some((OP_SEND_ACK, _)) => continue,
                Some((OP_ERROR, _)) => {
                    got_err = true;
                    break;
                }
                _ => {
                    got_err = true;
                    break;
                }
            }
        }
        assert!(got_err, "disk must fill on 1M tmpfs");
    }
    .await;
    std::process::Command::new("umount").arg(&mnt).output().ok();
}

#[tokio::test]
async fn identity_change_refused() {
    use ed25519_dalek::Signer;
    let srv = start(17220, tmpdir("identity", 17220));
    let tok = srv.issue();
    let (mut c, _) = connect(&srv, None).await;
    c.auth().await;
    c.enrol(&tok).await;
    let dev = c.device_static();
    let id1 = ed25519_dalek::SigningKey::from_bytes(&[51u8; 32]);
    let id2 = ed25519_dalek::SigningKey::from_bytes(&[52u8; 32]);
    let mk = |idkey: &ed25519_dalek::SigningKey, id: u32| {
        let ident = idkey.verifying_key().to_bytes();
        let entry_pub = [0x77u8; 32];
        let mut msg = Vec::new();
        msg.extend_from_slice(&dev);
        msg.extend_from_slice(&id.to_be_bytes());
        msg.extend_from_slice(&entry_pub);
        let sig = idkey.sign(&msg);
        let mut p = Vec::new();
        p.extend_from_slice(&ident);
        p.extend_from_slice(&[0u8, 1]);
        p.extend_from_slice(&id.to_be_bytes());
        p.push(1);
        p.extend_from_slice(&entry_pub);
        p.extend_from_slice(&sig.to_bytes());
        p
    };
    // Первая загрузка фиксирует identity.
    let (op, _) = c.xchg(OP_UPLOAD_PREKEYS, &mk(&id1, 1)).await.unwrap();
    assert_eq!(op, 25); // COUNT_RESP
    // Другая identity → ERROR BAD.
    let (op, _) = c.xchg(OP_UPLOAD_PREKEYS, &mk(&id2, 2)).await.unwrap();
    assert_eq!(op, OP_ERROR);
}

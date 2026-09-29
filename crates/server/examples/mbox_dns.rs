//! Diag-прогон mailbox через DNS-путь: A enrol → B enrol → A SEND → B FETCH →
//! B DELIVERY_ACK → A видит delivered. Точка входа — TCP-порт локального
//! slipstream-client (как noise_diag/enrol_diag).
//! Env: DIAG_PORT, DIAG_DOMAIN, DIAG_SERVER_PUB (hex), DIAG_TOKEN_A/B (hex 64).
//! Печатает `RESULT PASS` или `RESULT FAIL`.

use dmsg_protocol::{
    decode_frame, encode_frame, OP_AUTH_DOMAIN, OP_DELIVERY_ACK, OP_ENROL, OP_ENROLLED,
    OP_FETCH, OP_FETCH_RESP, OP_SEND, OP_SEND_ACK, OP_WELCOME, ST_ACCEPTED, ST_DELIVERED,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn read_token_file(path: &str) -> Option<[u8; 32]> {
    let s = std::fs::read_to_string(path).ok()?;
    hex_to32(s.trim())
}

fn hex_to32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(b)
}

struct Sess {
    t: snow::TransportState,
    s: tokio::net::TcpStream,
}

async fn wlen(s: &mut tokio::net::TcpStream, m: &[u8]) -> Option<()> {
    s.write_all(&(m.len() as u16).to_be_bytes()).await.ok()?;
    s.write_all(m).await.ok()?;
    Some(())
}

async fn rlen(s: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
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

async fn xchg(sess: &mut Sess, op: u8, payload: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut buf = vec![0u8; 65535];
    let inner = encode_frame(op, payload).ok()?;
    let n = sess.t.write_message(&inner, &mut buf).ok()?;
    wlen(&mut sess.s, &buf[..n]).await?;
    let c = rlen(&mut sess.s).await?;
    let n = sess.t.read_message(&c, &mut buf).ok()?;
    let (_, rop, p, _) = decode_frame(&buf[..n]).ok()?;
    Some((rop, p.to_vec()))
}

async fn session(port: u16, domain: &str, server_pub: &[u8; 32], token: &[u8; 32]) -> Option<(Sess, [u8; 16])> {
    let params: snow::params::NoiseParams = "Noise_IK_25519_ChaChaPoly_BLAKE2s".parse().ok()?;
    let kp = snow::Builder::new(params.clone()).generate_keypair().ok()?;
    let mut hs = snow::Builder::new(params)
        .local_private_key(&kp.private)
        .ok()?
        .remote_public_key(server_pub)
        .ok()?
        .build_initiator()
        .ok()?;
    let mut buf = vec![0u8; 65535];
    let mut s = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await.ok()?;
    let n = hs.write_message(&[], &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await?;
    let m2 = rlen(&mut s).await?;
    hs.read_message(&m2, &mut buf).ok()?;
    let t = hs.into_transport_mode().ok()?;
    let mut sess = Sess { t, s };
    // AUTH_DOMAIN
    let (op, _) = xchg(&mut sess, OP_AUTH_DOMAIN, domain.as_bytes()).await?;
    if op != OP_WELCOME {
        return None;
    }
    // ENROL
    let (op, p) = xchg(&mut sess, OP_ENROL, token).await?;
    if op != OP_ENROLLED || p.len() != 28 {
        return None;
    }
    let mut uid = [0u8; 16];
    uid.copy_from_slice(&p[..16]);
    Some((sess, uid))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let port: u16 = std::env::var("DIAG_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let domain = std::env::var("DIAG_DOMAIN").unwrap_or_default();
    let pubhex = std::env::var("DIAG_SERVER_PUB").unwrap_or_default();
    // Токены — только из файлов 600 (C2: не в env/логи/argv).
    let ta = read_token_file(&std::env::var("DIAG_TOKEN_A_FILE").unwrap_or_default());
    let tb = read_token_file(&std::env::var("DIAG_TOKEN_B_FILE").unwrap_or_default());
    let (pubk, toka, tokb) = match (hex_to32(&pubhex), ta, tb) {
        (Some(p), Some(a), Some(b)) if port != 0 && !domain.is_empty() => (p, a, b),
        _ => {
            eprintln!("usage: DIAG_PORT=p DIAG_DOMAIN=d DIAG_SERVER_PUB=hex DIAG_TOKEN_A/B_FILE=path mbox_dns");
            return std::process::ExitCode::from(2);
        }
    };
    match run(port, &domain, &pubk, &toka, &tokb).await {
        true => {
            println!("RESULT PASS");
            std::process::ExitCode::SUCCESS
        }
        false => {
            println!("RESULT FAIL");
            std::process::ExitCode::from(1)
        }
    }
}

async fn run(port: u16, domain: &str, pubk: &[u8; 32], toka: &[u8; 32], tokb: &[u8; 32]) -> bool {
    let (mut a, _auid) = match session(port, domain, pubk, toka).await {
        Some(v) => v,
        None => return false,
    };
    let (mut b, buid) = match session(port, domain, pubk, tokb).await {
        Some(v) => v,
        None => return false,
    };
    // A SEND → B
    let mut msg_id = [0u8; 16];
    msg_id.copy_from_slice(&buid[..16]); // детерминированный, но уникальный на прогон
    msg_id[0] ^= 0xA5;
    let mut payload = Vec::with_capacity(48);
    payload.extend_from_slice(&buid);
    payload.extend_from_slice(&msg_id);
    payload.extend_from_slice(b"hello-dns-mbox");
    let (op, p) = match xchg(&mut a, OP_SEND, &payload).await {
        Some(v) => v,
        None => return false,
    };
    if op != OP_SEND_ACK || p.len() != 17 || p[16] != ST_ACCEPTED {
        return false;
    }
    // B FETCH (пустой запрос) → 1 событие
    let (op, p) = match xchg(&mut b, OP_FETCH, &[]).await {
        Some(v) => v,
        None => return false,
    };
    if op != OP_FETCH_RESP || p.len() < 2 {
        return false;
    }
    let count = u16::from_be_bytes([p[0], p[1]]) as usize;
    if count != 1 {
        return false;
    }
    // запись: seq8 + sender32 + msgid16 + ctlen2 + ct
    if p.len() < 2 + 8 + 32 + 16 + 2 {
        return false;
    }
    let seq = u64::from_be_bytes(p[2..10].try_into().unwrap());
    let ctlen = u16::from_be_bytes(p[58..60].try_into().unwrap()) as usize;
    if p.len() != 60 + ctlen {
        return false;
    }
    if &p[60..] != b"hello-dns-mbox" {
        return false;
    }
    // B DELIVERY_ACK(count u16 + seq) → ответ OP_DELIVERY_ACK
    let mut ackp = vec![0u8, 1];
    ackp.extend_from_slice(&seq.to_be_bytes());
    let (op, _) = match xchg(&mut b, OP_DELIVERY_ACK, &ackp).await {
        Some(v) => v,
        None => return false,
    };
    if op != OP_DELIVERY_ACK {
        return false;
    }
    // A опрашивает статус: повторный SEND тот же msg_id → ST_DELIVERED
    let (op, p) = match xchg(&mut a, OP_SEND, &payload).await {
        Some(v) => v,
        None => return false,
    };
    op == OP_SEND_ACK && p.len() == 17 && p[16] == ST_DELIVERED
}

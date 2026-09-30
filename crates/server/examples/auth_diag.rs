//! Unified signup/login/resume through an explicitly supplied local transport port.
//! Точка входа — TCP-порт локального slipstream-client (как noise_diag).
//! Env: DIAG_PORT, DIAG_DOMAIN, DIAG_SERVER_PUB (public hex), DIAG_OPERATION
//! (signup/login/resume), DIAG_PAYLOAD_FILE (binary canonical auth payload, 0600),
//! DIAG_DEVICE_KEY_FILE (persistent private Noise hex key, 0600). Secrets in files.
//! Печатает `RESULT PASS user=<hex> contact=<id>` или `RESULT FAIL`.

use dmsg_protocol::{auth, *};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn hex_to32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(b)
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let port: u16 = std::env::var("DIAG_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let domain = std::env::var("DIAG_DOMAIN").unwrap_or_default();
    let pubhex = std::env::var("DIAG_SERVER_PUB").unwrap_or_default();
    let op = match std::env::var("DIAG_OPERATION").as_deref() {
        Ok("signup") => Some(OP_SIGNUP),
        Ok("login") => Some(OP_LOGIN),
        Ok("resume") => Some(OP_RESUME),
        _ => None,
    };
    let key = std::env::var("DIAG_DEVICE_KEY_FILE")
        .ok()
        .and_then(|p| read_secret(&p, 65))
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| hex_to32(v.trim()));
    let payload = if op == Some(OP_RESUME) {
        Some(vec![])
    } else {
        std::env::var("DIAG_PAYLOAD_FILE")
            .ok()
            .and_then(|p| read_secret(&p, auth::AUTH_PAYLOAD_MAX))
    };
    let (pubk, key, op, mut payload) = match (hex_to32(&pubhex), key, op, payload) {
        (Some(p), Some(k), Some(o), Some(v)) if port != 0 && !domain.is_empty() => (p, k, o, v),
        _ => {
            eprintln!("usage: auth_diag with DIAG_PORT, DIAG_DOMAIN, DIAG_SERVER_PUB, DIAG_OPERATION, DIAG_DEVICE_KEY_FILE and DIAG_PAYLOAD_FILE (signup/login)");
            return std::process::ExitCode::from(2);
        }
    };
    let result = run(port, &domain, &pubk, &key, op, &payload).await;
    payload.fill(0);
    match result {
        Some((u, c)) => {
            println!(
                "RESULT PASS user={} contact={}",
                hex_of(&u),
                String::from_utf8_lossy(&c)
            );
            std::process::ExitCode::SUCCESS
        }
        None => {
            println!("RESULT FAIL");
            std::process::ExitCode::from(1)
        }
    }
}

fn hex_of(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

fn read_secret(path: &str, max: usize) -> Option<Vec<u8>> {
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 || meta.len() > max as u64 {
        return None;
    }
    let mut v = vec![];
    file.take(max as u64 + 1).read_to_end(&mut v).ok()?;
    (v.len() <= max).then_some(v)
}

pub(crate) async fn run(
    port: u16,
    domain: &str,
    server_pub: &[u8; 32],
    private: &[u8; 32],
    op: u8,
    payload: &[u8],
) -> Option<(Vec<u8>, Vec<u8>)> {
    if (op == OP_SIGNUP && auth::parse_signup(payload).is_err())
        || (op == OP_LOGIN && auth::parse_login(payload).is_err())
    {
        return None;
    }
    let params: snow::params::NoiseParams = "Noise_IK_25519_ChaChaPoly_BLAKE2s".parse().ok()?;
    let mut hs = snow::Builder::new(params)
        .local_private_key(private)
        .ok()?
        .remote_public_key(server_pub)
        .ok()?
        .build_initiator()
        .ok()?;
    let mut buf = vec![0u8; 65535];
    let mut s = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .ok()?;
    let n = hs.write_message(&[], &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await?;
    let m2 = rlen(&mut s).await?;
    hs.read_message(&m2, &mut buf).ok()?;
    let mut t = hs.into_transport_mode().ok()?;
    // AUTH_DOMAIN → WELCOME
    let inner = encode_frame(OP_AUTH_DOMAIN, domain.as_bytes()).ok()?;
    let n = t.write_message(&inner, &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await?;
    let c = rlen(&mut s).await?;
    let n = t.read_message(&c, &mut buf).ok()?;
    let (_, welcome_op, _, _) = decode_frame(&buf[..n]).ok()?;
    if welcome_op != OP_WELCOME {
        return None;
    }
    // Unified auth; replacement confirmation is explicitly supplied in payload.
    let inner = encode_frame(op, payload).ok()?;
    let n = t.write_message(&inner, &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await?;
    let c = rlen(&mut s).await?;
    let n = t.read_message(&c, &mut buf).ok()?;
    let (_, op, p, _) = decode_frame(&buf[..n]).ok()?;
    if op != OP_AUTHENTICATED || auth::parse_authenticated(p).is_err() {
        return None;
    }
    Some((p[..16].to_vec(), p[16..].to_vec()))
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

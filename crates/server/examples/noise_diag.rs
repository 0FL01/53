//! Diag-инициатор Noise IK: тот же handshake, что в noise_probe, но точка входа —
//! TCP-порт локального slipstream-client (т.е. через DNS-путь, а не loopback к msgd).
//! Env: DIAG_PORT, DIAG_DOMAIN, DIAG_SERVER_PUB (hex, публичный — не секрет).
//! Печатает `RESULT PASS|FAIL`. Server-local использование, не публичный API.

use dmsg_protocol::{decode_frame, encode_frame, OP_AUTH_DOMAIN, OP_WELCOME};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let port: u16 = std::env::var("DIAG_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let domain = std::env::var("DIAG_DOMAIN").unwrap_or_default();
    let pubhex = std::env::var("DIAG_SERVER_PUB").unwrap_or_default();
    let server_pub = match hex_to32(&pubhex) {
        Some(k) if port != 0 && !domain.is_empty() => k,
        _ => {
            eprintln!("usage: DIAG_PORT=p DIAG_DOMAIN=d DIAG_SERVER_PUB=hex noise_diag");
            return std::process::ExitCode::from(2);
        }
    };
    match run(port, &domain, &server_pub).await {
        Some(true) => {
            println!("RESULT PASS");
            std::process::ExitCode::SUCCESS
        }
        _ => {
            println!("RESULT FAIL");
            std::process::ExitCode::from(1)
        }
    }
}

async fn run(port: u16, domain: &str, server_pub: &[u8; 32]) -> Option<bool> {
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
    let mut t = hs.into_transport_mode().ok()?;
    let inner = encode_frame(OP_AUTH_DOMAIN, domain.as_bytes()).ok()?;
    let n = t.write_message(&inner, &mut buf).ok()?;
    wlen(&mut s, &buf[..n]).await?;
    let c = rlen(&mut s).await?;
    let n = t.read_message(&c, &mut buf).ok()?;
    let (_, op, p, _) = decode_frame(&buf[..n]).ok()?;
    Some(op == OP_WELCOME && p == domain.as_bytes())
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

//! msgd stub (S2): принимает TCP от slipstream-target, сверяет домен, отвечает WELCOME.
//! Wire с первого дня: [version:u8][opcode:u8][len:u16 BE][payload..len], frame ≤ 16 KiB.
//! opcode 1 = HELLO (payload: ожидаемый клиентом домен), opcode 2 = WELCOME.
//! Неизвестная версия/opcode, oversize, чужой домен → close (+ счётчик, без ban:
//! per-IP ban на общем резолвере отключил бы всех его клиентов, ARCH §4).

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};

const VERSION: u8 = 1;
const OP_HELLO: u8 = 1;
const OP_WELCOME: u8 = 2;
const MAX_FRAME: usize = 16 * 1024;
const DOMAIN_MAX: usize = 253;

#[derive(Default)]
struct Counters {
    hello_ok: AtomicU64,
    mismatch: AtomicU64,
    proto_err: AtomicU64,
}

struct Config {
    domain: String,
    listen: String,
    data_dir: PathBuf,
    msgctl_sock: PathBuf,
}

fn load_config() -> Result<Config, String> {
    let domain = env::var("DMSG_DOMAIN").map_err(|_| "DMSG_DOMAIN is required".to_string())?;
    if domain.is_empty() || domain.len() > DOMAIN_MAX || !domain.is_ascii() {
        return Err("DMSG_DOMAIN invalid (empty, >253 or non-ascii)".to_string());
    }
    Ok(Config {
        domain,
        listen: env::var("MSGD_LISTEN").unwrap_or_else(|_| "127.0.0.1:7000".into()),
        data_dir: env::var("MSGD_DATA_DIR").map(PathBuf::from).unwrap_or("/var/lib/msgd".into()),
        msgctl_sock: env::var("MSGCTL_SOCK")
            .map(PathBuf::from)
            .unwrap_or("/var/lib/msgd/msgctl.sock".into()),
    })
}

async fn read_frame(stream: &mut tokio::net::TcpStream) -> Result<(u8, u8, Vec<u8>), &'static str> {
    let mut hdr = [0u8; 4];
    stream.read_exact(&mut hdr).await.map_err(|_| "eof")?;
    let (ver, op) = (hdr[0], hdr[1]);
    let len = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
    if ver != VERSION {
        return Err("unknown-version");
    }
    if len > MAX_FRAME {
        return Err("oversize");
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.map_err(|_| "eof")?;
    Ok((ver, op, payload))
}

async fn handle_tcp(mut stream: tokio::net::TcpStream, cfg: Arc<Config>, c: Arc<Counters>) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or("?".into());
    let (ver, op, payload) = match read_frame(&mut stream).await {
        Ok(f) => f,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: proto error from {peer}: {e}");
            return;
        }
    };
    if op != OP_HELLO {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
        eprintln!("msgd: unexpected opcode {op} (v{ver}) from {peer}");
        return;
    }
    if payload != cfg.domain.as_bytes() {
        c.mismatch.fetch_add(1, Ordering::Relaxed);
        eprintln!("msgd: domain mismatch from {peer} (close, no ban)");
        return;
    }
    let d = cfg.domain.as_bytes();
    let mut out = Vec::with_capacity(4 + d.len());
    out.push(VERSION);
    out.push(OP_WELCOME);
    out.extend_from_slice(&(d.len() as u16).to_be_bytes());
    out.extend_from_slice(d);
    if stream.write_all(&out).await.is_err() {
        return;
    }
    c.hello_ok.fetch_add(1, Ordering::Relaxed);
}

async fn serve_msgctl(cfg: Arc<Config>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&cfg.msgctl_sock);
    let listener = UnixListener::bind(&cfg.msgctl_sock)?;
    eprintln!("msgd: msgctl on {}", cfg.msgctl_sock.display());
    loop {
        let (mut sock, _) = listener.accept().await?;
        let cfg = cfg.clone();
        tokio::spawn(async move {
            let mut line = Vec::new();
            let mut buf = [0u8; 1];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if buf[0] == b'\n' {
                            break;
                        }
                        if line.len() < 64 {
                            line.push(buf[0]);
                        }
                    }
                    Err(_) => break,
                }
            }
            let reply = match line.as_slice() {
                b"ping" => "pong\n".to_string(),
                b"domain" => format!("{}\n", cfg.domain),
                _ => "err\n".to_string(),
            };
            let _ = sock.write_all(reply.as_bytes()).await;
        });
    }
}

async fn run(cfg: Config) -> std::io::Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let listener = TcpListener::bind(&cfg.listen).await?;
    eprintln!("msgd: stub listening on {} for domain {}", cfg.listen, cfg.domain);
    let cfg = Arc::new(cfg);
    let counters = Arc::new(Counters::default());
    let ctl = cfg.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_msgctl(ctl).await {
            eprintln!("msgd: msgctl error: {e}");
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;
        let cfg = cfg.clone();
        let counters = counters.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(10), handle_tcp(stream, cfg, counters)).await;
        });
    }
}

async fn msgctl_client(sock: &str, cmd: &str) -> std::io::Result<()> {
    let mut stream = tokio::net::UnixStream::connect(sock).await?;
    stream.write_all(format!("{cmd}\n").as_bytes()).await?;
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await?;
    print!("{}", String::from_utf8_lossy(&out));
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    if args.next().as_deref() == Some("msgctl") {
        let sock = env::var("MSGCTL_SOCK").unwrap_or("/var/lib/msgd/msgctl.sock".into());
        let cmd = args.next().unwrap_or_default();
        return match msgctl_client(&sock, &cmd).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("msgctl: {e}");
                ExitCode::from(1)
            }
        };
    }
    match load_config() {
        Ok(cfg) => {
            if let Err(e) = run(cfg).await {
                eprintln!("msgd: fatal: {e}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("msgd: config: {e}");
            ExitCode::from(2)
        }
    }
}

//! msgd stub (S2) + проводка P1: принимает TCP от slipstream-target, сверяет домен,
//! отвечает WELCOME. Wire — из dmsg-protocol (полный кадр ≤ 16 KiB).
//! opcode 1 = HELLO (payload: ожидаемый клиентом домен), opcode 2 = WELCOME.
//! Неизвестная версия/opcode, oversize, чужой домен → close (+ счётчик, без ban:
//! per-IP ban на общем резолвере отключил бы всех его клиентов, ARCH §4).
//! P1: при старте открывает SQLite (WAL + synchronous=FULL, миграции), бизнес-логики нет.

mod db;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};

use dmsg_protocol::{decode_frame, encode_frame, DOMAIN_MAX, OP_HELLO, OP_WELCOME};

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
    blobs_dir: PathBuf,
    msgctl_sock: PathBuf,
    schema_version: i64,
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
        blobs_dir: env::var("MSGD_BLOBS_DIR")
            .map(PathBuf::from)
            .unwrap_or("/var/lib/msgd/blobs".into()),
        msgctl_sock: env::var("MSGCTL_SOCK")
            .map(PathBuf::from)
            .unwrap_or("/var/lib/msgd/msgctl.sock".into()),
        schema_version: 0, // заполняется в run() после db::open
    })
}

async fn read_frame(stream: &mut tokio::net::TcpStream) -> Result<(u8, u8, Vec<u8>), &'static str> {
    use dmsg_protocol::{HEADER_LEN, MAX_FRAME, VERSION};
    let mut hdr = [0u8; HEADER_LEN];
    stream.read_exact(&mut hdr).await.map_err(|_| "eof")?;
    if hdr[0] != VERSION {
        return Err("unknown-version");
    }
    let total = HEADER_LEN + u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
    if total > MAX_FRAME {
        return Err("oversize");
    }
    let mut buf = vec![0u8; total];
    buf[..HEADER_LEN].copy_from_slice(&hdr);
    stream.read_exact(&mut buf[HEADER_LEN..]).await.map_err(|_| "eof")?;
    match decode_frame(&buf) {
        Ok((ver, op, payload, _)) => Ok((ver, op, payload.to_vec())),
        Err(_) => Err("oversize"), // недостижимо: размер уже проверен
    }
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
    let out = encode_frame(OP_WELCOME, d).expect("domain fits frame");
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
                b"dbversion" => format!("{}\n", cfg.schema_version),
                _ => "err\n".to_string(),
            };
            let _ = sock.write_all(reply.as_bytes()).await;
        });
    }
}

async fn run(cfg: Config) -> std::io::Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    std::fs::create_dir_all(&cfg.blobs_dir)?;
    let schema_version = db::open(cfg.data_dir.join("msgd.db")).map_err(std::io::Error::other)?;
    eprintln!("msgd: db schema version {schema_version}");
    let cfg = Config { schema_version, ..cfg };
    eprintln!("msgd: stub listening on {} for domain {}", cfg.listen, cfg.domain);
    let listener = TcpListener::bind(&cfg.listen).await?;
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

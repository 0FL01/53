//! msgd P2: только Noise IK поверх TCP от slipstream-target.
//! На каждый коннект — свой snow-responder → TransportState (1 TCP = 1 stream,
//! общий счётчик между коннектами запрещён — иначе nonce reuse).
//! Plaintext-HELLO удалён; домен сверяется первым transport-сообщением
//! OP_AUTH_DOMAIN внутри шифрованного канала. Неизвестный key после handshake
//! получает только enrolment API (мясо P3). Pre-auth bounded: кап + timeout,
//! close без ban (per-IP ban на общем резолвере отключил бы всех, ARCH §4).

mod db;
mod noise;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::Semaphore;

use dmsg_protocol::{
    decode_frame, encode_frame, DOMAIN_MAX, MAX_FRAME, OP_AUTH_DOMAIN, OP_WELCOME,
};

/// Кап незавершённых handshake (≪16 пилота, резерв до транспортных 32).
const PRE_AUTH_CAP: usize = 8;

#[derive(Default)]
struct Counters {
    hs_ok: AtomicU64,
    hs_fail: AtomicU64,
    pre_auth_full: AtomicU64,
    auth_ok: AtomicU64,
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
    noise_key_file: PathBuf,
    noise_private: [u8; noise::KEY_LEN],
}

fn load_config() -> Result<Config, String> {
    let domain = env::var("DMSG_DOMAIN").map_err(|_| "DMSG_DOMAIN is required".to_string())?;
    if domain.is_empty() || domain.len() > DOMAIN_MAX || !domain.is_ascii() {
        return Err("DMSG_DOMAIN invalid (empty, >253 or non-ascii)".to_string());
    }
    let noise_key_file: PathBuf = env::var("NOISE_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or("/run/secrets/noise_key".into());
    let noise_private =
        noise::load_private(&noise_key_file).map_err(|e| format!("noise key: {e}"))?;
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
        noise_key_file,
        noise_private,
    })
}

/// Прочитать length-prefixed сообщение (2 байта BE + тело), bound MAX_FRAME.
/// Формат handshake-сообщений Noise (не app-фреймы).
async fn read_hs_msg(stream: &mut tokio::net::TcpStream) -> Result<Vec<u8>, &'static str> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).await.map_err(|_| "eof")?;
    let len = u16::from_be_bytes(hdr) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err("oversize");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|_| "eof")?;
    Ok(buf)
}

async fn write_hs_msg(stream: &mut tokio::net::TcpStream, msg: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(msg.len() as u16).to_be_bytes()).await?;
    stream.write_all(msg).await
}

/// Один коннект: Noise IK handshake → первое transport-сообщение обязано быть
/// OP_AUTH_DOMAIN с верным доменом → шифрованный WELCOME. Иначе close + счётчик.
async fn handle_conn(
    mut stream: tokio::net::TcpStream,
    cfg: Arc<Config>,
    c: Arc<Counters>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or("?".into());
    let mut hsbuf = vec![0u8; 65535];
    let mut hs = match snow::Builder::new(noise::PATTERN.parse().expect("pattern"))
        .local_private_key(&cfg.noise_private)
        .and_then(|b| b.build_responder())
    {
        Ok(h) => h,
        Err(e) => {
            c.hs_fail.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: hs init: {e}");
            return;
        }
    };
    // IK: <- читаем msg1, -> пишем msg2.
    let msg1 = match read_hs_msg(&mut stream).await {
        Ok(m) => m,
        Err(e) => {
            c.hs_fail.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: hs read from {peer}: {e}");
            return;
        }
    };
    // Обнуляем принятое сразу после обработки (half-open hygiene).
    let r1 = hs.read_message(&msg1, &mut hsbuf);
    hsbuf.fill(0);
    let mut msg1 = msg1;
    msg1.fill(0);
    if let Err(e) = r1 {
        c.hs_fail.fetch_add(1, Ordering::Relaxed);
        eprintln!("msgd: hs msg1 from {peer}: {e}");
        return;
    }
    let len = match hs.write_message(&[], &mut hsbuf) {
        Ok(n) => n,
        Err(e) => {
            c.hs_fail.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: hs msg2 to {peer}: {e}");
            return;
        }
    };
    if write_hs_msg(&mut stream, &hsbuf[..len]).await.is_err() {
        c.hs_fail.fetch_add(1, Ordering::Relaxed);
        return;
    }
    hsbuf.fill(0);
    let mut transport = hs.into_transport_mode().expect("IK complete");
    drop(_permit); // слот pre-auth освобождён: handshake завершён
    c.hs_ok.fetch_add(1, Ordering::Relaxed);

    // Транспорт: читаем шифрованное → decrypt → app-фрейм.
    let cipher = match read_hs_msg(&mut stream).await {
        Ok(m) => m,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport read from {peer}: {e}");
            return;
        }
    };
    let mut plain = vec![0u8; 65535];
    let n = match transport.read_message(&cipher, &mut plain) {
        Ok(n) => n,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport decrypt from {peer}: {e}");
            return;
        }
    };
    let mut cipher = cipher;
    cipher.fill(0);
    let (ver, op, payload, _) = match decode_frame(&plain[..n]) {
        Ok(f) => f,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport frame from {peer}: {e:?}");
            return;
        }
    };
    let _ = ver;
    if op != OP_AUTH_DOMAIN || payload != cfg.domain.as_bytes() {
        c.mismatch.fetch_add(1, Ordering::Relaxed);
        eprintln!("msgd: domain mismatch from {peer} (close, no ban)");
        return;
    }
    let inner = encode_frame(OP_WELCOME, cfg.domain.as_bytes()).expect("domain fits");
    let mut out = vec![0u8; 65535];
    let wn = match transport.write_message(&inner, &mut out) {
        Ok(n) => n,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport encrypt to {peer}: {e}");
            return;
        }
    };
    if write_hs_msg(&mut stream, &out[..wn]).await.is_err() {
        return;
    }
    out.fill(0);
    c.auth_ok.fetch_add(1, Ordering::Relaxed);
    // P3: здесь enrolment поверх установленного канала для неизвестных ключей.
}

async fn serve_msgctl(cfg: Arc<Config>, c: Arc<Counters>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&cfg.msgctl_sock);
    let listener = UnixListener::bind(&cfg.msgctl_sock)?;
    eprintln!("msgd: msgctl on {}", cfg.msgctl_sock.display());
    loop {
        let (mut sock, _) = listener.accept().await?;
        let cfg = cfg.clone();
        let c = c.clone();
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
                b"stats" => format!(
                    "hs_ok={} hs_fail={} pre_auth_full={} auth_ok={} mismatch={} proto_err={}\n",
                    c.hs_ok.load(Ordering::Relaxed),
                    c.hs_fail.load(Ordering::Relaxed),
                    c.pre_auth_full.load(Ordering::Relaxed),
                    c.auth_ok.load(Ordering::Relaxed),
                    c.mismatch.load(Ordering::Relaxed),
                    c.proto_err.load(Ordering::Relaxed),
                ),
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
    eprintln!("msgd: noise listening on {} for domain {}", cfg.listen, cfg.domain);
    eprintln!("msgd: noise key from {}", cfg.noise_key_file.display());
    let listener = TcpListener::bind(&cfg.listen).await?;
    let cfg = Arc::new(cfg);
    let counters = Arc::new(Counters::default());
    let pre_auth = Arc::new(Semaphore::new(PRE_AUTH_CAP));
    let ctl = cfg.clone();
    let ctl_c = counters.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_msgctl(ctl, ctl_c).await {
            eprintln!("msgd: msgctl error: {e}");
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;
        let cfg = cfg.clone();
        let counters = counters.clone();
        // Кап pre-auth: нет слота — сразу close, слот не удерживаем.
        let permit = match pre_auth.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                counters.pre_auth_full.fetch_add(1, Ordering::Relaxed);
                drop(stream);
                continue;
            }
        };
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                handle_conn(stream, cfg, counters, permit),
            )
            .await;
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
    match args.next().as_deref() {
        Some("msgctl") => {
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
        Some("keygen") => {
            let mut out: Option<String> = None;
            while let Some(a) = args.next() {
                if a == "--out" {
                    out = args.next();
                }
            }
            return match out {
                Some(p) => match noise::keygen(&p) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("keygen: {e}");
                        ExitCode::from(1)
                    }
                },
                None => {
                    eprintln!("usage: msgd keygen --out <file>");
                    ExitCode::from(2)
                }
            };
        }
        Some("pubkey") => {
            let mut key: Option<String> = None;
            while let Some(a) = args.next() {
                if a == "--key" {
                    key = args.next();
                }
            }
            return match key {
                Some(p) => match noise::pubkey_hex(&p) {
                    Ok(h) => {
                        println!("{h}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("pubkey: {e}");
                        ExitCode::from(1)
                    }
                },
                None => {
                    eprintln!("usage: msgd pubkey --key <file>");
                    ExitCode::from(2)
                }
            };
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            return ExitCode::from(2);
        }
        None => {}
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

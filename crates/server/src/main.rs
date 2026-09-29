//! msgd P2: только Noise IK поверх TCP от slipstream-target.
//! На каждый коннект — свой snow-responder → TransportState (1 TCP = 1 stream,
//! общий счётчик между коннектами запрещён — иначе nonce reuse).
//! Plaintext-HELLO удалён; домен сверяется первым transport-сообщением
//! OP_AUTH_DOMAIN внутри шифрованного канала. Неизвестный key после handshake
//! получает только enrolment API (мясо P3). Pre-auth bounded: кап + timeout,
//! close без ban (per-IP ban на общем резолвере отключил бы всех, ARCH §4).

mod db;
mod blob;
mod enrol;
mod mbox;
mod noise;
mod prekey;

use std::collections::HashMap;
use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{Notify, Semaphore};

use dmsg_protocol::{
    bootstrap, decode_frame, encode_frame, mailbox as mp, DOMAIN_MAX, ERR_BAD, ERR_BOUND_OTHER,
    ERR_EXPIRED, ERR_NO_PREKEY, ERR_QUOTA, ERR_REVOKED, MAX_FRAME, OP_AUTH_DOMAIN, OP_BLOB_RESERVE,
    OP_BLOB_RESERVED, OP_CLAIM, OP_COUNT, OP_COUNT_RESP, OP_DELIVERY_ACK, OP_ENROL, OP_ENROLLED,
    OP_ERROR, OP_FETCH, OP_FETCH_RESP, OP_PREKEY, OP_SEND, OP_SEND_ACK, OP_UPLOAD_PREKEYS,
    OP_WELCOME, ST_ACCEPTED, ST_DELIVERED,
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
    enrol_ok: AtomicU64,
    enrol_fail: AtomicU64,
    send_ok: AtomicU64,
    send_dedup: AtomicU64,
    send_fail: AtomicU64,
    fetch_ok: AtomicU64,
    ack_ok: AtomicU64,
    mbox_err: AtomicU64,
}

/// Разделяемое состояние: db под мьютексом (держать только на TX, никогда через .await).
struct State {
    cfg: Arc<Config>,
    counters: Arc<Counters>,
    db: Arc<Mutex<rusqlite::Connection>>,
    live: Arc<Mutex<HashMap<Vec<u8>, Vec<Arc<Notify>>>>>,
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
    carrier_cert_file: PathBuf,
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
    let carrier_cert_file: PathBuf = env::var("CARRIER_CERT_FILE")
        .map(PathBuf::from)
        .unwrap_or("/run/secrets/carrier_cert.pem".into());
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
        carrier_cert_file,
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

/// Один коннект: Noise IK handshake → AUTH_DOMAIN → WELCOME → цикл сессии.
/// device_key — static инициатора из IK (get_remote_static), НЕ из тел сообщений.
/// После ENROLLED задача регистрирует Notify в live-карте; revoke будит и закрывает.
async fn handle_conn(
    mut stream: tokio::net::TcpStream,
    st: Arc<State>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let cfg = &st.cfg;
    let c = &st.counters;
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
    // Static инициатора — только из сессии. IK гарантирует его наличие в msg1.
    let device_key: [u8; 32] = match hs.get_remote_static() {
        Some(k) if k.len() == 32 => {
            let mut b = [0u8; 32];
            b.copy_from_slice(k);
            b
        }
        _ => {
            c.hs_fail.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: hs no remote static from {peer}");
            return;
        }
    };
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

    // Цикл сессии: до enrol — только ENROL; после — ENROL(replay)/SEND/FETCH/DELIVERY_ACK.
    // Неизвестное → close. Revoke/block будит через Notify → graceful close.
    // В конце — deregister. DB-транзакции короткие, никогда через .await.
    let notify = Arc::new(Notify::new());
    let mut enrolled_key: Option<Vec<u8>> = None;
    let mut enrolled_user: Option<[u8; 16]> = None;
    let close = async {
        loop {
            let cipher = read_hs_msg(&mut stream).await.map_err(|_| ())?;
            let mut plain = vec![0u8; 65535];
            let n = transport.read_message(&cipher, &mut plain).map_err(|_| ())?;
            let mut cipher = cipher;
            cipher.fill(0);
            let (_, op, payload, _) = decode_frame(&plain[..n]).map_err(|_| ())?;
            let reply = if op == OP_ENROL && payload.len() == 32 {
                enrol_reply(&st, &c, &device_key, &payload, &notify, &mut enrolled_key, &mut enrolled_user)?
            } else if enrolled_user.is_some() {
                let user = enrolled_user.expect("checked");
                match op {
                    OP_SEND => send_reply(&st, &c, &device_key, &payload)?,
                    OP_FETCH => fetch_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_DELIVERY_ACK => ack_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_UPLOAD_PREKEYS => upload_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_CLAIM => claim_reply(&st, &c, &payload)?,
                    OP_COUNT => count_reply(&st, &c, &payload)?,
                    OP_BLOB_RESERVE => reserve_reply(&st, &c, &user, &payload)?,
                    _ => {
                        c.proto_err.fetch_add(1, Ordering::Relaxed);
                        return Err(());
                    }
                }
            } else {
                c.proto_err.fetch_add(1, Ordering::Relaxed);
                return Err(());
            };
            let mut out = vec![0u8; 65535];
            let wn = transport.write_message(&reply, &mut out).map_err(|_| ())?;
            write_hs_msg(&mut stream, &out[..wn]).await.map_err(|_| ())?;
            out.fill(0);
        }
        #[allow(unreachable_code)]
        Ok::<(), ()>(())
    };
    tokio::select! {
        _ = notify.notified() => {
            eprintln!("msgd: session revoked, closing {peer}");
        }
        _ = close => {}
    }
    if let Some(k) = enrolled_key {
        let mut live = st.live.lock().expect("live");
        if let Some(v) = live.get_mut(&k) {
            v.retain(|n| !Arc::ptr_eq(n, &notify));
            if v.is_empty() {
                live.remove(&k);
            }
        }
    }
}

fn enrol_code(e: &enrol::EnrolError) -> u8 {
    match e {
        enrol::EnrolError::Bad | enrol::EnrolError::Store(_) => ERR_BAD,
        enrol::EnrolError::Expired => ERR_EXPIRED,
        enrol::EnrolError::Revoked => ERR_REVOKED,
        enrol::EnrolError::BoundOther => ERR_BOUND_OTHER,
    }
}

/// ENROL → CAS под коротким локом → ENROLLED/ERROR. Регистрирует live-сессию.
#[allow(clippy::too_many_arguments)]
fn enrol_reply(
    st: &State,
    c: &Counters,
    device_key: &[u8; 32],
    payload: &[u8],
    notify: &Arc<Notify>,
    enrolled_key: &mut Option<Vec<u8>>,
    enrolled_user: &mut Option<[u8; 16]>,
) -> Result<Vec<u8>, ()> {
    let now = now_secs();
    let res = {
        let db = st.db.lock().expect("db");
        enrol::enrol(&db, payload, device_key, now)
    };
    match res {
        Ok(done) => {
            c.enrol_ok.fetch_add(1, Ordering::Relaxed);
            let mut p = Vec::with_capacity(28);
            p.extend_from_slice(&done.user_id);
            p.extend_from_slice(done.contact_id.as_bytes());
            *enrolled_key = Some(device_key.to_vec());
            *enrolled_user = Some(done.user_id);
            st.live
                .lock()
                .expect("live")
                .entry(device_key.to_vec())
                .or_default()
                .push(notify.clone());
            Ok(encode_frame(OP_ENROLLED, &p).expect("fits"))
        }
        Err(e) => {
            c.enrol_fail.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[enrol_code(&e)]).expect("fits"))
        }
    }
}

fn mbox_code(e: &mbox::MboxError) -> u8 {
    match e {
        mbox::MboxError::Bad | mbox::MboxError::Store(_) => ERR_BAD,
        mbox::MboxError::Quota => ERR_QUOTA,
    }
}

/// SEND → quota-в-TX → INSERT ON CONFLICT → commit → SEND_ACK.
/// Повтор возвращает прежний accept (send_dedup), не новое событие.
fn send_reply(st: &State, c: &Counters, device_key: &[u8; 32], payload: &[u8]) -> Result<Vec<u8>, ()> {
    let s = mp::parse_send(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let now = now_secs();
    let res = {
        let mut db = st.db.lock().expect("db");
        mbox::send(&mut db, device_key, s.recipient, s.message_id, s.ciphertext, now)
    };
    match res {
        Ok(mbox::SendOutcome::New(_)) => {
            c.send_ok.fetch_add(1, Ordering::Relaxed);
            let mut p = Vec::with_capacity(17);
            p.extend_from_slice(s.message_id);
            p.push(ST_ACCEPTED);
            Ok(encode_frame(OP_SEND_ACK, &p).expect("fits"))
        }
        Ok(mbox::SendOutcome::Exists(_, delivered)) => {
            c.send_dedup.fetch_add(1, Ordering::Relaxed);
            let mut p = Vec::with_capacity(17);
            p.extend_from_slice(s.message_id);
            // Повтор после доставки сообщает актуальный статус (ST_DELIVERED),
            // иначе — прежний accept. Нового события нет в обоих случаях.
            p.push(if delivered { ST_DELIVERED } else { ST_ACCEPTED });
            Ok(encode_frame(OP_SEND_ACK, &p).expect("fits"))
        }
        Err(e) => {
            c.send_fail.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[mbox_code(&e)]).expect("fits"))
        }
    }
}

/// FETCH → пачка после cursor (влезает в кадр) → FETCH_RESP.
fn fetch_reply(
    st: &State,
    c: &Counters,
    user: &[u8; 16],
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    if !payload.is_empty() {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
        return Err(());
    }
    let rows = {
        let db = st.db.lock().expect("db");
        mbox::fetch(&db, user, device_key)
    }
    .map_err(|_| {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
    })?;
    // Пакуем, пока влезает в кадр (заголовок 4 + count 2 + записи).
    let mut p = vec![0u8, 0u8];
    let mut n: usize = 0;
    for e in &rows {
        let need = 8 + 32 + 16 + 2 + e.ciphertext.len();
        if 4 + p.len() + need > dmsg_protocol::MAX_FRAME || n >= dmsg_protocol::FETCH_BATCH_MAX {
            break;
        }
        p.extend_from_slice(&(e.seq as u64).to_be_bytes());
        p.extend_from_slice(&e.sender);
        p.extend_from_slice(&e.message_id);
        p.extend_from_slice(&(e.ciphertext.len() as u16).to_be_bytes());
        p.extend_from_slice(&e.ciphertext);
        n += 1;
    }
    p[0] = (n >> 8) as u8;
    p[1] = n as u8;
    c.fetch_ok.fetch_add(1, Ordering::Relaxed);
    Ok(encode_frame(OP_FETCH_RESP, &p).expect("fits"))
}

/// DELIVERY_ACK → cursor по непрерывному в той же TX → ответ: cursor u64.
fn ack_reply(
    st: &State,
    c: &Counters,
    user: &[u8; 16],
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    let seqs = mp::parse_delivery_ack(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let seqs: Vec<i64> = seqs.into_iter().map(|s| s as i64).collect();
    let cursor = {
        let mut db = st.db.lock().expect("db");
        mbox::ack(&mut db, user, device_key, &seqs)
    }
    .map_err(|_| {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
    })?;
    c.ack_ok.fetch_add(1, Ordering::Relaxed);
    Ok(encode_frame(OP_DELIVERY_ACK, &(cursor as u64).to_be_bytes()).expect("fits"))
}

/// UPLOAD_PREKEYS → binding-check → ответ COUNT_RESP (refill-сигнал).
fn upload_reply(
    st: &State,
    c: &Counters,
    user: &[u8; 16],
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    let (identity, entries) = mp::parse_upload(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let entries: Vec<prekey::Entry> = entries
        .into_iter()
        .map(|(key_id, one_time, pubkey, sig)| prekey::Entry { key_id, one_time, pubkey, sig })
        .collect();
    let res = {
        let mut db = st.db.lock().expect("db");
        prekey::upload(&mut db, device_key, user, identity, &entries)
    };
    match res {
        Ok(left) => {
            c.ack_ok.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_COUNT_RESP, &(left as u32).to_be_bytes()).expect("fits"))
        }
        Err(prekey::PrekeyError::IdentityChanged) => {
            c.mbox_err.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[ERR_BAD]).expect("fits"))
        }
        Err(_) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[ERR_BAD]).expect("fits"))
        }
    }
}

/// CLAIM → PREKEY или ERROR no-prekey.
fn claim_reply(st: &State, c: &Counters, payload: &[u8]) -> Result<Vec<u8>, ()> {
    let device = mp::parse_device(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let got = {
        let mut db = st.db.lock().expect("db");
        prekey::claim(&mut db, device)
    }
    .map_err(|_| {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
    })?;
    match got {
        Some((id, pubkey)) => {
            let mut p = Vec::with_capacity(36);
            p.extend_from_slice(&id.to_be_bytes());
            p.extend_from_slice(&pubkey);
            Ok(encode_frame(OP_PREKEY, &p).expect("fits"))
        }
        None => Ok(encode_frame(OP_ERROR, &[ERR_NO_PREKEY]).expect("fits")),
    }
}

/// COUNT → COUNT_RESP.
fn count_reply(st: &State, c: &Counters, payload: &[u8]) -> Result<Vec<u8>, ()> {
    let device = mp::parse_device(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let n = {
        let db = st.db.lock().expect("db");
        prekey::count(&db, device)
    }
    .map_err(|_| {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
    })?;
    Ok(encode_frame(OP_COUNT_RESP, &(n as u32).to_be_bytes()).expect("fits"))
}

fn blob_code(e: &blob::BlobError) -> u8 {
    match e {
        blob::BlobError::Bad | blob::BlobError::Store(_) => ERR_BAD,
        blob::BlobError::Quota => ERR_QUOTA,
    }
}

/// BLOB_RESERVE → BLOB_RESERVED или ERROR quota.
fn reserve_reply(
    st: &State,
    c: &Counters,
    user: &[u8; 16],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    let (blob_id, size) = mp::parse_reserve(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let now = now_secs();
    let res = {
        let mut db = st.db.lock().expect("db");
        blob::reserve(&mut db, user, blob_id, size as i64, now)
    };
    match res {
        Ok(()) => Ok(encode_frame(OP_BLOB_RESERVED, blob_id).expect("fits")),
        Err(e) => {
            c.mbox_err.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[blob_code(&e)]).expect("fits"))
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Разбудить живые сессии устройства (revoke/block) → graceful close.
fn wake_device(st: &State, device_key: &[u8]) {
    if let Some(v) = st.live.lock().expect("live").remove(device_key) {
        for n in v {
            n.notify_waiters();
        }
    }
}

/// Cap строки msgctl: invite-revoke/device-block с 64 hex не влезли бы в 64.
const MSGCTL_LINE_MAX: usize = 256;

/// Одна команда msgctl. Токены/ключи из аргументов никогда не попадают в логи.
fn handle_msgctl(st: &State, line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let mut parts = text.split_whitespace();
    match parts.next().unwrap_or("") {
        "ping" => "pong\n".into(),
        "domain" => format!("{}\n", st.cfg.domain),
        "dbversion" => format!("{}\n", st.cfg.schema_version),
        "stats" => {
            let c = &st.counters;
            format!(
                "hs_ok={} hs_fail={} pre_auth_full={} auth_ok={} mismatch={} proto_err={} enrol_ok={} enrol_fail={} send_ok={} send_dedup={} send_fail={} fetch_ok={} ack_ok={} mbox_err={}\n",
                c.hs_ok.load(Ordering::Relaxed),
                c.hs_fail.load(Ordering::Relaxed),
                c.pre_auth_full.load(Ordering::Relaxed),
                c.auth_ok.load(Ordering::Relaxed),
                c.mismatch.load(Ordering::Relaxed),
                c.proto_err.load(Ordering::Relaxed),
                c.enrol_ok.load(Ordering::Relaxed),
                c.enrol_fail.load(Ordering::Relaxed),
                c.send_ok.load(Ordering::Relaxed),
                c.send_dedup.load(Ordering::Relaxed),
                c.send_fail.load(Ordering::Relaxed),
                c.fetch_ok.load(Ordering::Relaxed),
                c.ack_ok.load(Ordering::Relaxed),
                c.mbox_err.load(Ordering::Relaxed),
            )
        }
        "invite-issue" => {
            let ttl: i64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(24 * 3600);
            match msgctl_issue(st, ttl.max(1)) {
                Ok(uri) => format!("{uri}\n"),
                Err(_) => "err\n".into(),
            }
        }
        "invite-revoke" => match parts.next().and_then(hex32) {
            Some(tok) => match msgctl_revoke(st, &tok) {
                Ok(()) => "ok\n".into(),
                Err(_) => "err\n".into(),
            },
            None => "err\n".into(),
        },
        "invite-list" => msgctl_list(st),
        "device-block" => match parts.next().and_then(hex32) {
            Some(key) => match msgctl_block(st, &key) {
                Ok(()) => "ok\n".into(),
                Err(_) => "err\n".into(),
            },
            None => "err\n".into(),
        },
        "device-unblock" => match parts.next().and_then(hex32) {
            Some(key) => match msgctl_unblock(st, &key) {
                Ok(()) => "ok\n".into(),
                Err(_) => "err\n".into(),
            },
            None => "err\n".into(),
        },
        "user-list" => msgctl_users(st),
        "quotas" => match parts.next() {
            Some(f) => match valid_prefix(f) {
                Some(p) => msgctl_quotas(st, Some(&p)),
                None => "err\n".into(),
            },
            None => msgctl_quotas(st, None),
        },
        "gc" => {
            let now = now_secs();
            let mut db = st.db.lock().expect("db");
            match blob::gc(&mut db, now) {
                Ok((b, e)) => format!("gc blobs={b} events={e}\n"),
                Err(_) => "err\n".into(),
            }
        }
        "backup" => match msgctl_backup(st) {
            Ok(rep) => format!("{rep}\n"),
            Err(_) => "err\n".into(),
        },
        _ => "err\n".into(),
    }
}

/// Backup-минимум: VACUUM INTO (атомарный снапшот без остановки записи) +
/// копия дерева blobs + integrity_check копии + ротация (держать 3).
/// Секреты НЕ входят (статичны; оператор архивирует secrets/ отдельно, см. runbook).
/// Файловый кросс-чек blob_meta↔файлы станет осмысленным в M5, когда чанки
/// лягут на диск; сейчас отчёт содержит оба счётчика без гейта.
fn msgctl_backup(st: &State) -> Result<String, String> {
    let ts = now_secs();
    let snap = st.cfg.data_dir.join("backup").join(format!("snap-{ts}"));
    std::fs::create_dir_all(snap.join("blobs")).map_err(|e| format!("mkdir: {e}"))?;
    let db_path = snap.join("msgd.db");
    {
        let db = st.db.lock().expect("db");
        let lit = db_path.to_string_lossy().replace('\'', "''");
        db.execute_batch(&format!("VACUUM INTO '{lit}'"))
            .map_err(|e| format!("vacuum: {e}"))?;
    }
    // Копия blobs обычным копированием: blobs-data — отдельный FS, хардлинки (EXDEV) невозможны.
    let (mut files, mut bytes) = (0usize, 0u64);
    let mut stack = vec![st.cfg.blobs_dir.clone()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(&st.cfg.blobs_dir).map_err(|e| format!("rel: {e}"))?;
            let dst = snap.join("blobs").join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            match std::fs::copy(&p, &dst) {
                Ok(n) => {
                    files += 1;
                    bytes += n;
                }
                Err(e) => {
                    // Гонка с GC/записью: файл ушёл из-под ног — честно прерываем.
                    return Err(format!("copy: {e}"));
                }
            }
        }
    }
    // Verify копии: integrity_check обязан вернуть ровно 'ok'.
    {
        let copy = rusqlite::Connection::open(&db_path).map_err(|e| format!("open: {e}"))?;
        let ok: String = copy
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| format!("check: {e}"))?;
        if ok.to_lowercase() != "ok" {
            return Err("integrity: corrupt".into());
        }
    }
    let db_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    // Ротация: держать 3 newest snap-*, старые тереть.
    if let Ok(rd) = std::fs::read_dir(snap.parent().expect("backup dir")) {
        let mut snaps: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("snap-")).unwrap_or(false)
            })
            .collect();
        snaps.sort();
        while snaps.len() > 3 {
            let old = snaps.remove(0);
            let _ = std::fs::remove_dir_all(&old);
        }
    }
    // Счётчики для отчёта (без гейта до M5).
    let (rows, _) = {
        let db = st.db.lock().expect("db");
        let rows: i64 = db
            .query_row("SELECT COUNT(*) FROM blob_meta", [], |r| r.get(0))
            .unwrap_or(0);
        (rows, 0)
    };
    let _ = rows;
    eprintln!("msgd: backup done");
    Ok(format!(
        "backup path={} db={db_bytes} blobs={files} blobs_bytes={bytes}",
        snap.display()
    ))
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(b)
}

/// Выпуск invite: token → INSERT → сборка dmsg://join URI. URI уходит только
/// в stdout админа (сервер-локально); в логи — лишь факт выпуска, без token.
fn msgctl_issue(st: &State, ttl_secs: i64) -> Result<String, String> {
    let mut token = [0u8; 32];
    getrandom::fill(&mut token).map_err(|e| format!("rng: {e}"))?;
    let now = now_secs();
    {
        let db = st.db.lock().expect("db");
        db.execute(
            "INSERT INTO invites(token, created_at, expires_at, revoked) VALUES(?1,?2,?3,0)",
            rusqlite::params![token.as_slice(), now, now + ttl_secs],
        )
        .map_err(|e| format!("insert: {e}"))?;
    }
    let cert_der = std::fs::read(&st.cfg.carrier_cert_file)
        .map_err(|e| format!("carrier cert: {e}"))?;
    // PEM или DER: PEM начинается с -----BEGIN, DER — с 0x30.
    let cert_der = if cert_der.starts_with(b"-----BEGIN") {
        pem_to_der(&cert_der).ok_or("carrier cert: bad PEM")?
    } else {
        cert_der
    };
    let pubkey = noise::pubkey_of(&st.cfg.noise_private);
    let uri = bootstrap::build(st.cfg.domain.as_bytes(), &cert_der, &pubkey, &token)
        .map_err(|e| format!("bootstrap: {e:?}"))?;
    eprintln!("msgd: invite issued");
    Ok(uri)
}

fn pem_to_der(pem: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let mut b64 = String::new();
    let mut in_body = false;
    for line in text.lines() {
        if line.starts_with("-----BEGIN") {
            in_body = true;
            continue;
        }
        if line.starts_with("-----END") {
            break;
        }
        if in_body {
            b64.push_str(line.trim());
        }
    }
    if b64.is_empty() {
        return None;
    }
    b64_standard_decode(&b64)
}

fn b64_standard_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8; 128] = &{
        let mut t = [255u8; 128];
        let mut i = 0u8;
        while i < 26 {
            t[(b'A' + i) as usize] = i;
            t[(b'a' + i) as usize] = 26 + i;
            i += 1;
        }
        let mut j = 0u8;
        while j < 10 {
            t[(b'0' + j) as usize] = 52 + j;
            j += 1;
        }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &ch in s.as_bytes() {
        if (ch as usize) >= 128 || T[ch as usize] == 255 {
            return None;
        }
        acc = (acc << 6) | T[ch as usize] as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Отзыв invite: revoke + будим живые сессии привязанного устройства.
fn msgctl_revoke(st: &State, token: &[u8; 32]) -> Result<(), String> {
    let bound: Option<Vec<u8>> = {
        let db = st.db.lock().expect("db");
        db.execute(
            "UPDATE invites SET revoked=1 WHERE token=?1",
            [token.as_slice()],
        )
        .map_err(|e| format!("revoke: {e}"))?;
        db.query_row(
            "SELECT bound_device_key FROM invites WHERE token=?1",
            [token.as_slice()],
            |r| r.get(0),
        )
        .map_err(|e| format!("lookup: {e}"))?
    };
    if let Some(k) = bound {
        wake_device(st, &k);
    }
    eprintln!("msgd: invite revoked");
    Ok(())
}

/// Блокировка устройства: revoke + закрыть живые сессии.
fn msgctl_block(st: &State, device_key: &[u8; 32]) -> Result<(), String> {
    {
        let db = st.db.lock().expect("db");
        db.execute(
            "UPDATE devices SET revoked=1 WHERE device_key=?1",
            [device_key.as_slice()],
        )
        .map_err(|e| format!("block: {e}"))?;
    }
    wake_device(st, device_key);
    eprintln!("msgd: device blocked");
    Ok(())
}

/// Разблокировка устройства: снимает revoked. Сессии не будим — устройство
/// переподключается само. Это НЕ перепривязка: потерял телефон —
/// revoke + новый invite (см. runbook), ошибся блокировкой — unblock.
fn msgctl_unblock(st: &State, device_key: &[u8; 32]) -> Result<(), String> {
    {
        let db = st.db.lock().expect("db");
        db.execute(
            "UPDATE devices SET revoked=0 WHERE device_key=?1",
            [device_key.as_slice()],
        )
        .map_err(|e| format!("unblock: {e}"))?;
    }
    eprintln!("msgd: device unblocked");
    Ok(())
}

/// Список пользователей: префикс contact_id + устройства + флаг блокировки.
/// Полные ID/ключи — никогда (прецедент invite-list).
fn msgctl_users(st: &State) -> String {
    let db = st.db.lock().expect("db");
    let mut stmt = match db.prepare(
        "SELECT u.contact_id, COUNT(d.device_key), COALESCE(SUM(d.revoked),0)
         FROM users u LEFT JOIN devices d ON d.user_id=u.user_id
         GROUP BY u.user_id ORDER BY u.created_at",
    ) {
        Ok(s) => s,
        Err(_) => return "err\n".into(),
    };
    let mut out = String::new();
    let rows: Vec<(String, i64, i64)> = match stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map(|it| it.collect::<Result<Vec<_>, _>>())
    {
        Ok(Ok(v)) => v,
        _ => return "err\n".into(),
    };
    for (contact, devs, blocked) in rows {
        out.push_str(&format!("{contact} devices={devs} blocked={blocked}\n"));
    }
    out.push_str("ok\n");
    out
}

/// Префикс contact_id для фильтра quotas: Crockford Base32 без дефисов,
/// 1–12 символов. Остальное — err (защита LIKE от мусора).
fn valid_prefix(s: &str) -> Option<String> {
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(s.to_ascii_uppercase())
}

/// Квоты: та же формула, что mbox::send и blob::reserve (per-user),
/// иначе дрейф диагностики. Только чтение, лок короткий.
fn msgctl_quotas(st: &State, filter: Option<&str>) -> String {
    use dmsg_protocol::{MAILBOX_BYTES_MAX, MAILBOX_EVENTS_MAX};
    let db = st.db.lock().expect("db");
    let mut out = format!(
        "limits events={MAILBOX_EVENTS_MAX} bytes={MAILBOX_BYTES_MAX}\n"
    );
    let mut stmt = match db.prepare(
        "SELECT u.user_id, u.contact_id,
           (SELECT COUNT(*) FROM mailbox_events m WHERE m.recipient_user_id=u.user_id),
           (SELECT COALESCE(SUM(LENGTH(m.ciphertext)),0) FROM mailbox_events m WHERE m.recipient_user_id=u.user_id),
           (SELECT COALESCE(SUM(b.size),0) FROM blob_meta b WHERE b.owner_user_id=u.user_id AND b.state='reserved'),
           (SELECT COALESCE(SUM(b.size),0) FROM blob_meta b WHERE b.owner_user_id=u.user_id AND b.state<>'reserved')
         FROM users u ORDER BY u.created_at",
    ) {
        Ok(s) => s,
        Err(_) => return "err\n".into(),
    };
    let rows: Vec<(Vec<u8>, String, i64, i64, i64, i64)> = match stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .map(|it| it.collect::<Result<Vec<_>, _>>())
    {
        Ok(Ok(v)) => v,
        _ => return "err\n".into(),
    };
    for (_, contact, events, bytes, reserved, blobs) in rows {
        if let Some(f) = filter {
            if !contact.starts_with(f) {
                continue;
            }
        }
        out.push_str(&format!(
            "{contact} events={events} bytes={bytes} blobs_reserved={reserved} blobs_bytes={blobs}\n"
        ));
    }
    out.push_str("ok\n");
    out
}
fn msgctl_list(st: &State) -> String {
    let db = st.db.lock().expect("db");
    let mut stmt = match db.prepare(
        "SELECT token, created_at, expires_at, revoked, bound_device_key FROM invites ORDER BY created_at",
    ) {
        Ok(s) => s,
        Err(_) => return "err\n".into(),
    };
    let mut out = String::new();
    let rows = match stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return "err\n".into(),
    };
    for row in rows.flatten() {
        let (tok, created, expires, revoked, bound) = row;
        out.push_str(&format!(
            "{} created={} expires={} revoked={} bound={}\n",
            hex_prefix(&tok),
            created,
            expires,
            revoked,
            if bound.is_some() { "yes" } else { "no" }
        ));
    }
    out.push_str("ok\n");
    out
}

fn hex_prefix(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    b.iter()
        .take(8)
        .map(|x| {
            format!(
                "{}{}",
                H[(x >> 4) as usize] as char,
                H[(x & 15) as usize] as char
            )
        })
        .collect()
}

async fn serve_msgctl(st: Arc<State>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&st.cfg.msgctl_sock);
    let listener = UnixListener::bind(&st.cfg.msgctl_sock)?;
    // Сокет только оператору: команды трогают invites/devices.
    std::fs::set_permissions(
        &st.cfg.msgctl_sock,
        std::fs::Permissions::from_mode(0o600),
    )?;
    eprintln!("msgd: msgctl on {}", st.cfg.msgctl_sock.display());
    loop {
        let (mut sock, _) = listener.accept().await?;
        let st = st.clone();
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
                        if line.len() < MSGCTL_LINE_MAX {
                            line.push(buf[0]);
                        }
                    }
                    Err(_) => break,
                }
            }
            let reply = handle_msgctl(&st, &line);
            let _ = sock.write_all(reply.as_bytes()).await;
        });
    }
}

async fn run(cfg: Config) -> std::io::Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    std::fs::create_dir_all(&cfg.blobs_dir)?;
    // Одно соединение на процесс (WAL + busy_timeout против ложных BUSY).
    let conn = db::connect(cfg.data_dir.join("msgd.db")).map_err(std::io::Error::other)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(std::io::Error::other)?;
    let schema_version = db::migrate(&conn).map_err(std::io::Error::other)?;
    eprintln!("msgd: db schema version {schema_version}");
    let cfg = Config { schema_version, ..cfg };
    eprintln!("msgd: noise listening on {} for domain {}", cfg.listen, cfg.domain);
    eprintln!("msgd: noise key from {}", cfg.noise_key_file.display());
    let listener = TcpListener::bind(&cfg.listen).await?;
    let st = Arc::new(State {
        db: Arc::new(Mutex::new(conn)),
        live: Arc::new(Mutex::new(HashMap::new())),
        counters: Arc::new(Counters::default()),
        cfg: Arc::new(cfg),
    });
    let pre_auth = Arc::new(Semaphore::new(PRE_AUTH_CAP));
    let ctl = st.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_msgctl(ctl).await {
            eprintln!("msgd: msgctl error: {e}");
        }
    });
    // GC: orphan-sweep 24ч + TTL 7 сут + caps, раз в 5 минут под коротким локом.
    let gc_st = st.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            let now = now_secs();
            let r = {
                let mut db = gc_st.db.lock().expect("db");
                blob::gc(&mut db, now)
            };
            match r {
                Ok((b, e)) if b + e > 0 => eprintln!("msgd: gc swept {b} blobs, {e} events"),
                Ok(_) => {}
                Err(e) => eprintln!("msgd: gc error: {e:?}"),
            }
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;
        let st = st.clone();
        // Кап pre-auth: нет слота — сразу close, слот не удерживаем.
        let permit = match pre_auth.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                st.counters.pre_auth_full.fetch_add(1, Ordering::Relaxed);
                drop(stream);
                continue;
            }
        };
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                handle_conn(stream, st, permit),
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
            // Вся остальная строка — одна команда (invite-revoke <hex> и т.п.).
            let rest: Vec<String> = args.collect();
            let cmd = rest.join(" ");
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

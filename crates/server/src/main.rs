//! msgd: unified account auth and durable mailbox over Noise IK/slipstream TCP.
//! На каждый коннект — свой snow-responder → TransportState (1 TCP = 1 stream,
//! общий счётчик между коннектами запрещён — иначе nonce reuse).
//! Plaintext-HELLO удалён; домен сверяется первым transport-сообщением
//! OP_AUTH_DOMAIN внутри шифрованного канала. Неизвестный key после handshake
//! получает только POLICY/SIGNUP/LOGIN/RESUME. Pre-auth bounded: кап + timeout,
//! close без ban (per-IP ban на общем резолвере отключил бы всех, ARCH §4).
//! R1: deadline 10s — только handshake-фаза и pre-account чтения; живая сессия —
//! per-read idle 600s (счётчик idle_close). Двухкап pre 8 / post 20, cleanup
//! live/pre-карт через SessionGuard/Drop, pre-auth реестр device→Notify.

mod auth;
mod blob;
mod db;
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

use dmsg_protocol::{auth as ap, mailbox as mp, profile, *};

/// Кап незавершённых handshake (≪16 пилота, резерв до транспортных 32).
const PRE_AUTH_CAP: usize = 8;
/// Кап post-handshake сессий: permit держится до конца handle_conn.
const POST_AUTH_CAP: usize = 20;
/// Deadline handshake-фазы (IK + AUTH_DOMAIN/WELCOME) и pre-account чтений.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-read idle-timeout живой authenticated сессии (счётчик idle_close).
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Transport-буферы: MAX_FRAME plaintext + 16 Noise-tag + 2 length-prefix.
const HS_BUF_LEN: usize = MAX_FRAME + 18; // 16402
/// Bound шифртекста на wire: MAX_FRAME + 16 tag.
const CIPHER_BOUND: usize = MAX_FRAME + 16; // 16400

#[derive(Default)]
struct Counters {
    hs_ok: AtomicU64,
    hs_fail: AtomicU64,
    pre_auth_full: AtomicU64,
    post_auth_full: AtomicU64,
    auth_ok: AtomicU64,
    mismatch: AtomicU64,
    proto_err: AtomicU64,
    idle_close: AtomicU64,
    account_ok: AtomicU64,
    account_fail: AtomicU64,
    send_ok: AtomicU64,
    send_dedup: AtomicU64,
    send_fail: AtomicU64,
    fetch_ok: AtomicU64,
    ack_ok: AtomicU64,
    mbox_err: AtomicU64,
}

/// Разделяемое состояние: db под мьютексом (держать только на TX, никогда через .await).
struct State {
    auth: auth::Engine,
    cfg: Arc<Config>,
    counters: Arc<Counters>,
    db: Arc<Mutex<rusqlite::Connection>>,
    live: Arc<Mutex<HashMap<Vec<u8>, Vec<Arc<Notify>>>>>,
    /// Pre-auth реестр device_key→Notify: handshake пройден, account ещё нет.
    /// Только живые pending сессии, ограничены 10s чтением + Drop cleanup.
    /// Replacement/block будит обе карты.
    pre: Arc<Mutex<HashMap<Vec<u8>, Vec<Arc<Notify>>>>>,
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
    if !profile::valid_domain(domain.as_bytes()) {
        return Err("DMSG_DOMAIN invalid".to_string());
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
        data_dir: env::var("MSGD_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or("/var/lib/msgd".into()),
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

/// Прочитать length-prefixed сообщение (2 байта BE + тело), bound CIPHER_BOUND.
/// Формат handshake-сообщений Noise (не app-фреймы) и transport-шифртекста:
/// кадр до MAX_FRAME + 16 Noise-tag.
async fn read_hs_msg(stream: &mut tokio::net::TcpStream) -> Result<Vec<u8>, &'static str> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).await.map_err(|_| "eof")?;
    let len = u16::from_be_bytes(hdr) as usize;
    if len == 0 || len > CIPHER_BOUND {
        return Err("oversize");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|_| "eof")?;
    Ok(buf)
}

/// То же с deadline: handshake-фаза — HANDSHAKE_TIMEOUT, живая сессия — IDLE_TIMEOUT.
async fn read_hs_msg_timeout(
    stream: &mut tokio::net::TcpStream,
    d: Duration,
) -> Result<Vec<u8>, &'static str> {
    tokio::time::timeout(d, read_hs_msg(stream))
        .await
        .map_err(|_| "timeout")?
}

async fn write_hs_msg(stream: &mut tokio::net::TcpStream, msg: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(msg.len() as u16).to_be_bytes()).await?;
    stream.write_all(msg).await
}

/// Cleanup live/pre-карт через Drop: владеет notify и ключами.
/// No-op, пока сессия нигде не зарегистрирована (оба key None): коннект,
/// упавший до регистрации, ничего не трогает.
struct SessionGuard {
    live: Arc<Mutex<HashMap<Vec<u8>, Vec<Arc<Notify>>>>>,
    pre: Arc<Mutex<HashMap<Vec<u8>, Vec<Arc<Notify>>>>>,
    pre_key: Option<Vec<u8>>,
    live_key: Option<Vec<u8>>,
    notify: Arc<Notify>,
}

impl SessionGuard {
    fn unlist(
        map: &Arc<Mutex<HashMap<Vec<u8>, Vec<Arc<Notify>>>>>,
        key: &[u8],
        notify: &Arc<Notify>,
    ) {
        let mut m = map.lock().expect("session map");
        if let Some(v) = m.get_mut(key) {
            v.retain(|n| !Arc::ptr_eq(n, notify));
            if v.is_empty() {
                m.remove(key);
            }
        }
    }

    /// Authenticated: pre → live, idempotent (same-key retries add no entries).
    fn authenticated(&mut self, device_key: &[u8]) {
        if let Some(pk) = self.pre_key.take() {
            Self::unlist(&self.pre, &pk, &self.notify);
        }
        if self.live_key.as_deref() == Some(device_key) {
            return;
        }
        if let Some(prev) = self.live_key.take() {
            Self::unlist(&self.live, &prev, &self.notify);
        }
        self.live
            .lock()
            .expect("live")
            .entry(device_key.to_vec())
            .or_default()
            .push(self.notify.clone());
        self.live_key = Some(device_key.to_vec());
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(k) = self.live_key.take() {
            Self::unlist(&self.live, &k, &self.notify);
        }
        if let Some(k) = self.pre_key.take() {
            Self::unlist(&self.pre, &k, &self.notify);
        }
    }
}

/// Один коннект: Noise IK handshake → AUTH_DOMAIN → WELCOME → цикл сессии.
/// device_key — static инициатора из IK (get_remote_static), НЕ из тел сообщений.
/// Timeout 10s — handshake/auth reads; authenticated session idle 600s.
/// AUTHENTICATED registers live Notify; replacement/block wakes and closes it.
/// Cleanup обеих карт — SessionGuard/Drop, владеющий ключами и notify.
async fn handle_conn(
    mut stream: tokio::net::TcpStream,
    st: Arc<State>,
    pre_permit: tokio::sync::OwnedSemaphorePermit,
    post_auth: Arc<Semaphore>,
) {
    let cfg = &st.cfg;
    let c = &st.counters;
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or("?".into());
    let mut hsbuf = vec![0u8; HS_BUF_LEN];
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
    // IK: <- читаем msg1 (deadline 10s — часть handshake-фазы).
    let msg1 = match read_hs_msg_timeout(&mut stream, HANDSHAKE_TIMEOUT).await {
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
    c.hs_ok.fetch_add(1, Ordering::Relaxed);
    // Двухкап: post-auth слот на весь остаток коннекта (permit до конца функции).
    // Нет слота — сразу close + счётчик. Pre-слот освобождаем только здесь.
    let _post_permit = match post_auth.try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            c.post_auth_full.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: post-auth full, closing {peer}");
            return;
        }
    };
    drop(pre_permit);
    // Pre-auth реестр: handshake пройден, account ещё нет. Replacement/block по
    // привязанному устройству будит и такие сессии (wake_device смотрит обе карты).
    let notify = Arc::new(Notify::new());
    st.pre
        .lock()
        .expect("pre")
        .entry(device_key.to_vec())
        .or_default()
        .push(notify.clone());
    let mut guard = SessionGuard {
        live: st.live.clone(),
        pre: st.pre.clone(),
        pre_key: Some(device_key.to_vec()),
        live_key: None,
        notify: notify.clone(),
    };

    // Транспорт: читаем шифрованное → decrypt → app-фрейм (deadline 10s).
    let cipher = match tokio::select! {
        _ = notify.notified() => return,
        r = read_hs_msg_timeout(&mut stream, HANDSHAKE_TIMEOUT) => r,
    } {
        Ok(m) => m,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport read from {peer}: {e}");
            return;
        }
    };
    let mut plain = vec![0u8; HS_BUF_LEN];
    let n = match transport.read_message(&cipher, &mut plain) {
        Ok(n) => n,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport decrypt from {peer}: {e}");
            return;
        }
    };
    debug_assert!(n <= MAX_FRAME);
    if n > MAX_FRAME {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
        eprintln!("msgd: plaintext over bound from {peer}");
        return;
    }
    let mut cipher = cipher;
    cipher.fill(0);
    let (_, op, payload, total) = match decode_frame(&plain[..n]) {
        Ok(f) => f,
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            eprintln!("msgd: transport frame from {peer}: {e:?}");
            return;
        }
    };
    if total != n || op != OP_AUTH_DOMAIN || payload != cfg.domain.as_bytes() {
        c.mismatch.fetch_add(1, Ordering::Relaxed);
        eprintln!("msgd: domain mismatch from {peer} (close, no ban)");
        return;
    }
    let inner = encode_frame(OP_WELCOME, cfg.domain.as_bytes()).expect("domain fits");
    let mut out = vec![0u8; HS_BUF_LEN];
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

    // Before account auth: policy/auth only. Afterwards: mailbox and peer binding.
    // Unknown → close. Replacement/block wakes Notify → graceful close.
    // Pending reads 10s; authenticated per-read idle 600s.
    // DB-транзакции короткие, никогда через .await.
    let mut authenticated_user: Option<[u8; 16]> = None;
    let close = async {
        loop {
            let idle = if authenticated_user.is_some() {
                IDLE_TIMEOUT
            } else {
                HANDSHAKE_TIMEOUT
            };
            let cipher = match read_hs_msg_timeout(&mut stream, idle).await {
                Ok(m) => m,
                Err(_) => {
                    if authenticated_user.is_some() {
                        c.idle_close.fetch_add(1, Ordering::Relaxed);
                    } else {
                        c.proto_err.fetch_add(1, Ordering::Relaxed);
                    }
                    return Err(());
                }
            };
            let mut plain = vec![0u8; HS_BUF_LEN];
            let n = transport
                .read_message(&cipher, &mut plain)
                .map_err(|_| ())?;
            debug_assert!(n <= MAX_FRAME);
            if n > MAX_FRAME {
                c.proto_err.fetch_add(1, Ordering::Relaxed);
                return Err(());
            }
            let mut cipher = cipher;
            cipher.fill(0);
            let (_, op, payload, total) = decode_frame(&plain[..n]).map_err(|_| ())?;
            if total != n {
                return Err(());
            }
            let reply = if op == OP_POLICY && payload.is_empty() {
                let db = st.db.lock().expect("db");
                match auth::mode(&db) {
                    Ok(mode) => encode_frame(OP_POLICY_RESP, &mode.encode()).expect("fits"),
                    Err(code) => encode_frame(OP_ERROR, &[code]).expect("fits"),
                }
            } else if matches!(op, OP_SIGNUP | OP_LOGIN | OP_RESUME) {
                account_reply(
                    &st,
                    &device_key,
                    op,
                    payload,
                    &mut guard,
                    &mut authenticated_user,
                )
                .await?
            } else if authenticated_user.is_some() {
                let user = authenticated_user.expect("checked");
                match op {
                    OP_SEND => send_reply(&st, &c, &device_key, &payload)?,
                    OP_FETCH => fetch_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_DELIVERY_ACK => ack_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_UPLOAD_PREKEYS => upload_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_CLAIM => claim_reply(&st, &c, &device_key, &payload)?,
                    OP_COUNT => count_reply(&st, &c, &device_key, &payload)?,
                    OP_BLOB_RESERVE => reserve_reply(&st, &c, &user, &device_key, &payload)?,
                    OP_DEVICE_BINDING => binding_reply(&st, &device_key, payload)?,
                    _ => {
                        c.proto_err.fetch_add(1, Ordering::Relaxed);
                        return Err(());
                    }
                }
            } else {
                c.proto_err.fetch_add(1, Ordering::Relaxed);
                return Err(());
            };
            plain.fill(0);
            let mut out = vec![0u8; HS_BUF_LEN];
            let wn = transport.write_message(&reply, &mut out).map_err(|_| ())?;
            write_hs_msg(&mut stream, &out[..wn])
                .await
                .map_err(|_| ())?;
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
    // Cleanup — Drop guard (владеет live_key/pre_key/notify); _post_permit
    // жив до конца функции и освобождает post-слот только здесь.
}

/// Credentials are verified off-thread, before the short atomic DB operation.
async fn account_reply(
    st: &State,
    device_key: &[u8; 32],
    op: u8,
    payload: &[u8],
    guard: &mut SessionGuard,
    authenticated_user: &mut Option<[u8; 16]>,
) -> Result<Vec<u8>, ()> {
    let res = if op == OP_RESUME {
        if !payload.is_empty() {
            Err(ERR_INVALID_INPUT)
        } else {
            auth::resume(&st.db.lock().expect("db"), device_key)
        }
    } else {
        st.auth
            .account(&st.db, device_key, op, payload, now_secs())
            .await
    };
    match res {
        Ok(auth::Outcome::Authenticated {
            user,
            contact,
            revoked,
        }) => {
            if let Some(old) = revoked {
                wake_device(st, &old);
            }
            // Register under the DB lock: replacement/block cannot commit
            // between the final active check and insertion in the live map.
            let db = st.db.lock().expect("db");
            if is_revoked(&db, device_key) {
                return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
            }
            guard.authenticated(device_key);
            *authenticated_user = Some(user);
            st.counters.account_ok.fetch_add(1, Ordering::Relaxed);
            let p = ap::build_authenticated(&user, &contact).map_err(|_| ())?;
            Ok(encode_frame(OP_AUTHENTICATED, &p).expect("fits"))
        }
        Ok(auth::Outcome::ReplaceRequired(old)) => {
            Ok(encode_frame(OP_REPLACE_REQUIRED, &old).expect("fits"))
        }
        Err(code) => {
            st.counters.account_fail.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[code]).expect("fits"))
        }
    }
}

/// Свежая проверка revoked по device_key сессии. Вызывать, удерживая db-лок
/// от SELECT до конца TX действия: при одном соединении за Mutex это
/// сериализует re-check с msgctl_block/revoke — эквивалент same-TX проверки
/// (конкурентная TX между SELECT и действием невозможна).
/// Неизвестный key → fail-closed (считать отозванным).
fn is_revoked(db: &rusqlite::Connection, device_key: &[u8]) -> bool {
    db.query_row(
        "SELECT revoked<>0 OR blocked<>0 FROM devices WHERE device_key=?1",
        [device_key],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v != 0)
    .unwrap_or(true)
}

fn mbox_code(e: &mbox::MboxError) -> u8 {
    e.code()
}

/// SEND → re-check revoked → dedup-в-TX → quota → INSERT → commit → SEND_ACK.
/// Повтор возвращает прежний accept (send_dedup), не новое событие.
/// Отозванному — ERROR REVOKED без закрытия (закрытие придёт через Notify).
fn send_reply(
    st: &State,
    c: &Counters,
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    // Every accepted event must fit a FETCH response, whose sender user ID
    // makes it larger than SEND. Never commit an undeliverable head-of-line row.
    if payload.len().saturating_sub(32) > CIPHERTEXT_MAX {
        c.send_fail.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_BAD]).expect("fits"));
    }
    let s = mp::parse_send(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let now = now_secs();
    let mut db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.send_fail.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let res = mbox::send(
        &mut db,
        device_key,
        s.recipient,
        s.message_id,
        s.ciphertext,
        now,
    );
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
    let db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let rows = mbox::fetch(&db, user, device_key).map_err(|_| {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
    })?;
    // Пакуем, пока влезает в кадр (заголовок 4 + count 2 + записи).
    let mut p = vec![0u8, 0u8];
    let mut n: usize = 0;
    for e in &rows {
        let need = 8 + 32 + 16 + 16 + 2 + e.ciphertext.len();
        if 4 + p.len() + need > dmsg_protocol::MAX_FRAME || n >= dmsg_protocol::FETCH_BATCH_MAX {
            break;
        }
        p.extend_from_slice(&(e.seq as u64).to_be_bytes());
        p.extend_from_slice(&e.sender);
        p.extend_from_slice(&e.sender_user);
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
    let mut db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let cursor = mbox::ack(&mut db, user, device_key, &seqs).map_err(|_| {
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
    let (identity, curve, entries) = mp::parse_upload(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let entries: Vec<prekey::Entry> = entries
        .into_iter()
        .map(|(key_id, one_time, pubkey, sig)| prekey::Entry {
            key_id,
            one_time,
            pubkey,
            sig,
        })
        .collect();
    let mut db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let res = prekey::upload(&mut db, device_key, user, identity, curve, &entries);
    match res {
        Ok(left) => {
            c.ack_ok.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_COUNT_RESP, &(left as u32).to_be_bytes()).expect("fits"))
        }
        Err(prekey::PrekeyError::IdentityChanged) => {
            c.mbox_err.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[ERR_BAD]).expect("fits"))
        }
        Err(e) => {
            c.proto_err.fetch_add(1, Ordering::Relaxed);
            Ok(encode_frame(OP_ERROR, &[e.code()]).expect("fits"))
        }
    }
}

/// CLAIM → per-op re-check revoked вызывателя → PREKEY или ERROR no-prekey.
fn claim_reply(
    st: &State,
    c: &Counters,
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    let device = mp::parse_device(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let mut db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let got = prekey::claim(&mut db, device).map_err(|_| {
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

/// COUNT → per-op re-check revoked вызывателя → COUNT_RESP.
fn count_reply(
    st: &State,
    c: &Counters,
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    let device = mp::parse_device(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let n = prekey::count(&db, device).map_err(|_| {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
    })?;
    Ok(encode_frame(OP_COUNT_RESP, &(n as u32).to_be_bytes()).expect("fits"))
}

fn blob_code(e: &blob::BlobError) -> u8 {
    e.code()
}

/// BLOB_RESERVE → per-op re-check revoked вызывателя → BLOB_RESERVED или ERROR quota.
fn reserve_reply(
    st: &State,
    c: &Counters,
    user: &[u8; 16],
    device_key: &[u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, ()> {
    let (blob_id, size) = mp::parse_reserve(payload).ok_or_else(|| {
        c.proto_err.fetch_add(1, Ordering::Relaxed);
    })?;
    let now = now_secs();
    let mut db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        c.mbox_err.fetch_add(1, Ordering::Relaxed);
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    let res = blob::reserve(&mut db, user, blob_id, size as i64, now);
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

fn binding_reply(st: &State, device_key: &[u8; 32], payload: &[u8]) -> Result<Vec<u8>, ()> {
    if payload.len() != 16 {
        return Err(());
    }
    let db = st.db.lock().expect("db");
    if is_revoked(&db, device_key) {
        return Ok(encode_frame(OP_ERROR, &[ERR_REVOKED]).expect("fits"));
    }
    match prekey::binding(&db, payload) {
        Ok(Some((device, ed, curve))) => Ok(encode_frame(
            OP_DEVICE_BINDING_RESP,
            &ap::build_binding(payload.try_into().map_err(|_| ())?, &device, &ed, &curve),
        )
        .expect("fits")),
        Ok(None) => Ok(encode_frame(OP_ERROR, &[ERR_NO_PREKEY]).expect("fits")),
        Err(e) => Ok(encode_frame(OP_ERROR, &[e.code()]).expect("fits")),
    }
}

/// Разбудить живые сессии устройства (revoke/block) → graceful close.
/// Смотрит обе карты (live + pre-auth, включая pending account сессии).
/// Вызывается СТРОГО после commit: msgctl_block/revoke держат только короткие
/// локи, wake — после их снятия, иначе окно block→notify пропускает SEND.
fn wake_device(st: &State, device_key: &[u8]) {
    let mut targets = Vec::new();
    {
        let mut live = st.live.lock().expect("live");
        if let Some(v) = live.remove(device_key) {
            targets.extend(v);
        }
    }
    {
        let mut pre = st.pre.lock().expect("pre");
        if let Some(v) = pre.remove(device_key) {
            targets.extend(v);
        }
    }
    for n in targets {
        // Each session has exactly one consumer. Keep a permit if revocation
        // wins before its select/notified future is first polled.
        n.notify_one();
    }
}

/// Cap строки msgctl: overlong → `err line-too-long` из read-loop (ниже).
/// Молчаливой резки нет: резка рвала hex пополам и давала ложный err/revoke не того.
const MSGCTL_LINE_MAX: usize = 256;

/// Лишних аргументов быть не должно: true, если после разбора команды
/// в строке ничего не осталось.
fn msgctl_no_more(parts: &mut std::str::SplitWhitespace<'_>) -> bool {
    parts.next().is_none()
}

/// Одна команда msgctl. Токены/ключи из аргументов никогда не попадают в логи.
/// Строгость R3: лишние аргументы — `err` у всех команд (безопасно: сокет
/// локальный 0600, сетевых msgctl-клиентов нет — C4); строка длиннее
/// MSGCTL_LINE_MAX — `err line-too-long` (режет read-loop, не здесь).
fn handle_msgctl(st: &State, line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let mut parts = text.split_whitespace();
    match parts.next().unwrap_or("") {
        "ping" => {
            if msgctl_no_more(&mut parts) {
                "pong\n".into()
            } else {
                "err\n".into()
            }
        }
        "domain" => {
            if msgctl_no_more(&mut parts) {
                format!("{}\n", st.cfg.domain)
            } else {
                "err\n".into()
            }
        }
        "dbversion" => {
            if msgctl_no_more(&mut parts) {
                format!("{}\n", st.cfg.schema_version)
            } else {
                "err\n".into()
            }
        }
        "stats" => {
            if !msgctl_no_more(&mut parts) {
                return "err\n".into();
            }
            let c = &st.counters;
            format!(
                "hs_ok={} hs_fail={} pre_auth_full={} post_auth_full={} auth_ok={} mismatch={} proto_err={} idle_close={} account_ok={} account_fail={} send_ok={} send_dedup={} send_fail={} fetch_ok={} ack_ok={} mbox_err={}\n",
                c.hs_ok.load(Ordering::Relaxed),
                c.hs_fail.load(Ordering::Relaxed),
                c.pre_auth_full.load(Ordering::Relaxed),
                c.post_auth_full.load(Ordering::Relaxed),
                c.auth_ok.load(Ordering::Relaxed),
                c.mismatch.load(Ordering::Relaxed),
                c.proto_err.load(Ordering::Relaxed),
                c.idle_close.load(Ordering::Relaxed),
                c.account_ok.load(Ordering::Relaxed),
                c.account_fail.load(Ordering::Relaxed),
                c.send_ok.load(Ordering::Relaxed),
                c.send_dedup.load(Ordering::Relaxed),
                c.send_fail.load(Ordering::Relaxed),
                c.fetch_ok.load(Ordering::Relaxed),
                c.ack_ok.load(Ordering::Relaxed),
                c.mbox_err.load(Ordering::Relaxed),
            )
        }
        "invite-issue" => {
            // ttl опционален; мусор вместо числа — err (молчаливого дефолта нет).
            let ttl: i64 = match parts.next() {
                None => 24 * 3600,
                Some(v) => match v.parse() {
                    Ok(t) => t,
                    Err(_) => return "err\n".into(),
                },
            };
            if !msgctl_no_more(&mut parts) {
                return "err\n".into();
            }
            match msgctl_issue(st, ttl) {
                Ok(invitation) => format!("{invitation}\n"),
                Err(_) => "err\n".into(),
            }
        }
        "invite-revoke" => match parts.next().map(hex32) {
            Some(Some(tok)) if msgctl_no_more(&mut parts) => match msgctl_revoke(st, &tok) {
                Ok(()) => "ok\n".into(),
                Err(_) => "err\n".into(),
            },
            _ => "err\n".into(),
        },
        "server-code" if msgctl_no_more(&mut parts) => server_code(st)
            .map(|s| format!("{s}\n"))
            .unwrap_or_else(|_| "err\n".into()),
        "registration-mode" => {
            let mode = parts.next();
            if !msgctl_no_more(&mut parts) {
                return "err\n".into();
            }
            let db = st.db.lock().expect("db");
            if let Some(mode) = mode {
                if !matches!(mode, "open" | "invite_only") {
                    return "err\n".into();
                }
                if db
                    .execute(
                        "UPDATE meta SET value=?1 WHERE key='registration_mode'",
                        [mode],
                    )
                    .is_err()
                {
                    return "err\n".into();
                }
            }
            match auth::mode(&db) {
                Ok(ap::RegistrationMode::Open) => "open\n".into(),
                Ok(ap::RegistrationMode::InviteOnly) => "invite_only\n".into(),
                Err(_) => "err\n".into(),
            }
        }
        "invite-list" => {
            if msgctl_no_more(&mut parts) {
                msgctl_list(st)
            } else {
                "err\n".into()
            }
        }
        "device-block" => match parts.next().map(hex32) {
            Some(Some(key)) if msgctl_no_more(&mut parts) => match msgctl_block(st, &key) {
                Ok(()) => "ok\n".into(),
                Err(_) => "err\n".into(),
            },
            _ => "err\n".into(),
        },
        "device-unblock" => match parts.next().map(hex32) {
            Some(Some(key)) if msgctl_no_more(&mut parts) => match msgctl_unblock(st, &key) {
                Ok(()) => "ok\n".into(),
                Err(_) => "err\n".into(),
            },
            _ => "err\n".into(),
        },
        "user-list" => {
            if msgctl_no_more(&mut parts) {
                msgctl_users(st)
            } else {
                "err\n".into()
            }
        }
        "quotas" => match parts.next() {
            Some(f) => match valid_prefix(f) {
                Some(p) if msgctl_no_more(&mut parts) => msgctl_quotas(st, Some(&p)),
                _ => "err\n".into(),
            },
            None => msgctl_quotas(st, None),
        },
        "gc" => {
            if !msgctl_no_more(&mut parts) {
                return "err\n".into();
            }
            let now = now_secs();
            let t0 = std::time::Instant::now();
            let mut db = st.db.lock().expect("db");
            match blob::gc(&mut db, now) {
                Ok((b, e)) => {
                    eprintln!("msgd: gc lock_hold_ms={}", t0.elapsed().as_millis());
                    format!("gc blobs={b} events={e}\n")
                }
                Err(_) => "err\n".into(),
            }
        }
        "backup" => {
            if !msgctl_no_more(&mut parts) {
                return "err\n".into();
            }
            match msgctl_backup(st) {
                Ok(rep) => format!("{rep}\n"),
                Err(_) => "err\n".into(),
            }
        }
        _ => "err\n".into(),
    }
}

/// Backup-минимум: VACUUM INTO (атомарный снапшот без остановки записи) +
/// копия дерева blobs + integrity_check копии. Ротация «держать 3» — ДО записи
/// нового снапшота (старых остаётся ≤2, новый не становится 4-м и не упирается
/// в место рядом со старыми). Любая ошибка после mkdir чистит недоснапшот
/// (remove_dir_all), мусора snap-* не копится.
/// Секреты НЕ входят (статичны; оператор архивирует secrets/ отдельно, см. runbook).
/// Файловый кросс-чек blob_meta↔файлы станет осмысленным в M5, когда чанки
/// лягут на диск; сейчас отчёт содержит оба счётчика без гейта.
fn msgctl_backup(st: &State) -> Result<String, String> {
    prune_snaps(st, 2);
    let ts = now_secs();
    let snap = st.cfg.data_dir.join("backup").join(format!("snap-{ts}"));
    match backup_inner(st, &snap) {
        Ok(rep) => Ok(rep),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&snap);
            Err(e)
        }
    }
}

/// Ротация snap-*: оставить newest `keep`, старые тереть. Вызывается ДО записи
/// нового снапшота, поэтому keep=2 при политике «держать 3».
fn prune_snaps(st: &State, keep: usize) {
    if let Ok(rd) = std::fs::read_dir(st.cfg.data_dir.join("backup")) {
        let mut snaps: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("snap-"))
                        .unwrap_or(false)
            })
            .collect();
        snaps.sort();
        while snaps.len() > keep {
            let old = snaps.remove(0);
            let _ = std::fs::remove_dir_all(&old);
        }
    }
}

fn backup_inner(st: &State, snap: &std::path::Path) -> Result<String, String> {
    std::fs::create_dir_all(snap.join("blobs")).map_err(|e| format!("mkdir: {e}"))?;
    let db_path = snap.join("msgd.db");
    {
        let t0 = std::time::Instant::now();
        let db = st.db.lock().expect("db");
        let lit = db_path.to_string_lossy().replace('\'', "''");
        db.execute_batch(&format!("VACUUM INTO '{lit}'"))
            .map_err(|e| format!("vacuum: {e}"))?;
        eprintln!(
            "msgd: backup vacuum lock_hold_ms={}",
            t0.elapsed().as_millis()
        );
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
            let rel = p
                .strip_prefix(&st.cfg.blobs_dir)
                .map_err(|e| format!("rel: {e}"))?;
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
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut b = [0u8; 32];
    for (i, c) in b.iter_mut().enumerate() {
        *c = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(b)
}

/// Issue a one-time signup invitation. The private control response carries
/// its canonical base64url token to the CLI's 0600 output file, never to logs.
fn msgctl_issue(st: &State, ttl_secs: i64) -> Result<String, String> {
    if ttl_secs <= 0 {
        return Err("invalid ttl".into());
    }
    let mut token = [0u8; 32];
    getrandom::fill(&mut token).map_err(|e| format!("rng: {e}"))?;
    let now = now_secs();
    let expires = now.checked_add(ttl_secs).ok_or("invalid ttl")?;
    {
        let db = st.db.lock().expect("db");
        db.execute(
            "INSERT INTO invites(token, created_at, expires_at, revoked) VALUES(?1,?2,?3,0)",
            rusqlite::params![token.as_slice(), now, expires],
        )
        .map_err(|e| format!("insert: {e}"))?;
    }
    let invitation = ap::build_invitation(&token);
    eprintln!("msgd: invite issued");
    Ok(invitation)
}

fn server_code(st: &State) -> Result<String, String> {
    let cert_der =
        std::fs::read(&st.cfg.carrier_cert_file).map_err(|e| format!("carrier cert: {e}"))?;
    // PEM или DER: PEM начинается с -----BEGIN, DER — с 0x30.
    let cert_der = if cert_der.starts_with(b"-----BEGIN") {
        pem_to_der(&cert_der).ok_or("carrier cert: bad PEM")?
    } else {
        cert_der
    };
    let pubkey = noise::pubkey_of(&st.cfg.noise_private);
    profile::build(st.cfg.domain.as_bytes(), &cert_der, &pubkey)
        .map_err(|e| format!("profile: {e:?}"))
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

/// Revoke signup authorization only; an invitation has no device association.
fn msgctl_revoke(st: &State, token: &[u8; 32]) -> Result<(), String> {
    {
        let db = st.db.lock().expect("db");
        db.execute(
            "UPDATE invites SET revoked=1 WHERE token=?1",
            [token.as_slice()],
        )
        .map_err(|e| format!("revoke: {e}"))?;
    }
    eprintln!("msgd: invite revoked");
    Ok(())
}

/// Блокировка устройства: blocked=1 + закрыть живые и pending сессии.
/// Порядок: сначала commit UPDATE (автокоммит), wake_device — строго после.
fn msgctl_block(st: &State, device_key: &[u8; 32]) -> Result<(), String> {
    {
        let db = st.db.lock().expect("db");
        db.execute(
            "UPDATE devices SET blocked=1 WHERE device_key=?1",
            [device_key.as_slice()],
        )
        .map_err(|e| format!("block: {e}"))?;
    }
    wake_device(st, device_key);
    eprintln!("msgd: device blocked");
    Ok(())
}

/// Clear an operator block only. A replaced/retired key is never reactivated.
fn msgctl_unblock(st: &State, device_key: &[u8; 32]) -> Result<(), String> {
    {
        let db = st.db.lock().expect("db");
        use rusqlite::OptionalExtension;
        let revoked = db
            .query_row(
                "SELECT revoked FROM devices WHERE device_key=?1",
                [device_key.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|_| "lookup failed".to_string())?;
        if revoked.is_some_and(|v| v != 0) {
            return Err("retired key".into());
        }
        db.execute(
            "UPDATE devices SET blocked=0 WHERE device_key=?1 AND revoked=0",
            [device_key.as_slice()],
        )
        .map_err(|_| "unblock failed".to_string())?;
    }
    eprintln!("msgd: device unblocked");
    Ok(())
}

/// Список пользователей: префикс contact_id + устройства + флаг блокировки.
/// Полные ID/ключи — никогда (прецедент invite-list).
fn msgctl_users(st: &State) -> String {
    let db = st.db.lock().expect("db");
    let mut stmt = match db.prepare(
        "SELECT u.contact_id, COUNT(d.device_key), COALESCE(SUM(d.revoked<>0 OR d.blocked<>0),0)
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
    let mut out = format!("limits events={MAILBOX_EVENTS_MAX} bytes={MAILBOX_BYTES_MAX}\n");
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
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
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
        "SELECT token, created_at, expires_at, revoked, used_at FROM invites ORDER BY created_at,token",
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
            r.get::<_, Option<i64>>(4)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return "err\n".into(),
    };
    for row in rows.flatten() {
        let (tok, created, expires, revoked, used) = row;
        out.push_str(&format!(
            "{} created={} expires={} revoked={} used={}\n",
            hex_prefix(&tok),
            created,
            expires,
            revoked,
            if used.is_some() { "yes" } else { "no" }
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
    std::fs::set_permissions(&st.cfg.msgctl_sock, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("msgd: msgctl on {}", st.cfg.msgctl_sock.display());
    loop {
        let (mut sock, _) = listener.accept().await?;
        let st = st.clone();
        tokio::spawn(async move {
            let mut line = Vec::new();
            let mut too_long = false;
            let mut buf = [0u8; 1];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if buf[0] == b'\n' {
                            break;
                        }
                        // Переполнение флагается, остаток строки дочитывается
                        // без хранения: ответ — `err line-too-long`, не резка.
                        if too_long {
                            continue;
                        }
                        if line.len() < MSGCTL_LINE_MAX {
                            line.push(buf[0]);
                        } else {
                            too_long = true;
                        }
                    }
                    Err(_) => break,
                }
            }
            let reply = if too_long {
                "err line-too-long\n".to_string()
            } else {
                handle_msgctl(&st, &line)
            };
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
    let cfg = Config {
        schema_version,
        ..cfg
    };
    eprintln!(
        "msgd: noise listening on {} for domain {}",
        cfg.listen, cfg.domain
    );
    eprintln!("msgd: noise key from {}", cfg.noise_key_file.display());
    let listener = TcpListener::bind(&cfg.listen).await?;
    let st = Arc::new(State {
        auth: auth::Engine::new()
            .await
            .map_err(|_| std::io::Error::other("auth initialization"))?,
        db: Arc::new(Mutex::new(conn)),
        live: Arc::new(Mutex::new(HashMap::new())),
        pre: Arc::new(Mutex::new(HashMap::new())),
        counters: Arc::new(Counters::default()),
        cfg: Arc::new(cfg),
    });
    let pre_auth = Arc::new(Semaphore::new(PRE_AUTH_CAP));
    let post_auth = Arc::new(Semaphore::new(POST_AUTH_CAP));
    let ctl = st.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_msgctl(ctl).await {
            eprintln!("msgd: msgctl error: {e}");
        }
    });
    // GC только ручной (msgctl gc): объёмы пилота малые, таймер убран —
    // оператор гоняет по runbook. Замер удержания лока — в gc/backup путях.
    loop {
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;
        let st = st.clone();
        let post = post_auth.clone();
        // Кап pre-auth: нет слота — сразу close, слот не удерживаем.
        let permit = match pre_auth.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                st.counters.pre_auth_full.fetch_add(1, Ordering::Relaxed);
                drop(stream);
                continue;
            }
        };
        // Глобального timeout на коннект нет (сессии живут дольше 10s):
        // handshake-deadline 10s живёт внутри handle_conn (per-read),
        // authenticated сессия — per-read idle 600s.
        tokio::spawn(async move {
            handle_conn(stream, st, permit, post).await;
        });
    }
}

/// Одна команда на сервер по сокету 0600. Возвращает сырой ответ (печатает вызывающий).
async fn msgctl_client(sock: &str, cmd: &str) -> std::io::Result<String> {
    let mut stream = tokio::net::UnixStream::connect(sock).await?;
    stream.write_all(format!("{cmd}\n").as_bytes()).await?;
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Секретные команды — ТОЛЬКО через `--file <path>`: argv-варианты с hex
/// удалены (hex в argv светится в `ps`, подтверждено дважды — C1).
/// Иначе usage/exit 2.
fn secret_file_arg(sub: &str, rest: &[String]) -> Option<String> {
    if rest.len() == 3 && rest[1] == "--file" {
        return Some(rest[2].clone());
    }
    eprintln!("usage: msgd msgctl {sub} --file <path-600>");
    None
}

/// Secret input is a small regular, owner-only file. No values in errors.
fn read_secret_file(path: &str) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| "input-unavailable".to_string())?;
    let meta = file
        .metadata()
        .map_err(|_| "input-unavailable".to_string())?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 || meta.len() > 128 {
        return Err("invalid-input-file".into());
    }
    let mut s = String::new();
    file.take(129)
        .read_to_string(&mut s)
        .map_err(|_| "invalid-input-file".to_string())?;
    if s.len() > 128 {
        return Err("invalid-input-file".into());
    }
    Ok(s.trim().to_string())
}

/// `invite-issue --out-file <path> [ttl]`: mandatory file, canonical positive TTL.
fn issue_args(rest: &[String]) -> Option<(Option<String>, Option<String>)> {
    let usage = || {
        eprintln!("usage: msgd msgctl invite-issue --out-file <file> [ttl_secs]");
        None
    };
    let mut out_file = None;
    let mut ttl = None;
    let mut i = 1;
    while i < rest.len() {
        if rest[i] == "--out-file" {
            if out_file.is_some() {
                return usage();
            }
            i += 1;
            if i >= rest.len() || rest[i].starts_with("--") {
                return usage();
            }
            out_file = Some(rest[i].clone());
        } else if rest[i].starts_with("--") || ttl.is_some() {
            return usage();
        } else {
            let value = match rest[i].parse::<i64>() {
                Ok(v) if v > 0 => v,
                _ => return usage(),
            };
            ttl = Some(value.to_string());
        }
        i += 1;
    }
    if out_file.is_none() {
        return usage();
    }
    Some((out_file, ttl))
}

async fn issue_dispatch(sock: &str, rest: &[String]) -> ExitCode {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let (dest, ttl) = match issue_args(rest) {
        Some((Some(dest), ttl)) => (dest, ttl),
        _ => return ExitCode::from(2),
    };
    // Reserve the 0600 output before the destructive server operation. Existing
    // paths (including symlinks) or missing parents fail without a request.
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&dest)
    {
        Ok(f) => f,
        Err(_) => {
            eprintln!("invite-issue: output-unavailable");
            return ExitCode::from(1);
        }
    };
    let cmd = match ttl {
        Some(t) => format!("invite-issue {t}"),
        None => "invite-issue".into(),
    };
    let result = match msgctl_client(sock, &cmd).await {
        Ok(reply) => match ap::parse_invitation(reply.trim()) {
            Ok(invite) => {
                if file
                    .write_all(reply.as_bytes())
                    .and_then(|()| file.sync_all())
                    .is_ok()
                {
                    println!("ok");
                    return ExitCode::SUCCESS;
                }
                // Best-effort retire the unusable invitation after commit.
                let token_hex: String = invite.iter().map(|b| format!("{b:02x}")).collect();
                let _ = msgctl_client(sock, &format!("invite-revoke {token_hex}")).await;
                "output-failed"
            }
            Err(_) => "rejected",
        },
        Err(_) => "transport-failed",
    };
    drop(file);
    let _ = std::fs::remove_file(&dest);
    eprintln!("invite-issue: {result}");
    ExitCode::from(1)
}

/// msgctl-клиент: разбор секретных команд локально (файлы), остальное —
/// passthrough на сервер (ping/stats/user-list/quotas/invite-list/gc/backup
/// без hex — как были).
async fn msgctl_dispatch(sock: &str, rest: &[String]) -> ExitCode {
    match rest.first().map(|s| s.as_str()).unwrap_or("") {
        "invite-issue" => issue_dispatch(sock, rest).await,
        "invite-revoke" | "device-block" | "device-unblock" => {
            let sub = &rest[0];
            let path = match secret_file_arg(sub, rest) {
                Some(p) => p,
                None => return ExitCode::from(2),
            };
            let input = match read_secret_file(&path) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("{sub}: {e}");
                    return ExitCode::from(1);
                }
            };
            let key = if sub == "invite-revoke" {
                ap::parse_invitation(&input).ok()
            } else {
                hex32(&input)
            };
            let Some(key) = key else {
                eprintln!("usage: msgd msgctl {sub} --file <path-600>");
                return ExitCode::from(2);
            };
            let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
            match msgctl_client(sock, &format!("{sub} {hex}")).await {
                Ok(reply) => {
                    print!("{reply}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("msgctl: {e}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            let cmd = rest.join(" ");
            match msgctl_client(sock, &cmd).await {
                Ok(reply) => {
                    print!("{reply}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("msgctl: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("msgctl") => {
            let sock = env::var("MSGCTL_SOCK").unwrap_or("/var/lib/msgd/msgctl.sock".into());
            // Секретные подкоманды разбираются локально (--file/--out-file),
            // остальное — passthrough на сервер. Hex в argv запрещён (C1).
            let rest: Vec<String> = args.collect();
            return msgctl_dispatch(&sock, &rest).await;
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

#[cfg(test)]
mod revocation_session_tests {
    use super::*;

    #[tokio::test]
    async fn revoke_before_first_notify_poll_closes_live_and_pre_sessions() {
        let live_notify = Arc::new(Notify::new());
        let pre_notify = Arc::new(Notify::new());
        let key = vec![71u8; 32];
        let st = State {
            auth: auth::Engine::new().await.unwrap(),
            cfg: Arc::new(Config {
                domain: "replace.test".into(),
                listen: String::new(),
                data_dir: PathBuf::new(),
                blobs_dir: PathBuf::new(),
                msgctl_sock: PathBuf::new(),
                schema_version: db::SCHEMA_VERSION,
                noise_key_file: PathBuf::new(),
                noise_private: [0u8; 32],
                carrier_cert_file: PathBuf::new(),
            }),
            db: Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())),
            counters: Arc::new(Counters::default()),
            live: Arc::new(Mutex::new(HashMap::from([(
                key.clone(),
                vec![live_notify.clone()],
            )]))),
            pre: Arc::new(Mutex::new(HashMap::from([(
                key.clone(),
                vec![pre_notify.clone()],
            )]))),
        };
        // Session registration has happened, but its select has not polled
        // notified() yet: replacement must not lose the committed revocation signal.
        wake_device(&st, &key);
        for notify in [live_notify, pre_notify] {
            tokio::time::timeout(Duration::from_millis(100), notify.notified())
                .await
                .unwrap();
        }
        assert!(st.live.lock().unwrap().is_empty());
        assert!(st.pre.lock().unwrap().is_empty());
    }
}

//! K1 транспорт: trait-шов + direct-TCP + библиотечный инициатор.
//!
//! Инициатор (Noise IK + AUTH_DOMAIN→WELCOME) — функция [`initiate`], а не копия
//! diag-main: тот же handshake, что в `crates/server/examples/noise_diag.rs`,
//! но как переиспользуемый библиотечный вызов. Харнес и [`DirectTcp::connect`]
//! зовут его, а не дублируют `main` примеров.

use dmsg_protocol::{decode_frame, encode_frame, OP_AUTH_DOMAIN, OP_WELCOME};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

/// Noise-паттерн msgd v1. Должен совпадать с серверным `noise::PATTERN`,
/// иначе рассинхрон handshake (см. crates/server/src/noise.rs).
pub const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

/// Bound шифрованного чтения: кадр MAX_FRAME + 16 Noise-tag.
/// Зеркало серверного CIPHER_BOUND (main.rs): больше — close.
const CIPHER_BOUND: usize = dmsg_protocol::MAX_FRAME + 16;

/// Scratch-буфер Noise-сообщений (как в diag-примерах).
const HS_BUF_LEN: usize = 65535;

/// Ошибка транспорта. Ключевой материал сюда никогда не попадает.
#[derive(Debug, PartialEq, Eq)]
pub enum TransportError {
    /// Сетевая ошибка (текст io::Error, без секретов).
    Io(String),
    /// Handshake не сошёлся (чужой ключ сервера, мусор, обрыв).
    Handshake(String),
    /// AUTH_DOMAIN отвергнут: закрыто без WELCOME.
    Auth(String),
    /// Битый app-кадр внутри канала.
    Frame(String),
    /// Канал закрыт (не подключён или сервер закрыл).
    Closed,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Handshake(e) => write!(f, "handshake: {e}"),
            Self::Auth(e) => write!(f, "auth: {e}"),
            Self::Frame(e) => write!(f, "frame: {e:?}"),
            Self::Closed => write!(f, "closed"),
        }
    }
}

impl std::error::Error for TransportError {}

/// Шов транспорта K1: connect / send / recv / close.
/// Переподключением владеет Supervisor (см. `crate::supervisor`), этот trait —
/// только один установленный шифрованный канал.
///
/// `async fn` в трейте — осознанно: trait используется только в своём коде
/// (ядро + будущий FFI K4), Send-границы футур не требуются.
#[allow(async_fn_in_trait)]
pub trait Transport: Send {
    /// Установить канал: TCP + Noise IK + AUTH_DOMAIN→WELCOME.
    /// Повторный вызов на живом канале — Ok (no-op).
    async fn connect(&mut self) -> Result<(), TransportError>;
    /// Отправить один app-кадр (opcode + body) внутрь канала.
    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), TransportError>;
    /// Принять один app-кадр из канала.
    async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError>;
    /// Закрыть канал (идемпотентно).
    async fn close(&mut self);
    /// true, если канал установлен.
    fn is_connected(&self) -> bool;
}

/// Direct-TCP реализация шва: 1 TCP = 1 Noise-канал (как сервер: общий счётчик
/// между коннектами запрещён — иначе nonce reuse).
pub struct DirectTcp {
    addr: String,
    server_pub: [u8; 32],
    domain: Vec<u8>,
    stream: Option<tokio::net::TcpStream>,
    noise: Option<snow::TransportState>,
    tx_bytes: u64,
    rx_bytes: u64,
}

impl DirectTcp {
    /// Создать неподключённый канал. Проверки лёгкие (пусто/длина);
    /// подлинность сервера — только Noise handshake в [`initiate`].
    pub fn new(
        addr: String,
        server_pub: [u8; 32],
        domain: Vec<u8>,
    ) -> Result<Self, TransportError> {
        if addr.is_empty() {
            return Err(TransportError::Handshake("empty addr".into()));
        }
        if domain.is_empty() || domain.len() > dmsg_protocol::DOMAIN_MAX {
            return Err(TransportError::Handshake("bad domain".into()));
        }
        Ok(Self {
            addr,
            server_pub,
            domain,
            stream: None,
            noise: None,
            tx_bytes: 0,
            rx_bytes: 0,
        })
    }

    fn noise_mut(&mut self) -> Result<&mut snow::TransportState, TransportError> {
        self.noise.as_mut().ok_or(TransportError::Closed)
    }

    /// Transfer an authenticated lane to its single duplex owner.
    /// Production RPC/close behavior is unchanged; this is not a split Noise API.
    #[cfg(feature = "voice-probe")]
    pub(crate) fn into_lane_parts(
        self,
    ) -> Result<(tokio::net::TcpStream, snow::TransportState), TransportError> {
        let (stream, noise, _) = self.into_traced_lane_parts()?;
        Ok((stream, noise))
    }

    /// Absolute stream offsets include IK, domain auth, login and lane attach.
    pub(crate) fn into_traced_lane_parts(
        mut self,
    ) -> Result<(tokio::net::TcpStream, snow::TransportState, (u64, u64)), TransportError> {
        Ok((
            self.stream.take().ok_or(TransportError::Closed)?,
            self.noise.take().ok_or(TransportError::Closed)?,
            (self.tx_bytes, self.rx_bytes),
        ))
    }
}

impl Transport for DirectTcp {
    async fn connect(&mut self) -> Result<(), TransportError> {
        if self.is_connected() {
            return Ok(());
        }
        let ch = initiate(&self.addr, &self.server_pub, &self.domain).await?;
        self.stream = ch.stream;
        self.noise = ch.noise;
        self.tx_bytes = ch.tx_bytes;
        self.rx_bytes = ch.rx_bytes;
        Ok(())
    }

    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), TransportError> {
        let inner = Zeroizing::new(
            encode_frame(opcode, payload).map_err(|e| TransportError::Frame(format!("{e:?}")))?,
        );
        let mut buf = Zeroizing::new(vec![0u8; HS_BUF_LEN]);
        let n = self.noise_mut().and_then(|t| {
            t.write_message(&inner, &mut buf)
                .map_err(|e| TransportError::Io(format!("encrypt: {e}")))
        })?;
        wlen_counted(
            self.stream.as_mut().ok_or(TransportError::Closed)?,
            &buf[..n],
            &mut self.tx_bytes,
        )
        .await
    }

    async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
        let cipher = rlen_counted(
            self.stream.as_mut().ok_or(TransportError::Closed)?,
            &mut self.rx_bytes,
        )
        .await?;
        let mut buf = Zeroizing::new(vec![0u8; HS_BUF_LEN]);
        let n = self.noise_mut().and_then(|t| {
            t.read_message(&cipher, &mut buf)
                .map_err(|_| TransportError::Closed)
        })?;
        let (_, op, payload, consumed) =
            decode_frame(&buf[..n]).map_err(|e| TransportError::Frame(format!("{e:?}")))?;
        if consumed != n {
            return Err(TransportError::Frame("trailing application bytes".into()));
        }
        Ok((op, payload.to_vec()))
    }

    async fn close(&mut self) {
        if let Some(mut s) = self.stream.take() {
            let _ = s.shutdown().await;
        }
        self.noise.take();
    }

    fn is_connected(&self) -> bool {
        self.stream.is_some() && self.noise.is_some()
    }
}

/// Библиотечный инициатор: TCP + Noise IK + AUTH_DOMAIN→WELCOME.
///
/// Контракт: Ok — только если сервер внутри шифрованного канала вернул WELCOME
/// с тем же доменом; иначе Err и никакого канала. Свежая эфемерная пара на
/// каждый вызов (static-identity persist — K2, через `crate::store` +
/// [`initiate_with_key`]).
pub async fn initiate(
    addr: &str,
    server_pub: &[u8; 32],
    domain: &[u8],
) -> Result<DirectTcp, TransportError> {
    let params: snow::params::NoiseParams = PATTERN
        .parse()
        .map_err(|e| TransportError::Handshake(format!("pattern: {e}")))?;
    let kp = snow::Builder::new(params)
        .generate_keypair()
        .map_err(|e| TransportError::Handshake(format!("keygen: {e}")))?;
    let privk: [u8; 32] = kp
        .private
        .as_slice()
        .try_into()
        .map_err(|_| TransportError::Handshake("keygen len".into()))?;
    initiate_with_key(addr, server_pub, domain, &privk).await
}

/// Initiator with a persisted pending/authenticated Noise device private key.
///
/// device_key сервера (static-публичник инициатора из IK-сессии) выводится
/// сервером сам; клиент свой static знает — он и есть device_key, из тела
/// сообщений ключ никогда не берётся. Auth retries use this same pending key;
/// authenticated reconnects use empty RESUME, without a bearer or password.
pub async fn initiate_with_key(
    addr: &str,
    server_pub: &[u8; 32],
    domain: &[u8],
    device_priv: &[u8; 32],
) -> Result<DirectTcp, TransportError> {
    if addr.is_empty() || domain.is_empty() || domain.len() > dmsg_protocol::DOMAIN_MAX {
        return Err(TransportError::Handshake("bad args".into()));
    }
    let params: snow::params::NoiseParams = PATTERN
        .parse()
        .map_err(|e| TransportError::Handshake(format!("pattern: {e}")))?;
    let mut hs = snow::Builder::new(params)
        .local_private_key(device_priv)
        .map_err(|e| TransportError::Handshake(format!("local key: {e}")))?
        .remote_public_key(server_pub)
        .map_err(|e| TransportError::Handshake(format!("remote key: {e}")))?
        .build_initiator()
        .map_err(|e| TransportError::Handshake(format!("initiator: {e}")))?;
    let mut buf = Zeroizing::new(vec![0u8; HS_BUF_LEN]);
    let mut s = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| TransportError::Io(format!("connect: {e}")))?;
    let (mut tx_bytes, mut rx_bytes) = (0, 0);
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|_| TransportError::Closed)?;
    wlen_counted(&mut s, &buf[..n], &mut tx_bytes).await?;
    // До WELCOME живого канала нет: любой обрыв здесь — handshake (чужой pinned
    // key — сервер молча рвёт без msg2; переполнение pre-auth; сброс), а не
    // Transport живого канала.
    let m2 = rlen_counted(&mut s, &mut rx_bytes)
        .await
        .map_err(|e| TransportError::Handshake(format!("msg2: {e}")))?;
    hs.read_message(&m2, &mut buf)
        .map_err(|_| TransportError::Handshake("msg2".into()))?;
    let mut t = hs
        .into_transport_mode()
        .map_err(|_| TransportError::Handshake("transport".into()))?;
    let inner = Zeroizing::new(
        encode_frame(OP_AUTH_DOMAIN, domain)
            .map_err(|e| TransportError::Frame(format!("{e:?}")))?,
    );
    let n = t
        .write_message(&inner, &mut buf)
        .map_err(|_| TransportError::Closed)?;
    wlen_counted(&mut s, &buf[..n], &mut tx_bytes).await?;
    let c = rlen_counted(&mut s, &mut rx_bytes)
        .await
        .map_err(|_| TransportError::Auth("no welcome".into()))?;
    let n = t
        .read_message(&c, &mut buf)
        .map_err(|_| TransportError::Auth("no welcome".into()))?;
    let (_, op, payload, consumed) =
        decode_frame(&buf[..n]).map_err(|e| TransportError::Frame(format!("{e:?}")))?;
    if consumed != n || op != OP_WELCOME || payload != domain {
        return Err(TransportError::Auth("domain mismatch".into()));
    }
    Ok(DirectTcp {
        addr: addr.to_string(),
        server_pub: *server_pub,
        domain: domain.to_vec(),
        stream: Some(s),
        noise: Some(t),
        tx_bytes,
        rx_bytes,
    })
}

/// Записать length-prefixed сообщение (2 байта BE + тело).
#[cfg(test)]
async fn wlen(s: &mut tokio::net::TcpStream, msg: &[u8]) -> Result<(), TransportError> {
    wlen_counted(s, msg, &mut 0).await
}

async fn wlen_counted(
    s: &mut tokio::net::TcpStream,
    msg: &[u8],
    total: &mut u64,
) -> Result<(), TransportError> {
    let len16 =
        u16::try_from(msg.len()).map_err(|_| TransportError::Frame("msg too long".into()))?;
    for bytes in [&len16.to_be_bytes()[..], msg] {
        let mut written = 0;
        while written < bytes.len() {
            let n = s
                .write(&bytes[written..])
                .await
                .map_err(|e| TransportError::Io(format!("write: {e}")))?;
            if n == 0 {
                return Err(TransportError::Closed);
            }
            *total = total.saturating_add(n as u64);
            written += n;
        }
    }
    Ok(())
}

/// Прочитать length-prefixed сообщение, bound CIPHER_BOUND. Пустое и больше
/// bound — ошибка (сервер так же режет oversize закрытием).
#[cfg(test)]
async fn rlen(s: &mut tokio::net::TcpStream) -> Result<Vec<u8>, TransportError> {
    rlen_counted(s, &mut 0).await
}

async fn read_counted(
    s: &mut tokio::net::TcpStream,
    bytes: &mut [u8],
    total: &mut u64,
) -> Result<(), TransportError> {
    let mut filled = 0;
    while filled < bytes.len() {
        let n = s
            .read(&mut bytes[filled..])
            .await
            .map_err(|_| TransportError::Closed)?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        *total = total.saturating_add(n as u64);
        filled += n;
    }
    Ok(())
}

async fn rlen_counted(
    s: &mut tokio::net::TcpStream,
    total: &mut u64,
) -> Result<Vec<u8>, TransportError> {
    let mut hdr = [0u8; 2];
    read_counted(s, &mut hdr, total).await?;
    let len = usize::from(u16::from_be_bytes(hdr));
    if len == 0 || len > CIPHER_BOUND {
        return Err(TransportError::Frame("oversize".into()));
    }
    let mut buf = vec![0u8; len];
    read_counted(s, &mut buf, total).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn absolute_offsets_include_handshake_login_attach_and_handoff() {
        use dmsg_protocol::{OP_AUTHENTICATED, OP_CALL_ATTACH, OP_CALL_ATTACHED, OP_RESUME};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut hs = snow::Builder::new(PATTERN.parse().unwrap())
                .local_private_key(&[71; 32])
                .unwrap()
                .build_responder()
                .unwrap();
            let mut buffer = [0; 1024];
            let cipher = rlen(&mut socket).await.unwrap();
            let mut rx = cipher.len() as u64 + 2;
            hs.read_message(&cipher, &mut buffer).unwrap();
            let n = hs.write_message(&[], &mut buffer).unwrap();
            wlen(&mut socket, &buffer[..n]).await.unwrap();
            let mut tx = n as u64 + 2;
            let mut noise = hs.into_transport_mode().unwrap();
            for (request, response) in [
                (OP_AUTH_DOMAIN, OP_WELCOME),
                (OP_RESUME, OP_AUTHENTICATED),
                (OP_CALL_ATTACH, OP_CALL_ATTACHED),
            ] {
                let cipher = rlen(&mut socket).await.unwrap();
                rx += cipher.len() as u64 + 2;
                let n = noise.read_message(&cipher, &mut buffer).unwrap();
                let (_, opcode, payload, consumed) = decode_frame(&buffer[..n]).unwrap();
                assert_eq!((opcode, consumed), (request, n));
                let plain = encode_frame(response, payload).unwrap();
                let n = noise.write_message(&plain, &mut buffer).unwrap();
                wlen(&mut socket, &buffer[..n]).await.unwrap();
                tx += n as u64 + 2;
            }
            (rx, tx)
        });
        let mut client = initiate_with_key(
            &addr,
            &crate::olm::device_pubkey(&[71; 32]),
            b"fixture.test",
            &[72; 32],
        )
        .await
        .unwrap();
        client.send_frame(OP_RESUME, &[]).await.unwrap();
        assert_eq!(
            client.recv_frame().await.unwrap(),
            (OP_AUTHENTICATED, vec![])
        );
        client.send_frame(OP_CALL_ATTACH, &[3; 18]).await.unwrap();
        assert_eq!(
            client.recv_frame().await.unwrap(),
            (OP_CALL_ATTACHED, vec![3; 18])
        );
        let expected = server.await.unwrap();
        let (_, _, offsets) = client.into_traced_lane_parts().unwrap();
        assert_eq!(offsets, expected);
        assert!(
            offsets.0 > 2 * 22 + 18,
            "IK/domain bytes must not be omitted"
        );
    }

    #[tokio::test]
    async fn failed_partial_record_read_still_accounts_successful_bytes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        server.write_all(&[0, 8, 1, 2, 3]).await.unwrap();
        server.shutdown().await.unwrap();
        let mut total = 47;
        assert_eq!(
            rlen_counted(&mut client, &mut total).await,
            Err(TransportError::Closed)
        );
        assert_eq!(total, 52);
    }

    #[test]
    fn pattern_matches_server() {
        // Тот же паттерн, что серверный noise::PATTERN, иначе handshake не сойдётся.
        assert_eq!(PATTERN, "Noise_IK_25519_ChaChaPoly_BLAKE2s");
        let _: snow::params::NoiseParams = PATTERN.parse().expect("pattern parses");
    }

    #[test]
    fn new_rejects_bad_args() {
        assert!(DirectTcp::new(String::new(), [7u8; 32], b"d".to_vec()).is_err());
        assert!(DirectTcp::new("127.0.0.1:1".into(), [7u8; 32], vec![]).is_err());
        assert!(DirectTcp::new("127.0.0.1:1".into(), [7u8; 32], vec![b'x'; 300]).is_err());
        assert!(DirectTcp::new("127.0.0.1:1".into(), [7u8; 32], b"k1.test".to_vec()).is_ok());
    }

    #[tokio::test]
    async fn connect_refused_is_err() {
        // Заведомо закрытый порт: bind+drop, затем connect → refused.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut ch =
            DirectTcp::new(format!("127.0.0.1:{port}"), [7u8; 32], b"k1.test".to_vec()).unwrap();
        assert!(!ch.is_connected());
        assert!(ch.connect().await.is_err());
        assert!(!ch.is_connected());
    }

    #[tokio::test]
    async fn authenticated_message_with_trailing_frame_is_rejected() {
        let private = [71; 32];
        let mut responder = snow::Builder::new(PATTERN.parse().unwrap())
            .local_private_key(&private)
            .unwrap()
            .build_responder()
            .unwrap();
        let mut initiator = snow::Builder::new(PATTERN.parse().unwrap())
            .local_private_key(&[72; 32])
            .unwrap()
            .remote_public_key(&crate::olm::device_pubkey(&private))
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut buf = Zeroizing::new(vec![0; HS_BUF_LEN]);
        let mut scratch = Zeroizing::new(vec![0; HS_BUF_LEN]);
        let n = initiator.write_message(&[], &mut buf).unwrap();
        responder.read_message(&buf[..n], &mut scratch).unwrap();
        let n = responder.write_message(&[], &mut buf).unwrap();
        initiator.read_message(&buf[..n], &mut scratch).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let mut transport = DirectTcp {
            addr: "fixture".into(),
            server_pub: [0; 32],
            domain: b"fixture".to_vec(),
            stream: Some(client),
            noise: Some(initiator.into_transport_mode().unwrap()),
            tx_bytes: 0,
            rx_bytes: 0,
        };
        let mut plain =
            Zeroizing::new(encode_frame(dmsg_protocol::OP_INVITE_REVOKED, &[]).unwrap());
        plain.extend_from_slice(
            &encode_frame(dmsg_protocol::OP_ERROR, &[dmsg_protocol::ERR_BAD]).unwrap(),
        );
        let n = responder
            .into_transport_mode()
            .unwrap()
            .write_message(&plain, &mut buf)
            .unwrap();
        wlen(&mut server, &buf[..n]).await.unwrap();
        assert_eq!(
            transport.recv_frame().await,
            Err(TransportError::Frame("trailing application bytes".into()))
        );
        transport.close().await;
    }
}

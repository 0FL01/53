#![allow(dead_code)]
use dmsg_protocol::{auth, *};
use std::{
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
pub const DOMAIN: &str = "auth.test";
pub const PASSWORD: &str = "correct-fixture-password";
static SERIAL: AtomicU64 = AtomicU64::new(0);

pub struct Server {
    pub child: Child,
    pub port: u16,
    pub dir: PathBuf,
    pub server_pub: [u8; 32],
}
impl Server {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "msgd-{name}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = env!("CARGO_BIN_EXE_msgd");
        assert!(Command::new(bin)
            .args(["keygen", "--out"])
            .arg(dir.join("noise_key"))
            .output()
            .unwrap()
            .status
            .success());
        let out = Command::new(bin)
            .args(["pubkey", "--key"])
            .arg(dir.join("noise_key"))
            .output()
            .unwrap();
        let hex = String::from_utf8(out.stdout).unwrap();
        let mut server_pub = [0; 32];
        for (i, b) in server_pub.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        assert!(Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=x",
                "-keyout"
            ])
            .arg(dir.join("k.pem"))
            .arg("-out")
            .arg(dir.join("carrier.pem"))
            .output()
            .unwrap()
            .status
            .success());
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Self::spawn(&dir, port);
        let s = Self {
            child,
            port,
            dir,
            server_pub,
        };
        s.ready();
        s
    }
    fn spawn(dir: &Path, port: u16) -> Child {
        Command::new(env!("CARGO_BIN_EXE_msgd"))
            .env("DMSG_DOMAIN", DOMAIN)
            .env("MSGD_LISTEN", format!("127.0.0.1:{port}"))
            .env("MSGD_DATA_DIR", dir.join("data"))
            .env("MSGD_BLOBS_DIR", dir.join("blobs"))
            .env("MSGCTL_SOCK", dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", dir.join("noise_key"))
            .env("CARRIER_CERT_FILE", dir.join("carrier.pem"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }
    fn ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.cli(&["ping"]).stdout == b"pong\n" {
                return;
            }
            assert!(Instant::now() < deadline, "server readiness");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    pub fn restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        std::fs::remove_file(self.dir.join("ctl.sock")).ok();
        self.child = Self::spawn(&self.dir, self.port);
        self.ready();
    }
    pub fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_msgd"))
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .arg("msgctl")
            .args(args)
            .output()
            .unwrap()
    }
    pub fn ctl(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(out.status.success(), "CLI failed: {args:?}");
        String::from_utf8(out.stdout).unwrap()
    }
    pub fn raw(&self, command: &str) -> String {
        use std::io::{Read, Write};
        let mut s = UnixStream::connect(self.dir.join("ctl.sock")).unwrap();
        s.write_all(command.as_bytes()).unwrap();
        s.write_all(b"\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }
    pub fn issue(&self) -> ([u8; 32], PathBuf) {
        let p = self.dir.join(format!(
            "invite-{}.txt",
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        assert_eq!(
            self.ctl(&["invite-issue", "--out-file", p.to_str().unwrap(), "3600"]),
            "ok\n"
        );
        let token = auth::parse_invitation(std::fs::read_to_string(&p).unwrap().trim()).unwrap();
        (token, p)
    }
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("data/msgd.db")).unwrap()
    }
    pub fn file(&self, name: &str, content: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, content).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        p
    }
    pub fn public_file(&self, name: &str, key: &[u8; 32]) -> PathBuf {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        self.file(name, hex.as_bytes())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
pub fn public(private: [u8; 32]) -> [u8; 32] {
    *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private)).as_bytes()
}

pub struct Client {
    pub socket: TcpStream,
    transport: snow::TransportState,
    pub device: [u8; 32],
}
impl Client {
    pub async fn connect(srv: &Server, private: [u8; 32]) -> Self {
        let mut c = Self::connect_noise(srv, private).await;
        assert_eq!(
            c.exchange(OP_AUTH_DOMAIN, DOMAIN.as_bytes()).await.0,
            OP_WELCOME
        );
        c
    }
    pub async fn connect_noise(srv: &Server, private: [u8; 32]) -> Self {
        let mut hs = snow::Builder::new("Noise_IK_25519_ChaChaPoly_BLAKE2s".parse().unwrap())
            .local_private_key(&private)
            .unwrap()
            .remote_public_key(&srv.server_pub)
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut socket = TcpStream::connect(("127.0.0.1", srv.port)).await.unwrap();
        let mut buf = vec![0; 65535];
        let n = hs.write_message(&[], &mut buf).unwrap();
        write_record(&mut socket, &buf[..n]).await;
        let r = read_record(&mut socket).await.unwrap();
        hs.read_message(&r, &mut buf).unwrap();
        Self {
            socket,
            transport: hs.into_transport_mode().unwrap(),
            device: public(private),
        }
    }
    pub async fn send(&mut self, op: u8, payload: &[u8]) {
        let inner = encode_frame(op, payload).unwrap();
        let mut buf = vec![0; MAX_FRAME + 16];
        let n = self.transport.write_message(&inner, &mut buf).unwrap();
        write_record(&mut self.socket, &buf[..n]).await;
    }
    pub async fn receive(&mut self) -> Option<(u8, Vec<u8>)> {
        let r = tokio::time::timeout(Duration::from_secs(10), read_record(&mut self.socket))
            .await
            .ok()??;
        let mut buf = vec![0; MAX_FRAME];
        let n = self.transport.read_message(&r, &mut buf).ok()?;
        let (_, op, p, total) = decode_frame(&buf[..n]).ok()?;
        assert_eq!(n, total);
        Some((op, p.to_vec()))
    }
    pub async fn exchange(&mut self, op: u8, payload: &[u8]) -> (u8, Vec<u8>) {
        self.send(op, payload).await;
        self.receive().await.expect("response")
    }
    pub async fn signup(&mut self, login: &str, invite: Option<&[u8; 32]>) -> (u8, Vec<u8>) {
        self.exchange(
            OP_SIGNUP,
            &auth::build_signup(login, PASSWORD, invite).unwrap(),
        )
        .await
    }
    pub async fn login(&mut self, login: &str, replace: Option<&[u8; 32]>) -> (u8, Vec<u8>) {
        self.exchange(
            OP_LOGIN,
            &auth::build_login(login, PASSWORD, replace).unwrap(),
        )
        .await
    }
    pub async fn closed(&mut self) {
        let mut buf = [0; 8];
        let r = tokio::time::timeout(Duration::from_secs(3), self.socket.read(&mut buf)).await;
        assert!(
            matches!(r, Ok(Ok(0))) || matches!(r, Ok(Err(_))),
            "revoked session not closed: {r:?}"
        );
    }
}
async fn write_record(s: &mut TcpStream, p: &[u8]) {
    s.write_all(&(p.len() as u16).to_be_bytes()).await.unwrap();
    s.write_all(p).await.unwrap();
}
async fn read_record(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut h = [0; 2];
    s.read_exact(&mut h).await.ok()?;
    let n = u16::from_be_bytes(h) as usize;
    if n == 0 || n > MAX_FRAME + 16 {
        return None;
    }
    let mut p = vec![0; n];
    s.read_exact(&mut p).await.ok()?;
    Some(p)
}

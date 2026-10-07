#![allow(dead_code)]
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};
pub const DER: &[u8] = &[0x30, 0x03, 0x01, 0x01, 0];
pub const PASSWORD: &str = "test password only";
pub fn msgd_bin() -> PathBuf {
    let p = std::env::var_os("MSGD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/msgd")
        });
    assert!(p.exists(), "build msgd first: cargo build -p msgd");
    p
}
pub struct LiveMsgd {
    child: Child,
    pub dir: PathBuf,
    pub addr: String,
    pub code: String,
    pub server_pub: [u8; 32],
    pub domain: String,
}
impl LiveMsgd {
    pub fn start(tag: &str, domain: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dmsg-v2-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("noise-key");
        let output = Command::new(msgd_bin())
            .env_clear()
            .args(["keygen", "--out"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(output.status.success());
        let hex = String::from_utf8(output.stdout).unwrap();
        let hex = hex.trim();
        let mut server_pub = [0; 32];
        for (i, b) in server_pub.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        std::fs::write(dir.join("carrier.der"), DER).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let child = Command::new(msgd_bin())
            .env_clear()
            .env("DMSG_DOMAIN", domain)
            .env("MSGD_LISTEN", &addr)
            .env("MSGD_DATA_DIR", dir.join("data"))
            .env("MSGD_BLOBS_DIR", dir.join("blobs"))
            .env("MSGCTL_SOCK", dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", &key)
            .env("CARRIER_CERT_FILE", dir.join("carrier.der"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(&addr).is_err() || !dir.join("ctl.sock").exists() {
            assert!(Instant::now() < deadline, "msgd not ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        let code = dmsg_protocol::profile::build(domain.as_bytes(), DER, &server_pub).unwrap();
        Self {
            child,
            dir,
            addr,
            code,
            server_pub,
            domain: domain.into(),
        }
    }
    pub fn ctl(&self, args: &[&str]) -> String {
        let out = Command::new(msgd_bin())
            .env_clear()
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .arg("msgctl")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "msgctl command failed");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    pub fn issue(&self, name: &str, ttl: &str) -> String {
        let p = self.dir.join(name);
        let out = Command::new(msgd_bin())
            .env_clear()
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .args(["msgctl", "invite-issue", "--out-file"])
            .arg(&p)
            .arg(ttl)
            .output()
            .unwrap();
        assert!(out.status.success(), "invite issue failed");
        let invite = std::fs::read_to_string(p).unwrap().trim().to_owned();
        assert!(dmsg_protocol::auth::parse_invitation(&invite).is_ok());
        invite
    }
    pub fn revoke(&self, invitation: &str) {
        let p = self.dir.join("revoke.secret");
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&p)
            .unwrap();
        file.write_all(invitation.as_bytes()).unwrap();
        drop(file);
        let out = Command::new(msgd_bin())
            .env_clear()
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .args(["msgctl", "invite-revoke", "--file"])
            .arg(p)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    pub async fn signup(&self, db: &Path, login: &str) -> dmsg_core::Account {
        self.signup_keyed(db, login, None).await
    }
    pub async fn signup_keyed(
        &self,
        db: &Path,
        login: &str,
        key: Option<&[u8]>,
    ) -> dmsg_core::Account {
        let invite = self.issue(&format!("{login}.invite"), "3600");
        let account = dmsg_core::signup_direct(
            &self.code,
            &self.addr,
            db,
            Some(DER),
            key,
            login,
            PASSWORD,
            Some(&invite),
        )
        .await
        .unwrap();
        let conn = match key {
            Some(k) => dmsg_core::store::open_encrypted(db, k),
            None => dmsg_core::store::open(db),
        }
        .unwrap();
        assert_eq!(
            conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            9
        );
        account
    }
    pub async fn connect(&self, db: &Path) -> dmsg_core::DirectTcp {
        self.connect_keyed(db, None).await
    }
    pub async fn connect_keyed(&self, db: &Path, key: Option<&[u8]>) -> dmsg_core::DirectTcp {
        let c = match key {
            Some(k) => dmsg_core::store::open_encrypted(db, k),
            None => dmsg_core::store::open(db),
        }
        .unwrap();
        let key = dmsg_core::store::load_identity(&c).unwrap().unwrap();
        dmsg_core::initiate_with_key(&self.addr, &self.server_pub, self.domain.as_bytes(), &key)
            .await
            .unwrap()
    }

    pub fn restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.child = Command::new(msgd_bin())
            .env_clear()
            .env("DMSG_DOMAIN", &self.domain)
            .env("MSGD_LISTEN", &self.addr)
            .env("MSGD_DATA_DIR", self.dir.join("data"))
            .env("MSGD_BLOBS_DIR", self.dir.join("blobs"))
            .env("MSGCTL_SOCK", self.dir.join("ctl.sock"))
            .env("NOISE_KEY_FILE", self.dir.join("noise-key"))
            .env("CARRIER_CERT_FILE", self.dir.join("carrier.der"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(&self.addr).is_err() {
            assert!(Instant::now() < deadline, "msgd restart not ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for LiveMsgd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

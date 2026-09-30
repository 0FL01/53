//! K4 UniFFI-фасад: ТОЛЬКО команды/события, списки строго пагинацией.
//!
//! Границы (аудит K4, ARCH §5):
//! - через FFI ходят только команды (вызовы) и события/страницы (DTO ниже);
//!   PCM, сырые DB-курсоры и полные невыгружаемые списки запрещены;
//! - владелец DB — Rust: Kotlin передаёт только путь к app-private файлу,
//!   Connection открывается и закрывается внутри каждого вызова;
//! - ciphertext outbox через границу не отдаём (только message_id hex +
//!   статус) — ретраем владеет Rust;
//! - секреты (ключи, token, plaintext) в DTO и ошибки не попадают.
//!
//! Сетевые команды — синхронные блокирующие обёртки: внутри строится
//! short-lived tokio-runtime + fresh `Core::open` + `DirectTcp::connect`.
//! Долгоживущего Core через FFI нет (Connection не Sync) — это осознанно:
//! сессии/ratchet персистентны в DB, повторный open дёшев.

use std::path::Path;
use std::sync::Arc;

use crate::contacts::{self, Contact};
use crate::enrol::EnrolError;
use crate::olm::OlmError;
use crate::transport::TransportError;

enum ConnectFailure {
    Local(FfiError),
    Transport(TransportError),
}

impl ConnectFailure {
    fn into_ffi(self) -> FfiError {
        match self {
            Self::Local(e) => e,
            Self::Transport(e) => FfiError::Transport(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// DTO: команды возвращают записи, события — отчёты. Всё Clone для UniFFI.
// ---------------------------------------------------------------------------

/// Офлайн-предпросмотр invite (domain + pin-fingerprint, без сети).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Preview {
    pub domain: String,
    pub pin_fingerprint_hex: String,
}

/// Public metadata only: neither the bearer nor full profile leaves the store.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DnsProfileInfo {
    pub domain: String,
    pub noise_pubkey: Vec<u8>,
    pub pin_fingerprint_hex: String,
    pub resolvers: Vec<String>,
}

/// Тип QR: enrol-приглашение или контакт. Оба формата обязан понимать сканер.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum QrKind {
    Join,
    Contact,
}

/// Итог обработки contact-QR (pin не двигается молча — см. contacts.rs).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum QrOutcome {
    Added,
    Unchanged,
    IdentityChanged,
}

/// Карточка контакта для UI (без ключей — ключи через границу не ходят).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ContactInfo {
    pub contact_id: String,
    pub state: String,
    pub has_keys: bool,
    /// true — отправка СТОП до явного confirm (подмена).
    pub identity_mismatch: bool,
}

/// Строка списка диалогов/контактов (id + state, без ключей).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ContactRow {
    pub contact_id: String,
    pub state: String,
}

/// Страница контактов: cursor — contact_id последней строки (None = сначала).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ContactsPage {
    pub rows: Vec<ContactRow>,
    pub next_cursor: Option<String>,
}

/// Одна входящая строка (plaintext уже расшифрован ядром).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InboxRow {
    pub seq: i64,
    pub contact_id: String,
    pub text: String,
}

/// Страница входящих: cursor — seq последней строки (0 = сначала).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InboxPage {
    pub rows: Vec<InboxRow>,
    pub next_cursor: Option<i64>,
}

/// Одна outbox-строка БЕЗ ciphertext (им владеет Rust).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutboxRow {
    pub message_id_hex: String,
    pub contact_id: String,
    pub status: String,
}

/// Страница outbox (queued + accepted): cursor — внутренний rowid.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutboxPage {
    pub rows: Vec<OutboxRow>,
    pub next_cursor: Option<i64>,
}

/// Состояние учётки для UI.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccountInfo {
    pub enrolled: bool,
    pub contact_id: Option<String>,
}

/// Итог enrolment (user_id через границу не отдаём — он в DB).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct EnrolledInfo {
    pub contact_id: String,
}

/// Одно расшифрованное входящее (событие приёма).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReceivedMsg {
    pub contact_id: String,
    pub text: String,
    pub message_id_hex: String,
    pub seq: u64,
}

/// Событие приёма пачки: числа, а не строки (см. FetchResult).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FetchReport {
    pub received: Vec<ReceivedMsg>,
    pub skipped_unknown: u64,
    pub skipped_blocked: u64,
    pub skipped_undecryptable: u64,
    pub skipped_mismatch: u64,
    pub cursor: u64,
}

/// Событие ретрая outbox.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RetryReport {
    pub resent: u64,
    pub accepted: u64,
    pub delivered: u64,
    pub skipped: u64,
}

/// Решение хранилища при старте (миграция — явный шаг, не молча).
/// FreshInstall без переноса sealed-копии = новая identity (потеря старой
/// честно показана UI-строкой, см. android SecureStore).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum StoragePlan {
    FreshInstall,
    MigrateLegacy,
    ReadyWrapped,
}

/// Ошибка фасада. Строки — только статические причины/классы (секретов,
/// ключей, plaintext, token здесь нет по построению мапперов ниже).
#[derive(Debug, PartialEq, Eq, uniffi::Error)]
pub enum FfiError {
    BadArgs(String),
    BadQr(String),
    PinMismatch,
    NotEnrolled,
    UnknownContact,
    NotAccepted,
    Blocked,
    IdentityMismatch,
    NothingToConfirm,
    MissingKeys,
    NoPeerPrekeys,
    UploadRejected,
    Quota,
    Revoked,
    BadToken,
    Expired,
    BoundOther,
    Busy,
    BadText,
    Transport(String),
    Store(String),
    Crypto(String),
    Protocol(String),
    Server(String),
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadArgs(e) => write!(f, "bad args: {e}"),
            Self::BadQr(e) => write!(f, "bad qr: {e}"),
            Self::PinMismatch => write!(f, "transport pin mismatch"),
            Self::NotEnrolled => write!(f, "not enrolled"),
            Self::UnknownContact => write!(f, "unknown contact"),
            Self::NotAccepted => write!(f, "contact not accepted"),
            Self::Blocked => write!(f, "contact blocked"),
            Self::IdentityMismatch => {
                write!(f, "identity changed, sending stopped until confirm")
            }
            Self::NothingToConfirm => write!(f, "nothing to confirm"),
            Self::MissingKeys => write!(f, "contact has no keys (scan QR first)"),
            Self::NoPeerPrekeys => write!(f, "peer has no one-time keys"),
            Self::UploadRejected => write!(f, "server rejected prekey upload"),
            Self::Quota => write!(f, "server quota"),
            Self::Revoked => write!(f, "device revoked"),
            Self::BadToken => write!(f, "bad token"),
            Self::Expired => write!(f, "invite expired"),
            Self::BoundOther => write!(f, "token bound to other key"),
            Self::Busy => write!(f, "server busy, retry later"),
            Self::BadText => write!(f, "bad text (empty or over limit)"),
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::Store(e) => write!(f, "store: {e}"),
            Self::Crypto(e) => write!(f, "crypto: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Server(e) => write!(f, "server: {e}"),
        }
    }
}

impl std::error::Error for FfiError {}

fn map_olm(e: OlmError) -> FfiError {
    match e {
        OlmError::UnknownContact => FfiError::UnknownContact,
        OlmError::NotAccepted => FfiError::NotAccepted,
        OlmError::Blocked => FfiError::Blocked,
        OlmError::IdentityMismatch => FfiError::IdentityMismatch,
        OlmError::NothingToConfirm => FfiError::NothingToConfirm,
        OlmError::MissingKeys => FfiError::MissingKeys,
        OlmError::NotEnrolled => FfiError::NotEnrolled,
        OlmError::NoPeerPrekeys => FfiError::NoPeerPrekeys,
        OlmError::UploadRejected => FfiError::UploadRejected,
        OlmError::Quota => FfiError::Quota,
        OlmError::Revoked => FfiError::Revoked,
        OlmError::Bad => FfiError::BadToken,
        OlmError::Busy => FfiError::Busy,
        OlmError::Server(c) => FfiError::Server(format!("error {c}")),
        OlmError::Transport(e) => FfiError::Transport(e),
        OlmError::Store(e) => FfiError::Store(e),
        OlmError::Crypto(e) => FfiError::Crypto(e.into()),
        OlmError::Protocol(e) => FfiError::Protocol(e.into()),
        OlmError::WireType(t) => FfiError::Protocol(format!("wire type {t}")),
        OlmError::WireVersion(v) => FfiError::Protocol(format!("wire version {v}")),
        OlmError::BadText => FfiError::BadText,
        OlmError::Enrol(e) => map_enrol(e),
    }
}

fn map_enrol(e: EnrolError) -> FfiError {
    match e {
        EnrolError::BadQr(s) => FfiError::BadQr(s.into()),
        EnrolError::PinMismatch => FfiError::PinMismatch,
        EnrolError::KeyMismatch(d) => FfiError::Transport(format!("noise key: {d}")),
        EnrolError::Auth(d) => FfiError::Transport(format!("auth: {d}")),
        EnrolError::Bad => FfiError::BadToken,
        EnrolError::Expired => FfiError::Expired,
        EnrolError::Revoked => FfiError::Revoked,
        EnrolError::BoundOther => FfiError::BoundOther,
        EnrolError::Busy => FfiError::Busy,
        EnrolError::Server(c) => FfiError::Server(format!("error {c}")),
        EnrolError::Protocol(s) => FfiError::Protocol(s.into()),
        EnrolError::Transport(s) => FfiError::Transport(s),
        EnrolError::Store(s) => FfiError::Store(s),
    }
}

// ---------------------------------------------------------------------------
// Чистые функции (без DB/сети): классификация QR, лимиты, решение хранилища.
// ---------------------------------------------------------------------------

/// Кап длины URI ДО разбора: явно больше любого валидного QR обоих форматов
/// (join ≤ ~5.5 KiB b64, contact ~170 символов). Сверх — явная ошибка
/// «oversized», а не молчаливый Truncated парсера.
pub const QR_URI_MAX: usize = 8192;

/// Классифицировать сканированный QR (оба формата). Битый/oversized —
/// явная ошибка со статической причиной (содержимое не возвращается).
#[uniffi::export]
pub fn qr_kind(uri: String) -> Result<QrKind, FfiError> {
    if uri.len() > QR_URI_MAX {
        return Err(FfiError::BadQr("oversized".into()));
    }
    if uri.starts_with(dmsg_protocol::bootstrap::URI_PREFIX) {
        return dmsg_protocol::bootstrap::parse(&uri)
            .map(|_| QrKind::Join)
            .map_err(|e| {
                FfiError::BadQr(
                    match e {
                        dmsg_protocol::bootstrap::BootstrapError::BadPrefix => "bad prefix",
                        dmsg_protocol::bootstrap::BootstrapError::BadEncoding => {
                            "bad encoding"
                        }
                        dmsg_protocol::bootstrap::BootstrapError::Truncated => "truncated",
                        dmsg_protocol::bootstrap::BootstrapError::BadVersion(_) => {
                            "bad version"
                        }
                        dmsg_protocol::bootstrap::BootstrapError::BadDomain => "bad domain",
                        dmsg_protocol::bootstrap::BootstrapError::BadCert => "bad cert",
                    }
                    .into(),
                )
            });
    }
    if uri.starts_with(contacts::CONTACT_PREFIX) {
        return contacts::parse_qr(&uri).map(|_| QrKind::Contact).map_err(|e| {
            FfiError::BadQr(
                match e {
                    contacts::QrError::BadPrefix => "bad prefix",
                    contacts::QrError::BadEncoding => "bad encoding",
                    contacts::QrError::Truncated => "truncated",
                    contacts::QrError::BadVersion(_) => "bad version",
                    contacts::QrError::BadContact => "bad contact id",
                }
                .into(),
            )
        });
    }
    Err(FfiError::BadQr("bad prefix".into()))
}

/// Кламп лимита страниц (контракт: 1..=100). Единая точка для Kotlin-зеркала.
#[uniffi::export]
pub fn page_limit(limit: u32) -> u32 {
    limit.clamp(1, 100)
}

/// Решение хранилища при старте по наличию файлов (чистая функция —
/// покрытие unit-тестом здесь, зеркало в Kotlin вызывает фасад).
#[uniffi::export]
pub fn storage_plan(has_legacy_db: bool, has_wrapped_db: bool) -> StoragePlan {
    if has_wrapped_db {
        StoragePlan::ReadyWrapped
    } else if has_legacy_db {
        StoragePlan::MigrateLegacy
    } else {
        StoragePlan::FreshInstall
    }
}

// ---------------------------------------------------------------------------
// Клиент: владеет только путём DB. Каждный вызов открывает store сам.
// ---------------------------------------------------------------------------

/// Фасад ядра для Kotlin. Send+Sync по построению (только путь-строка).
#[derive(uniffi::Object)]
pub struct DmsgClient {
    db_path: String,
    key: Option<[u8; 32]>,
}

fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

fn info_of(c: &Contact) -> ContactInfo {
    ContactInfo {
        contact_id: c.contact_id.clone(),
        state: c.state.clone(),
        has_keys: c.ed_identity.is_some() && c.curve_identity.is_some(),
        identity_mismatch: c.seen_ed.is_some() || c.seen_curve.is_some(),
    }
}

fn dns_failure(state: crate::dns::State) -> FfiError {
    if state == crate::dns::State::Failed(7) { return FfiError::PinMismatch; }
    FfiError::Transport(match state {
        crate::dns::State::Connecting => "DNS connect deadline".into(),
        crate::dns::State::Backoff(_) => "DNS unavailable; reconnect backoff".into(),
        crate::dns::State::Stopped => "DNS stopped".into(),
        crate::dns::State::Failed(c) => format!("DNS native failure {c}"),
        crate::dns::State::Ready(_) => "DNS stream unavailable".into(),
    })
}

fn parse_transport_args(
    server_pub: Vec<u8>,
    domain: String,
) -> Result<([u8; 32], Vec<u8>), FfiError> {
    let sp: [u8; 32] = server_pub
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::BadArgs("server_pub must be 32 bytes".into()))?;
    if domain.is_empty() || domain.len() > dmsg_protocol::DOMAIN_MAX {
        return Err(FfiError::BadArgs("bad domain".into()));
    }
    Ok((sp, domain.into_bytes()))
}

impl DmsgClient {
    fn dns_profile(&self) -> Result<crate::dns::Profile, FfiError> {
        crate::dns::load(&self.conn()?).map_err(FfiError::Store)?
            .ok_or_else(|| FfiError::BadArgs("DNS profile is not configured".into()))
    }

    fn dns_endpoint(&self, profile: &crate::dns::Profile) -> Result<String, FfiError> {
        crate::dns::endpoint(&self.db_path, profile).map(|a| a.to_string())
            .map_err(dns_failure)
    }

    fn conn(&self) -> Result<rusqlite::Connection, FfiError> {
        match &self.key {
            Some(k) => crate::store::open_encrypted(Path::new(&self.db_path), k),
            None => crate::store::open(Path::new(&self.db_path)),
        }.map_err(FfiError::Store)
    }

    fn core(&self) -> Result<crate::chat::Core, FfiError> {
        match &self.key {
            Some(k) => crate::chat::Core::open_encrypted(Path::new(&self.db_path), k),
            None => crate::chat::Core::open(Path::new(&self.db_path)),
        }.map_err(map_olm)
    }

    /// Enrolled commands must use the same Noise static as enrolment. The
    /// ephemeral K1 constructor authenticates as a different device each time.
    async fn connect(
        &self,
        addr: &str,
        server_pub: &[u8; 32],
        domain: &[u8],
    ) -> Result<crate::transport::DirectTcp, ConnectFailure> {
        let device_priv = crate::store::load_identity(&self.conn().map_err(ConnectFailure::Local)?)
            .map_err(|e| ConnectFailure::Local(FfiError::Store(e)))?
            .ok_or(ConnectFailure::Local(FfiError::NotEnrolled))?;
        crate::transport::initiate_with_key(addr, server_pub, domain, &device_priv)
            .await
            .map_err(ConnectFailure::Transport)
    }
}

#[uniffi::export]
impl DmsgClient {
    /// Offline import after QR preview confirmation. Saved pins are immutable.
    pub fn configure_dns(&self, qr: String, resolvers: Vec<String>) -> Result<(), FfiError> {
        let profile = crate::dns::Profile::from_qr(&qr, resolvers)
            .map_err(|e| FfiError::BadQr(e.into()))?;
        let mut conn = self.conn()?;
        // Serialize the first import's compare-and-save across all FFI callers,
        // not only Android's facade lock. Two different pins must not both win.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| FfiError::Store("dns import transaction".into()))?;
        if let Some(old) = crate::dns::load(&tx).map_err(FfiError::Store)? {
            if old.domain != profile.domain || old.certificate != profile.certificate
                || old.noise_pubkey != profile.noise_pubkey {
                return Err(FfiError::BadArgs("saved DNS identity/pin cannot be replaced".into()));
            }
        }
        crate::dns::save(&tx, &profile).map_err(FfiError::Store)?;
        tx.commit().map_err(|_| FfiError::Store("dns import commit".into()))
    }

    pub fn dns_profile_info(&self) -> Result<Option<DnsProfileInfo>, FfiError> {
        use sha2::Digest;
        Ok(crate::dns::load(&self.conn()?).map_err(FfiError::Store)?.map(|p| DnsProfileInfo {
            domain: p.domain,
            noise_pubkey: p.noise_pubkey.to_vec(),
            pin_fingerprint_hex: hex(&sha2::Sha256::digest(&p.certificate)),
            resolvers: p.resolvers.into_iter().map(|a| a.to_string()).collect(),
        }))
    }

    /// Network transitions invalidate sockets even if resolver IPs stayed equal.
    pub fn dns_network_changed(&self, resolvers: Vec<String>) -> Result<(), FfiError> {
        let mut profile = self.dns_profile()?;
        profile.resolvers = crate::dns::parse_resolvers(resolvers)
            .map_err(|e| FfiError::BadArgs(e.into()))?;
        crate::dns::save(&self.conn()?, &profile).map_err(FfiError::Store)?;
        self.stop_dns()
    }

    /// No SQLite access: cancellation can race a blocked connect/fetch.
    pub fn stop_dns(&self) -> Result<(), FfiError> {
        crate::dns::stop(&self.db_path).map_err(|e| FfiError::Transport(e.into()))
    }

    pub fn dns_status(&self) -> Result<String, FfiError> {
        crate::dns::status(&self.db_path).map(|s| match s {
            crate::dns::State::Connecting => "connecting".into(),
            crate::dns::State::Ready(_) => "ready".into(),
            crate::dns::State::Backoff(seconds) => format!("backoff {seconds}s"),
            crate::dns::State::Failed(7) => "carrier pin mismatch".into(),
            crate::dns::State::Failed(code) => format!("failed {code}"),
            crate::dns::State::Stopped => "stopped".into(),
        }).map_err(|e| FfiError::Transport(e.into()))
    }

    pub fn enrol_dns(&self, qr: String, resolvers: Vec<String>) -> Result<EnrolledInfo, FfiError> {
        self.configure_dns(qr.clone(), resolvers)?;
        let profile = self.dns_profile()?;
        let addr = self.dns_endpoint(&profile)?;
        self.enrol_from_qr(qr, addr, Some(profile.certificate))
    }

    pub fn reconnect_dns(&self) -> Result<u32, FfiError> {
        let p = self.dns_profile()?;
        self.reconnect(self.dns_endpoint(&p)?, p.noise_pubkey.to_vec(), p.domain)
    }
    pub fn fetch_dns(&self) -> Result<FetchReport, FfiError> {
        let p = self.dns_profile()?;
        self.fetch(self.dns_endpoint(&p)?, p.noise_pubkey.to_vec(), p.domain)
    }
    pub fn retry_dns(&self) -> Result<RetryReport, FfiError> {
        let p = self.dns_profile()?;
        self.retry_queued(self.dns_endpoint(&p)?, p.noise_pubkey.to_vec(), p.domain)
    }
    pub fn send_dns(&self, contact_id: String, text: String) -> Result<String, FfiError> {
        if text.is_empty() || text.len() > dmsg_protocol::TEXT_MAX { return Err(FfiError::BadText); }
        let p = self.dns_profile()?;
        let mut core = self.core()?;
        core.preflight_text(&contact_id, &text).map_err(map_olm)?;
        match crate::dns::endpoint(&self.db_path, &p) {
            Ok(a) => self.send_text(a.to_string(), p.noise_pubkey.to_vec(), p.domain, contact_id, text),
            Err(s @ (crate::dns::State::Connecting | crate::dns::State::Backoff(_) | crate::dns::State::Stopped)) => {
                core.queue_text_existing_session(&contact_id, &text).map_err(map_olm)?
                    .map(|mid| hex(&mid)).ok_or_else(|| dns_failure(s))
            }
            Err(s) => Err(dns_failure(s)),
        }
    }

    /// Открыть фасад над app-private файлом DB (файл создаётся лениво store).
    #[uniffi::constructor]
    pub fn open(db_path: String) -> Arc<Self> {
        Arc::new(Self { db_path, key: None })
    }

    /// Android passes a random 32-byte key unwrapped by Keystore. Existing
    /// encrypted DBs are verified immediately; no wrong-key fresh install.
    #[uniffi::constructor]
    pub fn open_encrypted(db_path: String, key: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let key: [u8; 32] = key.try_into()
            .map_err(|_| FfiError::BadArgs("storage key must be 32 bytes".into()))?;
        crate::store::open_encrypted(Path::new(&db_path), &key).map_err(FfiError::Store)?;
        Ok(Arc::new(Self { db_path, key: Some(key) }))
    }

    /// Учётка: enrolled + свой contact_id (None — свежая установка).
    pub fn account_info(&self) -> Result<AccountInfo, FfiError> {
        let conn = self.conn()?;
        match crate::store::load_account(&conn).map_err(FfiError::Store)? {
            Some((_, cid)) => Ok(AccountInfo { enrolled: true, contact_id: Some(cid) }),
            None => Ok(AccountInfo { enrolled: false, contact_id: None }),
        }
    }

    /// Офлайн-предпросмотр invite: domain + pin-fingerprint, без сети.
    pub fn enrol_preview(&self, qr: String) -> Result<Preview, FfiError> {
        let p = crate::enrol::preview(&qr).map_err(map_enrol)?;
        let hex_fp = p.pin_fingerprint_hex();
        Ok(Preview { domain: p.domain, pin_fingerprint_hex: hex_fp })
    }

    /// Полный enrol из QR (сеть). pin_der — ожидаемый DER для сверки
    /// (None — доверие сканированному QR как корню, см. enrol.rs).
    pub fn enrol_from_qr(
        &self,
        qr: String,
        addr: String,
        expected_pin_der: Option<Vec<u8>>,
    ) -> Result<EnrolledInfo, FfiError> {
        if addr.is_empty() {
            return Err(FfiError::BadArgs("empty addr".into()));
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;
        let db = Path::new(&self.db_path).to_path_buf();
        let enrol = async {
            match &self.key {
                Some(k) => crate::enrol::enrol_from_qr_encrypted(&qr, &addr, &db, expected_pin_der.as_deref(), k).await,
                None => crate::enrol::enrol_from_qr(&qr, &addr, &db, expected_pin_der.as_deref()).await,
            }
        };
        rt.block_on(enrol)
            .map(|e| EnrolledInfo { contact_id: e.contact_id })
            .map_err(map_enrol)
    }

    /// Свой contact-QR для показа (требует enrol).
    pub fn my_contact_qr(&self) -> Result<String, FfiError> {
        let core = self.core()?;
        let (uid, cid) = core.my_account().map_err(map_olm)?;
        let (ed, curve) = core.identity_keys();
        let dev = core.device_pub();
        contacts::build_qr(&cid, &uid, &dev, &ed, &curve)
            .map_err(|_| FfiError::Store("qr build".into()))
    }

    /// Обработать сканированный contact-QR (pin/seen по правилам contacts).
    pub fn add_contact_qr(&self, uri: String) -> Result<QrOutcome, FfiError> {
        if uri.len() > QR_URI_MAX {
            return Err(FfiError::BadQr("oversized".into()));
        }
        let conn = self.conn()?;
        match contacts::add_from_qr(&conn, &uri).map_err(map_olm)? {
            contacts::QrResult::Added => Ok(QrOutcome::Added),
            contacts::QrResult::Unchanged => Ok(QrOutcome::Unchanged),
            contacts::QrResult::IdentityChanged => Ok(QrOutcome::IdentityChanged),
        }
    }

    /// Запрос на добавление по ID (без ключей). Возвращает state.
    pub fn contact_request(&self, contact_id: String) -> Result<String, FfiError> {
        let conn = self.conn()?;
        contacts::request_add(&conn, &contact_id).map_err(map_olm)
    }

    /// Согласие на контакт.
    pub fn contact_accept(&self, contact_id: String) -> Result<(), FfiError> {
        let conn = self.conn()?;
        contacts::accept(&conn, &contact_id).map_err(map_olm)
    }

    /// Блок (терминален в v1).
    pub fn contact_block(&self, contact_id: String) -> Result<(), FfiError> {
        let conn = self.conn()?;
        contacts::block(&conn, &contact_id).map_err(map_olm)
    }

    /// Явное подтверждение подмены (seen → pin).
    pub fn contact_confirm(&self, contact_id: String) -> Result<(), FfiError> {
        let conn = self.conn()?;
        // Core не нужен: confirm — store-операция + удаление сессии.
        contacts::confirm_identity(&conn, &contact_id).map_err(map_olm)
    }

    /// Карточка контакта (ключи не возвращаются).
    pub fn contact_get(&self, contact_id: String) -> Result<ContactInfo, FfiError> {
        let conn = self.conn()?;
        contacts::get(&conn, &contact_id)
            .map_err(map_olm)?
            .map(|c| info_of(&c))
            .ok_or(FfiError::UnknownContact)
    }

    /// Страница контактов (cursor — contact_id, None = сначала).
    pub fn contacts_page(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ContactsPage, FfiError> {
        let conn = self.conn()?;
        let (rows, next) =
            contacts::list(&conn, cursor.as_deref(), page_limit(limit) as usize)
                .map_err(map_olm)?;
        Ok(ContactsPage {
            rows: rows
                .into_iter()
                .map(|(id, st)| ContactRow { contact_id: id, state: st })
                .collect(),
            next_cursor: next,
        })
    }

    /// Страница входящих (cursor — seq, 0 = сначала).
    pub fn inbox_page(&self, cursor: i64, limit: u32) -> Result<InboxPage, FfiError> {
        let conn = self.conn()?;
        let (rows, next) = crate::store::inbox_list(&conn, cursor, page_limit(limit) as usize)
            .map_err(FfiError::Store)?;
        Ok(InboxPage {
            rows: rows
                .into_iter()
                .map(|(seq, cid, text)| InboxRow { seq, contact_id: cid, text })
                .collect(),
            next_cursor: next,
        })
    }

    /// Страница outbox без ciphertext (cursor — внутренний rowid).
    pub fn outbox_page(&self, cursor: i64, limit: u32) -> Result<OutboxPage, FfiError> {
        let conn = self.conn()?;
        let (rows, next) =
            crate::store::outbox_queued(&conn, cursor, page_limit(limit) as usize)
                .map_err(FfiError::Store)?;
        Ok(OutboxPage {
            rows: rows
                .into_iter()
                .map(|(_, mid, cid, _, st)| OutboxRow {
                    message_id_hex: hex(&mid),
                    contact_id: cid,
                    status: st,
                })
                .collect(),
            next_cursor: next,
        })
    }

    /// Отправить текст (login + refill + claim + одна TX + SEND). Возвращает
    /// message_id hex. Блокирующий вызов для FGS/композера.
    pub fn send_text(
        &self,
        addr: String,
        server_pub: Vec<u8>,
        domain: String,
        contact_id: String,
        text: String,
    ) -> Result<String, FfiError> {
        if addr.is_empty() {
            return Err(FfiError::BadArgs("empty addr".into()));
        }
        if text.is_empty() || text.len() > dmsg_protocol::TEXT_MAX {
            return Err(FfiError::BadText);
        }
        let (sp, dom) = parse_transport_args(server_pub, domain)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;
        rt.block_on(async {
            let mut core = self.core()?;
            core.preflight_text(&contact_id, &text).map_err(map_olm)?;
            let mut t = match self.connect(&addr, &sp, &dom).await {
                Ok(t) => t,
                // Only a failed TCP connect is an offline send. A failed
                // Noise/domain handshake or local auth/store error is not.
                Err(ConnectFailure::Transport(TransportError::Io(e))) if e.starts_with("connect: ") => {
                    return match core.queue_text_existing_session(&contact_id, &text).map_err(map_olm)? {
                        Some(mid) => Ok(hex(&mid)),
                        None => Err(FfiError::Transport(format!("io: {e}"))),
                    };
                }
                Err(e) => return Err(e.into_ffi()),
            };
            let mid =
                core.send_text(&mut t, &contact_id, &text).await.map_err(map_olm)?;
            Ok(hex(&mid))
        })
    }

    /// Ретрай недоставленного тем же ciphertext (пачками, прогресс в DB).
    pub fn retry_queued(
        &self,
        addr: String,
        server_pub: Vec<u8>,
        domain: String,
    ) -> Result<RetryReport, FfiError> {
        if addr.is_empty() {
            return Err(FfiError::BadArgs("empty addr".into()));
        }
        let (sp, dom) = parse_transport_args(server_pub, domain)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;
        rt.block_on(async {
            let mut core = self.core()?;
            let mut t = self.connect(&addr, &sp, &dom).await.map_err(ConnectFailure::into_ffi)?;
            let s = core.retry_queued(&mut t).await.map_err(map_olm)?;
            Ok(RetryReport {
                resent: s.resent as u64,
                accepted: s.accepted as u64,
                delivered: s.delivered as u64,
                skipped: s.skipped as u64,
            })
        })
    }

    /// Приём пачки: FETCH → decrypt → inbox → ACK (событие для FGS/нотификаций).
    pub fn fetch(
        &self,
        addr: String,
        server_pub: Vec<u8>,
        domain: String,
    ) -> Result<FetchReport, FfiError> {
        if addr.is_empty() {
            return Err(FfiError::BadArgs("empty addr".into()));
        }
        let (sp, dom) = parse_transport_args(server_pub, domain)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;
        rt.block_on(async {
            let mut core = self.core()?;
            let mut t = self.connect(&addr, &sp, &dom).await.map_err(ConnectFailure::into_ffi)?;
            let r = core.fetch_and_decrypt(&mut t).await.map_err(map_olm)?;
            Ok(FetchReport {
                received: r
                    .received
                    .into_iter()
                    .map(|m| ReceivedMsg {
                        contact_id: m.contact_id,
                        text: m.text,
                        message_id_hex: hex(&m.message_id),
                        seq: m.seq,
                    })
                    .collect(),
                skipped_unknown: r.skipped_unknown as u64,
                skipped_blocked: r.skipped_blocked as u64,
                skipped_undecryptable: r.skipped_undecryptable as u64,
                skipped_mismatch: r.skipped_mismatch as u64,
                cursor: r.cursor,
            })
        })
    }

    /// Побудка после переподключения FGS: login + refill (возвращает запас).
    pub fn reconnect(
        &self,
        addr: String,
        server_pub: Vec<u8>,
        domain: String,
    ) -> Result<u32, FfiError> {
        if addr.is_empty() {
            return Err(FfiError::BadArgs("empty addr".into()));
        }
        let (sp, dom) = parse_transport_args(server_pub, domain)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;
        rt.block_on(async {
            let mut core = self.core()?;
            let mut t = self.connect(&addr, &sp, &dom).await.map_err(ConnectFailure::into_ffi)?;
            core.on_reconnect(&mut t).await.map_err(map_olm)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_dns_import_cannot_replace_the_winning_pin() {
        let (client, dir) = tmp_client("dns-import-race");
        drop(client.conn().unwrap());
        let client = std::sync::Arc::new(client);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(32));
        let threads: Vec<_> = (0..32).map(|i| {
            let client = client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let key = 8 + (i % 2) as u8;
                let qr = dmsg_protocol::bootstrap::build(b"fixture.invalid", &[0x30, 0], &[key; 32], &[91; 32]).unwrap();
                barrier.wait();
                (key, client.configure_dns(qr, vec!["127.0.0.1:53".into()]))
            })
        }).collect();
        let mut accepted = std::collections::HashSet::new();
        for thread in threads {
            let (key, result) = thread.join().unwrap();
            match result {
                Ok(()) => { accepted.insert(key); }
                Err(FfiError::BadArgs(_)) => (),
                Err(_) => panic!("unexpected DNS import failure"),
            }
        }
        assert_eq!(accepted.len(), 1, "only one immutable profile may win");
        assert!(accepted.contains(&client.dns_profile_info().unwrap().unwrap().noise_pubkey[0]));
        drop(client);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn enrolled_client(name: &str) -> (DmsgClient, std::path::PathBuf) {
        let (c, dir) = tmp_client(name);
        let conn = c.conn().expect("db");
        crate::store::save_identity(&conn, &[3; 32]).expect("identity");
        crate::store::save_account(&conn, &[4; 16], "ALICE0000001").expect("account");
        crate::store::save_token(&conn, &[5; 32]).expect("token");
        drop(conn);
        (c, dir)
    }

    fn peer_contact(c: &DmsgClient) -> (String, vodozemac::olm::Account) {
        let peer = vodozemac::olm::Account::new();
        let id = "BOBB00000002".to_string();
        let qr = contacts::build_qr(&id, &[6; 16], &[7; 32],
            peer.ed25519_key().as_bytes(), peer.curve25519_key().as_bytes()).expect("qr");
        assert_eq!(c.add_contact_qr(qr).expect("add"), QrOutcome::Added);
        c.contact_accept(id.clone()).expect("accept");
        (id, peer)
    }

    fn install_session(c: &DmsgClient, id: &str, peer: &mut vodozemac::olm::Account) {
        peer.generate_one_time_keys(1);
        let ot = *peer.one_time_keys().values().next().expect("one-time").as_bytes();
        let core = c.core().expect("core");
        let (account, _) = crate::olm::load_or_create(&c.conn().expect("db")).expect("account pickle");
        let session = crate::olm::outbound(&account, &crate::olm::curve_identity(peer), &ot).expect("session");
        crate::store::save_session(&c.conn().expect("db"), id,
            &crate::olm::pickle_session(&session).expect("pickle"),
            &crate::olm::ed_identity(peer), &crate::olm::curve_identity(peer)).expect("persist");
        drop(core);
    }

    fn offline_addr() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
        let addr = l.local_addr().expect("addr").to_string();
        drop(l);
        addr
    }

    fn offline_send(c: &DmsgClient, addr: &str, id: &str, text: &str) -> Result<String, FfiError> {
        c.send_text(addr.into(), vec![1; 32], "offline.test".into(), id.into(), text.into())
    }

    #[test]
    fn offline_send_existing_session_queues_ciphertext_via_ffi() {
        let (c, dir) = enrolled_client("queued-existing");
        let (id, mut peer) = peer_contact(&c);
        install_session(&c, &id, &mut peer);
        let c = DmsgClient::open_encrypted(c.db_path.clone(), vec![19; 32]).expect("encrypted");
        let text = "queued-plaintext-sentinel";
        let mid = offline_send(&c, &offline_addr(), &id, text).expect("queued");
        let page = c.outbox_page(0, 10).expect("outbox");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].message_id_hex, mid);
        assert_eq!(page.rows[0].status, "queued");
        let raw = std::fs::read(&c.db_path).expect("db bytes");
        assert!(!raw.windows(text.len()).any(|w| w == text.as_bytes()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn offline_send_changed_identity_stops_before_queue() {
        let (c, dir) = enrolled_client("queued-mismatch");
        let (id, mut peer) = peer_contact(&c);
        install_session(&c, &id, &mut peer);
        let evil = contacts::build_qr(&id, &[6; 16], &[7; 32], &[9; 32], &[8; 32]).expect("evil");
        assert_eq!(c.add_contact_qr(evil).expect("change"), QrOutcome::IdentityChanged);
        assert_eq!(offline_send(&c, &offline_addr(), &id, "secret"), Err(FfiError::IdentityMismatch));
        assert!(c.outbox_page(0, 10).expect("outbox").rows.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn offline_send_without_session_returns_transport_and_no_plaintext() {
        let (c, dir) = enrolled_client("queued-no-session");
        let (id, _) = peer_contact(&c);
        let c = DmsgClient::open_encrypted(c.db_path.clone(), vec![19; 32]).expect("encrypted");
        let text = "no-session-plaintext-sentinel";
        assert!(matches!(offline_send(&c, &offline_addr(), &id, text), Err(FfiError::Transport(_))));
        assert!(c.outbox_page(0, 10).expect("outbox").rows.is_empty());
        let raw = std::fs::read(&c.db_path).expect("db bytes");
        assert!(!raw.windows(text.len()).any(|w| w == text.as_bytes()));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tmp_client(name: &str) -> (DmsgClient, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("dmsg-k4-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let db = dir.join("core.db");
        let _ = std::fs::remove_file(&db);
        (DmsgClient { db_path: db.to_string_lossy().into_owned(), key: None }, dir)
    }

    fn sample_join() -> String {
        dmsg_protocol::bootstrap::build(
            b"msg.example.com",
            &[0x30u8, 0x03, 0x01, 0x01, 0x00],
            &[9u8; 32],
            &[7u8; 32],
        )
        .expect("build")
    }

    fn sample_contact() -> String {
        contacts::build_qr("ABCD1234EFGH", &[1u8; 16], &[2u8; 32], &[3u8; 32], &[4u8; 32])
            .expect("build")
    }

    #[test]
    fn qr_kinds_and_explicit_errors() {
        assert_eq!(qr_kind(sample_join()).expect("join"), QrKind::Join);
        assert_eq!(qr_kind(sample_contact()).expect("contact"), QrKind::Contact);
        // Чужой scheme — bad prefix.
        assert_eq!(
            qr_kind("https://x/".into()),
            Err(FfiError::BadQr("bad prefix".into()))
        );
        // Join с чужим type — тоже bad prefix (не misparse в contact).
        assert_eq!(
            qr_kind("dmsg://contact/AAAA".into()).unwrap_err(),
            FfiError::BadQr("truncated".into())
        );
        // Битый base64 contact — явная ошибка.
        assert_eq!(
            qr_kind("dmsg://contact/!!!".into()),
            Err(FfiError::BadQr("bad encoding".into()))
        );
        // Oversized — явная ошибка до парсера.
        let big = format!("dmsg://join/{}", "A".repeat(QR_URI_MAX));
        assert_eq!(qr_kind(big), Err(FfiError::BadQr("oversized".into())));
        let big2 = format!("dmsg://contact/{}", "A".repeat(QR_URI_MAX));
        assert_eq!(
            DmsgClient::open("/tmp/x".into()).add_contact_qr(big2),
            Err(FfiError::BadQr("oversized".into()))
        );
    }

    #[test]
    fn limits_and_storage_plan() {
        assert_eq!((page_limit(0), page_limit(1), page_limit(50), page_limit(5000)), (1, 1, 50, 100));
        assert_eq!(storage_plan(false, false), StoragePlan::FreshInstall);
        assert_eq!(storage_plan(true, false), StoragePlan::MigrateLegacy);
        assert_eq!(storage_plan(true, true), StoragePlan::ReadyWrapped);
        assert_eq!(storage_plan(false, true), StoragePlan::ReadyWrapped);
    }

    #[test]
    fn offline_commands_before_enrol() {
        let (c, dir) = tmp_client("offline");
        assert_eq!(
            c.account_info().expect("info"),
            AccountInfo { enrolled: false, contact_id: None }
        );
        let pv = c.enrol_preview(sample_join()).expect("preview");
        assert_eq!(pv.domain, "msg.example.com");
        assert_eq!(pv.pin_fingerprint_hex.len(), 64);
        // Свой QR до enrol — NotEnrolled, не паника.
        assert_eq!(c.my_contact_qr(), Err(FfiError::NotEnrolled));
        // Контакты/inbox/outbox работают без enrol (чистый store).
        assert_eq!(c.contact_request("ZZZZ9999YYYY".into()).expect("req"), "requested");
        assert_eq!(
            c.contact_get("ZZZZ9999YYYY".into()).expect("get").state,
            "requested"
        );
        let page = c.contacts_page(None, 10).expect("page");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.next_cursor, None);
        let inbox = c.inbox_page(0, 10).expect("inbox");
        assert!(inbox.rows.is_empty() && inbox.next_cursor.is_none());
        let outbox = c.outbox_page(0, 10).expect("outbox");
        assert!(outbox.rows.is_empty() && outbox.next_cursor.is_none());
        // Block/accept-гейты как в core.
        c.contact_block("ZZZZ9999YYYY".into()).expect("block");
        assert_eq!(
            c.contact_accept("ZZZZ9999YYYY".into()),
            Err(FfiError::Blocked)
        );
        assert_eq!(
            c.contact_confirm("ZZZZ9999YYYY".into()),
            Err(FfiError::NothingToConfirm)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn contact_qr_roundtrip_through_facade() {
        let (c, dir) = tmp_client("qr");
        assert_eq!(c.add_contact_qr(sample_contact()).expect("add"), QrOutcome::Added);
        assert_eq!(
            c.add_contact_qr(sample_contact()).expect("re"),
            QrOutcome::Unchanged
        );
        let evil = contacts::build_qr(
            "ABCD1234EFGH",
            &[1u8; 16],
            &[2u8; 32],
            &[9u8; 32],
            &[8u8; 32],
        )
        .expect("evil");
        assert_eq!(
            c.add_contact_qr(evil).expect("evil"),
            QrOutcome::IdentityChanged
        );
        let info = c.contact_get("ABCD1234EFGH".into()).expect("get");
        assert!(info.identity_mismatch);
        assert!(info.has_keys);
        c.contact_confirm("ABCD1234EFGH".into()).expect("confirm");
        assert!(!c.contact_get("ABCD1234EFGH".into()).expect("get2").identity_mismatch);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn network_args_validated_before_runtime() {
        let (c, dir) = tmp_client("netargs");
        // Короткий server_pub — BadArgs без сети.
        let r = c.send_text(
            "127.0.0.1:1".into(),
            vec![1u8; 5],
            "x.test".into(),
            "ABCD1234EFGH".into(),
            "hi".into(),
        );
        assert_eq!(r, Err(FfiError::BadArgs("server_pub must be 32 bytes".into())));
        // Пустой текст — BadText без сети.
        let r2 = c.send_text(
            "127.0.0.1:1".into(),
            vec![1u8; 32],
            "x.test".into(),
            "ABCD1234EFGH".into(),
            "".into(),
        );
        assert_eq!(r2, Err(FfiError::BadText));
        std::fs::remove_dir_all(&dir).ok();
    }
}

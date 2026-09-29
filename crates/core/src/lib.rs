//! dmsg-core K1–K3: клиентское ядро.
//!
//! Модули: [`transport`] (trait-шов + direct-TCP + библиотечный инициатор),
//! [`supervisor`] (один инстанс, reconnect с backoff — владеет Rust),
//! [`store`] (свой SQLite-файл ядра, WAL),
//! [`enrol`] (K2 offline-enrol из QR: preview без сети, pinned Noise, persist 0600),
//! [`olm`] (K3 Olm E2E поверх vodozemac: identity + signed prekeys,
//! claim/refill без планировщика),
//! [`contacts`] (K3 контакты: ID, request-add, block, contact-QR тем же
//! конвертом, подмена = стоп + confirm),
//! [`chat`] (K3 сессии 1-на-1: ratchet + ciphertext outbox в одной TX,
//! статусы queued/accepted/delivered, ретрай сохранённым ciphertext).
//!
//! Прямой Rust API + K4 UniFFI-фасад [`ffi`] (только команды/события,
//! списки пагинацией; владелец DB — Rust). Общего storage-крейта нет.
//!
//! КОНТРАКТ FFI (зафиксирован в K1, реализация в K4): любой списковый API ядра,
//! уходящий через будущий FFI (сообщения, контакты, события outbox), обязан
//! принимать пагинацию вида (cursor, limit) и возвращать страницу + next_cursor;
//! полные невыгружаемые списки запрещены. Списки [`contacts::list`],
//! [`store::inbox_list`], [`store::outbox_queued`] уже следуют этой форме.

pub mod chat;
pub mod contacts;
pub mod enrol;
pub mod ffi;
pub mod olm;
mod secure;
pub mod store;
pub mod supervisor;
pub mod transport;

uniffi::setup_scaffolding!();

pub use chat::{Core, FetchResult, RawEvent, Received, RetryStats};
pub use contacts::{Contact, QrResult};
pub use enrol::{enrol_from_qr, preview, Enrolled, EnrolError, Preview};
pub use olm::OlmError;
pub use supervisor::{Config as SupervisorConfig, Status, Supervisor};
pub use transport::{initiate, initiate_with_key, DirectTcp, Transport, TransportError};

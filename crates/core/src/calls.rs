//! One service-owned, ephemeral call actor. No call state is persisted.
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, Weak},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use dmsg_protocol::{
    call::*, ERR_BUSY, ERR_REVOKED, OP_CALL_ACK, OP_CALL_ATTACH, OP_CALL_ATTACHED, OP_CALL_COMMAND,
    OP_CALL_EVENT, OP_CALL_OPEN, OP_CALL_READY, OP_DEVICE_BINDING, OP_DEVICE_BINDING_RESP,
    OP_ERROR,
};
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::Zeroizing;

use crate::{
    chat::Core,
    ffi::{map_olm, FfiError},
    live_media::{
        lane::{self, Commit, Lane, SendRequest},
        MediaConfig, MediaSession,
    },
    transport::{DirectTcp, Transport},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CallPhase {
    Stopped,
    Listening,
    Dialing,
    Ringing,
    Connecting,
    Active,
    Ended,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CallDirection {
    Incoming,
    Outgoing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CallEndReason {
    None,
    Unavailable,
    Busy,
    Expired,
    Rejected,
    LocalEnded,
    RemoteEnded,
    ConnectionLost,
    Invalid,
    Revoked,
    AudioUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CallSnapshot {
    pub call_id: String,
    pub contact_id: String,
    pub phase: CallPhase,
    pub direction: CallDirection,
    pub reason: CallEndReason,
    pub media_handle: i64,
    pub available: bool,
}
#[uniffi::export(callback_interface)]
pub trait CallObserver: Send + Sync {
    fn changed(&self, state: CallSnapshot);
}

struct Admission {
    available: bool,
    stopped: bool,
    generation: u64,
    call: Option<[u8; 16]>,
    contact: Option<String>,
    incoming: bool,
    cancelled: bool,
}
struct Shared {
    admission: Mutex<Admission>,
    urgent: watch::Sender<Urgent>,
}
static OWNERS: OnceLock<Mutex<HashMap<String, Weak<Shared>>>> = OnceLock::new();
fn owners() -> &'static Mutex<HashMap<String, Weak<Shared>>> {
    OWNERS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn owner_key(owner: &str) -> String {
    std::fs::canonicalize(owner)
        .unwrap_or_else(|_| Path::new(owner).to_path_buf())
        .to_string_lossy()
        .into_owned()
}
/// TEXT's existing-session path remains usable while bulk is paused.
pub(crate) fn busy(owner: &str) -> bool {
    let shared = owners()
        .lock()
        .ok()
        .and_then(|o| o.get(&owner_key(owner)).and_then(Weak::upgrade));
    shared.is_some_and(|s| s.admission.lock().map(|a| a.call.is_some()).unwrap_or(true))
}

/// Called only after the owning store has committed the local block.
pub(crate) fn contact_blocked(owner: &str, contact: &str) {
    let shared = owners()
        .lock()
        .ok()
        .and_then(|o| o.get(&owner_key(owner)).and_then(Weak::upgrade));
    let Some(shared) = shared else {
        return;
    };
    if let Ok(mut a) = shared.admission.lock() {
        if !a.stopped && a.call.is_some() && a.contact.as_deref() == Some(contact) {
            a.cancelled = true;
            shared.urgent.send_replace(Urgent {
                generation: a.generation,
                reject: false,
                shutdown: false,
            });
        }
    };
}

#[derive(Clone, Copy, Default)]
struct Urgent {
    generation: u64,
    reject: bool,
    shutdown: bool,
}
enum LocalCommand {
    Dial {
        generation: u64,
        id: [u8; 16],
        contact: String,
    },
    Accept {
        generation: u64,
        id: [u8; 16],
    },
}
#[derive(uniffi::Object)]
pub struct DmsgCalls {
    shared: Arc<Shared>,
    commands: mpsc::Sender<LocalCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl DmsgCalls {
    pub(crate) fn start(
        db_path: String,
        key: Option<Zeroizing<Vec<u8>>>,
        observer: Arc<dyn CallObserver>,
    ) -> Result<Arc<Self>, FfiError> {
        Self::create(db_path, key, observer, Endpoint::Dns)
    }

    fn create(
        db_path: String,
        key: Option<Zeroizing<Vec<u8>>>,
        observer: Arc<dyn CallObserver>,
        endpoint: Endpoint,
    ) -> Result<Arc<Self>, FfiError> {
        let owner = owner_key(&db_path);
        let (urgent, cancellation) = watch::channel(Urgent::default());
        let shared = Arc::new(Shared {
            admission: Mutex::new(Admission {
                available: false,
                stopped: false,
                generation: 0,
                call: None,
                contact: None,
                incoming: false,
                cancelled: false,
            }),
            urgent,
        });
        let mut leases = owners().lock().map_err(|_| FfiError::Busy)?;
        if leases.get(&owner).and_then(Weak::upgrade).is_some() {
            return Err(FfiError::Busy);
        }
        leases.insert(owner.clone(), Arc::downgrade(&shared));
        let (commands, requests) = mpsc::channel(4);
        let worker_shared = shared.clone();
        let worker_owner = owner.clone();
        let worker = thread::Builder::new()
            .name("dmsg-calls".into())
            .spawn(move || {
                // The lease survives callbacks and all media joins, including failed startup.
                let _lease = Lease(worker_owner);
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(async {
                        let context = Context::load(db_path, key, endpoint);
                        match context {
                            Ok(context) => {
                                Actor::new(
                                    Arc::new(context),
                                    worker_shared.clone(),
                                    observer.clone(),
                                    requests,
                                    cancellation,
                                )
                                .run()
                                .await
                            }
                            Err(_) => {
                                publish_idle(&worker_shared, &observer, CallPhase::Stopped, false)
                            }
                        }
                    }),
                    Err(_) => publish_idle(&worker_shared, &observer, CallPhase::Stopped, false),
                }
                if let Ok(mut a) = worker_shared.admission.lock() {
                    a.stopped = true;
                    a.available = false;
                }
            })
            .map_err(|_| {
                leases.remove(&owner);
                FfiError::Transport("call worker unavailable".into())
            })?;
        drop(leases);
        Ok(Arc::new(Self {
            shared,
            commands,
            worker: Mutex::new(Some(worker)),
        }))
    }

    fn terminate(&self, call_id: &str, reject: bool) -> Result<(), FfiError> {
        let mut a = self.shared.admission.lock().map_err(|_| FfiError::Busy)?;
        if a.stopped || a.call.is_none() {
            return Ok(());
        }
        // Empty ID is the outgoing UI's stop-before-first-callback slot only.
        if !(call_id.is_empty() && !a.incoming) && Some(parse_id(call_id)?) != a.call {
            return Ok(());
        }
        if reject && !a.incoming {
            return Err(FfiError::InvalidInput);
        }
        a.cancelled = true;
        self.shared.urgent.send_replace(Urgent {
            generation: a.generation,
            reject,
            shutdown: false,
        });
        Ok(())
    }
}
#[uniffi::export]
impl DmsgCalls {
    pub fn dial(&self, contact_id: String) -> Result<(), FfiError> {
        if !crate::contacts::valid_contact_id(&contact_id) {
            return Err(FfiError::InvalidInput);
        }
        let contact = contact_id;
        let mut a = self.shared.admission.lock().map_err(|_| FfiError::Busy)?;
        if a.stopped || !a.available {
            return Err(FfiError::Transport("call listener unavailable".into()));
        }
        if a.call.is_some() {
            return Err(FfiError::Busy);
        }
        let mut id = [0; 16];
        while id == [0; 16] {
            getrandom::fill(&mut id).map_err(|_| FfiError::Crypto("rng".into()))?;
        }
        let generation = a.generation.wrapping_add(1).max(1);
        self.commands
            .try_send(LocalCommand::Dial {
                generation,
                id,
                contact: contact.clone(),
            })
            .map_err(|_| FfiError::Busy)?;
        a.generation = generation;
        a.call = Some(id);
        a.contact = Some(contact);
        a.incoming = false;
        a.cancelled = false;
        Ok(())
    }
    pub fn accept(&self, call_id: String) -> Result<(), FfiError> {
        let id = parse_id(&call_id)?;
        let a = self.shared.admission.lock().map_err(|_| FfiError::Busy)?;
        if a.stopped || !a.available {
            return Err(FfiError::Transport("call listener unavailable".into()));
        }
        if a.call != Some(id) || !a.incoming || a.cancelled {
            return Err(FfiError::InvalidInput);
        }
        self.commands
            .try_send(LocalCommand::Accept {
                generation: a.generation,
                id,
            })
            .map_err(|_| FfiError::Busy)
    }
    pub fn reject(&self, call_id: String) -> Result<(), FfiError> {
        self.terminate(&call_id, true)
    }
    pub fn end(&self, call_id: String) -> Result<(), FfiError> {
        self.terminate(&call_id, false)
    }
    /// The service calls this off the UI thread. It joins only this object's worker.
    pub fn shutdown(&self) -> Result<(), FfiError> {
        {
            let mut a = self.shared.admission.lock().map_err(|_| FfiError::Busy)?;
            a.stopped = true;
            a.available = false;
            a.cancelled = true;
            self.shared.urgent.send_replace(Urgent {
                shutdown: true,
                ..Urgent::default()
            });
        }
        let mut worker = self.worker.lock().map_err(|_| FfiError::Busy)?;
        if worker
            .as_ref()
            .is_some_and(|w| w.thread().id() == thread::current().id())
        {
            return Err(FfiError::BadArgs(
                "shutdown must run outside the observer callback".into(),
            ));
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| FfiError::Transport("call worker failed".into()))?;
        }
        Ok(())
    }
}
impl Drop for DmsgCalls {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
struct Lease(String);
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut owners) = owners().lock() {
            owners.remove(&self.0);
        }
    }
}

enum Endpoint {
    Dns,
    #[cfg(test)]
    Local {
        address: String,
        offers: Arc<Mutex<Vec<[u8; 32]>>>,
    },
}
struct Context {
    db_path: String,
    key: Option<Zeroizing<Vec<u8>>>,
    profile: crate::dns::Profile,
    device: [u8; 32],
    account: ([u8; 16], String),
    endpoint: Endpoint,
}
impl Context {
    fn load(
        db_path: String,
        key: Option<Zeroizing<Vec<u8>>>,
        endpoint: Endpoint,
    ) -> Result<Self, FfiError> {
        let core = open_core(&db_path, &key)?;
        let profile = crate::dns::load(&core.conn)
            .map_err(FfiError::Store)?
            .ok_or(FfiError::InvalidInput)?;
        let device = core.device_pub();
        let account = core.my_account().map_err(map_olm)?;
        Ok(Self {
            db_path,
            key,
            profile,
            device,
            account,
            endpoint,
        })
    }
    fn core(&self) -> Result<Core, FfiError> {
        let core = open_core(&self.db_path, &self.key)?;
        if core.device_pub() != self.device || core.my_account().map_err(map_olm)? != self.account {
            return Err(FfiError::Revoked);
        }
        Ok(core)
    }
    fn address(&self) -> Result<String, FfiError> {
        match &self.endpoint {
            #[cfg(test)]
            Endpoint::Local { address, .. } => Ok(address.clone()),
            Endpoint::Dns => {
                // Normal "online" owns carrier startup. Never wait synchronously
                // for a 35s DNS bootstrap inside this current-thread event loop.
                if !matches!(
                    crate::dns::status(&self.db_path),
                    Ok(crate::dns::State::Ready(_))
                ) {
                    return Err(FfiError::Transport("DNS unavailable".into()));
                }
                crate::dns::endpoint(&self.db_path, &self.profile)
                    .map(|a| a.to_string())
                    .map_err(|_| FfiError::Transport("DNS unavailable".into()))
            }
        }
    }
    fn connected(&self) -> bool {
        match &self.endpoint {
            #[cfg(test)]
            Endpoint::Local { .. } => true,
            Endpoint::Dns => matches!(
                crate::dns::status(&self.db_path),
                Ok(crate::dns::State::Ready(_))
            ),
        }
    }
    async fn connect(&self) -> Result<DirectTcp, FfiError> {
        let addr = self.address()?;
        let key = {
            let core = self.core()?;
            Zeroizing::new(
                crate::store::load_identity(&core.conn)
                    .map_err(FfiError::Store)?
                    .ok_or(FfiError::NotEnrolled)?,
            )
        };
        let mut t = crate::transport::initiate_with_key(
            &addr,
            &self.profile.noise_pubkey,
            self.profile.domain.as_bytes(),
            &key,
        )
        .await
        .map_err(|_| FfiError::Transport("call handshake failed".into()))?;
        self.core()?.login(&mut t).await.map_err(map_olm)?;
        Ok(t)
    }
    async fn signal(&self) -> Result<Lane, FfiError> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut t = self.connect().await?;
            t.send_frame(OP_CALL_OPEN, &[VERSION])
                .await
                .map_err(transport_error)?;
            let (op, p) = t.recv_frame().await.map_err(transport_error)?;
            server_error(op, &p)?;
            if op != OP_CALL_READY || p != [VERSION, PROFILE] {
                return Err(FfiError::Protocol("invalid call ready".into()));
            }
            into_lane(t)
        })
        .await
        .map_err(|_| FfiError::Transport("call open deadline".into()))?
    }
    async fn attach(&self, id: [u8; 16]) -> Result<(DirectTcp, DirectTcp), FfiError> {
        tokio::time::timeout(Duration::from_secs(ATTACH_SECS), async {
            let mut lanes = Vec::with_capacity(2);
            for lane in [LaneKind::Rtp, LaneKind::Srtcp] {
                let mut t = self.connect().await?;
                let p = Attach {
                    call_id: id,
                    epoch: 1,
                    lane,
                }
                .encode()
                .map_err(protocol_error)?;
                t.send_frame(OP_CALL_ATTACH, &p)
                    .await
                    .map_err(transport_error)?;
                let (op, echo) = t.recv_frame().await.map_err(transport_error)?;
                server_error(op, &echo)?;
                if op != OP_CALL_ATTACHED || echo != p {
                    return Err(FfiError::Protocol(
                        "invalid call attach acknowledgement".into(),
                    ));
                }
                lanes.push(t);
            }
            let control = lanes.pop().expect("two attachments");
            Ok((lanes.pop().expect("two attachments"), control))
        })
        .await
        .map_err(|_| FfiError::Transport("call attach deadline".into()))?
    }

    async fn prepare(&self, contact: String, id: [u8; 16]) -> Result<Prepared, FfiError> {
        // This short account RPC ends before media sockets exist. Signaling keeps
        // listening while directory/prekey IO is in flight; it is never repurposed.
        tokio::time::timeout(Duration::from_secs(10), async {
            self.core()?.call_peer(&contact).map_err(map_olm)?;
            let mut t = self.connect().await?;
            let c = crate::contacts::get(&self.core()?.conn, &contact)
                .map_err(map_olm)?
                .ok_or(FfiError::UnknownContact)?;
            let user = c.user_id.ok_or(FfiError::MissingKeys)?;
            t.send_frame(OP_DEVICE_BINDING, &user)
                .await
                .map_err(transport_error)?;
            let (op, p) = t.recv_frame().await.map_err(transport_error)?;
            server_error(op, &p)?;
            if op != OP_DEVICE_BINDING_RESP {
                return Err(FfiError::Protocol("invalid call peer binding".into()));
            }
            let binding = dmsg_protocol::auth::parse_binding(&p).map_err(protocol_error)?;
            crate::contacts::check_binding(&self.core()?.conn, &contact, &binding)
                .map_err(map_olm)?;
            let peer = self.core()?.call_peer(&contact).map_err(map_olm)?;
            let has_session = crate::store::load_session(&self.core()?.conn, &contact)
                .map_err(FfiError::Store)?
                .is_some();
            if !has_session {
                let (_, otk) = crate::olm::claim_key(&mut t, &peer.device)
                    .await
                    .map_err(map_olm)?;
                // Reload inside the write transaction AFTER the network operation.
                // A concurrent TEXT sender's winning ratchet is never overwritten.
                let mut core = self.core()?;
                let tx = core
                    .conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|_| FfiError::Store("call session transaction failed".into()))?;
                let current = crate::contacts::get(&tx, &contact)
                    .map_err(map_olm)?
                    .ok_or(FfiError::UnknownContact)?;
                if current != c {
                    return Err(FfiError::IdentityMismatch);
                }
                crate::contacts::sendable(&current).map_err(map_olm)?;
                if crate::store::load_session(&tx, &contact)
                    .map_err(FfiError::Store)?
                    .is_none()
                {
                    let (account, _) = crate::olm::load_or_create(&tx).map_err(map_olm)?;
                    let ed = current.ed_identity.ok_or(FfiError::MissingKeys)?;
                    let curve = current.curve_identity.ok_or(FfiError::MissingKeys)?;
                    let session = crate::olm::outbound(&account, &curve, &otk).map_err(map_olm)?;
                    let pickle =
                        Zeroizing::new(crate::olm::pickle_session(&session).map_err(map_olm)?);
                    crate::store::save_session(&tx, &contact, &pickle, &ed, &curve)
                        .map_err(FfiError::Store)?;
                }
                tx.commit()
                    .map_err(|_| FfiError::Store("call session commit failed".into()))?;
            }
            drop(t);
            let mut core = self.core()?;
            let peer = core.call_peer(&contact).map_err(map_olm)?;
            let local = core
                .call_offer(&contact, id, Action::Invite)
                .map_err(map_olm)?;
            #[cfg(test)]
            self.observe_offer(&local);
            let ciphertext = core.encrypt_call(&contact, &local).map_err(map_olm)?;
            Ok(Prepared {
                peer_user: peer.user,
                peer_device: peer.device,
                local,
                ciphertext,
            })
        })
        .await
        .map_err(|_| FfiError::Transport("call prepare deadline".into()))?
    }
    #[cfg(test)]
    fn observe_offer(&self, envelope: &KeyEnvelope) {
        use sha2::Digest;
        if let Endpoint::Local { offers, .. } = &self.endpoint {
            offers
                .lock()
                .unwrap()
                .push(sha2::Sha256::digest(envelope.key).into());
        }
    }
}
fn open_core(path: &str, key: &Option<Zeroizing<Vec<u8>>>) -> Result<Core, FfiError> {
    match key {
        Some(key) => Core::open_encrypted(Path::new(path), key),
        None => Core::open(Path::new(path)),
    }
    .map_err(map_olm)
}
fn transport_error(_: crate::transport::TransportError) -> FfiError {
    FfiError::Transport("call stream failed".into())
}
fn protocol_error(_: impl std::fmt::Debug) -> FfiError {
    FfiError::Protocol("invalid call frame".into())
}
fn server_error(op: u8, p: &[u8]) -> Result<(), FfiError> {
    if op != OP_ERROR {
        return Ok(());
    }
    match p {
        [ERR_REVOKED] => Err(FfiError::Revoked),
        [ERR_BUSY] => Err(FfiError::Busy),
        _ => Err(FfiError::Protocol("call rejected".into())),
    }
}
fn into_lane(t: DirectTcp) -> Result<Lane, FfiError> {
    let (stream, noise) = t.into_lane_parts().map_err(transport_error)?;
    lane::spawn(stream, noise).map_err(|_| FfiError::Transport("call lane unavailable".into()))
}

type Work<T> = Pin<Box<dyn Future<Output = Result<T, FfiError>>>>;
struct Prepared {
    peer_user: [u8; 16],
    peer_device: [u8; 32],
    local: KeyEnvelope,
    ciphertext: Vec<u8>,
}
struct Call {
    id: [u8; 16],
    generation: u64,
    contact: String,
    direction: CallDirection,
    phase: CallPhase,
    peer_user: [u8; 16],
    peer_device: [u8; 32],
    local: Option<KeyEnvelope>,
    remote: Option<KeyEnvelope>,
    deadline: Option<Instant>,
    submitted: bool,
    accepted: bool,
    ready: bool,
    ending: Option<Reason>,
    lanes: Option<(Lane, Lane)>,
    media: Option<(MediaSession, Arc<crate::call_media_handles::Entry>, i64)>,
}
impl Call {
    fn snapshot(&self, reason: Reason, available: bool) -> CallSnapshot {
        CallSnapshot {
            call_id: hex_id(self.id),
            contact_id: self.contact.clone(),
            phase: self.phase,
            direction: self.direction,
            reason: end_reason(reason),
            media_handle: self.media.as_ref().map_or(0, |m| m.2),
            available,
        }
    }
}
struct Pending {
    id: u32,
    call: [u8; 16],
    action: Action,
    deadline: Instant,
    payload: Option<Vec<u8>>,
    receipt: Option<oneshot::Receiver<Commit>>,
}
struct Actor {
    context: Arc<Context>,
    shared: Arc<Shared>,
    observer: Arc<dyn CallObserver>,
    commands: mpsc::Receiver<LocalCommand>,
    urgent: watch::Receiver<Urgent>,
    call: Option<Call>,
    prepare: Option<Work<Prepared>>,
    attaching: Option<Work<(DirectTcp, DirectTcp)>>,
    pending: VecDeque<Pending>,
    next_request: u32,
    signal_failed: bool,
}
impl Actor {
    fn new(
        context: Arc<Context>,
        shared: Arc<Shared>,
        observer: Arc<dyn CallObserver>,
        commands: mpsc::Receiver<LocalCommand>,
        urgent: watch::Receiver<Urgent>,
    ) -> Self {
        Self {
            context,
            shared,
            observer,
            commands,
            urgent,
            call: None,
            prepare: None,
            attaching: None,
            pending: VecDeque::new(),
            next_request: 0,
            signal_failed: false,
        }
    }
    async fn run(mut self) {
        publish_idle(&self.shared, &self.observer, CallPhase::Listening, false);
        let mut backoff = 250;
        loop {
            if self.urgent.borrow().shutdown
                || self
                    .shared
                    .admission
                    .lock()
                    .map(|a| a.stopped)
                    .unwrap_or(true)
            {
                break;
            }
            // Commands accepted by a former listener must never replay after loss.
            while self.commands.try_recv().is_ok() {}
            let context = self.context.clone();
            let connect = context.signal();
            tokio::pin!(connect);
            let opened = tokio::select! {
                biased;
                _ = self.urgent.changed() => { continue; }
                result = &mut connect => result,
            };
            match opened {
                Ok(mut signal) => {
                    backoff = 250;
                    self.signal_failed = false;
                    publish_idle(&self.shared, &self.observer, CallPhase::Listening, true);
                    let reason = self.listen(&mut signal).await;
                    set_available(&self.shared, false);
                    self.finish(reason).await;
                    signal.joined_close().await;
                    self.pending.clear();
                    if self.urgent.borrow().shutdown
                        || reason == Reason::Revoked
                        || self
                            .shared
                            .admission
                            .lock()
                            .map(|a| a.stopped)
                            .unwrap_or(true)
                    {
                        break;
                    }
                }
                Err(FfiError::Revoked) => break,
                Err(_) => (),
            }
            publish_idle(&self.shared, &self.observer, CallPhase::Stopped, false);
            tokio::select! {
                _ = self.urgent.changed() => (),
                _ = tokio::time::sleep(Duration::from_millis(backoff)) => (),
            }
            backoff = (backoff * 2).min(5000);
        }
        self.finish(Reason::LocalEnded).await;
        publish_idle(&self.shared, &self.observer, CallPhase::Stopped, false);
    }
    async fn listen(&mut self, signal: &mut Lane) -> Reason {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut heartbeat = Instant::now() + Duration::from_secs(HEARTBEAT_SECS);
        loop {
            let urgent = *self.urgent.borrow_and_update();
            if urgent.shutdown {
                if self.call.is_none() {
                    return Reason::LocalEnded;
                }
                self.local_end(Reason::LocalEnded).await;
            }
            if !urgent.shutdown
                && self
                    .shared
                    .admission
                    .lock()
                    .map(|a| a.stopped)
                    .unwrap_or(true)
            {
                return Reason::ConnectionLost;
            }
            if self.cancelled() && !urgent.shutdown {
                let reason = if urgent.reject {
                    Reason::Rejected
                } else {
                    Reason::LocalEnded
                };
                if let Some(call) = &self.call {
                    if call.submitted && call.ending.is_none() {
                        let action = if urgent.generation == call.generation
                            && urgent.reject
                            && !call.accepted
                            && self.attaching.is_none()
                        {
                            Action::Reject
                        } else {
                            Action::End
                        };
                        self.request_end(action, reason).await;
                    } else if !call.submitted {
                        self.finish(reason).await;
                    }
                } else {
                    self.finish(reason).await;
                }
            }
            if self.flush(signal).is_err() {
                return Reason::ConnectionLost;
            }
            tokio::select! {
                biased;
                changed = self.urgent.changed() => { if changed.is_err() { return Reason::LocalEnded; } }
                frame = signal.incoming.recv() => {
                    let Some(Ok(frame)) = frame else { return Reason::ConnectionLost; };
                    if !self.context.connected() { return Reason::ConnectionLost; }
                    let result = self.frame(frame.frame.0, &frame.frame.1).await;
                    if let Err(reason) = result { return reason; }
                }
                command = self.commands.recv() => {
                    let Some(command) = command else { return Reason::LocalEnded; };
                    if self.command(command).is_err() { self.local_end(Reason::Invalid).await; }
                }
                result = async { self.prepare.as_mut().expect("guarded preparation").await }, if self.prepare.is_some() && self.call.as_ref().is_some_and(|c| c.ending.is_none()) => {
                    self.prepare = None;
                    if self.cancelled() { continue; }
                    match result {
                        Ok(prepared) => {
                            if let Some(call) = &mut self.call {
                                call.peer_user = prepared.peer_user; call.peer_device = prepared.peer_device;
                                call.local = Some(prepared.local);
                                let (id, recipient) = (call.id, call.peer_user);
                                // Serialize admission with UI cancellation; never queue an
                                // Invite for an already-cancelled dial generation.
                                let shared = self.shared.clone();
                                let a = shared.admission.lock().expect("call admission");
                                if a.cancelled { continue; }
                                if self.send(Action::Invite, id, recipient, prepared.ciphertext).is_err() { return Reason::ConnectionLost; }
                                self.call.as_mut().expect("prepared call").submitted = true;
                            }
                        }
                        Err(error) => self.local_end(failure_reason(&error)).await,
                    }
                }
                result = async { self.attaching.as_mut().expect("guarded attachments").await }, if self.attaching.is_some() && self.call.as_ref().is_some_and(|c| c.ending.is_none()) => {
                    self.attaching = None;
                    match result {
                        Ok((rtp, control)) => {
                            match (into_lane(rtp), into_lane(control)) {
                                (Ok(rtp), Ok(control)) => { if let Some(call) = &mut self.call { call.lanes = Some((rtp, control)); } }
                                _ => { self.local_end(Reason::ConnectionLost).await; continue; }
                            }
                            self.start_media().await;
                        }
                        Err(error) => self.local_end(failure_reason(&error)).await,
                    }
                }
                _ = tick.tick() => {
                    if !self.context.connected() { return Reason::ConnectionLost; }
                    let now = Instant::now();
                    if self.call.as_ref().is_some_and(|c| c.deadline.is_some_and(|d| now >= d)) {
                        if self.call.as_ref().is_some_and(|c| c.ending.is_some()) { return Reason::ConnectionLost; }
                        self.local_end(Reason::Expired).await;
                    }
                    if now >= heartbeat {
                        heartbeat = now + Duration::from_secs(HEARTBEAT_SECS);
                        if !self.pending.iter().any(|p| p.action == Action::Heartbeat) && self.send(Action::Heartbeat, [0; 16], [0; 16], vec![]).is_err() { return Reason::ConnectionLost; }
                    }
                    if let Some((session, entry, _)) = self.call.as_ref().and_then(|c| c.media.as_ref()) {
                        let stats = session.snapshot();
                        let stop_requested = entry.stop_requested();
                        self.media_progress(now, &stats, stop_requested).await;
                    }
                }
            }
        }
    }
    async fn media_progress(
        &mut self,
        now: Instant,
        stats: &crate::live_media::Stats,
        stop_requested: bool,
    ) {
        let Some(call) = &mut self.call else {
            return;
        };
        if call.ending.is_some() {
            return;
        }
        if stats.failed || stats.stopped {
            // The media worker already joined/closed its lanes.
            // Their server registration emits authoritative Ended;
            // SIG distinguishes remote hangup from actual loss.
            // JNI retirement after that failure must not replace its reason.
            call.ending = Some(Reason::ConnectionLost);
            call.deadline = Some(now + Duration::from_secs(30));
        } else if stop_requested {
            self.local_end(Reason::AudioUnavailable).await;
        } else if stats.ready && call.phase != CallPhase::Active {
            call.phase = CallPhase::Active;
            notify(
                &self.shared,
                &self.observer,
                call.snapshot(Reason::Ok, true),
            );
        }
    }
    fn cancelled(&self) -> bool {
        self.shared
            .admission
            .lock()
            .map(|a| a.call.is_some() && a.cancelled)
            .unwrap_or(true)
    }
    fn command(&mut self, command: LocalCommand) -> Result<(), FfiError> {
        match command {
            LocalCommand::Dial {
                generation,
                id,
                contact,
            } => {
                let a = self.shared.admission.lock().map_err(|_| FfiError::Busy)?;
                if a.generation != generation
                    || a.call != Some(id)
                    || a.cancelled
                    || !a.available
                    || self.call.is_some()
                {
                    return Ok(());
                }
                drop(a);
                self.call = Some(Call {
                    id,
                    generation,
                    contact: contact.clone(),
                    direction: CallDirection::Outgoing,
                    phase: CallPhase::Dialing,
                    peer_user: [0; 16],
                    peer_device: [0; 32],
                    local: None,
                    remote: None,
                    deadline: Some(Instant::now() + Duration::from_secs(RING_SECS)),
                    submitted: false,
                    accepted: false,
                    ready: false,
                    ending: None,
                    lanes: None,
                    media: None,
                });
                notify(
                    &self.shared,
                    &self.observer,
                    self.call.as_ref().expect("dial").snapshot(Reason::Ok, true),
                );
                let context = self.context.clone();
                self.prepare = Some(Box::pin(async move { context.prepare(contact, id).await }));
            }
            LocalCommand::Accept { generation, id } => {
                if self.cancelled() {
                    return Ok(());
                }
                let Some(call) = &mut self.call else {
                    return Ok(());
                };
                if call.generation != generation
                    || call.id != id
                    || call.direction != CallDirection::Incoming
                    || call.phase != CallPhase::Ringing
                    || call.local.is_some()
                {
                    return Ok(());
                }
                let mut core = self.context.core()?;
                let local = core
                    .call_offer(&call.contact, id, Action::Accept)
                    .map_err(map_olm)?;
                #[cfg(test)]
                self.context.observe_offer(&local);
                let ciphertext = core.encrypt_call(&call.contact, &local).map_err(map_olm)?;
                call.local = Some(local);
                let shared = self.shared.clone();
                let a = shared.admission.lock().map_err(|_| FfiError::Busy)?;
                if a.cancelled || a.stopped || a.generation != generation {
                    return Ok(());
                }
                self.send(Action::Accept, id, [0; 16], ciphertext)
                    .map_err(|_| FfiError::Busy)?;
            }
        }
        Ok(())
    }
    fn send(
        &mut self,
        action: Action,
        call: [u8; 16],
        recipient: [u8; 16],
        ciphertext: Vec<u8>,
    ) -> Result<(), ()> {
        if self.pending.len() >= EVENT_CAP {
            return Err(());
        }
        self.next_request = self.next_request.wrapping_add(1).max(1);
        let command = Command {
            request_id: self.next_request,
            action,
            call_id: call,
            epoch: if action == Action::Heartbeat { 0 } else { 1 },
            recipient,
            ciphertext,
        };
        let payload = command.encode().map_err(|_| ())?;
        self.pending.push_back(Pending {
            id: command.request_id,
            call,
            action,
            deadline: Instant::now() + Duration::from_secs(30),
            payload: Some(payload),
            receipt: None,
        });
        Ok(())
    }
    fn flush(&mut self, signal: &Lane) -> Result<(), ()> {
        if self.signal_failed {
            return Err(());
        }
        for p in &mut self.pending {
            if Instant::now() >= p.deadline {
                return Err(());
            }
            if let Some(receipt) = &mut p.receipt {
                match receipt.try_recv() {
                    Ok(Commit::Committed { .. }) => p.receipt = None,
                    Err(oneshot::error::TryRecvError::Empty) => (),
                    _ => return Err(()),
                }
            }
            if let Some(payload) = p.payload.take() {
                let (committed, receipt) = oneshot::channel();
                match signal.outgoing.try_send(SendRequest {
                    opcode: OP_CALL_COMMAND,
                    payload,
                    deadline: Some(p.deadline),
                    committed: Some(committed),
                }) {
                    Ok(()) => p.receipt = Some(receipt),
                    Err(mpsc::error::TrySendError::Full(request)) => {
                        p.payload = Some(request.payload);
                        break;
                    }
                    Err(_) => return Err(()),
                }
            }
        }
        Ok(())
    }
    async fn frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), Reason> {
        if opcode == OP_ERROR {
            return Err(if payload == [ERR_REVOKED] {
                Reason::Revoked
            } else {
                Reason::ConnectionLost
            });
        }
        if opcode == OP_CALL_ACK {
            let ack = Ack::decode(payload).map_err(|_| Reason::Invalid)?;
            let index = self
                .pending
                .iter()
                .position(|p| p.id == ack.request_id)
                .ok_or(Reason::Invalid)?;
            let pending = self.pending.remove(index).expect("matched request");
            if pending.call != ack.call_id
                || ack.epoch
                    != if pending.action == Action::Heartbeat {
                        0
                    } else {
                        1
                    }
                || pending.payload.is_some()
            {
                return Err(Reason::Invalid);
            }
            if pending.action == Action::Heartbeat {
                return if ack.reason == Reason::Ok {
                    Ok(())
                } else {
                    Err(ack.reason)
                };
            }
            if self.call.as_ref().is_none_or(|c| c.id != ack.call_id) {
                return Ok(());
            }
            if matches!(pending.action, Action::End | Action::Reject) {
                let reason = self
                    .call
                    .as_ref()
                    .and_then(|c| c.ending)
                    .unwrap_or(ack.reason);
                self.finish(if ack.reason == Reason::Ok {
                    reason
                } else {
                    ack.reason
                })
                .await;
                return Ok(());
            }
            if self.cancelled() {
                return Ok(());
            }
            if ack.reason != Reason::Ok {
                self.local_end(ack.reason).await;
                return Ok(());
            }
            match pending.action {
                Action::Ringing => {
                    let call = self.call.as_mut().expect("ring acknowledgement");
                    call.phase = CallPhase::Ringing;
                    notify(
                        &self.shared,
                        &self.observer,
                        call.snapshot(Reason::Ok, true),
                    );
                }
                Action::Accept => {
                    self.begin_attach();
                }
                _ => (),
            }
            return Ok(());
        }
        if opcode != OP_CALL_EVENT {
            return Err(Reason::Invalid);
        }
        let event = Event::decode(payload).map_err(|_| Reason::Invalid)?;
        if event.kind == EventKind::Invite {
            if self.call.is_some() || busy(&self.context.db_path) {
                self.send(Action::Reject, event.call_id, [0; 16], vec![])
                    .map_err(|_| Reason::ConnectionLost)?;
                return Ok(());
            }
            let received = self.context.core().and_then(|mut c| {
                c.decrypt_call(
                    event.peer_user,
                    event.peer_device,
                    &event.ciphertext,
                    event.call_id,
                    Action::Invite,
                )
                .map_err(map_olm)
            });
            let (contact, remote) = match received {
                Ok(received) => received,
                Err(_) => {
                    self.send(Action::End, event.call_id, [0; 16], vec![])
                        .map_err(|_| Reason::ConnectionLost)?;
                    return Ok(());
                }
            };
            let mut a = self
                .shared
                .admission
                .lock()
                .map_err(|_| Reason::ConnectionLost)?;
            if a.call.is_some() || a.stopped || !a.available {
                drop(a);
                self.send(Action::Reject, event.call_id, [0; 16], vec![])
                    .map_err(|_| Reason::ConnectionLost)?;
                return Ok(());
            }
            // A block may have committed after decrypt_call, before reservation.
            // Revalidate while holding admission: a later commit's notification
            // must then observe this peer and cancel the reserved generation.
            if self
                .context
                .core()
                .and_then(|c| c.call_peer(&contact).map_err(map_olm))
                .is_err()
            {
                drop(a);
                self.send(Action::End, event.call_id, [0; 16], vec![])
                    .map_err(|_| Reason::ConnectionLost)?;
                return Ok(());
            }
            a.generation = a.generation.wrapping_add(1).max(1);
            a.call = Some(event.call_id);
            a.cancelled = false;
            a.incoming = true;
            a.contact = Some(contact.clone());
            let generation = a.generation;
            drop(a);
            self.call = Some(Call {
                id: event.call_id,
                generation,
                contact,
                direction: CallDirection::Incoming,
                phase: CallPhase::Dialing,
                peer_user: event.peer_user,
                peer_device: event.peer_device,
                local: None,
                remote: Some(remote),
                deadline: Some(
                    Instant::now() + Duration::from_millis(u64::from(event.remaining_ms)),
                ),
                submitted: true,
                accepted: false,
                ready: false,
                ending: None,
                lanes: None,
                media: None,
            });
            // The UI must not expose an answerable incoming call before this ACK.
            self.send(Action::Ringing, event.call_id, [0; 16], vec![])
                .map_err(|_| Reason::ConnectionLost)?;
            return Ok(());
        }
        let Some(call) = &mut self.call else {
            return Ok(());
        };
        if call.id != event.call_id {
            return Ok(());
        }
        if call.peer_user != event.peer_user || call.peer_device != event.peer_device {
            return Err(Reason::Invalid);
        }
        if call.ending.is_some() && event.kind != EventKind::Ended {
            return Ok(());
        }
        match event.kind {
            EventKind::Ringing if call.direction == CallDirection::Outgoing && !call.accepted => {
                call.phase = CallPhase::Ringing;
                notify(
                    &self.shared,
                    &self.observer,
                    call.snapshot(Reason::Ok, true),
                );
            }
            EventKind::Accepted if call.direction == CallDirection::Outgoing && !call.accepted => {
                let (contact, remote) = self
                    .context
                    .core()
                    .map_err(|e| failure_reason(&e))?
                    .decrypt_call(
                        event.peer_user,
                        event.peer_device,
                        &event.ciphertext,
                        event.call_id,
                        Action::Accept,
                    )
                    .map_err(|_| Reason::Invalid)?;
                if contact != call.contact {
                    return Err(Reason::Invalid);
                }
                call.remote = Some(remote);
                self.begin_attach();
            }
            EventKind::MediaReady if call.accepted => {
                call.ready = true;
                self.start_media().await;
            }
            EventKind::Ended => {
                let reason = match call.ending {
                    Some(Reason::ConnectionLost) | None => event.reason,
                    Some(reason) => reason,
                };
                self.finish(reason).await;
            }
            _ => return Err(Reason::Invalid),
        }
        Ok(())
    }
    fn begin_attach(&mut self) {
        if let Some(call) = &mut self.call {
            call.accepted = true;
            call.deadline = Some(Instant::now() + Duration::from_secs(ATTACH_SECS));
            let context = self.context.clone();
            let id = call.id;
            self.attaching = Some(Box::pin(async move { context.attach(id).await }));
        }
    }
    async fn start_media(&mut self) {
        if self.cancelled() {
            return;
        }
        let Some(call) = &mut self.call else {
            return;
        };
        if !call.ready || !call.accepted || call.lanes.is_none() || call.media.is_some() {
            return;
        }
        let (Some(local), Some(remote)) = (&call.local, &call.remote) else {
            self.local_end(Reason::Invalid).await;
            return;
        };
        if local.sender_binding != remote.recipient_binding
            || local.recipient_binding != remote.sender_binding
        {
            self.local_end(Reason::Invalid).await;
            return;
        }
        let config = MediaConfig {
            profile_ms: 40,
            media_tx: local.key,
            media_rx: remote.key,
            ssrc_tx: local.ssrc,
            ssrc_rx: remote.ssrc,
            cname: local.cname,
            peer_cname: remote.cname,
            initial_sequence: local.sequence,
            peer_initial_sequence: remote.sequence,
            initial_timestamp: local.timestamp,
            peer_initial_timestamp: remote.timestamp,
            healthy_cycle_ms: 600,
            capacity_bps: 50_000,
        };
        let (rtp, control) = call.lanes.take().expect("attached lanes");
        let mut session = match MediaSession::start(config, rtp, control) {
            Ok(session) => session,
            Err(_) => {
                self.local_end(Reason::Invalid).await;
                return;
            }
        };
        let (handle, entry) =
            match crate::call_media_handles::register(&self.context.db_path, session.audio()) {
                Ok(handle) => handle,
                Err(_) => {
                    session.stop().await;
                    self.local_end(Reason::AudioUnavailable).await;
                    return;
                }
            };
        call.local = None;
        call.remote = None;
        call.deadline = None;
        call.phase = CallPhase::Connecting;
        call.media = Some((session, entry, handle));
        notify(
            &self.shared,
            &self.observer,
            call.snapshot(Reason::Ok, true),
        );
    }
    async fn local_end(&mut self, reason: Reason) {
        self.request_end(Action::End, reason).await;
    }
    async fn request_end(&mut self, action: Action, reason: Reason) {
        if let Some(call) = &mut self.call {
            if call.ending.is_some() {
                return;
            }
            if call.submitted {
                call.ending = Some(reason);
                call.deadline = Some(Instant::now() + Duration::from_secs(30));
                let id = call.id;
                if self.send(action, id, [0; 16], vec![]).is_err() {
                    // Retiring SIG removes every server route on queue/send loss.
                    self.signal_failed = true;
                }
                // Keep attachment/media owners alive until SIG End is processed.
                // Closing a media lane first would race End into ConnectionLost.
                return;
            }
        }
        self.finish(reason).await;
    }
    async fn finish(&mut self, reason: Reason) {
        self.prepare = None;
        self.attaching = None;
        let call = self.call.take();
        let Some(mut call) = call else {
            // An urgent stop can beat the queued Dial itself.
            let available = if let Ok(mut a) = self.shared.admission.lock() {
                let reserved = a.call.take().is_some();
                a.contact = None;
                a.cancelled = false;
                reserved && a.available && !a.stopped
            } else {
                false
            };
            if available {
                publish_idle(&self.shared, &self.observer, CallPhase::Listening, true);
            }
            return;
        };
        if let Some((mut media, entry, _)) = call.media.take() {
            let stats = media.stop().await;
            // JNI stop may be waiting on this completion. Never complete first.
            entry.complete(stats);
        }
        if let Some((mut rtp, mut control)) = call.lanes.take() {
            rtp.joined_close().await;
            control.joined_close().await;
        }
        call.local = None;
        call.remote = None;
        let available = {
            let mut a = self.shared.admission.lock().expect("call admission");
            if a.generation == call.generation {
                a.call = None;
                a.contact = None;
                a.cancelled = false;
            }
            a.available && !a.stopped
        };
        call.phase = CallPhase::Ended;
        notify(
            &self.shared,
            &self.observer,
            call.snapshot(reason, available),
        );
        if available {
            publish_idle(&self.shared, &self.observer, CallPhase::Listening, true);
        }
    }
}

#[cfg(test)]
#[path = "calls_host_tests.rs"]
mod tests;
fn set_available(shared: &Shared, available: bool) {
    if let Ok(mut a) = shared.admission.lock() {
        a.available = available && !a.stopped;
    }
}
fn publish_idle(
    shared: &Shared,
    observer: &Arc<dyn CallObserver>,
    phase: CallPhase,
    available: bool,
) {
    set_available(shared, available);
    let available = shared
        .admission
        .lock()
        .map(|a| a.available)
        .unwrap_or(false);
    notify(
        shared,
        observer,
        CallSnapshot {
            call_id: String::new(),
            contact_id: String::new(),
            phase,
            direction: CallDirection::Outgoing,
            reason: CallEndReason::None,
            media_handle: 0,
            available,
        },
    );
}
fn notify(shared: &Shared, observer: &Arc<dyn CallObserver>, state: CallSnapshot) {
    // UniFFI converts an unexpected foreign callback exception into a panic.
    // Keep teardown on this actor so audio.stop's completion cannot be stranded.
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer.changed(state))).is_err() {
        if let Ok(mut admission) = shared.admission.lock() {
            admission.stopped = true;
            admission.available = false;
        }
    }
}
fn hex_id(id: [u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
fn parse_id(id: &str) -> Result<[u8; 16], FfiError> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(FfiError::InvalidInput);
    }
    let mut value = [0; 16];
    for (i, byte) in value.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&id[i * 2..i * 2 + 2], 16).map_err(|_| FfiError::InvalidInput)?;
    }
    if value == [0; 16] {
        return Err(FfiError::InvalidInput);
    }
    Ok(value)
}
fn failure_reason(error: &FfiError) -> Reason {
    match error {
        FfiError::Revoked | FfiError::NotEnrolled => Reason::Revoked,
        FfiError::Busy => Reason::Busy,
        FfiError::Transport(_) => Reason::ConnectionLost,
        FfiError::NotAccepted
        | FfiError::Blocked
        | FfiError::NoPeerPrekeys
        | FfiError::UnknownContact => Reason::Unavailable,
        _ => Reason::Invalid,
    }
}
fn end_reason(reason: Reason) -> CallEndReason {
    match reason {
        Reason::Ok => CallEndReason::None,
        Reason::Unavailable => CallEndReason::Unavailable,
        Reason::Busy => CallEndReason::Busy,
        Reason::Expired => CallEndReason::Expired,
        Reason::Rejected => CallEndReason::Rejected,
        Reason::LocalEnded => CallEndReason::LocalEnded,
        Reason::RemoteEnded => CallEndReason::RemoteEnded,
        Reason::ConnectionLost => CallEndReason::ConnectionLost,
        Reason::Invalid => CallEndReason::Invalid,
        Reason::Revoked => CallEndReason::Revoked,
        Reason::AudioUnavailable => CallEndReason::AudioUnavailable,
    }
}

//! Durable voice notes. Only the manifest is locally sealed; subordinate blob
//! rows contain routing/receipt metadata and immutable E2E ciphertext, no keys.
//! An opaque transfer has no Connection: advance owns one independent Noise
//! stream, while prepare/commit are short, separately serialized store calls.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use chacha20poly1305::{
    aead::{Aead, Payload},
    ChaCha20Poly1305, KeyInit, Nonce,
};
use dmsg_protocol::{
    blob,
    e2e::{Body, Event, VoiceManifest},
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::{
    chat::{contact_keys, recipient_binding, Core},
    contacts,
    history::{HistoryMessage, MessageDirection, MessageKind},
    olm::{self, OlmError},
    transport::{DirectTcp, Transport},
};

const CACHE_MAX: i64 = 32 * 1024 * 1024;
const LEASE_MS: i64 = 120_000;
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_DOMAIN: &[u8] = b"dmsg voice chunk v1\0";

pub(crate) fn decode_manifest(bytes: &[u8]) -> Result<(Event, VoiceManifest), String> {
    let event = dmsg_protocol::e2e::decode(bytes).map_err(str::to_owned)?;
    let Body::Voice(manifest) = &event.body else {
        return Err("not a voice manifest".into());
    };
    manifest.validate().map_err(str::to_owned)?;
    if manifest.plain_len as usize > 120 * 1024 {
        return Err("voice container too large".into());
    }
    Ok((event.clone(), manifest.clone()))
}

fn nonce(manifest: &VoiceManifest, index: u16) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[..8].copy_from_slice(&manifest.nonce_prefix);
    nonce[8..].copy_from_slice(&u32::from(index).to_be_bytes());
    nonce
}

fn chunk_aad(event: &Event, manifest: &VoiceManifest, index: u16) -> Result<Vec<u8>, String> {
    let mut aad = CHUNK_DOMAIN.to_vec();
    aad.extend_from_slice(&manifest.blob_id);
    aad.extend_from_slice(&event.message_id);
    aad.extend_from_slice(&event.sender_ed);
    aad.extend_from_slice(&manifest.recipient_binding);
    aad.extend_from_slice(&u32::from(index).to_be_bytes());
    aad.extend_from_slice(&u32::from(manifest.chunk_count().map_err(str::to_owned)?).to_be_bytes());
    aad.extend_from_slice(&manifest.plain_len.to_be_bytes());
    Ok(aad)
}

fn decrypt_chunk(
    event: &Event,
    manifest: &VoiceManifest,
    index: u16,
    bytes: &[u8],
) -> Result<Vec<u8>, String> {
    if blob::chunk_len(manifest.byte_len, index) != Some(bytes.len()) {
        return Err("voice chunk length mismatch".into());
    }
    ChaCha20Poly1305::new((&manifest.key).into())
        .decrypt(
            Nonce::from_slice(&nonce(manifest, index)),
            Payload {
                msg: bytes,
                aad: &chunk_aad(event, manifest, index)?,
            },
        )
        .map_err(|_| "voice chunk authentication failed".into())
}

fn finished_note(
    event: &Event,
    manifest: &VoiceManifest,
    chunks: &[Option<Vec<u8>>],
) -> Result<Vec<u8>, String> {
    if chunks.len() != usize::from(manifest.chunk_count().map_err(str::to_owned)?) {
        return Err("voice chunk count mismatch".into());
    }
    let mut bytes = Vec::with_capacity(manifest.plain_len as usize);
    for (index, chunk) in chunks.iter().enumerate() {
        bytes.extend_from_slice(&decrypt_chunk(
            event,
            manifest,
            index as u16,
            chunk.as_deref().ok_or("voice not downloaded")?,
        )?);
    }
    if bytes.len() != manifest.plain_len as usize {
        return Err("voice plaintext length mismatch".into());
    }
    let note = crate::voice_codec::parse(&bytes)?;
    if note.sample_count != manifest.sample_count || note.waveform != manifest.waveform {
        return Err("voice metadata mismatch".into());
    }
    Ok(bytes)
}

/// Same existing recipient epoch hash used by TEXT; no object checksum.
pub(crate) fn own_binding(conn: &Connection) -> Result<[u8; 32], String> {
    use sha2::{Digest, Sha256};
    let (user, _) = crate::store::load_account(conn)?.ok_or("voice account missing")?;
    let device =
        olm::device_pubkey(&crate::store::load_identity(conn)?.ok_or("voice identity missing")?);
    let (account, _) = olm::load_or_create(conn).map_err(|_| "voice account missing")?;
    let mut hash = Sha256::new();
    hash.update(b"dmsg recipient binding v1\0");
    hash.update(user);
    hash.update(device);
    hash.update(olm::ed_identity(&account));
    hash.update(olm::curve_identity(&account));
    Ok(hash.finalize().into())
}

pub(crate) fn insert_transfer(
    tx: &Transaction<'_>,
    id: i64,
    manifest: &VoiceManifest,
    recipient_device: &[u8; 32],
    downloaded: bool,
) -> Result<(), String> {
    tx.execute("INSERT INTO core_blob_transfers(local_id,blob_id,recipient_device,byte_len,chunk_count,downloaded,last_used_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,manifest.blob_id.as_slice(),recipient_device.as_slice(),manifest.byte_len,manifest.chunk_count().map_err(str::to_owned)?,downloaded,crate::history::local_time_ms()?]).map_err(|_| "voice transfer insert failed")?;
    Ok(())
}

impl Core {
    pub fn voice_session_ready(&self, cid: &str) -> Result<bool, OlmError> {
        let c = contacts::get(&self.conn, cid)?.ok_or(OlmError::UnknownContact)?;
        contacts::sendable(&c)?;
        let (_, _, ed, curve) = contact_keys(&c)?;
        match crate::store::load_session(&self.conn, cid).map_err(OlmError::Store)? {
            Some((_, old_ed, old_curve)) if ed == old_ed && curve == old_curve => Ok(true),
            Some(_) => Err(OlmError::IdentityMismatch),
            None => Ok(false),
        }
    }

    /// Stable MID is the attempt token. Repeated calls return the exact original
    /// row (including hidden rows) before touching audio or advancing a ratchet.
    pub fn queue_voice(
        &mut self,
        cid: &str,
        mid: &[u8; 16],
        encoded_note: &[u8],
        reply_to_local_id: Option<i64>,
    ) -> Result<HistoryMessage, OlmError> {
        let device_pub = self.device_pub();
        let sender_ed = olm::ed_identity(&self.account);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| OlmError::Store("voice queue transaction failed".into()))?;
        let old: Option<(i64,String,String,Option<Vec<u8>>)> = tx.query_row("SELECT local_id,contact_id,kind,reply_ref FROM core_messages WHERE direction='outgoing' AND message_id=?1",[mid.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|_|OlmError::Store("voice attempt lookup failed".into()))?;
        if let Some((id, old_cid, kind, old_reply)) = old {
            if old_cid != cid || kind != "voice" {
                return Err(OlmError::MessageUnavailable);
            }
            let requested = crate::store::reply_target(&tx, cid, reply_to_local_id, false)?;
            if old_reply != requested.as_ref().map(|r| r.encode().to_vec()) {
                return Err(OlmError::MessageUnavailable);
            }
            return crate::history::message_row(&tx, cid, id)
                .map_err(|_| OlmError::Store("voice attempt read failed".into()));
        }
        let reply_to = crate::store::reply_target(&tx, cid, reply_to_local_id, true)?;
        let note = crate::voice_codec::parse(encoded_note).map_err(|_| OlmError::BadVoice)?;
        let c = contacts::get(&tx, cid)?.ok_or(OlmError::UnknownContact)?;
        contacts::sendable(&c)?;
        let (_, device, ed, curve) = contact_keys(&c)?;
        let (pickle, old_ed, old_curve) = crate::store::load_session(&tx, cid)
            .map_err(OlmError::Store)?
            .ok_or(OlmError::VoiceSessionRequired)?;
        if ed != old_ed || curve != old_curve {
            return Err(OlmError::IdentityMismatch);
        }
        let plain_len = u32::try_from(note.bytes.len()).map_err(|_| OlmError::BadVoice)?;
        let count = plain_len.div_ceil(blob::CHUNK_PLAIN_MAX as u32);
        let mut manifest = VoiceManifest {
            blob_id: [0; 16],
            key: [0; 32],
            nonce_prefix: [0; 8],
            recipient_binding: recipient_binding(&c)?,
            plain_len,
            byte_len: plain_len
                .checked_add(count * 16)
                .ok_or(OlmError::BadVoice)?,
            sample_count: note.sample_count,
            waveform: note.waveform,
        };
        getrandom::fill(&mut manifest.blob_id).map_err(|_| OlmError::Crypto("rng"))?;
        getrandom::fill(&mut manifest.key).map_err(|_| OlmError::Crypto("rng"))?;
        getrandom::fill(&mut manifest.nonce_prefix).map_err(|_| OlmError::Crypto("rng"))?;
        let event = Event {
            message_id: *mid,
            sender_ed,
            reply_to,
            body: Body::Voice(manifest.clone()),
        };
        let envelope = dmsg_protocol::e2e::encode(&event).map_err(OlmError::Protocol)?;
        let mut sessions = olm::unpickle_sessions(&pickle)?;
        let wire = olm::encode_wire(
            &sessions[0]
                .encrypt(&envelope)
                .map_err(|_| OlmError::Crypto("voice encrypt"))?,
        );
        // Include the Olm envelope and SEND_MEDIA routing metadata in 128KiB.
        if wire.len() > dmsg_protocol::CIPHERTEXT_MAX
            || manifest.byte_len as usize + wire.len() + 80 > blob::VOICE_MAX
        {
            return Err(OlmError::BadVoice);
        }
        let id = crate::store::insert_outgoing(
            &tx,
            cid,
            &device_pub,
            &manifest.recipient_binding,
            &event,
            &wire,
        )
        .map_err(OlmError::Store)?;
        insert_transfer(&tx, id, &manifest, &device, true).map_err(OlmError::Store)?;
        let cipher = ChaCha20Poly1305::new((&manifest.key).into());
        for (index, plain) in note.bytes.chunks(blob::CHUNK_PLAIN_MAX).enumerate() {
            let index = index as u16;
            let bytes = cipher
                .encrypt(
                    Nonce::from_slice(&nonce(&manifest, index)),
                    Payload {
                        msg: plain,
                        aad: &chunk_aad(&event, &manifest, index).map_err(OlmError::Store)?,
                    },
                )
                .map_err(|_| OlmError::Crypto("voice chunk encrypt"))?;
            tx.execute(
                "INSERT INTO core_blob_chunks(local_id,chunk_index,ciphertext) VALUES(?1,?2,?3)",
                params![id, index, bytes],
            )
            .map_err(|_| OlmError::Store("voice chunk insert failed".into()))?;
        }
        crate::store::save_session(&tx, cid, &olm::pickle_sessions(&sessions)?, &ed, &curve)
            .map_err(OlmError::Store)?;
        let row = crate::history::message_row(&tx, cid, id)
            .map_err(|_| OlmError::Store("voice queue projection failed".into()))?;
        tx.commit()
            .map_err(|_| OlmError::Store("voice queue commit failed".into()))?;
        Ok(row)
    }
}

pub(crate) fn upload_complete(conn: &Connection, mid: &[u8; 16]) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM core_blob_transfers b JOIN core_messages m USING(local_id) WHERE m.message_id=?1 AND m.direction='outgoing' AND b.upload_complete=1)",[mid.as_slice()],|r|r.get(0)).map_err(|_|"voice upload status failed".into())
}

pub(crate) fn send_media_binding(
    conn: &Connection,
    mid: &[u8; 16],
) -> Result<([u8; 16], [u8; 32]), String> {
    let (blob,device):(Vec<u8>,Vec<u8>)=conn.query_row("SELECT b.blob_id,b.recipient_device FROM core_blob_transfers b JOIN core_messages m USING(local_id) WHERE m.message_id=?1 AND m.direction='outgoing' AND b.upload_complete=1",[mid.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"voice upload incomplete")?;
    Ok((
        blob.try_into().map_err(|_| "invalid voice blob id")?,
        device.try_into().map_err(|_| "invalid voice recipient")?,
    ))
}

pub(crate) fn purge_deleted(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM core_blob_chunks WHERE local_id=?1", [id])
        .map_err(|_| "voice cache delete failed")?;
    // Accepted outgoing events still reconcile their original SEND_MEDIA status.
    // Retain only the keyless, frozen routing metadata; the manifest/key and all
    // playback ciphertext are gone. Incoming tombstones have no transfer row.
    conn.execute(
        "UPDATE core_blob_transfers SET downloaded=0,active_until_ms=0 WHERE local_id=?1",
        [id],
    )
    .map_err(|_| "voice transfer delete failed")?;
    conn.execute("DELETE FROM core_blob_transfers WHERE local_id=?1 AND NOT EXISTS(SELECT 1 FROM core_messages m WHERE m.local_id=?1 AND m.direction='outgoing' AND m.delivery_state IN ('accepted','delivered'))", [id])
        .map_err(|_| "voice transfer delete failed")?;
    Ok(())
}

pub(crate) fn purge_hidden_sent(conn: &Connection) -> Result<(), String> {
    // Keep keyless routing metadata for accepted SEND_MEDIA reconciliation.
    conn.execute("DELETE FROM core_blob_chunks WHERE local_id IN (SELECT local_id FROM core_messages WHERE kind='voice' AND hidden_self=1 AND delivery_state!='queued')",[]).map_err(|_|"hidden voice cache cleanup failed")?;
    conn.execute("UPDATE core_blob_transfers SET downloaded=0,active_until_ms=0 WHERE local_id IN (SELECT local_id FROM core_messages WHERE kind='voice' AND hidden_self=1 AND delivery_state!='queued')",[]).map_err(|_|"hidden voice cache cleanup failed")?;
    conn.execute("UPDATE core_messages SET media_manifest=NULL WHERE kind='voice' AND hidden_self=1 AND delivery_state!='queued'",[]).map_err(|_|"hidden voice manifest cleanup failed")?;
    Ok(())
}

fn trim_cache(conn: &Connection, clear: bool) -> Result<(), String> {
    let now = crate::history::local_time_ms()?;
    let mut total: i64 = conn
        .query_row(
            "SELECT coalesce(sum(length(ciphertext)),0) FROM core_blob_chunks",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "voice cache size failed")?;
    let ids: Vec<i64> = {
        let mut stmt=conn.prepare("SELECT b.local_id FROM core_blob_transfers b JOIN core_messages m USING(local_id) WHERE b.active_until_ms<=?1 AND NOT(m.direction='outgoing' AND m.delivery_state='queued') ORDER BY b.last_used_ms,b.local_id").map_err(|_|"voice cache list failed")?;
        let rows = stmt
            .query_map([now], |r| r.get(0))
            .map_err(|_| "voice cache list failed")?;
        rows.collect::<Result<_, _>>()
            .map_err(|_| "voice cache list failed")?
    };
    for id in ids {
        if !clear && total <= CACHE_MAX {
            break;
        }
        let size:i64=conn.query_row("SELECT coalesce(sum(length(ciphertext)),0) FROM core_blob_chunks WHERE local_id=?1",[id],|r|r.get(0)).map_err(|_|"voice cache size failed")?;
        conn.execute("DELETE FROM core_blob_chunks WHERE local_id=?1", [id])
            .map_err(|_| "voice cache evict failed")?;
        conn.execute(
            "UPDATE core_blob_transfers SET downloaded=0 WHERE local_id=?1",
            [id],
        )
        .map_err(|_| "voice cache evict failed")?;
        total -= size;
    }
    Ok(())
}

pub(crate) fn clear_cache(conn: &Connection) -> Result<(), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "voice cache transaction failed")?;
    trim_cache(&tx, true)?;
    tx.commit().map_err(|_| "voice cache commit failed".into())
}

pub(crate) fn pending_upload(conn: &Connection) -> Result<Option<HistoryMessage>, String> {
    let row:Option<(i64,String)>=conn.query_row("SELECT m.local_id,m.contact_id FROM core_messages m JOIN core_blob_transfers b USING(local_id) JOIN core_contacts c ON c.contact_id=m.contact_id WHERE m.direction='outgoing' AND m.kind='voice' AND m.delivery_state='queued' AND m.deleted_all=0 AND b.upload_complete=0 AND c.state IN ('accepted','accepted_server','inviting') AND c.seen_user IS NULL AND c.seen_device IS NULL AND c.seen_ed IS NULL AND c.seen_curve IS NULL ORDER BY m.local_id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|"voice queue lookup failed")?;
    row.map(|(id, cid)| {
        crate::history::message_row(conn, &cid, id).map_err(|_| "voice queue read failed".into())
    })
    .transpose()
}

pub(crate) fn data(conn: &Connection, cid: &str, id: i64) -> Result<Vec<u8>, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "voice data transaction failed")?;
    let row = crate::history::message_row(&tx, cid, id).map_err(|_| "voice message unavailable")?;
    if row.kind != MessageKind::Voice
        || row.hidden_self
        || row.deleted_all
        || !row.voice.is_some_and(|v| v.downloaded)
    {
        return Err("voice not downloaded".into());
    }
    let bytes:Vec<u8>=tx.query_row("SELECT dmsg_unseal('voice_manifest',media_manifest) FROM core_messages WHERE local_id=?1",[id],|r|r.get(0)).map_err(|_|"voice manifest unavailable")?;
    let (event, manifest) = decode_manifest(&bytes)?;
    let chunks = load_chunks(&tx, id, manifest.chunk_count().map_err(str::to_owned)?)?;
    let note = finished_note(&event, &manifest, &chunks)?;
    tx.execute(
        "UPDATE core_blob_transfers SET last_used_ms=?2 WHERE local_id=?1",
        params![id, crate::history::local_time_ms()?],
    )
    .map_err(|_| "voice cache touch failed")?;
    tx.commit().map_err(|_| "voice data commit failed")?;
    Ok(note)
}

fn load_chunks(conn: &Connection, id: i64, count: u16) -> Result<Vec<Option<Vec<u8>>>, String> {
    let mut chunks = vec![None; usize::from(count)];
    let mut stmt=conn.prepare("SELECT chunk_index,ciphertext FROM core_blob_chunks WHERE local_id=?1 ORDER BY chunk_index").map_err(|_|"voice chunks lookup failed")?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((r.get::<_, usize>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|_| "voice chunks lookup failed")?;
    for row in rows {
        let (index, bytes) = row.map_err(|_| "voice chunk read failed")?;
        *chunks.get_mut(index).ok_or("voice chunk index invalid")? = Some(bytes);
    }
    Ok(chunks)
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VoiceTransferProgress {
    pub transferred: u32,
    pub total: u32,
    pub complete: bool,
}

pub(crate) enum Endpoint {
    Direct {
        addr: String,
        server_pub: [u8; 32],
        domain: Vec<u8>,
    },
    Dns {
        owner: String,
        profile: crate::dns::Profile,
    },
}

struct Snapshot {
    owner: String,
    cid: String,
    id: i64,
    contact_binding: [u8; 32],
    manifest_bytes: Vec<u8>,
    event: Event,
    manifest: VoiceManifest,
    device_priv: [u8; 32],
    account: ([u8; 16], String),
    download: bool,
}

#[derive(Clone, Copy)]
enum Phase {
    Status,
    Reserve,
    Chunks,
    Finish,
    Done,
}
// Only Rust can construct or consume these authenticated stream receipts.
struct Receipt {
    bitmap: u64,
    complete: bool,
    chunk: Option<(u16, Vec<u8>)>,
}
struct Network {
    transport: Option<DirectTcp>,
    phase: Phase,
    bitmap: u64,
    chunks: Vec<Option<Vec<u8>>>,
    receipt: Option<Receipt>,
}
struct TransferState {
    runtime: tokio::runtime::Runtime,
    network: Network,
}

#[derive(uniffi::Object)]
pub struct VoiceTransfer {
    snapshot: Snapshot,
    endpoint: Endpoint,
    state: Mutex<TransferState>,
    cancelled: AtomicBool,
    cancel_signal: tokio::sync::Notify,
}

pub(crate) fn prepare(
    conn: &Connection,
    owner: &str,
    cid: &str,
    id: i64,
    download: bool,
    endpoint: Endpoint,
) -> Result<Arc<VoiceTransfer>, OlmError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| OlmError::Transport("voice runtime unavailable".into()))?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| OlmError::Store("voice prepare transaction failed".into()))?;
    let row =
        crate::history::message_row(&tx, cid, id).map_err(|_| OlmError::MessageUnavailable)?;
    if row.kind != MessageKind::Voice
        || row.deleted_all
        || (download && row.hidden_self)
        || (!download && row.direction != MessageDirection::Outgoing)
    {
        return Err(OlmError::MessageUnavailable);
    }
    let c = contacts::get(&tx, cid)?.ok_or(OlmError::UnknownContact)?;
    contacts::sendable(&c)?;
    let contact_binding = recipient_binding(&c)?;
    let bytes:Vec<u8>=tx.query_row("SELECT dmsg_unseal('voice_manifest',media_manifest) FROM core_messages WHERE local_id=?1",[id],|r|r.get(0)).map_err(|_|OlmError::MessageUnavailable)?;
    let (event, manifest) = decode_manifest(&bytes).map_err(OlmError::Store)?;
    if (!download && manifest.recipient_binding != contact_binding)
        || (row.direction == MessageDirection::Incoming
            && manifest.recipient_binding != own_binding(&tx).map_err(OlmError::Store)?)
    {
        return Err(OlmError::MessageUnavailable);
    }
    let count = manifest.chunk_count().map_err(OlmError::Protocol)?;
    let chunks = load_chunks(&tx, id, count).map_err(OlmError::Store)?;
    if !download && chunks.iter().any(Option::is_none) {
        return Err(OlmError::MessageUnavailable);
    }
    // Existing download chunks were individually authenticated before commit.
    let bitmap = if download {
        chunks.iter().enumerate().fold(
            0,
            |mask, (i, c)| if c.is_some() { mask | (1 << i) } else { mask },
        )
    } else {
        0
    };
    let device_priv = crate::store::load_identity(&tx)
        .map_err(OlmError::Store)?
        .ok_or(OlmError::NotEnrolled)?;
    let account = crate::store::load_account(&tx)
        .map_err(OlmError::Store)?
        .ok_or(OlmError::NotEnrolled)?;
    if tx
        .execute(
            "UPDATE core_blob_transfers SET active_until_ms=?2 WHERE local_id=?1",
            params![
                id,
                crate::history::local_time_ms().map_err(OlmError::Store)? + LEASE_MS
            ],
        )
        .map_err(|_| OlmError::Store("voice lease failed".into()))?
        != 1
    {
        return Err(OlmError::MessageUnavailable);
    }
    tx.commit()
        .map_err(|_| OlmError::Store("voice prepare commit failed".into()))?;
    Ok(Arc::new(VoiceTransfer {
        snapshot: Snapshot {
            owner: owner.into(),
            cid: cid.into(),
            id,
            contact_binding,
            manifest_bytes: bytes,
            event,
            manifest,
            device_priv,
            account,
            download,
        },
        endpoint,
        state: Mutex::new(TransferState {
            runtime,
            network: Network {
                transport: None,
                phase: Phase::Status,
                bitmap,
                chunks,
                receipt: None,
            },
        }),
        cancelled: AtomicBool::new(false),
        cancel_signal: tokio::sync::Notify::new(),
    }))
}

impl VoiceTransfer {
    fn progress(&self, n: &Network) -> VoiceTransferProgress {
        let transferred = (0..n.chunks.len())
            .filter(|i| n.bitmap & (1 << i) != 0)
            .map(|i| blob::chunk_len(self.snapshot.manifest.byte_len, i as u16).unwrap_or(0) as u32)
            .sum();
        VoiceTransferProgress {
            transferred,
            total: self.snapshot.manifest.byte_len,
            complete: matches!(n.phase, Phase::Done),
        }
    }

    async fn request(
        &self,
        n: &mut Network,
        opcode: u8,
        payload: &[u8],
    ) -> Result<(u8, Vec<u8>), OlmError> {
        if n.transport.is_none() {
            let (addr, server, domain) = match &self.endpoint {
                Endpoint::Direct {
                    addr,
                    server_pub,
                    domain,
                } => (addr.clone(), *server_pub, domain.clone()),
                Endpoint::Dns { owner, profile } => (
                    crate::dns::endpoint(owner, profile)
                        .map_err(|_| OlmError::Transport("voice DNS unavailable".into()))?
                        .to_string(),
                    profile.noise_pubkey,
                    profile.domain.as_bytes().to_vec(),
                ),
            };
            let mut t = crate::transport::initiate_with_key(
                &addr,
                &server,
                &domain,
                &self.snapshot.device_priv,
            )
            .await
            .map_err(|_| OlmError::Transport("voice pinned channel failed".into()))?;
            t.send_frame(dmsg_protocol::OP_RESUME, &[])
                .await
                .map_err(|_| OlmError::Transport("voice resume failed".into()))?;
            let (op, p) = t
                .recv_frame()
                .await
                .map_err(|_| OlmError::Transport("voice resume failed".into()))?;
            if op == dmsg_protocol::OP_ERROR && p.len() == 1 {
                return Err(OlmError::Auth(crate::auth::map_error(p[0], false)));
            }
            let a = dmsg_protocol::auth::parse_authenticated(&p)
                .map_err(|_| OlmError::Protocol("voice resume response"))?;
            if op != dmsg_protocol::OP_AUTHENTICATED
                || (a.user_id, a.contact_id) != self.snapshot.account
            {
                return Err(OlmError::Protocol("voice account mismatch"));
            }
            n.transport = Some(t);
        }
        let t = n
            .transport
            .as_mut()
            .ok_or(OlmError::Protocol("voice stream missing"))?;
        t.send_frame(opcode, payload)
            .await
            .map_err(|_| OlmError::Transport("voice send failed".into()))?;
        t.recv_frame()
            .await
            .map_err(|_| OlmError::Transport("voice receive failed".into()))
    }

    async fn step(&self, n: &mut Network) -> Result<(), OlmError> {
        let m = &self.snapshot.manifest;
        let all = blob::full_bitmap(m.byte_len).ok_or(OlmError::BadVoice)?;
        let (opcode, payload) = match n.phase {
            Phase::Done => return Ok(()),
            Phase::Status => (
                dmsg_protocol::OP_BLOB_STATUS,
                blob::build_status(&m.blob_id),
            ),
            Phase::Reserve => (
                dmsg_protocol::OP_BLOB_RESERVE,
                blob::build_reserve(&m.blob_id, m.byte_len).ok_or(OlmError::BadVoice)?,
            ),
            Phase::Finish => (
                dmsg_protocol::OP_BLOB_FINISH,
                blob::build_finish(&m.blob_id),
            ),
            Phase::Chunks => {
                let index = (0..n.chunks.len())
                    .find(|i| n.bitmap & (1 << i) == 0)
                    .ok_or(OlmError::Protocol("voice chunks complete"))?
                    as u16;
                if self.snapshot.download {
                    (
                        dmsg_protocol::OP_BLOB_GET,
                        blob::build_get(&m.blob_id, index).ok_or(OlmError::BadVoice)?,
                    )
                } else {
                    (
                        dmsg_protocol::OP_BLOB_PUT,
                        blob::build_put(
                            &m.blob_id,
                            index,
                            n.chunks[index as usize]
                                .as_deref()
                                .ok_or(OlmError::MessageUnavailable)?,
                        )
                        .ok_or(OlmError::BadVoice)?,
                    )
                }
            }
        };
        let (op, p) = self.request(n, opcode, &payload).await?;
        if op == dmsg_protocol::OP_ERROR && p.len() == 1 {
            if matches!(n.phase, Phase::Status)
                && !self.snapshot.download
                && p[0] == dmsg_protocol::ERR_BAD
            {
                n.phase = Phase::Reserve;
                return Ok(());
            }
            return Err(match p[0] {
                dmsg_protocol::ERR_QUOTA => OlmError::Quota,
                dmsg_protocol::ERR_REVOKED => OlmError::Revoked,
                dmsg_protocol::ERR_BUSY => OlmError::Busy,
                dmsg_protocol::ERR_BAD => OlmError::Bad,
                c => OlmError::Server(c),
            });
        }
        let mut chunk = None;
        match n.phase {
            Phase::Status => {
                if op != dmsg_protocol::OP_BLOB_STATUS_RESP {
                    return Err(OlmError::Protocol("voice status opcode"));
                }
                let s = blob::parse_status_response(&p)
                    .ok_or(OlmError::Protocol("voice status invalid"))?;
                if s.blob_id != m.blob_id || s.byte_len != m.byte_len {
                    return Err(OlmError::Protocol("voice status mismatch"));
                }
                if self.snapshot.download {
                    if s.state != blob::State::Complete {
                        return Err(OlmError::MessageUnavailable);
                    }
                    if n.bitmap == all {
                        finished_note(&self.snapshot.event, m, &n.chunks)
                            .map_err(|_| OlmError::BadVoice)?;
                        n.phase = Phase::Done;
                    } else {
                        n.phase = Phase::Chunks;
                    }
                } else {
                    n.bitmap = s.bitmap;
                    n.phase = if s.state == blob::State::Complete {
                        Phase::Done
                    } else if n.bitmap == all {
                        Phase::Finish
                    } else {
                        Phase::Chunks
                    };
                }
            }
            Phase::Reserve => {
                if op != dmsg_protocol::OP_BLOB_RESERVED
                    || blob::parse_reserved(&p) != Some(m.blob_id)
                {
                    return Err(OlmError::Protocol("voice reservation mismatch"));
                }
                n.bitmap = 0;
                n.phase = Phase::Chunks;
            }
            Phase::Chunks => {
                let index = (0..n.chunks.len())
                    .find(|i| n.bitmap & (1 << i) == 0)
                    .ok_or(OlmError::Protocol("voice chunk index"))?
                    as u16;
                if self.snapshot.download {
                    if op != dmsg_protocol::OP_BLOB_DATA {
                        return Err(OlmError::Protocol("voice data opcode"));
                    }
                    let c = blob::parse_data(&p).ok_or(OlmError::Protocol("voice data invalid"))?;
                    if c.blob_id != &m.blob_id || c.index != index {
                        return Err(OlmError::Protocol("voice data mismatch"));
                    }
                    decrypt_chunk(&self.snapshot.event, m, index, c.bytes)
                        .map_err(|_| OlmError::BadVoice)?;
                    n.chunks[index as usize] = Some(c.bytes.to_vec());
                    chunk = Some((index, c.bytes.to_vec()));
                } else if op != dmsg_protocol::OP_BLOB_PUT_ACK
                    || blob::parse_put_ack(&p) != Some((m.blob_id, index))
                {
                    return Err(OlmError::Protocol("voice put receipt mismatch"));
                }
                n.bitmap |= 1 << index;
                if n.bitmap == all {
                    if self.snapshot.download {
                        finished_note(&self.snapshot.event, m, &n.chunks)
                            .map_err(|_| OlmError::BadVoice)?;
                        n.phase = Phase::Done;
                    } else {
                        n.phase = Phase::Finish;
                    }
                }
            }
            Phase::Finish => {
                if op != dmsg_protocol::OP_BLOB_FINISH_ACK
                    || blob::parse_finish_ack(&p) != Some(m.blob_id)
                {
                    return Err(OlmError::Protocol("voice finish receipt mismatch"));
                }
                n.phase = Phase::Done;
            }
            Phase::Done => (),
        }
        n.receipt = Some(Receipt {
            bitmap: n.bitmap,
            complete: matches!(n.phase, Phase::Done),
            chunk,
        });
        Ok(())
    }
}

#[uniffi::export]
impl VoiceTransfer {
    /// Exactly one blob request/receipt. The same runtime/Noise stream survives
    /// all steps. Commit is required before issuing another network operation.
    pub fn advance(&self) -> Result<VoiceTransferProgress, crate::ffi::FfiError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| crate::ffi::FfiError::Store("voice handle poisoned".into()))?;
        if self.cancelled.load(Ordering::Acquire) {
            return Err(crate::ffi::FfiError::MessageUnavailable);
        }
        if state.network.receipt.is_some() {
            return Err(crate::ffi::FfiError::Busy);
        }
        let TransferState { runtime, network } = &mut *state;
        let result=runtime.block_on(async {
            tokio::select! {
                biased;
                _=self.cancel_signal.notified()=>Err(OlmError::MessageUnavailable),
                result=tokio::time::timeout(STEP_TIMEOUT,self.step(network))=>result.unwrap_or_else(|_|Err(OlmError::Transport("voice step timeout".into()))),
            }
        });
        if result.is_err() || self.cancelled.load(Ordering::Acquire) {
            if let Some(mut t) = network.transport.take() {
                runtime.block_on(t.close());
            }
            self.cancelled.store(true, Ordering::Release);
        }
        result.map_err(crate::ffi::map_olm)?;
        if self.cancelled.load(Ordering::Acquire) {
            return Err(crate::ffi::FfiError::MessageUnavailable);
        }
        Ok(self.progress(network))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel_signal.notify_one();
        if let Ok(mut state) = self.state.try_lock() {
            let TransferState { runtime, network } = &mut *state;
            if let Some(mut t) = network.transport.take() {
                runtime.block_on(t.close());
            }
        }
    }
}

pub(crate) fn commit(
    conn: &Connection,
    owner: &str,
    transfer: &VoiceTransfer,
) -> Result<VoiceTransferProgress, OlmError> {
    if owner != transfer.snapshot.owner || transfer.cancelled.load(Ordering::Acquire) {
        return Err(OlmError::MessageUnavailable);
    }
    let mut state = transfer
        .state
        .lock()
        .map_err(|_| OlmError::Store("voice handle poisoned".into()))?;
    if transfer.cancelled.load(Ordering::Acquire) {
        return Err(OlmError::MessageUnavailable);
    }
    let n = &mut state.network;
    let s = &transfer.snapshot;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| OlmError::Store("voice commit transaction failed".into()))?;
    let row =
        crate::history::message_row(&tx, &s.cid, s.id).map_err(|_| OlmError::MessageUnavailable)?;
    if row.deleted_all || (s.download && row.hidden_self) {
        return Err(OlmError::MessageUnavailable);
    }
    let c = contacts::get(&tx, &s.cid)?.ok_or(OlmError::UnknownContact)?;
    contacts::sendable(&c)?;
    if recipient_binding(&c)? != s.contact_binding {
        return Err(OlmError::MessageUnavailable);
    }
    let current:Option<Vec<u8>>=tx.query_row("SELECT CASE WHEN media_manifest IS NULL THEN NULL ELSE dmsg_unseal('voice_manifest',media_manifest) END FROM core_messages WHERE local_id=?1",[s.id],|r|r.get(0)).map_err(|_|OlmError::Store("voice commit manifest read failed".into()))?;
    if current.as_deref() != Some(s.manifest_bytes.as_slice()) {
        return Err(OlmError::MessageUnavailable);
    }
    let receipt = n.receipt.as_ref();
    if let Some(receipt) = receipt {
        if let Some((index, bytes)) = &receipt.chunk {
            let old: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT ciphertext FROM core_blob_chunks WHERE local_id=?1 AND chunk_index=?2",
                    params![s.id, index],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| OlmError::Store("voice commit chunk lookup failed".into()))?;
            if old.as_deref().is_some_and(|old| old != bytes) {
                return Err(OlmError::Protocol("voice immutable chunk changed"));
            }
            tx.execute("INSERT INTO core_blob_chunks(local_id,chunk_index,ciphertext,confirmed) VALUES(?1,?2,?3,1) ON CONFLICT(local_id,chunk_index) DO UPDATE SET confirmed=1",params![s.id,index,bytes]).map_err(|_|OlmError::Store("voice download receipt failed".into()))?;
        }
        if !s.download {
            for index in 0..n.chunks.len() {
                tx.execute(
                    "UPDATE core_blob_chunks SET confirmed=?3 WHERE local_id=?1 AND chunk_index=?2",
                    params![s.id, index, receipt.bitmap & (1 << index) != 0],
                )
                .map_err(|_| OlmError::Store("voice upload receipt failed".into()))?;
            }
        }
        tx.execute("UPDATE core_blob_transfers SET upload_complete=max(upload_complete,?2),downloaded=max(downloaded,?3),last_used_ms=?4,active_until_ms=?5 WHERE local_id=?1 AND blob_id=?6",params![s.id,!s.download && receipt.complete,s.download && receipt.complete,crate::history::local_time_ms().map_err(OlmError::Store)?,if receipt.complete {0}else{crate::history::local_time_ms().map_err(OlmError::Store)?+LEASE_MS},s.manifest.blob_id.as_slice()]).map_err(|_|OlmError::Store("voice receipt update failed".into()))?;
    }
    trim_cache(&tx, false).map_err(OlmError::Store)?;
    if transfer.cancelled.load(Ordering::Acquire) {
        return Err(OlmError::MessageUnavailable);
    }
    tx.commit()
        .map_err(|_| OlmError::Store("voice receipt commit failed".into()))?;
    n.receipt = None;
    Ok(transfer.progress(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    const CID: &str = "222222222222";

    fn fixture(name: &str) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dmsg-voice9-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::store::open_encrypted(&dir.join("core.db"), &[44; 32]).unwrap();
        crate::store::save_identity(&conn, &[45; 32]).unwrap();
        crate::store::save_account(&conn, &[46; 16], "111111111111").unwrap();
        contacts::request_add(&conn, CID).unwrap();
        (conn, dir)
    }
    fn event() -> Event {
        Event {
            reply_to: None,
            message_id: [1; 16],
            sender_ed: [2; 32],
            body: Body::Voice(VoiceManifest {
                blob_id: [3; 16],
                key: [4; 32],
                nonce_prefix: [5; 8],
                recipient_binding: [6; 32],
                plain_len: 9000,
                byte_len: 9032,
                sample_count: 16_000,
                waveform: vec![7; 64],
            }),
        }
    }
    #[test]
    fn chunks_authenticate_mid_author_epoch_geometry_and_order_without_checksum() {
        let event = event();
        let Body::Voice(m) = &event.body else {
            unreachable!()
        };
        let plain = vec![9; blob::CHUNK_PLAIN_MAX];
        let cipher = ChaCha20Poly1305::new((&m.key).into())
            .encrypt(
                Nonce::from_slice(&nonce(m, 0)),
                Payload {
                    msg: &plain,
                    aad: &chunk_aad(&event, m, 0).unwrap(),
                },
            )
            .unwrap();
        assert_eq!(decrypt_chunk(&event, m, 0, &cipher).unwrap(), plain);
        assert!(decrypt_chunk(&event, m, 1, &cipher).is_err());
        let mut changed = event.clone();
        changed.message_id[0] ^= 1;
        assert!(decrypt_chunk(&changed, m, 0, &cipher).is_err());
        changed = event.clone();
        changed.sender_ed[0] ^= 1;
        assert!(decrypt_chunk(&changed, m, 0, &cipher).is_err());
        let mut changed = m.clone();
        changed.recipient_binding[0] ^= 1;
        assert!(decrypt_chunk(&event, &changed, 0, &cipher).is_err());
        changed = m.clone();
        changed.plain_len += 1;
        changed.byte_len += 1;
        assert!(decrypt_chunk(&event, &changed, 0, &cipher).is_err());
        let mut bad = cipher.clone();
        bad[10] ^= 1;
        assert!(decrypt_chunk(&event, m, 0, &bad).is_err());
        assert!(decrypt_chunk(&event, m, 0, &cipher[..cipher.len() - 1]).is_err());
        assert!(finished_note(&event, m, &[Some(cipher), None]).is_err());
    }

    #[test]
    fn pending_delete_wins_voice_and_pending_edit_is_ignored_but_dedup_survives() {
        for delete in [false, true] {
            let (mut conn, dir) = fixture(if delete {
                "pending-delete"
            } else {
                "pending-edit"
            });
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let control = Event {
                reply_to: None,
                message_id: [8; 16],
                sender_ed: [2; 32],
                body: if delete {
                    Body::Delete {
                        target: [1; 16],
                        revision: 2,
                    }
                } else {
                    Body::Edit {
                        target: [1; 16],
                        revision: 20,
                        text: "must never become voice text".into(),
                    }
                },
            };
            crate::store::receive_event(&tx, CID, &[10; 32], &control, None).unwrap();
            let id = crate::store::receive_event(
                &tx,
                CID,
                &[10; 32],
                &event(),
                Some(dmsg_protocol::chronology::Order {
                    seq: 1,
                    timestamp_ms: 1000,
                }),
            )
            .unwrap()
            .unwrap();
            let row = crate::history::message_row(&tx, CID, id).unwrap();
            assert_eq!(row.kind, MessageKind::Voice);
            assert!(row.text.is_empty());
            assert_eq!(row.deleted_all, delete);
            assert_eq!(row.voice.is_none(), delete);
            assert_eq!(row.revision, if delete { 2 } else { 0 });
            let late_edit = Event {
                reply_to: None,
                message_id: [9; 16],
                sender_ed: [2; 32],
                body: Body::Edit {
                    target: [1; 16],
                    revision: 30,
                    text: "still no voice edit".into(),
                },
            };
            assert!(crate::store::valid_control_target(&tx, CID, &[10; 32], &late_edit).unwrap());
            crate::store::receive_event(&tx, CID, &[10; 32], &late_edit, None).unwrap();
            assert_eq!(crate::history::message_row(&tx, CID, id).unwrap(), row);
            assert!(crate::store::durable_event(&tx, &[10; 32], &control.message_id).unwrap());
            assert!(crate::store::durable_event(&tx, &[10; 32], &late_edit.message_id).unwrap());
            let edit_texts: i64 = tx
                .query_row(
                    "SELECT count(*) FROM core_messages WHERE kind='edit' AND text IS NOT NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(edit_texts, 0);
            tx.commit().unwrap();
            drop(conn);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn cache_lru_is_bounded_and_clear_preserves_queued_and_active_chunks() {
        let (mut conn, dir) = fixture("cache-lru");
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut ids = Vec::new();
        let now = crate::history::local_time_ms().unwrap();
        for index in 0..274u64 {
            let mut e = event();
            e.message_id[..8].copy_from_slice(&(index + 1).to_be_bytes());
            let Body::Voice(m) = &mut e.body else {
                unreachable!()
            };
            m.blob_id = e.message_id;
            m.plain_len = 120 * 1024;
            m.byte_len = m.plain_len + m.plain_len.div_ceil(blob::CHUNK_PLAIN_MAX as u32) * 16;
            let m = m.clone();
            let id = if index == 1 {
                let id = crate::store::insert_outgoing(
                    &tx,
                    CID,
                    &[10; 32],
                    &[11; 32],
                    &e,
                    b"opaque Olm",
                )
                .unwrap();
                insert_transfer(&tx, id, &m, &[12; 32], true).unwrap();
                id
            } else {
                crate::store::receive_event(
                    &tx,
                    CID,
                    &[10; 32],
                    &e,
                    Some(dmsg_protocol::chronology::Order {
                        seq: index as i64 + 1,
                        timestamp_ms: 1000,
                    }),
                )
                .unwrap()
                .unwrap()
            };
            tx.execute("UPDATE core_blob_transfers SET downloaded=1,last_used_ms=?2,active_until_ms=?3 WHERE local_id=?1",params![id,index+1,if index==0 { now+LEASE_MS } else { 0 }]).unwrap();
            for chunk in 0..m.chunk_count().unwrap() {
                tx.execute("INSERT INTO core_blob_chunks(local_id,chunk_index,ciphertext,confirmed) VALUES(?1,?2,?3,1)",params![id,chunk,vec![7u8;blob::chunk_len(m.byte_len,chunk).unwrap()]]).unwrap();
            }
            ids.push(id);
        }
        trim_cache(&tx, false).unwrap();
        let size: i64 = tx
            .query_row(
                "SELECT sum(length(ciphertext)) FROM core_blob_chunks",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(size <= CACHE_MAX);
        let has_chunks = |id: i64| {
            tx.query_row::<bool, _, _>(
                "SELECT EXISTS(SELECT 1 FROM core_blob_chunks WHERE local_id=?1)",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(has_chunks(ids[0]) && has_chunks(ids[1]));
        assert!(!has_chunks(ids[2]) && !has_chunks(ids[3]));
        assert!(has_chunks(ids[4]) && has_chunks(ids[273]));
        trim_cache(&tx, true).unwrap();
        assert!(has_chunks(ids[0]) && has_chunks(ids[1]));
        assert_eq!(
            tx.query_row::<i64, _, _>(
                "SELECT count(DISTINCT local_id) FROM core_blob_chunks",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            tx.query_row::<i64, _, _>(
                "SELECT count(*) FROM core_blob_transfers WHERE downloaded=1",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            2
        );
        tx.commit().unwrap();
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

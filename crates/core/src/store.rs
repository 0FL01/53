//! Fresh-only schema10. One durable row per text, voice or control event.
//! Production creates sealed storage directly; no schema or plaintext conversion.
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};

pub const SCHEMA_VERSION: i64 = 10;

const SCHEMA: &str = "
CREATE TABLE core_identity(id INTEGER PRIMARY KEY CHECK(id=1),device_priv BLOB NOT NULL);
CREATE TABLE core_account(id INTEGER PRIMARY KEY CHECK(id=1),user_id BLOB NOT NULL,contact_id TEXT NOT NULL);
CREATE TABLE core_olm(id INTEGER PRIMARY KEY CHECK(id=1),pickle BLOB NOT NULL,next_key_id INTEGER NOT NULL);
CREATE TABLE core_sessions(contact_id TEXT PRIMARY KEY,pickle BLOB NOT NULL,peer_ed BLOB NOT NULL,peer_curve BLOB NOT NULL);
CREATE TABLE core_contacts(
 contact_id TEXT PRIMARY KEY,user_id BLOB,device_key BLOB,ed_identity BLOB,curve_identity BLOB,state TEXT NOT NULL,
 seen_user BLOB,seen_device BLOB,seen_ed BLOB,seen_curve BLOB,local_alias BLOB,
 read_cursor INTEGER NOT NULL DEFAULT 0 CHECK(read_cursor>=0),local_activity_ms INTEGER NOT NULL DEFAULT 0 CHECK(local_activity_ms>=0));
CREATE UNIQUE INDEX core_contact_user ON core_contacts(user_id) WHERE user_id IS NOT NULL;
CREATE INDEX core_dialog_activity ON core_contacts(local_activity_ms DESC,contact_id ASC);
CREATE TABLE core_dns_profile(id INTEGER PRIMARY KEY CHECK(id=1),profile BLOB NOT NULL);
CREATE TABLE core_messages(
 local_id INTEGER PRIMARY KEY AUTOINCREMENT,
 message_id BLOB NOT NULL CHECK(length(message_id)=16),sender_device BLOB NOT NULL CHECK(length(sender_device)=32),
 contact_id TEXT NOT NULL,direction TEXT NOT NULL CHECK(direction IN ('incoming','outgoing')),
  kind TEXT NOT NULL CHECK(kind IN ('text','voice','edit','delete')),target_mid BLOB,reply_ref BLOB,
 revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0),text BLOB,media_manifest BLOB,ciphertext BLOB,recipient_binding BLOB,
 delivery_state TEXT,local_timestamp_ms INTEGER NOT NULL CHECK(local_timestamp_ms>=0),
 server_seq INTEGER CHECK(server_seq>0),server_timestamp_ms INTEGER CHECK(server_timestamp_ms>=0),
 hidden_self INTEGER NOT NULL DEFAULT 0 CHECK(hidden_self IN (0,1)),deleted_all INTEGER NOT NULL DEFAULT 0 CHECK(deleted_all IN (0,1)),
  UNIQUE(sender_device,message_id),
  CHECK(reply_ref IS NULL OR (kind IN ('text','voice') AND typeof(reply_ref)='blob' AND length(reply_ref)=48)),
 CHECK((kind IN ('text','voice') AND target_mid IS NULL) OR (kind IN ('edit','delete') AND target_mid IS NOT NULL AND length(target_mid)=16 AND revision>0)),
 CHECK((direction='incoming' AND delivery_state IS NULL AND ciphertext IS NULL AND recipient_binding IS NULL)
    OR (direction='outgoing' AND delivery_state IS NOT NULL AND delivery_state IN ('queued','accepted','delivered') AND ciphertext IS NOT NULL AND recipient_binding IS NOT NULL AND length(recipient_binding)=32)),
 CHECK((server_seq IS NULL)=(server_timestamp_ms IS NULL)),
 CHECK(kind IN ('text','voice') OR (server_seq IS NULL AND hidden_self=0 AND deleted_all=0)),
 CHECK(kind NOT IN ('text','voice') OR ((direction='outgoing' AND delivery_state='queued') OR server_seq IS NOT NULL)),
 CHECK(kind!='delete' OR text IS NULL),
 CHECK(kind!='text' OR ((hidden_self=0 AND deleted_all=0 AND text IS NOT NULL) OR ((hidden_self=1 OR deleted_all=1) AND text IS NULL))),
 CHECK(kind='voice' OR media_manifest IS NULL),
 CHECK(kind!='voice' OR (text IS NULL AND (media_manifest IS NOT NULL OR hidden_self=1 OR deleted_all=1))),
 CHECK(deleted_all=0 OR media_manifest IS NULL));
CREATE UNIQUE INDEX core_messages_outgoing ON core_messages(message_id) WHERE direction='outgoing';
CREATE UNIQUE INDEX core_messages_server_seq ON core_messages(server_seq) WHERE server_seq IS NOT NULL;
CREATE INDEX core_messages_contact ON core_messages(contact_id,local_id DESC) WHERE kind IN ('text','voice');
CREATE INDEX core_messages_timeline ON core_messages(contact_id,(server_seq IS NULL),coalesce(server_seq,local_id)) WHERE kind IN ('text','voice');
CREATE INDEX core_messages_pending ON core_messages(contact_id,sender_device,target_mid,revision DESC) WHERE kind IN ('edit','delete');
CREATE INDEX core_messages_queue ON core_messages(local_id) WHERE direction='outgoing' AND delivery_state IN ('queued','accepted');
CREATE TABLE core_blob_transfers(
 local_id INTEGER PRIMARY KEY REFERENCES core_messages(local_id),blob_id BLOB NOT NULL UNIQUE CHECK(length(blob_id)=16),
 recipient_device BLOB NOT NULL CHECK(length(recipient_device)=32),
 byte_len INTEGER NOT NULL CHECK(byte_len>0 AND byte_len<=131072),chunk_count INTEGER NOT NULL CHECK(chunk_count>0 AND chunk_count<=17),
 upload_complete INTEGER NOT NULL DEFAULT 0 CHECK(upload_complete IN (0,1)),downloaded INTEGER NOT NULL DEFAULT 0 CHECK(downloaded IN (0,1)),
 last_used_ms INTEGER NOT NULL CHECK(last_used_ms>=0),active_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(active_until_ms>=0));
CREATE TABLE core_blob_chunks(
 local_id INTEGER NOT NULL REFERENCES core_blob_transfers(local_id),chunk_index INTEGER NOT NULL CHECK(chunk_index>=0 AND chunk_index<17),
 ciphertext BLOB NOT NULL CHECK(length(ciphertext)>16 AND length(ciphertext)<=8192),
 confirmed INTEGER NOT NULL DEFAULT 0 CHECK(confirmed IN (0,1)),PRIMARY KEY(local_id,chunk_index));
";

/// Plain mode exists only for Rust-only isolated harnesses. It cannot open a
/// sealed database or convert a previously initialized plain database.
pub fn open(path: &std::path::Path) -> Result<Connection, String> {
    open_mode(path, None)
}

pub fn open_encrypted(path: &std::path::Path, key: &[u8]) -> Result<Connection, String> {
    let key = key.try_into().map_err(|_| "storage key must be 32 bytes")?;
    open_mode(path, Some(key))
}

/// Validate before any writes, and again under the initialization lock. Returning
/// true means an actually empty, unversioned store, not an unsupported old store.
fn validate(conn: &Connection, key: &Option<[u8; 32]>) -> Result<bool, String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| "schema version lookup failed")?;
    let nonempty: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name NOT LIKE 'sqlite_%')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "schema lookup failed")?;
    if version == 0 && !nonempty {
        return Ok(true);
    }
    if version != SCHEMA_VERSION {
        return Err("unsupported core schema".into());
    }
    let marker: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_storage')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "storage marker lookup failed")?;
    match (key, marker) {
        (Some(k), true) => {
            let (version, verifier): (i64, Vec<u8>) = conn
                .query_row(
                    "SELECT version,verifier FROM core_storage WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| "storage marker corrupt")?;
            if version != 1 {
                return Err("unsupported storage format".into());
            }
            crate::secure::verify(k, &verifier)?;
        }
        (None, true) => return Err("encrypted storage requires key".into()),
        (Some(_), false) => {
            return Err("encrypted storage marker missing; conversion is unsupported".into())
        }
        (None, false) => {
            // A deleted marker is not permission to reinterpret sealed values.
            for (table, column) in SEALED_FIELDS {
                let sealed: bool = conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE substr({column},1,7)=x'444d53472d5331')"), [], |r| r.get(0)).map_err(|_| "storage format lookup failed")?;
                if sealed {
                    return Err("storage marker missing for encrypted values".into());
                }
            }
        }
    }
    conn.prepare("SELECT local_id,message_id,sender_device,kind,target_mid,reply_ref,revision,text,media_manifest,ciphertext,recipient_binding,hidden_self,deleted_all,server_seq FROM core_messages LIMIT 0").map_err(|_| "invalid current message schema")?;
    conn.prepare("SELECT blob_id,recipient_device,byte_len,chunk_count,upload_complete,downloaded,last_used_ms,active_until_ms FROM core_blob_transfers LIMIT 0").map_err(|_| "invalid current blob schema")?;
    conn.prepare("SELECT local_id,chunk_index,ciphertext,confirmed FROM core_blob_chunks LIMIT 0")
        .map_err(|_| "invalid current chunk schema")?;
    Ok(false)
}

const SEALED_FIELDS: [(&str, &str); 7] = [
    ("core_identity", "device_priv"),
    ("core_olm", "pickle"),
    ("core_sessions", "pickle"),
    ("core_messages", "text"),
    ("core_messages", "media_manifest"),
    ("core_dns_profile", "profile"),
    ("core_contacts", "local_alias"),
];

fn open_mode(path: &std::path::Path, key: Option<[u8; 32]>) -> Result<Connection, String> {
    if path.exists() {
        let probe = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| "storage compatibility probe failed")?;
        validate(&probe, &key)?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|_| "storage directory failed")?;
    }
    let mut conn = Connection::open(path).map_err(|_| "storage open failed")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "storage timeout setup failed")?;
    let fresh = validate(&conn, &key)?;
    crate::secure::register(&conn, key)?;
    if fresh {
        conn.execute_batch("PRAGMA synchronous=FULL;")
            .map_err(|_| "storage durability setup failed")?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| "schema transaction failed")?;
        if validate(&tx, &key)? {
            tx.execute_batch(SCHEMA)
                .map_err(|_| "create schema10 failed")?;
            if let Some(k) = key {
                tx.execute_batch("CREATE TABLE core_storage(id INTEGER PRIMARY KEY CHECK(id=1),version INTEGER NOT NULL,verifier BLOB NOT NULL);").map_err(|_| "storage marker creation failed")?;
                tx.execute(
                    "INSERT INTO core_storage VALUES(1,1,?1)",
                    [crate::secure::verifier(&k)?],
                )
                .map_err(|_| "storage marker creation failed")?;
                for (table, column) in SEALED_FIELDS {
                    for action in ["INSERT", "UPDATE"] {
                        tx.execute_batch(&format!("CREATE TRIGGER {table}_{column}_sealed_{action} BEFORE {action} ON {table}
                         WHEN NEW.{column} IS NOT NULL AND (typeof(NEW.{column})!='blob' OR substr(NEW.{column},1,7)!=x'444d53472d5331')
                         BEGIN SELECT RAISE(ABORT,'unencrypted storage write'); END;")).map_err(|_| "storage guard creation failed")?;
                    }
                }
            }
            tx.execute_batch("PRAGMA user_version=10;")
                .map_err(|_| "schema version update failed")?;
        }
        tx.commit().map_err(|_| "schema commit failed")?;
    }
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .map_err(|_| "storage durability setup failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "storage chmod failed")?;
    }
    Ok(conn)
}

pub fn save_identity(conn: &Connection, privkey: &[u8; 32]) -> Result<(), String> {
    if load_identity(conn)?.is_some_and(|old| old != *privkey) {
        return Err("identity cannot be replaced".into());
    }
    conn.execute("INSERT INTO core_identity VALUES(1,dmsg_seal('device_priv',?1)) ON CONFLICT(id) DO NOTHING",[privkey.as_slice()]).map_err(|_| "save identity failed")?;
    if load_identity(conn)? != Some(*privkey) {
        return Err("save identity: no row".into());
    }
    Ok(())
}

pub fn load_identity(conn: &Connection) -> Result<Option<[u8; 32]>, String> {
    let value: Option<Vec<u8>> = conn
        .query_row(
            "SELECT dmsg_unseal('device_priv',device_priv) FROM core_identity WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "load identity failed")?;
    value
        .map(|v| {
            v.try_into()
                .map_err(|_| "invalid device identity length".into())
        })
        .transpose()
}

pub fn save_account(conn: &Connection, user_id: &[u8; 16], contact_id: &str) -> Result<(), String> {
    dmsg_protocol::auth::build_authenticated(user_id, contact_id)
        .map_err(|_| "invalid account contact id")?;
    conn.execute(
        "INSERT INTO core_account VALUES(1,?1,?2) ON CONFLICT(id) DO NOTHING",
        params![user_id.as_slice(), contact_id],
    )
    .map_err(|_| "save account failed")?;
    if load_account(conn)? != Some((*user_id, contact_id.into())) {
        return Err("account cannot be replaced".into());
    }
    Ok(())
}

pub fn load_account(conn: &Connection) -> Result<Option<([u8; 16], String)>, String> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT user_id,contact_id FROM core_account WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| "load account failed")?;
    row.map(|(uid, cid)| {
        let uid = uid.try_into().map_err(|_| "invalid account user id")?;
        dmsg_protocol::auth::build_authenticated(&uid, &cid)
            .map_err(|_| "invalid account contact id")?;
        Ok((uid, cid))
    })
    .transpose()
}

pub fn save_olm(conn: &Connection, pickle: &str, next_key_id: u32) -> Result<(), String> {
    conn.execute("INSERT INTO core_olm VALUES(1,dmsg_seal('olm_pickle',?1),?2) ON CONFLICT(id) DO UPDATE SET pickle=excluded.pickle,next_key_id=excluded.next_key_id",params![pickle,next_key_id]).map_err(|_| "save olm failed")?;
    Ok(())
}

pub fn load_olm(conn: &Connection) -> Result<Option<(String, u32)>, String> {
    conn.query_row("SELECT CAST(dmsg_unseal('olm_pickle',pickle) AS TEXT),next_key_id FROM core_olm WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_| "load olm failed".into())
}

pub fn save_session(
    conn: &Connection,
    contact_id: &str,
    pickle: &str,
    peer_ed: &[u8; 32],
    peer_curve: &[u8; 32],
) -> Result<(), String> {
    conn.execute("INSERT INTO core_sessions VALUES(?1,dmsg_seal('session_pickle',?2),?3,?4) ON CONFLICT(contact_id) DO UPDATE SET pickle=excluded.pickle,peer_ed=excluded.peer_ed,peer_curve=excluded.peer_curve",params![contact_id,pickle,peer_ed.as_slice(),peer_curve.as_slice()]).map_err(|_| "save session failed")?;
    Ok(())
}

pub fn load_session(
    conn: &Connection,
    contact_id: &str,
) -> Result<Option<(String, [u8; 32], [u8; 32])>, String> {
    let row: Option<(String,Vec<u8>,Vec<u8>)> = conn.query_row("SELECT CAST(dmsg_unseal('session_pickle',pickle) AS TEXT),peer_ed,peer_curve FROM core_sessions WHERE contact_id=?1",[contact_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_| "load session failed")?;
    row.map(|(p, e, c)| {
        Ok((
            p,
            e.try_into().map_err(|_| "invalid session ed key")?,
            c.try_into().map_err(|_| "invalid session curve key")?,
        ))
    })
    .transpose()
}

pub fn delete_session(conn: &Connection, contact_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM core_sessions WHERE contact_id=?1",
        [contact_id],
    )
    .map_err(|_| "delete session failed")?;
    Ok(())
}

pub mod outbox_status {
    pub const QUEUED: &str = "queued";
    pub const ACCEPTED: &str = "accepted";
    pub const DELIVERED: &str = "delivered";
}

/// Local selector becomes a sender-relative wire identity. Duplicate attempts
/// use retained BASE metadata, independent of visibility or current pins.
pub(crate) fn reply_target(
    conn: &Connection,
    cid: &str,
    local_id: Option<i64>,
    require_visible: bool,
) -> Result<Option<dmsg_protocol::e2e::ReplyRef>, crate::olm::OlmError> {
    use crate::olm::OlmError;
    let Some(id) = local_id else {
        return Ok(None);
    };
    if id <= 0 {
        return Err(OlmError::MessageUnavailable);
    }
    let row: Option<(Vec<u8>, Vec<u8>, bool)> = conn.query_row(
        "SELECT sender_device,message_id,hidden_self=0 AND deleted_all=0 FROM core_messages WHERE local_id=?1 AND contact_id=?2 AND kind IN ('text','voice')",
        params![id,cid], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional().map_err(|_|OlmError::Store("reply target lookup failed".into()))?;
    let (sender, mid, visible) = row.ok_or(OlmError::MessageUnavailable)?;
    if require_visible && !visible {
        return Err(OlmError::MessageUnavailable);
    }
    Ok(Some(dmsg_protocol::e2e::ReplyRef {
        sender_device: sender
            .try_into()
            .map_err(|_| OlmError::Store("reply sender corrupt".into()))?,
        message_id: mid
            .try_into()
            .map_err(|_| OlmError::Store("reply MID corrupt".into()))?,
    }))
}

pub(crate) fn insert_outgoing(
    tx: &Transaction<'_>,
    contact_id: &str,
    sender: &[u8; 32],
    binding: &[u8; 32],
    event: &dmsg_protocol::e2e::Event,
    wire: &[u8],
) -> Result<i64, String> {
    use dmsg_protocol::e2e::Body;
    let (kind, target, revision, text) = match &event.body {
        Body::Text(text) => ("text", None, 0, Some(text.as_str())),
        Body::Voice(_) => ("voice", None, 0, None),
        Body::Edit {
            target, revision, ..
        } => ("edit", Some(target.as_slice()), *revision, None),
        Body::Delete { target, revision } => ("delete", Some(target.as_slice()), *revision, None),
    };
    let manifest = if kind == "voice" {
        Some(dmsg_protocol::e2e::encode(event).map_err(str::to_owned)?)
    } else {
        None
    };
    let now = crate::history::local_time_ms()?;
    let reply = event.reply_to.as_ref().map(|r| r.encode());
    tx.execute("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,target_mid,revision,text,ciphertext,recipient_binding,delivery_state,local_timestamp_ms,media_manifest,reply_ref)
      VALUES(?1,?2,?3,'outgoing',?4,?5,?6,CASE WHEN ?7 IS NULL THEN NULL ELSE dmsg_seal('message_text',?7) END,?8,?9,'queued',?10,CASE WHEN ?11 IS NULL THEN NULL ELSE dmsg_seal('voice_manifest',?11) END,?12)",params![event.message_id.as_slice(),sender.as_slice(),contact_id,kind,target,revision,text,wire,binding.as_slice(),now,manifest,reply.as_ref().map(|r|r.as_slice())]).map_err(|_| "outgoing event insert failed")?;
    let id = tx.last_insert_rowid();
    if matches!(kind, "text" | "voice") {
        activity(tx, contact_id, now)?;
    }
    Ok(id)
}

fn activity(tx: &Transaction<'_>, contact_id: &str, now: i64) -> Result<(), String> {
    if tx.execute("UPDATE core_contacts SET local_activity_ms=max(local_activity_ms,?2) WHERE contact_id=?1",params![contact_id,now]).map_err(|_| "dialog activity update failed")? != 1 {
        return Err("message contact missing".into());
    }
    Ok(())
}

pub(crate) fn durable_event(
    conn: &Connection,
    sender: &[u8; 32],
    mid: &[u8; 16],
) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM core_messages WHERE sender_device=?1 AND message_id=?2)",
        params![sender.as_slice(), mid.as_slice()],
        |r| r.get(0),
    )
    .map_err(|_| "event dedup failed".into())
}

pub(crate) fn outgoing_binding(
    conn: &Connection,
    mid: &[u8; 16],
) -> Result<(String, [u8; 32]), String> {
    let (kind,bytes): (String,Vec<u8>) = conn.query_row("SELECT kind,recipient_binding FROM core_messages WHERE message_id=?1 AND direction='outgoing'",[mid.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_| "outgoing binding missing")?;
    Ok((
        kind,
        bytes.try_into().map_err(|_| "invalid outgoing binding")?,
    ))
}

/// Control authorization is scoped to its authenticated author and dialog. A
/// known foreign/control target is invalid; an unknown text may arrive later.
pub(crate) fn valid_control_target(
    conn: &Connection,
    cid: &str,
    sender: &[u8; 32],
    event: &dmsg_protocol::e2e::Event,
) -> Result<bool, String> {
    use dmsg_protocol::e2e::Body;
    let target = match &event.body {
        Body::Text(_) | Body::Voice(_) => return Ok(true),
        Body::Edit { target, .. } | Body::Delete { target, .. } => target,
    };
    if target == &event.message_id {
        return Ok(false);
    }
    let known: Option<(String,String,String)> = conn.query_row("SELECT contact_id,direction,kind FROM core_messages WHERE sender_device=?1 AND message_id=?2",params![sender.as_slice(),target.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_| "control target lookup failed")?;
    // EDIT against a voice is journaled as an ignored control, retaining dedup.
    Ok(known.is_none_or(|(c, d, k)| {
        c == cid && d == "incoming" && matches!(k.as_str(), "text" | "voice")
    }))
}

/// Journal the event and project it under the caller's crypto transaction.
/// Only TEXT returns a bubble ID. NULL bodies never enter dmsg_unseal.
pub(crate) fn receive_event(
    tx: &Transaction<'_>,
    cid: &str,
    sender: &[u8; 32],
    event: &dmsg_protocol::e2e::Event,
    order: Option<dmsg_protocol::chronology::Order>,
) -> Result<Option<i64>, String> {
    use dmsg_protocol::e2e::Body;
    let now = crate::history::local_time_ms()?;
    let reply = event.reply_to.as_ref().map(|r| r.encode());
    match &event.body {
        Body::Text(original) => {
            let order = order.ok_or("text metadata missing")?;
            let deleted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND target_mid=?3 AND kind='delete')",params![cid,sender.as_slice(),event.message_id.as_slice()],|r|r.get(0)).map_err(|_| "pending delete lookup failed")?;
            let winning: Option<(u64,Option<String>)> = tx.query_row("SELECT revision,CASE WHEN text IS NULL THEN NULL ELSE CAST(dmsg_unseal('message_text',text) AS TEXT) END FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND target_mid=?3 AND kind IN ('edit','delete') ORDER BY revision DESC,local_id ASC LIMIT 1",params![cid,sender.as_slice(),event.message_id.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_| "pending edit lookup failed")?;
            let revision = winning.as_ref().map_or(0, |r| r.0);
            let text = if deleted {
                None
            } else {
                Some(
                    winning
                        .as_ref()
                        .and_then(|r| r.1.as_deref())
                        .unwrap_or(original),
                )
            };
            tx.execute("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,revision,text,local_timestamp_ms,server_seq,server_timestamp_ms,deleted_all,reply_ref)
             VALUES(?1,?2,?3,'incoming','text',?4,CASE WHEN ?5 IS NULL THEN NULL ELSE dmsg_seal('message_text',?5) END,?6,?7,?8,?9,?10)",params![event.message_id.as_slice(),sender.as_slice(),cid,revision,text,now,order.seq,order.timestamp_ms,deleted,reply.as_ref().map(|r|r.as_slice())]).map_err(|_| "incoming text insert failed")?;
            let id = tx.last_insert_rowid();
            clear_controls(tx, cid, sender, &event.message_id)?;
            activity(tx, cid, now)?;
            Ok(Some(id))
        }
        Body::Voice(manifest) => {
            let order = order.ok_or("voice metadata missing")?;
            let revision: u64 = tx.query_row("SELECT coalesce(max(revision),0) FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND target_mid=?3 AND kind='delete'",params![cid,sender.as_slice(),event.message_id.as_slice()],|r|r.get(0)).map_err(|_| "pending delete lookup failed")?;
            let deleted = revision > 0;
            let sealed = if deleted {
                None
            } else {
                Some(dmsg_protocol::e2e::encode(event).map_err(str::to_owned)?)
            };
            tx.execute("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,revision,media_manifest,local_timestamp_ms,server_seq,server_timestamp_ms,deleted_all,reply_ref) VALUES(?1,?2,?3,'incoming','voice',?4,CASE WHEN ?5 IS NULL THEN NULL ELSE dmsg_seal('voice_manifest',?5) END,?6,?7,?8,?9,?10)",params![event.message_id.as_slice(),sender.as_slice(),cid,revision,sealed,now,order.seq,order.timestamp_ms,deleted,reply.as_ref().map(|r|r.as_slice())]).map_err(|_| "incoming voice insert failed")?;
            let id = tx.last_insert_rowid();
            if !deleted {
                let device_priv = load_identity(tx)?.ok_or("voice recipient identity missing")?;
                crate::voice::insert_transfer(
                    tx,
                    id,
                    manifest,
                    &crate::olm::device_pubkey(&device_priv),
                    false,
                )?;
            }
            clear_controls(tx, cid, sender, &event.message_id)?;
            activity(tx, cid, now)?;
            Ok(Some(id))
        }
        Body::Edit {
            target,
            revision,
            text,
        } => {
            let terminal: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND ((message_id=?3 AND (kind='voice' OR (kind='text' AND (deleted_all=1 OR hidden_self=1)))) OR (target_mid=?3 AND kind='delete')))",params![cid,sender.as_slice(),target.as_slice()],|r|r.get(0)).map_err(|_| "terminal target lookup failed")?;
            let current: u64 = tx.query_row("SELECT coalesce(max(revision),0) FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND (message_id=?3 OR target_mid=?3)",params![cid,sender.as_slice(),target.as_slice()],|r|r.get(0)).map_err(|_| "target revision lookup failed")?;
            let winning = *revision > current && !terminal;
            tx.execute("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,target_mid,revision,text,local_timestamp_ms)
              VALUES(?1,?2,?3,'incoming','edit',?4,?5,CASE WHEN ?6 IS NULL THEN NULL ELSE dmsg_seal('message_text',?6) END,?7)",params![event.message_id.as_slice(),sender.as_slice(),cid,target.as_slice(),revision,if winning {Some(text.as_str())} else {None},now]).map_err(|_| "incoming edit insert failed")?;
            if *revision > current {
                let changed = tx.execute("UPDATE core_messages SET revision=?4,text=CASE WHEN hidden_self=1 OR deleted_all=1 THEN NULL ELSE dmsg_seal('message_text',?5) END WHERE contact_id=?1 AND sender_device=?2 AND message_id=?3 AND kind='text' AND deleted_all=0",params![cid,sender.as_slice(),target.as_slice(),revision,text]).map_err(|_| "edit projection failed")?;
                if changed == 1 || terminal {
                    clear_controls(tx, cid, sender, target)?;
                } else {
                    tx.execute("UPDATE core_messages SET text=NULL WHERE contact_id=?1 AND sender_device=?2 AND target_mid=?3 AND kind='edit' AND revision<?4",params![cid,sender.as_slice(),target.as_slice(),revision]).map_err(|_| "pending edit cleanup failed")?;
                }
            }
            Ok(None)
        }
        Body::Delete { target, revision } => {
            tx.execute("INSERT INTO core_messages(message_id,sender_device,contact_id,direction,kind,target_mid,revision,local_timestamp_ms) VALUES(?1,?2,?3,'incoming','delete',?4,?5,?6)",params![event.message_id.as_slice(),sender.as_slice(),cid,target.as_slice(),revision,now]).map_err(|_| "incoming delete insert failed")?;
            tx.execute("UPDATE core_messages SET deleted_all=1,text=NULL,media_manifest=NULL,revision=max(revision,?4) WHERE contact_id=?1 AND sender_device=?2 AND message_id=?3 AND kind IN ('text','voice')",params![cid,sender.as_slice(),target.as_slice(),revision]).map_err(|_| "delete projection failed")?;
            tx.execute("DELETE FROM core_blob_chunks WHERE local_id IN (SELECT local_id FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND message_id=?3 AND kind='voice')",params![cid,sender.as_slice(),target.as_slice()]).map_err(|_| "deleted voice cache cleanup failed")?;
            tx.execute("DELETE FROM core_blob_transfers WHERE local_id IN (SELECT local_id FROM core_messages WHERE contact_id=?1 AND sender_device=?2 AND message_id=?3 AND kind='voice')",params![cid,sender.as_slice(),target.as_slice()]).map_err(|_| "deleted voice transfer cleanup failed")?;
            clear_controls(tx, cid, sender, target)?;
            Ok(None)
        }
    }
}

pub(crate) fn clear_controls(
    tx: &Transaction<'_>,
    cid: &str,
    sender: &[u8; 32],
    target: &[u8; 16],
) -> Result<(), String> {
    tx.execute("UPDATE core_messages SET text=NULL WHERE contact_id=?1 AND sender_device=?2 AND target_mid=?3 AND kind='edit'",params![cid,sender.as_slice(),target.as_slice()]).map_err(|_| "control body cleanup failed")?;
    Ok(())
}

pub fn outbox_set_status(conn: &Connection, mid: &[u8; 16], status: &str) -> Result<(), String> {
    outbox_set_status_order(conn, mid, status, None)
}

pub(crate) fn outbox_set_status_order(
    conn: &Connection,
    mid: &[u8; 16],
    status: &str,
    order: Option<dmsg_protocol::chronology::Order>,
) -> Result<(), String> {
    if !matches!(status, "queued" | "accepted" | "delivered") {
        return Err("invalid delivery state".into());
    }
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "delivery transaction failed")?;
    let (kind,seq,time): (String,Option<i64>,Option<i64>) = tx.query_row("SELECT kind,server_seq,server_timestamp_ms FROM core_messages WHERE message_id=?1 AND direction='outgoing'",[mid.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_| "outgoing event missing")?;
    let (seq, time) = if matches!(kind.as_str(), "text" | "voice") {
        if let Some(o) = order {
            if o.seq <= 0
                || o.timestamp_ms < 0
                || seq.is_some_and(|s| s != o.seq)
                || time.is_some_and(|t| t != o.timestamp_ms)
            {
                return Err("server order changed or invalid".into());
            }
            (Some(o.seq), Some(o.timestamp_ms))
        } else {
            (seq, time)
        }
    } else {
        if order.is_some() {
            return Err("control chronology unsupported".into());
        }
        (None, None)
    };
    tx.execute("UPDATE core_messages SET delivery_state=CASE WHEN delivery_state='delivered' OR ?2='delivered' THEN 'delivered' WHEN delivery_state='accepted' OR ?2='accepted' THEN 'accepted' ELSE 'queued' END,server_seq=?3,server_timestamp_ms=?4 WHERE message_id=?1 AND direction='outgoing'",params![mid.as_slice(),status,seq,time]).map_err(|_| "delivery update failed")?;
    crate::voice::purge_hidden_sent(&tx)?;
    tx.commit().map_err(|_| "delivery commit failed".into())
}

pub fn outbox_queued(
    conn: &Connection,
    cursor: i64,
    limit: usize,
) -> Result<(Vec<(i64, [u8; 16], String, Vec<u8>, String)>, Option<i64>), String> {
    let limit = limit.clamp(1, 100);
    let mut stmt=conn.prepare("SELECT local_id,message_id,contact_id,ciphertext,delivery_state FROM core_messages WHERE direction='outgoing' AND local_id>?1 AND delivery_state IN ('queued','accepted') ORDER BY local_id LIMIT ?2").map_err(|_| "outbox list failed")?;
    let rows = stmt
        .query_map(params![cursor, (limit + 1) as i64], |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })
        .map_err(|_| "outbox list failed")?;
    let mut out = vec![];
    for row in rows {
        let (id, mid, cid, ct, status) = row.map_err(|_| "outbox row failed")?;
        out.push((
            id,
            mid.try_into().map_err(|_| "outbox MID invalid")?,
            cid,
            ct,
            status,
        ));
    }
    let next = if out.len() > limit {
        out.pop();
        out.last().map(|r| r.0)
    } else {
        None
    };
    Ok((out, next))
}

pub fn inbox_count(conn: &Connection) -> Result<i64, String> {
    conn.query_row(
        "SELECT count(*) FROM core_messages WHERE direction='incoming' AND kind IN ('text','voice')",
        [],
        |r| r.get(0),
    )
    .map_err(|_| "inbox count failed".into())
}

pub fn inbox_list(
    conn: &Connection,
    cursor: i64,
    limit: usize,
) -> Result<(Vec<(i64, String, String)>, Option<i64>), String> {
    let limit = limit.clamp(1, 100);
    let mut stmt=conn.prepare("SELECT server_seq,contact_id,CAST(dmsg_unseal('message_text',text) AS TEXT) FROM core_messages WHERE direction='incoming' AND kind='text' AND hidden_self=0 AND deleted_all=0 AND server_seq>?1 ORDER BY server_seq LIMIT ?2").map_err(|_| "inbox list failed")?;
    let rows = stmt
        .query_map(params![cursor, (limit + 1) as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|_| "inbox list failed")?;
    let mut out = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "inbox row failed")?;
    let next = if out.len() > limit {
        out.pop();
        out.last().map(|r| r.0)
    } else {
        None
    };
    Ok((out, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: [u8; 32] = [63; 32];
    fn path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dmsg-store9-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("core.db")
    }
    fn cleanup(p: &std::path::Path) {
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn unsupported_old_future_and_nonempty_unversioned_stores_are_unchanged() {
        for version in [0, 1, 5, 6, 7, 8, 9, 11, 100] {
            let p = path(&format!("reject-{version}"));
            let c = Connection::open(&p).unwrap();
            c.execute_batch(&format!("CREATE TABLE sentinel(v TEXT); INSERT INTO sentinel VALUES('preserve'); PRAGMA user_version={version};")).unwrap();
            drop(c);
            let before = std::fs::read(&p).unwrap();
            assert!(open(&p).is_err());
            assert!(open_encrypted(&p, &KEY).is_err());
            assert_eq!(std::fs::read(&p).unwrap(), before);
            cleanup(&p);
        }
    }

    #[test]
    fn fresh_encryption_first_write_guards_key_and_marker_are_fail_closed() {
        let p = path("encrypted");
        let c = open_encrypted(&p, &KEY).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            10
        );
        for table in ["core_history", "core_inbox", "core_outbox"] {
            assert_eq!(
                c.query_row::<i64, _, _>(
                    "SELECT count(*) FROM sqlite_master WHERE name=?1",
                    [table],
                    |r| r.get(0)
                )
                .unwrap(),
                0
            );
        }
        save_identity(&c, &[27; 32]).unwrap();
        save_account(&c, &[28; 16], "ABCD1234EFGH").unwrap();
        let (account, _) = crate::olm::load_or_create(&c).unwrap();
        crate::olm::persist(&c, &account, u32::MAX).unwrap();
        save_session(&c, "ABCD1234EFGH", "fixture-session", &[29; 32], &[30; 32]).unwrap();
        let raw: Vec<u8> = c
            .query_row("SELECT device_priv FROM core_identity", [], |r| r.get(0))
            .unwrap();
        assert!(raw.starts_with(b"DMSG-S1"));
        assert_ne!(raw, [27; 32]);
        assert!(c
            .execute(
                "UPDATE core_identity SET device_priv=?1",
                [[27; 32].as_slice()]
            )
            .is_err());
        assert!(save_identity(&c, &[31; 32]).is_err());
        drop(c);
        assert!(open(&p).is_err());
        assert!(open_encrypted(&p, &[62; 32]).is_err());
        assert!(open_encrypted(&p, &[0; 31]).is_err());
        let c = open_encrypted(&p, &KEY).unwrap();
        assert_eq!(load_identity(&c).unwrap(), Some([27; 32]));
        assert_eq!(load_olm(&c).unwrap().unwrap().1, u32::MAX);
        assert_eq!(
            crate::olm::load_or_create(&c).unwrap().0.identity_keys(),
            account.identity_keys()
        );
        assert_eq!(
            load_session(&c, "ABCD1234EFGH").unwrap().unwrap().0,
            "fixture-session"
        );
        c.execute_batch("DELETE FROM core_storage;").unwrap();
        drop(c);
        assert!(open_encrypted(&p, &KEY).is_err());
        let c = Connection::open(&p).unwrap();
        c.execute_batch("DROP TABLE core_storage;").unwrap();
        drop(c);
        assert!(open_encrypted(&p, &KEY).is_err());
        assert!(open(&p).is_err());
        cleanup(&p);
    }

    #[test]
    fn initialized_plain_store_is_never_converted() {
        let p = path("plain");
        let c = open(&p).unwrap();
        save_identity(&c, &[9; 32]).unwrap();
        drop(c);
        let before = std::fs::read(&p).unwrap();
        assert!(open_encrypted(&p, &KEY).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        let c = open(&p).unwrap();
        assert_eq!(load_identity(&c).unwrap(), Some([9; 32]));
        let mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert!(save_account(&c, &[0; 16], "short").is_err());
        save_account(&c, &[2; 16], "ABCD1234EFGH").unwrap();
        assert!(save_account(&c, &[3; 16], "ABCD1234EFGH").is_err());
        c.execute("UPDATE core_account SET user_id=?1", [[0; 5].as_slice()])
            .unwrap();
        assert!(load_account(&c).is_err());
        c.execute(
            "UPDATE core_identity SET device_priv=?1",
            [[0; 5].as_slice()],
        )
        .unwrap();
        assert!(load_identity(&c).is_err());
        drop(c);
        cleanup(&p);
    }

    #[cfg(unix)]
    #[test]
    fn fresh_file_is_0600_and_ciphertext_tamper_fails() {
        use std::os::unix::fs::PermissionsExt;
        let p = path("tamper");
        let c = open_encrypted(&p, &KEY).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        save_identity(&c, &[25; 32]).unwrap();
        let mut raw: Vec<u8> = c
            .query_row("SELECT device_priv FROM core_identity", [], |r| r.get(0))
            .unwrap();
        *raw.last_mut().unwrap() ^= 1;
        c.execute("UPDATE core_identity SET device_priv=?1", [raw])
            .unwrap();
        assert!(load_identity(&c).is_err());
        drop(c);
        cleanup(&p);
    }
}

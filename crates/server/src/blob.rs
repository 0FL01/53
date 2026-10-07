//! Opaque immutable chunks: sync temp -> rename -> sync parent -> DB receipt -> ACK.
//! Called on a blocking worker with the shared blob/backup/GC permit. DB locks
//! cover only metadata, never filesystem work; each phase rechecks exact device.

use dmsg_protocol::{blob as bp, *};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const BLOB_SIZE_MAX: i64 = dmsg_protocol::BLOB_SIZE_MAX as i64;
/// Even tiny reservations consume bounded metadata/filesystem resources.
pub const BLOB_COUNT_MAX: i64 = 512;

#[derive(Debug, PartialEq, Eq)]
pub enum BlobError {
    Bad,
    Quota,
    Busy,
    Revoked,
    Store(String),
}
impl BlobError {
    pub fn code(&self) -> u8 {
        match self {
            Self::Bad | Self::Store(_) => ERR_BAD,
            Self::Quota => ERR_QUOTA,
            Self::Busy => ERR_BUSY,
            Self::Revoked => ERR_REVOKED,
        }
    }
}
fn store(e: rusqlite::Error) -> BlobError {
    if matches!(&e,rusqlite::Error::SqliteFailure(f,_) if f.code==rusqlite::ErrorCode::DatabaseBusy)
    {
        BlobError::Busy
    } else {
        BlobError::Store(e.to_string())
    }
}
fn io(e: std::io::Error) -> BlobError {
    BlobError::Store(e.to_string())
}

fn active(db: &Connection, user: &[u8; 16], device: &[u8; 32]) -> Result<(), BlobError> {
    let ok: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE device_key=?1 AND user_id=?2 AND revoked=0 AND blocked=0)",params![device,user],|r|r.get(0)).map_err(store)?;
    if ok {
        Ok(())
    } else {
        Err(BlobError::Revoked)
    }
}
fn contacts(db: &Connection, a: &[u8], b: &[u8]) -> Result<(), BlobError> {
    let (blocked,accepted):(bool,bool)=db.query_row("SELECT EXISTS(SELECT 1 FROM contact_permissions WHERE ((user_id=?1 AND peer_user_id=?2) OR (user_id=?2 AND peer_user_id=?1)) AND state='blocked'), EXISTS(SELECT 1 FROM contact_permissions WHERE ((user_id=?1 AND peer_user_id=?2) OR (user_id=?2 AND peer_user_id=?1)) AND state='accepted')",params![a,b],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store)?;
    if !blocked && accepted {
        Ok(())
    } else {
        Err(BlobError::Bad)
    }
}

struct Meta {
    owner: [u8; 16],
    device: [u8; 32],
    size: u32,
    complete: bool,
}
fn meta(db: &Connection, id: &[u8; 16], now: i64) -> Result<Meta, BlobError> {
    db.query_row("SELECT owner_user_id,owner_device,size,state='complete' FROM blob_meta WHERE blob_id=?1 AND expires_at>?2",params![id,now],|r|Ok(Meta{owner:r.get(0)?,device:r.get(1)?,size:r.get(2)?,complete:r.get(3)?})).optional().map_err(store)?.ok_or(BlobError::Bad)
}
fn owned(
    db: &Connection,
    id: &[u8; 16],
    user: &[u8; 16],
    device: &[u8; 32],
    now: i64,
) -> Result<Meta, BlobError> {
    active(db, user, device)?;
    let m = meta(db, id, now)?;
    if m.owner != *user || m.device != *device {
        return Err(BlobError::Bad);
    }
    Ok(m)
}
fn readable(
    db: &Connection,
    id: &[u8; 16],
    user: &[u8; 16],
    device: &[u8; 32],
    now: i64,
) -> Result<Meta, BlobError> {
    active(db, user, device)?;
    let m = meta(db, id, now)?;
    if m.owner == *user && m.device == *device {
        return Ok(m);
    }
    if m.owner == *user {
        return Err(BlobError::Bad);
    }
    if !m.complete {
        return Err(BlobError::Bad);
    }
    // An accepted ACL remains attached to its original recipient. Retiring the
    // sender does not reroute it or erase the recipient's already accepted data;
    // every requester still must be the exact active authenticated device.
    let acl:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM blob_acl WHERE blob_id=?1 AND recipient_user=?2 AND recipient_device=?3)",params![id,user,device],|r|r.get(0)).map_err(store)?;
    if !acl {
        return Err(BlobError::Bad);
    }
    contacts(db, &m.owner, user)?;
    Ok(m)
}

pub fn reserve(
    db: &mut Connection,
    user: &[u8; 16],
    device: &[u8; 32],
    id: &[u8; 16],
    size: u32,
    now: i64,
) -> Result<(), BlobError> {
    if i64::from(size) > BLOB_SIZE_MAX {
        return Err(BlobError::Bad);
    }
    bp::chunk_count(size).ok_or(BlobError::Bad)?;
    let tx = db.transaction().map_err(store)?;
    active(&tx, user, device)?;
    let prior: Option<(Vec<u8>, Vec<u8>, u32, i64)> = tx
        .query_row(
            "SELECT owner_user_id,owner_device,size,expires_at FROM blob_meta WHERE blob_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(store)?;
    if let Some((u, d, s, expiry)) = prior {
        return if u == user && d == device && s == size && expiry > now {
            Ok(())
        } else {
            Err(BlobError::Bad)
        };
    }
    let (count, total): (i64, i64) = tx
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(size),0) FROM blob_meta WHERE owner_user_id=?1",
            [user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(store)?;
    if count >= BLOB_COUNT_MAX || total + i64::from(size) > MAILBOX_BYTES_MAX as i64 {
        return Err(BlobError::Quota);
    }
    tx.execute("INSERT INTO blob_meta(blob_id,owner_user_id,owner_device,size,created_at,expires_at,state) VALUES(?1,?2,?3,?4,?5,?6,'reserved')",params![id,user,device,size,now,now+BLOB_RESERVE_TTL_SECS as i64]).map_err(store)?;
    tx.commit().map_err(store)
}

fn receipts(db: &Connection, id: &[u8; 16], size: u32) -> Result<u64, BlobError> {
    let mut stmt = db
        .prepare(
            "SELECT chunk_index,byte_len FROM blob_chunks WHERE blob_id=?1 ORDER BY chunk_index",
        )
        .map_err(store)?;
    let rows = stmt
        .query_map([id], |r| Ok((r.get::<_, u16>(0)?, r.get::<_, usize>(1)?)))
        .map_err(store)?;
    let mut bitmap = 0;
    for row in rows {
        let (index, len) = row.map_err(store)?;
        if bp::chunk_len(size, index) != Some(len) {
            return Err(BlobError::Bad);
        }
        bitmap |= 1u64 << index;
    }
    Ok(bitmap)
}
pub fn status(
    db: &Connection,
    user: &[u8; 16],
    device: &[u8; 32],
    id: &[u8; 16],
    now: i64,
) -> Result<bp::Status, BlobError> {
    let m = readable(db, id, user, device, now)?;
    let bitmap = receipts(db, id, m.size)?;
    Ok(bp::Status {
        blob_id: *id,
        byte_len: m.size,
        state: if m.complete {
            bp::State::Complete
        } else {
            bp::State::Reserved
        },
        bitmap,
    })
}

pub fn directory(root: &Path, id: &[u8; 16]) -> PathBuf {
    root.join(id.iter().map(|b| format!("{b:02x}")).collect::<String>())
}
pub fn chunk_path(root: &Path, id: &[u8; 16], index: u16) -> PathBuf {
    directory(root, id).join(format!("{index}.chunk"))
}
fn read_chunk(path: &Path, expected: usize) -> Result<Vec<u8>, BlobError> {
    let f = File::open(path).map_err(io)?;
    if !f.metadata().map_err(io)?.is_file() {
        return Err(BlobError::Bad);
    }
    let mut out = Vec::with_capacity(expected);
    f.take((bp::CHUNK_MAX + 1) as u64)
        .read_to_end(&mut out)
        .map_err(io)?;
    if out.len() != expected {
        return Err(BlobError::Bad);
    }
    Ok(out)
}
fn sync_dir(path: &Path) -> Result<(), BlobError> {
    File::open(path).and_then(|f| f.sync_all()).map_err(io)
}

/// Replaying an orphaned durable rename (crash before receipt) is safe only if
/// the exact bytes match; the caller then commits the missing receipt.
fn durable_chunk(root: &Path, id: &[u8; 16], index: u16, bytes: &[u8]) -> Result<(), BlobError> {
    let dir = directory(root, id);
    fs::create_dir_all(&dir).map_err(io)?;
    sync_dir(root)?;
    let path = chunk_path(root, id, index);
    if path.exists() {
        if read_chunk(&path, bytes.len())? != bytes {
            return Err(BlobError::Bad);
        }
        File::open(&path).and_then(|f| f.sync_all()).map_err(io)?;
        sync_dir(&dir)?;
        return Ok(());
    }
    let temp = dir.join(format!("{index}.part"));
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)
        .map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)?;
    drop(f);
    fs::rename(&temp, &path).map_err(io)?;
    sync_dir(&dir)
}

pub fn put(
    db: &Arc<Mutex<Connection>>,
    root: &Path,
    user: &[u8; 16],
    device: &[u8; 32],
    id: &[u8; 16],
    index: u16,
    bytes: &[u8],
    now: i64,
) -> Result<(), BlobError> {
    let (size, received) = {
        let db = db.lock().expect("db");
        let m = owned(&db, id, user, device, now)?;
        if bp::chunk_len(m.size, index) != Some(bytes.len()) {
            return Err(BlobError::Bad);
        }
        let received = receipts(&db, id, m.size)? & (1u64 << index) != 0;
        if m.complete && !received {
            return Err(BlobError::Bad);
        }
        (m.size, received)
    };
    // Missing/corrupt acknowledged data never gets silently replaced.
    if received {
        if read_chunk(&chunk_path(root, id, index), bytes.len())? != bytes {
            return Err(BlobError::Bad);
        }
    } else {
        durable_chunk(root, id, index, bytes)?;
    }
    let mut db = db.lock().expect("db");
    let tx = db.transaction().map_err(store)?;
    let m = owned(&tx, id, user, device, now)?;
    if m.size != size {
        return Err(BlobError::Bad);
    }
    tx.execute("INSERT INTO blob_chunks(blob_id,chunk_index,byte_len) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![id,index,bytes.len()]).map_err(store)?;
    tx.commit().map_err(store)
}
pub fn finish(
    db: &Arc<Mutex<Connection>>,
    root: &Path,
    user: &[u8; 16],
    device: &[u8; 32],
    id: &[u8; 16],
    now: i64,
) -> Result<(), BlobError> {
    let size = {
        let db = db.lock().expect("db");
        let m = owned(&db, id, user, device, now)?;
        if receipts(&db, id, m.size)? != bp::full_bitmap(m.size).ok_or(BlobError::Bad)? {
            return Err(BlobError::Bad);
        }
        m.size
    };
    for i in 0..bp::chunk_count(size).ok_or(BlobError::Bad)? {
        read_chunk(
            &chunk_path(root, id, i),
            bp::chunk_len(size, i).ok_or(BlobError::Bad)?,
        )?;
    }
    let mut db = db.lock().expect("db");
    let tx = db.transaction().map_err(store)?;
    let m = owned(&tx, id, user, device, now)?;
    if !m.complete {
        tx.execute(
            "UPDATE blob_meta SET state='complete',expires_at=?2 WHERE blob_id=?1",
            params![id, now + DATA_TTL_SECS as i64],
        )
        .map_err(store)?;
    }
    tx.commit().map_err(store)
}
pub fn get(
    db: &Arc<Mutex<Connection>>,
    root: &Path,
    user: &[u8; 16],
    device: &[u8; 32],
    id: &[u8; 16],
    index: u16,
    now: i64,
) -> Result<Vec<u8>, BlobError> {
    let len = {
        let db = db.lock().expect("db");
        let m = readable(&db, id, user, device, now)?;
        if !m.complete {
            return Err(BlobError::Bad);
        }
        bp::chunk_len(m.size, index).ok_or(BlobError::Bad)?
    };
    let bytes = read_chunk(&chunk_path(root, id, index), len)?;
    readable(&db.lock().expect("db"), id, user, device, now)?;
    Ok(bytes)
}

/// Mailbox, dedup fields and recipient ACL commit together. A matching retry
/// returns the ORIGINAL accept even after recipient replacement, never regrants
/// an ACL or routes it to the replacement. Conflicting retries are rejected.
pub fn send_media(
    db: &mut Connection,
    user: &[u8; 16],
    device: &[u8; 32],
    s: &bp::SendMedia<'_>,
    now: i64,
) -> Result<u8, BlobError> {
    let tx = db.transaction().map_err(store)?;
    active(&tx, user, device)?;
    let prior:Option<(Vec<u8>,Option<Vec<u8>>,Option<Vec<u8>>,Vec<u8>,bool)>=tx.query_row("SELECT recipient_user_id,recipient_device,blob_id,ciphertext,delivered FROM mailbox_events WHERE sender_device=?1 AND message_id=?2",params![device,s.message_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(store)?;
    if let Some((u, d, b, c, delivered)) = prior {
        if u != s.recipient
            || d.as_deref() != Some(s.recipient_device.as_slice())
            || b.as_deref() != Some(s.blob_id.as_slice())
            || c != s.ciphertext
        {
            return Err(BlobError::Bad);
        }
        return Ok(if delivered { ST_DELIVERED } else { ST_ACCEPTED });
    }
    let m = owned(&tx, s.blob_id, user, device, now)?;
    if !m.complete {
        return Err(BlobError::Bad);
    }
    active(&tx, s.recipient, s.recipient_device)?;
    contacts(&tx, user, s.recipient)?;
    let (count,bytes):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(LENGTH(ciphertext)),0) FROM mailbox_events WHERE recipient_user_id=?1",[s.recipient],|r|Ok((r.get(0)?,r.get(1)?))).map_err(store)?;
    if count >= MAILBOX_EVENTS_MAX as i64
        || bytes + s.ciphertext.len() as i64 > MAILBOX_BYTES_MAX as i64
    {
        return Err(BlobError::Quota);
    }
    tx.execute("INSERT INTO mailbox_events(recipient_user_id,recipient_device,sender_device,message_id,blob_id,ciphertext,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![s.recipient,s.recipient_device,device,s.message_id,s.blob_id,s.ciphertext,now]).map_err(store)?;
    tx.execute("INSERT INTO blob_acl(blob_id,recipient_user,recipient_device) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![s.blob_id,s.recipient,s.recipient_device]).map_err(store)?;
    tx.execute(
        "UPDATE blob_meta SET expires_at=MAX(expires_at,?2) WHERE blob_id=?1",
        params![s.blob_id, now + DATA_TTL_SECS as i64],
    )
    .map_err(store)?;
    tx.commit().map_err(store)?;
    Ok(ST_ACCEPTED)
}

pub fn process(
    db: &Arc<Mutex<Connection>>,
    root: &Path,
    user: &[u8; 16],
    device: &[u8; 32],
    op: u8,
    p: &[u8],
    now: i64,
) -> Result<(u8, Vec<u8>), BlobError> {
    match op {
        OP_BLOB_RESERVE => {
            let (id, size) = bp::parse_reserve(p).ok_or(BlobError::Bad)?;
            reserve(&mut db.lock().expect("db"), user, device, &id, size, now)?;
            Ok((OP_BLOB_RESERVED, id.to_vec()))
        }
        OP_BLOB_STATUS => {
            let id = bp::parse_status(p).ok_or(BlobError::Bad)?;
            let s = status(&db.lock().expect("db"), user, device, &id, now)?;
            Ok((
                OP_BLOB_STATUS_RESP,
                bp::build_status_response(&s).ok_or(BlobError::Bad)?,
            ))
        }
        OP_BLOB_PUT => {
            let c = bp::parse_put(p).ok_or(BlobError::Bad)?;
            put(db, root, user, device, c.blob_id, c.index, c.bytes, now)?;
            Ok((
                OP_BLOB_PUT_ACK,
                bp::build_put_ack(c.blob_id, c.index).ok_or(BlobError::Bad)?,
            ))
        }
        OP_BLOB_FINISH => {
            let id = bp::parse_finish(p).ok_or(BlobError::Bad)?;
            finish(db, root, user, device, &id, now)?;
            Ok((OP_BLOB_FINISH_ACK, bp::build_finish_ack(&id)))
        }
        OP_BLOB_GET => {
            let (id, index) = bp::parse_get(p).ok_or(BlobError::Bad)?;
            let bytes = get(db, root, user, device, &id, index, now)?;
            Ok((
                OP_BLOB_DATA,
                bp::build_data(&id, index, &bytes).ok_or(BlobError::Bad)?,
            ))
        }
        OP_SEND_MEDIA => {
            let s = bp::parse_send_media(p).ok_or(BlobError::Bad)?;
            let status = send_media(&mut db.lock().expect("db"), user, device, &s, now)?;
            let mut ack = s.message_id.to_vec();
            ack.push(status);
            Ok((OP_SEND_ACK, ack))
        }
        _ => Err(BlobError::Bad),
    }
}

/// SQL metadata deletion is committed before filesystem removal; a crash can
/// leave an orphan, but never an acknowledged reference to a removed chunk.
pub fn gc(db: &Arc<Mutex<Connection>>, root: &Path, now: i64) -> Result<(usize, usize), BlobError> {
    let (blobs, events, live) = {
        let mut db = db.lock().expect("db");
        let tx = db.transaction().map_err(store)?;
        tx.execute("DELETE FROM blob_chunks WHERE blob_id IN (SELECT blob_id FROM blob_meta WHERE expires_at<=?1)",[now]).map_err(store)?;
        tx.execute("DELETE FROM blob_acl WHERE blob_id IN (SELECT blob_id FROM blob_meta WHERE expires_at<=?1)",[now]).map_err(store)?;
        let b = tx
            .execute("DELETE FROM blob_meta WHERE expires_at<=?1", [now])
            .map_err(store)?;
        let e = tx
            .execute(
                "DELETE FROM mailbox_events WHERE created_at<=?1",
                [now - DATA_TTL_SECS as i64],
            )
            .map_err(store)?;
        let mut live = std::collections::HashMap::new();
        {
            let mut stmt = tx.prepare("SELECT blob_id FROM blob_meta").map_err(store)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, [u8; 16]>(0))
                .map_err(store)?;
            for row in rows {
                let id = row.map_err(store)?;
                live.insert(
                    directory(root, &id),
                    receipts(&tx, &id, meta(&tx, &id, now)?.size)?,
                );
            }
        }
        tx.commit().map_err(store)?;
        (b, e, live)
    };
    for ent in fs::read_dir(root).map_err(io)? {
        let ent = ent.map_err(io)?;
        let path = ent.path();
        if ent.file_type().map_err(io)?.is_dir() {
            if let Some(bitmap) = live.get(&path) {
                for file in fs::read_dir(&path).map_err(io)? {
                    let file = file.map_err(io)?;
                    let index = file
                        .file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".chunk"))
                        .and_then(|n| n.parse::<u16>().ok());
                    if !index.is_some_and(|i| i < 64 && bitmap & (1u64 << i) != 0) {
                        fs::remove_file(file.path()).map_err(io)?;
                    }
                }
                sync_dir(&path)?;
            } else {
                fs::remove_dir_all(path).map_err(io)?;
            }
        } else {
            fs::remove_file(path).map_err(io)?;
        }
    }
    sync_dir(root)?;
    Ok((blobs, events))
}

/// Backup copies only DB-referenced immutable chunks and verifies exact sizes.
/// Caller holds the shared exclusion permit from snapshot through this copy.
pub fn copy_references(
    snapshot: &Connection,
    root: &Path,
    dst: &Path,
) -> Result<(usize, u64), BlobError> {
    let mut stmt=snapshot.prepare("SELECT c.blob_id,c.chunk_index,c.byte_len,m.size FROM blob_chunks c JOIN blob_meta m ON m.blob_id=c.blob_id ORDER BY c.blob_id,c.chunk_index").map_err(store)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, [u8; 16]>(0)?,
                r.get::<_, u16>(1)?,
                r.get::<_, usize>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })
        .map_err(store)?;
    let (mut files, mut total) = (0, 0);
    for row in rows {
        let (id, index, len, size) = row.map_err(store)?;
        if bp::chunk_len(size, index) != Some(len) {
            return Err(BlobError::Bad);
        }
        let bytes = read_chunk(&chunk_path(root, &id, index), len)?;
        durable_chunk(dst, &id, index, &bytes)?;
        files += 1;
        total += len as u64;
    }
    let invalid:bool=snapshot.query_row("SELECT EXISTS(SELECT 1 FROM blob_meta m WHERE state='complete' AND (SELECT COALESCE(SUM(byte_len),0) FROM blob_chunks WHERE blob_id=m.blob_id)<>m.size) OR EXISTS(SELECT 1 FROM mailbox_events e WHERE e.blob_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM blob_meta m JOIN blob_acl a ON a.blob_id=m.blob_id WHERE m.blob_id=e.blob_id AND m.state='complete' AND a.recipient_user=e.recipient_user_id AND a.recipient_device=e.recipient_device))",[],|r|r.get(0)).map_err(store)?;
    if invalid {
        return Err(BlobError::Bad);
    }
    Ok((files, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> (Arc<Mutex<Connection>>, PathBuf) {
        let root = std::env::temp_dir().join(format!("msgd-blob-{name}-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        for n in 1..=3u8 {
            db.execute(
                "INSERT INTO users VALUES(?1,?2,?3,'$argon2id$test',1)",
                params![[n; 16], format!("{n:012}"), format!("user{n}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
                params![[n; 32], [n; 16]],
            )
            .unwrap();
        }
        (Arc::new(Mutex::new(db)), root)
    }
    #[test]
    fn rename_before_receipt_and_partial_temp_recover_without_false_ack() {
        let (db, root) = fixture("crash");
        let id = [4; 16];
        reserve(&mut db.lock().unwrap(), &[1; 16], &[1; 32], &id, 8193, 100).unwrap();
        durable_chunk(&root, &id, 0, &vec![9; 8192]).unwrap();
        assert_eq!(
            status(&db.lock().unwrap(), &[1; 16], &[1; 32], &id, 101)
                .unwrap()
                .bitmap,
            0
        );
        assert_eq!(
            put(&db, &root, &[1; 16], &[1; 32], &id, 0, &vec![8; 8192], 101),
            Err(BlobError::Bad)
        );
        put(&db, &root, &[1; 16], &[1; 32], &id, 0, &vec![9; 8192], 101).unwrap();
        fs::write(directory(&root, &id).join("1.part"), b"partial").unwrap();
        assert_eq!(
            finish(&db, &root, &[1; 16], &[1; 32], &id, 101),
            Err(BlobError::Bad)
        );
        put(&db, &root, &[1; 16], &[1; 32], &id, 1, b"z", 101).unwrap();
        finish(&db, &root, &[1; 16], &[1; 32], &id, 102).unwrap();
        assert_eq!(
            get(&db, &root, &[1; 16], &[1; 32], &id, 1, 103).unwrap(),
            b"z"
        );
        fs::remove_file(chunk_path(&root, &id, 0)).unwrap();
        assert!(put(&db, &root, &[1; 16], &[1; 32], &id, 0, &vec![9; 8192], 103).is_err());
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn quotas_count_complete_and_reserved_and_gc_cleans_orphans() {
        let (db, root) = fixture("quota");
        let id = [4; 16];
        reserve(&mut db.lock().unwrap(), &[1; 16], &[1; 32], &id, 1, 100).unwrap();
        put(&db, &root, &[1; 16], &[1; 32], &id, 0, b"a", 100).unwrap();
        finish(&db, &root, &[1; 16], &[1; 32], &id, 100).unwrap();
        db.lock()
            .unwrap()
            .execute("UPDATE blob_meta SET size=?1", [MAILBOX_BYTES_MAX as i64])
            .unwrap_err();
        for n in 0..63u8 {
            let mut id = [n; 16];
            id[15] = 99;
            reserve(
                &mut db.lock().unwrap(),
                &[1; 16],
                &[1; 32],
                &id,
                BLOB_SIZE_MAX as u32,
                100,
            )
            .unwrap();
        }
        assert_eq!(
            reserve(
                &mut db.lock().unwrap(),
                &[1; 16],
                &[1; 32],
                &[99; 16],
                BLOB_SIZE_MAX as u32,
                100
            ),
            Err(BlobError::Quota)
        );
        let orphan = directory(&root, &[100; 16]);
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("0.part"), b"x").unwrap();
        let (b, _) = gc(&db, &root, 100 + BLOB_RESERVE_TTL_SECS as i64).unwrap();
        assert_eq!(b, 63);
        assert!(!orphan.exists());
        assert!(chunk_path(&root, &id, 0).exists());
        let (b, _) = gc(&db, &root, 100 + DATA_TTL_SECS as i64).unwrap();
        assert_eq!(b, 1);
        assert!(!directory(&root, &id).exists());
        for n in 0..BLOB_COUNT_MAX {
            let mut id = [0; 16];
            id[..8].copy_from_slice(&n.to_be_bytes());
            reserve(
                &mut db.lock().unwrap(),
                &[1; 16],
                &[1; 32],
                &id,
                1,
                1_000_000,
            )
            .unwrap();
        }
        assert_eq!(
            reserve(
                &mut db.lock().unwrap(),
                &[1; 16],
                &[1; 32],
                &[254; 16],
                1,
                1_000_000
            ),
            Err(BlobError::Quota)
        );
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revoked_requester_cannot_use_acl_but_sender_retirement_keeps_accepted_data() {
        let (db, root) = fixture("revocation");
        let id = [9; 16];
        reserve(&mut db.lock().unwrap(), &[1; 16], &[1; 32], &id, 1, 100).unwrap();
        put(&db, &root, &[1; 16], &[1; 32], &id, 0, b"a", 100).unwrap();
        finish(&db, &root, &[1; 16], &[1; 32], &id, 100).unwrap();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO contact_permissions VALUES(?1,?2,'accepted')",
                params![[2u8; 16], [1u8; 16]],
            )
            .unwrap();
        let p = bp::build_send_media(&[2; 16], &[2; 32], &[10; 16], &id, b"olm").unwrap();
        send_media(
            &mut db.lock().unwrap(),
            &[1; 16],
            &[1; 32],
            &bp::parse_send_media(&p).unwrap(),
            100,
        )
        .unwrap();
        db.lock()
            .unwrap()
            .execute(
                "UPDATE devices SET revoked=1 WHERE device_key=?1",
                [[1; 32]],
            )
            .unwrap();
        assert_eq!(
            get(&db, &root, &[1; 16], &[1; 32], &id, 0, 101),
            Err(BlobError::Revoked)
        );
        assert_eq!(
            get(&db, &root, &[2; 16], &[2; 32], &id, 0, 101).unwrap(),
            b"a"
        );
        db.lock()
            .unwrap()
            .execute(
                "UPDATE devices SET blocked=1 WHERE device_key=?1",
                [[2; 32]],
            )
            .unwrap();
        assert_eq!(
            get(&db, &root, &[2; 16], &[2; 32], &id, 0, 101),
            Err(BlobError::Revoked)
        );
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
}

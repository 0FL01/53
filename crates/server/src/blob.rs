//! Blobs P4.3: reservation + ACL + orphan-sweep + GC.
//! Сервер хранит только метаданные и opaque-куски (куски — M5, здесь только учёт):
//! reserve проверяет квоты, чтение — contact-разрешение (проверяется вызывателем
//! по contact_permissions; здесь — владение), GC чистит старое детерминированно.

use rusqlite::{Connection, OptionalExtension};
use dmsg_protocol::{BLOB_RESERVE_TTL_SECS, DATA_TTL_SECS, MAILBOX_BYTES_MAX, MAILBOX_EVENTS_MAX};

/// Максимум одного attachment (wire ciphertext), 512 KiB.
pub const BLOB_SIZE_MAX: i64 = 512 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum BlobError {
    Bad,
    Quota,
    Store(String),
}

/// Reservation: проверки квот → INSERT (идемпотентно по blob_id) → commit.
pub fn reserve(
    conn: &mut Connection,
    owner: &[u8],
    blob_id: &[u8],
    size: i64,
    now: i64,
) -> Result<(), BlobError> {
    if blob_id.len() != 16 || size <= 0 || size > BLOB_SIZE_MAX {
        return Err(BlobError::Bad);
    }
    let tx = conn.transaction().map_err(|e| BlobError::Store(e.to_string()))?;
    let total: Option<i64> = tx
        .query_row(
            "SELECT COALESCE(SUM(size),0) FROM blob_meta WHERE owner_user_id=?1 AND state='reserved'",
            [owner],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| BlobError::Store(e.to_string()))?
        .unwrap_or(Some(0));
    if total.unwrap_or(0) + size > MAILBOX_BYTES_MAX as i64 {
        return Err(BlobError::Quota);
    }
    tx.execute(
        "INSERT INTO blob_meta(blob_id,owner_user_id,size,created_at,expires_at,state)
         VALUES(?1,?2,?3,?4,?5,'reserved')
         ON CONFLICT(blob_id) DO NOTHING",
        rusqlite::params![blob_id, owner, size, now, now + BLOB_RESERVE_TTL_SECS as i64],
    )
    .map_err(|e| BlobError::Store(e.to_string()))?;
    tx.commit().map_err(|e| BlobError::Store(e.to_string()))?;
    Ok(())
}

/// GC: orphan-sweep (reservation старше 24ч) + TTL событий (7 сут) +
/// caps mailbox (старые seq первыми). Возвращает (blobs, events).
pub fn gc(conn: &mut Connection, now: i64) -> Result<(usize, usize), BlobError> {
    let tx = conn.transaction().map_err(|e| BlobError::Store(e.to_string()))?;
    let b = tx
        .execute(
            "DELETE FROM blob_meta WHERE state='reserved' AND expires_at<?1",
            [now],
        )
        .map_err(|e| BlobError::Store(e.to_string()))?;
    let mut e = tx
        .execute(
            "DELETE FROM mailbox_events WHERE created_at<?1",
            [now - DATA_TTL_SECS as i64],
        )
        .map_err(|e| BlobError::Store(e.to_string()))?;
    // Caps: сначала события сверх 512, потом байты сверх 32 MiB — старые первыми.
    loop {
        let over: bool = tx
            .query_row(
                "SELECT COUNT(*)>?1 OR COALESCE(SUM(LENGTH(ciphertext)),0)>?2 FROM mailbox_events",
                rusqlite::params![MAILBOX_EVENTS_MAX as i64, MAILBOX_BYTES_MAX as i64],
                |r| r.get(0),
            )
            .map_err(|e| BlobError::Store(e.to_string()))?;
        if !over {
            break;
        }
        let cut: Option<i64> = tx
            .query_row("SELECT MIN(seq) FROM mailbox_events", [], |r| r.get(0))
            .optional()
            .map_err(|e| BlobError::Store(e.to_string()))?
            .unwrap_or(None);
        match cut {
            Some(s) => {
                e += tx
                    .execute("DELETE FROM mailbox_events WHERE seq=?1", [s])
                    .map_err(|e| BlobError::Store(e.to_string()))?;
            }
            None => break,
        }
    }
    tx.commit().map_err(|e| BlobError::Store(e.to_string()))?;
    Ok((b, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn reserve_quota_and_sweep() {
        let mut conn = mem();
        let owner = [41u8; 16];
        // Битые аргументы.
        assert_eq!(reserve(&mut conn, &owner, &[0u8; 15], 10, 100).unwrap_err(), BlobError::Bad);
        assert_eq!(reserve(&mut conn, &owner, &[0u8; 16], 0, 100).unwrap_err(), BlobError::Bad);
        assert_eq!(reserve(&mut conn, &owner, &[0u8; 16], BLOB_SIZE_MAX + 1, 100).unwrap_err(), BlobError::Bad);
        // Резерв + идемпотентный повтор.
        reserve(&mut conn, &owner, &[1u8; 16], 1000, 100).unwrap();
        reserve(&mut conn, &owner, &[1u8; 16], 1000, 101).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM blob_meta", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // Sweep: reservation протухла (24ч), свежая живёт.
        reserve(&mut conn, &owner, &[2u8; 16], 10, 200).unwrap();
        let (b, _) = gc(&mut conn, 200 + BLOB_RESERVE_TTL_SECS as i64 + 1).unwrap();
        assert_eq!(b, 2);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM blob_meta", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn gc_caps_evict_oldest_first() {
        let mut conn = mem();
        let u = [43u8; 16];
        conn.execute("INSERT INTO users(user_id,contact_id,created_at) VALUES(?1,'z',1)", [&u[..]]).unwrap();
        for i in 0..513i64 {
            conn.execute(
                "INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at)
                 VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![&u[..], [8u8; 32], [(i % 256) as u8, (i / 256) as u8, 0,0,0,0,0,0,0,0,0,0,0,0,0,0], vec![0u8; 4], 1000],
            )
            .unwrap();
        }
        let (_, e) = gc(&mut conn, 2000).unwrap();
        assert_eq!(e, 1);
        let min: i64 = conn.query_row("SELECT MIN(seq) FROM mailbox_events", [], |r| r.get(0)).unwrap();
        assert_eq!(min, 2); // seq=1 (старейший) вытеснен
    }

    #[test]
    fn gc_ttl_expired_events() {
        let mut conn = mem();
        let u = [42u8; 16];
        conn.execute("INSERT INTO users(user_id,contact_id,created_at) VALUES(?1,'y',1)", [&u[..]]).unwrap();
        // Старое событие (TTL) + новые.
        for i in 0..4i64 {
            conn.execute(
                "INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at)
                 VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![&u[..], [7u8; 32], [i as u8; 16], vec![0u8; 8], if i == 0 { 0 } else { 1000 }],
            )
            .unwrap();
        }
        // now далеко за TTL: старое (created 0) уходит, новые (1000) тоже старше 7 сут? now=8 сут.
        let (_, e) = gc(&mut conn, 8 * 24 * 3600).unwrap();
        assert_eq!(e, 4);
    }
}

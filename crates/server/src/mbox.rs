//! Mailbox P4.2: SEND / FETCH / DELIVERY_ACK поверх SQLite.
//! Правила: server-ACK строго после commit; повтор = прежний accept (UNIQUE);
//! cursor — отдельной таблицей, двигается в той же TX, что DELIVERY_ACK,
//! только по непрерывному (гэпы остаются); FETCH cursor не двигает.
//! sender_device всегда из Noise-сессии (аргумент), никогда из тела.

use rusqlite::{Connection, OptionalExtension};
use dmsg_protocol::{ERR_BAD, ERR_BUSY, ERR_QUOTA, FETCH_BATCH_MAX, MAILBOX_BYTES_MAX, MAILBOX_EVENTS_MAX};

#[derive(Debug, PartialEq, Eq)]
pub enum MboxError {
    /// Неизвестный получатель / битые аргументы.
    Bad,
    /// Квота: 512 событий или 32 MiB.
    Quota,
    /// SQLITE_BUSY: повторить позже (wire ERR_BUSY=7). Маппится ДО схлопывания в Store.
    Busy,
    /// Ошибка SQLite.
    Store(String),
}

/// Wire-код ошибки (см. dmsg_protocol::ERR_*).
impl MboxError {
    pub fn code(&self) -> u8 {
        match self {
            MboxError::Bad | MboxError::Store(_) => ERR_BAD,
            MboxError::Quota => ERR_QUOTA,
            MboxError::Busy => ERR_BUSY,
        }
    }
}

/// SQLITE_BUSY → Busy, остальное — Store с контекстом. Вызывать на КАЖДОМ
/// rusqlite-результате до схлопывания, иначе busy утонет в Store→ERR_BAD.
fn store(prefix: &str, e: rusqlite::Error) -> MboxError {
    if matches!(&e, rusqlite::Error::SqliteFailure(f, _)
        if f.code == rusqlite::ErrorCode::DatabaseBusy)
    {
        MboxError::Busy
    } else {
        MboxError::Store(format!("{prefix}: {e}"))
    }
}

/// seq нужен тестам и будущей диагностике; wire отвечает message_id+status.
#[allow(dead_code)]
#[derive(Debug)]
pub enum SendOutcome {
    /// Новое событие, seq.
    New(i64),
    /// Дубликат: прежний accept, seq и флаг delivered существующего.
    Exists(i64, bool),
}

/// SEND: quota-в-TX → INSERT ON CONFLICT → commit. Возврат до коммита запрещён.
pub fn send(
    conn: &mut Connection,
    sender_device: &[u8],
    recipient: &[u8],
    message_id: &[u8],
    ciphertext: &[u8],
    now: i64,
) -> Result<SendOutcome, MboxError> {
    // Получатель обязан существовать (иначе письма в никуда).
    let known: bool = conn
        .query_row("SELECT 1 FROM users WHERE user_id=?1", [recipient], |_| Ok(()))
        .optional()
        .map_err(|e| store("user", e))?
        .is_some();
    if !known {
        return Err(MboxError::Bad);
    }
    let tx = conn.transaction().map_err(|e| store("begin", e))?;
    let (count, bytes): (i64, Option<i64>) = tx
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(ciphertext)),0) FROM mailbox_events WHERE recipient_user_id=?1",
            [recipient],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| store("quota", e))?;
    if count >= MAILBOX_EVENTS_MAX as i64
        || bytes.unwrap_or(0) + ciphertext.len() as i64 > MAILBOX_BYTES_MAX as i64
    {
        return Err(MboxError::Quota);
    }
    let inserted = tx
        .execute(
            "INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(sender_device,message_id) DO NOTHING",
            rusqlite::params![recipient, sender_device, message_id, ciphertext, now],
        )
        .map_err(|e| store("insert", e))?;
    let seq: i64 = if inserted == 1 {
        tx.last_insert_rowid()
    } else {
        tx.query_row(
            "SELECT seq FROM mailbox_events WHERE sender_device=?1 AND message_id=?2",
            rusqlite::params![sender_device, message_id],
            |r| r.get(0),
        )
        .map_err(|e| store("seq", e))?
    };
    // Повтор после доставки сообщает отправителю актуальный статус.
    let delivered: bool = tx
        .query_row(
            "SELECT delivered FROM mailbox_events WHERE sender_device=?1 AND message_id=?2",
            rusqlite::params![sender_device, message_id],
            |r| r.get::<_, i64>(0).map(|v| v != 0),
        )
        .map_err(|e| store("delivered", e))?;
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(if inserted == 1 {
        SendOutcome::New(seq)
    } else {
        SendOutcome::Exists(seq, delivered)
    })
}

/// Одна выдача FETCH: события после cursor, по seq. Cursor НЕ двигается (см. ack).
pub struct Fetched {
    pub seq: i64,
    pub sender: Vec<u8>,
    pub message_id: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub fn fetch(
    conn: &Connection,
    recipient: &[u8],
    device: &[u8],
) -> Result<Vec<Fetched>, MboxError> {
    let cursor: i64 = conn
        .query_row(
            "SELECT last_seq FROM cursors WHERE recipient_user_id=?1 AND device_key=?2",
            rusqlite::params![recipient, device],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| store("cursor", e))?
        .unwrap_or(0);
    let mut stmt = conn
        .prepare(
            "SELECT seq,sender_device,message_id,ciphertext FROM mailbox_events
             WHERE recipient_user_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
        )
        .map_err(|e| store("prepare", e))?;
    let rows = stmt
        .query_map(
            rusqlite::params![recipient, cursor, FETCH_BATCH_MAX as i64],
            |r| {
                Ok(Fetched {
                    seq: r.get(0)?,
                    sender: r.get(1)?,
                    message_id: r.get(2)?,
                    ciphertext: r.get(3)?,
                })
            },
        )
        .map_err(|e| store("fetch", e))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| store("row", e))?);
    }
    Ok(out)
}

/// DELIVERY_ACK: пометить seq + двинуть cursor по непрерывному — всё в одной TX.
/// Возвращает новый cursor.
pub fn ack(
    conn: &mut Connection,
    recipient: &[u8],
    device: &[u8],
    seqs: &[i64],
) -> Result<i64, MboxError> {
    let tx = conn.transaction().map_err(|e| store("begin", e))?;
    for seq in seqs {
        tx.execute(
            "UPDATE mailbox_events SET delivered=1 WHERE recipient_user_id=?1 AND seq=?2",
            rusqlite::params![recipient, seq],
        )
        .map_err(|e| store("mark", e))?;
    }
    let mut cursor: i64 = tx
        .query_row(
            "SELECT last_seq FROM cursors WHERE recipient_user_id=?1 AND device_key=?2",
            rusqlite::params![recipient, device],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| store("cursor", e))?
        .unwrap_or(0);
    // Непрерывный вперёд от cursor по доставленным.
    loop {
        let next: Option<i64> = tx
            .query_row(
                "SELECT seq FROM mailbox_events WHERE recipient_user_id=?1 AND seq=?2 AND delivered=1",
                rusqlite::params![recipient, cursor + 1],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| store("next", e))?;
        match next {
            Some(_) => cursor += 1,
            None => break,
        }
    }
    tx.execute(
        "INSERT INTO cursors(recipient_user_id,device_key,last_seq) VALUES(?1,?2,?3)
         ON CONFLICT(recipient_user_id,device_key) DO UPDATE SET last_seq=excluded.last_seq",
        rusqlite::params![recipient, device, cursor],
    )
    .map_err(|e| store("cursor", e))?;
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    fn user(conn: &Connection, id: &[u8; 16]) {
        conn.execute(
            "INSERT INTO users(user_id,contact_id,created_at) VALUES(?1,'x',1)",
            [id.as_slice()],
        )
        .unwrap();
    }

    #[test]
    fn send_ack_dedup_and_quota() {
        let mut conn = mem();
        let u = [1u8; 16];
        user(&conn, &u);
        let s = [2u8; 32];
        let m = [3u8; 16];
        let seq = match send(&mut conn, &s, &u, &m, b"hello", 10).unwrap() {
            SendOutcome::New(q) => q,
            SendOutcome::Exists(..) => panic!("want new"),
        };
        assert!(seq >= 1);
        // Повтор тем же message_id — прежний accept, без нового события.
        match send(&mut conn, &s, &u, &m, b"hello-again", 11).unwrap() {
            SendOutcome::Exists(q, delivered) => {
                assert_eq!(q, seq);
                assert!(!delivered);
            }
            SendOutcome::New(_) => panic!("want dedup"),
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM mailbox_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // Неизвестный получатель.
        assert_eq!(
            send(&mut conn, &s, &[9u8; 16], &[4u8; 16], b"x", 12).unwrap_err(),
            MboxError::Bad
        );
    }

    #[test]
    fn sqlite_busy_maps_to_busy_code() {
        let e = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None);
        assert_eq!(store("t", e), MboxError::Busy);
        assert_eq!(MboxError::Busy.code(), dmsg_protocol::ERR_BUSY);
        assert_eq!(MboxError::Quota.code(), dmsg_protocol::ERR_QUOTA);
    }

    #[test]
    fn fetch_ack_cursor_contiguous() {
        let mut conn = mem();
        let u = [5u8; 16];
        user(&conn, &u);
        let dev = [6u8; 32];
        let s = [7u8; 32];
        for i in 0..3u8 {
            let mut m = [0u8; 16];
            m[0] = i;
            assert!(matches!(send(&mut conn, &s, &u, &m, b"d", 20), Ok(SendOutcome::New(_))));
        }
        let got = fetch(&conn, &u, &dev).unwrap();
        assert_eq!(got.len(), 3);
        // FETCH cursor не двигает: повторная выдача та же.
        assert_eq!(fetch(&conn, &u, &dev).unwrap().len(), 3);
        // ACK среднего: cursor стоит (гэп).
        let c = ack(&mut conn, &u, &dev, &[got[1].seq]).unwrap();
        assert_eq!(c, 0);
        // ACK первого: cursor идёт до непрерывного (1,2 уже delivered).
        let c = ack(&mut conn, &u, &dev, &[got[0].seq]).unwrap();
        assert_eq!(c, got[1].seq);
        // Выдача продолжается с cursor.
        let got2 = fetch(&conn, &u, &dev).unwrap();
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].seq, got[2].seq);
    }
}

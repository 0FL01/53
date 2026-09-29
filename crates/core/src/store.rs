//! Свой SQLite-файл ядра (WAL + минимальные миграции).
//!
//! Схему сервера НЕ копируем: здесь только нужное core. K1 — identity
//! (Noise static-приватник устройства, persist для K2); outbox — позже в K3.
//! Секреты — только через этот файл (0600), никогда в логи/argv/Git.

/// Версия схемы ядра. Миграции — только вперёд, по одной на версию.
pub const SCHEMA_VERSION: i64 = 3;

/// Открыть (создать) файл БД ядра: parent-dirs, WAL, файл 0600, миграции.
///
/// Миграция v1: только `core_identity` (одна строка id=1, сырые 32 байта
/// Noise static-приватника). Миграция v2 (K2 enrol): `core_account` (одна
/// строка id=1: user_id 16 байт + contact_id). Миграция v3 (K3 Olm):
/// `core_olm` (пикл Account + счётчик key_id), `core_sessions` (пиклы
/// сессий 1-на-1), `core_contacts` (пины identity, состояния),
/// `core_outbox` (сохранённый ciphertext + статусы), `core_inbox`
/// (дедуп по sender+message_id). Таблиц сервера здесь нет и не будет.
/// Пиклы лежат в том же файле 0600, что device_priv — та же модель угроз.
pub fn open(path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
    }
    let conn = rusqlite::Connection::open(path).map_err(|e| format!("open: {e}"))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS core_identity(
           id INTEGER PRIMARY KEY CHECK(id=1),
           device_priv BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS core_account(
           id INTEGER PRIMARY KEY CHECK(id=1),
           user_id BLOB NOT NULL,
           contact_id TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS core_olm(
           id INTEGER PRIMARY KEY CHECK(id=1),
           pickle TEXT NOT NULL,
           next_key_id INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS core_sessions(
           contact_id TEXT PRIMARY KEY,
           pickle TEXT NOT NULL,
           peer_ed BLOB NOT NULL,
           peer_curve BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS core_contacts(
           contact_id TEXT PRIMARY KEY,
           user_id BLOB,
           device_key BLOB,
           ed_identity BLOB,
           curve_identity BLOB,
           state TEXT NOT NULL,
           seen_ed BLOB,
           seen_curve BLOB
         );
         CREATE TABLE IF NOT EXISTS core_outbox(
           message_id BLOB PRIMARY KEY,
           contact_id TEXT NOT NULL,
           ciphertext BLOB NOT NULL,
           status TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS core_inbox(
           sender_device BLOB NOT NULL,
           message_id BLOB NOT NULL,
           contact_id TEXT NOT NULL,
           text TEXT NOT NULL,
           seq INTEGER NOT NULL,
           PRIMARY KEY(sender_device, message_id)
         );
         -- K3: bearer-token invite (32 байта) для ENROL-replay на каждом
         -- новом коннекте: сервер держит enrolled-флаг per-connection и
         -- требует ENROL даже от известного ключа (replay идемпотентен).
         CREATE TABLE IF NOT EXISTS core_token(
           id INTEGER PRIMARY KEY CHECK(id=1),
           token BLOB NOT NULL
         );",
    )
    .map_err(|e| format!("migrate v3: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod: {e}"))?;
    }
    Ok(conn)
}

/// Сохранить Noise static-приватник устройства (upsert единственной строки).
/// В логи ключ не попадает — только rowcount в ошибке.
pub fn save_identity(conn: &rusqlite::Connection, privkey: &[u8; 32]) -> Result<(), String> {
    let n = conn
        .execute(
            "INSERT INTO core_identity(id, device_priv) VALUES(1,?1)
             ON CONFLICT(id) DO UPDATE SET device_priv=excluded.device_priv",
            rusqlite::params![privkey.as_slice()],
        )
        .map_err(|e| format!("save identity: {e}"))?;
    if n != 1 {
        return Err("save identity: no row".into());
    }
    Ok(())
}

/// Загрузить приватник устройства. None — enrol ещё не было (норма K1).
/// Мусор длиной ≠32 — Err (fail-closed, не silent-None).
pub fn load_identity(conn: &rusqlite::Connection) -> Result<Option<[u8; 32]>, String> {
    let row: Option<Vec<u8>> = conn
        .query_row("SELECT device_priv FROM core_identity WHERE id=1", [], |r| r.get(0))
        .optional()
        .map_err(|e| format!("load identity: {e}"))?;
    match row {
        None => Ok(None),
        Some(v) if v.len() == 32 => {
            let mut k = [0u8; 32];
            k.copy_from_slice(&v);
            Ok(Some(k))
        }
        Some(v) => Err(format!("load identity: want 32 bytes, got {}", v.len())),
    }
}

/// Сохранить учётные данные enrolment (upsert единственной строки).
/// user_id — ровно 16 байт, contact_id — 12 символов Crockford; иначе Err
/// (fail-closed, мусор из сети в store не пишем).
pub fn save_account(
    conn: &rusqlite::Connection,
    user_id: &[u8; 16],
    contact_id: &str,
) -> Result<(), String> {
    if contact_id.len() != 12 || !contact_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("save account: bad contact_id".into());
    }
    let n = conn
        .execute(
            "INSERT INTO core_account(id, user_id, contact_id) VALUES(1,?1,?2)
             ON CONFLICT(id) DO UPDATE SET user_id=excluded.user_id, contact_id=excluded.contact_id",
            rusqlite::params![user_id.as_slice(), contact_id],
        )
        .map_err(|e| format!("save account: {e}"))?;
    if n != 1 {
        return Err("save account: no row".into());
    }
    Ok(())
}

/// Загрузить учётные данные. None — enrol ещё не было. Мусор — Err
/// (fail-closed, не silent-None).
pub fn load_account(conn: &rusqlite::Connection) -> Result<Option<([u8; 16], String)>, String> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row("SELECT user_id, contact_id FROM core_account WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(|e| format!("load account: {e}"))?;
    match row {
        None => Ok(None),
        Some((uid, cid))
            if uid.len() == 16
                && cid.len() == 12
                && cid.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            let mut user_id = [0u8; 16];
            user_id.copy_from_slice(&uid);
            Ok(Some((user_id, cid)))
        }
        Some((uid, cid)) => Err(format!(
            "load account: bad row (user {} bytes, contact {:?})",
            uid.len(),
            cid.len()
        )),
    }
}
/// K3: состояние Olm Account. `pickle` — serde_json AccountPickle (тот же
/// файл 0600, что device_priv), `next_key_id` — следующий u32 key_id для
/// one-time ключей (монотонный, гэпы допустимы — сервер делает upsert).
pub fn save_olm(conn: &rusqlite::Connection, pickle: &str, next_key_id: u32) -> Result<(), String> {
    let n = conn
        .execute(
            "INSERT INTO core_olm(id, pickle, next_key_id) VALUES(1,?1,?2)
             ON CONFLICT(id) DO UPDATE SET pickle=excluded.pickle, next_key_id=excluded.next_key_id",
            rusqlite::params![pickle, next_key_id],
        )
        .map_err(|e| format!("save olm: {e}"))?;
    if n != 1 {
        return Err("save olm: no row".into());
    }
    Ok(())
}

/// Загрузить Olm-состояние. None — первый запуск K3 (создать Account).
pub fn load_olm(conn: &rusqlite::Connection) -> Result<Option<(String, u32)>, String> {
    conn.query_row("SELECT pickle, next_key_id FROM core_olm WHERE id=1", [], |r| {
        Ok((r.get(0)?, r.get(0)?))
    })
    .optional()
    .map_err(|e| format!("load olm: {e}"))
}

/// K3: сохранить пикл сессии 1-на-1 + ключи пира на момент создания
/// (для проверки подмены). Upsert по contact_id.
pub fn save_session(
    conn: &rusqlite::Connection,
    contact_id: &str,
    pickle: &str,
    peer_ed: &[u8; 32],
    peer_curve: &[u8; 32],
) -> Result<(), String> {
    let n = conn
        .execute(
            "INSERT INTO core_sessions(contact_id, pickle, peer_ed, peer_curve)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(contact_id) DO UPDATE SET pickle=excluded.pickle,
               peer_ed=excluded.peer_ed, peer_curve=excluded.peer_curve",
            rusqlite::params![contact_id, pickle, peer_ed.as_slice(), peer_curve.as_slice()],
        )
        .map_err(|e| format!("save session: {e}"))?;
    if n != 1 {
        return Err("save session: no row".into());
    }
    Ok(())
}

/// Загрузить сессию: (пикл, peer_ed, peer_curve). None — сессии ещё нет.
pub fn load_session(
    conn: &rusqlite::Connection,
    contact_id: &str,
) -> Result<Option<(String, [u8; 32], [u8; 32])>, String> {
    let row: Option<(String, Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT pickle, peer_ed, peer_curve FROM core_sessions WHERE contact_id=?1",
            [contact_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("load session: {e}"))?;
    match row {
        None => Ok(None),
        Some((p, ed, curve)) if ed.len() == 32 && curve.len() == 32 => {
            let mut e = [0u8; 32];
            let mut c = [0u8; 32];
            e.copy_from_slice(&ed);
            c.copy_from_slice(&curve);
            Ok(Some((p, e, c)))
        }
        Some(_) => Err("load session: bad key len".into()),
    }
}

/// Удалить сессию (после confirm identity — новая сессия со свежими ключами).
pub fn delete_session(conn: &rusqlite::Connection, contact_id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM core_sessions WHERE contact_id=?1", [contact_id])
        .map_err(|e| format!("delete session: {e}"))?;
    Ok(())
}

/// Статус outbox-записи. Ретрай шлёт ТОЛЬКО сохранённый ciphertext.
pub mod outbox_status {
    /// Сообщение зашифровано и сохранено, SEND ещё не принят сервером.
    pub const QUEUED: &str = "queued";
    /// Сервер принял (SEND_ACK ST_ACCEPTED), доставки может не быть.
    pub const ACCEPTED: &str = "accepted";
    /// Сервер подтвердил доставку (SEND_ACK ST_DELIVERED при повторе).
    pub const DELIVERED: &str = "delivered";
}

/// K3: вставить outbox-запись. message_id случаен — дубликат означает
/// программную ошибку, поэтому строгий INSERT (не IGNORE).
pub fn outbox_insert(
    conn: &rusqlite::Connection,
    message_id: &[u8; 16],
    contact_id: &str,
    ciphertext: &[u8],
    status: &str,
) -> Result<(), String> {
    let n = conn
        .execute(
            "INSERT INTO core_outbox(message_id, contact_id, ciphertext, status)
             VALUES(?1,?2,?3,?4)",
            rusqlite::params![message_id.as_slice(), contact_id, ciphertext, status],
        )
        .map_err(|e| format!("outbox insert: {e}"))?;
    if n != 1 {
        return Err("outbox insert: no row".into());
    }
    Ok(())
}

/// Обновить статус outbox-записи (queued→accepted→delivered, только вперёд
/// по смыслу — порядок обеспечивает вызывающий).
pub fn outbox_set_status(
    conn: &rusqlite::Connection,
    message_id: &[u8; 16],
    status: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE core_outbox SET status=?1 WHERE message_id=?2",
        rusqlite::params![status, message_id.as_slice()],
    )
    .map_err(|e| format!("outbox status: {e}"))?;
    Ok(())
}

/// Все недоставленные (queued + accepted) для ретрая тем же ciphertext.
/// Пагинация по контракту FFI: (cursor=rowid, limit) → (rows, next).
/// Row: (rowid, message_id 16, contact_id, ciphertext, status).
pub fn outbox_queued(
    conn: &rusqlite::Connection,
    cursor: i64,
    limit: usize,
) -> Result<(Vec<(i64, [u8; 16], String, Vec<u8>, String)>, Option<i64>), String> {
    let lim = (limit.min(100).max(1) + 1) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT rowid, message_id, contact_id, ciphertext, status FROM core_outbox
             WHERE rowid>?1 AND status IN ('queued','accepted') ORDER BY rowid LIMIT ?2",
        )
        .map_err(|e| format!("outbox list: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![cursor, lim], |r| {
            Ok((r.get(0)?, r.get::<_, Vec<u8>>(1)?, r.get(2)?, r.get::<_, Vec<u8>>(3)?, r.get(4)?))
        })
        .map_err(|e| format!("outbox list: {e}"))?;
    let mut out: Vec<(i64, [u8; 16], String, Vec<u8>, String)> = Vec::new();
    for r in rows {
        let (id, mid, cid, ct, st): (i64, Vec<u8>, String, Vec<u8>, String) =
            r.map_err(|e| format!("outbox row: {e}"))?;
        if mid.len() != 16 {
            return Err("outbox row: bad message_id".into());
        }
        let mut m = [0u8; 16];
        m.copy_from_slice(&mid);
        out.push((id, m, cid, ct, st));
    }
    let next = if out.len() == lim as usize {
        out.pop();
        out.last().map(|r| r.0)
    } else {
        None
    };
    Ok((out, next))
}

/// K3: вставить входящее. Дедуп локальный: повтор (sender,message_id)
/// (replay/reorder с сервера) — IGNORE, одна строка. true = новое.
pub fn inbox_insert_ignore(
    conn: &rusqlite::Connection,
    sender_device: &[u8],
    message_id: &[u8],
    contact_id: &str,
    text: &str,
    seq: i64,
) -> Result<bool, String> {
    let n = conn
        .execute(
            "INSERT INTO core_inbox(sender_device, message_id, contact_id, text, seq)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(sender_device,message_id) DO NOTHING",
            rusqlite::params![sender_device, message_id, contact_id, text, seq],
        )
        .map_err(|e| format!("inbox insert: {e}"))?;
    Ok(n == 1)
}

/// Число входящих строк (для тестов дедупа).
pub fn inbox_count(conn: &rusqlite::Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM core_inbox", [], |r| r.get(0))
        .map_err(|e| format!("inbox count: {e}"))
}

/// Список входящих с пагинацией по контракту FFI: cursor = seq последней
/// выданной строки (0 = сначала), limit ≤ 100. Возврат: rows + next_cursor
/// (None — страниц больше нет). Row: (seq, contact_id, text).
pub fn inbox_list(
    conn: &rusqlite::Connection,
    cursor: i64,
    limit: usize,
) -> Result<(Vec<(i64, String, String)>, Option<i64>), String> {
    let lim = (limit.min(100).max(1) + 1) as i64;
    let mut stmt = conn
        .prepare("SELECT seq, contact_id, text FROM core_inbox WHERE seq>?1 ORDER BY seq LIMIT ?2")
        .map_err(|e| format!("inbox list: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![cursor, lim], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| format!("inbox list: {e}"))?;
    let mut out: Vec<(i64, String, String)> = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("inbox row: {e}"))?);
    }
    let next = if out.len() == lim as usize {
        out.pop();
        out.last().map(|r| r.0)
    } else {
        None
    };
    Ok((out, next))
}
/// K3: сохранить bearer-token invite (upsert). Token — секрет: тот же
/// файл 0600, в логи не попадает. Нужен для ENROL-replay на каждом коннекте
/// (серверный enrolled-флаг живёт per-connection).
pub fn save_token(conn: &rusqlite::Connection, token: &[u8; 32]) -> Result<(), String> {
    let n = conn
        .execute(
            "INSERT INTO core_token(id, token) VALUES(1,?1)
             ON CONFLICT(id) DO UPDATE SET token=excluded.token",
            rusqlite::params![token.as_slice()],
        )
        .map_err(|e| format!("save token: {e}"))?;
    if n != 1 {
        return Err("save token: no row".into());
    }
    Ok(())
}

/// Загрузить token. None — enrol ещё не было. Длина ≠32 — Err (fail-closed).
pub fn load_token(conn: &rusqlite::Connection) -> Result<Option<[u8; 32]>, String> {
    let row: Option<Vec<u8>> = conn
        .query_row("SELECT token FROM core_token WHERE id=1", [], |r| r.get(0))
        .optional()
        .map_err(|e| format!("load token: {e}"))?;
    match row {
        None => Ok(None),
        Some(v) if v.len() == 32 => {
            let mut t = [0u8; 32];
            t.copy_from_slice(&v);
            Ok(Some(t))
        }
        Some(v) => Err(format!("load token: want 32 bytes, got {}", v.len())),
    }
}
/// Минимум `Option::optional` без нового dep: только для single-row SELECT.
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dmsg-core-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        dir.join("core.db")
    }

    fn cleanup(p: &std::path::Path) {
        if let Some(d) = p.parent() {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn identity_roundtrip_and_wal() {
        let p = tmp_db("store1");
        let _ = std::fs::remove_file(&p);
        let conn = open(&p).expect("open");
        assert_eq!(load_identity(&conn).expect("load"), None);
        let mode: String =
            conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).expect("journal_mode");
        assert_eq!(mode, "wal");
        let k = [0xABu8; 32];
        save_identity(&conn, &k).expect("save");
        assert_eq!(load_identity(&conn).expect("reload"), Some(k));
        drop(conn);
        // Персистентность: переоткрытие видит тот же ключ.
        let conn2 = open(&p).expect("reopen");
        assert_eq!(load_identity(&conn2).expect("load2"), Some(k));
        drop(conn2);
        cleanup(&p);
    }

    #[cfg(unix)]
    #[test]
    fn db_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp_db("store2");
        let _ = std::fs::remove_file(&p);
        let conn = open(&p).expect("open");
        let mode = std::fs::metadata(&p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "db file must be 0600");
        drop(conn);
        cleanup(&p);
    }

    #[test]
    fn token_roundtrip_and_strict() {
        let p = tmp_db("store4");
        let _ = std::fs::remove_file(&p);
        let conn = open(&p).expect("open");
        assert_eq!(load_token(&conn).expect("load"), None);
        save_token(&conn, &[0xEEu8; 32]).expect("save");
        assert_eq!(load_token(&conn).expect("reload"), Some([0xEEu8; 32]));
        // Перезапись тем же путём (re-enrol тем же token — upsert).
        save_token(&conn, &[0xFFu8; 32]).expect("overwrite");
        assert_eq!(load_token(&conn).expect("reload2"), Some([0xFFu8; 32]));
        // Мусор fail-closed.
        conn.execute("UPDATE core_token SET token=?1 WHERE id=1", [&[0u8; 5][..]])
            .expect("corrupt");
        assert!(load_token(&conn).is_err(), "short token must fail");
        drop(conn);
        cleanup(&p);
    }

    #[test]
    fn account_roundtrip_and_strict() {
        let p = tmp_db("store3");
        let _ = std::fs::remove_file(&p);
        let conn = open(&p).expect("open");
        assert_eq!(load_account(&conn).expect("load"), None);
        let uid = [0xCDu8; 16];
        save_account(&conn, &uid, "ABCD1234EFGH").expect("save");
        assert_eq!(load_account(&conn).expect("reload"), Some((uid, "ABCD1234EFGH".into())));
        // Мусор fail-closed.
        assert!(save_account(&conn, &[0u8; 16], "short").is_err());
        conn.execute(
            "UPDATE core_account SET user_id=?1 WHERE id=1",
            [&[0u8; 5][..]],
        )
        .expect("corrupt");
        assert!(load_account(&conn).is_err(), "short user_id must fail, not silent-None");
        drop(conn);
        cleanup(&p);
    }
}

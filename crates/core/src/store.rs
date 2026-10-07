//! Unified schema. Only explicitly approved, authenticated 6→7 is upgraded.
//! Same-schema plain-to-sealed conversion is a storage feature, not auth migration.
pub const SCHEMA_VERSION: i64 = 7;
const ORDER_INDEXES:&str="CREATE UNIQUE INDEX core_history_server_seq ON core_history(server_seq) WHERE server_seq IS NOT NULL;
 CREATE INDEX core_history_timeline ON core_history(contact_id,(CASE WHEN server_seq IS NOT NULL THEN 1 WHEN delivery_state='queued' THEN 2 ELSE 0 END),coalesce(server_seq,local_id));";

/// Internal plain storage for isolated Rust harnesses.
pub fn open(path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    open_mode(path, None)
}

/// Шифрованное хранилище. Ключ ровно 32 байта; его создаёт и запечатывает
/// Android Keystore. Неверный ключ/повреждённый marker не создают новую БД.
/// При миграции дождитесь закрытия других SQLite connections; при занятом
/// WAL cleanup_pending блокирует открытие до успешного checkpoint/VACUUM.
pub fn open_encrypted(path: &std::path::Path, key: &[u8]) -> Result<rusqlite::Connection, String> {
    let key: [u8; 32] = key.try_into().map_err(|_| "storage key must be 32 bytes")?;
    open_mode(path, Some(key))
}

fn open_mode(
    path: &std::path::Path,
    key: Option<[u8; 32]>,
) -> Result<rusqlite::Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
    }
    let mut conn = rusqlite::Connection::open(path).map_err(|e| format!("open: {e}"))?;
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
    if version != SCHEMA_VERSION && version != 6 && !(version == 0 && !nonempty) {
        return Err("unsupported core schema".into());
    }
    // Check the marker BEFORE schema creation or any write, including on the
    // plain harness entrypoint. SQLite reads an existing WAL on opening.
    let marker_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='core_storage')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "storage marker lookup failed")?;
    let marker: Option<(i64, Vec<u8>, i64)> = if marker_exists {
        conn.query_row(
            "SELECT version, verifier, cleanup_pending FROM core_storage WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| "storage marker corrupt")?
    } else {
        None
    };
    if marker_exists && marker.is_none() {
        return Err("storage marker missing".into());
    }
    match (&key, &marker) {
        (None, Some(_)) => return Err("encrypted storage requires key".into()),
        (Some(k), Some((1, verifier, pending))) if *pending == 0 || *pending == 1 => {
            crate::secure::verify(k, verifier)?;
        }
        (Some(_), Some(_)) => return Err("unsupported storage format".into()),
        _ => {}
    }
    crate::secure::register(&conn, key)?;
    if version == 6 && marker.is_none() {
        return Err("history upgrade requires authenticated storage".into());
    }
    if version == 0 {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| "schema transaction failed")?;
        let locked_version: i64 = tx
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| "schema lookup failed")?;
        let locked_nonempty: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name NOT LIKE 'sqlite_%')",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "schema lookup failed")?;
        if locked_version != SCHEMA_VERSION && !(locked_version == 0 && !locked_nonempty) {
            return Err("unsupported core schema".into());
        }
        if locked_version == 0 {
            tx.execute_batch(
        "CREATE TABLE core_identity(
           id INTEGER PRIMARY KEY CHECK(id=1),
           device_priv BLOB NOT NULL
         );
          CREATE TABLE core_account(
           id INTEGER PRIMARY KEY CHECK(id=1),
           user_id BLOB NOT NULL,
           contact_id TEXT NOT NULL
         );
          CREATE TABLE core_olm(
           id INTEGER PRIMARY KEY CHECK(id=1),
           pickle TEXT NOT NULL,
           next_key_id INTEGER NOT NULL
         );
          CREATE TABLE core_sessions(
           contact_id TEXT PRIMARY KEY,
           pickle TEXT NOT NULL,
           peer_ed BLOB NOT NULL,
           peer_curve BLOB NOT NULL
         );
          CREATE TABLE core_contacts(
           contact_id TEXT PRIMARY KEY,
           user_id BLOB,
           device_key BLOB,
           ed_identity BLOB,
           curve_identity BLOB,
            state TEXT NOT NULL,
            seen_user BLOB,
            seen_device BLOB,
            seen_ed BLOB,
            seen_curve BLOB,
            local_alias TEXT,
            read_cursor INTEGER NOT NULL DEFAULT 0 CHECK(read_cursor>=0),
            local_activity_ms INTEGER NOT NULL DEFAULT 0 CHECK(local_activity_ms>=0)
         );
          CREATE TABLE core_outbox(
           message_id BLOB PRIMARY KEY,
           contact_id TEXT NOT NULL,
           ciphertext BLOB NOT NULL,
           status TEXT NOT NULL
         );
          CREATE TABLE core_inbox(
           sender_device BLOB NOT NULL,
           message_id BLOB NOT NULL,
           contact_id TEXT NOT NULL,
           text TEXT NOT NULL,
           seq INTEGER NOT NULL,
            PRIMARY KEY(sender_device, message_id)
          );
          CREATE TABLE core_history(
            local_id INTEGER PRIMARY KEY AUTOINCREMENT,
            message_id BLOB NOT NULL CHECK(length(message_id)=16),
            contact_id TEXT NOT NULL,
            direction TEXT NOT NULL CHECK(direction IN ('incoming','outgoing')),
            sender_device BLOB,
            text TEXT NOT NULL,
            local_timestamp_ms INTEGER NOT NULL CHECK(local_timestamp_ms>=0),
             delivery_state TEXT,
             server_seq INTEGER CHECK(server_seq>0),
             server_timestamp_ms INTEGER CHECK(server_timestamp_ms>=0),
             order_checked INTEGER NOT NULL DEFAULT 0 CHECK(order_checked IN (0,1)),
             CHECK((server_seq IS NULL)=(server_timestamp_ms IS NULL)),
            CHECK((direction='incoming' AND sender_device IS NOT NULL AND length(sender_device)=32 AND delivery_state IS NULL)
               OR (direction='outgoing' AND sender_device IS NULL AND delivery_state IS NOT NULL AND delivery_state IN ('queued','accepted','delivered')))
          );
          CREATE UNIQUE INDEX core_history_outgoing ON core_history(message_id) WHERE direction='outgoing';
          CREATE UNIQUE INDEX core_history_incoming ON core_history(sender_device,message_id) WHERE direction='incoming';
          CREATE INDEX core_history_contact ON core_history(contact_id,local_id DESC);
          CREATE INDEX core_history_unread ON core_history(contact_id,local_id) WHERE direction='incoming';
          CREATE INDEX core_dialog_activity ON core_contacts(local_activity_ms DESC,contact_id ASC);
           CREATE TABLE core_dns_profile(
            id INTEGER PRIMARY KEY CHECK(id=1),
            profile TEXT NOT NULL
           );
           CREATE UNIQUE INDEX core_contact_user ON core_contacts(user_id) WHERE user_id IS NOT NULL;
            PRAGMA user_version=7;",
    )
    .map_err(|_| "create schema v7 failed")?;
        }
        if locked_version == 0 {
            tx.execute_batch(ORDER_INDEXES)
                .map_err(|_| "history order index failed")?;
        }
        tx.commit().map_err(|_| "schema commit failed")?;
    }
    if version == 6 {
        // Marker/key verification above precedes ALL upgrade writes. DDL and
        // version change commit together; existing opaque values/IDs untouched.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| "history upgrade transaction failed")?;
        let locked: i64 = tx
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| "history upgrade version failed")?;
        if locked == 6 {
            // Reject malformed lookalike schema rather than manufacture data.
            let _: i64 = tx
                .query_row(
                    "SELECT count(*) FROM core_history WHERE local_id>0",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| "history upgrade schema invalid")?;
            tx.execute_batch("ALTER TABLE core_history ADD COLUMN server_seq INTEGER CHECK(server_seq>0);
                ALTER TABLE core_history ADD COLUMN server_timestamp_ms INTEGER CHECK(server_timestamp_ms>=0 AND ((server_seq IS NULL)=(server_timestamp_ms IS NULL)));
                ALTER TABLE core_history ADD COLUMN order_checked INTEGER NOT NULL DEFAULT 0 CHECK(order_checked IN (0,1));
                PRAGMA user_version=7;").map_err(|_|"history upgrade failed")?;
            tx.execute_batch(ORDER_INDEXES)
                .map_err(|_| "history order index failed")?;
        } else if locked != SCHEMA_VERSION {
            return Err("unsupported core schema".into());
        }
        tx.commit().map_err(|_| "history upgrade commit failed")?;
    }
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .map_err(|_| "storage durability setup failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod: {e}"))?;
    }
    if let Some(k) = key {
        if marker.is_none() {
            // A missing/deleted marker must never re-encrypt existing sealed
            // values as if they were plain current-schema storage.
            let already_sealed: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM core_identity WHERE substr(device_priv,1,7)=?1)
                     OR EXISTS(SELECT 1 FROM core_olm WHERE substr(pickle,1,7)=?1)
                     OR EXISTS(SELECT 1 FROM core_sessions WHERE substr(pickle,1,7)=?1)
                      OR EXISTS(SELECT 1 FROM core_inbox WHERE substr(text,1,7)=?1)
                       OR EXISTS(SELECT 1 FROM core_dns_profile WHERE substr(profile,1,7)=?1)
                       OR EXISTS(SELECT 1 FROM core_history WHERE substr(text,1,7)=?1)
                       OR EXISTS(SELECT 1 FROM core_contacts WHERE substr(local_alias,1,7)=?1)",
                    [b"DMSG-S1".as_slice()],
                    |r| r.get(0),
                )
                .map_err(|_| "storage format detection failed")?;
            if already_sealed {
                return Err("storage marker missing for encrypted values".into());
            }
            // No per-column changes are visible unless all of them AND the
            // authenticated marker commit together. A failed TX leaves the
            // entire plain current-schema DB readable via the original entrypoint.
            conn.execute_batch("PRAGMA secure_delete=ON; BEGIN EXCLUSIVE;")
                .map_err(|_| "storage migration begin failed")?;
            let migrate = (|| -> Result<(), String> {
                for (table, column, field) in [
                    ("core_identity", "device_priv", "device_priv"),
                    ("core_olm", "pickle", "olm_pickle"),
                    ("core_sessions", "pickle", "session_pickle"),
                    ("core_inbox", "text", "inbox_text"),
                    ("core_dns_profile", "profile", "dns_profile"),
                    ("core_history", "text", "history_text"),
                    ("core_contacts", "local_alias", "contact_alias"),
                ] {
                    conn.execute(
                        &format!("UPDATE {table} SET {column}=dmsg_seal('{field}', {column}) WHERE {column} IS NOT NULL"),
                        [],
                    )
                    .map_err(|_| "storage migration field failed")?;
                }
                // Stale plain handles may outlive this sealing. SQLite
                // reloads the schema after COMMIT; reject their plaintext
                // writes instead of mixing formats in the sealed DB.
                for (table, column) in [
                    ("core_identity", "device_priv"),
                    ("core_olm", "pickle"),
                    ("core_sessions", "pickle"),
                    ("core_inbox", "text"),
                    ("core_dns_profile", "profile"),
                    ("core_history", "text"),
                    ("core_contacts", "local_alias"),
                ] {
                    for action in ["INSERT", "UPDATE"] {
                        conn.execute_batch(&format!(
                            "CREATE TRIGGER {table}_sealed_{action} BEFORE {action} ON {table}
                             WHEN NEW.{column} IS NOT NULL AND (typeof(NEW.{column}) != 'blob' OR
                                  substr(NEW.{column},1,7) != x'444d53472d5331')
                             BEGIN SELECT RAISE(ABORT,'unencrypted storage write'); END;"
                        ))
                        .map_err(|_| "storage migration guard failed")?;
                    }
                }
                conn.execute_batch("CREATE TABLE core_storage(id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL, verifier BLOB NOT NULL, cleanup_pending INTEGER NOT NULL);")
                    .map_err(|_| "storage migration marker failed")?;
                let verifier = crate::secure::verifier(&k)?;
                conn.execute("INSERT INTO core_storage VALUES(1,1,?1,1)", [verifier])
                    .map_err(|_| "storage migration marker failed")?;
                conn.execute_batch("COMMIT")
                    .map_err(|_| "storage migration commit failed".into())
            })();
            if let Err(e) = migrate {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
        if marker.as_ref().is_none_or(|m| m.2 == 1) {
            // WAL may contain the pre-migration pages. A busy checkpoint is
            // NOT success: do not admit the connection until it can be
            // truncated, VACUUMed and truncated again. Pending marker makes
            // interruption/restart resume cleanup without re-encryption.
            conn.execute_batch("PRAGMA secure_delete=ON;")
                .map_err(|_| "storage cleanup failed")?;
            checkpoint(&conn)?;
            conn.execute_batch("VACUUM")
                .map_err(|_| "storage vacuum failed; cleanup pending")?;
            checkpoint(&conn)?;
            conn.execute("UPDATE core_storage SET cleanup_pending=0 WHERE id=1", [])
                .map_err(|_| "storage cleanup marker failed")?;
        }
    }
    Ok(conn)
}

fn checkpoint(conn: &rusqlite::Connection) -> Result<(), String> {
    let (busy, _, _): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|_| "storage WAL checkpoint failed; cleanup pending")?;
    if busy != 0 {
        return Err("storage WAL checkpoint busy; cleanup pending".into());
    }
    Ok(())
}

/// Сохранить Noise static-приватник устройства (upsert единственной строки).
/// В логи ключ не попадает — только rowcount в ошибке.
pub fn save_identity(conn: &rusqlite::Connection, privkey: &[u8; 32]) -> Result<(), String> {
    if load_identity(conn)?.is_some_and(|old| old != *privkey) {
        return Err("identity cannot be replaced".into());
    }
    let n = conn
        .execute(
            "INSERT INTO core_identity(id, device_priv) VALUES(1,dmsg_seal('device_priv',?1))
              ON CONFLICT(id) DO NOTHING",
            rusqlite::params![privkey.as_slice()],
        )
        .map_err(|e| format!("save identity: {e}"))?;
    if n != 1 && load_identity(conn)? != Some(*privkey) {
        return Err("save identity: no row".into());
    }
    Ok(())
}

/// Load pending or authenticated device key. Invalid lengths fail closed.
/// Мусор длиной ≠32 — Err (fail-closed, не silent-None).
pub fn load_identity(conn: &rusqlite::Connection) -> Result<Option<[u8; 32]>, String> {
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT dmsg_unseal('device_priv',device_priv) FROM core_identity WHERE id=1",
            [],
            |r| r.get(0),
        )
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

/// Accept account atomically; a foreign account can never overwrite it.
/// user_id — ровно 16 байт, contact_id — 12 символов Crockford; иначе Err
/// (fail-closed, мусор из сети в store не пишем).
pub fn save_account(
    conn: &rusqlite::Connection,
    user_id: &[u8; 16],
    contact_id: &str,
) -> Result<(), String> {
    if dmsg_protocol::auth::build_authenticated(user_id, contact_id).is_err() {
        return Err("save account: bad contact_id".into());
    }
    let n = conn
        .execute(
            "INSERT INTO core_account(id, user_id, contact_id) VALUES(1,?1,?2)
              ON CONFLICT(id) DO NOTHING",
            rusqlite::params![user_id.as_slice(), contact_id],
        )
        .map_err(|e| format!("save account: {e}"))?;
    if n != 1 && load_account(conn)? != Some((*user_id, contact_id.to_owned())) {
        return Err("account cannot be replaced".into());
    }
    Ok(())
}

/// Load accepted account; malformed rows fail closed.
/// (fail-closed, не silent-None).
pub fn load_account(conn: &rusqlite::Connection) -> Result<Option<([u8; 16], String)>, String> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT user_id, contact_id FROM core_account WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("load account: {e}"))?;
    match row {
        None => Ok(None),
        Some((uid, cid))
            if uid.len() == 16
                && cid.len() == 12
                && dmsg_protocol::auth::build_authenticated(&[0; 16], &cid).is_ok() =>
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
            "INSERT INTO core_olm(id, pickle, next_key_id) VALUES(1,dmsg_seal('olm_pickle',?1),?2)
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
    conn.query_row("SELECT CAST(dmsg_unseal('olm_pickle',pickle) AS TEXT), next_key_id FROM core_olm WHERE id=1", [], |r| {
        Ok((r.get(0)?, r.get(1)?))
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
             VALUES(?1,dmsg_seal('session_pickle',?2),?3,?4)
             ON CONFLICT(contact_id) DO UPDATE SET pickle=excluded.pickle,
               peer_ed=excluded.peer_ed, peer_curve=excluded.peer_curve",
            rusqlite::params![
                contact_id,
                pickle,
                peer_ed.as_slice(),
                peer_curve.as_slice()
            ],
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
            "SELECT CAST(dmsg_unseal('session_pickle',pickle) AS TEXT), peer_ed, peer_curve FROM core_sessions WHERE contact_id=?1",
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
    conn.execute(
        "DELETE FROM core_sessions WHERE contact_id=?1",
        [contact_id],
    )
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

/// Advance the exact outgoing history and outbox status in one transaction.
/// Replayed/late ACKs never regress delivered or accepted to an earlier state.
pub fn outbox_set_status(
    conn: &rusqlite::Connection,
    message_id: &[u8; 16],
    status: &str,
) -> Result<(), String> {
    outbox_set_status_order(conn, message_id, status, None)
}

pub(crate) fn outbox_set_status_order(
    conn: &rusqlite::Connection,
    message_id: &[u8; 16],
    status: &str,
    order: Option<dmsg_protocol::chronology::Order>,
) -> Result<(), String> {
    if !matches!(status, "queued" | "accepted" | "delivered") {
        return Err("invalid delivery state".into());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "delivery transaction failed")?;
    if let Some(order) = order {
        let id: i64 = tx
            .query_row(
                "SELECT local_id FROM core_history WHERE message_id=?1 AND direction='outgoing'",
                [message_id.as_slice()],
                |r| r.get(0),
            )
            .map_err(|_| "history order row missing")?;
        crate::history::set_order(&tx, id, Some(order))?;
    }
    for (table, column, condition) in [
        ("core_outbox", "status", ""),
        (
            "core_history",
            "delivery_state",
            " AND direction='outgoing'",
        ),
    ] {
        let n = tx
            .execute(
                &format!(
                    "UPDATE {table} SET {column}=CASE
                WHEN {column}='delivered' OR ?1='delivered' THEN 'delivered'
                WHEN {column}='accepted' OR ?1='accepted' THEN 'accepted'
                ELSE 'queued' END WHERE message_id=?2{condition}"
                ),
                rusqlite::params![status, message_id.as_slice()],
            )
            .map_err(|_| "delivery update failed")?;
        if n != 1 {
            return Err("outgoing delivery record missing".into());
        }
    }
    tx.commit().map_err(|_| "delivery commit failed".into())
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
            Ok((
                r.get(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get(4)?,
            ))
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
             VALUES(?1,?2,?3,dmsg_seal('inbox_text',?4),?5) ON CONFLICT(sender_device,message_id) DO NOTHING",
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
        .prepare("SELECT seq, contact_id, CAST(dmsg_unseal('inbox_text',text) AS TEXT) FROM core_inbox WHERE seq>?1 ORDER BY seq LIMIT ?2")
        .map_err(|e| format!("inbox list: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![cursor, lim], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
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

    #[test]
    fn old_future_and_nonempty_unversioned_schemas_are_rejected_without_mutation() {
        for version in [0, 1, 2, 3, 4, 5, SCHEMA_VERSION + 1, 99] {
            let dir =
                std::env::temp_dir().join(format!("dmsg-schema-{}-{version}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("core.db");
            let c = rusqlite::Connection::open(&p).unwrap();
            c.execute_batch(&format!("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('keep'); PRAGMA user_version={version};")).unwrap();
            drop(c);
            let before = std::fs::read(&p).unwrap();
            assert_eq!(open(&p).unwrap_err(), "unsupported core schema");
            assert_eq!(
                open_encrypted(&p, &[7; 32]).unwrap_err(),
                "unsupported core schema"
            );
            assert_eq!(before, std::fs::read(&p).unwrap());
            let c = rusqlite::Connection::open(&p).unwrap();
            assert_eq!(
                c.query_row::<String, _, _>("SELECT value FROM sentinel", [], |r| r.get(0))
                    .unwrap(),
                "keep"
            );
            assert_eq!(
                c.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                    .unwrap(),
                version
            );
            drop(c);
            let _ = std::fs::remove_dir_all(dir);
        }
        let dir = std::env::temp_dir().join(format!("dmsg-schema-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("core.db");
        let c = open(&p).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    const KEY: [u8; 32] = [0x37; 32];

    #[test]
    fn approved_six_to_seven_is_authenticated_atomic_and_preserves_opaque_rows() {
        let p = tmp_db("chronology-upgrade");
        cleanup(&p);
        let c = open_encrypted(&p, &KEY).unwrap();
        save_identity(&c, &[3; 32]).unwrap();
        save_account(&c, &[4; 16], "PEER00000001").unwrap();
        c.execute(
            "INSERT INTO core_contacts(contact_id,state) VALUES('PEER00000001','accepted')",
            [],
        )
        .unwrap();
        let tx = rusqlite::Transaction::new_unchecked(&c, rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::history::insert(&tx, &[8; 16], "PEER00000001", None, "keep encrypted text").unwrap();
        tx.commit().unwrap();
        c.execute(
            "UPDATE core_contacts SET read_cursor=1 WHERE contact_id='PEER00000001'",
            [],
        )
        .unwrap();
        let opaque: Vec<u8> = c
            .query_row("SELECT text FROM core_history", [], |r| r.get(0))
            .unwrap();
        let identity: Vec<u8> = c
            .query_row("SELECT device_priv FROM core_identity", [], |r| r.get(0))
            .unwrap();
        // Exact prior history schema, with the original sealed bytes/local IDs.
        c.execute_batch("DROP INDEX core_history_server_seq; DROP INDEX core_history_timeline;
            ALTER TABLE core_history RENAME TO history7;
            CREATE TABLE core_history(local_id INTEGER PRIMARY KEY AUTOINCREMENT,message_id BLOB NOT NULL CHECK(length(message_id)=16),contact_id TEXT NOT NULL,sender_device BLOB,direction TEXT NOT NULL CHECK(direction IN ('incoming','outgoing')),text BLOB NOT NULL,local_timestamp_ms INTEGER NOT NULL CHECK(local_timestamp_ms>=0),delivery_state TEXT, UNIQUE(sender_device,message_id));
            INSERT INTO core_history(local_id,message_id,contact_id,sender_device,direction,text,local_timestamp_ms,delivery_state) SELECT local_id,message_id,contact_id,sender_device,direction,text,local_timestamp_ms,delivery_state FROM history7;
            DROP TABLE history7;
            CREATE UNIQUE INDEX core_history_outgoing_mid ON core_history(message_id) WHERE direction='outgoing';
            CREATE INDEX core_history_contact_local ON core_history(contact_id,local_id DESC);
            PRAGMA user_version=6;").unwrap();
        drop(c);
        assert!(open_encrypted(&p, &[1; 32]).is_err());
        let raw = rusqlite::Connection::open(&p).unwrap();
        assert_eq!(
            raw.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            6
        );
        drop(raw);
        let c = open_encrypted(&p, &KEY).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            7
        );
        assert_eq!(
            opaque,
            c.query_row::<Vec<u8>, _, _>("SELECT text FROM core_history", [], |r| r.get(0))
                .unwrap()
        );
        assert_eq!(
            identity,
            c.query_row::<Vec<u8>, _, _>("SELECT device_priv FROM core_identity", [], |r| r.get(0))
                .unwrap()
        );
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT read_cursor FROM core_contacts", [], |r| r.get(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row::<(i64, Option<i64>, Option<i64>, i64), _, _>(
                "SELECT local_id,server_seq,server_timestamp_ms,order_checked FROM core_history",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            )
            .unwrap(),
            (1, None, None, 0)
        );
        drop(c);
        assert!(open_encrypted(&p, &KEY).is_ok());
        cleanup(&p);
    }

    #[test]
    fn unauthenticated_six_is_not_upgraded_or_sealed() {
        let path = tmp_db("chronology-unauthenticated");
        cleanup(&path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE sentinel(value BLOB); INSERT INTO sentinel VALUES(x'123456'); PRAGMA user_version=6;").unwrap();
        drop(c);
        let before = std::fs::read(&path).unwrap();
        assert!(open(&path).err().unwrap().contains("authenticated storage"));
        assert!(open_encrypted(&path, &KEY)
            .err()
            .unwrap()
            .contains("authenticated storage"));
        assert_eq!(before, std::fs::read(&path).unwrap());
        let c = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert_eq!(
            c.query_row("SELECT hex(value) FROM sentinel", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "123456"
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop(c);
        cleanup(&path);
    }

    #[test]
    fn same_current_schema_plain_to_sealed_seals_every_sensitive_column_and_reopens() {
        let p = tmp_db("encrypted-migration");
        cleanup(&p);
        let legacy = open(&p).expect("legacy");
        let device = [0x41; 32];
        let account = vodozemac::olm::Account::new();
        let pickle = serde_json::to_string(&account.pickle()).expect("pickle");
        let session_account = vodozemac::olm::Account::new();
        let mut recipient = vodozemac::olm::Account::new();
        recipient.generate_one_time_keys(1);
        let ot = *recipient
            .one_time_keys()
            .values()
            .next()
            .unwrap()
            .as_bytes();
        let session = session_account
            .create_outbound_session(
                vodozemac::olm::SessionConfig::version_1(),
                recipient.curve25519_key(),
                vodozemac::Curve25519PublicKey::from_bytes(ot),
            )
            .unwrap();
        let spickle = crate::olm::pickle_session(&session).unwrap();
        save_identity(&legacy, &device).unwrap();
        save_olm(&legacy, &pickle, 18).unwrap();
        save_session(&legacy, "ABCD1234EFGH", &spickle, &[1; 32], &[2; 32]).unwrap();
        inbox_insert_ignore(
            &legacy,
            &[5; 32],
            &[6; 16],
            "ABCD1234EFGH",
            "private inbox sentinel",
            1,
        )
        .unwrap();
        crate::contacts::request_add(&legacy, "ABCD1234EFGH").unwrap();
        crate::history::set_contact_alias(&legacy, "ABCD1234EFGH", Some("private alias sentinel"))
            .unwrap();
        let tx =
            rusqlite::Transaction::new_unchecked(&legacy, rusqlite::TransactionBehavior::Immediate)
                .unwrap();
        crate::history::insert(
            &tx,
            &[8; 16],
            "ABCD1234EFGH",
            None,
            "private outgoing history sentinel",
        )
        .unwrap();
        tx.commit().unwrap();
        drop(legacy);
        let stale_legacy_handle = open(&p).unwrap();

        let encrypted = open_encrypted(&p, &KEY).expect("migrate");
        assert!(
            save_olm(&stale_legacy_handle, &pickle, 18).is_err(),
            "old handle must not rewrite plaintext"
        );
        assert!(crate::history::set_contact_alias(
            &stale_legacy_handle,
            "ABCD1234EFGH",
            Some("plaintext stale alias")
        )
        .is_err());
        drop(stale_legacy_handle);
        assert_eq!(load_identity(&encrypted).unwrap(), Some(device));
        assert_eq!(load_olm(&encrypted).unwrap(), Some((pickle.clone(), 18)));
        assert_eq!(
            load_session(&encrypted, "ABCD1234EFGH").unwrap().unwrap().0,
            spickle
        );
        assert_eq!(
            inbox_list(&encrypted, 0, 10).unwrap().0[0].2,
            "private inbox sentinel"
        );
        assert_eq!(
            crate::history::history_page(&encrypted, "ABCD1234EFGH", None, 10)
                .unwrap()
                .rows[0]
                .text,
            "private outgoing history sentinel"
        );
        assert_eq!(
            crate::history::dialogs_page(&encrypted, None, 10)
                .unwrap()
                .rows[0]
                .local_alias
                .as_deref(),
            Some("private alias sentinel")
        );
        let first: Vec<u8> = encrypted
            .query_row("SELECT pickle FROM core_olm", [], |r| r.get(0))
            .unwrap();
        save_olm(&encrypted, &pickle, 18).unwrap();
        let second: Vec<u8> = encrypted
            .query_row("SELECT pickle FROM core_olm", [], |r| r.get(0))
            .unwrap();
        assert_ne!(first, second, "a new nonce per write");
        drop(encrypted);

        for file in [p.clone(), p.with_extension("db-wal")] {
            if let Ok(raw) = std::fs::read(file) {
                for secret in [
                    &device[..],
                    pickle.as_bytes(),
                    spickle.as_bytes(),
                    b"private inbox sentinel",
                    b"private alias sentinel",
                    b"private outgoing history sentinel",
                ] {
                    assert!(
                        !raw.windows(secret.len()).any(|w| w == secret),
                        "plaintext on disk"
                    );
                }
            }
        }
        let reopened = open_encrypted(&p, &KEY).expect("same key");
        assert_eq!(load_olm(&reopened).unwrap().unwrap().1, 18);
        assert_eq!(
            load_session(&reopened, "ABCD1234EFGH").unwrap().unwrap().0,
            spickle
        );
        assert!(open(&p).is_err(), "plaintext entrypoint rejects sealed DB");
        assert!(open_encrypted(&p, &[8; 32])
            .unwrap_err()
            .contains("wrong storage key"));
        let saved: Vec<u8> = reopened
            .query_row("SELECT pickle FROM core_olm", [], |r| r.get(0))
            .unwrap();
        let mut broken = saved;
        *broken.last_mut().unwrap() ^= 1;
        reopened
            .execute("UPDATE core_olm SET pickle=?1", [broken])
            .unwrap();
        assert!(load_olm(&reopened)
            .unwrap_err()
            .contains("authentication failed"));
        drop(reopened);
        assert!(load_olm(&open_encrypted(&p, &KEY).unwrap()).is_err());
        let raw = rusqlite::Connection::open(&p).unwrap();
        raw.execute("DELETE FROM core_storage", []).unwrap();
        drop(raw);
        assert!(open_encrypted(&p, &KEY)
            .unwrap_err()
            .contains("storage marker"));
        cleanup(&p);
    }

    #[test]
    fn failed_sealing_preserves_plain_current_schema_and_pending_cleanup_is_retryable() {
        let p = tmp_db("encrypted-failure");
        cleanup(&p);
        let conn = open(&p).unwrap();
        save_identity(&conn, &[3; 32]).unwrap();
        save_olm(&conn, "test pickle", 1).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_migration BEFORE UPDATE ON core_olm
                            BEGIN SELECT RAISE(ABORT,'denied'); END;",
        )
        .unwrap();
        drop(conn);
        assert!(open_encrypted(&p, &KEY).is_err());
        let legacy = open(&p).expect("rollback left DB plaintext");
        assert_eq!(load_identity(&legacy).unwrap(), Some([3; 32]));
        assert_eq!(load_olm(&legacy).unwrap(), Some(("test pickle".into(), 1)));
        legacy
            .execute_batch("DROP TRIGGER reject_migration")
            .unwrap();
        drop(legacy);
        let encrypted = open_encrypted(&p, &KEY).expect("migration retry");
        encrypted
            .execute("UPDATE core_storage SET cleanup_pending=1", [])
            .unwrap();
        drop(encrypted);
        let retried = open_encrypted(&p, &KEY).expect("interrupted cleanup resumed");
        assert_eq!(load_olm(&retried).unwrap(), Some(("test pickle".into(), 1)));
        let pending: i64 = retried
            .query_row("SELECT cleanup_pending FROM core_storage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 0);
        drop(retried);
        cleanup(&p);
    }

    #[test]
    fn active_plain_reader_blocks_wal_cleanup_until_retry() {
        let p = tmp_db("encrypted-busy-reader");
        cleanup(&p);
        let writer = open(&p).unwrap();
        save_identity(&writer, &[7; 32]).unwrap();
        drop(writer);
        let reader = rusqlite::Connection::open(&p).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: Vec<u8> = reader
            .query_row(
                "SELECT device_priv FROM core_identity WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let error = open_encrypted(&p, &KEY).unwrap_err();
        assert!(
            error.contains("checkpoint busy"),
            "unexpected cleanup error: {error}"
        );
        assert!(
            open(&p).is_err(),
            "pending migration must reject legacy open"
        );
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        let encrypted = open_encrypted(&p, &KEY).expect("cleanup after reader closes");
        assert_eq!(load_identity(&encrypted).unwrap(), Some([7; 32]));
        drop(encrypted);
        cleanup(&p);
    }

    fn tmp_db(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dmsg-core-{}-{name}", std::process::id()));
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
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("journal_mode");
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
    fn olm_roundtrip_survives_reopen() {
        let p = tmp_db("olm-roundtrip");
        let _ = std::fs::remove_file(&p);
        let conn = open(&p).expect("open");
        assert_eq!(load_olm(&conn).expect("empty"), None);
        let (account, next) = crate::olm::load_or_create(&conn).expect("create olm");
        assert_eq!(next, 1);
        let keys = account.identity_keys();
        // Exercise the complete u32 range as well as the distinct SQL types.
        crate::olm::persist(&conn, &account, u32::MAX).expect("persist olm");
        let saved = load_olm(&conn).expect("reload").expect("olm row");
        assert_eq!(saved.1, u32::MAX);
        drop(conn);

        let reopened = open(&p).expect("reopen");
        assert_eq!(
            load_olm(&reopened).expect("reload after reopen"),
            Some(saved)
        );
        let (restored, next) = crate::olm::load_or_create(&reopened).expect("restore olm");
        assert_eq!(next, u32::MAX);
        assert_eq!(restored.identity_keys(), keys);
        drop(reopened);
        cleanup(&p);
    }

    #[test]
    fn immutable_identity_rejects_replacement_and_invalid_length() {
        let p = tmp_db("store4");
        let _ = std::fs::remove_file(&p);
        let conn = open(&p).expect("open");
        assert_eq!(load_identity(&conn).expect("load"), None);
        save_identity(&conn, &[0xEEu8; 32]).expect("save");
        assert_eq!(load_identity(&conn).expect("reload"), Some([0xEEu8; 32]));
        assert!(save_identity(&conn, &[0xFFu8; 32]).is_err());
        assert_eq!(load_identity(&conn).expect("reload2"), Some([0xEEu8; 32]));
        // Мусор fail-closed.
        conn.execute(
            "UPDATE core_identity SET device_priv=?1 WHERE id=1",
            [&[0u8; 5][..]],
        )
        .expect("corrupt");
        assert!(load_identity(&conn).is_err(), "short identity must fail");
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
        assert_eq!(
            load_account(&conn).expect("reload"),
            Some((uid, "ABCD1234EFGH".into()))
        );
        // Мусор fail-closed.
        assert!(save_account(&conn, &[0u8; 16], "short").is_err());
        conn.execute(
            "UPDATE core_account SET user_id=?1 WHERE id=1",
            [&[0u8; 5][..]],
        )
        .expect("corrupt");
        assert!(
            load_account(&conn).is_err(),
            "short user_id must fail, not silent-None"
        );
        drop(conn);
        cleanup(&p);
    }
}

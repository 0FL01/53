//! SQLite: открытие (WAL + synchronous=FULL), миграции схемы v1→v4.
//! v2: cursors (закладка получателя). v3: device_identities (binding prekeys).
//! v4: account rebind invites, one active device per account.

use rusqlite::Connection;
use std::path::Path;

/// Текущая версия схемы.
pub const SCHEMA_VERSION: i64 = 4;

const MIGRATION_V1: &str = "
CREATE TABLE IF NOT EXISTS meta(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS users(
  user_id BLOB PRIMARY KEY,
  contact_id TEXT UNIQUE,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices(
  device_key BLOB PRIMARY KEY,
  user_id BLOB,
  created_at INTEGER NOT NULL,
  revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS invites(
  token BLOB PRIMARY KEY,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  bound_device_key BLOB,
  revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS contact_permissions(
  user_id BLOB NOT NULL,
  peer_user_id BLOB NOT NULL,
  state TEXT NOT NULL,
  PRIMARY KEY(user_id, peer_user_id)
);
CREATE TABLE IF NOT EXISTS prekeys(
  device_key BLOB NOT NULL,
  key_id INTEGER NOT NULL,
  pubkey BLOB NOT NULL,
  signature BLOB NOT NULL,
  one_time INTEGER NOT NULL,
  consumed INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(device_key, key_id)
);
CREATE TABLE IF NOT EXISTS mailbox_events(
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  recipient_user_id BLOB NOT NULL,
  sender_device BLOB NOT NULL,
  message_id BLOB NOT NULL,
  ciphertext BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  delivered INTEGER NOT NULL DEFAULT 0,
  UNIQUE(sender_device, message_id)
);
CREATE TABLE IF NOT EXISTS blob_meta(
  blob_id BLOB PRIMARY KEY,
  owner_user_id BLOB NOT NULL,
  size INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  state TEXT NOT NULL
);
";

const MIGRATION_V2: &str = "
CREATE TABLE IF NOT EXISTS cursors(
  recipient_user_id BLOB NOT NULL,
  device_key BLOB NOT NULL,
  last_seq INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(recipient_user_id, device_key)
);
";

const MIGRATION_V3: &str = "
CREATE TABLE IF NOT EXISTS device_identities(
  device_key BLOB PRIMARY KEY,
  user_id BLOB NOT NULL,
  identity_pubkey BLOB NOT NULL
);
";

const MIGRATION_V4: &str = "
ALTER TABLE invites ADD COLUMN rebind_user_id BLOB;
CREATE UNIQUE INDEX one_active_device_per_user ON devices(user_id)
  WHERE revoked=0 AND user_id IS NOT NULL;
";

/// Открыть БД с режимом msgd: WAL + FULL. Возвращает соединение (миграции — migrate()).
pub fn connect<P: AsRef<Path>>(path: P) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    Ok(conn)
}

/// Применить миграции к уже открытому соединению (для тестов на :memory: тоже).
pub fn migrate(conn: &Connection) -> rusqlite::Result<i64> {
    migrate_inner(conn)
}

fn migrate_inner(conn: &Connection) -> rusqlite::Result<i64> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(MIGRATION_V1)?;
    tx.execute_batch(MIGRATION_V2)?;
    tx.execute_batch(MIGRATION_V3)?;
    let version: i64 = tx.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |r| r.get::<_, String>(0).map(|v| v.parse().unwrap_or(0)),
    ).unwrap_or(0);
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if version < 4 {
        // Duplicate active devices fail closed; never silently pick a winner.
        tx.execute_batch(MIGRATION_V4)?;
    }
    if version < SCHEMA_VERSION {
        tx.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_fresh_and_reopen() {
        let dir = std::env::temp_dir().join(format!("msgd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        assert_eq!(migrate(&connect(&db).unwrap()).unwrap(), SCHEMA_VERSION);
        // Повторное открытие идемпотентно, версия та же.
        assert_eq!(migrate(&connect(&db).unwrap()).unwrap(), SCHEMA_VERSION);
        let conn = Connection::open(&db).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        assert_eq!(sync, 2); // FULL
        std::fs::remove_dir_all(&dir).ok();
    }

    fn v3() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        conn.execute_batch(MIGRATION_V2).unwrap();
        conn.execute_batch(MIGRATION_V3).unwrap();
        conn.execute("INSERT INTO meta VALUES('schema_version','3')", []).unwrap();
        conn
    }

    #[test]
    fn migrate_v3_preserves_invites_and_enforces_single_device() {
        let conn = v3();
        conn.execute("INSERT INTO invites(token,created_at,expires_at) VALUES(?1,1,2)",
            [[1u8; 32].as_slice()],
        ).unwrap();
        assert_eq!(migrate(&conn).unwrap(), 4);
        assert_eq!(migrate(&conn).unwrap(), 4);
        let target: Option<Vec<u8>> = conn.query_row(
            "SELECT rebind_user_id FROM invites", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(target, None);
        conn.execute("INSERT INTO devices VALUES(?1,?2,1,0)",
            rusqlite::params![[2u8; 32].as_slice(), [3u8; 16].as_slice()],
        ).unwrap();
        assert!(conn.execute("INSERT INTO devices VALUES(?1,?2,1,0)",
            rusqlite::params![[4u8; 32].as_slice(), [3u8; 16].as_slice()],
        ).is_err());
        conn.execute("INSERT INTO devices VALUES(?1,?2,1,1)",
            rusqlite::params![[4u8; 32].as_slice(), [3u8; 16].as_slice()],
        ).unwrap();
        assert!(conn.execute("UPDATE devices SET revoked=0", []).is_err());
    }

    #[test]
    fn migration_duplicate_active_devices_rolls_back_without_repair() {
        let conn = v3();
        for key in [[1u8; 32], [2u8; 32]] {
            conn.execute("INSERT INTO devices VALUES(?1,?2,1,0)",
                rusqlite::params![key.as_slice(), [3u8; 16].as_slice()],
            ).unwrap();
        }
        assert!(migrate(&conn).is_err());
        let version: String = conn.query_row("SELECT value FROM meta", [], |r| r.get(0)).unwrap();
        assert_eq!(version, "3");
        assert!(conn.prepare("SELECT rebind_user_id FROM invites").is_err());
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM devices WHERE revoked=0", [],
            |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
}

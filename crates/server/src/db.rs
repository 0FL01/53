//! SQLite: открытие (WAL + synchronous=FULL), миграции схемы v1→v3.
//! v2: cursors (закладка получателя). v3: device_identities (binding prekeys).

use rusqlite::Connection;
use std::path::Path;

/// Текущая версия схемы.
pub const SCHEMA_VERSION: i64 = 3;

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
    conn.execute_batch(MIGRATION_V1)?;
    conn.execute_batch(MIGRATION_V2)?;
    conn.execute_batch(MIGRATION_V3)?;
    let version: i64 = conn.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |r| r.get::<_, String>(0).map(|v| v.parse().unwrap_or(0)),
    ).unwrap_or(0);
    if version < SCHEMA_VERSION {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
    }
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
}

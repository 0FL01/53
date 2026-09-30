//! Fresh schema 5 only. Compatibility is checked before writable open/WAL.
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;

pub const SCHEMA_VERSION: i64 = 5;
const SCHEMA: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta VALUES('schema_version','5'),('registration_mode','invite_only');
CREATE TABLE users(
  user_id BLOB PRIMARY KEY NOT NULL CHECK(length(user_id)=16),
  contact_id TEXT UNIQUE NOT NULL CHECK(length(contact_id)=12),
  login TEXT UNIQUE NOT NULL,
  password_hash TEXT NOT NULL CHECK(password_hash LIKE '$argon2id$%'),
  created_at INTEGER NOT NULL
);
CREATE TABLE devices(
  device_key BLOB PRIMARY KEY NOT NULL CHECK(length(device_key)=32),
  user_id BLOB NOT NULL REFERENCES users(user_id),
  created_at INTEGER NOT NULL,
  revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)),
  blocked INTEGER NOT NULL DEFAULT 0 CHECK(blocked IN (0,1))
);
CREATE UNIQUE INDEX one_active_device_per_user ON devices(user_id) WHERE revoked=0;
CREATE TABLE invites(
  token BLOB PRIMARY KEY NOT NULL CHECK(length(token)=32),
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)),
  used_at INTEGER
);
CREATE TABLE contact_permissions(
  user_id BLOB NOT NULL, peer_user_id BLOB NOT NULL, state TEXT NOT NULL,
  PRIMARY KEY(user_id,peer_user_id)
);
CREATE TABLE prekeys(
  device_key BLOB NOT NULL, key_id INTEGER NOT NULL, pubkey BLOB NOT NULL,
  signature BLOB NOT NULL, one_time INTEGER NOT NULL,
  consumed INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(device_key,key_id)
);
CREATE TABLE mailbox_events(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, recipient_user_id BLOB NOT NULL,
  sender_device BLOB NOT NULL, message_id BLOB NOT NULL, ciphertext BLOB NOT NULL,
  created_at INTEGER NOT NULL, delivered INTEGER NOT NULL DEFAULT 0,
  UNIQUE(sender_device,message_id)
);
CREATE TABLE blob_meta(
  blob_id BLOB PRIMARY KEY, owner_user_id BLOB NOT NULL, size INTEGER NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, state TEXT NOT NULL
);
CREATE TABLE cursors(
  recipient_user_id BLOB NOT NULL, device_key BLOB NOT NULL,
  last_seq INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(recipient_user_id,device_key)
);
CREATE TABLE device_identities(
  device_key BLOB PRIMARY KEY, user_id BLOB NOT NULL,
  identity_pubkey BLOB NOT NULL CHECK(length(identity_pubkey)=32),
  curve_pubkey BLOB NOT NULL CHECK(length(curve_pubkey)=32)
);
";

fn version(conn: &Connection) -> rusqlite::Result<i64> {
    let objects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if objects == 0 {
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        return if v == 0 {
            Ok(0)
        } else {
            Err(rusqlite::Error::InvalidQuery)
        };
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if value.as_deref() != Some("5") {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mode: String = conn.query_row(
        "SELECT value FROM meta WHERE key='registration_mode'",
        [],
        |r| r.get(0),
    )?;
    if !matches!(mode.as_str(), "open" | "invite_only") {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(SCHEMA_VERSION)
}

pub fn connect<P: AsRef<Path>>(path: P) -> rusqlite::Result<Connection> {
    if path.as_ref().exists() {
        let probe = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        version(&probe)?;
    }
    let conn = Connection::open(path)?;
    version(&conn)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    Ok(conn)
}

/// Historical name used by fixtures; creates fresh schema, never migrates old data.
pub fn migrate(conn: &Connection) -> rusqlite::Result<i64> {
    if version(conn)? == SCHEMA_VERSION {
        return Ok(SCHEMA_VERSION);
    }
    let tx = conn.unchecked_transaction()?;
    if version(&tx)? == 0 {
        tx.execute_batch(SCHEMA)?;
    }
    tx.commit()?;
    Ok(SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_reopen_full_and_unique_active() {
        let dir = std::env::temp_dir().join(format!("msgd-schema-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fresh.db");
        let conn = connect(&path).unwrap();
        assert_eq!(migrate(&conn).unwrap(), 5);
        assert_eq!(migrate(&conn).unwrap(), 5);
        assert_eq!(
            conn.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        conn.execute(
            "INSERT INTO users VALUES(?1,'0123456789AB','alice','$argon2id$test',1)",
            [[1u8; 16].as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
            rusqlite::params![[2u8; 32].as_slice(), [1u8; 16].as_slice()],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
                rusqlite::params![[3u8; 32].as_slice(), [1u8; 16].as_slice()]
            )
            .is_err());
        drop(conn);
        assert_eq!(migrate(&connect(&path).unwrap()).unwrap(), 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn incompatible_versions_rejected_before_wal_without_mutation() {
        let dir = std::env::temp_dir().join(format!("msgd-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for v in [0, 1, 2, 3, 4, 6, 99] {
            let path = dir.join(format!("v{v}.db"));
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('preserve');").unwrap();
            conn.execute(
                "INSERT INTO meta VALUES('schema_version',?1)",
                [v.to_string()],
            )
            .unwrap();
            drop(conn);
            let before = std::fs::read(&path).unwrap();
            assert!(connect(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!path.with_extension("db-wal").exists());
            let conn = Connection::open(&path).unwrap();
            assert!(migrate(&conn).is_err());
            drop(conn);
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn existing_empty_zero_is_fresh_but_future_empty_and_live_legacy_wal_are_read_only() {
        let dir = std::env::temp_dir().join(format!("msgd-preflight-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.db");
        drop(Connection::open(&path).unwrap());
        let conn = connect(&path).unwrap();
        assert_eq!(migrate(&conn).unwrap(), SCHEMA_VERSION);
        drop(conn);

        let path = dir.join("future-empty.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version=6").unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(connect(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("db-wal").exists());

        let path = dir.join("legacy-live.db");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES('schema_version','4'); CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('retain uncheckpointed data');").unwrap();
        let main_before = std::fs::read(&path).unwrap();
        let wal = path.with_extension("db-wal");
        let wal_before = std::fs::read(&wal).unwrap();
        assert!(connect(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), main_before);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
        assert_eq!(
            writer
                .query_row("SELECT value FROM sentinel", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "retain uncheckpointed data"
        );
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

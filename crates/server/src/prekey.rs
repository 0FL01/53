//! Prekeys P4.3: upload с binding-проверкой, атомарный claim, count.
//! Сервер сверяет только формат и Ed25519-подпись identity-ключом
//! по (device_key || key_id BE || pubkey) — E2E-валидность не его дело.
//! Первая загрузка фиксирует identity; смена identity → отказ (тест P4.4).

use dmsg_protocol::{ERR_BAD, ERR_BUSY};
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, PartialEq, Eq)]
pub enum PrekeyError {
    Bad,
    IdentityChanged,
    /// SQLITE_BUSY: повторить позже (wire ERR_BUSY=7). Маппится ДО схлопывания в Store.
    Busy,
    Store(String),
}

/// Wire-код ошибки (см. dmsg_protocol::ERR_*).
impl PrekeyError {
    pub fn code(&self) -> u8 {
        match self {
            PrekeyError::Bad | PrekeyError::IdentityChanged | PrekeyError::Store(_) => ERR_BAD,
            PrekeyError::Busy => ERR_BUSY,
        }
    }
}

/// SQLITE_BUSY → Busy, остальное — Store с контекстом. Вызывать на КАЖДОМ
/// rusqlite-результате до схлопывания, иначе busy утонет в Store→ERR_BAD.
fn store(prefix: &str, e: rusqlite::Error) -> PrekeyError {
    if matches!(&e, rusqlite::Error::SqliteFailure(f, _)
        if f.code == rusqlite::ErrorCode::DatabaseBusy)
    {
        PrekeyError::Busy
    } else {
        PrekeyError::Store(format!("{prefix}: {e}"))
    }
}

pub struct Entry<'a> {
    pub key_id: u32,
    pub one_time: u8,
    pub pubkey: &'a [u8],
    pub sig: &'a [u8],
}

/// Проверить подпись одной записи identity-ключом.
fn verify(device: &[u8], identity: &[u8], e: &Entry) -> bool {
    if identity.len() != 32 || e.pubkey.len() != 32 || e.sig.len() != 64 {
        return false;
    }
    let Ok(pk) = ed25519_dalek::VerifyingKey::from_bytes(identity.try_into().expect("checked"))
    else {
        return false;
    };
    let mut msg = Vec::with_capacity(32 + 4 + 32);
    msg.extend_from_slice(device);
    msg.extend_from_slice(&e.key_id.to_be_bytes());
    msg.extend_from_slice(e.pubkey);
    let Ok(sig) = ed25519_dalek::Signature::from_slice(e.sig) else {
        return false;
    };
    pk.verify_strict(&msg, &sig).is_ok()
}

/// Upload: binding-check всех записей, фиксация/сверка identity, INSERT. Одна TX.
pub fn upload(
    conn: &mut Connection,
    device: &[u8],
    user: &[u8],
    identity: &[u8],
    curve: &[u8],
    entries: &[Entry],
) -> Result<i64, PrekeyError> {
    if identity.len() != 32 || curve.len() != 32 {
        return Err(PrekeyError::Bad);
    }
    if ed25519_dalek::VerifyingKey::from_bytes(identity.try_into().expect("checked")).is_err() {
        return Err(PrekeyError::Bad);
    }
    for e in entries {
        if !verify(device, identity, e) {
            return Err(PrekeyError::Bad);
        }
    }
    let tx = conn.transaction().map_err(|e| store("begin", e))?;
    let active = tx
        .query_row(
            "SELECT 1 FROM devices WHERE device_key=?1 AND user_id=?2 AND revoked=0 AND blocked=0",
            rusqlite::params![device, user],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| store("active", e))?
        .is_some();
    if !active {
        return Err(PrekeyError::Bad);
    }
    let stored: Option<(Vec<u8>, Vec<u8>)> = tx
        .query_row(
            "SELECT identity_pubkey,curve_pubkey FROM device_identities WHERE device_key=?1",
            [device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| store("identity", e))?;
    match stored {
        None => {
            tx.execute(
                "INSERT INTO device_identities(device_key,user_id,identity_pubkey,curve_pubkey) VALUES(?1,?2,?3,?4)",
                rusqlite::params![device, user, identity,curve],
            )
            .map_err(|e| store("identity", e))?;
        }
        Some((ed, prev_curve)) if ed.as_slice() != identity || prev_curve.as_slice() != curve => {
            return Err(PrekeyError::IdentityChanged)
        }
        Some(_) => {}
    }
    for e in entries {
        tx.execute(
            "INSERT INTO prekeys(device_key,key_id,pubkey,signature,one_time,consumed)
             VALUES(?1,?2,?3,?4,?5,0)
             ON CONFLICT(device_key,key_id) DO UPDATE SET pubkey=excluded.pubkey,signature=excluded.signature",
            rusqlite::params![device, e.key_id, e.pubkey, e.sig, e.one_time],
        )
        .map_err(|e| store("prekey", e))?;
    }
    let n: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM prekeys WHERE device_key=?1 AND one_time=1 AND consumed=0",
            [device],
            |r| r.get(0),
        )
        .map_err(|e| store("count", e))?;
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(n)
}

/// Claim: атомарно забрать один unconsumed one-time key (SELECT+UPDATE в TX).
pub fn claim(conn: &mut Connection, device: &[u8]) -> Result<Option<(u32, Vec<u8>)>, PrekeyError> {
    let tx = conn.transaction().map_err(|e| store("begin", e))?;
    let row: Option<(u32, Vec<u8>)> = tx
        .query_row(
             "SELECT p.key_id,p.pubkey FROM prekeys p JOIN devices d ON d.device_key=p.device_key WHERE p.device_key=?1 AND d.revoked=0 AND d.blocked=0 AND p.one_time=1 AND p.consumed=0 ORDER BY p.key_id LIMIT 1",
            [device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| store("claim", e))?;
    if let Some((id, _)) = row {
        tx.execute(
            "UPDATE prekeys SET consumed=1 WHERE device_key=?1 AND key_id=?2",
            rusqlite::params![device, id],
        )
        .map_err(|e| store("consume", e))?;
    }
    tx.commit().map_err(|e| store("commit", e))?;
    Ok(row)
}

/// Число unconsumed one-time ключей (refill-сигнал клиенту).
pub fn count(conn: &Connection, device: &[u8]) -> Result<i64, PrekeyError> {
    conn.query_row(
        "SELECT COUNT(*) FROM prekeys p JOIN devices d ON d.device_key=p.device_key WHERE p.device_key=?1 AND d.revoked=0 AND d.blocked=0 AND p.one_time=1 AND p.consumed=0",
        [device],
        |r| r.get(0),
    )
    .map_err(|e| store("count", e))
}

pub fn binding(
    conn: &Connection,
    user: &[u8],
) -> Result<Option<([u8; 32], [u8; 32], [u8; 32])>, PrekeyError> {
    conn.query_row("SELECT d.device_key,i.identity_pubkey,i.curve_pubkey FROM devices d JOIN device_identities i ON i.device_key=d.device_key AND i.user_id=d.user_id WHERE d.user_id=?1 AND d.revoked=0 AND d.blocked=0",[user],|r| {
        let d: Vec<u8> = r.get(0)?; let ed: Vec<u8> = r.get(1)?; let c: Vec<u8> = r.get(2)?;
        Ok((d.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,ed.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,c.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?))
    }).optional().map_err(|e| store("binding",e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use ed25519_dalek::Signer;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    fn signed(
        device: &[u8],
        idkey: &ed25519_dalek::SigningKey,
        id: u32,
        pubk: &[u8; 32],
    ) -> (u32, u8, Vec<u8>, Vec<u8>) {
        let mut msg = Vec::new();
        msg.extend_from_slice(device);
        msg.extend_from_slice(&id.to_be_bytes());
        msg.extend_from_slice(pubk);
        let sig = idkey.sign(&msg);
        (id, 1, pubk.to_vec(), sig.to_bytes().to_vec())
    }

    #[test]
    fn upload_claim_empty_and_identity_change() {
        let mut conn = mem();
        let dev = [11u8; 32];
        let user = [12u8; 16];
        conn.execute(
            "INSERT INTO users VALUES(?1,'0123456789AB','alice','$argon2id$test',1)",
            [user.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices(device_key,user_id,created_at) VALUES(?1,?2,1)",
            rusqlite::params![dev.as_slice(), user.as_slice()],
        )
        .unwrap();
        let curve = [14u8; 32];
        let idkey = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
        let idpub = idkey.verifying_key().to_bytes();
        // Пустой запас.
        assert!(claim(&mut conn, &dev).unwrap().is_none());
        assert_eq!(count(&conn, &dev).unwrap(), 0);
        // Загрузка двух ключей.
        let e1 = signed(&dev, &idkey, 1, &[21u8; 32]);
        let e2 = signed(&dev, &idkey, 2, &[22u8; 32]);
        let entries = [
            Entry {
                key_id: e1.0,
                one_time: e1.1,
                pubkey: &e1.2,
                sig: &e1.3,
            },
            Entry {
                key_id: e2.0,
                one_time: e2.1,
                pubkey: &e2.2,
                sig: &e2.3,
            },
        ];
        assert_eq!(
            upload(&mut conn, &dev, &user, &idpub, &curve, &entries).unwrap(),
            2
        );
        assert_eq!(binding(&conn, &user).unwrap(), Some((dev, idpub, curve)));
        assert_eq!(
            upload(&mut conn, &dev, &user, &idpub, &[99; 32], &entries),
            Err(PrekeyError::IdentityChanged)
        );
        assert_eq!(binding(&conn, &user).unwrap(), Some((dev, idpub, curve)));
        // Claim забирает по одному, второй раз — второй, третий — пусто.
        let (id, _) = claim(&mut conn, &dev).unwrap().expect("first");
        assert_eq!(id, 1);
        let (id, _) = claim(&mut conn, &dev).unwrap().expect("second");
        assert_eq!(id, 2);
        assert!(claim(&mut conn, &dev).unwrap().is_none());
        // Битый sig отклоняется.
        let mut bad = signed(&dev, &idkey, 3, &[23u8; 32]);
        bad.3[0] ^= 0xFF;
        let be = [Entry {
            key_id: bad.0,
            one_time: bad.1,
            pubkey: &bad.2,
            sig: &bad.3,
        }];
        assert_eq!(
            upload(&mut conn, &dev, &user, &idpub, &curve, &be).unwrap_err(),
            PrekeyError::Bad
        );
        // Смена identity отклоняется.
        let other = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let opub = other.verifying_key().to_bytes();
        let e3 = signed(&dev, &other, 4, &[24u8; 32]);
        let oe = [Entry {
            key_id: e3.0,
            one_time: e3.1,
            pubkey: &e3.2,
            sig: &e3.3,
        }];
        assert_eq!(
            upload(&mut conn, &dev, &user, &opub, &curve, &oe).unwrap_err(),
            PrekeyError::IdentityChanged
        );
    }

    #[test]
    fn sqlite_busy_maps_to_busy_code() {
        let e = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None);
        assert_eq!(store("t", e), PrekeyError::Busy);
        assert_eq!(PrekeyError::Busy.code(), dmsg_protocol::ERR_BUSY);
        assert_eq!(dmsg_protocol::ERR_BUSY, 7);
    }
}

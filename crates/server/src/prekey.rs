//! Prekeys P4.3: upload с binding-проверкой, атомарный claim, count.
//! Сервер сверяет только формат и Ed25519-подпись identity-ключом
//! по (device_key || key_id BE || pubkey) — E2E-валидность не его дело.
//! Первая загрузка фиксирует identity; смена identity → отказ (тест P4.4).

use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, PartialEq, Eq)]
pub enum PrekeyError {
    Bad,
    IdentityChanged,
    Store(String),
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
    let Ok(pk) = ed25519_dalek::VerifyingKey::from_bytes(
        identity.try_into().expect("checked"),
    ) else {
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
    entries: &[Entry],
) -> Result<i64, PrekeyError> {
    if identity.len() != 32 || entries.is_empty() {
        return Err(PrekeyError::Bad);
    }
    for e in entries {
        if !verify(device, identity, e) {
            return Err(PrekeyError::Bad);
        }
    }
    let tx = conn.transaction().map_err(|e| PrekeyError::Store(e.to_string()))?;
    let stored: Option<Vec<u8>> = tx
        .query_row(
            "SELECT identity_pubkey FROM device_identities WHERE device_key=?1",
            [device],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| PrekeyError::Store(e.to_string()))?;
    match stored {
        None => {
            tx.execute(
                "INSERT INTO device_identities(device_key,user_id,identity_pubkey) VALUES(?1,?2,?3)",
                rusqlite::params![device, user, identity],
            )
            .map_err(|e| PrekeyError::Store(e.to_string()))?;
        }
        Some(prev) if prev.as_slice() != identity => return Err(PrekeyError::IdentityChanged),
        Some(_) => {}
    }
    for e in entries {
        tx.execute(
            "INSERT INTO prekeys(device_key,key_id,pubkey,signature,one_time,consumed)
             VALUES(?1,?2,?3,?4,?5,0)
             ON CONFLICT(device_key,key_id) DO UPDATE SET pubkey=excluded.pubkey,signature=excluded.signature",
            rusqlite::params![device, e.key_id, e.pubkey, e.sig, e.one_time],
        )
        .map_err(|e| PrekeyError::Store(e.to_string()))?;
    }
    let n: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM prekeys WHERE device_key=?1 AND one_time=1 AND consumed=0",
            [device],
            |r| r.get(0),
        )
        .map_err(|e| PrekeyError::Store(e.to_string()))?;
    tx.commit().map_err(|e| PrekeyError::Store(e.to_string()))?;
    Ok(n)
}

/// Claim: атомарно забрать один unconsumed one-time key (SELECT+UPDATE в TX).
pub fn claim(conn: &mut Connection, device: &[u8]) -> Result<Option<(u32, Vec<u8>)>, PrekeyError> {
    let tx = conn.transaction().map_err(|e| PrekeyError::Store(e.to_string()))?;
    let row: Option<(u32, Vec<u8>)> = tx
        .query_row(
            "SELECT key_id,pubkey FROM prekeys WHERE device_key=?1 AND one_time=1 AND consumed=0 ORDER BY key_id LIMIT 1",
            [device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| PrekeyError::Store(e.to_string()))?;
    if let Some((id, _)) = row {
        tx.execute(
            "UPDATE prekeys SET consumed=1 WHERE device_key=?1 AND key_id=?2",
            rusqlite::params![device, id],
        )
        .map_err(|e| PrekeyError::Store(e.to_string()))?;
    }
    tx.commit().map_err(|e| PrekeyError::Store(e.to_string()))?;
    Ok(row)
}

/// Число unconsumed one-time ключей (refill-сигнал клиенту).
pub fn count(conn: &Connection, device: &[u8]) -> Result<i64, PrekeyError> {
    conn.query_row(
        "SELECT COUNT(*) FROM prekeys WHERE device_key=?1 AND one_time=1 AND consumed=0",
        [device],
        |r| r.get(0),
    )
    .map_err(|e| PrekeyError::Store(e.to_string()))
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

    fn signed(device: &[u8], idkey: &ed25519_dalek::SigningKey, id: u32, pubk: &[u8; 32]) -> (u32, u8, Vec<u8>, Vec<u8>) {
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
        let idkey = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
        let idpub = idkey.verifying_key().to_bytes();
        // Пустой запас.
        assert!(claim(&mut conn, &dev).unwrap().is_none());
        assert_eq!(count(&conn, &dev).unwrap(), 0);
        // Загрузка двух ключей.
        let e1 = signed(&dev, &idkey, 1, &[21u8; 32]);
        let e2 = signed(&dev, &idkey, 2, &[22u8; 32]);
        let entries = [Entry { key_id: e1.0, one_time: e1.1, pubkey: &e1.2, sig: &e1.3 },
                       Entry { key_id: e2.0, one_time: e2.1, pubkey: &e2.2, sig: &e2.3 }];
        assert_eq!(upload(&mut conn, &dev, &user, &idpub, &entries).unwrap(), 2);
        // Claim забирает по одному, второй раз — второй, третий — пусто.
        let (id, _) = claim(&mut conn, &dev).unwrap().expect("first");
        assert_eq!(id, 1);
        let (id, _) = claim(&mut conn, &dev).unwrap().expect("second");
        assert_eq!(id, 2);
        assert!(claim(&mut conn, &dev).unwrap().is_none());
        // Битый sig отклоняется.
        let mut bad = signed(&dev, &idkey, 3, &[23u8; 32]);
        bad.3[0] ^= 0xFF;
        let be = [Entry { key_id: bad.0, one_time: bad.1, pubkey: &bad.2, sig: &bad.3 }];
        assert_eq!(upload(&mut conn, &dev, &user, &idpub, &be).unwrap_err(), PrekeyError::Bad);
        // Смена identity отклоняется.
        let other = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let opub = other.verifying_key().to_bytes();
        let e3 = signed(&dev, &other, 4, &[24u8; 32]);
        let oe = [Entry { key_id: e3.0, one_time: e3.1, pubkey: &e3.2, sig: &e3.3 }];
        assert_eq!(
            upload(&mut conn, &dev, &user, &opub, &oe).unwrap_err(),
            PrekeyError::IdentityChanged
        );
    }
}

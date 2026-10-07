//! Durable requests in existing schema5. No private keys or plaintext messages.
use dmsg_protocol::{
    auth::DeviceBinding,
    contacts::{PeerProfile, REQUESTS_MAX},
    ERR_BAD, ERR_BUSY, ERR_QUOTA,
};
use rusqlite::{Connection, OptionalExtension};

fn error(e: rusqlite::Error) -> u8 {
    if matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::DatabaseBusy)
    {
        ERR_BUSY
    } else {
        ERR_BAD
    }
}

pub fn request(db: &Connection, sender: &[u8], recipient: &[u8]) -> Result<(), u8> {
    if sender == recipient || recipient.len() != 16 {
        return Err(ERR_BAD);
    }
    let known: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE user_id=?1)",
            [recipient],
            |r| r.get(0),
        )
        .map_err(error)?;
    if !known {
        return Err(ERR_BAD);
    }
    let existing: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM contact_permissions WHERE user_id=?1 AND peer_user_id=?2)",
            rusqlite::params![recipient, sender],
            |r| r.get(0),
        )
        .map_err(error)?;
    if existing {
        return Ok(());
    }
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contact_permissions WHERE user_id=?1 AND state='requested'",
            [recipient],
            |r| r.get(0),
        )
        .map_err(error)?;
    if count >= REQUESTS_MAX as i64 {
        return Err(ERR_QUOTA);
    }
    db.execute("INSERT INTO contact_permissions(user_id,peer_user_id,state) VALUES(?1,?2,'requested') ON CONFLICT DO NOTHING",rusqlite::params![recipient,sender]).map_err(error)?;
    Ok(())
}

pub fn pending(db: &Connection, user: &[u8]) -> Result<Vec<PeerProfile>, u8> {
    let mut stmt=db.prepare("SELECT u.contact_id,u.user_id,d.device_key,i.identity_pubkey,i.curve_pubkey FROM contact_permissions p JOIN users u ON u.user_id=p.peer_user_id JOIN devices d ON d.user_id=u.user_id AND d.revoked=0 AND d.blocked=0 JOIN device_identities i ON i.device_key=d.device_key AND i.user_id=u.user_id WHERE p.user_id=?1 AND p.state='requested' ORDER BY u.user_id LIMIT ?2").map_err(error)?;
    let rows = stmt
        .query_map(rusqlite::params![user, REQUESTS_MAX as i64], |r| {
            Ok(PeerProfile {
                contact_id: r.get(0)?,
                binding: DeviceBinding {
                    user_id: r.get(1)?,
                    device_key: r.get(2)?,
                    ed25519: r.get(3)?,
                    curve25519: r.get(4)?,
                },
            })
        })
        .map_err(error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(error)
}

pub fn decide(db: &Connection, user: &[u8], peer: &[u8], decision: u8) -> Result<(), u8> {
    let state = match decision {
        1 => "accepted",
        2 => "blocked",
        _ => return Err(ERR_BAD),
    };
    let old: Option<String> = db
        .query_row(
            "SELECT state FROM contact_permissions WHERE user_id=?1 AND peer_user_id=?2",
            rusqlite::params![user, peer],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    match old.as_deref() {
        Some("blocked") => return if decision == 2 { Ok(()) } else { Err(ERR_BAD) },
        Some("requested" | "accepted") => (),
        _ => return Err(ERR_BAD),
    }
    db.execute(
        "UPDATE contact_permissions SET state=?3 WHERE user_id=?1 AND peer_user_id=?2",
        rusqlite::params![user, peer, state],
    )
    .map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_are_scoped_idempotent_bounded_and_block_terminal() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO users VALUES(?1,'0123456789AB','bob','$argon2id$test',1)",
            [&[2u8; 16][..]],
        )
        .unwrap();
        request(&db, &[1; 16], &[2; 16]).unwrap();
        request(&db, &[1; 16], &[2; 16]).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM contact_permissions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(decide(&db, &[3; 16], &[1; 16], 1), Err(ERR_BAD));
        decide(&db, &[2; 16], &[1; 16], 1).unwrap();
        request(&db, &[1; 16], &[2; 16]).unwrap();
        assert_eq!(
            db.query_row("SELECT state FROM contact_permissions", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "accepted"
        );
        decide(&db, &[2; 16], &[1; 16], 2).unwrap();
        assert_eq!(decide(&db, &[2; 16], &[1; 16], 1), Err(ERR_BAD));
        for n in 3..35 {
            request(&db, &[n; 16], &[2; 16]).unwrap();
        }
        assert_eq!(request(&db, &[35; 16], &[2; 16]), Err(ERR_QUOTA));
        assert_eq!(request(&db, &[2; 16], &[2; 16]), Err(ERR_BAD));
    }
}

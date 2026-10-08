//! Live schema7 / Noise evidence for the single-secret invitation contract.
mod common;
use common::*;
use dmsg_protocol::{auth, invitation as ip, *};

async fn issue(c: &mut Client, id: [u8; 16]) -> ip::Issued {
    let (op, payload) = c.exchange(OP_INVITE_ISSUE, &id).await;
    assert_eq!(op, OP_INVITE_ISSUED);
    ip::parse_issued(&payload).unwrap()
}

#[tokio::test]
async fn one_bearer_atomic_race_owner_retry_replacement_restart_backup_retention() {
    let mut s = Server::new("invitation");
    let mut unauthorized = Client::connect(&s, [10; 32]).await;
    unauthorized.send(OP_INVITE_LIST, &[]).await;
    assert!(unauthorized.receive().await.is_none());
    let (admin, _) = s.issue();
    let mut owner = Client::connect(&s, [11; 32]).await;
    let (op, p) = owner.signup("inviteowner", Some(&admin)).await;
    assert_eq!(op, OP_AUTHENTICATED);
    let owner_profile = auth::parse_authenticated(&p).unwrap();
    assert_eq!(s.ctl(&["dbversion"]), "7\n");
    let admin_shape:i64=s.db().query_row("SELECT COUNT(*) FROM invites WHERE token=?1 AND owner_user_id IS NULL AND issue_id IS NULL AND phrase_ascii IS NULL",[admin.as_slice()],|r|r.get(0)).unwrap();
    assert_eq!(admin_shape, 1);
    let original = issue(&mut owner, [1; 16]).await;
    assert_eq!(
        original.invitation.expires_at - original.invitation.created_at,
        ip::TTL_SECONDS
    );
    let phrase = original.phrase.as_deref().unwrap();
    assert_eq!(phrase.split(' ').count(), 6);
    let token = ip::parse_invitation_input(phrase).unwrap();
    let qr = auth::build_invitation(&token);
    assert_eq!(qr.len(), 43);
    assert!(ip::parse_invitation_input(&qr).unwrap() == token);
    let retry = issue(&mut owner, [1; 16]).await;
    assert!(retry.phrase == original.phrase);
    assert_eq!(retry.invitation, original.invitation);
    let mut b = Client::connect(&s, [12; 32]).await;
    let mut c = Client::connect(&s, [13; 32]).await;
    let (rb, rc) = tokio::join!(
        b.signup("invitewinnerb", Some(&token)),
        c.signup("invitewinnerc", Some(&token))
    );
    let mut successful = 0;
    for result in [&rb, &rc] {
        if result.0 == OP_AUTHENTICATED {
            successful += 1;
        } else {
            assert_eq!(result, &(OP_ERROR, vec![ERR_INVITE_USED]));
        }
    }
    assert_eq!(successful, 1);
    // Existing signup retry order is unchanged: same active device/account
    // succeeds even though the invitation is now used.
    if rb.0 == OP_AUTHENTICATED {
        assert_eq!(
            b.signup("invitewinnerb", Some(&token)).await.0,
            OP_AUTHENTICATED
        );
    } else {
        assert_eq!(
            c.signup("invitewinnerc", Some(&token)).await.0,
            OP_AUTHENTICATED
        );
    }
    let used = issue(&mut owner, [1; 16]).await;
    assert_eq!(used.state, ip::State::Used);
    assert!(used.phrase.is_none());
    let mut foreign = if rb.0 == OP_AUTHENTICATED { b } else { c };
    assert_eq!(
        foreign.exchange(OP_INVITE_REVOKE, &[1; 16]).await,
        (OP_ERROR, vec![ERR_BAD])
    );
    assert_eq!(
        owner.exchange(OP_INVITE_REVOKE, &[9; 16]).await,
        (OP_ERROR, vec![ERR_BAD])
    );
    assert_eq!(
        owner.exchange(OP_INVITE_REVOKE, &[1; 16]).await,
        (OP_INVITE_REVOKED, vec![])
    );
    let active = issue(&mut owner, [2; 16]).await;
    issue(&mut owner, [3; 16]).await;
    assert_eq!(
        owner.exchange(OP_INVITE_REVOKE, &[3; 16]).await,
        (OP_INVITE_REVOKED, vec![])
    );
    assert_eq!(
        owner.exchange(OP_INVITE_REVOKE, &[3; 16]).await,
        (OP_INVITE_REVOKED, vec![])
    );
    let revoked = issue(&mut owner, [3; 16]).await;
    assert_eq!(revoked.state, ip::State::Revoked);
    assert!(revoked.phrase.is_none());
    assert_eq!(
        owner.exchange(OP_INVITE_LIST, &[0]).await,
        (OP_ERROR, vec![ERR_INVALID_INPUT])
    );
    assert_eq!(
        owner.exchange(OP_INVITE_ISSUE, &[0; 17]).await,
        (OP_ERROR, vec![ERR_INVALID_INPUT])
    );
    let mut replacement = Client::connect(&s, [14; 32]).await;
    assert_eq!(
        replacement
            .login("inviteowner", Some(&owner.device))
            .await
            .0,
        OP_AUTHENTICATED
    );
    owner.closed().await;
    assert!(issue(&mut replacement, [2; 16]).await.phrase == active.phrase);
    let (op, p) = replacement.exchange(OP_INVITE_LIST, &[]).await;
    assert_eq!(op, OP_INVITE_LISTED);
    assert_eq!(
        ip::parse_list(&p).unwrap().invitations,
        vec![active.invitation]
    );
    // GC cannot discard used/revoked self-service rows or their replay secret.
    s.ctl(&["gc"]);
    let backup = s.ctl(&["backup"]);
    let snapshot = backup
        .split_whitespace()
        .find_map(|part| part.strip_prefix("path="))
        .unwrap();
    let backup_db =
        rusqlite::Connection::open(std::path::Path::new(snapshot).join("msgd.db")).unwrap();
    let retained: i64 = backup_db
        .query_row(
            "SELECT COUNT(*) FROM invites WHERE owner_user_id=?1 AND phrase_ascii IS NOT NULL",
            [owner_profile.user_id.as_slice()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, 3);
    let saved: String = backup_db
        .query_row(
            "SELECT phrase_ascii FROM invites WHERE owner_user_id=?1 AND issue_id=?2",
            rusqlite::params![owner_profile.user_id.as_slice(), [2u8; 16].as_slice()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(saved == active.phrase.as_deref().unwrap());
    drop(backup_db);
    drop(replacement);
    drop(foreign);
    s.restart();
    let mut resumed = Client::connect(&s, [14; 32]).await;
    assert_eq!(resumed.exchange(OP_RESUME, &[]).await.0, OP_AUTHENTICATED);
    let retry = issue(&mut resumed, [2; 16]).await;
    assert_eq!(retry.invitation, active.invitation);
    assert!(retry.phrase == active.phrase);
    assert_eq!(issue(&mut resumed, [1; 16]).await.state, ip::State::Used);
    assert_eq!(issue(&mut resumed, [3; 16]).await.state, ip::State::Revoked);
    // Both raw QR and phrase address exactly the persisted token; no alias row.
    let rows: i64 = s
        .db()
        .query_row(
            "SELECT COUNT(*) FROM invites WHERE owner_user_id=?1",
            [owner_profile.user_id.as_slice()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 3);
}

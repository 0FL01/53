//! Credential auth wire/durability/race evidence against a live loopback msgd.
#[allow(dead_code)]
#[path = "../examples/auth_diag.rs"]
mod auth_diag;
mod common;
use common::*;
use dmsg_protocol::{auth, mailbox, *};

fn error(code: u8) -> (u8, Vec<u8>) {
    (OP_ERROR, vec![code])
}
fn counts(s: &Server) -> (i64, i64, i64) {
    s.db().query_row("SELECT (SELECT COUNT(*) FROM users),(SELECT COUNT(*) FROM devices),(SELECT COUNT(*) FROM devices WHERE revoked=0)",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
}

#[tokio::test]
async fn policy_signup_lost_reply_replay_and_restart_resume() {
    let mut srv = Server::new("replay");
    let mut short = Client::connect(&srv, [1; 32]).await;
    assert_eq!(
        short.exchange(OP_POLICY, &[]).await,
        (OP_POLICY_RESP, vec![0])
    );
    drop(short); // Frontend releases this short connection before human input.
    let (token, path) = srv.issue();
    let mut c = Client::connect(&srv, [2; 32]).await;
    let payload = auth::build_signup("ALIce", PASSWORD, Some(&token)).unwrap();
    c.send(OP_SIGNUP, &payload).await;
    // Commit observed independently, without reading AUTHENTICATED.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while counts(&srv).0 == 0 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    drop(c);
    srv.db()
        .execute("UPDATE invites SET expires_at=0", [])
        .unwrap();
    assert_eq!(
        srv.ctl(&["invite-revoke", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    assert_eq!(srv.ctl(&["registration-mode", "open"]), "open\n");
    let mut c = Client::connect(&srv, [2; 32]).await;
    let original = c.exchange(OP_SIGNUP, &payload).await;
    assert_eq!(original.0, OP_AUTHENTICATED);
    auth::parse_authenticated(&original.1).unwrap();
    assert_eq!(
        c.exchange(
            OP_SIGNUP,
            &auth::build_signup("alice", "wrong-fixture-password", Some(&token)).unwrap()
        )
        .await,
        error(ERR_CREDENTIALS)
    );
    assert_eq!(counts(&srv), (1, 1, 1));
    let hash: String = srv
        .db()
        .query_row("SELECT password_hash FROM users", [], |r| r.get(0))
        .unwrap();
    assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(!hash.contains(PASSWORD));
    drop(c);
    srv.restart();
    let mut c = Client::connect(&srv, [2; 32]).await;
    assert_eq!(c.exchange(OP_RESUME, &[]).await, original);
    assert_eq!(c.login("alice", None).await, original);
    assert_eq!(srv.ctl(&["registration-mode"]), "open\n");
    let mut unknown = Client::connect(&srv, [3; 32]).await;
    assert_eq!(unknown.exchange(OP_RESUME, &[]).await, error(ERR_REVOKED));
    assert_eq!(
        unknown.exchange(OP_RESUME, &[0]).await,
        error(ERR_INVALID_INPUT)
    );
    let users = srv.ctl(&["user-list"]);
    assert!(!users.contains(&hash));
    assert!(!users.contains(PASSWORD));
}

#[tokio::test]
async fn signup_policy_invite_expiry_revoke_and_one_time_authorization() {
    let srv = Server::new("invites");
    let mut c = Client::connect(&srv, [4; 32]).await;
    assert_eq!(c.signup("alice", None).await, error(ERR_INVITE_REQUIRED));
    let (expired, _) = srv.issue();
    srv.db()
        .execute(
            "UPDATE invites SET expires_at=0 WHERE token=?1",
            [expired.as_slice()],
        )
        .unwrap();
    assert_eq!(c.signup("alice", Some(&expired)).await, error(ERR_EXPIRED));
    let (revoked, path) = srv.issue();
    assert_eq!(
        srv.ctl(&["invite-revoke", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    assert_eq!(c.signup("alice", Some(&revoked)).await, error(ERR_REVOKED));
    assert_eq!(c.signup("alice", Some(&[255; 32])).await, error(ERR_BAD));
    assert_eq!(counts(&srv), (0, 0, 0));
    let (token, path) = srv.issue();
    let original = c.signup("alice", Some(&token)).await;
    assert_eq!(original.0, OP_AUTHENTICATED);
    assert_eq!(
        srv.ctl(&["invite-revoke", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    // An invite never becomes a device credential or revocation authority.
    assert_eq!(c.exchange(OP_RESUME, &[]).await, original);
    let mut other = Client::connect(&srv, [5; 32]).await;
    let (fresh, _) = srv.issue();
    assert_eq!(other.signup("bob", Some(&token)).await, error(ERR_REVOKED));
    assert_eq!(srv.ctl(&["registration-mode", "open"]), "open\n");
    let bob = other.signup("bob", None).await;
    assert_eq!(bob.0, OP_AUTHENTICATED);
    assert_eq!(
        other.signup("alice", Some(&fresh)).await,
        error(ERR_BOUND_OTHER)
    );
    assert_eq!(counts(&srv), (2, 2, 2));
}

#[tokio::test]
async fn occupied_signup_new_device_is_conflict_but_same_key_retry_verifies_password() {
    let srv = Server::new("occupied-signup");
    let (used, _) = srv.issue();
    let mut owner = Client::connect(&srv, [31; 32]).await;
    let original = owner.signup("alice", Some(&used)).await;
    assert_eq!(original.0, OP_AUTHENTICATED);
    let original_hash: String = srv
        .db()
        .query_row("SELECT password_hash FROM users", [], |r| r.get(0))
        .unwrap();
    let (unused, _) = srv.issue();
    let mut other = Client::connect(&srv, [32; 32]).await;
    let wrong = auth::build_signup("alice", "wrong-fixture-password", Some(&unused)).unwrap();
    assert_eq!(other.exchange(OP_SIGNUP, &wrong).await, error(ERR_CONFLICT));
    assert_eq!(
        other.signup("alice", Some(&unused)).await,
        error(ERR_CONFLICT)
    );
    assert_eq!(
        owner.exchange(OP_SIGNUP, &wrong).await,
        error(ERR_CREDENTIALS)
    );
    assert_eq!(owner.signup("alice", Some(&used)).await, original);
    assert_eq!(counts(&srv), (1, 1, 1));
    let db = srv.db();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM cursors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT password_hash FROM users", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        original_hash
    );
    assert_eq!(
        db.query_row(
            "SELECT used_at FROM invites WHERE token=?1",
            [unused.as_slice()],
            |r| r.get::<_, Option<i64>>(0)
        )
        .unwrap(),
        None
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM invites WHERE used_at IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn wrong_and_missing_credentials_are_uniform_and_throttled_before_hash() {
    let srv = Server::new("credentials");
    srv.ctl(&["registration-mode", "open"]);
    let mut a = Client::connect(&srv, [6; 32]).await;
    assert_eq!(a.signup("alice", None).await.0, OP_AUTHENTICATED);
    let mut c = Client::connect(&srv, [7; 32]).await;
    for name in ["alice", "missing"] {
        assert_eq!(
            c.exchange(
                OP_LOGIN,
                &auth::build_login(name, "wrong-fixture-password", None).unwrap()
            )
            .await,
            error(ERR_CREDENTIALS)
        );
    }
    for _ in 0..6 {
        assert_eq!(c.login("missing", None).await, error(ERR_CREDENTIALS));
    }
    assert_eq!(c.login("missing", None).await, error(ERR_THROTTLED));
    let mut raw = auth::build_login("alice", PASSWORD, None).unwrap();
    raw[1] = b'A';
    assert_eq!(a.exchange(OP_LOGIN, &raw).await, error(ERR_INVALID_INPUT));
    assert_eq!(counts(&srv), (1, 1, 1));
}

#[tokio::test]
async fn signup_invite_race_and_same_key_retry_are_atomic() {
    let srv = Server::new("signup-races");
    let (token, _) = srv.issue();
    let mut a = Client::connect(&srv, [8; 32]).await;
    let mut b = Client::connect(&srv, [9; 32]).await;
    let (ra, rb) = tokio::join!(
        a.signup("alice", Some(&token)),
        b.signup("bob", Some(&token))
    );
    assert!(matches!(
        (ra.0, rb.0),
        (OP_AUTHENTICATED, OP_ERROR) | (OP_ERROR, OP_AUTHENTICATED)
    ));
    assert_eq!(
        if ra.0 == OP_ERROR {
            ra.clone()
        } else {
            rb.clone()
        },
        error(ERR_INVITE_USED)
    );
    assert_eq!(counts(&srv), (1, 1, 1));
    drop((a, b));
    let (token, _) = srv.issue();
    let mut a = Client::connect(&srv, [10; 32]).await;
    let mut b = Client::connect(&srv, [10; 32]).await;
    let (ra, rb) = tokio::join!(
        a.signup("carol", Some(&token)),
        b.signup("carol", Some(&token))
    );
    assert_eq!(ra.0, OP_AUTHENTICATED);
    assert_eq!(ra, rb);
    assert_eq!(counts(&srv), (2, 2, 2));
}

#[tokio::test]
async fn login_challenge_confirm_cas_closes_live_and_pending_and_skips_history() {
    let mut srv = Server::new("replacement");
    srv.ctl(&["registration-mode", "open"]);
    let mut old = Client::connect(&srv, [11; 32]).await;
    let original = old.signup("alice", None).await;
    assert_eq!(original.0, OP_AUTHENTICATED);
    let alice = auth::parse_authenticated(&original.1).unwrap().user_id;
    let mut sender = Client::connect(&srv, [12; 32]).await;
    sender.signup("bob", None).await;
    let mut send = [alice.as_slice(), &[1; 16], b"history"].concat();
    assert_eq!(sender.exchange(OP_SEND, &send).await.0, OP_SEND_ACK);
    let old_device = old.device;
    // Pending old-key session is registered as soon as Noise reveals its key.
    let mut pending = Client::connect_noise(&srv, [11; 32]).await;
    let mut a = Client::connect(&srv, [13; 32]).await;
    let mut b = Client::connect(&srv, [14; 32]).await;
    assert_eq!(
        a.login("alice", None).await,
        (OP_REPLACE_REQUIRED, old_device.to_vec())
    );
    assert_eq!(
        b.login("alice", Some(&[0; 32])).await,
        (OP_REPLACE_REQUIRED, old_device.to_vec())
    );
    assert_eq!(counts(&srv), (2, 2, 2));
    let db = srv.db();
    db.execute(
        "INSERT INTO prekeys VALUES(?1,1,?2,?3,1,0)",
        rusqlite::params![
            old_device.as_slice(),
            [1u8; 32].as_slice(),
            [1u8; 64].as_slice()
        ],
    )
    .unwrap();
    drop(db);
    let (ra, rb) = tokio::join!(
        a.login("alice", Some(&old_device)),
        b.login("alice", Some(&old_device))
    );
    let (winner, loser, first) = if ra.0 == OP_AUTHENTICATED {
        (&mut a, &mut b, ra.clone())
    } else {
        (&mut b, &mut a, rb.clone())
    };
    assert_eq!(first, original);
    let challenge = if winner.device == public([13; 32]) {
        rb.clone()
    } else {
        ra.clone()
    };
    assert_eq!(challenge, (OP_REPLACE_REQUIRED, winner.device.to_vec()));
    old.closed().await;
    pending.closed().await;
    assert_eq!(winner.login("alice", Some(&old_device)).await, original); // idempotent confirmation
    assert_eq!(loser.login("alice", Some(&old_device)).await, challenge); // renewed confirmation needed
    assert!(
        mailbox::parse_fetch_resp(&winner.exchange(OP_FETCH, &[]).await.1)
            .unwrap()
            .is_empty()
    );
    send[16..32].fill(2);
    assert_eq!(sender.exchange(OP_SEND, &send).await.0, OP_SEND_ACK);
    let fetched = winner.exchange(OP_FETCH, &[]).await.1;
    let events = mailbox::parse_fetch_resp(&fetched).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sender_user,
        auth::parse_authenticated(&sender.exchange(OP_RESUME, &[]).await.1)
            .unwrap()
            .user_id
    );
    assert_eq!(events[0].sender, sender.device);
    let db = srv.db();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM prekeys WHERE device_key=?1",
            [old_device.as_slice()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(counts(&srv), (2, 3, 2));
    drop(db);
    let path = srv.public_file("old.hex", &old_device);
    assert_eq!(
        srv.ctl(&["device-unblock", "--file", path.to_str().unwrap()]),
        "err\n"
    );
    let mut retired = Client::connect(&srv, [11; 32]).await;
    assert_eq!(retired.exchange(OP_RESUME, &[]).await, error(ERR_REVOKED));
    assert_eq!(retired.login("alice", None).await, error(ERR_REVOKED));
    let winner_private = if winner.device == public([13; 32]) {
        [13; 32]
    } else {
        [14; 32]
    };
    drop((a, b, sender, retired));
    srv.restart();
    let mut c = Client::connect(&srv, winner_private).await;
    assert_eq!(c.exchange(OP_RESUME, &[]).await, original);
}

#[tokio::test]
async fn block_unblock_active_only_and_valid_credentials_required_for_replacement() {
    let srv = Server::new("blocks");
    srv.ctl(&["registration-mode", "open"]);
    let mut a = Client::connect(&srv, [15; 32]).await;
    let original = a.signup("alice", None).await;
    let path = srv.public_file("active.hex", &a.device);
    let mut pending = Client::connect(&srv, [15; 32]).await;
    assert_eq!(
        srv.ctl(&["device-block", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    a.closed().await;
    pending.closed().await;
    let mut a = Client::connect(&srv, [15; 32]).await;
    assert_eq!(a.exchange(OP_RESUME, &[]).await, error(ERR_REVOKED));
    assert_eq!(
        srv.ctl(&["device-unblock", "--file", path.to_str().unwrap()]),
        "ok\n"
    );
    assert_eq!(a.exchange(OP_RESUME, &[]).await, original);
    let mut b = Client::connect(&srv, [16; 32]).await;
    b.signup("bob", None).await;
    assert_eq!(
        b.login("alice", Some(&a.device)).await,
        error(ERR_BOUND_OTHER)
    );
    assert_eq!(counts(&srv), (2, 2, 2));
}

#[tokio::test]
async fn confirmed_login_lost_response_retries_after_policy_change_and_restart() {
    let mut srv = Server::new("lost-confirm");
    srv.ctl(&["registration-mode", "open"]);
    let mut old = Client::connect(&srv, [17; 32]).await;
    let original = old.signup("alice", None).await;
    let mut fresh = Client::connect(&srv, [18; 32]).await;
    assert_eq!(
        fresh.login("alice", None).await,
        (OP_REPLACE_REQUIRED, old.device.to_vec())
    );
    let confirm = auth::build_login("alice", PASSWORD, Some(&old.device)).unwrap();
    fresh.send(OP_LOGIN, &confirm).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while counts(&srv).1 != 2 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // Observe commit but discard the first AUTHENTICATED response.
    drop(fresh);
    old.closed().await;
    srv.ctl(&["registration-mode", "invite_only"]);
    srv.restart();
    let mut fresh = Client::connect(&srv, [18; 32]).await;
    assert_eq!(fresh.exchange(OP_LOGIN, &confirm).await, original);
    assert_eq!(fresh.exchange(OP_RESUME, &[]).await, original);
    assert_eq!(counts(&srv), (1, 2, 1));
}

#[tokio::test]
async fn binding_is_authenticated_exact_user_lookup_with_immutable_active_keys() {
    let srv = Server::new("bindings");
    srv.ctl(&["registration-mode", "open"]);
    let mut a = Client::connect(&srv, [19; 32]).await;
    let original = a.signup("alice", None).await;
    let user = auth::parse_authenticated(&original.1).unwrap().user_id;
    let mut b = Client::connect(&srv, [20; 32]).await;
    b.signup("bob", None).await;
    let mut outsider = Client::connect(&srv, [21; 32]).await;
    outsider.send(OP_DEVICE_BINDING, &user).await;
    outsider.closed().await;
    assert_eq!(
        b.exchange(OP_DEVICE_BINDING, &user).await,
        error(ERR_NO_PREKEY)
    );
    let ed = ed25519_dalek::SigningKey::from_bytes(&[22; 32])
        .verifying_key()
        .to_bytes();
    let curve = public([23; 32]);
    let mut upload = [ed.as_slice(), curve.as_slice(), &[0, 0]].concat();
    assert_eq!(
        a.exchange(OP_UPLOAD_PREKEYS, &upload).await,
        (OP_COUNT_RESP, vec![0; 4])
    );
    let expected = auth::build_binding(&user, &a.device, &ed, &curve);
    assert_eq!(
        b.exchange(OP_DEVICE_BINDING, &user).await,
        (OP_DEVICE_BINDING_RESP, expected.clone())
    );
    upload[32] ^= 1;
    assert_eq!(a.exchange(OP_UPLOAD_PREKEYS, &upload).await, error(ERR_BAD));
    upload[32] ^= 1;
    upload[..32].copy_from_slice(
        &ed25519_dalek::SigningKey::from_bytes(&[24; 32])
            .verifying_key()
            .to_bytes(),
    );
    assert_eq!(a.exchange(OP_UPLOAD_PREKEYS, &upload).await, error(ERR_BAD));
    assert_eq!(
        b.exchange(OP_DEVICE_BINDING, &user).await,
        (OP_DEVICE_BINDING_RESP, expected)
    );
    assert_eq!(
        b.exchange(OP_DEVICE_BINDING, &[255; 16]).await,
        error(ERR_NO_PREKEY)
    );
    let mut new = Client::connect(&srv, [25; 32]).await;
    assert_eq!(new.login("alice", Some(&a.device)).await, original);
    assert_eq!(
        b.exchange(OP_DEVICE_BINDING, &user).await,
        error(ERR_NO_PREKEY)
    );
    let new_ed = ed25519_dalek::SigningKey::from_bytes(&[26; 32])
        .verifying_key()
        .to_bytes();
    let new_curve = public([27; 32]);
    let upload = [new_ed.as_slice(), new_curve.as_slice(), &[0, 0]].concat();
    assert_eq!(
        new.exchange(OP_UPLOAD_PREKEYS, &upload).await.0,
        OP_COUNT_RESP
    );
    assert_eq!(
        b.exchange(OP_DEVICE_BINDING, &user).await,
        (
            OP_DEVICE_BINDING_RESP,
            auth::build_binding(&user, &new.device, &new_ed, &new_curve)
        )
    );
}

#[tokio::test]
async fn send_fetch_expanded_record_boundary_is_deliverable_and_oversize_rolls_back() {
    let srv = Server::new("fetch-boundary");
    srv.ctl(&["registration-mode", "open"]);
    let mut a = Client::connect(&srv, [28; 32]).await;
    let authenticated = a.signup("alice", None).await;
    let user = auth::parse_authenticated(&authenticated.1).unwrap().user_id;
    let mut b = Client::connect(&srv, [29; 32]).await;
    b.signup("bob", None).await;
    let limit = CIPHERTEXT_MAX;
    let mut send = [user.as_slice(), &[1; 16], vec![0x71; limit].as_slice()].concat();
    assert_eq!(b.exchange(OP_SEND, &send).await.0, OP_SEND_ACK);
    let (op, payload) = a.exchange(OP_FETCH, &[]).await;
    assert_eq!(op, OP_FETCH_RESP);
    assert_eq!(payload.len(), MAX_PAYLOAD);
    let events = mailbox::parse_fetch_resp(&payload).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ciphertext, vec![0x71; limit]);
    send[16..32].fill(2);
    send.push(0x71);
    assert_eq!(b.exchange(OP_SEND, &send).await, error(ERR_BAD));
    assert_eq!(
        srv.db()
            .query_row("SELECT COUNT(*) FROM mailbox_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn full_mailbox_retry_preserves_ack_ciphertext_and_quota_after_restart() {
    let mut srv = Server::new("full-mailbox-retry");
    srv.ctl(&["registration-mode", "open"]);
    let mut recipient = Client::connect(&srv, [33; 32]).await;
    let authenticated = recipient.signup("alice", None).await;
    let user = auth::parse_authenticated(&authenticated.1).unwrap().user_id;
    let mut sender = Client::connect(&srv, [34; 32]).await;
    sender.signup("bob", None).await;
    let id = [1; 16];
    let original_send = [user.as_slice(), id.as_slice(), b"original ciphertext"].concat();
    let accepted = sender.exchange(OP_SEND, &original_send).await;
    assert_eq!(
        accepted,
        (OP_SEND_ACK, [id.as_slice(), &[ST_ACCEPTED]].concat())
    );
    let (seq, created_at) = {
        let mut db = srv.db();
        let original: (i64, i64) = db.query_row("SELECT seq,created_at FROM mailbox_events WHERE sender_device=?1 AND message_id=?2", rusqlite::params![sender.device.as_slice(), id.as_slice()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let tx = db.transaction().unwrap();
        for n in 1..MAILBOX_EVENTS_MAX {
            let mut fill_id = [0; 16];
            fill_id[..8].copy_from_slice(&(n as u64).to_be_bytes());
            tx.execute("INSERT INTO mailbox_events(recipient_user_id,sender_device,message_id,ciphertext,created_at) VALUES(?1,?2,?3,?4,?5)", rusqlite::params![user.as_slice(), sender.device.as_slice(), fill_id.as_slice(), b"filler", original.1]).unwrap();
        }
        tx.commit().unwrap();
        original
    };
    let stats = || {
        srv.db()
            .query_row(
                "SELECT COUNT(*),SUM(LENGTH(ciphertext)) FROM mailbox_events",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap()
    };
    let full = stats();
    assert_eq!(full.0, MAILBOX_EVENTS_MAX as i64);
    drop((recipient, sender));
    srv.restart();
    let mut recipient = Client::connect(&srv, [33; 32]).await;
    assert_eq!(recipient.exchange(OP_RESUME, &[]).await, authenticated);
    let mut sender = Client::connect(&srv, [34; 32]).await;
    assert_eq!(sender.exchange(OP_RESUME, &[]).await.0, OP_AUTHENTICATED);
    let fresh_send = [user.as_slice(), &[2; 16], b"new ciphertext"].concat();
    assert_eq!(
        sender.exchange(OP_SEND, &fresh_send).await,
        error(ERR_QUOTA)
    );
    assert_eq!(sender.exchange(OP_SEND, &original_send).await, accepted);
    let changed_send = [
        user.as_slice(),
        id.as_slice(),
        b"changed ciphertext is still a duplicate",
    ]
    .concat();
    assert_eq!(sender.exchange(OP_SEND, &changed_send).await, accepted);
    assert_eq!(
        srv.db().query_row("SELECT seq,created_at,ciphertext FROM mailbox_events WHERE sender_device=?1 AND message_id=?2", rusqlite::params![sender.device.as_slice(), id.as_slice()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?))).unwrap(),
        (seq, created_at, b"original ciphertext".to_vec())
    );
    let ack = [
        1u16.to_be_bytes().as_slice(),
        (seq as u64).to_be_bytes().as_slice(),
    ]
    .concat();
    assert_eq!(
        recipient.exchange(OP_DELIVERY_ACK, &ack).await.0,
        OP_DELIVERY_ACK
    );
    assert_eq!(
        sender.exchange(OP_SEND, &changed_send).await,
        (OP_SEND_ACK, [id.as_slice(), &[ST_DELIVERED]].concat())
    );
    assert_eq!(
        sender.exchange(OP_SEND, &fresh_send).await,
        error(ERR_QUOTA)
    );
    assert_eq!(
        srv.db()
            .query_row(
                "SELECT COUNT(*),SUM(LENGTH(ciphertext)) FROM mailbox_events",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
        full
    );
}

#[tokio::test]
async fn diagnostic_example_signup_login_and_resume_use_the_requested_opcode() {
    let srv = Server::new("auth-diag");
    let (invite, _) = srv.issue();
    let signup = auth::build_signup("alice", PASSWORD, Some(&invite)).unwrap();
    let original = auth_diag::run(
        srv.port,
        DOMAIN,
        &srv.server_pub,
        &[30; 32],
        OP_SIGNUP,
        &signup,
    )
    .await
    .expect("diagnostic signup");
    let login = auth::build_login("alice", PASSWORD, None).unwrap();
    assert_eq!(
        auth_diag::run(
            srv.port,
            DOMAIN,
            &srv.server_pub,
            &[30; 32],
            OP_LOGIN,
            &login
        )
        .await,
        Some(original.clone())
    );
    assert_eq!(
        auth_diag::run(srv.port, DOMAIN, &srv.server_pub, &[30; 32], OP_RESUME, &[]).await,
        Some(original)
    );
}

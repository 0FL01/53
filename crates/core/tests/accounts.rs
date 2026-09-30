//! Unified signup/login/resume and pin regressions against isolated live msgd.
mod support;
use dmsg_core::{
    auth::{self, LoginOutcome},
    AuthError, Core,
};

#[tokio::test]
async fn lost_signup_and_confirm_responses_retry_with_the_same_durable_device_key() {
    use dmsg_core::Transport;
    let srv = LiveMsgd::start("lost-auth", "lost.test");
    let db = srv.dir.join("pending.db");
    let invite = srv.issue("pending.invite", "3600");
    let secret = dmsg_protocol::auth::parse_invitation(&invite).unwrap();
    let mut conn = dmsg_core::store::open(&db).unwrap();
    let key = auth::pending_key(&mut conn).unwrap();
    drop(conn);
    let mut t =
        dmsg_core::initiate_with_key(&srv.addr, &srv.server_pub, srv.domain.as_bytes(), &key)
            .await
            .unwrap();
    t.send_frame(
        dmsg_protocol::OP_SIGNUP,
        &dmsg_protocol::auth::build_signup("alice", PASSWORD, Some(&secret)).unwrap(),
    )
    .await
    .unwrap();
    let (op, payload) = t.recv_frame().await.unwrap();
    assert_eq!(op, dmsg_protocol::OP_AUTHENTICATED);
    t.close().await;
    assert_eq!(
        dmsg_core::store::load_account(&dmsg_core::store::open(&db).unwrap()).unwrap(),
        None
    );
    let account = dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        Some(&invite),
    )
    .await
    .unwrap();
    assert_eq!(
        dmsg_core::store::load_identity(&dmsg_core::store::open(&db).unwrap()).unwrap(),
        Some(key)
    );
    assert_eq!(
        account.user_id,
        dmsg_protocol::auth::parse_authenticated(&payload)
            .unwrap()
            .user_id
    );
    let new_db = srv.dir.join("replace.db");
    let old = match dmsg_core::login_direct(
        &srv.code,
        &srv.addr,
        &new_db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        None,
    )
    .await
    .unwrap()
    {
        LoginOutcome::ReplacementRequired(k) => k,
        _ => panic!("replacement confirmation required"),
    };
    let new_key = dmsg_core::store::load_identity(&dmsg_core::store::open(&new_db).unwrap())
        .unwrap()
        .unwrap();
    let mut t =
        dmsg_core::initiate_with_key(&srv.addr, &srv.server_pub, srv.domain.as_bytes(), &new_key)
            .await
            .unwrap();
    t.send_frame(
        dmsg_protocol::OP_LOGIN,
        &dmsg_protocol::auth::build_login("alice", PASSWORD, Some(&old)).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        t.recv_frame().await.unwrap().0,
        dmsg_protocol::OP_AUTHENTICATED
    );
    t.close().await;
    assert_eq!(
        dmsg_core::store::load_account(&dmsg_core::store::open(&new_db).unwrap()).unwrap(),
        None
    );
    assert_eq!(
        dmsg_core::login_direct(
            &srv.code,
            &srv.addr,
            &new_db,
            Some(DER),
            None,
            "alice",
            PASSWORD,
            Some(&old)
        )
        .await
        .unwrap(),
        LoginOutcome::Authenticated(account)
    );
    assert_eq!(
        dmsg_core::store::load_identity(&dmsg_core::store::open(&new_db).unwrap()).unwrap(),
        Some(new_key)
    );
    let mut old_channel = srv.connect(&db).await;
    old_channel
        .send_frame(
            dmsg_protocol::OP_LOGIN,
            &dmsg_protocol::auth::build_login(
                "alice",
                PASSWORD,
                Some(&dmsg_core::olm::device_pubkey(&new_key)),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        old_channel.recv_frame().await.unwrap(),
        (dmsg_protocol::OP_ERROR, vec![dmsg_protocol::ERR_REVOKED])
    );
}
use support::*;

#[tokio::test]
async fn signup_reconnect_invitation_used_and_encrypted_store() {
    let srv = LiveMsgd::start("accounts", "accounts.test");
    let db = srv.dir.join("a.db");
    let invite = srv.issue("inv", "3600");
    let pv = dmsg_core::preview(&srv.code).unwrap();
    assert_eq!(pv.pin_fingerprint, auth::pin_fingerprint(DER));
    let a = dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &db,
        Some(DER),
        None,
        "Alice",
        PASSWORD,
        Some(&invite),
    )
    .await
    .unwrap();
    let mut core = Core::open(&db).unwrap();
    let mut t = srv.connect(&db).await;
    assert_eq!(core.on_reconnect(&mut t).await.unwrap(), 16);
    core.login(&mut t).await.unwrap();
    assert_eq!(
        core.my_account().unwrap(),
        (a.user_id, a.contact_id.clone())
    );
    assert_eq!(
        dmsg_core::signup_direct(
            &srv.code,
            &srv.addr,
            &db,
            Some(DER),
            None,
            "other",
            PASSWORD,
            Some(&invite)
        )
        .await,
        Err(AuthError::AlreadyAuthenticated)
    );
    assert_eq!(
        dmsg_core::signup_direct(
            &srv.code,
            &srv.addr,
            &srv.dir.join("b.db"),
            Some(DER),
            None,
            "bobby",
            PASSWORD,
            Some(&invite)
        )
        .await,
        Err(AuthError::InviteUsed)
    );
    let invite = srv.issue("sealed", "3600");
    let sealed = srv.dir.join("sealed.db");
    let a = dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &sealed,
        Some(DER),
        Some(&[0x37; 32]),
        "sealed",
        PASSWORD,
        Some(&invite),
    )
    .await
    .unwrap();
    let client =
        dmsg_core::ffi::DmsgClient::open_encrypted(sealed.to_string_lossy().into(), vec![0x37; 32])
            .unwrap();
    assert!(client.account_info().unwrap().authenticated);
    assert_eq!(
        client.account_info().unwrap().contact_id,
        Some(a.contact_id)
    );
    assert!(dmsg_core::store::open(&sealed).is_err());
}
#[tokio::test]
async fn wrong_pin_and_noise_key_never_accept_account() {
    let srv = LiveMsgd::start("pins", "pins.test");
    let db = srv.dir.join("bad.db");
    let r = dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &db,
        Some(&[0x30, 0]),
        None,
        "alice",
        PASSWORD,
        None,
    )
    .await;
    assert_eq!(r, Err(AuthError::PinMismatch));
    assert!(!db.exists());
    let wrong = dmsg_protocol::profile::build(b"pins.test", DER, &[9; 32]).unwrap();
    let r = dmsg_core::signup_direct(
        &wrong,
        &srv.addr,
        &db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        None,
    )
    .await;
    assert!(matches!(r, Err(AuthError::Transport(_))));
    let conn = dmsg_core::store::open(&db).unwrap();
    assert_eq!(dmsg_core::store::load_account(&conn).unwrap(), None);
}
#[tokio::test]
async fn expired_revoked_invitation_does_not_control_device_resume() {
    let srv = LiveMsgd::start("invite-life", "invites.test");
    let invite = srv.issue("expired", "1");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(
        dmsg_core::signup_direct(
            &srv.code,
            &srv.addr,
            &srv.dir.join("expired.db"),
            Some(DER),
            None,
            "expired",
            PASSWORD,
            Some(&invite)
        )
        .await,
        Err(AuthError::InviteExpired)
    );
    let invite = srv.issue("revoked", "3600");
    srv.revoke(&invite);
    assert_eq!(
        dmsg_core::signup_direct(
            &srv.code,
            &srv.addr,
            &srv.dir.join("revoked.db"),
            Some(DER),
            None,
            "revoked",
            PASSWORD,
            Some(&invite)
        )
        .await,
        Err(AuthError::InviteRevoked)
    );
    let db = srv.dir.join("active.db");
    srv.signup(&db, "active").await;
    assert_eq!(srv.ctl(&["registration-mode", "open"]), "open");
    let mut t = srv.connect(&db).await;
    Core::open(&db).unwrap().login(&mut t).await.unwrap();
}
#[tokio::test]
async fn policy_open_login_cancel_confirm_and_old_device_denied() {
    let srv = LiveMsgd::start("replacement", "replace.test");
    let mut t = dmsg_core::initiate(&srv.addr, &srv.server_pub, srv.domain.as_bytes())
        .await
        .unwrap();
    assert_eq!(
        auth::registration_policy(&mut t).await.unwrap(),
        dmsg_protocol::auth::RegistrationMode::InviteOnly
    );
    assert_eq!(srv.ctl(&["registration-mode", "open"]), "open");
    assert_eq!(
        auth::registration_policy(&mut t).await.unwrap(),
        dmsg_protocol::auth::RegistrationMode::Open
    );
    let old_db = srv.dir.join("old.db");
    let account = dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &old_db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        None,
    )
    .await
    .unwrap();
    let new_db = srv.dir.join("new.db");
    assert_eq!(
        dmsg_core::login_direct(
            &srv.code,
            &srv.addr,
            &new_db,
            Some(DER),
            None,
            "alice",
            "wrong password",
            None
        )
        .await,
        Err(AuthError::InvalidCredentials)
    );
    let expected = match dmsg_core::login_direct(
        &srv.code,
        &srv.addr,
        &new_db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        None,
    )
    .await
    .unwrap()
    {
        LoginOutcome::ReplacementRequired(key) => key,
        _ => panic!("must request explicit confirmation"),
    };
    let mut old = Core::open(&old_db).unwrap();
    let mut t = srv.connect(&old_db).await;
    old.login(&mut t).await.unwrap();
    assert_eq!(
        dmsg_core::store::load_account(&dmsg_core::store::open(&new_db).unwrap()).unwrap(),
        None,
        "cancel has no account effect"
    );
    let outcome = dmsg_core::login_direct(
        &srv.code,
        &srv.addr,
        &new_db,
        Some(DER),
        None,
        "alice",
        PASSWORD,
        Some(&expected),
    )
    .await
    .unwrap();
    assert_eq!(outcome, LoginOutcome::Authenticated(account));
    let mut t = srv.connect(&old_db).await;
    assert!(old.login(&mut t).await.is_err());
    let mut new = Core::open(&new_db).unwrap();
    let mut t = srv.connect(&new_db).await;
    new.on_reconnect(&mut t).await.unwrap();
}

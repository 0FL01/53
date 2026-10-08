//! Core invitation/auth isolation against scripted streams and fresh live msgd.
//! Direct TCP here is host evidence, not Android DNS acceptance.
mod support;
use dmsg_core::{
    auth::AuthError,
    ffi::{self, DmsgClient, FfiError},
    olm::OlmError,
    Core, Transport, TransportError,
};
use dmsg_protocol::{
    invitation::{self, Issued, Metadata, State},
    *,
};
use std::collections::VecDeque;
use support::*;

struct Script {
    replies: VecDeque<(u8, Vec<u8>)>,
    sent: Vec<(u8, Vec<u8>)>,
}
impl Transport for Script {
    async fn connect(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
    async fn send_frame(&mut self, op: u8, payload: &[u8]) -> Result<(), TransportError> {
        self.sent.push((op, payload.to_vec()));
        Ok(())
    }
    async fn recv_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
        self.replies.pop_front().ok_or(TransportError::Closed)
    }
    async fn close(&mut self) {}
    fn is_connected(&self) -> bool {
        true
    }
}
fn phrase() -> String {
    [invitation::word(0).unwrap(); 6].join(" ")
}
fn issued(id: [u8; 16]) -> Issued {
    Issued {
        server_now: 100,
        invitation: Metadata {
            issue_id: id,
            created_at: 100,
            expires_at: 100 + invitation::TTL_SECONDS,
        },
        state: State::Active,
        phrase: Some(phrase()),
    }
}
fn local(tag: &str) -> (Core, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "dmsg-invitation-unit-{}-{tag}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let c = dmsg_core::store::open(&path).unwrap();
    dmsg_core::store::save_identity(&c, &[7; 32]).unwrap();
    dmsg_core::store::save_account(&c, &[8; 16], "ABCD1234EFGH").unwrap();
    drop(c);
    (Core::open(&path).unwrap(), path)
}
fn script(reply: (u8, Vec<u8>)) -> Script {
    Script {
        replies: VecDeque::from([
            (
                OP_AUTHENTICATED,
                dmsg_protocol::auth::build_authenticated(&[8; 16], "ABCD1234EFGH").unwrap(),
            ),
            reply,
        ]),
        sent: vec![],
    }
}

#[tokio::test]
async fn each_command_resumes_once_and_foreign_account_stops_before_command() {
    let (mut core, path) = local("resume");
    for command in 0..3 {
        let mut s = script((OP_INVITE_REVOKED, vec![]));
        s.replies[0] = (
            OP_AUTHENTICATED,
            dmsg_protocol::auth::build_authenticated(&[9; 16], "ABCD1234EFGH").unwrap(),
        );
        let error = match command {
            0 => core.issue_invitation(&mut s, &[1; 16]).await.err().unwrap(),
            1 => core.list_invitations(&mut s).await.err().unwrap(),
            _ => core
                .revoke_invitation(&mut s, &[1; 16])
                .await
                .err()
                .unwrap(),
        };
        assert_eq!(error, OlmError::Protocol("authenticated account mismatch"));
        assert_eq!(s.sent, vec![(OP_RESUME, vec![])]);
    }
    let mut s = script((
        OP_INVITE_ISSUED,
        invitation::build_issued(&issued([1; 16])).unwrap(),
    ));
    let result = core.issue_invitation(&mut s, &[1; 16]).await.unwrap();
    assert_eq!(result.phrase.as_deref(), Some(phrase().as_str()));
    assert_eq!(
        s.sent,
        vec![(OP_RESUME, vec![]), (OP_INVITE_ISSUE, vec![1; 16])]
    );
    let mut s = script((
        OP_INVITE_LISTED,
        invitation::build_list(&invitation::List {
            server_now: 100,
            invitations: vec![issued([1; 16]).invitation],
        })
        .unwrap(),
    ));
    assert_eq!(
        core.list_invitations(&mut s)
            .await
            .unwrap()
            .invitations
            .len(),
        1
    );
    assert_eq!(s.sent, vec![(OP_RESUME, vec![]), (OP_INVITE_LIST, vec![])]);
    let mut s = script((OP_INVITE_REVOKED, vec![]));
    core.revoke_invitation(&mut s, &[1; 16]).await.unwrap();
    assert_eq!(
        s.sent,
        vec![(OP_RESUME, vec![]), (OP_INVITE_REVOKE, vec![1; 16])]
    );
    // A Core opened earlier must still check the current durable acceptance.
    let conn = dmsg_core::store::open(&path).unwrap();
    conn.execute("DELETE FROM core_account", []).unwrap();
    drop(conn);
    let mut s = script((OP_INVITE_REVOKED, vec![]));
    assert_eq!(
        core.revoke_invitation(&mut s, &[1; 16]).await,
        Err(OlmError::NotEnrolled)
    );
    assert!(s.sent.is_empty());
    drop(core);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn strict_replies_and_invitation_quota_are_secret_free() {
    let (mut core, path) = local("strict");
    let valid = invitation::build_issued(&issued([1; 16])).unwrap();
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut terminal_secret = valid.clone();
    terminal_secret[40] = 1;
    let mut future_creation = valid.clone();
    future_creation[24..32].copy_from_slice(&101_i64.to_be_bytes());
    for reply in [
        (OP_INVITE_LISTED, valid.clone()),
        (
            OP_INVITE_ISSUED,
            invitation::build_issued(&issued([2; 16])).unwrap(),
        ),
        (OP_INVITE_ISSUED, trailing),
        (OP_INVITE_ISSUED, terminal_secret),
        (OP_INVITE_ISSUED, future_creation),
        (OP_ERROR, vec![ERR_QUOTA, 0]),
    ] {
        let error = core
            .issue_invitation(&mut script(reply), &[1; 16])
            .await
            .unwrap_err();
        assert!(matches!(error, OlmError::Protocol(_)));
        assert!(!format!("{error:?}").contains(&phrase()));
    }
    assert_eq!(
        core.issue_invitation(&mut script((OP_ERROR, vec![ERR_QUOTA])), &[1; 16])
            .await
            .unwrap_err(),
        OlmError::Auth(AuthError::InviteLimit)
    );
    assert_eq!(
        dmsg_core::auth::map_error(ERR_QUOTA, false),
        AuthError::Server(ERR_QUOTA)
    );
    assert!(matches!(
        core.revoke_invitation(&mut script((OP_INVITE_REVOKED, vec![0])), &[1; 16])
            .await,
        Err(OlmError::Protocol(_))
    ));
    let mut list = invitation::build_list(&invitation::List {
        server_now: 100,
        invitations: vec![issued([1; 16]).invitation],
    })
    .unwrap();
    list.push(0);
    assert!(matches!(
        core.list_invitations(&mut script((OP_INVITE_LISTED, list)))
            .await,
        Err(OlmError::Protocol(_))
    ));
    drop(core);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pure_token_and_ffi_account_preflight_do_not_start_dns() {
    let token = ffi::invitation_token(phrase()).unwrap();
    assert_eq!(token.len(), 43);
    assert_eq!(ffi::invitation_token(token.clone()).unwrap(), token);
    assert_eq!(
        dmsg_protocol::auth::parse_invitation(&token).unwrap(),
        invitation::parse_invitation_input(&phrase()).unwrap()
    );
    for input in [
        "dmsg://server/AAAA".into(),
        "a".repeat(257),
        "wrong phrase".into(),
    ] {
        assert_eq!(ffi::invitation_token(input), Err(FfiError::InvalidInput));
    }
    let path = std::env::temp_dir().join(format!("dmsg-invitation-ffi-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let client = DmsgClient::open(path.to_string_lossy().into());
    assert_eq!(
        client.issue_invitation_dns(vec![1; 15]),
        Err(FfiError::InvalidInput)
    );
    assert!(!path.exists());
    assert_eq!(client.list_invitations_dns(), Err(FfiError::NotEnrolled));
    assert_eq!(
        client.issue_invitation_dns(vec![1; 16]),
        Err(FfiError::NotEnrolled)
    );
    assert_eq!(
        client.revoke_invitation_dns(vec![1; 16]),
        Err(FfiError::NotEnrolled)
    );
    assert_eq!(client.dns_status().unwrap(), "stopped");
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn live_issue_restart_phrase_qr_signup_owner_isolation_and_terminal_retry() {
    let mut srv = LiveMsgd::start("core-self-invitations", "invitation.test");
    let owner_db = srv.dir.join("owner.db");
    srv.signup(&owner_db, "owner").await;
    let mut core = Core::open(&owner_db).unwrap();
    let mut t = srv.connect(&owner_db).await;
    let first = core.issue_invitation(&mut t, &[1; 16]).await.unwrap();
    assert_eq!(first.state, State::Active);
    assert_eq!(
        core.list_invitations(&mut t)
            .await
            .unwrap()
            .invitations
            .len(),
        1
    );
    t.close().await;
    srv.restart();
    let mut t = srv.connect(&owner_db).await;
    let retried = core.issue_invitation(&mut t, &[1; 16]).await.unwrap();
    assert!(
        retried.phrase == first.phrase,
        "same-ID retry must recover the original phrase"
    );
    assert_eq!(retried.invitation, first.invitation);
    let phrase = first.phrase.as_deref().unwrap();
    dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &srv.dir.join("phrase.db"),
        Some(DER),
        None,
        "recipient",
        PASSWORD,
        Some(phrase),
    )
    .await
    .unwrap();
    let used = core.issue_invitation(&mut t, &[1; 16]).await.unwrap();
    assert_eq!(used.state, State::Used);
    assert!(used.phrase.is_none());
    core.revoke_invitation(&mut t, &[1; 16]).await.unwrap();
    let second = core.issue_invitation(&mut t, &[2; 16]).await.unwrap();
    let qr = ffi::invitation_token(second.phrase.clone().unwrap()).unwrap();
    dmsg_core::signup_direct(
        &srv.code,
        &srv.addr,
        &srv.dir.join("qr.db"),
        Some(DER),
        None,
        "qruser",
        PASSWORD,
        Some(&qr),
    )
    .await
    .unwrap();
    assert_eq!(
        dmsg_core::signup_direct(
            &srv.code,
            &srv.addr,
            &srv.dir.join("replay.db"),
            Some(DER),
            None,
            "replay",
            PASSWORD,
            Some(second.phrase.as_deref().unwrap())
        )
        .await,
        Err(AuthError::InviteUsed)
    );
    let third = core.issue_invitation(&mut t, &[3; 16]).await.unwrap();
    let third_phrase = third.phrase.as_deref().unwrap();
    let other_db = srv.dir.join("other.db");
    srv.signup(&other_db, "other").await;
    let mut other = Core::open(&other_db).unwrap();
    let mut ot = srv.connect(&other_db).await;
    assert!(other
        .list_invitations(&mut ot)
        .await
        .unwrap()
        .invitations
        .is_empty());
    assert_eq!(
        other.revoke_invitation(&mut ot, &[3; 16]).await,
        Err(OlmError::Auth(AuthError::InvalidInput))
    );
    core.revoke_invitation(&mut t, &[3; 16]).await.unwrap();
    let revoked = core.issue_invitation(&mut t, &[3; 16]).await.unwrap();
    assert_eq!(revoked.state, State::Revoked);
    assert!(revoked.phrase.is_none());
    assert_eq!(
        dmsg_core::signup_direct(
            &srv.code,
            &srv.addr,
            &srv.dir.join("revoked.db"),
            Some(DER),
            None,
            "revoked",
            PASSWORD,
            Some(third_phrase)
        )
        .await,
        Err(AuthError::InviteRevoked)
    );
    assert!(core
        .list_invitations(&mut t)
        .await
        .unwrap()
        .invitations
        .is_empty());
    t.close().await;
    ot.close().await;
}

//! Pinned Noise AUTH_DOMAIN→WELCOME against isolated live v2 msgd. This is a
//! direct Rust transport harness, not evidence for Android DNS acceptance.
mod support;
use dmsg_core::Transport;
use support::LiveMsgd;
#[tokio::test]
async fn auth_welcome_against_live_msgd() {
    let srv = LiveMsgd::start("welcome", "welcome.test");
    let ch = dmsg_core::initiate(&srv.addr, &srv.server_pub, srv.domain.as_bytes())
        .await
        .expect("AUTH_DOMAIN→WELCOME");
    assert!(ch.is_connected());
}
#[tokio::test]
async fn wrong_domain_gets_no_welcome() {
    let srv = LiveMsgd::start("wrongdom", "welcome.test");
    assert!(
        dmsg_core::initiate(&srv.addr, &srv.server_pub, b"evil.example")
            .await
            .is_err()
    );
}

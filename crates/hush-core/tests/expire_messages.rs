mod presence_helper;

use hush_core::Account;
use hush_relay::{router, RelayConfig};
use presence_helper::recipient_online;
use tempfile::TempDir;
use tokio::time::{sleep, Duration};

async fn spawn_relay() -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    sleep(Duration::from_millis(80)).await;
    (base, dir)
}

/// Two contacts with each other's queue grants applied.
async fn pair() -> (Account, Account, TempDir, TempDir, TempDir) {
    let (relay, relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.recv().await.unwrap();
    bob.recv().await.unwrap();
    (alice, bob, relay_dir, alice_dir, bob_dir)
}

#[tokio::test]
async fn an_expiring_line_is_delivered_out_of_band_and_never_filed() {
    let (mut alice, mut bob, _relay_dir, _alice_dir, _bob_dir) = pair().await;
    recipient_online(&bob).await;

    let (id, expires_ms) = alice
        .send_expire_text("bob", "burn after reading", 30_000)
        .await
        .unwrap();
    assert!(expires_ms > 0);

    let inbox = bob.recv().await.unwrap();
    assert!(
        inbox.messages.is_empty(),
        "an expiring line is never a filed chat record"
    );
    assert_eq!(inbox.ephemeral.len(), 1);
    let line = &inbox.ephemeral[0];
    assert_eq!(line.from_nick, "alice");
    assert_eq!(line.id, id);
    assert_eq!(line.body, "burn after reading");
    assert_eq!(line.expires_ms, expires_ms);

    // Neither vault holds a chat row for it, on either side.
    assert!(alice.chat_history().is_empty(), "the sender filed nothing");
    assert!(bob.chat_history().is_empty(), "the receiver filed nothing");
}

#[tokio::test]
async fn an_expired_line_is_dropped_instead_of_shown_late() {
    let (mut alice, mut bob, _relay_dir, _alice_dir, _bob_dir) = pair().await;
    recipient_online(&bob).await;

    alice
        .send_expire_text("bob", "too late", 1_000)
        .await
        .unwrap();
    sleep(Duration::from_millis(1_200)).await;

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.messages.is_empty());
    assert!(
        inbox.ephemeral.is_empty(),
        "a line past its deadline is consumed, not delivered"
    );
    assert!(bob.chat_history().is_empty());
}

#[tokio::test]
async fn an_expiring_line_refuses_an_empty_body_or_an_out_of_range_lifetime() {
    let dir = TempDir::new().unwrap();
    let mut account = Account::create(dir.path(), "pw", "http://127.0.0.1:1")
        .await
        .unwrap();
    // Validation happens before contact lookup, so the relay never matters.
    assert!(account
        .send_expire_text("nobody", "   ", 30_000)
        .await
        .is_err());
    assert!(account.send_expire_text("nobody", "hi", 0).await.is_err());
    assert!(account
        .send_expire_text("nobody", "hi", 24 * 60 * 60 * 1_000 + 1)
        .await
        .is_err());
}

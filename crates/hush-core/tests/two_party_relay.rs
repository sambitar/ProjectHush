use hush_core::Account;
use hush_relay::{RelayConfig, router};
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

#[tokio::test]
async fn two_accounts_exchange_encrypted_text() {
    let (relay, _relay_dir) = spawn_relay().await;
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

    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();

    bob.send_text("alice", "hello from bob").await.unwrap();

    let messages = alice.recv().await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].from_nick, "bob");
    assert_eq!(
        messages[0].payload,
        hush_core::MessagePayload::Text {
            body: "hello from bob".into()
        }
    );

    alice
        .send_text("bob", "hello back from alice")
        .await
        .unwrap();
    let bob_messages = bob.recv().await.unwrap();
    assert_eq!(bob_messages.len(), 1);
    assert_eq!(
        bob_messages[0].payload,
        hush_core::MessagePayload::Text {
            body: "hello back from alice".into()
        }
    );
}

mod presence_helper;

use hush_core::{Account, InvitePayload, RelayClient};
use presence_helper::recipient_online;
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

    recipient_online(&alice).await;
    bob.send_text("alice", "hello from bob").await.unwrap();

    let inbox = alice.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    assert_eq!(inbox.messages[0].from_nick, "bob");
    assert_eq!(
        inbox.messages[0].payload,
        hush_core::MessagePayload::Text {
            body: "hello from bob".into()
        }
    );

    recipient_online(&bob).await;
    alice
        .send_text("bob", "hello back from alice")
        .await
        .unwrap();
    let bob_inbox = bob.recv().await.unwrap();
    assert_eq!(bob_inbox.messages.len(), 1);
    assert_eq!(
        bob_inbox.messages[0].payload,
        hush_core::MessagePayload::Text {
            body: "hello back from alice".into()
        }
    );
}

#[tokio::test]
async fn relay_blob_has_no_cleartext_sender_json() {
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

    recipient_online(&alice).await;
    bob.send_text("alice", "opaque wire test").await.unwrap();

    let alice_inv = InvitePayload::decode(&alice.invite().await.unwrap()).unwrap();
    let client = RelayClient::new(&relay);
    let ids = client
        .list(&alice_inv.queue_id, &alice_inv.queue_token)
        .await
        .unwrap();
    assert_eq!(ids.len(), 1);
    let body = client
        .get(&alice_inv.queue_id, &alice_inv.queue_token, &ids[0])
        .await
        .unwrap();
    assert!(
        !body.windows(7).any(|w| w == b"\"from\""),
        "relay blob must not be legacy JSON envelope"
    );
    let bob_id = bob.protocol_name();
    assert!(
        !body
            .windows(bob_id.len())
            .any(|w| w == bob_id.as_bytes()),
        "relay blob must not contain sender protocol name"
    );

    let inbox = alice.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
}

#[tokio::test]
async fn send_rejected_when_recipient_offline() {
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

    let err = bob.send_text("alice", "nope").await.unwrap_err();
    assert!(
        err.to_string().contains("offline"),
        "expected offline error, got {err}"
    );
}

#[tokio::test]
async fn remove_contact_drops_from_list() {
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
    assert_eq!(alice.contacts().len(), 1);

    alice.remove_contact("bob").await.unwrap();
    assert_eq!(alice.contacts().len(), 0);
    assert!(alice.remove_contact("bob").await.is_err());
}

#[tokio::test]
async fn chat_history_survives_reopen() {
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

    recipient_online(&alice).await;
    bob.send_text("alice", "persist me").await.unwrap();
    alice.recv().await.unwrap();

    let alice2 = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert_eq!(alice2.chat_history().len(), 1);
    assert_eq!(
        alice2.chat_history()[0].contact_nick,
        "bob"
    );
}

/// Revocation's effect on the wire. Once an account burns its own queue the
/// inbox is destroyed and its credential stops authorizing anything, so a
/// revoked device cannot read what it was sent and peers cannot queue more.
#[tokio::test]
async fn reporting_a_queue_burns_it() {
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

    recipient_online(&alice).await;
    bob.send_text("alice", "delivered before revocation")
        .await
        .unwrap();

    let inv = InvitePayload::decode(&alice.invite().await.unwrap()).unwrap();
    let client = RelayClient::new(&relay);

    // The device is live and its inbox holds the pending message.
    let ids = client.list(&inv.queue_id, &inv.queue_token).await.unwrap();
    assert_eq!(ids.len(), 1);

    client
        .report_queue(&inv.queue_id, &inv.queue_token)
        .await
        .unwrap();

    // The credential still parses, but it no longer authorizes anything: the
    // queue is gone, so there is nothing to read and nothing to report again.
    assert!(
        client.list(&inv.queue_id, &inv.queue_token).await.is_err(),
        "a burned queue must not be readable"
    );
    assert!(
        client
            .report_queue(&inv.queue_id, &inv.queue_token)
            .await
            .is_err(),
        "a burned queue must not accept further reports"
    );

    // And nothing further can be delivered into it.
    assert!(
        bob.send_text("alice", "after revocation").await.is_err(),
        "a burned queue must reject new deliveries"
    );
}

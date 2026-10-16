mod presence_helper;

use hush_core::{Account, RelayClient, ShortInvite};
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

/// `add_contact` sends the new contact a queue grant, and a grant that verifies
/// against the identity key their invite pinned is proof they added our invite.
/// Draining it has to leave the trust established rather than pending until the
/// first message happens to arrive: readiness is read off the stored grant, so
/// nothing has to record it separately.
#[tokio::test]
async fn a_verified_queue_grant_marks_the_contact_inbound_ready() {
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

    // Each side has added the other, and neither has drained yet.
    assert!(!alice.contacts()[0].inbound_ready());
    assert!(!bob.contacts()[0].inbound_ready());

    // Draining Alice's inbox consumes Bob's grant. It is not a message...
    let inbox = alice.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "a queue grant is not a message");

    // ...but it is the moment Alice learns Bob added her invite.
    assert!(
        alice.contacts()[0].inbound_ready(),
        "a verified queue grant means the peer added our invite"
    );
    assert!(
        !alice.contacts()[0].heard_from,
        "nothing has been filed from Bob, so the fallback is untouched: the \
         stored grant is what carries readiness here"
    );
    // And it has to survive a reopen, or the UI flips back to pending.
    drop(alice);
    let reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert!(reopened.contacts()[0].inbound_ready());
    assert!(!reopened.contacts()[0].heard_from);
}

/// The pointer invite end to end: the code names the account, the prekey
/// bundle comes from the relay's directory at accept time, and the session
/// works exactly as with a full inline invite.
#[tokio::test]
async fn a_pointer_invite_adds_a_contact_through_the_directory() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    // Account::create publishes the prekey record, so the directory can
    // already serve the bundle a pointer invite points at.
    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();

    let alice_invite = alice.invite().await.unwrap();
    assert!(
        alice_invite.starts_with("hush1"),
        "the shared invite is a pointer code: {alice_invite}"
    );
    assert!(
        alice_invite.len() < 250,
        "the code stays address-shaped: {} chars",
        alice_invite.len()
    );

    bob.add_contact("alice", &alice_invite).await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();

    recipient_online(&alice).await;
    bob.send_text("alice", "hello via the directory").await.unwrap();
    let inbox = alice.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    assert_eq!(inbox.messages[0].from_nick, "bob");
    assert_eq!(
        inbox.messages[0].payload,
        hush_core::MessagePayload::Text {
            body: "hello via the directory".into()
        }
    );
}

/// `add_contact` still accepts the legacy full-payload invites (v2 JSON and
/// v1 base64): old copies of the app and printed codes keep working.
#[tokio::test]
async fn a_legacy_full_invite_still_adds_a_contact() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();

    // Mint the legacy format directly: a fresh full bundle, exactly what a
    // peer on an older build shares.
    let legacy = alice.fresh_invite().await.unwrap();
    bob.add_contact("alice", &legacy).await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();

    recipient_online(&alice).await;
    bob.send_text("alice", "legacy path still fine").await.unwrap();
    let inbox = alice.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
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

    let alice_ptr = hush_core::ShortInvite::decode(&alice.invite().await.unwrap()).unwrap();
    let client = RelayClient::new(&relay);
    // Reading our own inbox needs the private read credential; the invite carries
    // only the delivery token, which must not authorize a read. Bob's add-contact
    // grant and his message are both queued here, so both ciphertexts are checked.
    let ids = client
        .list(&alice_ptr.queue_id, alice.own_read_token())
        .await
        .unwrap();
    assert_eq!(ids.len(), 2, "one queue grant and one message");
    let bob_id = bob.protocol_name();
    for id in &ids {
        let body = client
            .get(&alice_ptr.queue_id, alice.own_read_token(), id)
            .await
            .unwrap();
        assert!(
            !body.windows(7).any(|w| w == b"\"from\""),
            "relay blob must not be legacy JSON envelope"
        );
        assert!(
            !body
                .windows(bob_id.len())
                .any(|w| w == bob_id.as_bytes()),
            "relay blob must not contain sender protocol name"
        );
    }

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

    let inv = ShortInvite::decode(&alice.invite().await.unwrap()).unwrap();
    let client = RelayClient::new(&relay);

    // The device is live and its inbox holds the pending message plus bob's
    // queue grant. Only the private read credential can see them — the published
    // delivery token cannot.
    let read_token = alice.own_read_token().to_string();
    let ids = client.list(&inv.queue_id, &read_token).await.unwrap();
    assert_eq!(ids.len(), 2, "one queue grant and one message");

    client
        .report_queue(&inv.queue_id, &read_token)
        .await
        .unwrap();

    // The credential still parses, but it no longer authorizes anything: the
    // queue is gone, so there is nothing to read and nothing to report again.
    assert!(
        client.list(&inv.queue_id, &read_token).await.is_err(),
        "a burned queue must not be readable"
    );
    assert!(
        client
            .report_queue(&inv.queue_id, &read_token)
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

/// Appearing offline: the owner's `go_offline` clears its heartbeat, so a
/// contact's presence check flips to offline at once rather than waiting out
/// the relay's 90 s freshness window.
#[tokio::test]
async fn go_offline_hides_presence_from_a_contact_immediately() {
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
    assert!(
        bob.contact_is_online("alice").await.unwrap(),
        "a heartbeating vault reads online"
    );

    alice.go_offline().await.unwrap();
    assert!(
        !bob.contact_is_online("alice").await.unwrap(),
        "clearing the heartbeat reads offline at once"
    );
}

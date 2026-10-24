mod presence_helper;

use hush_core::{Account, MessagePayload};
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

/// 1:1 call signalling rides the session and lands in `InboxResult::calls`,
/// never in the chat log and never as an inbox row.
#[tokio::test]
async fn call_offer_answer_and_end_round_trip() {
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

    recipient_online(&bob).await;
    alice
        .send_call_offer("bob", "session-1", "v=0 offer sdp")
        .await
        .unwrap();

    let bob_inbox = bob.recv().await.unwrap();
    assert!(
        bob_inbox.messages.is_empty(),
        "call signalling must never be a chat row"
    );
    assert_eq!(bob_inbox.calls.len(), 1);
    assert_eq!(bob_inbox.calls[0].from_nick, "alice");
    assert_eq!(
        bob_inbox.calls[0].payload,
        MessagePayload::CallOffer {
            session_id: "session-1".into(),
            sdp: "v=0 offer sdp".into(),
        }
    );

    recipient_online(&alice).await;
    bob.send_call_answer("alice", "session-1", "v=0 answer sdp")
        .await
        .unwrap();
    let alice_inbox = alice.recv().await.unwrap();
    assert!(alice_inbox.messages.is_empty());
    assert_eq!(alice_inbox.calls.len(), 1);
    assert_eq!(
        alice_inbox.calls[0].payload,
        MessagePayload::CallAnswer {
            session_id: "session-1".into(),
            sdp: "v=0 answer sdp".into(),
        }
    );

    recipient_online(&bob).await;
    alice
        .send_call_end("bob", "session-1", "hangup")
        .await
        .unwrap();
    let bob_inbox = bob.recv().await.unwrap();
    assert!(bob_inbox.messages.is_empty());
    assert_eq!(bob_inbox.calls.len(), 1);
    assert_eq!(
        bob_inbox.calls[0].payload,
        MessagePayload::CallEnd {
            session_id: "session-1".into(),
            reason: "hangup".into(),
        }
    );
}

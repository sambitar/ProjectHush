use hush_core::{Account, ChatRecordPayload, InvitePayload, MessagePayload};
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
async fn a_handle_round_trips_through_an_invite() {
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let relay = "http://127.0.0.1:1";

    let mut alice = Account::create(alice_dir.path(), "pw", relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", relay).await.unwrap();

    alice.set_mail_handle("Sandy").await.unwrap();
    assert_eq!(alice.mail_handle(), Some("sandy"));
    assert_eq!(alice.mail_address().as_deref(), Some("sandy@myserver.com"));

    let invite = alice.invite().await.unwrap();
    let decoded = InvitePayload::decode(&invite).unwrap();
    assert_eq!(decoded.handle.as_deref(), Some("sandy"));

    bob.add_contact("alice", &invite).await.unwrap();
    assert_eq!(bob.contacts()[0].mail_handle.as_deref(), Some("sandy"));
}

#[tokio::test]
async fn setting_a_handle_invalidates_a_cached_invite() {
    let dir = TempDir::new().unwrap();
    let mut account = Account::create(dir.path(), "pw", "http://127.0.0.1:1")
        .await
        .unwrap();
    let before = account.invite().await.unwrap();
    account.set_mail_handle("sandy").await.unwrap();
    let after = account.invite().await.unwrap();
    assert_ne!(before, after, "a new handle must go out on the next invite");
    assert_eq!(
        InvitePayload::decode(&after).unwrap().handle.as_deref(),
        Some("sandy")
    );
}

/// The whole point of mail versus chat: it lands in the recipient's queue while
/// they are away. No heartbeat is sent for Alice, and `send_mail` must still
/// succeed where `send_text` would return `ContactOffline`.
#[tokio::test]
async fn mail_arrives_while_the_recipient_is_offline() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "pw", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", &relay).await.unwrap();
    let a_inv = alice.invite().await.unwrap();
    let b_inv = bob.invite().await.unwrap();
    bob.add_contact("alice", &a_inv).await.unwrap();
    alice.add_contact("bob", &b_inv).await.unwrap();

    // Alice never heartbeats: she is offline. Live chat would be refused here.
    assert!(
        bob.send_text("alice", "live").await.is_err(),
        "chat must still require presence"
    );

    let thread = bob
        .send_mail(
            "alice",
            None,
            Some("Q3 numbers"),
            "see attached",
            &[("q3.txt".to_string(), b"revenue".to_vec())],
        )
        .await
        .unwrap();

    let inbox = alice.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    assert_eq!(inbox.messages[0].from_nick, "bob");
    match &inbox.messages[0].payload {
        MessagePayload::Mail {
            thread_id,
            subject,
            body,
            attachments,
        } => {
            assert_eq!(thread_id, &thread);
            assert_eq!(subject.as_deref(), Some("Q3 numbers"));
            assert_eq!(body, "see attached");
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0].name, "q3.txt");
            assert_eq!(attachments[0].data, b"revenue");
        }
        other => panic!("expected mail payload, got {other:?}"),
    }
    // The received attachment is also stored as a decryptable vault blob.
    assert_eq!(inbox.messages[0].mail_attachments.len(), 1);
    let blob = alice
        .read_blob(&inbox.messages[0].mail_attachments[0].blob_id)
        .unwrap();
    assert_eq!(blob, b"revenue");
}

#[tokio::test]
async fn a_reply_lands_in_the_same_thread() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "pw", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", &relay).await.unwrap();
    let a_inv = alice.invite().await.unwrap();
    let b_inv = bob.invite().await.unwrap();
    bob.add_contact("alice", &a_inv).await.unwrap();
    alice.add_contact("bob", &b_inv).await.unwrap();

    let thread = bob
        .send_mail("alice", None, Some("Lunch?"), "noon?", &[])
        .await
        .unwrap();
    alice.recv().await.unwrap();

    // Alice replies with the same thread id; Bob replies again.
    alice
        .send_mail("bob", Some(&thread), Some("Re: Lunch?"), "works", &[])
        .await
        .unwrap();
    bob.recv().await.unwrap();

    let thread_ids = |records: &[hush_core::ChatRecord]| -> Vec<String> {
        records
            .iter()
            .filter_map(|r| match &r.payload {
                ChatRecordPayload::Mail { thread_id, .. } => Some(thread_id.clone()),
                _ => None,
            })
            .collect()
    };
    // Each side keeps its own sent copy plus the reply, all under one thread id.
    assert_eq!(
        thread_ids(&bob.mail_records()),
        vec![thread.clone(), thread.clone()]
    );
    assert_eq!(
        thread_ids(&alice.mail_records()),
        vec![thread.clone(), thread.clone()]
    );

    // Only mail shows up on the mail rail; a plain chat line does not.
    alice.heartbeat_presence().await.unwrap();
    bob.send_text("alice", "psst").await.unwrap();
    assert_eq!(bob.mail_records().len(), 2);
}

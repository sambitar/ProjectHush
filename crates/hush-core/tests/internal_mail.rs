use hush_core::{Account, ChatRecordPayload, MessagePayload, ShortInvite};
use hush_relay::{RelayConfig, router};
use tempfile::TempDir;
use tokio::time::{sleep, Duration};

async fn spawn_relay() -> (String, TempDir) {
    spawn_relay_on(0).await
}

/// Serve a relay on a specific port. `0` picks a free one.
async fn spawn_relay_on(port: u16) -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
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
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "pw", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", &relay).await.unwrap();

    alice.set_mail_handle("Sandy").await.unwrap();
    assert_eq!(alice.mail_handle(), Some("sandy"));
    // No vault name was set, so the address uses the default vault name.
    assert_eq!(alice.mail_address().as_deref(), Some("sandy@hush"));
    alice.set_vault_name("Quiet-Harbor").await.unwrap();
    assert_eq!(alice.mail_address().as_deref(), Some("sandy@quiet-harbor"));

    let invite = alice.invite().await.unwrap();
    // Pointer invites (hush1…) carry the handle and vault in the clear; the
    // prekey record they point at is what needs the relay.
    let decoded = ShortInvite::decode(&invite).unwrap();
    assert_eq!(decoded.handle.as_deref(), Some("sandy"));
    assert_eq!(decoded.vault.as_deref(), Some("quiet-harbor"));

    bob.add_contact("alice", &invite).await.unwrap();
    assert_eq!(bob.contacts()[0].mail_handle.as_deref(), Some("sandy"));
    assert_eq!(
        bob.contacts()[0].mail_vault.as_deref(),
        Some("quiet-harbor")
    );
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
        ShortInvite::decode(&after).unwrap().handle.as_deref(),
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

    let sent = bob
        .send_mail(
            "alice",
            None,
            Some("Q3 numbers"),
            "see attached",
            &[("q3.txt".to_string(), b"revenue".to_vec())],
        )
        .await
        .unwrap();
    assert!(!sent.queued, "a live relay must not leave mail in the Outbox");
    let thread = sent.thread_id;

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
        .unwrap()
        .thread_id;
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

/// The Outbox exists because the relay can be down. A send must survive that:
/// it is recorded as queued, and the next flush puts it on the wire.
///
/// Pointer invites need the relay to pair (the accept fetches the prekey
/// record), so the accounts meet while it is up and the down stretch is
/// simulated by pointing the vault at a dead address — the same seam the
/// relay settings use.
#[tokio::test]
async fn mail_queues_while_the_relay_is_down_and_goes_out_on_retry() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "pw", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", &relay).await.unwrap();
    let a_inv = alice.invite().await.unwrap();
    let b_inv = bob.invite().await.unwrap();
    bob.add_contact("alice", &a_inv).await.unwrap();
    alice.add_contact("bob", &b_inv).await.unwrap();

    // The relay "goes down": the vault's base points at a dead port.
    alice
        .set_relay_clearnet("http://127.0.0.1:1")
        .await
        .unwrap();
    let outcome = alice
        .send_mail("bob", None, Some("later"), "when the relay is back", &[])
        .await
        .unwrap();
    assert!(outcome.queued, "a dead relay must queue, not fail the send");
    assert_eq!(alice.queued_mail_count(), 1);
    assert_eq!(
        alice.mail_records().len(),
        1,
        "a queued message is recorded so the Outbox can show it"
    );
    assert_eq!(
        alice.retry_outbox().await.unwrap(),
        0,
        "a retry against a dead relay delivers nothing and stays queued"
    );
    assert_eq!(alice.queued_mail_count(), 1);

    // The relay "comes back"; the next flush delivers.
    alice.set_relay_clearnet(&relay).await.unwrap();
    assert_eq!(alice.retry_outbox().await.unwrap(), 1);
    assert_eq!(alice.queued_mail_count(), 0);

    let inbox = bob.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    match &inbox.messages[0].payload {
        MessagePayload::Mail { body, .. } => assert_eq!(body, "when the relay is back"),
        other => panic!("expected mail, got {other:?}"),
    }
}

/// Sending to somebody who is not a contact is not a delivery problem, so it
/// must fail loudly rather than sit in the Outbox forever.
#[tokio::test]
async fn mail_to_an_unknown_contact_fails_instead_of_queueing() {
    let (relay, _relay_dir) = spawn_relay().await;
    let dir = TempDir::new().unwrap();
    let mut alice = Account::create(dir.path(), "pw", &relay).await.unwrap();

    assert!(alice.send_mail("nobody", None, None, "hi", &[]).await.is_err());
    assert_eq!(alice.queued_mail_count(), 0);
    assert_eq!(alice.mail_records().len(), 0);
}

#[tokio::test]
async fn a_draft_is_saved_edited_and_deleted() {
    let (relay, _relay_dir) = spawn_relay().await;
    let dir = TempDir::new().unwrap();
    let mut alice = Account::create(dir.path(), "pw", &relay).await.unwrap();

    let id = alice
        .save_draft(None, None, "bob", Some("hi"), "first pass", &[])
        .await
        .unwrap();
    assert_eq!(alice.draft_records().len(), 1);

    // Saving with an id edits in place rather than piling up drafts.
    let same = alice
        .save_draft(Some(&id), None, "bob", Some("hi"), "second pass", &[])
        .await
        .unwrap();
    assert_eq!(same, id);
    assert_eq!(alice.draft_records().len(), 1);

    // Drafts survive a reload, which is the whole reason they are in the vault.
    let reopened = Account::open(dir.path(), "pw").await.unwrap();
    match &reopened.draft_records()[0].payload {
        ChatRecordPayload::Draft { body, .. } => assert_eq!(body, "second pass"),
        other => panic!("expected a draft, got {other:?}"),
    }
    assert!(
        reopened.chat_history().is_empty(),
        "a draft must not show up as chat history"
    );

    let mut alice = reopened;
    assert!(alice.delete_draft(&id).await.unwrap());
    assert!(alice.draft_records().is_empty());
}

/// A draft is user text, so it may not sit on disk in the clear.
#[tokio::test]
async fn a_draft_body_is_not_plaintext_on_disk() {
    let (relay, _relay_dir) = spawn_relay().await;
    let dir = TempDir::new().unwrap();
    let mut alice = Account::create(dir.path(), "pw", &relay).await.unwrap();
    alice
        .save_draft(None, None, "bob", Some("secret subject"), "swordfish body", &[])
        .await
        .unwrap();

    let raw = std::fs::read(dir.path().join("chat.enc")).unwrap();
    let haystack = String::from_utf8_lossy(&raw);
    assert!(!haystack.contains("swordfish body"));
    assert!(!haystack.contains("secret subject"));
}

//! Meetings: a local record fanned out over the members' 1:1 sessions.
//!
//! There is no relay-side room and no group object. These tests cover the three
//! things that can actually go wrong: the record surviving a restart, one
//! delivery per member rather than none or several, and a recipient that has
//! never seen the meeting materialising it from the descriptor that rides on
//! every message.

use hush_core::{Account, MessagePayload};
use hush_relay::{router, RelayConfig};
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

/// Create a meeting, send in it, and reopen the vault: the record and its log
/// have to come back, because they live in the same encrypted log as chat.
#[tokio::test]
async fn a_meeting_and_its_messages_survive_a_reopen() {
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
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();

    let meeting = alice
        .create_meeting("Standup", &["bob".to_string()])
        .await
        .unwrap();
    assert_eq!(meeting.name, "Standup");
    assert_eq!(meeting.members, vec!["bob".to_string()]);
    assert!(meeting.messages.is_empty());

    alice
        .send_meeting_message(&meeting.id, "morning all")
        .await
        .unwrap();

    // The sender's own view is in the vault, not only on the wire.
    let reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    let meetings = reopened.meetings();
    assert_eq!(meetings.len(), 1);
    assert_eq!(meetings[0].id, meeting.id);
    assert_eq!(meetings[0].name, "Standup");
    assert_eq!(meetings[0].members, vec!["bob".to_string()]);
    assert_eq!(meetings[0].messages.len(), 1);
    assert!(meetings[0].messages[0].outgoing);
    assert_eq!(meetings[0].messages[0].body, "morning all");
}

/// Fan-out: one message in a two-member meeting is two deliveries, one to each
/// member, and neither member sees the other's copy.
#[tokio::test]
async fn a_meeting_message_reaches_every_member_exactly_once() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let carol_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();

    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    let carol_invite = carol.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    carol.add_contact("alice", &alice_invite).await.unwrap();

    let meeting = alice
        .create_meeting("Trip", &["bob".to_string(), "carol".to_string()])
        .await
        .unwrap();

    alice
        .send_meeting_message(&meeting.id, "we leave at six")
        .await
        .unwrap();

    // Delivery is store-and-forward, so neither member has to be online.
    let bob_inbox = bob.recv().await.unwrap();
    assert_eq!(bob_inbox.messages.len(), 1, "bob gets exactly his one copy");
    assert_eq!(bob_inbox.messages[0].from_nick, "alice");
    match &bob_inbox.messages[0].payload {
        MessagePayload::Meeting {
            meeting_id,
            name,
            members,
            body,
        } => {
            assert_eq!(meeting_id, &meeting.id);
            assert_eq!(name, "Trip");
            assert_eq!(members, &vec!["bob".to_string(), "carol".to_string()]);
            assert_eq!(body, "we leave at six");
        }
        other => panic!("expected a meeting message, got {other:?}"),
    }

    let carol_inbox = carol.recv().await.unwrap();
    assert_eq!(carol_inbox.messages.len(), 1, "carol gets exactly her one copy");
    assert_eq!(carol_inbox.messages[0].from_nick, "alice");
    assert!(matches!(
        carol_inbox.messages[0].payload,
        MessagePayload::Meeting { .. }
    ));
}

/// A recipient that has never seen the meeting materialises it from the
/// descriptor on the first message, then appends the ones that follow.
#[tokio::test]
async fn a_recipient_materialises_an_unknown_meeting_from_the_descriptor() {
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
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();

    assert!(bob.meetings().is_empty(), "bob starts with no meetings");

    let meeting = alice
        .create_meeting("Book club", &["bob".to_string()])
        .await
        .unwrap();
    alice
        .send_meeting_message(&meeting.id, "chapter one")
        .await
        .unwrap();

    bob.recv().await.unwrap();

    let meetings = bob.meetings();
    assert_eq!(meetings.len(), 1, "the descriptor created the meeting");
    assert_eq!(meetings[0].id, meeting.id);
    assert_eq!(meetings[0].name, "Book club");
    assert_eq!(meetings[0].members, vec!["bob".to_string()]);
    assert_eq!(meetings[0].messages.len(), 1);
    assert!(!meetings[0].messages[0].outgoing);
    assert_eq!(meetings[0].messages[0].from.as_deref(), Some("alice"));
    assert_eq!(meetings[0].messages[0].body, "chapter one");

    // A second message for the same meeting appends rather than creating a
    // second record or replacing the local one.
    alice
        .send_meeting_message(&meeting.id, "chapter two")
        .await
        .unwrap();
    bob.recv().await.unwrap();

    let meetings = bob.meetings();
    assert_eq!(meetings.len(), 1, "the known meeting is not duplicated");
    assert_eq!(meetings[0].name, "Book club");
    assert_eq!(meetings[0].messages.len(), 2);
    assert_eq!(meetings[0].messages[1].body, "chapter two");

    // And it survives a restart, because it went into the same encrypted log.
    let reopened = Account::open(bob_dir.path(), "bob-pass").await.unwrap();
    assert_eq!(reopened.meetings().len(), 1);
    assert_eq!(reopened.meetings()[0].messages.len(), 2);
}

/// Membership changes are local: the creator can add and remove members, and the
/// change is in their vault, not pushed to anyone.
#[tokio::test]
async fn meeting_membership_changes_are_local_and_persisted() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let carol_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();

    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    let carol_invite = carol.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    carol.add_contact("alice", &alice_invite).await.unwrap();

    let meeting = alice
        .create_meeting("Planning", &["bob".to_string()])
        .await
        .unwrap();
    alice
        .add_meeting_member(&meeting.id, "carol")
        .await
        .unwrap();

    let reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert_eq!(
        reopened.meetings()[0].members,
        vec!["bob".to_string(), "carol".to_string()]
    );

    // An added member receives later messages; a removed one does not.
    alice
        .remove_meeting_member(&meeting.id, "bob")
        .await
        .unwrap();
    alice
        .send_meeting_message(&meeting.id, "carol only")
        .await
        .unwrap();

    let carol_inbox = carol.recv().await.unwrap();
    assert_eq!(carol_inbox.messages.len(), 1);
    assert!(matches!(
        carol_inbox.messages[0].payload,
        MessagePayload::Meeting { .. }
    ));
    assert!(
        bob.recv().await.unwrap().messages.is_empty(),
        "a removed member gets nothing"
    );
}

/// An unknown member is refused before anything is written, rather than a
/// meeting that can never deliver.
#[tokio::test]
async fn a_meeting_needs_a_name_and_known_members() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();

    assert!(
        alice.create_meeting("", &["nobody".to_string()]).await.is_err(),
        "a meeting needs a name"
    );
    assert!(
        alice.create_meeting("Standup", &[]).await.is_err(),
        "a meeting needs at least one member"
    );
    assert!(
        alice
            .create_meeting("Standup", &["ghost".to_string()])
            .await
            .is_err(),
        "a member has to be a contact"
    );
    assert!(alice.meetings().is_empty(), "nothing was written");
}

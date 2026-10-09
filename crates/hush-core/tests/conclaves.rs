//! Conclaves: a local record fanned out over the trustees' 1:1 sessions.
//!
//! There is no relay-side room and no group object. Membership is consensual:
//! creating a conclave and naming people sends invites, and a named person joins
//! only by accepting. A message never materialises a conclave on a vault that
//! has never seen it. The convener is the single authority for invites, kicks
//! and ending; any joined trustee may leave.

use hush_core::{Account, Error, MessagePayload};
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

/// Two accounts that know each other, ready to create or be invited to a
/// conclave. The vault dirs are returned so the caller can reopen them.
async fn two_contacts(
    relay: &str,
    alice_dir: &TempDir,
    bob_dir: &TempDir,
) -> (Account, Account) {
    let mut alice = Account::create(alice_dir.path(), "alice-pass", relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    (alice, bob)
}

/// An invite alone must not materialise a conclave: it becomes a pending record
/// the invitee explicitly accepts. Accepting then creates the record and joins
/// the trustee.
#[tokio::test]
async fn an_invite_is_pending_until_accepted_and_never_materialises_a_conclave() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();

    // The invite reaches bob as a control payload: it is not a chat message, and
    // it does not create a conclave.
    let inbox = bob.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "an invite is not a message");
    assert!(
        bob.conclaves().is_empty(),
        "an invite must not materialise a conclave"
    );
    let pending = bob.pending_invites();
    assert_eq!(pending.len(), 1, "the invite is stored as pending");
    assert_eq!(pending[0].conclave_id, conclave.id);
    assert_eq!(pending[0].name, "Standup");

    // Accepting is the only path that creates the record.
    let joined = bob.accept_conclave_invite(&conclave.id).await.unwrap();
    assert_eq!(joined.id, conclave.id);
    assert!(bob.pending_invites().is_empty(), "the pending invite is consumed");
    assert_eq!(bob.conclaves().len(), 1);
    assert_eq!(bob.conclaves()[0].name, "Standup");

    // The convener learns the trustee joined.
    alice.recv().await.unwrap();
    let record = &alice.conclaves()[0];
    assert_eq!(record.trustees.len(), 2);
    assert_eq!(record.joined().count(), 2, "bob is joined in alice's record");
}

/// A named invitee receives nothing until they accept: fan-out targets the
/// joined set only.
#[tokio::test]
async fn an_invitee_receives_no_messages_until_they_accept() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Book club", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap(); // consume the invite; do not accept

    alice
        .send_conclave_message(&conclave.id, "chapter one")
        .await
        .unwrap();
    let inbox = bob.recv().await.unwrap();
    assert!(
        inbox.messages.is_empty(),
        "an unaccepted invitee is not a fan-out target"
    );

    // Once accepted, the same fan-out reaches them.
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();
    alice
        .send_conclave_message(&conclave.id, "chapter two")
        .await
        .unwrap();
    let inbox = bob.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    match &inbox.messages[0].payload {
        MessagePayload::Conclave { body, .. } => assert_eq!(body, "chapter two"),
        other => panic!("expected a conclave message, got {other:?}"),
    }
}

/// A file shared in a conclave reaches every joined trustee as its own encrypted
/// copy and is appended to each recipient's conclave log; a named invitee who
/// has not accepted gets nothing. Files are per-recipient blobs, so this is N
/// copies for N trustees, and there is no backfill for a trustee who joins
/// later.
#[tokio::test]
async fn a_conclave_file_reaches_every_joined_trustee_and_not_an_invitee() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let carol_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;
    // Carol is a contact of alice's, invited but never accepting.
    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();
    let carol_invite = carol.invite().await.unwrap();
    let alice_invite = alice.invite().await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    carol.add_contact("alice", &alice_invite).await.unwrap();

    let conclave = alice
        .create_conclave("Trip", &["bob".to_string(), "carol".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap(); // invite
    carol.recv().await.unwrap(); // invite; not accepted
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    let blob = alice
        .send_conclave_file_bytes(&conclave.id, "notes.txt", b"hello bytes")
        .await
        .unwrap();
    assert_eq!(alice.read_blob(&blob).unwrap(), b"hello bytes");

    // The sender's own log records the file against its own blob.
    let sent = &alice.conclaves()[0].messages;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].file.as_ref().unwrap().name, "notes.txt");

    // Bob, joined, receives exactly one copy, with the bytes on the wire.
    let inbox = bob.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1, "bob gets exactly his one copy");
    match &inbox.messages[0].payload {
        MessagePayload::Conclave { file, body, .. } => {
            assert!(body.is_empty(), "a file message has no body");
            let f = file.as_ref().expect("a file attachment");
            assert_eq!(f.name, "notes.txt");
            assert_eq!(f.data, b"hello bytes");
        }
        other => panic!("expected a conclave file, got {other:?}"),
    }
    let bob_msg = &bob.conclaves()[0].messages[0];
    let bob_file = bob_msg.file.as_ref().expect("bob's log has the file");
    assert_eq!(bob_file.name, "notes.txt");
    assert_eq!(bob_file.size, 11);
    // The recipient stored the bytes as its own local blob: a 32-byte id, not a
    // reference into the sender's vault.
    assert_eq!(bob_file.blob_id_hex.len(), 64);

    // Carol, invited but not joined, receives nothing and has no conclave.
    let carol_inbox = carol.recv().await.unwrap();
    assert!(
        carol_inbox.messages.is_empty(),
        "an unaccepted invitee is not a fan-out target for files either"
    );
    assert!(carol.conclaves().is_empty());
}

/// A created conclave and its messages survive a reopen, and the convener is
/// recorded.
#[tokio::test]
async fn a_conclave_and_its_messages_survive_a_reopen() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();
    assert_eq!(conclave.name, "Standup");
    assert_eq!(conclave.joined().count(), 1, "only the convener is joined yet");
    assert_eq!(conclave.invited().count(), 1);
    assert!(conclave.messages.is_empty());
    let convener = conclave.convener.clone();
    assert!(!convener.is_empty());

    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    alice
        .send_conclave_message(&conclave.id, "morning all")
        .await
        .unwrap();

    let reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    let conclaves = reopened.conclaves();
    assert_eq!(conclaves.len(), 1);
    assert_eq!(conclaves[0].id, conclave.id);
    assert_eq!(conclaves[0].name, "Standup");
    assert_eq!(conclaves[0].convener, convener);
    assert_eq!(conclaves[0].messages.len(), 1);
    assert!(conclaves[0].messages[0].outgoing);
    assert_eq!(conclaves[0].messages[0].body, "morning all");
}

/// Fan-out: one message to a two-trustee conclave is exactly one delivery per
/// joined trustee, and the payload carries the trustee nicknames.
#[tokio::test]
async fn a_conclave_message_reaches_every_joined_trustee_exactly_once() {
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

    let conclave = alice
        .create_conclave("Trip", &["bob".to_string(), "carol".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    carol.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    carol.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();
    assert_eq!(
        alice.conclaves()[0].joined().count(),
        3,
        "alice should see both trustees joined: {:?}",
        alice.conclaves()[0].trustees
    );

    alice
        .send_conclave_message(&conclave.id, "we leave at six")
        .await
        .unwrap();

    let bob_inbox = bob.recv().await.unwrap();
    assert_eq!(bob_inbox.messages.len(), 1, "bob gets exactly his one copy");
    assert_eq!(bob_inbox.messages[0].from_nick, "alice");
    match &bob_inbox.messages[0].payload {
        MessagePayload::Conclave {
            conclave_id,
            name,
            trustees,
            body,
            ..
        } => {
            assert_eq!(conclave_id, &conclave.id);
            assert_eq!(name, "Trip");
            assert_eq!(trustees, &vec!["bob".to_string(), "carol".to_string()]);
            assert_eq!(body, "we leave at six");
        }
        other => panic!("expected a conclave message, got {other:?}"),
    }

    let carol_inbox = carol.recv().await.unwrap();
    assert_eq!(carol_inbox.messages.len(), 1, "carol gets exactly her one copy");
    assert!(matches!(
        carol_inbox.messages[0].payload,
        MessagePayload::Conclave { .. }
    ));
}

/// Declining removes the invited trustee from the convener's record and tells no
/// one else. The invitee never materialises a conclave.
#[tokio::test]
async fn declining_an_invite_drops_it_from_the_convener_only() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Planning", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.decline_conclave_invite(&conclave.id).await.unwrap();
    assert!(bob.pending_invites().is_empty());
    assert!(bob.conclaves().is_empty(), "declining materialises nothing");

    alice.recv().await.unwrap();
    let record = &alice.conclaves()[0];
    assert_eq!(record.invited().count(), 0, "the declined invite is dropped");
    assert_eq!(record.trustees.len(), 1, "only the convener remains");
}

/// Only the convener may invite or kick; a joined trustee is refused at the API
/// boundary, not merely by a hidden button.
#[tokio::test]
async fn only_the_convener_may_invite_or_kick() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Planning", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    assert!(matches!(
        bob.invite_conclave_trustee(&conclave.id, "alice").await,
        Err(Error::NotConclaveConvener)
    ));
    assert!(matches!(
        bob.kick_conclave_trustee(&conclave.id, "alice").await,
        Err(Error::NotConclaveConvener)
    ));
}

/// A kick is the convener's, converges to every joined trustee, and stops the
/// kicked trustee from receiving further messages.
#[tokio::test]
async fn a_kick_converges_and_cuts_off_delivery() {
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

    let conclave = alice
        .create_conclave("Trip", &["bob".to_string(), "carol".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    carol.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    carol.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    alice.kick_conclave_trustee(&conclave.id, "bob").await.unwrap();
    // The kick converges to the remaining joined trustee, who learns the new
    // roster.
    carol.recv().await.unwrap();
    let carol_record = &carol.conclaves()[0];
    assert_eq!(
        carol_record.trustee(&bob.protocol_name()).map(|t| t.state),
        Some(hush_core::TrusteeState::Kicked),
        "carol's record shows bob kicked"
    );

    alice
        .send_conclave_message(&conclave.id, "carol only")
        .await
        .unwrap();
    assert_eq!(carol.recv().await.unwrap().messages.len(), 1);
    assert!(
        bob.recv().await.unwrap().messages.is_empty(),
        "a kicked trustee gets nothing"
    );
}

/// Any joined trustee may leave, the convener converges the roster, and the
/// leaver stops receiving.
#[tokio::test]
async fn any_joined_trustee_may_leave() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Planning", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    bob.leave_conclave(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();
    let record = &alice.conclaves()[0];
    assert_eq!(record.joined().count(), 1, "only the convener remains joined");
    assert_eq!(
        record.trustee(&bob.protocol_name()).map(|t| t.state),
        Some(hush_core::TrusteeState::Left)
    );

    alice
        .send_conclave_message(&conclave.id, "anyone there?")
        .await
        .unwrap();
    assert!(
        bob.recv().await.unwrap().messages.is_empty(),
        "a departed trustee gets nothing"
    );
}

/// The convener leaving ends the conclave for everyone: the end is broadcast,
/// the record stays read-only on every peer, and nobody can send in it.
#[tokio::test]
async fn the_convener_leaving_ends_the_conclave_for_everyone() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Trip", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    alice
        .send_conclave_message(&conclave.id, "we leave at six")
        .await
        .unwrap();
    bob.recv().await.unwrap();

    alice.leave_conclave(&conclave.id).await.unwrap();

    // The record is kept but marked ended, locally and on the trustee.
    assert!(alice.conclaves()[0].ended);
    assert!(matches!(
        alice.send_conclave_message(&conclave.id, "still here?").await,
        Err(Error::ConclaveEnded)
    ));

    bob.recv().await.unwrap();
    assert!(bob.conclaves()[0].ended, "the end reached bob");
    assert_eq!(bob.conclaves()[0].messages.len(), 1, "history is kept");
    assert!(matches!(
        bob.send_conclave_message(&conclave.id, "hello?").await,
        Err(Error::ConclaveEnded)
    ));

    // A reopen keeps the ended record and its history.
    let reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert!(reopened.conclaves()[0].ended);
    assert_eq!(reopened.conclaves()[0].messages.len(), 1);
}

/// A conclave needs a name and at least one known contact to invite, and an
/// unknown contact is refused before anything is written.
#[tokio::test]
async fn a_conclave_needs_a_name_and_known_contacts() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();

    assert!(
        alice.create_conclave("", &["nobody".to_string()]).await.is_err(),
        "a conclave needs a name"
    );
    assert!(
        alice.create_conclave("Standup", &[]).await.is_err(),
        "a conclave needs at least one trustee"
    );
    assert!(
        alice
            .create_conclave("Standup", &["ghost".to_string()])
            .await
            .is_err(),
        "a trustee has to be a contact"
    );
    assert!(alice.conclaves().is_empty(), "nothing was written");
}

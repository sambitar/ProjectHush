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

/// One refused copy must not abandon the rest of the fan-out.
///
/// A conclave is N independent 1:1 puts with no relay-side room to re-read a
/// copy from. The old fan-out propagated the first error with `?`, so one dead
/// target silently cost every trustee after it and the sender was told nothing.
/// Revoking one trustee's queue makes exactly its put refused (403) while it
/// stays a contact, so the assertions are that the *other* trustee still gets
/// the message and that the sender keeps her own copy.
#[tokio::test]
async fn a_refused_trustee_does_not_abandon_the_rest_of_the_fan_out() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let carol_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let carol_invite = carol.invite().await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    carol.add_contact("alice", &alice_invite).await.unwrap();

    let conclave = alice
        .create_conclave("Trip", &["bob".to_string(), "carol".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    carol.recv().await.unwrap();
    carol.accept_conclave_invite(&conclave.id).await.unwrap();

    // Alice drains the accepts *and* the queue grants, so she holds bob's
    // per-contact queue and delivers into it rather than his inbox.
    alice.recv().await.unwrap();
    assert_eq!(
        alice.conclaves()[0].joined().count(),
        3,
        "all three are joined before the send"
    );

    // Bob cuts alice off: this burns the queue she was handed for him, so her
    // next put there is refused — while bob is still her contact.
    bob.revoke_contact("alice").await.unwrap();

    alice
        .send_conclave_message(&conclave.id, "we leave at six")
        .await
        .unwrap();

    // Carol, ordered after bob, still receives it: the refusal did not abort.
    let inbox = carol.recv().await.unwrap();
    let got = inbox.messages.iter().any(|m| {
        matches!(&m.payload, MessagePayload::Conclave { body, .. } if body == "we leave at six")
    });
    assert!(
        got,
        "carol must still get the message after bob's copy was refused"
    );

    // And the sender kept her own copy rather than losing it to the error.
    let logged = alice.conclaves()[0]
        .messages
        .iter()
        .any(|m| m.body == "we leave at six");
    assert!(logged, "the sender's own copy is still recorded");
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

/// Deleting an ended conclave destroys this vault's copy — record and message
/// log together — and nothing else. The other trustees keep their own copies,
/// the files shared in the conclave stay readable from the blob store, and
/// nothing a peer still sends can resurrect the deleted record.
#[tokio::test]
async fn deleting_an_ended_conclave_drops_its_history_but_keeps_the_files_and_the_peers_copy() {
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

    // A shared file, so deletion can be shown to keep the blob it pointed at.
    let blob = alice
        .send_conclave_file_bytes(&conclave.id, "notes.txt", b"trip notes")
        .await
        .unwrap();
    bob.recv().await.unwrap();
    assert_eq!(bob.conclaves()[0].messages.len(), 1, "bob holds the file message");

    // A conclave that has not ended cannot be deleted: ending or leaving is
    // how an active one concludes.
    assert!(matches!(
        alice.delete_conclave(&conclave.id).await,
        Err(Error::ConclaveNotEnded)
    ));

    // End it for everyone, then delete alice's copy.
    alice.leave_conclave(&conclave.id).await.unwrap();
    bob.recv().await.unwrap();
    assert!(bob.conclaves()[0].ended);

    assert!(alice.delete_conclave(&conclave.id).await.unwrap());
    assert!(
        alice.conclaves().is_empty(),
        "the record and its history leave alice's vault"
    );
    // An unknown id is a quiet no-op, matching delete-mail's idempotence.
    assert!(!alice.delete_conclave(&conclave.id).await.unwrap());

    // The file the deleted messages referenced is still in alice's vault.
    assert_eq!(
        alice.read_blob(&blob).unwrap(),
        b"trip notes",
        "the shared file outlives the conclave"
    );

    // Nothing was sent to bob: his copy, history and file all stay.
    assert_eq!(bob.conclaves().len(), 1);
    assert_eq!(bob.conclaves()[0].messages.len(), 1);
    assert_eq!(bob.conclaves()[0].messages[0].file.as_ref().unwrap().name, "notes.txt");
    assert_eq!(bob.read_blob(&blob).unwrap(), b"trip notes");

    // The deletion is written, so a reopen does not bring the record back.
    let reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert!(reopened.conclaves().is_empty());
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

// ---------------------------------------------------------------------------
// Paper — the conclave's shared collaborative document (Task 19).
//
// The document itself is a CRDT owned by each trustee's daemon; hush-core's
// job is transport: fan the descriptor and the opaque updates out to the
// joined trustees, and surface what arrives. A paper payload is not a chat
// message, so it must never enter the conclave's message log — the local
// document lives in the daemon's paper sidecar instead.
// ---------------------------------------------------------------------------

fn paper_for(conclave_id: &str, creator: &str) -> hush_core::PaperDescriptor {
    hush_core::PaperDescriptor {
        paper_id: "p-1".into(),
        conclave_id: conclave_id.into(),
        creator: creator.into(),
        hub: creator.into(),
        title: "Minutes".into(),
        created_ms: 1_760_000_000_000,
    }
}

/// The descriptor and every later update reach each joined trustee, bytes
/// intact. A yrs update is binary, so it is carried as bytes and not as text.
#[tokio::test]
async fn a_paper_descriptor_and_its_deltas_reach_every_joined_trustee() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let carol_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;
    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();
    let carol_invite = carol.invite().await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    carol.add_contact("alice", &alice.invite().await.unwrap()).await.unwrap();

    let conclave = alice
        .create_conclave("Trip", &["bob".to_string(), "carol".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    carol.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    carol.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();
    assert_eq!(alice.conclaves()[0].joined().count(), 3, "all three joined");

    let alice_id = alice.protocol_name().to_string();
    let descriptor = paper_for(&conclave.id, &alice_id);
    let update = vec![0u8, 1, 2, 255, 0];

    alice
        .send_conclave_paper_state(&conclave.id, descriptor.clone())
        .await
        .unwrap();
    alice
        .send_conclave_paper_delta(&conclave.id, &descriptor.paper_id, &update)
        .await
        .unwrap();

    for (who, inbox) in [("bob", bob.recv().await.unwrap()), ("carol", carol.recv().await.unwrap())]
    {
        assert_eq!(inbox.papers.len(), 2, "{who} gets the descriptor then the delta");
        assert_eq!(inbox.papers[0].from_nick, "alice");
        assert_eq!(inbox.papers[0].from_account, alice_id, "{who} sees the author");
        match &inbox.papers[0].payload {
            MessagePayload::PaperState { paper } => assert_eq!(paper, &descriptor),
            other => panic!("{who} expected a paper state, got {other:?}"),
        }
        match &inbox.papers[1].payload {
            MessagePayload::PaperDelta { paper_id, update: got } => {
                assert_eq!(paper_id, &descriptor.paper_id);
                assert_eq!(got, &update, "{who} gets the update bytes intact");
            }
            other => panic!("{who} expected a paper delta, got {other:?}"),
        }
        assert!(
            inbox.messages.is_empty(),
            "{who}: a paper payload is not a chat message"
        );
    }
    assert!(
        bob.conclaves()[0].messages.is_empty(),
        "the document does not enter the message log"
    );
}

/// A paper payload in an ended conclave is refused, exactly like a message:
/// `ConclaveEnd` makes the paper read-only on every peer.
#[tokio::test]
async fn paper_payloads_are_refused_once_the_conclave_has_ended() {
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

    let descriptor = paper_for(&conclave.id, alice.protocol_name());
    alice
        .send_conclave_paper_state(&conclave.id, descriptor.clone())
        .await
        .unwrap();

    alice.leave_conclave(&conclave.id).await.unwrap();
    assert!(alice.conclaves()[0].ended);

    assert!(matches!(
        alice
            .send_conclave_paper_delta(&conclave.id, &descriptor.paper_id, &[1, 2, 3])
            .await,
        Err(Error::ConclaveEnded)
    ));
    assert!(matches!(
        alice
            .send_conclave_paper_snapshot(&conclave.id, descriptor.clone(), &[1, 2, 3])
            .await,
        Err(Error::ConclaveEnded)
    ));
    // The end reaches the trustee, which refuses to send too.
    bob.recv().await.unwrap();
    assert!(bob.conclaves()[0].ended);
    assert!(matches!(
        bob.send_conclave_paper_delta(&conclave.id, &descriptor.paper_id, &[1, 2, 3])
            .await,
        Err(Error::ConclaveEnded)
    ));
}

/// A vault that has no such conclave cannot send into it — the same boundary
/// `send_conclave_message` enforces.
#[tokio::test]
async fn a_paper_payload_cannot_be_sent_into_an_unknown_conclave() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let (mut alice, _bob) = two_contacts(
        &relay,
        &alice_dir,
        &TempDir::new().unwrap(),
    )
    .await;

    let descriptor = paper_for("c-nobody", alice.protocol_name());
    assert!(matches!(
        alice
            .send_conclave_paper_state("c-nobody", descriptor.clone())
            .await,
        Err(Error::ConclaveNotFound)
    ));
    assert!(matches!(
        alice
            .send_conclave_paper_delta("c-nobody", &descriptor.paper_id, &[1])
            .await,
        Err(Error::ConclaveNotFound)
    ));
}

/// Hub succession is the convener's word and nobody else's.
///
/// The rule is enforced in `recv`, where the conclave record is in hand, so a
/// claim that is not authoritative is dropped before the daemon ever sees it —
/// there is no second copy of the roster for it to be judged against.
#[tokio::test]
async fn only_the_convener_may_re_home_a_paper() {
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

    let alice_id = alice.protocol_name().to_string();
    let bob_id = bob.protocol_name().to_string();

    // Bob is a joined trustee, so he can put a payload on the wire — which is
    // exactly what a peer running a modified client would do. He is not the
    // convener, so Alice must ignore the claim entirely.
    let mut claim = paper_for(&conclave.id, &alice_id);
    claim.hub = bob_id.clone();
    bob.send_paper_signal(
        &conclave.id,
        &alice_id,
        MessagePayload::PaperHub { paper: claim },
    )
    .await
    .unwrap();
    let inbox = alice.recv().await.unwrap();
    assert!(
        inbox.papers.is_empty(),
        "a re-home from a trustee who is not the convener is dropped"
    );

    // The convener's claim is the one that settles it.
    let mut claim = paper_for(&conclave.id, &alice_id);
    claim.hub = bob_id.clone();
    alice
        .send_conclave_paper_hub(&conclave.id, claim.clone())
        .await
        .unwrap();
    let inbox = bob.recv().await.unwrap();
    assert_eq!(inbox.papers.len(), 1, "the convener's re-home is delivered");
    match &inbox.papers[0].payload {
        MessagePayload::PaperHub { paper } => assert_eq!(paper, &claim),
        other => panic!("expected a hub claim, got {other:?}"),
    }
}

/// The default succession rule: a paper whose hub leaves is inherited by the
/// conclave's convener, and by nobody else.
///
/// This is what runs on a kick or a leave with no user action: `recv` reports
/// the departure, the daemon asks the core to inherit, and only the convener
/// comes back with a descriptor to apply.
#[tokio::test]
async fn the_convener_inherits_a_paper_whose_hub_left() {
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

    let alice_id = alice.protocol_name().to_string();
    let bob_id = bob.protocol_name().to_string();
    let mut paper = paper_for(&conclave.id, &alice_id);
    paper.hub = bob_id.clone();

    // Bob is a joined trustee but not the convener, so the departure is not his
    // to act on: he inherits nothing and says so by returning `None` rather
    // than failing, because every daemon tries this.
    let mut claim = paper.clone();
    claim.hub = alice_id.clone();
    assert!(
        bob.inherit_conclave_paper_hub(&conclave.id, &alice_id, claim.clone())
            .await
            .unwrap()
            .is_none(),
        "a trustee who is not the convener inherits nothing"
    );

    // The claim only applies to the hub that actually left.
    assert!(
        alice
            .inherit_conclave_paper_hub(&conclave.id, "c-nobody", paper.clone())
            .await
            .unwrap()
            .is_none(),
        "a paper whose hub is still here is not re-homed"
    );

    // Bob leaves. Alice is the convener, so she takes the rendezvous.
    bob.leave_conclave(&conclave.id).await.unwrap();
    let inbox = alice.recv().await.unwrap();
    assert!(
        inbox.departed.contains(&(conclave.id.clone(), bob_id.clone())),
        "the departure is reported, which is what triggers succession: {:?}",
        inbox.departed
    );

    let moved = alice
        .inherit_conclave_paper_hub(&conclave.id, &bob_id, paper)
        .await
        .unwrap()
        .expect("the convener inherits the hub");
    assert_eq!(moved.hub, alice_id);
    assert_eq!(moved.paper_id, "p-1", "nothing else about the paper moves");
}

/// The convener's own kick moves the roster without a `recv` to notice it, so
/// the check is "is the hub still joined?" rather than "did I just see them
/// leave" — and the same rule refuses to move a paper whose hub is still here.
#[tokio::test]
async fn kicking_the_hub_moves_the_rendezvous_to_the_convener() {
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

    let alice_id = alice.protocol_name().to_string();
    let bob_id = bob.protocol_name().to_string();
    let mut paper = paper_for(&conclave.id, &bob_id);
    paper.hub = bob_id.clone();

    // Bob is the hub and still joined, so naming him must not move the paper.
    assert!(
        alice
            .inherit_conclave_paper_hub(&conclave.id, &bob_id, paper.clone())
            .await
            .unwrap()
            .is_none(),
        "a hub that is still a joined trustee keeps the rendezvous"
    );

    // The convener kicks the hub. There is no departure to observe — the roster
    // is already updated — which is exactly why the rule is checked against the
    // roster and not against an event.
    alice.kick_conclave_trustee(&conclave.id, "bob").await.unwrap();
    let roster = alice.conclaves();
    let joined = roster
        .iter()
        .find(|c| c.id == conclave.id)
        .expect("the conclave is still there");
    assert!(
        joined.joined().all(|t| t.account_id != bob_id),
        "the kick removed bob from the joined set"
    );

    let moved = alice
        .inherit_conclave_paper_hub(&conclave.id, &bob_id, paper)
        .await
        .unwrap()
        .expect("the convener inherits a hub it just kicked");
    assert_eq!(moved.hub, alice_id);

    // And the kick is not a silent one: the vault that was removed is told, so
    // it stops believing it is still a member. `broadcast_update` goes to the
    // joined set, which no longer contains it — this is the one address that is
    // not on that list, and the one that matters most.
    bob.recv().await.unwrap();
    let roster = bob.conclaves();
    let seen = roster
        .iter()
        .find(|c| c.id == conclave.id)
        .and_then(|c| c.trustee(&bob_id).map(|t| t.state.clone()));
    assert_eq!(
        seen,
        Some(hush_core::TrusteeState::Kicked),
        "the kicked vault learns it was kicked"
    );
}

/// A trustee who is not the convener cannot make the claim at all.
#[tokio::test]
async fn a_trustee_cannot_re_home_a_paper() {    let (relay, _relay_dir) = spawn_relay().await;
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

    let mut claim = paper_for(&conclave.id, alice.protocol_name());
    claim.hub = bob.protocol_name().to_string();
    assert!(matches!(
        bob.send_conclave_paper_hub(&conclave.id, claim).await,
        Err(Error::NotConclaveConvener)
    ));
}

// --- Voice (Task 23) ---

fn voice_session(conclave_id: &str, opener: &str, session_id: &str) -> hush_core::VoiceDescriptor {
    hush_core::VoiceDescriptor {
        session_id: session_id.into(),
        conclave_id: conclave_id.into(),
        opener: opener.into(),
        started_ms: 1_000,
    }
}

/// The authority rule for opening, checked where the roster is in hand: the
/// sender must be the opener the descriptor names *and* a joined trustee. A
/// voice room is announced, not asked for — joining is the consent — so a
/// forged open must not even surface as room state on the peers.
#[tokio::test]
async fn a_voice_open_reaches_a_joined_trustee_as_voice_not_chat() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    alice
        .send_conclave_voice_open(
            &conclave.id,
            voice_session(&conclave.id, &alice.protocol_name(), "v-1"),
        )
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "a voice open is not a chat message");
    assert_eq!(inbox.voices.len(), 1, "the open is routed as voice");
    match &inbox.voices[0].payload {
        MessagePayload::VoiceOpen { session } => {
            assert_eq!(session.session_id, "v-1");
            assert_eq!(session.opener, alice.protocol_name());
        }
        other => panic!("expected a voice open, got {other:?}"),
    }
    // Consumed on delivery: the next poll does not see it again.
    let again = bob.recv().await.unwrap();
    assert!(again.voices.is_empty());
}

/// The open's opener must be the sender: a trustee announcing a room in
/// someone else's name is dropped and consumed, never surfaced.
#[tokio::test]
async fn a_voice_open_forging_the_opener_is_dropped() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    // Alice names *bob* as the opener of a room she sends.
    alice
        .send_conclave_voice_open(
            &conclave.id,
            voice_session(&conclave.id, &bob.protocol_name(), "v-1"),
        )
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.voices.is_empty(), "a forged open is not room state");
    assert!(inbox.messages.is_empty());
}

/// You can only mute or leave as yourself: a `who` that is not the sender is
/// dropped at the same place the roster is in hand.
#[tokio::test]
async fn a_voice_mute_for_someone_else_is_dropped() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();

    alice
        .send_conclave_voice_mute(&conclave.id, "v-1", &bob.protocol_name(), true)
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.voices.is_empty(), "a mute in someone else's name is dropped");
    assert!(inbox.messages.is_empty());
}

/// Voice follows the conclave's lifecycle: once it has ended, sending is
/// refused exactly as a conclave message is.
#[tokio::test]
async fn voice_senders_refuse_once_the_conclave_has_ended() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_contacts(&relay, &alice_dir, &bob_dir).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();
    alice.end_conclave(&conclave.id).await.unwrap();
    bob.recv().await.unwrap();

    assert!(matches!(
        alice
            .send_conclave_voice_open(
                &conclave.id,
                voice_session(&conclave.id, &alice.protocol_name(), "v-1")
            )
            .await,
        Err(Error::ConclaveEnded)
    ));
}

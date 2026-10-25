//! 1:1 contact requests over a real relay.
//!
//! A requester holds the peer's invite and sends a `ContactRequest` as the
//! first message on a fresh session. The peer must see a pending row, not a
//! contact, and must be able to accept or decline. Accepting is what creates
//! the contact on both sides and starts the normal queue-grant exchange.

use hush_core::Account;
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

async fn pair() -> (Account, Account, TempDir, TempDir, TempDir) {
    let (relay, relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();
    (alice, bob, alice_dir, bob_dir, relay_dir)
}

#[tokio::test]
async fn a_request_is_pending_until_the_other_side_accepts() {
    let (mut alice, mut bob, _alice_dir, _bob_dir, _relay_dir) = pair().await;
    let bob_invite = bob.invite().await.unwrap();

    alice
        .request_contact("bob", &bob_invite, Some("the relay test asks"))
        .await
        .unwrap();
    assert!(
        alice.contacts().is_empty(),
        "a request must not create a contact"
    );
    assert_eq!(alice.outgoing_contact_requests().len(), 1);

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "a request is not chat");
    assert!(bob.contacts().is_empty());
    let pending = bob.incoming_contact_requests();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].account_id, alice.protocol_name());
    assert_eq!(pending[0].note.as_deref(), Some("the relay test asks"));

    let nick = bob
        .accept_contact_request(alice.protocol_name(), Some("alice"))
        .await
        .unwrap();
    assert_eq!(nick, "alice");
    assert!(bob.incoming_contact_requests().is_empty());
    assert!(bob
        .contacts()
        .iter()
        .any(|c| c.account_id == alice.protocol_name()));

    let inbox = alice.recv().await.unwrap();
    assert!(inbox.messages.is_empty());
    assert!(
        alice.outgoing_contact_requests().is_empty(),
        "accepting clears the outgoing row"
    );
    assert!(
        alice
            .contacts()
            .iter()
            .any(|c| c.account_id == bob.protocol_name()),
        "the accept makes bob a contact"
    );
}

#[tokio::test]
async fn a_declined_request_creates_no_contact_and_can_be_asked_again() {
    let (mut alice, mut bob, _alice_dir, _bob_dir, _relay_dir) = pair().await;
    let bob_invite = bob.invite().await.unwrap();

    alice
        .request_contact("bob", &bob_invite, None)
        .await
        .unwrap();
    bob.recv().await.unwrap();
    assert_eq!(bob.incoming_contact_requests().len(), 1);

    bob.decline_contact_request(alice.protocol_name())
        .await
        .unwrap();
    assert!(bob.incoming_contact_requests().is_empty());
    assert!(bob.contacts().is_empty());

    let inbox = alice.recv().await.unwrap();
    assert!(inbox.messages.is_empty());
    assert!(alice.outgoing_contact_requests().is_empty());
    assert!(
        alice.contacts().is_empty(),
        "a decline must not create a contact"
    );

    // Consent can change: the same account may ask again, and the request rides
    // the existing session instead of opening a second one.
    alice
        .request_contact("bob", &bob_invite, Some("second try"))
        .await
        .unwrap();
    bob.recv().await.unwrap();
    let pending = bob.incoming_contact_requests();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].note.as_deref(), Some("second try"));

    bob.accept_contact_request(alice.protocol_name(), None)
        .await
        .unwrap();
    alice.recv().await.unwrap();
    assert!(alice
        .contacts()
        .iter()
        .any(|c| c.account_id == bob.protocol_name()));
}

#[tokio::test]
async fn a_retry_to_a_pending_peer_re_sends_as_a_fresh_prekey_message() {
    let (mut alice, mut bob, _alice_dir, _bob_dir, _relay_dir) = pair().await;
    let bob_invite = bob.invite().await.unwrap();

    alice
        .request_contact("bob", &bob_invite, Some("first"))
        .await
        .unwrap();
    // The first request is never polled, so bob holds no session for alice.
    // Asking again must not be refused: the retry has to arrive under the
    // sender's own identity, which only a fresh prekey message can do.
    alice
        .request_contact("bob", &bob_invite, Some("second try"))
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.warnings.is_empty(), "{:?}", inbox.warnings);
    let pending = bob.incoming_contact_requests();
    assert_eq!(pending.len(), 1, "a retry is one request, not two");
    assert_eq!(pending[0].account_id, alice.protocol_name());
    assert_eq!(pending[0].note.as_deref(), Some("second try"));
}

#[tokio::test]
async fn requesting_an_existing_contact_is_refused() {
    let (mut alice, mut bob, _alice_dir, _bob_dir, _relay_dir) = pair().await;
    let bob_invite = bob.invite().await.unwrap();
    alice
        .request_contact("bob", &bob_invite, None)
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_contact_request(alice.protocol_name(), Some("alice"))
        .await
        .unwrap();
    alice.recv().await.unwrap();

    let again = alice.request_contact("bob", &bob_invite, None).await;
    assert!(again.is_err(), "an existing contact cannot be requested");
}

#[tokio::test]
async fn two_requests_crossing_on_the_wire_converge() {
    let (mut alice, mut bob, _alice_dir, _bob_dir, _relay_dir) = pair().await;
    let a_inv = alice.invite().await.unwrap();
    let b_inv = bob.invite().await.unwrap();

    // Both sides ask at the same time. Each request is a prekey message on a
    // session the other side also opened; libsignal keeps both attributable, so
    // the result must be a pending request on each side, not a lost pair.
    alice
        .request_contact("bob", &b_inv, Some("a"))
        .await
        .unwrap();
    bob.request_contact("alice", &a_inv, Some("b"))
        .await
        .unwrap();
    let alice_inbox = alice.recv().await.unwrap();
    let bob_inbox = bob.recv().await.unwrap();
    assert!(
        alice_inbox.warnings.is_empty(),
        "{:?}",
        alice_inbox.warnings
    );
    assert!(bob_inbox.warnings.is_empty(), "{:?}", bob_inbox.warnings);
    assert_eq!(alice.incoming_contact_requests().len(), 1);
    assert_eq!(bob.incoming_contact_requests().len(), 1);
    assert!(alice.contacts().is_empty());
    assert!(bob.contacts().is_empty());

    let aid = alice.incoming_contact_requests()[0].account_id.clone();
    let bid = bob.incoming_contact_requests()[0].account_id.clone();
    alice
        .accept_contact_request(&aid, Some("bob"))
        .await
        .unwrap();
    bob.accept_contact_request(&bid, Some("alice"))
        .await
        .unwrap();
    let alice_inbox = alice.recv().await.unwrap();
    let bob_inbox = bob.recv().await.unwrap();
    assert!(
        alice_inbox.warnings.is_empty(),
        "{:?}",
        alice_inbox.warnings
    );
    assert!(bob_inbox.warnings.is_empty(), "{:?}", bob_inbox.warnings);
    assert_eq!(alice.contacts().len(), 1);
    assert_eq!(bob.contacts().len(), 1);
    assert_eq!(alice.contacts()[0].nickname, "bob");
    assert_eq!(bob.contacts()[0].nickname, "alice");
}

#[tokio::test]
async fn a_strangers_request_survives_an_unlock_with_an_empty_identity_store() {
    // The identity store is trust-on-first-use and is rebuilt from disk on every
    // unlock. With nothing pinned, a stranger's first prekey message used to be
    // decrypted under whichever contact was tried first, refused there as
    // someone else's card, and consumed — so the request vanished without a
    // trace and the sender was told it was already pending.
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

    // Bob has exactly one contact, so exactly one address the stranger could be
    // mis-attributed to.
    let bob_invite = bob.invite().await.unwrap();
    carol
        .request_contact("bob", &bob_invite, Some("carol here"))
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_contact_request(carol.protocol_name(), Some("carol"))
        .await
        .unwrap();
    carol.recv().await.unwrap();
    assert_eq!(bob.contacts().len(), 1);

    // A fresh unlock: the store comes back from `device.enc` alone.
    drop(bob);
    let mut bob = Account::open(bob_dir.path(), "bob-pass").await.unwrap();

    alice
        .request_contact("bob", &bob_invite, Some("first time"))
        .await
        .unwrap();
    let inbox = bob.recv().await.unwrap();
    assert!(inbox.warnings.is_empty(), "{:?}", inbox.warnings);
    let pending = bob.incoming_contact_requests();
    assert_eq!(pending.len(), 1, "the request must not be swallowed");
    assert_eq!(pending[0].account_id, alice.protocol_name());
    assert_eq!(pending[0].note.as_deref(), Some("first time"));

    // The existing contact still reaches bob under its own identity, so the
    // stranger did not take over carol's address.
    bob.heartbeat_presence().await.unwrap();
    carol.send_text("bob", "still me").await.unwrap();
    let inbox = bob.recv().await.unwrap();
    assert!(inbox.warnings.is_empty(), "{:?}", inbox.warnings);
    assert_eq!(inbox.messages.len(), 1);
    assert_eq!(inbox.messages[0].from_nick, "carol");
}

//! Trustee reach (Task 24): a trust is either mail only or full.
//!
//! The reach is a per-relationship choice, one on each side, and a surface is
//! usable only when both sides allow it — so the effective reach is the narrower
//! of the two. These tests pin the three places that matters: the send
//! boundary, the receive boundary, and the announcement that carries the choice
//! across the wire.

mod presence_helper;

use hush_core::{Account, Error, MessagePayload, TrusteeReach};
use hush_relay::{RelayConfig, router};
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

/// Two accounts that are each other's contacts, with fresh vaults.
async fn pair(relay: &str) -> (Account, Account, TempDir, TempDir) {
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "alice-pass", relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", relay)
        .await
        .unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    (alice, bob, alice_dir, bob_dir)
}

/// Mail only removes the live surfaces and nothing else: chat is refused at the
/// boundary, and mail — the floor the relationship is named for — still flows.
#[tokio::test]
async fn mail_only_refuses_chat_but_not_mail() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();
    assert_eq!(alice.contacts()[0].reach, TrusteeReach::MailOnly);

    recipient_online(&bob).await;
    assert!(
        matches!(
            alice.send_text("bob", "hi").await,
            Err(Error::ReachMailOnly)
        ),
        "a mail-only trust has no chat"
    );

    // Mail is unaffected: it rides the store-and-forward path, not the live one.
    alice
        .send_mail("bob", None, Some("Hello"), "by post", &[])
        .await
        .unwrap();
    let inbox = bob.recv().await.unwrap();
    assert!(
        inbox
            .messages
            .iter()
            .any(|m| matches!(m.payload, MessagePayload::Mail { .. })),
        "mail must still arrive for a mail-only trust"
    );
}

/// The choice is announced, so the peer narrows too: after Bob drains the
/// announcement, his own chat with Alice is refused by the intersection.
#[tokio::test]
async fn reach_announcement_narrows_the_peer() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();

    // Bob drains the announcement. It is the only thing in his queue, because
    // Alice's `add_contact` grant already went out before the reach change.
    let inbox = bob.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "a reach change is not a message");
    assert_eq!(
        bob.contacts()[0].peer_reach,
        TrusteeReach::MailOnly,
        "the announcement lands as the peer's announced reach"
    );

    recipient_online(&alice).await;
    assert!(
        matches!(
            bob.send_text("alice", "hi").await,
            Err(Error::ReachMailOnly)
        ),
        "either side narrowing makes the relationship mail-only"
    );
}

/// A peer that has not yet learned the reach can still send a live message. It
/// is refused at the receiver's boundary rather than filed, and the refusal is
/// reported so the recipient knows why nothing arrived.
#[tokio::test]
async fn inbound_chat_from_a_mail_only_peer_is_refused() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();

    // Bob has not drained the announcement, so his send still goes out.
    recipient_online(&alice).await;
    bob.send_text("alice", "psst").await.unwrap();

    let inbox = alice.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "the chat must not be filed");
    assert!(
        inbox.warnings.iter().any(|w| w.contains("mail-only")),
        "the refusal must be reported, got {:?}",
        inbox.warnings
    );
}

/// A conclave invite from a mail-only peer is refused before it becomes a
/// pending invite, so the peer cannot pull the relationship into a surface it
/// excluded.
#[tokio::test]
async fn conclave_invite_from_a_mail_only_peer_is_refused() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();

    // Bob does not know yet, so he can still create the conclave and invite.
    bob.create_conclave("Book club", &["alice".to_string()])
        .await
        .unwrap();

    let inbox = alice.recv().await.unwrap();
    assert!(
        alice.pending_invites().is_empty(),
        "a mail-only relationship has no conclaves"
    );
    assert!(
        inbox.warnings.iter().any(|w| w.contains("mail-only")),
        "the refusal must be reported, got {:?}",
        inbox.warnings
    );
}

/// The creator's boundary: a conclave cannot be created with a mail-only
/// trustee, nor can one be invited to an existing conclave. Widening back to
/// full re-opens both.
#[tokio::test]
async fn conclaves_refuse_a_mail_only_trustee() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _bob, _a, _b) = pair(&relay).await;

    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();
    assert!(
        matches!(
            alice
                .create_conclave("Standup", &["bob".to_string()])
                .await,
            Err(Error::ReachMailOnly)
        ),
        "a mail-only trust cannot be a conclave trustee"
    );

    // Widen back: the conclave is allowed again, and the invite path is too.
    alice
        .set_contact_reach("bob", TrusteeReach::Full)
        .await
        .unwrap();
    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();

    let carol_dir = TempDir::new().unwrap();
    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();
    let carol_invite = carol.invite().await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    alice
        .set_contact_reach("carol", TrusteeReach::MailOnly)
        .await
        .unwrap();
    assert!(
        matches!(
            alice
                .invite_conclave_trustee(&conclave.id, "carol")
                .await,
            Err(Error::ReachMailOnly)
        ),
        "a mail-only trust cannot be invited to a conclave"
    );
}

/// The choice survives a reopen, so a restart cannot silently widen a
/// relationship the user narrowed.
#[tokio::test]
async fn reach_survives_reopen() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _bob, alice_dir, _b) = pair(&relay).await;
    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();
    drop(alice);

    let mut reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert_eq!(reopened.contacts()[0].reach, TrusteeReach::MailOnly);
    // The gate reads the persisted choice, so a restart does not re-open chat.
    // The reach check runs before the presence check, so no heartbeat is needed.
    assert!(matches!(
        reopened.send_text("bob", "hi").await,
        Err(Error::ReachMailOnly)
    ));
}

/// Narrowing a relationship does not retroactively remove a trustee from a
/// conclave they are already in: the fan-out still reaches them, and only a
/// *new* join is refused. A silent drop from the fan-out would be worse than the
/// inconsistency, and a kick is the removal path.
#[tokio::test]
async fn narrowing_does_not_evict_an_existing_conclave_trustee() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    let conclave = alice
        .create_conclave("Standup", &["bob".to_string()])
        .await
        .unwrap();
    bob.recv().await.unwrap();
    bob.accept_conclave_invite(&conclave.id).await.unwrap();
    alice.recv().await.unwrap();
    assert_eq!(alice.conclaves()[0].joined().count(), 2);

    // Narrow now, with bob already joined.
    alice
        .set_contact_reach("bob", TrusteeReach::MailOnly)
        .await
        .unwrap();

    // The fan-out still reaches the joined set...
    alice
        .send_conclave_message(&conclave.id, "still here")
        .await
        .expect("an existing membership is not revoked by a reach change");
    let inbox = bob.recv().await.unwrap();
    assert!(
        inbox
            .messages
            .iter()
            .any(|m| matches!(m.payload, MessagePayload::Conclave { .. })),
        "a grandfathered trustee still receives the conclave"
    );

    // ...but a fresh join is refused, which is the rule the change did apply.
    let carol_dir = TempDir::new().unwrap();
    let mut carol = Account::create(carol_dir.path(), "carol-pass", &relay)
        .await
        .unwrap();
    let carol_invite = carol.invite().await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();
    alice
        .set_contact_reach("carol", TrusteeReach::MailOnly)
        .await
        .unwrap();
    assert!(matches!(
        alice
            .invite_conclave_trustee(&conclave.id, "carol")
            .await,
        Err(Error::ReachMailOnly)
    ));
}

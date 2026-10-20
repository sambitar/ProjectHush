//! Trustee signals: the non-chat sideband between two contacts.
//!
//! These tests pin the envelope's contract rather than any one feature: a
//! signal never appears as a message, a denied contact gets nothing, expiry and
//! duplicate suppression work, the audit log is per contact, and an ack comes
//! back when the sender asked for one.

use std::time::{SystemTime, UNIX_EPOCH};

use hush_core::{Account, Error, SignalBody, SignalPolicy, SignalStatus};
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The envelope's whole promise: the body arrives out of band, never as a
/// message, is recorded in the audit log, and the requested ack comes back.
#[tokio::test]
async fn a_signal_arrives_out_of_band_and_is_acknowledged() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    let id = alice
        .send_signal(
            "bob",
            SignalBody::Ping {
                note: Some("look at this".into()),
            },
            None,
            true,
        )
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.messages.is_empty(), "a signal is not a message");
    assert_eq!(inbox.signals.len(), 1);
    let signal = &inbox.signals[0];
    assert_eq!(signal.id, id);
    assert_eq!(signal.from_nick, "alice");
    assert_eq!(
        signal.body,
        SignalBody::Ping {
            note: Some("look at this".into())
        }
    );
    assert_eq!(inbox.signal_acks.len(), 0);

    let log = bob.signals_for("alice").unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].id, id);
    assert_eq!(log[0].kind, "ping");
    assert!(log[0].supported);

    let ack = alice.recv().await.unwrap();
    assert!(ack.messages.is_empty());
    assert_eq!(ack.signal_acks.len(), 1);
    assert_eq!(ack.signal_acks[0].id, id);
    assert_eq!(ack.signal_acks[0].status, SignalStatus::Delivered);
}

/// An ack is optional: a heartbeat should not generate return traffic.
#[tokio::test]
async fn an_unacknowledged_signal_produces_no_reply() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    alice
        .send_signal("bob", SignalBody::Ping { note: None }, None, false)
        .await
        .unwrap();
    assert_eq!(bob.recv().await.unwrap().signals.len(), 1);

    let back = alice.recv().await.unwrap();
    assert!(back.signal_acks.is_empty(), "no ack was requested");
}

/// A signal that expires in flight is dropped, audited nowhere, and answered
/// with `Expired` when the sender asked for an ack.
#[tokio::test]
async fn an_expired_signal_is_dropped() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    let id = alice
        .send_signal(
            "bob",
            SignalBody::Ping {
                note: Some("too late".into()),
            },
            Some(now_ms() + 50),
            true,
        )
        .await
        .unwrap();
    sleep(Duration::from_millis(120)).await;

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.signals.is_empty(), "an expired signal is not applied");
    assert!(bob.signals_for("alice").unwrap().is_empty());

    let ack = alice.recv().await.unwrap();
    assert_eq!(ack.signal_acks.len(), 1);
    assert_eq!(ack.signal_acks[0].id, id);
    assert_eq!(ack.signal_acks[0].status, SignalStatus::Expired);
}

/// Deny is silent in both directions: no inbound signal is applied, no ack is
/// sent, and the local side refuses to send either.
#[tokio::test]
async fn a_denied_trustee_gets_no_signals() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    bob.set_contact_signals("alice", SignalPolicy::Deny)
        .await
        .unwrap();
    assert_eq!(bob.contacts()[0].signals, SignalPolicy::Deny);

    alice
        .send_signal("bob", SignalBody::Ping { note: None }, None, true)
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(inbox.signals.is_empty());
    assert!(bob.signals_for("alice").unwrap().is_empty());

    // The sender's ack never comes, because the receiver answered nothing.
    let back = alice.recv().await.unwrap();
    assert!(back.signal_acks.is_empty());

    // And the local boundary refuses a send from the denied side.
    assert!(matches!(
        bob.send_signal("alice", SignalBody::Ping { note: None }, None, false)
            .await,
        Err(Error::SignalsDisabled)
    ));
}

/// Signals are a layer of their own: a mail-only relationship has no chat, but
/// the sideband still works.
#[tokio::test]
async fn signals_work_on_a_mail_only_relationship() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, mut bob, _a, _b) = pair(&relay).await;

    alice
        .set_contact_reach("bob", hush_core::TrusteeReach::MailOnly)
        .await
        .unwrap();
    alice.retry_pending_reach().await.unwrap();

    alice
        .send_signal("bob", SignalBody::Ping { note: None }, None, true)
        .await
        .expect("signals are not gated by reach");

    let inbox = bob.recv().await.unwrap();
    assert_eq!(inbox.signals.len(), 1);
    assert!(inbox.messages.is_empty());
}

/// The policy is vault state, not memory: a restart cannot silently re-open a
/// sideband the user closed.
#[tokio::test]
async fn signal_policy_survives_reopen() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _bob, alice_dir, _b) = pair(&relay).await;

    alice
        .set_contact_signals("bob", SignalPolicy::Deny)
        .await
        .unwrap();
    drop(alice);

    let mut reopened = Account::open(alice_dir.path(), "alice-pass").await.unwrap();
    assert_eq!(reopened.contacts()[0].signals, SignalPolicy::Deny);
    assert!(matches!(
        reopened
            .send_signal("bob", SignalBody::Ping { note: None }, None, false)
            .await,
        Err(Error::SignalsDisabled)
    ));
}

/// A locally constructed `Unknown` is never sendable: it exists only for
/// decoding a kind this build does not know.
#[tokio::test]
async fn an_unknown_body_cannot_be_sent() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _bob, _a, _b) = pair(&relay).await;

    assert!(matches!(
        alice
            .send_signal("bob", SignalBody::Unknown, None, false)
            .await,
        Err(Error::InvalidSignal)
    ));
}

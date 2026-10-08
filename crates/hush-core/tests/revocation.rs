//! Revocation: deleting a contact burns the queue that contact delivers into,
//! and only that one.
//!
//! The design is one queue per contact: this account mints a queue for each
//! contact and reads it with a credential only it holds. Deleting a contact
//! burns its queue, which stops that contact delivering without touching any
//! other contact's queue.
//!
//! The first test below is the one that fails if the burn is removed: the
//! revoked contact's send must be rejected, while another contact still
//! delivers. The second covers the blocklist that drops a revoked contact's
//! stragglers from the shared inbox, which is a separate mechanism from the
//! burn.

mod presence_helper;

use hush_core::{Account, RelayClient};
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

/// Whether a queue can still be read with `read_token` — i.e. it has not been
/// burned. A burned queue reports an error rather than an empty list.
async fn queue_is_readable(relay: &str, queue_id: &str, read_token: &str) -> bool {
    RelayClient::new(relay)
        .list(queue_id, read_token)
        .await
        .is_ok()
}

/// Alice deletes Bob while Carol stays. Bob's queue is burned and Bob can no
/// longer deliver; Carol's queue is untouched and Carol still delivers.
///
/// This is the mutation-check target: stub `Account::burn_contact_queue` to
/// `Ok(())` and both the "burned" assertion and the "cannot deliver" assertion
/// fail, because the queue is still live and Bob's put is accepted.
#[tokio::test]
async fn deleting_one_contact_burns_only_its_queue() {
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

    bob.add_contact("alice", &alice_invite).await.unwrap();
    carol.add_contact("alice", &alice_invite).await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    alice.add_contact("carol", &carol_invite).await.unwrap();

    // Both contacts drain the grant that Alice's add sent into their inbox.
    assert_eq!(bob.recv().await.unwrap().messages.len(), 0);
    assert_eq!(carol.recv().await.unwrap().messages.len(), 0);

    // The grants must actually have been used: each contact now has a
    // per-contact queue recorded for Alice, not just her shared inbox.
    for (who, contact) in [("bob", &bob), ("carol", &carol)] {
        let alice_contact = &contact.contacts()[0];
        assert!(
            alice_contact.delivery_queue_id.is_some()
                && alice_contact.delivery_queue_token.is_some(),
            "{who} must deliver into a per-contact queue, not the shared inbox"
        );
    }

    let bob_account = bob.protocol_name().to_string();
    let carol_account = carol.protocol_name().to_string();
    let bob_queue = alice.own_contact_queue(&bob_account);
    let carol_queue = alice.own_contact_queue(&carol_account);

    // Before the delete, both contacts deliver to their own queue.
    recipient_online(&alice).await;
    bob.send_text("alice", "before delete").await.unwrap();
    carol.send_text("alice", "before delete").await.unwrap();
    assert_eq!(alice.recv().await.unwrap().messages.len(), 2);

    // Alice deletes Bob.
    alice.revoke_contact("bob").await.unwrap();
    assert!(
        alice.contacts().iter().all(|c| c.nickname != "bob"),
        "the deleted contact is gone from the list"
    );
    assert!(
        alice.contacts().iter().any(|c| c.nickname == "carol"),
        "the other contact is untouched"
    );
    assert!(
        !queue_is_readable(&relay, &bob_queue.0, &bob_queue.1).await,
        "the deleted contact's queue must be burned immediately"
    );
    assert!(
        queue_is_readable(&relay, &carol_queue.0, &carol_queue.1).await,
        "the other contact's queue must not be burned"
    );

    // Carol still delivers. This is the positive half: revocation is scoped to
    // one contact, not a lockout of everyone.
    recipient_online(&alice).await;
    carol.send_text("alice", "still here").await.unwrap();
    let after = alice.recv().await.unwrap();
    assert_eq!(
        after.messages.len(),
        1,
        "the surviving contact must still deliver"
    );
    assert_eq!(after.messages[0].from_nick, "carol");

    // The negative case: Bob can no longer deliver. Remove the burn and this
    // send succeeds, which is what makes this a real test of the path.
    recipient_online(&alice).await;
    let err = bob
        .send_text("alice", "after delete")
        .await
        .expect_err("a revoked contact must not be able to deliver");
    assert!(
        !err.to_string().is_empty(),
        "the failure must carry a reason"
    );
}

/// A revoked contact that still holds only the shared inbox cannot get a
/// message surfaced.
///
/// The burn cannot cover this: the shared inbox belongs to every contact, so
/// burning it would cut off everyone. The `revoked` blocklist drops the
/// straggler instead.
///
/// The blob is rewritten in the legacy envelope, which names its sender,
/// because that is the only shape the blocklist can see. A current-format blob
/// on the shared inbox cannot be attributed at all once the sender is gone from
/// the contact list — `try_decrypt_from_any_contact` has nobody to try — so it
/// is left in the queue and warned about rather than filed. Remove the
/// `revoked` check and the legacy blob decrypts and is filed under a hex
/// nickname, which is what this asserts against.
#[tokio::test]
async fn a_revoked_contact_cannot_deliver_into_the_shared_inbox() {
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

    // Bob never drains, so it never learns of the grant and keeps delivering to
    // Alice's shared inbox.
    recipient_online(&alice).await;
    bob.send_text("alice", "into the inbox").await.unwrap();

    let invite = hush_core::InvitePayload::decode(&alice_invite).unwrap();
    let client = RelayClient::new(&relay);
    let read_token = alice.own_read_token().to_string();
    // The inbox holds Bob's queue grant as well as the message, so both are
    // rewritten; the grant is dropped for the same reason, and is not surfaced
    // either way.
    let ids = client.list(&invite.queue_id, &read_token).await.unwrap();
    assert!(!ids.is_empty(), "Bob's grant and message are in the inbox");
    for id in &ids {
        let body = client
            .get(&invite.queue_id, &read_token, id)
            .await
            .unwrap();
        client
            .delete(&invite.queue_id, &read_token, id)
            .await
            .unwrap();
        let legacy = serde_json::json!({ "from": bob.protocol_name(), "ct": body });
        client
            .put(
                &invite.queue_id,
                &invite.queue_token,
                legacy.to_string().as_bytes(),
            )
            .await
            .unwrap();
    }

    alice.revoke_contact("bob").await.unwrap();

    let after = alice.recv().await.unwrap();
    assert!(
        after.messages.is_empty(),
        "a revoked contact's inbox message must be dropped, not filed"
    );
}

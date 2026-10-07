//! Contacts as device sets, and fan-out across devices.
//!
//! The stored shape of a contact changed in the multi-device model: it used to
//! be one device with its queue stored inline, and is now an account id plus a
//! set of devices. An existing vault must keep working without any user action,
//! so most of this file is about the upgrade being faithful.

mod presence_helper;

use hush_core::{Account, ChatRecordPayload, Contact, ContactDevice};
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

/// A contact as it was stored before multi-device: one device, fields inline.
/// The `protocol_name` is that device's identity key hash.
const LEGACY_CONTACT: &str = r#"{
    "nickname":"bob",
    "protocol_name":"ab12cd34",
    "queue_id":"q1",
    "queue_token":"t1",
    "inbound_ready":true
}"#;

#[test]
fn a_legacy_contact_deserializes_into_a_single_device_set() {
    let contact: Contact = serde_json::from_str(LEGACY_CONTACT).unwrap();

    assert_eq!(contact.nickname, "bob");
    assert_eq!(contact.account_id, "ab12cd34");
    assert_eq!(contact.devices.len(), 1, "one device before, one device now");
    assert_eq!(contact.devices[0].device_id, 1);
    assert_eq!(contact.devices[0].queue_id, "q1");
    assert_eq!(contact.devices[0].queue_token, "t1");
    assert!(contact.inbound_ready, "the flag must survive the upgrade");
}

/// The legacy `protocol_name` *was* the remote device's identity key hash, so
/// the identity key is recovered from it rather than lost. If it were dropped,
/// the contact would need re-inviting, which the upgrade must never require.
#[test]
fn a_legacy_contact_keeps_its_identity_key() {
    let identity = vec![7u8; 33];
    let legacy = format!(
        r#"{{"nickname":"bob","protocol_name":"{}","queue_id":"q1",
             "queue_token":"t1","inbound_ready":false}}"#,
        hex::encode(&identity)
    );

    let contact: Contact = serde_json::from_str(&legacy).unwrap();
    assert_eq!(contact.devices[0].identity_key, identity);
}

/// A legacy contact had no `inbound_ready` in the oldest vaults either.
#[test]
fn a_legacy_contact_without_the_optional_flag_still_loads() {
    let legacy = r#"{"nickname":"bob","protocol_name":"ab12","queue_id":"q1","queue_token":"t1"}"#;
    let contact: Contact = serde_json::from_str(legacy).unwrap();
    assert!(!contact.inbound_ready);
    assert_eq!(contact.devices.len(), 1);
}

#[test]
fn a_current_contact_round_trips() {
    let contact = Contact {
        nickname: "bob".into(),
        account_id: "acct".into(),
        mail_handle: None,
        devices: vec![
            ContactDevice {
                device_id: 1,
                identity_key: vec![1u8; 33],
                queue_id: "q1".into(),
                queue_token: "t1".into(),
                added_unix: 0,
            },
            ContactDevice {
                device_id: 2,
                identity_key: vec![2u8; 33],
                queue_id: "q2".into(),
                queue_token: "t2".into(),
                added_unix: 0,
            },
        ],
        inbound_ready: true,
        devices_fetched_unix: 0,
    };

    let json = serde_json::to_string(&contact).unwrap();
    let back: Contact = serde_json::from_str(&json).unwrap();

    assert_eq!(back.nickname, contact.nickname);
    assert_eq!(back.account_id, contact.account_id);
    assert_eq!(back.devices, contact.devices, "both devices must survive");
    assert!(back.inbound_ready);
}

/// The two stored shapes must not be confused for one another: the current
/// shape has no top-level `queue_id`, the legacy shape has no `devices`.
#[test]
fn the_current_shape_serializes_without_legacy_fields() {
    let contact = Contact {
        nickname: "bob".into(),
        account_id: "acct".into(),
        mail_handle: None,
        devices: vec![ContactDevice {
            device_id: 1,
            identity_key: vec![1u8; 33],
            queue_id: "q1".into(),
            queue_token: "t1".into(),
            added_unix: 0,
        }],
        inbound_ready: false,
        devices_fetched_unix: 0,
    };

    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&contact).unwrap()).unwrap();
    assert!(value.get("devices").is_some());
    assert!(
        value.get("queue_id").is_none(),
        "the legacy inline queue must not be written back"
    );
    assert!(value.get("protocol_name").is_none());
}

/// Delivery goes to the account's device 1 when we have no reason to prefer
/// another, which is what every pre-multi-device contact has.
#[test]
fn the_primary_device_is_device_one() {
    let contact = Contact {
        nickname: "bob".into(),
        account_id: "acct".into(),
        mail_handle: None,
        devices: vec![
            ContactDevice {
                device_id: 3,
                identity_key: vec![3u8; 33],
                queue_id: "q3".into(),
                queue_token: "t3".into(),
                added_unix: 0,
            },
            ContactDevice {
                device_id: 1,
                identity_key: vec![1u8; 33],
                queue_id: "q1".into(),
                queue_token: "t1".into(),
                added_unix: 0,
            },
        ],
        inbound_ready: false,
        devices_fetched_unix: 0,
    };

    assert_eq!(
        contact.primary_device().unwrap().queue_id,
        "q1",
        "device 1 wins even when it is not listed first"
    );
}

/// Link a second device of `owner` into `dir`, the way the real flow does: the
/// shared vault is transferred, the device key material is not, and device 1
/// signs the new device into the account's list before anyone can reach it.
///
/// Returns the linked account and the account id both devices share.
async fn link_second_device(
    owner: &mut Account,
    dir: &std::path::Path,
    passphrase: &str,
) -> Account {
    let archive = owner.export_backup().await.unwrap();
    Account::import_backup(dir, &archive).unwrap();
    let mut linked = Account::adopt_linked_vault(dir, passphrase, 2).await.unwrap();

    owner
        .record_linked_device(&linked.link_request().await.unwrap())
        .await
        .unwrap();
    // Prekeys can only be published for a device that is already in the signed
    // list, so this has to come after the list write.
    linked.announce_linked_device(5).await.unwrap();
    linked
}

/// The point of the whole model: one send reaches every device of the recipient,
/// and every other device of the sender.
///
/// Alice has two devices, Bob has one. Bob's message must land in both of
/// Alice's inboxes, and Alice's message must reach Bob *and* be filed on Alice's
/// second device as something she sent.
#[tokio::test]
async fn a_message_reaches_every_device_of_the_recipient_and_our_own() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_1_dir = TempDir::new().unwrap();
    let alice_2_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice_1 = Account::create(alice_1_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();

    let alice_invite = alice_1.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice_1.add_contact("bob", &bob_invite).await.unwrap();

    // Some history for the transfer to carry, so the link can be checked for
    // bringing the conversation across and not just the contact list.
    recipient_online(&bob).await;
    alice_1.send_text("bob", "before linking").await.unwrap();
    assert_eq!(bob.recv().await.unwrap().messages.len(), 1);

    let mut alice_2 = link_second_device(&mut alice_1, alice_2_dir.path(), "alice-pass").await;

    // What the link must and must not have carried (Q4). The identity key is the
    // part that stays behind: sharing it would make the two devices impossible to
    // tell apart, which is the one thing revocation has to do.
    assert_ne!(
        alice_1.link_request().await.unwrap().identity_key,
        alice_2.link_request().await.unwrap().identity_key,
        "a linked device must have its own identity, or it could never be revoked alone"
    );
    assert_eq!(
        alice_2.protocol_name(),
        alice_1.protocol_name(),
        "both devices of one account share the account id"
    );
    assert_eq!(
        alice_2.contacts().len(),
        1,
        "the contact list must come across with the vault"
    );
    let carried: Vec<_> = alice_2
        .chat_history()
        .iter()
        .filter_map(|r| match &r.payload {
            ChatRecordPayload::Text { body } => Some(body.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        carried,
        vec!["before linking".to_string()],
        "the transferred history must be readable on the linked device"
    );

    // Bob added Alice before she linked, and adding a contact already checks the
    // list. That check found one device, so Bob's cache is current and a send
    // now would skip the refresh — a linked device is invisible for up to the
    // 5 minute TTL, which is the documented cost of caching the list at all.
    // Refreshing explicitly stands in for the TTL expiring.
    assert_eq!(
        bob.refresh_contact_devices("alice").await.unwrap(),
        2,
        "once the list is re-read, both of Alice's devices must be known"
    );

    // Bob sends to Alice. Bob only ever had the invite, which names device 1.
    recipient_online(&alice_1).await;
    bob.send_text("alice", "for every device").await.unwrap();

    let on_device_1 = alice_1.recv().await.unwrap();
    assert_eq!(
        on_device_1.messages.len(),
        1,
        "the device the invite named must still receive"
    );
    assert_eq!(on_device_1.messages[0].from_nick, "bob");

    let on_device_2 = alice_2.recv().await.unwrap();
    assert_eq!(
        on_device_2.messages.len(),
        1,
        "the linked device must receive the same message"
    );
    assert_eq!(on_device_2.messages[0].from_nick, "bob");
    assert_eq!(
        on_device_2.messages[0].payload,
        hush_core::MessagePayload::Text {
            body: "for every device".into()
        }
    );

    // Alice sends from device 1. Bob receives it, and her other device gets a
    // copy so the conversation is not split across devices.
    recipient_online(&bob).await;
    alice_1.send_text("bob", "from device 1").await.unwrap();

    let bob_inbox = bob.recv().await.unwrap();
    assert_eq!(bob_inbox.messages.len(), 1);
    assert_eq!(bob_inbox.messages[0].from_nick, "alice");

    let device_2_inbox = alice_2.recv().await.unwrap();
    assert!(
        device_2_inbox.messages.is_empty(),
        "a copy of our own send must not surface as an incoming message"
    );
    let filed = alice_2.chat_history().iter().any(|record| {
        record.outgoing
            && record.contact_nick == "bob"
            && matches!(
                &record.payload,
                ChatRecordPayload::Text { body } if body == "from device 1"
            )
    });
    assert!(
        filed,
        "the linked device must file what device 1 sent as ours, under the right contact"
    );
}

/// Peers can only discover an inbox that moved (the credential split, a vault
/// migration) through the published device list, and the publish on the unlock
/// that rotates is best effort. A device whose publish was lost — or that never
/// published — has to publish on a later unlock instead of never.
#[tokio::test]
async fn a_device_with_no_published_list_publishes_one() {
    let (relay, _relay_dir) = spawn_relay().await;
    let dir = TempDir::new().unwrap();
    let mut alice = Account::create(dir.path(), "alice-pass", &relay)
        .await
        .unwrap();

    assert!(
        alice.ensure_device_list_published().await.unwrap(),
        "an account with nothing published must publish, or a moved inbox is undiscoverable"
    );
    assert!(
        !alice.ensure_device_list_published().await.unwrap(),
        "a list that already names this inbox must not be rewritten on every unlock"
    );
}

/// What is published has to name the inbox this device actually reads, so a peer
/// that refreshes from the list keeps delivering somewhere that works.
#[tokio::test]
async fn the_published_list_agrees_with_the_inbox_that_receives() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "alice-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();

    let bob_invite = bob.invite().await.unwrap();
    let alice_invite = alice.invite().await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    let from_invite = alice.contacts()[0].devices[0].queue_id.clone();

    bob.ensure_device_list_published().await.unwrap();
    alice.refresh_contact_devices("bob").await.unwrap();

    assert_eq!(
        alice.contacts()[0].devices[0].queue_id, from_invite,
        "the published list must not move a peer off the inbox that receives"
    );

    recipient_online(&bob).await;
    alice.send_text("bob", "still reachable").await.unwrap();
    assert_eq!(
        bob.recv().await.unwrap().messages.len(),
        1,
        "the inbox named by the published list must be the one that receives"
    );
}

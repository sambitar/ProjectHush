use libsignal_protocol::IdentityKeyPair;
use rand::rngs::OsRng;
use rand::TryRngCore;

use hush_core::device_list::{AccountToken, DeviceList, DeviceRecord};

fn account_key() -> (IdentityKeyPair, String) {
    let mut csprng = OsRng.unwrap_err();
    let pair = IdentityKeyPair::generate(&mut csprng);
    let id = hex::encode(pair.identity_key().serialize());
    (pair, id)
}

fn record(device_id: u32, seed: u8) -> DeviceRecord {
    DeviceRecord {
        device_id,
        identity_key: vec![seed; 33],
        registration_id: device_id,
        queue_id: format!("queue-{device_id}"),
        queue_token: format!("token-{device_id}"),
        added_unix: 1_700_000_000,
    }
}

#[test]
fn signed_list_verifies_and_detects_tampering() {
    let (key, account_id) = account_key();
    let list = DeviceList::new(account_id.clone(), vec![record(1, 1), record(2, 2)]);
    let signed = list.sign(&key).unwrap();

    let parsed = DeviceList::verify(&signed, &account_id).unwrap();
    assert_eq!(parsed.devices.len(), 2);
    assert_eq!(parsed.token_epoch, 0);

    // Tamper in a way that keeps the JSON valid, so this exercises the signature
    // check rather than the parser. Flipping an arbitrary byte in the middle
    // corrupts the JSON and fails at parse time, which would pass this test even
    // with signature verification removed.
    let needle = b"queue-1";
    let at = signed
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("payload should contain the queue id");
    let mut tampered = signed.clone();
    tampered[at] = b'p'; // "queue-1" -> "pueue-1", still a valid JSON string
    assert!(
        serde_json::from_slice::<serde_json::Value>(&tampered[..tampered.len() - 64]).is_ok(),
        "the tampered payload must still parse, or this proves nothing about the signature"
    );
    assert!(DeviceList::verify(&tampered, &account_id).is_err());
}

/// The property the whole design rests on: a relay is untrusted, so a list it
/// invented must not be accepted. A different key signing a list that claims the
/// same account id has to be rejected.
#[test]
fn a_list_signed_by_the_wrong_key_is_rejected() {
    let (_, account_id) = account_key();
    let (impostor, _) = account_key();

    // The impostor builds a list for our account id and signs it with its own key.
    let list = DeviceList::new(account_id.clone(), vec![record(1, 1)]);
    let signed = list.sign(&impostor).unwrap();

    assert!(
        DeviceList::verify(&signed, &account_id).is_err(),
        "a list signed by anyone other than the account root must be rejected"
    );
}

#[test]
fn a_list_for_a_different_account_is_rejected() {
    let (key, _) = account_key();
    let (_, other_account) = account_key();

    let list = DeviceList::new(other_account.clone(), vec![record(1, 1)]);
    let signed = list.sign(&key).unwrap();

    let (_, expected) = account_key();
    assert!(DeviceList::verify(&signed, &expected).is_err());
}

#[test]
fn a_truncated_blob_is_rejected_rather_than_panicking() {
    let (key, account_id) = account_key();
    let signed = DeviceList::new(account_id.clone(), vec![record(1, 1)])
        .sign(&key)
        .unwrap();

    // Anything at or below the signature length has no payload to parse.
    for len in [0, 1, 63, 64] {
        assert!(DeviceList::verify(&signed[..len], &account_id).is_err());
    }
}

#[test]
fn removing_a_device_drops_it_and_bumps_the_epoch() {
    let (_, account_id) = account_key();
    let list = DeviceList::new(account_id, vec![record(1, 1), record(2, 2)]);

    let after = list.without(2);
    assert_eq!(after.devices.len(), 1);
    assert_eq!(after.devices[0].device_id, 1);
    assert!(
        after.token_epoch > list.token_epoch,
        "revocation must rotate the token epoch"
    );
}

#[test]
fn adding_a_device_keeps_the_epoch_and_replaces_a_duplicate() {
    let (_, account_id) = account_key();
    let mut list = DeviceList::new(account_id, vec![record(1, 1)]);

    list.add(record(2, 2));
    assert_eq!(list.devices.len(), 2);
    assert_eq!(
        list.token_epoch, 0,
        "adding a device must not rotate the token: only revocation does"
    );

    // Re-adding the same id replaces rather than duplicates.
    let mut replacement = record(2, 9);
    replacement.queue_id = "queue-replaced".into();
    list.add(replacement);
    assert_eq!(list.devices.len(), 2);
    assert_eq!(list.find(2).unwrap().queue_id, "queue-replaced");
}

#[test]
fn a_round_tripped_list_survives_a_removal() {
    let (key, account_id) = account_key();
    let list = DeviceList::new(account_id.clone(), vec![record(1, 1), record(2, 2)]);
    let revoked = list.without(2);

    let signed = revoked.sign(&key).unwrap();
    let parsed = DeviceList::verify(&signed, &account_id).unwrap();

    assert_eq!(parsed.devices.len(), 1);
    assert_eq!(parsed.token_epoch, 1);
    assert!(parsed.find(2).is_none());
}

#[test]
fn rotating_the_token_invalidates_the_old_one() {
    let old = AccountToken::generate();
    let new = old.rotate();

    assert_ne!(old.expose(), new.expose());
    assert!(
        old.matches(old.expose()),
        "the old token still matches itself"
    );
    assert!(
        !old.matches(new.expose()),
        "a rotated token must not accept the previous value"
    );
    assert!(new.matches(new.expose()));
    assert!(
        !old.matches(""),
        "an empty candidate must not match a real token"
    );
}

#[test]
fn the_token_is_never_printed() {
    let token = AccountToken::generate();
    let rendered = format!("{token:?}");
    assert!(
        !rendered.contains(token.expose()),
        "Debug must not leak the token: {rendered}"
    );
}

//! The per-device prekey endpoint.
//!
//! Authorization here is the device's own signature, checked against the
//! identity key its entry carries in the account's signed device list. These
//! tests are mostly about what must *not* be accepted, because the happy path is
//! the easy part and the failure to reject is what would let a revoked or
//! unrelated device hijack another device's prekeys.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hush_core::prekey::PREKEY_RECORD_VERSION;
use hush_core::device_list::{DeviceList, DeviceRecord};
use hush_core::{OneTimePreKey, PreKeyRecord};
use hush_relay::{router, RelayConfig};
use http_body_util::BodyExt;
use libsignal_protocol::{kem, IdentityKeyPair, KeyPair};
use rand::rngs::OsRng;
use rand::TryRngCore;
use tempfile::NamedTempFile;
use tower::ServiceExt;

fn temp_db() -> NamedTempFile {
    NamedTempFile::new().unwrap()
}

fn app(db: &NamedTempFile) -> axum::Router {
    router(db.path().to_str().unwrap(), RelayConfig::default()).unwrap()
}

fn account() -> (IdentityKeyPair, String) {
    let mut csprng = OsRng.unwrap_err();
    let pair = IdentityKeyPair::generate(&mut csprng);
    let id = hex::encode(pair.identity_key().serialize());
    (pair, id)
}

/// A genuinely signed prekey record for `device_id`, as the device itself would
/// publish it.
fn make_record(keys: &IdentityKeyPair, device_id: u32, one_time_count: usize) -> PreKeyRecord {
    let mut csprng = OsRng.unwrap_err();

    let signed_pair = KeyPair::generate(&mut csprng);
    let signed_public = signed_pair.public_key.serialize();
    let signed_signature = keys
        .private_key()
        .calculate_signature(&signed_public, &mut csprng)
        .unwrap();

    let kyber_pair = kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut csprng);
    let kyber_public = kyber_pair.public_key.serialize();
    let kyber_signature = keys
        .private_key()
        .calculate_signature(&kyber_public, &mut csprng)
        .unwrap();

    let one_time_prekeys = (0..one_time_count)
        .map(|i| OneTimePreKey {
            key_id: 100 + i as u32,
            public_key: KeyPair::generate(&mut csprng)
                .public_key
                .serialize()
                .to_vec(),
        })
        .collect();

    PreKeyRecord {
        v: PREKEY_RECORD_VERSION,
        device_id,
        registration_id: 7,
        identity_key: keys.identity_key().serialize().to_vec(),
        signed_pre_key_id: 1,
        signed_pre_key_public: signed_public.to_vec(),
        signed_pre_key_signature: signed_signature.to_vec(),
        kyber_pre_key_id: 2,
        kyber_pre_key_public: kyber_public.to_vec(),
        kyber_pre_key_signature: kyber_signature.to_vec(),
        one_time_prekeys,
    }
}

/// Publish a device list naming `devices` (as `(device_id, identity_key)`),
/// signed by device 1.
async fn publish_list(app: &axum::Router, signer: &IdentityKeyPair, account_id: &str, devices: Vec<(u32, Vec<u8>)>) {
    let records = devices
        .into_iter()
        .map(|(device_id, identity_key)| DeviceRecord {
            device_id,
            identity_key,
            registration_id: 7,
            queue_id: format!("queue-{device_id}"),
            queue_token: format!("token-{device_id}"),
            added_unix: 1_700_000_000,
        })
        .collect();
    let signed = DeviceList::new(account_id.to_string(), records)
        .sign(signer)
        .unwrap();
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/d/{account_id}"))
                .body(Body::from(signed))
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::NO_CONTENT, "device list must publish");
}

async fn put_record(
    app: &axum::Router,
    account_id: &str,
    device_id: u32,
    record: &PreKeyRecord,
) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/k/{account_id}/{device_id}"))
                .body(Body::from(record.encode().unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn get_record(
    app: &axum::Router,
    account_id: &str,
    device_id: u32,
) -> (StatusCode, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/k/{account_id}/{device_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn a_published_record_round_trips() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;

    let record = make_record(&keys, 1, 3);
    assert_eq!(
        put_record(&app, &account_id, 1, &record).await,
        StatusCode::NO_CONTENT
    );

    let (status, body) = get_record(&app, &account_id, 1).await;
    assert_eq!(status, StatusCode::OK);
    let fetched = PreKeyRecord::decode(&body).unwrap();
    let expected_identity = keys.identity_key().serialize();
    fetched.verify(&expected_identity).unwrap();
    assert_eq!(fetched.signed_pre_key_id, record.signed_pre_key_id);
    assert_eq!(fetched.one_time_prekeys.len(), 1, "a fetch takes exactly one");
}

/// The property that makes the pool worth having: two peers must not open
/// sessions against the same one-time prekey.
#[tokio::test]
async fn each_fetch_consumes_a_different_one_time_prekey() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;
    put_record(&app, &account_id, 1, &make_record(&keys, 1, 2)).await;

    let (_, first) = get_record(&app, &account_id, 1).await;
    let (_, second) = get_record(&app, &account_id, 1).await;
    let first = PreKeyRecord::decode(&first).unwrap();
    let second = PreKeyRecord::decode(&second).unwrap();

    assert_eq!(first.one_time_prekeys.len(), 1);
    assert_eq!(second.one_time_prekeys.len(), 1);
    assert_ne!(
        first.one_time_prekeys[0].key_id, second.one_time_prekeys[0].key_id,
        "a one-time prekey must never be handed out twice"
    );
}

/// An empty pool must not fail the fetch. libsignal accepts a bundle with no
/// one-time prekey, so the peer still gets a usable session, just without the
/// extra forward secrecy for the first message.
#[tokio::test]
async fn an_exhausted_pool_still_serves_a_bundle() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;
    put_record(&app, &account_id, 1, &make_record(&keys, 1, 1)).await;

    let (_, first) = get_record(&app, &account_id, 1).await;
    assert_eq!(PreKeyRecord::decode(&first).unwrap().one_time_prekeys.len(), 1);

    let (status, drained) = get_record(&app, &account_id, 1).await;
    assert_eq!(status, StatusCode::OK, "a dry pool is not an error");
    let drained = PreKeyRecord::decode(&drained).unwrap();
    assert!(drained.one_time_prekeys.is_empty());
    assert_eq!(
        drained.signed_pre_key_id,
        PreKeyRecord::decode(&first).unwrap().signed_pre_key_id,
        "the signed prekey is what carries the session once the pool is dry"
    );
}

/// Only a listed device may publish, and only under its own identity key. This is
/// what a revoked device loses: once it is gone from the list there is no
/// identity key to check against, so it cannot republish itself.
#[tokio::test]
async fn a_record_for_an_unlisted_device_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;

    // Device 2 is not in the list.
    let record = make_record(&keys, 2, 1);
    assert_eq!(
        put_record(&app, &account_id, 2, &record).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_record_with_the_wrong_identity_key_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let (other, _) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;

    // Correctly signed, but by a key the list does not vouch for.
    let record = make_record(&other, 1, 1);
    assert_eq!(
        put_record(&app, &account_id, 1, &record).await,
        StatusCode::UNAUTHORIZED
    );
}

/// The signature check is what proves the publisher holds the identity private
/// key. Without it, reading the public device list would be enough to replace a
/// device's prekeys with ones the reader controls.
#[tokio::test]
async fn a_record_with_a_forged_prekey_signature_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;

    let mut record = make_record(&keys, 1, 1);
    record.signed_pre_key_signature = vec![0u8; 64];
    assert_eq!(
        put_record(&app, &account_id, 1, &record).await,
        StatusCode::UNAUTHORIZED
    );

    let mut record = make_record(&keys, 1, 1);
    record.kyber_pre_key_signature = vec![0u8; 64];
    assert_eq!(
        put_record(&app, &account_id, 1, &record).await,
        StatusCode::UNAUTHORIZED,
        "the kyber signature is checked too, not just the signed prekey"
    );
}

#[tokio::test]
async fn a_record_claiming_another_device_id_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;

    // Signed for device 1 but posted to device 1's slot while claiming to be
    // device 2. The path and the body must agree.
    let record = make_record(&keys, 2, 1);
    assert_eq!(
        put_record(&app, &account_id, 1, &record).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_device_that_never_published_has_no_record() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity)]).await;

    let (status, _) = get_record(&app, &account_id, 1).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A peer verifies the fetched record independently, so a relay that served
/// something else could not make it be trusted. This asserts the fetched bytes
/// still verify, which is the peer's actual guarantee.
#[tokio::test]
async fn a_fetched_record_verifies_for_the_peer() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();
    let identity = keys.identity_key().serialize().to_vec();
    publish_list(&app, &keys, &account_id, vec![(1, identity.clone())]).await;
    put_record(&app, &account_id, 1, &make_record(&keys, 1, 1)).await;

    let (_, body) = get_record(&app, &account_id, 1).await;
    let fetched = PreKeyRecord::decode(&body).unwrap();
    fetched.verify(&identity).unwrap();
    // And it converts into something libsignal will accept.
    assert!(fetched.to_prekey_bundle().is_ok());
}

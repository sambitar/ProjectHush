//! The per-account prekey endpoint.
//!
//! Authorization is the publisher's own signature, checked against the identity
//! key the account id *is* the hex of. These tests are mostly about what must
//! *not* be accepted, because the happy path is the easy part and a failure to
//! reject is what would let anyone who knows a public account id replace that
//! account's prekeys.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hush_core::prekey::PREKEY_RECORD_VERSION;
use hush_core::{OneTimePreKey, PreKeyRecord, DEVICE_ID};
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

/// A fresh account: its identity key pair, and the account id peers address it
/// by, which is the hex of its identity public key.
fn account() -> (IdentityKeyPair, String) {
    let mut csprng = OsRng.unwrap_err();
    let pair = IdentityKeyPair::generate(&mut csprng);
    let id = hex::encode(pair.identity_key().serialize());
    (pair, id)
}

/// A genuinely signed prekey record for `device_id`, as the account itself would
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

async fn put_record(app: &axum::Router, account_id: &str, record: &PreKeyRecord) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/k/{account_id}"))
                .body(Body::from(record.encode().unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn get_record(app: &axum::Router, account_id: &str) -> (StatusCode, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/k/{account_id}"))
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

    let record = make_record(&keys, DEVICE_ID, 3);
    assert_eq!(
        put_record(&app, &account_id, &record).await,
        StatusCode::NO_CONTENT
    );

    let (status, body) = get_record(&app, &account_id).await;
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
    put_record(&app, &account_id, &make_record(&keys, DEVICE_ID, 2)).await;

    let (_, first) = get_record(&app, &account_id).await;
    let (_, second) = get_record(&app, &account_id).await;
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
    put_record(&app, &account_id, &make_record(&keys, DEVICE_ID, 1)).await;

    let (_, first) = get_record(&app, &account_id).await;
    assert_eq!(
        PreKeyRecord::decode(&first).unwrap().one_time_prekeys.len(),
        1
    );

    let (status, drained) = get_record(&app, &account_id).await;
    assert_eq!(status, StatusCode::OK, "a dry pool is not an error");
    let drained = PreKeyRecord::decode(&drained).unwrap();
    assert!(drained.one_time_prekeys.is_empty());
    assert_eq!(
        drained.signed_pre_key_id,
        PreKeyRecord::decode(&first).unwrap().signed_pre_key_id,
        "the signed prekey is what carries the session once the pool is dry"
    );
}

/// A record signed by a key that is not the account's must be rejected. This is
/// the whole authorization: the account id is the identity key, so a publisher
/// that cannot sign with the matching private key cannot claim the account.
#[tokio::test]
async fn a_record_with_the_wrong_identity_key_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (_keys, account_id) = account();
    let (other, _) = account();

    // Correctly signed, but by a key that is not the one the account id names.
    let record = make_record(&other, DEVICE_ID, 1);
    assert_eq!(
        put_record(&app, &account_id, &record).await,
        StatusCode::UNAUTHORIZED
    );
}

/// The signature check is what proves the publisher holds the identity private
/// key. Without it, knowing a public account id would be enough to replace its
/// prekeys with ones the writer controls.
#[tokio::test]
async fn a_record_with_a_forged_prekey_signature_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (keys, account_id) = account();

    let mut record = make_record(&keys, DEVICE_ID, 1);
    record.signed_pre_key_signature = vec![0u8; 64];
    assert_eq!(
        put_record(&app, &account_id, &record).await,
        StatusCode::UNAUTHORIZED
    );

    let mut record = make_record(&keys, DEVICE_ID, 1);
    record.kyber_pre_key_signature = vec![0u8; 64];
    assert_eq!(
        put_record(&app, &account_id, &record).await,
        StatusCode::UNAUTHORIZED,
        "the kyber signature is checked too, not just the signed prekey"
    );
}

/// An account id that is not a hex identity key cannot be checked against
/// anything, so it is refused rather than stored unverified.
#[tokio::test]
async fn a_non_hex_account_id_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (keys, _) = account();

    let record = make_record(&keys, DEVICE_ID, 1);
    assert_eq!(
        put_record(&app, "not-hex", &record).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn an_account_that_never_published_has_no_record() {
    let db = temp_db();
    let app = app(&db);
    let (_keys, account_id) = account();

    let (status, _) = get_record(&app, &account_id).await;
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
    put_record(&app, &account_id, &make_record(&keys, DEVICE_ID, 1)).await;

    let (_, body) = get_record(&app, &account_id).await;
    let fetched = PreKeyRecord::decode(&body).unwrap();
    fetched.verify(&identity).unwrap();
    // And it converts into something libsignal will accept, addressed to the
    // account's one device.
    let bundle = fetched.to_prekey_bundle().unwrap();
    assert_eq!(u32::from(bundle.device_id().unwrap()), DEVICE_ID);
}

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hush_core::device_list::{DeviceList, DeviceRecord};
use hush_relay::{router, RelayConfig};
use libsignal_protocol::IdentityKeyPair;
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

/// A stand-in for device 1: its identity key hash is the account id, and it is
/// the only key that can authorize a write.
fn account() -> (IdentityKeyPair, String) {
    let mut csprng = OsRng.unwrap_err();
    let pair = IdentityKeyPair::generate(&mut csprng);
    let id = hex::encode(pair.identity_key().serialize());
    (pair, id)
}

fn record(device_id: u32) -> DeviceRecord {
    DeviceRecord {
        device_id,
        identity_key: vec![device_id as u8; 33],
        registration_id: device_id,
        queue_id: format!("queue-{device_id}"),
        queue_token: format!("token-{device_id}"),
        added_unix: 1_700_000_000,
    }
}

async fn put_dir(app: &axum::Router, account_id: &str, body: &[u8]) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/d/{account_id}"))
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn get_dir(app: &axum::Router, account_id: &str) -> (StatusCode, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/d/{account_id}"))
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
async fn a_signed_list_round_trips() {
    let db = temp_db();
    let app = app(&db);
    let (key, account_id) = account();

    let signed = DeviceList::new(account_id.clone(), vec![record(1), record(2)])
        .sign(&key)
        .unwrap();
    assert_eq!(
        put_dir(&app, &account_id, &signed).await,
        StatusCode::NO_CONTENT
    );

    // A peer fetching this list holds no credential for the account, so reads
    // must work unauthenticated. The signature is what makes it trustworthy.
    let (status, body) = get_dir(&app, &account_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, signed);

    let parsed = DeviceList::verify(&body, &account_id).unwrap();
    assert_eq!(parsed.devices.len(), 2);
}

#[tokio::test]
async fn an_unknown_account_is_not_found() {
    let db = temp_db();
    let app = app(&db);
    let (_, account_id) = account();
    let (status, _) = get_dir(&app, &account_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unsigned_write_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (_, account_id) = account();

    assert_eq!(
        put_dir(&app, &account_id, b"just some bytes").await,
        StatusCode::UNAUTHORIZED
    );
}

/// The property revocation depends on: a device that does not hold device 1's
/// private key cannot publish a list, no matter what it claims.
#[tokio::test]
async fn a_list_signed_by_another_key_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (_, account_id) = account();
    let (impostor, _) = account();

    // A valid device list, correctly shaped, for the right account id, but
    // signed by the wrong key.
    let signed = DeviceList::new(account_id.clone(), vec![record(1)])
        .sign(&impostor)
        .unwrap();

    assert_eq!(
        put_dir(&app, &account_id, &signed).await,
        StatusCode::UNAUTHORIZED,
        "only device 1 may write a device list"
    );
    let (status, _) = get_dir(&app, &account_id).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "nothing may have been stored"
    );
}

#[tokio::test]
async fn a_valid_write_cannot_be_published_under_another_account() {
    let db = temp_db();
    let app = app(&db);
    let (key, account_id) = account();
    let (_, other_account) = account();

    let signed = DeviceList::new(account_id.clone(), vec![record(1)])
        .sign(&key)
        .unwrap();

    // The signature is genuine, but for a different account id than the path.
    assert_eq!(
        put_dir(&app, &other_account, &signed).await,
        StatusCode::UNAUTHORIZED
    );
}

/// The revocation path the token model could not execute. Device 1 removes a
/// device, re-signs, and the relay accepts the update with no rotation step and
/// no window for the revoked device to interfere.
#[tokio::test]
async fn revocation_publishes_a_smaller_list_with_no_rotation_step() {
    let db = temp_db();
    let app = app(&db);
    let (key, account_id) = account();

    let before = DeviceList::new(account_id.clone(), vec![record(1), record(2), record(3)]);
    let signed_before = before.sign(&key).unwrap();
    assert_eq!(
        put_dir(&app, &account_id, &signed_before).await,
        StatusCode::NO_CONTENT
    );

    let after = before.without(2);
    let signed_after = after.sign(&key).unwrap();
    assert_eq!(
        put_dir(&app, &account_id, &signed_after).await,
        StatusCode::NO_CONTENT,
        "the account must be able to publish the updated list straight away"
    );

    let (_, body) = get_dir(&app, &account_id).await;
    let parsed = DeviceList::verify(&body, &account_id).unwrap();
    assert_eq!(parsed.devices.len(), 2);
    assert!(parsed.find(2).is_none(), "the revoked device is gone");
    assert_eq!(
        parsed.epoch, 1,
        "the epoch advanced for peers holding a cache"
    );
}

#[tokio::test]
async fn an_oversized_list_is_rejected() {
    let db = temp_db();
    let app = app(&db);
    let (_, account_id) = account();
    let huge = vec![0u8; RelayConfig::default().max_blob_bytes + 1];
    assert_eq!(
        put_dir(&app, &account_id, &huge).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

//! The queue credential split: what a peer is given to *deliver* must not also
//! let it read, delete or burn.
//!
//! An invite carries the delivery token, so that token is public to anyone who
//! has been handed the invite — the same people who are meant to deliver. If
//! that one token also authorized reads, a peer could list the owner's inbox
//! (message timing and volume, the "who talks to whom" property the relay is
//! explicitly meant not to expose) and, worse, burn it: `report_queue` is
//! destructive.
//!
//! The read token never leaves the device. It authorizes list/read/delete/burn;
//! the delivery token is derived from it as `HMAC(read_token, context)`, so the
//! relay can recognise a read request without a second credential to register.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hush_core::queue_delivery_token;
use hush_relay::{router, RelayConfig};
use tempfile::NamedTempFile;
use tower::ServiceExt;

fn temp_db() -> NamedTempFile {
    NamedTempFile::new().unwrap()
}

fn app(db: &NamedTempFile) -> axum::Router {
    router(db.path().to_str().unwrap(), RelayConfig::default()).unwrap()
}

async fn request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

/// The whole point, in one test: the owner can do everything with its read
/// token, and the token that appears in the invite can only deliver.
#[tokio::test]
async fn the_published_delivery_token_cannot_read_delete_or_burn_an_inbox() {
    let db = temp_db();
    let app = app(&db);

    let queue_id = "b7c1f2a4-0000-4000-8000-000000000001".to_string();
    let read_token = "9f2d3e4a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60";
    let delivery_token = queue_delivery_token(read_token);
    assert_ne!(
        delivery_token, read_token,
        "the published token must not be the private one"
    );

    // The owner binds its queue by heartbeating with the read token. Only the
    // owner can do this: the stored binding is the delivery token derived from
    // the read token, and nothing else produces it.
    let (status, _) = request(
        &app,
        "PUT",
        &format!("/q/{queue_id}/presence"),
        read_token,
        Vec::new(),
    )
    .await;
    assert!(status.is_success(), "the owner must be able to bind its queue");

    // A peer delivers using the token it was handed in the invite.
    let (status, _) = request(
        &app,
        "PUT",
        &format!("/q/{queue_id}"),
        &delivery_token,
        b"sealed for the owner".to_vec(),
    )
    .await;
    assert!(status.is_success(), "delivery with the delivery token");

    // The owner drains with the read token.
    let (status, listed) = request(&app, "GET", &format!("/q/{queue_id}"), read_token, Vec::new())
        .await;
    assert_eq!(status, StatusCode::OK, "the owner must be able to list");
    let blobs: serde_json::Value = serde_json::from_slice(&listed).unwrap();
    let blob_id = blobs["blobs"][0]["id"].as_str().unwrap().to_string();

    let (status, body) = request(
        &app,
        "GET",
        &format!("/q/{queue_id}/{blob_id}"),
        read_token,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"sealed for the owner");

    // Now the attack, using the token the peer already holds.
    let leaked = delivery_token;
    let leaked_queue = queue_id.clone();

    // Listing is refused.
    let (status, _) = request(&app, "GET", &format!("/q/{leaked_queue}"), &leaked, Vec::new()).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "the delivery token must not list the owner's inbox"
    );

    // Reading a specific blob is refused.
    let (status, _) = request(
        &app,
        "GET",
        &format!("/q/{leaked_queue}/{blob_id}"),
        &leaked,
        Vec::new(),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "and must not read a message");

    // Deleting is refused.
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/q/{leaked_queue}/{blob_id}"),
        &leaked,
        Vec::new(),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "and must not delete a message");

    // And burning is refused. This is the one that mattered most: it is
    // destructive and it is what a contact's delete relies on being the owner's
    // alone.
    let (status, _) = request(
        &app,
        "POST",
        &format!("/q/{leaked_queue}/report"),
        &leaked,
        Vec::new(),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "the delivery token must not burn the owner's inbox"
    );

    // The message is still there and still readable by the owner, which is the
    // property the whole split exists to preserve.
    let (status, listed) = request(&app, "GET", &format!("/q/{queue_id}"), read_token, Vec::new())
        .await;
    assert_eq!(status, StatusCode::OK);
    let blobs: serde_json::Value = serde_json::from_slice(&listed).unwrap();
    assert_eq!(
        blobs["blobs"].as_array().unwrap().len(),
        1,
        "the attack must not have destroyed the owner's message"
    );
}

/// A peer may deliver to a queue the owner has not bound yet, and the owner can
/// still read it afterwards. The queue is created by whichever side gets there
/// first, and both credentials resolve to the same stored hash.
#[tokio::test]
async fn a_peer_may_deliver_before_the_owner_binds_its_queue() {
    let db = temp_db();
    let app = app(&db);

    let queue_id = "b7c1f2a4-0000-4000-8000-000000000002".to_string();
    let read_token = "1111111111111111111111111111111111111111111111111111111111111111";
    let delivery_token = queue_delivery_token(read_token);

    let (status, _) = request(
        &app,
        "PUT",
        &format!("/q/{queue_id}"),
        &delivery_token,
        b"first".to_vec(),
    )
    .await;
    assert!(status.is_success(), "a peer delivers first");

    let (status, listed) = request(&app, "GET", &format!("/q/{queue_id}"), read_token, Vec::new())
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the owner must still be able to read a queue it never bound"
    );
    let blobs: serde_json::Value = serde_json::from_slice(&listed).unwrap();
    assert_eq!(blobs["blobs"].as_array().unwrap().len(), 1);
}

/// Presence is the one read that a *peer* performs, so it stays on the delivery
/// token. Nothing else does.
#[tokio::test]
async fn a_peer_can_read_presence_with_the_delivery_token() {
    let db = temp_db();
    let app = app(&db);

    let queue_id = "b7c1f2a4-0000-4000-8000-000000000003".to_string();
    let read_token = "2222222222222222222222222222222222222222222222222222222222222222";
    let delivery_token = queue_delivery_token(read_token);

    let (status, _) = request(
        &app,
        "PUT",
        &format!("/q/{queue_id}/presence"),
        read_token,
        Vec::new(),
    )
    .await;
    assert!(status.is_success());

    let (status, _) = request(
        &app,
        "GET",
        &format!("/q/{queue_id}/presence"),
        &delivery_token,
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a peer holds only the delivery token and still needs the online indicator"
    );
}

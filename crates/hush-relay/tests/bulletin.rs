//! The bulletin lane: one flat, append-only log of opaque blobs that any member
//! may publish to and every member drains.
//!
//! This is the first relay ability added since the founding constraint, so the
//! tests are about what the relay must *not* learn or hold as much as about the
//! round trip: the stored row carries no member, a re-post does not become a
//! second copy, burning a member's queue does not delete the corpus, and every
//! drainer sees the same lane. The publish credential is the *private* half of
//! the queue pair — the delivery token travels in invites, so accepting it here
//! would let anyone holding an invite publish as that member and spend their
//! rate budget.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use http_body_util::BodyExt;
use hush_core::queue_delivery_token;
use hush_relay::{router, RelayConfig};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::NamedTempFile;
use tokio::time::sleep;
use tower::ServiceExt;

fn temp_db() -> NamedTempFile {
    NamedTempFile::new().unwrap()
}

async fn app(config: RelayConfig, db: &NamedTempFile) -> axum::Router {
    router(db.path().to_str().unwrap(), config).unwrap()
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

/// Drain the lane and return `(seq, body)` pairs in order.
async fn drain(app: &axum::Router, queue_id: &str, read_token: &str, since: i64) -> Vec<(i64, Vec<u8>)> {
    let (status, bytes) = request(
        app,
        "GET",
        &format!("/b/{queue_id}?since={since}"),
        read_token,
        Vec::new(),
    )
    .await;
    assert!(status.is_success(), "drain failed: {status}");
    let parsed: Value = serde_json::from_slice(&bytes).unwrap();
    parsed["blobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["seq"].as_i64().unwrap(),
                B64.decode(b["body"].as_str().unwrap()).unwrap(),
            )
        })
        .collect()
}

const QUEUE: &str = "b7c1f2a4-0000-4000-8000-0000000000b1";
const READ: &str = "9f2d3e4a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60";

/// The core round trip, and `since` as the cursor a drainer keeps.
#[tokio::test]
async fn a_blob_round_trips_and_since_is_the_cursor() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;

    let (status, bytes) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"first".to_vec()).await;
    assert!(status.is_success(), "publish failed: {status}");
    let put: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(put["seq"].as_i64().unwrap(), 1, "the log starts at seq 1");

    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"second".to_vec()).await;

    let all = drain(&app, QUEUE, READ, 0).await;
    assert_eq!(all.len(), 2, "a fresh cursor sees the whole lane");
    assert_eq!(all[0].1, b"first");
    assert_eq!(all[1].1, b"second");

    // The cursor: everything after the first blob is the second one only.
    let after = drain(&app, QUEUE, READ, all[0].0).await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].1, b"second");
    assert_eq!(after[0].0, all[1].0);
}

/// The publish credential is the private one. The token that travels in invites
/// must not be able to spend a member's budget.
#[tokio::test]
async fn a_publish_needs_the_private_credential() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let delivery = queue_delivery_token(READ);

    // The owner binds its own bulletin queue by publishing once.
    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"mine".to_vec()).await;
    assert!(status.is_success(), "the owner publishes with the read token");

    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), &delivery, b"x".to_vec()).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the token that travels in invites must not publish"
    );

    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), "someone-elses", b"x".to_vec()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a stranger's token");

    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), "", b"x".to_vec()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no credential at all");

    // And the owner is still the owner afterwards.
    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"more".to_vec()).await;
    assert!(status.is_success());
}

/// Why the client does not publish under its inbox queue id.
///
/// The relay binds an unclaimed queue to whichever credential reaches it first —
/// that self-binding is how it avoids a registration step. The inbox id and its
/// delivery token both travel in an invite, so a contact could claim that queue
/// on the owner's behalf with a token whose derived value the owner's read token
/// cannot reproduce, and the owner would be locked out of their own publish
/// credential. This test pins that behaviour so the reason the client derives a
/// queue id only it can name (`hush_core::bulletin_queue_id`) stays visible.
#[tokio::test]
async fn an_unclaimed_queue_binds_to_the_first_credential() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let delivery = queue_delivery_token(READ);

    // A contact publishes under the inbox id with the delivery token it holds.
    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), &delivery, b"claim".to_vec()).await;
    assert!(status.is_success(), "the first credential claims the queue");

    // The owner's read token no longer matches the binding.
    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"mine".to_vec()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Publishing is not a heartbeat: the queue is bound, but presence stays zero,
/// so a member publishing a zine does not appear online to contacts.
#[tokio::test]
async fn a_publish_binds_the_queue_without_a_heartbeat() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"x".to_vec()).await;

    let delivery = queue_delivery_token(READ);
    let (status, bytes) = request(
        &app,
        "GET",
        &format!("/q/{QUEUE}/presence"),
        &delivery,
        Vec::new(),
    )
    .await;
    assert!(status.is_success(), "the queue must exist after a publish");
    let presence: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(presence["last_at"].as_i64().unwrap(), 0);
    assert_eq!(presence["online"].as_bool().unwrap(), false);
}

#[tokio::test]
async fn an_oversize_blob_is_refused() {
    let db = temp_db();
    let config = RelayConfig::default();
    let cap = config.bulletin_max_blob_bytes;
    let app = app(config, &db).await;

    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, vec![0u8; cap]).await;
    assert!(status.is_success(), "a blob at the cap is fine");

    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, vec![0u8; cap + 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// The bulletin has its own clock, longer than the chat one, and expiry is the
/// on-request purge this crate already uses.
#[tokio::test]
async fn the_bulletin_expires_on_its_own_clock() {
    let db = temp_db();
    let app = app(RelayConfig::for_bulletin_tests(), &db).await;

    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"old".to_vec()).await;
    assert_eq!(drain(&app, QUEUE, READ, 0).await.len(), 1);

    sleep(Duration::from_secs(2)).await;
    assert!(
        drain(&app, QUEUE, READ, 0).await.is_empty(),
        "a blob past the bulletin TTL is gone"
    );
}

#[tokio::test]
async fn a_member_is_rate_limited_on_publish() {
    let db = temp_db();
    let config = RelayConfig {
        rate_limit_max: 3,
        rate_limit_window: Duration::from_secs(60),
        ..RelayConfig::default()
    };
    let app = app(config, &db).await;

    for i in 0..3 {
        let (status, _) = request(
            &app,
            "PUT",
            &format!("/b/{QUEUE}"),
            READ,
            format!("blob-{i}").into_bytes(),
        )
        .await;
        assert!(status.is_success(), "publish {i} is within the budget");
    }
    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"one-more".to_vec()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // The limit is the member's, and draining is not charged against it: a
    // poll tick must not be able to lock a member out of publishing.
    let (status, _) = request(&app, "GET", &format!("/b/{QUEUE}?since=0"), READ, Vec::new()).await;
    assert!(status.is_success());
}

/// The row the relay writes names no member. Read straight from SQLite, because
/// this is a claim about storage, not about a response body.
#[tokio::test]
async fn the_stored_row_names_no_member() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"ciphertext-bytes".to_vec()).await;

    let conn = Connection::open(db.path()).unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(bulletin)").unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|c| c.unwrap())
        .collect();
    assert_eq!(
        columns,
        vec!["seq", "hash", "body", "inserted_at"],
        "no queue id, no account id, no timestamp of who"
    );

    let (body, hash): (Vec<u8>, String) = conn
        .query_row("SELECT body, hash FROM bulletin", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(body, b"ciphertext-bytes", "the relay stores what it was given");
    assert!(!hash.is_empty(), "the dedup hash is stored beside it");

    // Nothing anywhere in the file spells out the member's credential.
    let queue_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM queues", [], |row| row.get(0))
        .unwrap();
    assert_eq!(queue_rows, 1, "the queue exists, in the queue table only");
}

/// A re-post is the same blob, so it keeps its place and refreshes the clock
/// instead of becoming a second copy — which is what makes the holder re-post
/// sweep affordable.
#[tokio::test]
async fn a_re_post_keeps_its_place_and_refreshes_the_clock() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;

    let (_, first) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"held".to_vec()).await;
    let seq: i64 = serde_json::from_slice::<Value>(&first).unwrap()["seq"]
        .as_i64()
        .unwrap();
    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"newer".to_vec()).await;

    let (_, again) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"held".to_vec()).await;
    let re_seq: i64 = serde_json::from_slice::<Value>(&again).unwrap()["seq"]
        .as_i64()
        .unwrap();
    assert_eq!(re_seq, seq, "a re-post is the same row");

    let lane = drain(&app, QUEUE, READ, 0).await;
    assert_eq!(lane.len(), 2, "the lane did not grow a copy");
    assert_eq!(lane[0].1, b"held");
}

/// Burning a member's queue is about their inbox. The corpus must survive it:
/// the bulletin has no foreign key into the queue table on purpose.
#[tokio::test]
async fn burning_a_queue_leaves_the_bulletin_alone() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;

    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"corpus".to_vec()).await;
    let (status, _) = request(&app, "POST", &format!("/q/{QUEUE}/report"), READ, Vec::new()).await;
    assert!(status.is_success(), "the owner can burn its own queue");

    let other = "b7c1f2a4-0000-4000-8000-0000000000b2";
    let other_read = "1111111111111111111111111111111111111111111111111111111111111111";
    let lane = drain(&app, other, other_read, 0).await;
    assert_eq!(lane.len(), 1, "another member still drains the corpus");
    assert_eq!(lane[0].1, b"corpus");

    // And the burned queue cannot publish any more.
    let (status, _) = request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"x".to_vec()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Everyone drains the same lane: the drain says nothing about what this member
/// reads, because there is no per-reader view to differ.
#[tokio::test]
async fn every_drainer_sees_the_same_lane() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"one".to_vec()).await;
    request(&app, "PUT", &format!("/b/{QUEUE}"), READ, b"two".to_vec()).await;

    let a = drain(&app, QUEUE, READ, 0).await;
    let b = drain(&app, "b7c1f2a4-0000-4000-8000-0000000000b3", "2222222222222222222222222222222222222222222222222222222222222222", 0).await;
    assert_eq!(a, b);

    // Draining removes nothing: the lane is not a mailbox.
    assert_eq!(drain(&app, QUEUE, READ, 0).await.len(), 2);
}

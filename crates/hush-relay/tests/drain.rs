use axum::body::Body;
use axum::http::{Request, StatusCode};
use hush_core::queue_delivery_token;
use hush_relay::{RelayConfig, router};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::NamedTempFile;
use tower::ServiceExt;

fn temp_db() -> NamedTempFile {
    NamedTempFile::new().unwrap()
}

async fn app(config: RelayConfig, db: &NamedTempFile) -> axum::Router {
    router(db.path().to_str().unwrap(), config).unwrap()
}

fn auth(token: &str) -> (&'static str, String) {
    ("authorization", format!("Bearer {token}"))
}

async fn put_blob(app: &axum::Router, queue: &str, token: &str, body: &[u8]) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/q/{queue}"))
                .header(auth(token).0, auth(token).1)
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "put must succeed");
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    json["id"].as_str().unwrap().to_string()
}

async fn drain(app: &axum::Router, read_token: &str, query: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/q/q1/drain{query}"))
                .header(auth(read_token).0, auth(read_token).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn ack(app: &axum::Router, read_token: &str, queue: &str, ids: &[String]) -> StatusCode {
    let body = serde_json::json!({ "ids": ids });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/q/{queue}/drain/ack"))
                .header(auth(read_token).0, auth(read_token).1)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    res.status()
}

#[tokio::test]
async fn drain_page_is_stable_until_acked_then_gone() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let read = "read-secret";
    let delivery = queue_delivery_token(read);

    let mut sent_ids = Vec::new();
    for i in 0..3u8 {
        let id = put_blob(&app, "q1", &delivery, format!("blob-{i}").as_bytes()).await;
        sent_ids.push(id);
    }

    let (status, json) = drain(&app, read, "").await;
    assert_eq!(status, StatusCode::OK, "drain must succeed: {json}");
    let blobs = json["blobs"].as_array().expect("blobs array");
    assert_eq!(blobs.len(), 3, "all three blobs in one page");

    // Order is created order; bodies decode to what was stored — the same
    // base64 wire encoding the bulletin drain uses.
    use base64::Engine;
    let ids: Vec<String> = blobs
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, sent_ids, "drain preserves created order");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(blobs[0]["body"].as_str().unwrap())
            .unwrap(),
        b"blob-0"
    );

    // Non-consuming: an identical page comes back until the caller acks.
    let (_, again) = drain(&app, read, "").await;
    assert_eq!(again, json, "the page must be stable before the ack");

    // Ack consumes exactly the acked ids.
    let status = ack(&app, read, "q1", &ids).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, after) = drain(&app, read, "").await;
    assert_eq!(after["blobs"].as_array().unwrap().len(), 0);

    // A repeated ack is idempotent success, not an error.
    let status = ack(&app, read, "q1", &ids).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn held_blob_survives_a_partial_ack() {
    // The whole point of the two-phase shape: the caller files what it can
    // and acks only those. An unacked blob — a conclave message whose roster
    // has not converged, say — must still be there afterwards.
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let read = "read-secret";
    let delivery = queue_delivery_token(read);
    let first = put_blob(&app, "q1", &delivery, b"filed").await;
    let held = put_blob(&app, "q1", &delivery, b"held-for-later").await;

    let (_, json) = drain(&app, read, "").await;
    let status = ack(&app, read, "q1", &[json["blobs"][0]["id"].as_str().unwrap().into()])
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, next) = drain(&app, read, "").await;
    let ids: Vec<String> = next["blobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![held], "the held blob must redeliver");
    assert_ne!(ids, vec![first]);
}

#[tokio::test]
async fn drain_refuses_the_delivery_token_and_unknown_queues() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let read = "read-secret";
    let delivery = queue_delivery_token(read);
    put_blob(&app, "q1", &delivery, b"blob").await;

    // The delivery token is the public half: it must not drain an inbox.
    let (status, _) = drain(&app, &delivery, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An unknown queue is not found, not an empty success — the caller should
    // not mistake a wrong address for a drained one.
    let (status, _) = drain(&app, "no-such-token", "").await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::NOT_FOUND,
        "wrong token on an unknown queue must not read as success, got {status}"
    );

    // And the ack is guarded the same way: the queue exists, so a wrong
    // token is unauthorized.
    let status = ack(&app, "no-such-token", "q1", &["x".into()]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_ack_can_only_consume_its_own_queue() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let q1_read = "q1-read-secret";
    let q2_read = "q2-read-secret";
    let q1_delivery = queue_delivery_token(q1_read);
    let q2_delivery = queue_delivery_token(q2_read);
    let q1_blob = put_blob(&app, "q1", &q1_delivery, b"q1-blob").await;
    put_blob(&app, "q2", &q2_delivery, b"q2-blob").await;

    // q2's read token authorizes q2, not q1: the ack must refuse and leave
    // q1's blob in place. A wrong-token ack on an unknown-to-that-token queue
    // is 404; either way it must not be a success that deletes.
    let status = ack(&app, q2_read, "q1", &[q1_blob.clone()]).await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::NOT_FOUND,
        "cross-queue ack must be refused, got {status}"
    );

    let (_, still_there) = drain(&app, q1_read, "").await;
    assert_eq!(
        still_there["blobs"].as_array().unwrap().len(),
        1,
        "the refused ack must not have consumed the blob"
    );
}

#[tokio::test]
async fn drain_limit_bounds_the_page_and_the_rest_survives() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let read = "read-secret";
    let delivery = queue_delivery_token(read);
    for i in 0..5u8 {
        put_blob(&app, "q1", &delivery, format!("blob-{i}").as_bytes()).await;
    }

    let (status, json) = drain(&app, read, "?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["blobs"].as_array().unwrap().len(), 2, "limit clamps");

    // The unasked-for rest is still on the queue: nothing was consumed.
    let (_, rest) = drain(&app, read, "?limit=1024").await;
    assert_eq!(
        rest["blobs"].as_array().unwrap().len(),
        5,
        "a limited peek consumes nothing"
    );
}

#[tokio::test]
async fn drain_route_absent_when_disabled_so_clients_fall_back() {
    let db = temp_db();
    let config = RelayConfig {
        enable_batch_drain: false,
        ..RelayConfig::default()
    };
    let app = app(config, &db).await;
    let read = "read-secret";
    let delivery = queue_delivery_token(read);
    put_blob(&app, "q1", &delivery, b"blob").await;

    let (status, _) = drain(&app, read, "").await;
    assert_eq!(
        status, StatusCode::METHOD_NOT_ALLOWED,
        "a disabled drain must look like an old relay: POST falls through to the dynamic \
         /q/{{queue_id}}/{{blob_id}} route, which has no POST handler, so the answer is 405 — \
         exactly what a pre-drain relay answers and what triggers the client fallback"
    );

    // The legacy per-blob path still works with the drain off.
    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/q/q1")
                .header(auth(read).0, auth(read).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
}

#[tokio::test]
async fn burned_queue_refuses_to_drain() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let read = "read-secret";
    let delivery = queue_delivery_token(read);
    put_blob(&app, "q1", &delivery, b"blob").await;

    let burn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/q/q1/report")
                .header(auth(read).0, auth(read).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(burn.status(), StatusCode::NO_CONTENT);

    let (status, _) = drain(&app, read, "").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a burned queue must refuse to drain, like it refuses to list"
    );
}

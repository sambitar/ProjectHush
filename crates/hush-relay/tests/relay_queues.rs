use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hush_core::queue_delivery_token;
use hush_relay::{RelayConfig, router};
use http_body_util::BodyExt;
use tempfile::NamedTempFile;
use tokio::time::sleep;
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

#[tokio::test]
async fn unauthorized_put_fails() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/test-queue")
                .body(Body::from(vec![1, 2, 3]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn put_get_delete_roundtrip() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    // The owner holds both halves of the credential pair: it publishes the
    // delivery token and keeps the read token for its own inbox.
    let read = "slot-secret-abc";
    let delivery = queue_delivery_token(read);
    let body = b"ciphertext-bytes";

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/q1")
                .header(auth(&delivery).0, auth(&delivery).1)
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let put_bytes = put.into_body().collect().await.unwrap().to_bytes();
    let put_json: serde_json::Value = serde_json::from_slice(&put_bytes).unwrap();
    let blob_id = put_json["id"].as_str().unwrap();

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

    let got = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/q/q1/{}", blob_id))
                .header(auth(read).0, auth(read).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(got.status(), StatusCode::OK);
    let got_bytes = got.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(got_bytes.as_ref(), body);

    let del = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/q/q1/{}", blob_id))
                .header(auth(read).0, auth(read).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn wrong_token_put_fails_after_queue_minted() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/q2")
                .header("authorization", "Bearer good")
                .body(Body::from(vec![9]))
                .unwrap(),
        )
        .await
        .unwrap();
    let bad = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/q2")
                .header("authorization", "Bearer bad")
                .body(Body::from(vec![8]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expire_drops_blob() {
    let db = temp_db();
    let app = app(RelayConfig::for_tests(), &db).await;
    let read = "tok";
    let delivery = queue_delivery_token(read);
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/exp")
                .header(auth(&delivery).0, auth(&delivery).1)
                .body(Body::from(b"old".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    sleep(Duration::from_secs(2)).await;
    let list = app
        .oneshot(
            Request::builder()
                .uri("/q/exp")
                .header(auth(read).0, auth(read).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = list.into_body().collect().await.unwrap().to_bytes();
    let blobs = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
    assert!(blobs["blobs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn report_burns_queue() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let read = "burn-me";
    let delivery = queue_delivery_token(read);
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/burnq")
                .header(auth(&delivery).0, auth(&delivery).1)
                .body(Body::from(b"x".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    // Burning needs the read token; a peer holding only the delivery token
    // cannot destroy the inbox.
    let rep = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/q/burnq/report")
                .header(auth(read).0, auth(read).1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rep.status(), StatusCode::NO_CONTENT);
    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/q/burnq")
                .header(auth(&delivery).0, auth(&delivery).1)
                .body(Body::from(b"y".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::FORBIDDEN);
}

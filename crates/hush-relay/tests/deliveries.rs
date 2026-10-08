//! Outsider deliveries on the relay: opaque containers under a random
//! capability id, with no owner and no authorization.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hush_relay::{RelayConfig, router};
use http_body_util::BodyExt;
use tempfile::NamedTempFile;
use tower::ServiceExt;

fn temp_db() -> NamedTempFile {
    NamedTempFile::new().unwrap()
}

async fn app(config: RelayConfig, db: &NamedTempFile) -> axum::Router {
    router(db.path().to_str().unwrap(), config).unwrap()
}

const ID: &str = "0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn a_delivery_round_trips() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let body = vec![7u8; 512];

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/drop/{ID}"))
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::NO_CONTENT);

    let got = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/drop/{ID}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(got.status(), StatusCode::OK);
    let bytes = got.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.to_vec(), body);
}

#[tokio::test]
async fn fetching_the_same_delivery_twice_still_works() {
    // A mistyped PIN, a refresh, or a second device must not destroy the
    // message: expiry, not a fetch count, is what bounds storage.
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    for _ in 0..2 {
        let put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/drop/{ID}"))
                    .body(Body::from(vec![1u8, 2, 3]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::NO_CONTENT);
    }

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/drop/{ID}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/drop/{ID}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK, "a second open must still work");
}

#[tokio::test]
async fn an_unknown_id_is_not_found_and_a_malformed_one_is_rejected() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/drop/ffffffffffffffffffffffffffffffff")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // Not 32 hex characters: refused before it reaches the store.
    for bad in ["short", "0123456789abcdef0123456789abcdeg", &"z".repeat(32)] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/drop/{bad}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad:?}");
    }
}

#[tokio::test]
async fn an_oversize_delivery_is_refused() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let too_big = vec![0u8; hush_core::MAX_DROP_BYTES + 1];
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/drop/{ID}"))
                .body(Body::from(too_big))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn an_expired_delivery_is_gone() {
    let db = temp_db();
    let config = RelayConfig::for_tests(); // 1s TTL
    let app = app(config, &db).await;

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/drop/{ID}"))
                .body(Body::from(vec![9u8; 16]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::NO_CONTENT);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/drop/{ID}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

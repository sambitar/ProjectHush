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

/// A rendezvous id is 32 lowercase hex characters — the whole shape the relay
/// may assume about a drop id.
fn valid_id() -> String {
    "ab3f".repeat(8)
}

async fn put_drop(
    app: &axum::Router,
    id: &str,
    body: &[u8],
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/drop/{id}"))
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_drop(app: &axum::Router, id: &str) -> (StatusCode, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/drop/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body)
}

#[tokio::test]
async fn a_drop_round_trips_and_is_bounded() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let body = vec![7u8; 128];

    // No bearer token: the id is the capability, the body is ciphertext.
    assert_eq!(
        put_drop(&app, &valid_id(), &body).await.status(),
        StatusCode::NO_CONTENT
    );
    let (status, got) = get_drop(&app, &valid_id()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got, body);

    // The member consumes after a successful open: the slot is gone.
    let del = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/drop/{}", valid_id()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
    assert_eq!(get_drop(&app, &valid_id()).await.0, StatusCode::NOT_FOUND);

    // Oversize is refused.
    assert_eq!(
        put_drop(&app, &valid_id(), &vec![0u8; hush_core::MAX_DROP_BYTES + 1])
            .await
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // A non-rendezvous id is refused: 32 lowercase hex or nothing.
    assert_eq!(
        put_drop(&app, "short", &body).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        put_drop(&app, &"A".repeat(32), &body).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get_drop(&app, "nope").await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_drop_page_serves_static_with_a_closed_csp() {
    let db = temp_db();
    let app = app(RelayConfig::default(), &db).await;
    let res = app
        .oneshot(Request::builder().method("GET").uri("/drop").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let headers = res.headers();
    assert_eq!(
        headers["content-type"],
        "text/html; charset=utf-8"
    );
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'"), "closed by default: {csp}");
    assert!(!csp.contains("http"), "no external origins may be named: {csp}");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["cache-control"], "no-store");
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("hush-drop-pk-v1:"), "the page explains the key format");
    assert!(body.contains("PUT"), "the page uploads by PUT");
}

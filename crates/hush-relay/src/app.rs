use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use serde::Serialize;

use crate::config::RelayConfig;
use crate::error::RelayError;
use crate::store::{BlobMeta, Store};

pub struct AppState {
    pub store: Store,
}

#[derive(Serialize)]
struct PutResponse {
    id: String,
}

#[derive(Serialize)]
struct ListResponse {
    blobs: Vec<BlobMeta>,
}

#[derive(Serialize)]
struct PresenceResponse {
    last_at: u64,
    online: bool,
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, RelayError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(RelayError::Unauthorized)?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or(RelayError::Unauthorized)?;
    if token.is_empty() {
        return Err(RelayError::Unauthorized);
    }
    Ok(token)
}

async fn put_blob(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PutResponse>, RelayError> {
    let token = bearer_token(&headers)?;
    let id = state.store.put_blob(&queue_id, token, &body)?;
    Ok(Json(PutResponse { id }))
}

async fn list_blobs(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ListResponse>, RelayError> {
    let token = bearer_token(&headers)?;
    let blobs = state.store.list_blobs(&queue_id, token)?;
    Ok(Json(ListResponse { blobs }))
}

async fn get_blob(
    State(state): State<Arc<AppState>>,
    Path((queue_id, blob_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Vec<u8>, RelayError> {
    let token = bearer_token(&headers)?;
    let body = state.store.get_blob(&queue_id, token, &blob_id)?;
    Ok(body)
}

async fn delete_blob(
    State(state): State<Arc<AppState>>,
    Path((queue_id, blob_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.delete_blob(&queue_id, token, &blob_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn report_queue(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.report_queue(&queue_id, token)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn touch_presence(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.touch_presence(&queue_id, token)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_presence(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<PresenceResponse>, RelayError> {
    let token = bearer_token(&headers)?;
    let last_at = state.store.presence_last_at(&queue_id, token)?;
    let online = presence_is_live(last_at);
    Ok(Json(PresenceResponse { last_at, online }))
}

fn presence_is_live(last_at: u64) -> bool {
    if last_at == 0 {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs();
    now.saturating_sub(last_at) <= 90
}

async fn health() -> StatusCode {
    StatusCode::OK
}

/// Store this account's signed device list.
///
/// No bearer credential: the blob must carry a signature by device 1 that
/// verifies against the account id, and that is what authorizes the write. A
/// revoked device cannot produce one.
async fn put_device_dir(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, RelayError> {
    state.store.put_device_dir(&account_id, &body)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Read an account's device list.
///
/// Unauthenticated on purpose: a peer fetching a contact's list holds no
/// credential for that account and does not need one, because the signature is
/// what makes the contents trustworthy. Access control here would buy nothing
/// while making the list unreadable by the only parties that need it. The
/// account id is an identity key hash, so the directory is not enumerable
/// without already knowing the account.
async fn get_device_dir(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Result<Vec<u8>, RelayError> {
    state.store.get_device_dir(&account_id)
}

pub fn router(db_path: &str, config: RelayConfig) -> Result<Router, RelayError> {
    let store = Store::open(db_path, config)?;
    let state = Arc::new(AppState { store });
    Ok(Router::new()
        .route("/health", get(health))
        .route("/d/{account_id}", put(put_device_dir).get(get_device_dir))
        .route("/q/{queue_id}", put(put_blob).get(list_blobs))
        .route(
            "/q/{queue_id}/presence",
            put(touch_presence).get(get_presence),
        )
        .route("/q/{queue_id}/report", post(report_queue))
        .route("/q/{queue_id}/{blob_id}", get(get_blob).delete(delete_blob))
        .with_state(state))
}

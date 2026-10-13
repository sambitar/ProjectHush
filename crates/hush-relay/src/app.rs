use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::{get, post, put},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};

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

#[derive(Serialize)]
struct BulletinPutResponse {
    seq: i64,
}

#[derive(Serialize)]
struct BulletinListResponse {
    blobs: Vec<BulletinEntryResponse>,
}

/// A drained blob, base64 so the JSON body can carry ciphertext.
#[derive(Serialize)]
struct BulletinEntryResponse {
    seq: i64,
    body: String,
    inserted_at: i64,
}

#[derive(Deserialize)]
struct BulletinQuery {
    #[serde(default)]
    since: Option<i64>,
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
) -> Result<Response, RelayError> {
    let token = bearer_token(&headers)?;
    let body = state.store.get_blob(&queue_id, &token, &blob_id)?;
    // A queue blob is ciphertext and opaque to the relay; never let a client
    // sniff it as something renderable.
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

async fn delete_blob(
    State(state): State<Arc<AppState>>,
    Path((queue_id, blob_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.delete_blob(&queue_id, &token, &blob_id)?;
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

/// Clear a queue's heartbeat, so peers see it offline immediately. The owner's
/// own "appear offline" switch drives this; a peer cannot call it (it holds the
/// delivery token, not the read token).
async fn clear_presence(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.clear_presence(&queue_id, token)?;
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

/// Publish an account's prekey record.
///
/// No bearer credential: the record is signed by the account's own identity
/// private key, and the relay checks that signature against the identity key the
/// account id names. That is strictly stronger than a token, because knowing a
/// public account id does not let anyone replace that account's prekeys.
async fn put_prekey_record(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, RelayError> {
    state.store.put_prekey_record(&account_id, &body)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fetch an account's prekey record, consuming one one-time prekey.
///
/// Unauthenticated on purpose: a peer starting a session holds no credential for
/// the account, and the record is self-authenticating. The response is the
/// record with at most one one-time prekey left in it, which the peer uses and
/// discards.
async fn get_prekey_record(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Result<Vec<u8>, RelayError> {
    state.store.take_prekey_record(&account_id)
}

/// Report a queue as burned: future deliveries to it are refused and the stored
/// blobs are gone. Authorized with the read token — whoever can read a queue can
/// burn it; that is the model.
async fn report_queue(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.report_queue(&queue_id, token)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The drop page: a static, committed bundle. It seals client-side and uploads
/// once — the relay serves bytes it compiled in and never learns anything.
///
/// The lock-down headers are the point of serving it from here: no external
/// origin may be fetched, nothing is cached or referrer-leaked, and the body is
/// marked un-sniffable.
const DROP_PAGE: &str = include_str!("../assets/drop/page.built.html");

async fn drop_page() -> Response {
    let mut response = Response::new(Body::from(DROP_PAGE));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             connect-src 'self'; base-uri 'none'; form-action 'none'",
        ),
    );
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// Park a sealed drop under its rendezvous id.
///
/// No bearer token on purpose: the id is the capability, the body is already
/// ciphertext, and the dropper holds nothing else. The store rate-limits per id
/// and refuses oversize bodies; the id shape is validated there too.
async fn put_drop(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, RelayError> {
    state.store.put_drop(&id, &body)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fetch a sealed drop for draining. Non-consuming by design — the member
/// deletes only after the container opened and filed — so a crashed drain
/// never loses a drop.
async fn get_drop(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let Some(body) = state.store.get_drop(&id)? else {
        return Err(RelayError::NotFound);
    };
    // Opaque ciphertext, like a queue blob: never renderable, never sniffed.
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

/// Consume a drained drop. Idempotent on purpose: a second device draining the
/// same rendezvous slot must not error because the first got there first.
async fn delete_drop(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, RelayError> {
    state.store.delete_drop(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Publish one opaque blob to the bulletin.
///
/// The path names a queue, but only so the relay can authorize and rate-limit:
/// the row it writes carries no queue id, so the relay cannot say afterwards
/// which member published what. This is the one ability the relay gained for
/// zines — a flat log any member may append to and every member drains — and it
/// reuses the credential pair every other route uses, with the private half.
async fn put_bulletin(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<BulletinPutResponse>, RelayError> {
    let token = bearer_token(&headers)?;
    let seq = state.store.put_bulletin(&queue_id, token, &body)?;
    Ok(Json(BulletinPutResponse { seq }))
}

/// Drain the bulletin: everything after `since`, oldest first.
///
/// Non-consuming, unlike a queue: the log belongs to everybody, so a drain
/// removes nothing and a member re-reads only what its own cursor has not seen.
async fn get_bulletin(
    State(state): State<Arc<AppState>>,
    Path(queue_id): Path<String>,
    Query(query): Query<BulletinQuery>,
    headers: HeaderMap,
) -> Result<Json<BulletinListResponse>, RelayError> {
    let token = bearer_token(&headers)?;
    let since = query.since.unwrap_or(0).max(0);
    let blobs = state.store.list_bulletin(&queue_id, token, since)?;
    Ok(Json(BulletinListResponse {
        blobs: blobs
            .into_iter()
            .map(|b| BulletinEntryResponse {
                seq: b.seq,
                body: B64.encode(&b.body),
                inserted_at: b.inserted_at,
            })
            .collect(),
    }))
}

/// The dumb queue relay: blob stores per queue, prekey records, presence.
///
/// The one outsider surface is the drop family: a sealed container parked by
/// rendezvous id (`/drop/{id}`), addressed to a published drop key the relay
/// never sees, plus the static page that seals it (`/drop`). Everything else
/// requires a queue token or is a signed prekey record.
///
/// The bulletin (`/b/{queue_id}`) is the second: an append-only log of opaque
/// blobs addressed to nobody. It is the deliberate exception to "the relay
/// gains no new abilities" — see `docs/THREAT_MODEL.md`, which states what it
/// costs.
pub fn router(db_path: &str, config: RelayConfig) -> Result<Router, RelayError> {
    let store = Store::open(db_path, config)?;
    let state = Arc::new(AppState { store });
    Ok(Router::new()
        .route("/health", get(health))
        .route("/drop", get(drop_page))
        .route(
            "/drop/{id}",
            put(put_drop).get(get_drop).delete(delete_drop),
        )
        .route(
            "/k/{account_id}",
            put(put_prekey_record).get(get_prekey_record),
        )
        .route("/q/{queue_id}", put(put_blob).get(list_blobs))
        .route(
            "/q/{queue_id}/presence",
            put(touch_presence)
                .get(get_presence)
                .delete(clear_presence),
        )
        .route("/q/{queue_id}/report", post(report_queue))
        .route("/q/{queue_id}/{blob_id}", get(get_blob).delete(delete_blob))
        .route("/b/{queue_id}", put(put_bulletin).get(get_bulletin))
        .with_state(state))
}

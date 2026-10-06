mod account_worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use account_worker::{AccountWorker, WorkResult};
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use base64::{engine::general_purpose::STANDARD, Engine};
use hush_core::{ChatRecord, Contact, MessagePayload, ReceivedMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

const AUTO_LOCK: Duration = Duration::from_secs(15 * 60);

#[derive(Parser)]
#[command(name = "hush-daemon")]
struct Args {
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    #[arg(long, env = "HUSH_DAEMON_ADDR", default_value = "127.0.0.1:9876")]
    addr: String,
}

struct App {
    dir: PathBuf,
    worker: AccountWorker,
    unlocked: Mutex<bool>,
    unlocked_at: Mutex<Option<Instant>>,
}

#[derive(Serialize)]
struct StatusResponse {
    locked: bool,
    vault_dir: String,
    protocol_name: Option<String>,
    vault_bytes: u64,
    contacts: usize,
}

#[derive(Deserialize)]
struct UnlockRequest {
    passphrase: String,
}

#[derive(Deserialize)]
struct AddContactRequest {
    nickname: String,
    invite: String,
}

#[derive(Deserialize)]
struct SendTextRequest {
    to: String,
    text: String,
}

#[derive(Deserialize)]
struct SendFileRequest {
    to: String,
    path: String,
}

#[derive(Serialize)]
struct InviteResponse {
    invite: String,
}

#[derive(Serialize)]
struct ContactDto {
    nickname: String,
    protocol_name: String,
    /// They have added your invite (we received from them at least once).
    inbound_ready: bool,
}

#[derive(Serialize)]
struct ApiError {
    error: String,
}

#[derive(Deserialize)]
struct SendFileDataRequest {
    to: String,
    name: String,
    data_base64: String,
}

#[derive(Serialize)]
struct ReceivedDto {
    from: String,
    kind: String,
    text: Option<String>,
    file_name: Option<String>,
    file_size: Option<u64>,
    blob_id: Option<String>,
}

#[derive(Serialize)]
struct BlobResponse {
    data_base64: String,
}

/// libsignal protocol futures are not `Send`; account ops run on a dedicated worker thread.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let app = App {
        dir: args.dir,
        worker: AccountWorker::spawn(),
        unlocked: Mutex::new(false),
        unlocked_at: Mutex::new(None),
    };
    let state = Arc::new(app);

    tokio::spawn(auto_lock_loop(state.clone()));

    let router = Router::new()
        .route("/api/status", get(status))
        .route("/api/unlock", post(unlock))
        .route("/api/lock", post(lock))
        .route("/api/invite", post(invite))
        .route("/api/contacts", get(contacts))
        .route("/api/contacts/add", post(add_contact))
        .route("/api/send", post(send_text))
        .route("/api/send-file", post(send_file))
        .route("/api/send-file-data", post(send_file_data))
        .route("/api/recv", post(recv))
        .route("/api/messages", get(messages))
        .route("/api/blob/{id}", get(blob))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let vault_dir = state.dir.display().to_string();
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    eprintln!("hush-daemon on http://{} vault={}", args.addr, vault_dir);
    axum::serve(listener, router).await?;
    Ok(())
}

async fn auto_lock_loop(state: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let unlocked = *state.unlocked.lock().await;
        if !unlocked {
            continue;
        }
        let at = *state.unlocked_at.lock().await;
        if let Some(at) = at {
            if at.elapsed() >= AUTO_LOCK {
                let _ = state.worker.lock().await;
                *state.unlocked.lock().await = false;
                *state.unlocked_at.lock().await = None;
            }
        }
    }
}

async fn touch(state: &App) {
    *state.unlocked_at.lock().await = Some(Instant::now());
}

fn map_err(result: WorkResult) -> Result<WorkResult, (StatusCode, Json<ApiError>)> {
    match result {
        WorkResult::Err(msg) if msg == "locked" => {
            Err((StatusCode::UNAUTHORIZED, Json(ApiError { error: msg })))
        }
        WorkResult::Err(msg) => Err((StatusCode::BAD_REQUEST, Json(ApiError { error: msg }))),
        other => Ok(other),
    }
}

async fn status(State(state): State<Arc<App>>) -> Json<StatusResponse> {
    let locked = !*state.unlocked.lock().await;
    let (protocol_name, vault_bytes, contacts) = if locked {
        (None, 0, 0)
    } else {
        match state.worker.meta().await {
            WorkResult::Meta {
                protocol_name,
                vault_bytes,
                contacts,
            } => (Some(protocol_name), vault_bytes, contacts),
            _ => (None, 0, 0),
        }
    };
    Json(StatusResponse {
        locked,
        vault_dir: state.dir.display().to_string(),
        protocol_name,
        vault_bytes,
        contacts,
    })
}

async fn unlock(
    State(state): State<Arc<App>>,
    Json(body): Json<UnlockRequest>,
) -> Result<StatusCode, StatusCode> {
    match state.worker.unlock(&state.dir, &body.passphrase).await {
        WorkResult::Ok => {
            *state.unlocked.lock().await = true;
            touch(&state).await;
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn lock(State(state): State<Arc<App>>) -> StatusCode {
    state.worker.lock().await;
    *state.unlocked.lock().await = false;
    *state.unlocked_at.lock().await = None;
    StatusCode::NO_CONTENT
}

async fn invite(
    State(state): State<Arc<App>>,
) -> Result<Json<InviteResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.invite().await)?;
    match result {
        WorkResult::Invite(invite) => Ok(Json(InviteResponse { invite })),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

async fn contacts(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<ContactDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.contacts().await)?;
    match result {
        WorkResult::Contacts(list) => Ok(Json(
            list.iter()
                .map(|c: &Contact| ContactDto {
                    nickname: c.nickname.clone(),
                    protocol_name: c.protocol_name.clone(),
                    inbound_ready: c.inbound_ready,
                })
                .collect(),
        )),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

async fn add_contact(
    State(state): State<Arc<App>>,
    Json(body): Json<AddContactRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.add_contact(&body.nickname, &body.invite).await)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_text(
    State(state): State<Arc<App>>,
    Json(body): Json<SendTextRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.send_text(&body.to, &body.text).await)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_file(
    State(state): State<Arc<App>>,
    Json(body): Json<SendFileRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .send_file(&body.to, &PathBuf::from(&body.path))
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_file_data(
    State(state): State<Arc<App>>,
    Json(body): Json<SendFileDataRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let data = STANDARD
        .decode(body.data_base64.trim())
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "invalid base64 file data".into(),
                }),
            )
        })?;
    map_err(
        state
            .worker
            .send_file_bytes(&body.to, &body.name, data)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct RecvResponse {
    messages: Vec<ReceivedDto>,
    warnings: Vec<String>,
}

async fn recv(
    State(state): State<Arc<App>>,
) -> Result<Json<RecvResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.recv().await)?;
    let inbox = match result {
        WorkResult::Inbox(inbox) => inbox,
        _ => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "unexpected worker response".into(),
                }),
            ))
        }
    };
    Ok(Json(RecvResponse {
        messages: inbox.messages.into_iter().map(to_dto).collect(),
        warnings: inbox.warnings,
    }))
}

fn to_dto(msg: ReceivedMessage) -> ReceivedDto {
    let from = msg.from_nick;
    match msg.payload {
        MessagePayload::Text { body } => ReceivedDto {
            from,
            kind: "text".into(),
            text: Some(body),
            file_name: None,
            file_size: None,
            blob_id: None,
        },
        MessagePayload::File { name, data } => ReceivedDto {
            from,
            kind: "file".into(),
            text: None,
            file_name: Some(name),
            file_size: Some(data.len() as u64),
            blob_id: msg.file_blob_id.map(|id| hex::encode(id.0)),
        },
    }
}

async fn messages(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<ChatRecord>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.messages().await)?;
    match result {
        WorkResult::Messages(list) => Ok(Json(list)),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

async fn blob(
    State(state): State<Arc<App>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<BlobResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.blob(&id).await)?;
    match result {
        WorkResult::Blob(data) => Ok(Json(BlobResponse {
            data_base64: STANDARD.encode(data),
        })),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

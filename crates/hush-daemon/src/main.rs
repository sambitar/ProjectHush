mod account_worker;
mod paths;
mod security;
mod suspend_lock;
mod ui_prefs;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use account_worker::{AccountWorker, ContactDto, WorkResult};
use axum::{
    extract::State,
    http::{header, Method, StatusCode},
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use base64::{engine::general_purpose::STANDARD, Engine};
use hush_core::{ChatRecord, MessagePayload, ReceivedMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::{AllowOrigin, CorsLayer};

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
    recovery_acknowledged: bool,
    last_backup_unix: u64,
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
struct RemoveContactRequest {
    nickname: String,
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

fn local_ui_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            origin.to_str().is_ok_and(|s| {
                s.starts_with("http://127.0.0.1:")
                    || s.starts_with("http://localhost:")
                    || s == "https://tauri.localhost"
                    || s == "http://tauri.localhost"
                    || s == "tauri://localhost"
            })
        }))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE])
}

async fn vault_mount_watch(state: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if !*state.unlocked.lock().await {
            continue;
        }
        let dir = state.dir.clone();
        let ok = tokio::task::spawn_blocking(move || dir.is_dir() && std::fs::metadata(&dir).is_ok())
            .await
            .unwrap_or(false);
        if !ok {
            force_lock(&state).await;
            eprintln!("hush-daemon: vault path unavailable — auto-locked");
        }
    }
}

pub(crate) async fn force_lock(state: &Arc<App>) {
    state.worker.lock().await;
    *state.unlocked.lock().await = false;
    *state.unlocked_at.lock().await = None;
}

/// libsignal protocol futures are not `Send`; account ops run on a dedicated worker thread.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    security::harden_process();
    let args = Args::parse();
    let vault_dir = paths::vault_dir_from_args(args.dir);
    let app = App {
        dir: vault_dir,
        worker: AccountWorker::spawn(),
        unlocked: Mutex::new(false),
        unlocked_at: Mutex::new(None),
    };
    let state = Arc::new(app);

    tokio::spawn(auto_lock_loop(state.clone()));
    tokio::spawn(vault_mount_watch(state.clone()));
    suspend_lock::spawn_suspend_lock_watch(state.clone());

    let router = Router::new()
        .route("/api/status", get(status))
        .route("/api/unlock", post(unlock))
        .route("/api/lock", post(lock))
        .route("/api/invite", post(invite))
        .route("/api/contacts", get(contacts))
        .route("/api/contacts/add", post(add_contact))
        .route("/api/contacts/remove", post(remove_contact))
        .route("/api/send", post(send_text))
        .route("/api/send-file", post(send_file))
        .route("/api/send-file-data", post(send_file_data))
        .route("/api/presence/heartbeat", post(presence_heartbeat))
        .route("/api/recv", post(recv))
        .route("/api/messages", get(messages))
        .route("/api/blob/{id}", get(blob))
        .route("/api/backup/export", post(backup_export))
        .route("/api/backup/import", post(backup_import))
        .route("/api/recovery/pending", get(recovery_pending))
        .route("/api/recovery/acknowledge", post(recovery_acknowledge))
        .route("/api/ui-prefs", get(get_ui_prefs).put(put_ui_prefs))
        .layer(local_ui_cors())
        .with_state(state.clone());

    let vault_dir = state.dir.display().to_string();
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    eprintln!(
        "hush-daemon on http://{} vault={} (set HUSH_DATA_DIR for default data root)",
        args.addr,
        vault_dir
    );
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
        let limit = ui_prefs::auto_lock_duration(&state.dir);
        if limit.is_none() {
            continue;
        }
        let at = *state.unlocked_at.lock().await;
        if let Some(at) = at {
            if at.elapsed() >= limit.unwrap() {
                force_lock(&state).await;
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
    let (protocol_name, vault_bytes, contacts, recovery_acknowledged, last_backup_unix) =
        if locked {
            (None, 0, 0, true, 0)
        } else {
            match state.worker.meta().await {
                WorkResult::Meta {
                    protocol_name,
                    vault_bytes,
                    contacts,
                    recovery_acknowledged,
                    last_backup_unix,
                } => (
                    Some(protocol_name),
                    vault_bytes,
                    contacts,
                    recovery_acknowledged,
                    last_backup_unix,
                ),
                _ => (None, 0, 0, true, 0),
            }
        };
    Json(StatusResponse {
        locked,
        vault_dir: state.dir.display().to_string(),
        protocol_name,
        vault_bytes,
        contacts,
        recovery_acknowledged,
        last_backup_unix,
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
        WorkResult::Contacts(list) => Ok(Json(list)),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

async fn presence_heartbeat(
    State(state): State<Arc<App>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.heartbeat_presence().await)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_contact(
    State(state): State<Arc<App>>,
    Json(body): Json<AddContactRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.add_contact(&body.nickname, &body.invite).await)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_contact(
    State(state): State<Arc<App>>,
    Json(body): Json<RemoveContactRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.remove_contact(&body.nickname).await)?;
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

#[derive(Serialize)]
struct SendFileDataResponse {
    blob_id: String,
}

async fn send_file_data(
    State(state): State<Arc<App>>,
    Json(body): Json<SendFileDataRequest>,
) -> Result<Json<SendFileDataResponse>, (StatusCode, Json<ApiError>)> {
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
    let result = map_err(
        state
            .worker
            .send_file_bytes(&body.to, &body.name, data)
            .await,
    )?;
    match result {
        WorkResult::BlobId(blob_id) => Ok(Json(SendFileDataResponse { blob_id })),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
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

#[derive(Serialize)]
struct BackupExportResponse {
    filename: String,
    data_base64: String,
}

#[derive(Deserialize)]
struct BackupImportRequest {
    data_base64: String,
}

#[derive(Serialize)]
struct RecoveryPendingResponse {
    code: String,
}

async fn backup_export(
    State(state): State<Arc<App>>,
) -> Result<Json<BackupExportResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.backup_export().await)?;
    match result {
        WorkResult::Backup(data) => {
            let stamp = chrono_lite_timestamp();
            Ok(Json(BackupExportResponse {
                filename: format!("hush-vault-backup-{}.tar", stamp),
                data_base64: STANDARD.encode(data),
            }))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

async fn backup_import(
    State(state): State<Arc<App>>,
    Json(body): Json<BackupImportRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if *state.unlocked.lock().await {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "lock vault before importing a backup".into(),
            }),
        ));
    }
    let data = STANDARD.decode(body.data_base64.trim()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid base64 backup".into(),
            }),
        )
    })?;
    map_err(state.worker.backup_import(&state.dir, data).await)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn recovery_pending(
    State(state): State<Arc<App>>,
) -> Result<Json<RecoveryPendingResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.recovery_pending().await)?;
    match result {
        WorkResult::RecoveryCode(code) => Ok(Json(RecoveryPendingResponse { code })),
        WorkResult::Err(msg) => Err((StatusCode::NOT_FOUND, Json(ApiError { error: msg }))),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

async fn recovery_acknowledge(
    State(state): State<Arc<App>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.recovery_ack().await)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_ui_prefs(State(state): State<Arc<App>>) -> Json<ui_prefs::UiPrefs> {
    Json(ui_prefs::load(&state.dir))
}

async fn put_ui_prefs(
    State(state): State<Arc<App>>,
    Json(body): Json<ui_prefs::UiPrefs>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if body.theme != "light" && body.theme != "dark" && body.theme != "system" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "theme must be light, dark, or system".into(),
            }),
        ));
    }
    if body.auto_lock_minutes != 0 && (body.auto_lock_minutes < 5 || body.auto_lock_minutes > 24 * 60) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "auto_lock_minutes must be 0 (off) or between 5 and 1440".into(),
            }),
        ));
    }
    ui_prefs::save(&state.dir, &body).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: e }),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

fn chrono_lite_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
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

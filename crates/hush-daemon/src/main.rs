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
    extract::{Path, State},
    http::{header, Method, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use clap::Parser;
use base64::{engine::general_purpose::STANDARD, Engine};
use hush_core::{ChatRecord, ConclaveRecord, MessagePayload, PendingInvite, ReceivedMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Parser)]
#[command(name = "hush-daemon")]
struct Args {
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// TCP bind address. TCP is off unless this, HUSH_DAEMON_ADDR, or
    /// client-config.json sets one; the Unix socket is always the default API.
    #[arg(long, env = "HUSH_DAEMON_ADDR")]
    addr: Option<String>,
    /// Unix socket path. Defaults to HUSH_DAEMON_SOCKET, then client-config.json,
    /// then `{runtime_dir}/hush-daemon.sock`.
    #[arg(long, env = "HUSH_DAEMON_SOCKET")]
    socket: Option<String>,
    /// Skip the Unix socket listener (TCP-only debugging).
    #[arg(long)]
    no_socket: bool,
}

struct App {
    dir: PathBuf,
    /// Unix socket the API is bound to; `None` when `--no-socket` (DR-02).
    socket: Option<PathBuf>,
    /// Resolved TCP bind address, if TCP was explicitly enabled.
    tcp_addr: Option<String>,
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
    /// Relay this vault actually uses; the resolved config relay while locked.
    relay: String,
    /// Where the relay value came from when it is not the vault's stored one.
    relay_source: String,
    /// Unix socket the daemon API is bound to (`null` when disabled).
    socket: Option<String>,
    /// TCP bind address, when the TCP listener is enabled.
    tcp_addr: Option<String>,
    /// This member's mail handle (local part only), if set.
    mail_handle: Option<String>,
    /// The vault's own name — the suffix of this member's mail address, so the
    /// UI never hardcodes it. The default stands in while locked.
    vault_name: String,
}

#[derive(Deserialize)]
struct UnlockRequest {
    passphrase: String,
}

#[derive(Deserialize)]
struct ChangePassphraseRequest {
    current_passphrase: String,
    new_passphrase: String,
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
    /// Mail only: the thread this message belongs to.
    thread_id: Option<String>,
    /// Mail only: the subject line, if the sender set one.
    subject: Option<String>,
    /// Mail only: attachment references (bytes are fetched via `/api/blob/{id}`).
    attachments: Vec<MailAttachmentDto>,
    /// Conclave only: the id the message's conclave is keyed on.
    conclave_id: Option<String>,
    /// Conclave only: the conclave's name, for materialising it on first sight.
    conclave_name: Option<String>,
    /// Conclave only: the sender's trustee nicknames, for rendering on first sight.
    conclave_trustees: Vec<String>,
}

#[derive(Serialize)]
struct MailAttachmentDto {
    name: String,
    size: u64,
    blob_id: String,
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
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
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
    let hardening = security::harden_process();
    let args = Args::parse();
    let vault_dir = paths::vault_dir_from_args(args.dir);
    let tcp_addr = hush_core::config::resolve_daemon_addr(&vault_dir, args.addr.as_deref());
    let socket = hush_core::config::resolve_daemon_socket(&vault_dir, args.socket.as_deref());
    if args.no_socket && tcp_addr.is_none() {
        return Err("--no-socket needs a TCP address (--addr / HUSH_DAEMON_ADDR)".into());
    }
    let app = App {
        dir: vault_dir,
        socket: if args.no_socket {
            None
        } else {
            Some(socket.clone())
        },
        tcp_addr: tcp_addr.clone(),
        worker: AccountWorker::spawn(),
        unlocked: Mutex::new(false),
        unlocked_at: Mutex::new(None),
    };
    let state = Arc::new(app);

    tokio::spawn(auto_lock_loop(state.clone()));
    tokio::spawn(vault_mount_watch(state.clone()));
    tokio::spawn(outbox_loop(state.clone()));
    suspend_lock::spawn_suspend_lock_watch(state.clone());

    let router = Router::new()
        .route("/api/status", get(status))
        .route("/api/unlock", post(unlock))
        .route("/api/lock", post(lock))
        .route("/api/change-passphrase", post(change_passphrase))
        .route("/api/mail/handle", post(set_mail_handle))
        .route("/api/mail/send", post(send_mail))
        .route("/api/mail/draft", post(save_draft))
        .route("/api/mail/draft/{id}", delete(delete_draft))
        .route("/api/mail/{id}", delete(delete_mail))
        .route("/api/mail", get(mail))
        .route("/api/conclaves", get(conclaves).post(create_conclave))
        .route("/api/conclaves/message", post(send_conclave_message))
        .route("/api/conclaves/file", post(send_conclave_file))
        .route("/api/conclaves/trustees/invite", post(invite_conclave_trustee))
        .route("/api/conclaves/trustees/kick", post(kick_conclave_trustee))
        .route("/api/conclaves/trustees/accept", post(accept_conclave_invite))
        .route("/api/conclaves/trustees/decline", post(decline_conclave_invite))
        .route("/api/conclaves/trustees/leave", post(leave_conclave))
        .route("/api/conclaves/invites", get(pending_invites))
        .route("/api/conclaves/end", post(end_conclave))
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

    let socket_listener = if args.no_socket {
        None
    } else {
        Some(bind_socket(&socket)?)
    };
    let tcp_listener = match tcp_addr.as_deref() {
        Some(addr) => Some(tokio::net::TcpListener::bind(addr).await?),
        None => None,
    };
    let shutdown_rx = spawn_shutdown_watch();

    eprintln!(
        "hush-daemon vault={} socket={}{}",
        state.dir.display(),
        if args.no_socket {
            "(disabled)".to_string()
        } else {
            socket.display().to_string()
        },
        match &tcp_addr {
            Some(a) => format!(" tcp=http://{}", a),
            None => String::new(),
        }
    );
    eprintln!("hush-daemon hardening: {}", hardening.summary());

    match (socket_listener, tcp_listener) {
        (Some(unix), Some(tcp)) => {
            let tcp_router = router.clone();
            let mut tcp_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                let res = axum::serve(tcp, tcp_router)
                    .with_graceful_shutdown(async move {
                        let _ = tcp_rx.changed().await;
                    })
                    .await;
                if let Err(e) = res {
                    eprintln!("hush-daemon: tcp listener stopped: {e}");
                }
            });
            let mut unix_rx = shutdown_rx.clone();
            axum::serve(unix, router)
                .with_graceful_shutdown(async move {
                    let _ = unix_rx.changed().await;
                })
                .await?;
        }
        (Some(unix), None) => {
            let mut unix_rx = shutdown_rx.clone();
            axum::serve(unix, router)
                .with_graceful_shutdown(async move {
                    let _ = unix_rx.changed().await;
                })
                .await?;
        }
        (None, Some(tcp)) => {
            let mut tcp_rx = shutdown_rx.clone();
            axum::serve(tcp, router)
                .with_graceful_shutdown(async move {
                    let _ = tcp_rx.changed().await;
                })
                .await?;
        }
        (None, None) => return Err("no listener configured".into()),
    }

    if !args.no_socket {
        let _ = std::fs::remove_file(&socket);
    }
    Ok(())
}

/// Resolve on SIGINT or SIGTERM so the socket file is removed, not leaked.
fn spawn_shutdown_watch() -> tokio::sync::watch::Receiver<bool> {
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut term) = signal(SignalKind::terminate()) {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = tx.send(true);
    });
    rx
}

/// Create the runtime dir, clear a stale socket, and bind with `0600`.
///
/// A socket that still accepts connections belongs to a live daemon: we refuse
/// rather than unlink it out from under that process.
fn bind_socket(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) if is_socket_file(path) => {
            if socket_is_live(path) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "another hush-daemon is already listening on {} (use --socket to pick a different path)",
                        path.display()
                    ),
                ));
            }
            std::fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

fn is_socket_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_socket())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// A socket that still accepts connections belongs to a running daemon.
#[cfg(unix)]
fn socket_is_live(path: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn socket_is_live(_path: &std::path::Path) -> bool {
    false
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

/// Flush the Outbox on a timer while the vault is unlocked.
///
/// Queued mail is the one piece of work with no user action behind it: the
/// message is already written, and the only thing missing is a relay that will
/// take it. Retrying here means a message composed while the relay was down goes
/// out on its own. A tick with nothing queued touches no network at all.
async fn outbox_loop(state: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        if !*state.unlocked.lock().await {
            continue;
        }
        let _ = state.worker.retry_outbox().await;
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
    let (
        protocol_name,
        vault_bytes,
        contacts,
        recovery_acknowledged,
        last_backup_unix,
        relay,
        mail_handle,
        vault_name,
    ) = if locked {
        (None, 0, 0, true, 0, None, None, None)
    } else {
        match state.worker.meta().await {
            WorkResult::Meta {
                protocol_name,
                vault_bytes,
                contacts,
                recovery_acknowledged,
                last_backup_unix,
                relay,
                mail_handle,
                vault_name,
            } => (
                Some(protocol_name),
                vault_bytes,
                contacts,
                recovery_acknowledged,
                last_backup_unix,
                Some(relay),
                mail_handle,
                Some(vault_name),
            ),
            _ => (None, 0, 0, true, 0, None, None, None),
        }
    };
    let (relay, relay_source) = match relay {
        Some(stored) => (stored, "vault".to_string()),
        None => {
            let (value, source) =
                hush_core::config::resolve_relay_url_with_source(&state.dir, None);
            (value, source.as_str().to_string())
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
        relay,
        relay_source,
        socket: state.socket.as_ref().map(|p| p.display().to_string()),
        tcp_addr: state.tcp_addr.clone(),
        mail_handle,
        vault_name: vault_name.unwrap_or_else(|| hush_core::DEFAULT_VAULT_NAME.to_string()),
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

/// Rotate the vault passphrase.
///
/// Requires the vault to be unlocked: the current passphrase is what proves the
/// caller owns it, and the master key that gets re-wrapped only exists in memory
/// while unlocked. A wrong current passphrase is a 403 rather than a 401, so the
/// UI can tell "you typed the old passphrase wrong" apart from "you are locked
/// out and need to unlock first".
async fn change_passphrase(
    State(state): State<Arc<App>>,
    Json(body): Json<ChangePassphraseRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if !*state.unlocked.lock().await {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "locked".into(),
            }),
        ));
    }
    match state
        .worker
        .change_passphrase(&body.current_passphrase, &body.new_passphrase)
        .await
    {
        WorkResult::Ok => {
            touch(&state).await;
            Ok(StatusCode::NO_CONTENT)
        }
        WorkResult::WrongPassphrase => Err((
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: "current passphrase is incorrect".into(),
            }),
        )),
        WorkResult::Err(message) => {
            Err((StatusCode::BAD_REQUEST, Json(ApiError { error: message })))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

#[derive(Deserialize)]
struct SetMailHandleRequest {
    handle: String,
}

async fn set_mail_handle(
    State(state): State<Arc<App>>,
    Json(body): Json<SetMailHandleRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.set_mail_handle(&body.handle).await)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MailAttachmentInput {
    name: String,
    data_base64: String,
}

#[derive(Deserialize)]
struct SendMailRequest {
    to: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    body: String,
    #[serde(default)]
    attachments: Vec<MailAttachmentInput>,
}

#[derive(Serialize)]
struct SendMailResponse {
    thread_id: String,
    /// True when the relay refused it and the message is in the Outbox.
    queued: bool,
}

async fn send_mail(
    State(state): State<Arc<App>>,
    Json(body): Json<SendMailRequest>,
) -> Result<Json<SendMailResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let attachments = decode_attachments(&body.attachments)?;
    // An all-whitespace subject is no subject at all.
    let subject = body
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let result = map_err(
        state
            .worker
            .send_mail(
                &body.to,
                body.thread_id.clone(),
                subject,
                &body.body,
                attachments,
            )
            .await,
    )?;
    match result {
        WorkResult::MailSent { thread_id, queued } => {
            Ok(Json(SendMailResponse { thread_id, queued }))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

/// The Mail rail's whole mailbox. Messages and drafts come back together because
/// every caller wants both, and two round trips would only be two chances for
/// the two halves to disagree.
#[derive(Serialize)]
struct MailboxResponse {
    mail: Vec<ChatRecord>,
    drafts: Vec<ChatRecord>,
}

async fn mail(
    State(state): State<Arc<App>>,
) -> Result<Json<MailboxResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mail = match map_err(state.worker.mail_records().await)? {
        WorkResult::Mail(list) => list,
        _ => return Err(unexpected_worker_response()),
    };
    let drafts = match map_err(state.worker.draft_records().await)? {
        WorkResult::Drafts(list) => list,
        _ => return Err(unexpected_worker_response()),
    };
    Ok(Json(MailboxResponse { mail, drafts }))
}

#[derive(Deserialize)]
struct CreateConclaveRequest {
    name: String,
    /// The contacts to invite. `alias` accepts the pre-rename `members` key.
    #[serde(alias = "members")]
    trustees: Vec<String>,
}

#[derive(Deserialize)]
struct SendConclaveMessageRequest {
    conclave_id: String,
    body: String,
}

#[derive(Deserialize)]
struct SendConclaveFileRequest {
    conclave_id: String,
    name: String,
    data_base64: String,
}

#[derive(Deserialize)]
struct ConclaveTrusteeRequest {
    conclave_id: String,
    nickname: String,
}

#[derive(Deserialize)]
struct ConclaveIdRequest {
    conclave_id: String,
}

/// Every conclave this vault holds, each with its own message log.
async fn conclaves(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<ConclaveRecord>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.conclaves().await)?;
    match result {
        WorkResult::Conclaves(list) => Ok(Json(list)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn create_conclave(
    State(state): State<Arc<App>>,
    Json(body): Json<CreateConclaveRequest>,
) -> Result<Json<ConclaveRecord>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.create_conclave(&body.name, body.trustees).await)?;
    match result {
        WorkResult::ConclaveCreated(conclave) => Ok(Json(conclave)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn send_conclave_message(
    State(state): State<Arc<App>>,
    Json(body): Json<SendConclaveMessageRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .send_conclave_message(&body.conclave_id, &body.body)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_conclave_file(
    State(state): State<Arc<App>>,
    Json(body): Json<SendConclaveFileRequest>,
) -> Result<Json<SendFileDataResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let data = STANDARD.decode(body.data_base64.trim()).map_err(|_| {
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
            .send_conclave_file_bytes(&body.conclave_id, &body.name, data)
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

async fn invite_conclave_trustee(
    State(state): State<Arc<App>>,
    Json(body): Json<ConclaveTrusteeRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .invite_conclave_trustee(&body.conclave_id, &body.nickname)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn kick_conclave_trustee(
    State(state): State<Arc<App>>,
    Json(body): Json<ConclaveTrusteeRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .kick_conclave_trustee(&body.conclave_id, &body.nickname)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn accept_conclave_invite(
    State(state): State<Arc<App>>,
    Json(body): Json<ConclaveIdRequest>,
) -> Result<Json<ConclaveRecord>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.accept_conclave_invite(&body.conclave_id).await)?;
    match result {
        WorkResult::ConclaveAccepted(conclave) => Ok(Json(conclave)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn decline_conclave_invite(
    State(state): State<Arc<App>>,
    Json(body): Json<ConclaveIdRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .decline_conclave_invite(&body.conclave_id)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave_conclave(
    State(state): State<Arc<App>>,
    Json(body): Json<ConclaveIdRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.leave_conclave(&body.conclave_id).await)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Invitations this vault has received but not yet accepted or declined.
async fn pending_invites(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<PendingInvite>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.pending_invites().await)?;
    match result {
        WorkResult::PendingInvites(list) => Ok(Json(list)),
        _ => Err(unexpected_worker_response()),
    }
}

/// End a conclave for everyone: the convener broadcasts an `ended` descriptor so
/// the conclave becomes read-only on every peer, not only locally.
async fn end_conclave(
    State(state): State<Arc<App>>,
    Json(body): Json<ConclaveIdRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.end_conclave(&body.conclave_id).await)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SaveDraftRequest {
    /// Set to edit an existing draft; omitted to create one.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    to: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    attachments: Vec<MailAttachmentInput>,
}

#[derive(Serialize)]
struct SaveDraftResponse {
    id: String,
}

async fn save_draft(
    State(state): State<Arc<App>>,
    Json(body): Json<SaveDraftRequest>,
) -> Result<Json<SaveDraftResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let attachments = decode_attachments(&body.attachments)?;
    let subject = body
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let result = map_err(
        state
            .worker
            .save_draft(
                body.id,
                body.thread_id,
                body.to.trim(),
                subject,
                &body.body,
                attachments,
            )
            .await,
    )?;
    match result {
        WorkResult::DraftSaved(id) => Ok(Json(SaveDraftResponse { id })),
        _ => Err(unexpected_worker_response()),
    }
}

async fn delete_draft(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.delete_draft(&id).await)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Drop a mail record from the log. This is "delete forever" from Trash, not the
/// trash flag — the flag lives in `ui-prefs`.
async fn delete_mail(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.delete_mail(&id).await)?;
    Ok(StatusCode::NO_CONTENT)
}

/// What a handler returns next to a non-2xx status: the code and the JSON body.
type ApiErrorResponse = (StatusCode, Json<ApiError>);

fn decode_attachments(
    attachments: &[MailAttachmentInput],
) -> Result<Vec<(String, Vec<u8>)>, ApiErrorResponse> {
    let mut out = Vec::with_capacity(attachments.len());
    for a in attachments {
        let data = STANDARD.decode(a.data_base64.trim()).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: format!("invalid base64 for attachment {:?}", a.name),
                }),
            )
        })?;
        out.push((a.name.clone(), data));
    }
    Ok(out)
}

fn unexpected_worker_response() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "unexpected worker response".into(),
        }),
    )
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
        messages: inbox.messages.into_iter().filter_map(to_dto).collect(),
        warnings: inbox.warnings,
    }))
}

/// `None` for an own-copy envelope: `recv` files those into the chat log itself
/// and never surfaces them, so one reaching here would be a bug rather than a
/// message to show.
fn to_dto(msg: ReceivedMessage) -> Option<ReceivedDto> {
    let from = msg.from_nick;
    let mail_attachments = msg.mail_attachments;
    match msg.payload {
        MessagePayload::Text { body } => Some(ReceivedDto {
            from,
            kind: "text".into(),
            text: Some(body),
            file_name: None,
            file_size: None,
            blob_id: None,
            thread_id: None,
            subject: None,
            attachments: Vec::new(),
            conclave_id: None,
            conclave_name: None,
            conclave_trustees: Vec::new(),
        }),
        MessagePayload::File { name, data } => Some(ReceivedDto {
            from,
            kind: "file".into(),
            text: None,
            file_name: Some(name),
            file_size: Some(data.len() as u64),
            blob_id: msg.file_blob_id.map(|id| hex::encode(id.0)),
            thread_id: None,
            subject: None,
            attachments: Vec::new(),
            conclave_id: None,
            conclave_name: None,
            conclave_trustees: Vec::new(),
        }),
        MessagePayload::Mail {
            thread_id,
            subject,
            body,
            ..
        } => Some(ReceivedDto {
            from,
            kind: "mail".into(),
            text: Some(body),
            file_name: None,
            file_size: None,
            blob_id: None,
            thread_id: Some(thread_id),
            subject,
            attachments: mail_attachments
                .into_iter()
                .map(|a| MailAttachmentDto {
                    name: a.name,
                    size: a.size,
                    blob_id: hex::encode(a.blob_id.0),
                })
                .collect(),
            conclave_id: None,
            conclave_name: None,
            conclave_trustees: Vec::new(),
        }),
        MessagePayload::Conclave {
            conclave_id,
            name,
            trustees,
            body,
            ..
        } => Some(ReceivedDto {
            from,
            kind: "conclave".into(),
            text: Some(body),
            file_name: None,
            file_size: None,
            blob_id: None,
            thread_id: None,
            subject: None,
            attachments: Vec::new(),
            conclave_id: Some(conclave_id),
            conclave_name: Some(name),
            conclave_trustees: trustees,
        }),
        // Control payloads are handled inside `recv` and never surface as inbox
        // items.
        MessagePayload::QueueGrant { .. }
        | MessagePayload::ConclaveInvite { .. }
        | MessagePayload::ConclaveAccept { .. }
        | MessagePayload::ConclaveDecline { .. }
        | MessagePayload::ConclaveLeave { .. }
        | MessagePayload::ConclaveUpdate { .. }
        | MessagePayload::ConclaveEnd { .. } => None,
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

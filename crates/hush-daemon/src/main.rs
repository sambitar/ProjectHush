mod account_worker;
mod paths;
mod reads;
mod reads_store;
mod security;
mod suspend_lock;
mod torbox;
mod torrents;
mod torrents_store;
mod ui_prefs;
mod wallet;

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use account_worker::{AccountWorker, ContactDto, WorkResult};
use axum::{
    extract::{Path, Query, State},
    http::{header, Method, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::Parser;
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
    /// The Reads pane's HN client: outbound fetch + reader extraction + cache.
    reads: reads::ReadsClient,
    /// The Torrents pane's TorBox pipeline: add → cache → download into vault.
    torrents: Arc<torrents::TorrentsManager>,
    /// The self-custody wallet: one per vault, sealed with the vault master key.
    wallet: Arc<wallet::WalletManager>,
    unlocked: Mutex<bool>,
    unlocked_at: Mutex<Option<Instant>>,
    /// The SOCKS proxy the desktop shell reported its Tor to be listening on,
    /// when the relay switch is on. Runtime-only: the shell posts it after
    /// starting Tor and the daemon hands it to the account's relay client.
    relay_socks: Mutex<Option<String>>,
    /// The SOCKS proxy the shell reported for the *wallet's* Tor.
    ///
    /// Separate from `relay_socks` because the two are not the same switch: the
    /// wallet may be on Tor while the relay is not (the UI offers that), and
    /// turning the relay off stops Tor only when nothing else needs it. The
    /// wallet keeps its route either way.
    wallet_socks: Mutex<Option<String>>,
    /// The last relay reachability probe: `(reachable, when)`. The UI's
    /// connection light is only honest if it reflects this rather than Tor's
    /// bootstrap phase, which stays "ready" long after the network is gone.
    relay_health: Mutex<Option<(bool, Instant)>>,
    /// Guards against stacking probes: the worker is a single-threaded actor,
    /// so overlapping probes would queue behind each other and starve real
    /// work (heartbeats, mail).
    relay_probe_inflight: AtomicBool,
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
    /// Whether the relay path is switched to Tor (onion address + SOCKS
    /// proxy). The prefs value while locked, the vault's applied value when
    /// unlocked.
    relay_tor: bool,
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
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE])
}

/// The daemon's whole HTTP surface, extracted so tests can exercise the real
/// routes against a worker without binding a socket.
fn api_router(state: Arc<App>) -> Router {
    // The wallet can move funds, so it is only reachable over the Unix socket
    // (DR-02) — never over the optional TCP listener.
    let wallet_routes = state.socket.is_some();

    let mut router = Router::new()
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
        .route("/api/drops/key", get(drops_key).post(drops_key_create))
        .route("/api/drops/rotate", post(drops_rotate))
        .route("/api/drops/drain", post(drops_drain))
        .route("/api/reads/feed", get(reads_feed))
        .route(
            "/api/reads/sources",
            get(reads_sources_list).post(reads_sources_add),
        )
        .route("/api/reads/sources/{id}", delete(reads_sources_remove))
        .route("/api/reads/story/{id}", get(reads_story))
        .route("/api/reads/reader", get(reads_reader))
        .route("/api/reads/state", get(reads_state))
        .route("/api/reads/saved", post(reads_save_toggle))
        .route("/api/reads/hidden", post(reads_hidden_toggle))
        .route("/api/reads/read", post(reads_mark_read))
        .route("/api/torrents/state", get(torrents_state))
        .route("/api/torrents/key", post(torrents_set_key))
        .route("/api/torrents/test", post(torrents_test_key))
        .route("/api/torrents/refresh", post(torrents_refresh))
        .route("/api/torrents/add", post(torrents_add))
        .route("/api/torrents/add-file", post(torrents_add_file))
        .route("/api/torrents/pause", post(torrents_pause))
        .route("/api/torrents/resume", post(torrents_resume))
        .route("/api/torrents/retry", post(torrents_retry))
        .route("/api/torrents/remove", post(torrents_remove))
        .route("/api/torrents/concurrency", post(torrents_concurrency))
        .route("/api/torrents/library/{id}", delete(torrents_delete_library))
        .route("/api/conclaves", get(conclaves).post(create_conclave))
        .route("/api/conclaves/message", post(send_conclave_message))
        .route("/api/conclaves/file", post(send_conclave_file))
        .route(
            "/api/conclaves/trustees/invite",
            post(invite_conclave_trustee),
        )
        .route("/api/conclaves/trustees/kick", post(kick_conclave_trustee))
        .route(
            "/api/conclaves/trustees/accept",
            post(accept_conclave_invite),
        )
        .route(
            "/api/conclaves/trustees/decline",
            post(decline_conclave_invite),
        )
        .route("/api/conclaves/trustees/leave", post(leave_conclave))
        .route("/api/conclaves/invites", get(pending_invites))
        .route("/api/conclaves/end", post(end_conclave))
        .route("/api/conclaves/{id}", delete(delete_conclave))
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
        .route("/api/blob/{id}/materialize", post(blob_materialize))
        .route("/api/backup/export", post(backup_export))
        .route("/api/backup/import", post(backup_import))
        .route("/api/recovery/pending", get(recovery_pending))
        .route("/api/recovery/acknowledge", post(recovery_acknowledge))
        .route("/api/relay", get(get_relay))
        .route("/api/relay/mode", post(put_relay_mode))
        .route("/api/relay/onion", post(put_relay_onion))
        .route("/api/ui-prefs", get(get_ui_prefs).put(put_ui_prefs));

    if wallet_routes {
        router = router
            .route("/api/wallet", get(wallet_state))
            .route("/api/wallet/create", post(wallet_create))
            .route("/api/wallet/restore", post(wallet_restore))
            .route("/api/wallet/address", post(wallet_address))
            .route("/api/wallet/sync", post(wallet_sync))
            .route("/api/wallet/send", post(wallet_send))
            .route("/api/wallet/mode", post(wallet_mode))
            .route("/api/wallet/proxy", post(wallet_proxy))
            .route("/api/wallet/token", post(wallet_token_add))
            .route("/api/wallet/token/remove", post(wallet_token_remove))
            .route("/api/wallet/wipe", post(wallet_wipe));
    }

    router.layer(local_ui_cors()).with_state(state)
}

async fn vault_mount_watch(state: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if !*state.unlocked.lock().await {
            continue;
        }
        let dir = state.dir.clone();
        let ok =
            tokio::task::spawn_blocking(move || dir.is_dir() && std::fs::metadata(&dir).is_ok())
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
    // One daemon per vault. Held for the process lifetime; the OS releases it
    // on exit, so a crash never leaves a stale lock.
    let _vault_lock = acquire_vault_lock(&vault_dir)?;
    let tcp_addr = hush_core::config::resolve_daemon_addr(&vault_dir, args.addr.as_deref());
    let socket = hush_core::config::resolve_daemon_socket(&vault_dir, args.socket.as_deref());
    if args.no_socket && tcp_addr.is_none() {
        return Err("--no-socket needs a TCP address (--addr / HUSH_DAEMON_ADDR)".into());
    }
    let reads_cache = vault_dir.join("reads-cache");
    let worker = AccountWorker::spawn();
    let torrents = Arc::new(torrents::TorrentsManager::new(
        worker.clone(),
        &vault_dir,
    ));
    let wallet = Arc::new(wallet::WalletManager::new(worker.clone(), &vault_dir));
    let app = App {
        dir: vault_dir,
        socket: if args.no_socket {
            None
        } else {
            Some(socket.clone())
        },
        tcp_addr: tcp_addr.clone(),
        worker,
        reads: reads::ReadsClient::new(&reads_cache),
        torrents: torrents.clone(),
        wallet: wallet.clone(),
        unlocked: Mutex::new(false),
        unlocked_at: Mutex::new(None),
        relay_socks: Mutex::new(None),
        wallet_socks: Mutex::new(None),
        relay_health: Mutex::new(None),
        relay_probe_inflight: AtomicBool::new(false),
    };
    let state = Arc::new(app);

    tokio::spawn(auto_lock_loop(state.clone()));
    tokio::spawn(vault_mount_watch(state.clone()));
    tokio::spawn(outbox_loop(state.clone()));
    tokio::spawn(heartbeat_loop(state.clone()));
    tokio::spawn(torrents.run());
    suspend_lock::spawn_suspend_lock_watch(state.clone());

    let router = api_router(state.clone());

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
            use tokio::signal::unix::{signal, SignalKind};
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

/// Take an exclusive, non-blocking lock on the vault directory.
///
/// The socket check in [`bind_socket`] only catches a peer that is *answering*
/// on the socket. A daemon that is alive but busy — for example blocked in a
/// long vault store, so it is not accepting — looks dead there, and a second
/// daemon will unlink the socket and take over. Both then run against one
/// vault: they share the state sidecar and the vault, so they clobber each
/// other's state and duplicate downloads. This lock closes that hole. The OS
/// releases it when the process exits, so a crash never leaves a stale lock.
#[cfg(target_os = "linux")]
fn acquire_vault_lock(dir: &std::path::Path) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::io::AsRawFd;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(".hush-daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "another hush-daemon is already running on {} ({err}); stop it before starting a new one",
            dir.display()
        ));
    }
    Ok(Some(file))
}

/// No-op on platforms without `flock`; the socket check still applies.
#[cfg(not(target_os = "linux"))]
fn acquire_vault_lock(_dir: &std::path::Path) -> Result<Option<std::fs::File>, String> {
    Ok(None)
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
/// Keep this vault's relay presence live while the vault is unlocked.
///
/// Presence is "heartbeat seen in the last 90 s" at the relay, and without this
/// loop the only heartbeats come from the UI's poll. A backgrounded or
/// throttled webview (an unfocused WebKitGTK window slows its timers) lets the
/// heartbeat go stale, the peer's presence check starts failing mid-chat, and
/// sends bounce with "offline" even though both people are at their desks. The
/// daemon owns the session, so it owns the heartbeat; the UI heartbeat stays
/// and is harmless.
async fn heartbeat_loop(state: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        if !*state.unlocked.lock().await {
            continue;
        }
        let result = state.worker.heartbeat_presence().await;
        // The heartbeat is a real round trip along the selected relay path —
        // its outcome is ground truth for the connection light, independent
        // of the poll-triggered probes. A "locked" error is not a network
        // fact; anything else is.
        match result {
            WorkResult::Ok => {
                *state.relay_health.lock().await = Some((true, Instant::now()));
            }
            WorkResult::Err(e) if e != "locked" => {
                *state.relay_health.lock().await = Some((false, Instant::now()));
            }
            _ => {}
        }
    }
}

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
        relay_tor,
    ) = if locked {
        (None, 0, 0, true, 0, None, None, None, None)
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
                relay_tor,
            } => (
                Some(protocol_name),
                vault_bytes,
                contacts,
                recovery_acknowledged,
                last_backup_unix,
                Some(relay),
                mail_handle,
                Some(vault_name),
                Some(relay_tor),
            ),
            _ => (None, 0, 0, true, 0, None, None, None, None),
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
    // The mode the user set. While unlocked the vault's applied value is the
    // truth; while locked the prefs value is all there is.
    let relay_tor = relay_tor.unwrap_or_else(|| ui_prefs::load(&state.dir).relay_tor);
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
        relay_tor,
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
            apply_relay_prefs(&state).await;
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Apply the relay switch the user set before unlocking.
///
/// While the vault is locked the switch lives in the (plaintext) prefs file —
/// the pre-login toggle reads and flips it there, and an onion address typed
/// on that screen is staged beside it. Unlock is the first moment the vault
/// can carry the mode, so it is applied here: traffic follows the switch from
/// the very first request. A failure to apply (no address on file for a
/// switch that is on) is not an unlock failure; the relay endpoints surface
/// the state and the fail-closed relay client holds the line.
/// Keep the wallet's own routing in step with the relay switch.
///
/// The wallet reads the switch fresh on every request (`relay_is_tor`), but the
/// BDK client's own mode is set only through `apply_relay`. A switch flipped on
/// a path that forgot to tell the wallet would leave the wallet talking to its
/// chain source in the clear while the UI said Tor — the one thing it must
/// never do. Every path that can change the switch calls this.
async fn sync_wallet_relay(state: &Arc<App>) {
    let relay_tor = ui_prefs::load(&state.dir).relay_tor;
    // The wallet's Tor is whichever proxy the shell reported for it; the relay
    // switch is the other source, and either names the same Tor process. When
    // neither is known the wallet is left without a route, which in Tor mode is
    // a refusal rather than a direct connection.
    let socks = match state.wallet_socks.lock().await.clone() {
        Some(socks) => Some(socks),
        None => state.relay_socks.lock().await.clone(),
    };
    state.wallet.apply_relay(relay_tor, socks).await;
}

async fn apply_relay_prefs(state: &Arc<App>) {
    let prefs = ui_prefs::load(&state.dir);
    let socks = state.relay_socks.lock().await.clone();
    let onion = prefs
        .relay_onion_staged
        .clone()
        .or_else(|| hush_core::config::resolve_onion_relay_url(&state.dir));
    let result = state
        .worker
        .relay_apply(prefs.relay_tor, onion, socks)
        .await;
    // The wallet follows the same switch, and is locked on while it is.
    sync_wallet_relay(state).await;
    // The path may have just changed; a reachability answer from the old path
    // would be a lie about the new one.
    *state.relay_health.lock().await = None;
    if prefs.relay_tor && matches!(result, WorkResult::Ok) {
        // The staged address now lives in the vault; the handoff is done.
        let mut prefs = ui_prefs::load(&state.dir);
        prefs.relay_onion_staged = None;
        let _ = ui_prefs::save(&state.dir, &prefs);
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
    /// Filed outsider drops — the Drops folder's contents. Kept out of `mail`
    /// so the ordinary inbox rendering never has to know the variant exists.
    drops: Vec<ChatRecord>,
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
    let drops = match map_err(state.worker.drop_records().await)? {
        WorkResult::Drops(list) => list,
        _ => return Err(unexpected_worker_response()),
    };
    Ok(Json(MailboxResponse {
        mail,
        drafts,
        drops,
    }))
}

#[derive(Serialize)]
struct DropsKeyResponse {
    /// Whether the account has a drop key yet.
    exists: bool,
    /// The pasteable public key, `hush-drop-pk-v1:…` — what a member publishes.
    /// `None` when no key exists yet.
    public_key: Option<String>,
    /// The drop page's URL on the vault's preferred relay base (onion when
    /// staged, clearnet otherwise). Published beside the key.
    page_url: Option<String>,
}

#[derive(Serialize)]
struct DropsDrainResponse {
    /// Rendezvous slots a container was found on this pass.
    hit_slots: Vec<String>,
    /// Containers left parked (unopenable), for the next pass to retry.
    remaining: usize,
}

/// `GET /api/drops/key` — the published drop key and page URL, or `exists:
/// false` while the account has none.
async fn drops_key(
    State(state): State<Arc<App>>,
) -> Result<Json<DropsKeyResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match map_err(state.worker.drop_key().await)? {
        WorkResult::DropKey {
            exists,
            public_key,
            page_url,
        } => Ok(Json(DropsKeyResponse {
            exists,
            public_key,
            page_url,
        })),
        _ => Err(unexpected_worker_response()),
    }
}

/// `POST /api/drops/key` — create the drop keypair. An error (400) when one
/// already exists: rotation is the deliberate act.
async fn drops_key_create(
    State(state): State<Arc<App>>,
) -> Result<Json<DropsKeyResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match map_err(state.worker.drop_key_create().await)? {
        WorkResult::DropKey {
            exists,
            public_key,
            page_url,
        } => Ok(Json(DropsKeyResponse {
            exists,
            public_key,
            page_url,
        })),
        _ => Err(unexpected_worker_response()),
    }
}

/// `POST /api/drops/rotate` — new drop key; the retired one keeps draining
/// until the relay's TTL expires the drops still in flight on it.
async fn drops_rotate(
    State(state): State<Arc<App>>,
) -> Result<Json<DropsKeyResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match map_err(state.worker.drop_key_rotate().await)? {
        WorkResult::DropKey {
            exists,
            public_key,
            page_url,
        } => Ok(Json(DropsKeyResponse {
            exists,
            public_key,
            page_url,
        })),
        _ => Err(unexpected_worker_response()),
    }
}

/// `POST /api/drops/drain` — one drain pass now. The worker also drains after
/// every poll of `/api/recv`; this endpoint lets the UI force one (the
/// Settings → Drops pane does, right after key generation).
async fn drops_drain(
    State(state): State<Arc<App>>,
) -> Result<Json<DropsDrainResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match map_err(state.worker.drain_drops().await)? {
        WorkResult::Drain(report) => Ok(Json(DropsDrainResponse {
            hit_slots: report.hit_slots,
            remaining: report.remaining,
        })),
        _ => Err(unexpected_worker_response()),
    }
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
    let result = map_err(
        state
            .worker
            .create_conclave(&body.name, body.trustees)
            .await,
    )?;
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

/// Delete an ended conclave and its message log from this vault. Local only:
/// the other trustees keep their own copies, and the vault blobs the shared
/// files referenced stay — there is no blob collection pass, the same trade
/// `DELETE /api/mail/{id}` makes for attachments.
async fn delete_conclave(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.delete_conclave(&id).await)?;
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

/// The relay switch's state as the UI should render it: readable while the
/// vault is locked (the pre-login switch's first paint) and while unlocked.
#[derive(Serialize)]
struct RelayResponse {
    /// The user's choice for the switch, from the prefs file — the same value
    /// whether the vault is locked or not. The login card and the settings
    /// switch both render this, so the UI never argues with the user about
    /// what they asked for.
    tor_mode: bool,
    /// Whether that choice is actually in effect on the vault's traffic:
    /// applied to the unlocked vault, an onion address on file, and a SOCKS
    /// port known. The pill's "needs attention" state comes from this being
    /// false while `tor_mode` is true.
    applied: bool,
    /// The onion address on file (the vault's when unlocked, the staged one
    /// otherwise).
    onion_url: Option<String>,
    /// The SOCKS proxy currently applied in the daemon, when the shell has
    /// reported its Tor.
    socks: Option<String>,
    /// Whether the relay answered a health probe: `true` reachable, `false`
    /// unreachable, `null` not yet known (locked, or the first probe still
    /// pending). This is what makes the connection light honest — Tor can be
    /// "ready" with the network gone, and only a real probe notices.
    reachable: Option<bool>,
}

/// Whether the relay is reachable right now, from a briefly cached probe.
///
/// A probe is a real round trip (through Tor when the switch is on), so it
/// never blocks this request: a stale cache returns its last answer and kicks
/// off a background refresh, guarded so probes never stack on the worker. The
/// UI polls faster than the cache lives, so a change of state shows within a
/// poll or two.
async fn relay_reachability(state: &Arc<App>) -> Option<bool> {
    let cached = *state.relay_health.lock().await;
    if !*state.unlocked.lock().await {
        return None;
    }
    let fresh = cached
        .map(|(_, at)| at.elapsed() < Duration::from_secs(4))
        .unwrap_or(false);
    if !fresh && !state.relay_probe_inflight.swap(true, Ordering::AcqRel) {
        let state = state.clone();
        tokio::spawn(async move {
            if let WorkResult::Reachable(reachable) = state.worker.relay_probe().await {
                *state.relay_health.lock().await = Some((reachable, Instant::now()));
            }
            state.relay_probe_inflight.store(false, Ordering::Release);
        });
    }
    cached.map(|(reachable, _)| reachable)
}

async fn get_relay(State(state): State<Arc<App>>) -> Json<RelayResponse> {
    let locked = !*state.unlocked.lock().await;
    let prefs = ui_prefs::load(&state.dir);
    let socks = state.relay_socks.lock().await.clone();
    let reachable = relay_reachability(&state).await;
    if locked {
        Json(RelayResponse {
            tor_mode: prefs.relay_tor,
            applied: false,
            onion_url: prefs
                .relay_onion_staged
                .or_else(|| hush_core::config::resolve_onion_relay_url(&state.dir)),
            socks,
            reachable,
        })
    } else {
        match state.worker.relay_info().await {
            WorkResult::RelayInfo {
                relay_tor,
                onion_url,
            } => {
                let onion_url = onion_url
                    .or(prefs.relay_onion_staged)
                    .or_else(|| hush_core::config::resolve_onion_relay_url(&state.dir));
                let applied = relay_tor && onion_url.is_some() && socks.is_some();
                Json(RelayResponse {
                    tor_mode: prefs.relay_tor,
                    applied,
                    onion_url,
                    socks,
                    reachable,
                })
            }
            _ => Json(RelayResponse {
                tor_mode: prefs.relay_tor,
                applied: false,
                onion_url: prefs
                    .relay_onion_staged
                    .or_else(|| hush_core::config::resolve_onion_relay_url(&state.dir)),
                socks,
                reachable,
            }),
        }
    }
}

#[derive(Deserialize)]
struct RelayModeRequest {
    /// Whether the relay path goes through Tor.
    onion: bool,
    /// The SOCKS proxy the shell's Tor is listening on, reported when the
    /// switch is turned on. Optional and validated: a bad value is refused
    /// rather than silently producing direct traffic.
    #[serde(default)]
    socks: Option<String>,
}

/// Flip the relay switch. Persists the mode (effective across restarts),
/// records the SOCKS proxy, and applies both to the live vault when it is
/// unlocked — while locked, unlock applies them.
///
/// There is no automatic fall back to clearnet anywhere in this path: with
/// the switch on and Tor down, requests fail (and mail queues in the outbox)
/// until Tor connects or the user turns the switch off.
async fn put_relay_mode(
    State(state): State<Arc<App>>,
    Json(body): Json<RelayModeRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if let Some(socks) = &body.socks {
        if let Err(e) = hush_core::validate_socks_proxy(socks) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: e.to_string(),
                }),
            ));
        }
    }
    let prefs = ui_prefs::load(&state.dir);
    if let Err(e) = ui_prefs::save(
        &state.dir,
        &ui_prefs::UiPrefs {
            relay_tor: body.onion,
            ..prefs
        },
    ) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: e }),
        ));
    }
    if body.onion {
        if let Some(socks) = body.socks.clone() {
            *state.relay_socks.lock().await = Some(socks);
        }
    } else {
        *state.relay_socks.lock().await = None;
    }
    // The wallet follows the relay's Tor choice, and is locked on while it is.
    sync_wallet_relay(&state).await;
    // Apply the choice to the live vault, but do not fail the request when
    // that cannot happen yet (no onion address on file): the switch is a
    // preference, and whether it is in effect is what `applied` in
    // `GET /api/relay` reports — the UI's pending state, not a 400.
    let onion = if body.onion {
        ui_prefs::load(&state.dir)
            .relay_onion_staged
            .or_else(|| hush_core::config::resolve_onion_relay_url(&state.dir))
    } else {
        None
    };
    let _ = state
        .worker
        .relay_apply(body.onion, onion, body.socks)
        .await;
    // The path may have just changed; drop any cached reachability so the
    // next poll probes the new route rather than reporting the old one.
    *state.relay_health.lock().await = None;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RelayOnionRequest {
    /// A v3 onion address for this vault's relay, validated before staging.
    onion: String,
}

/// Attach the onion address of this vault's relay.
///
/// While the vault is locked the address is staged in prefs and applied at
/// unlock; while unlocked it is written straight into the vault together with
/// the user's current switch choice — so attaching completes a switch that
/// was waiting for an address, rather than leaving it pending. Either way the
/// clearnet base is never overwritten: the switch stays able to flip back.
async fn put_relay_onion(
    State(state): State<Arc<App>>,
    Json(body): Json<RelayOnionRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let normalized = hush_core::validate_onion_relay(&body.onion).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
    })?;
    let unlocked = !*state.unlocked.lock().await;
    let prefs = ui_prefs::load(&state.dir);
    if unlocked {
        let socks = state.relay_socks.lock().await.clone();
        map_err(
            state
                .worker
                .relay_apply(prefs.relay_tor, Some(normalized.clone()), socks)
                .await,
        )?;
        // Applied into the vault; clear any stale stage.
        let mut prefs = ui_prefs::load(&state.dir);
        prefs.relay_onion_staged = None;
        let _ = ui_prefs::save(&state.dir, &prefs);
    } else {
        let prefs = ui_prefs::UiPrefs {
            relay_onion_staged: Some(normalized),
            ..prefs
        };
        ui_prefs::save(&state.dir, &prefs).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: e }),
            )
        })?;
    }
    Ok(StatusCode::NO_CONTENT)
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
    if body.auto_lock_minutes != 0
        && (body.auto_lock_minutes < 5 || body.auto_lock_minutes > 24 * 60)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "auto_lock_minutes must be 0 (off) or between 5 and 1440".into(),
            }),
        ));
    }
    let relay_changed = ui_prefs::load(&state.dir).relay_tor != body.relay_tor;
    ui_prefs::save(&state.dir, &body).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: e }),
        )
    })?;
    // The pre-login screen flips the relay switch through this route, so the
    // wallet has to hear about it here too. Only when it actually moved: a
    // theme change must not rebuild the wallet.
    if relay_changed {
        sync_wallet_relay(&state).await;
    }
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

#[derive(Deserialize)]
struct MaterializeRequest {
    /// The original file name; its extension drives the OS open handler.
    #[serde(default)]
    name: String,
}

#[derive(Serialize)]
struct MaterializeResponse {
    path: String,
}

/// Decrypt a stored blob to a temp file and return its path, so the desktop
/// shell can open or save it without the bytes ever entering the webview.
///
/// The write streams chunk by chunk (see `materialize_blob`), so peak memory is
/// one chunk regardless of file size — unlike `GET /api/blob/{id}`, which
/// base64-encodes the whole file into one JSON body.
async fn blob_materialize(
    State(state): State<Arc<App>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<MaterializeRequest>,
) -> Result<Json<MaterializeResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.blob_materialize(&id, &body.name).await)?;
    match result {
        WorkResult::Path(path) => Ok(Json(MaterializeResponse { path })),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

// ---------------------------------------------------------------------------
// Reads (Hacker News reader) — routes behind `/api/reads/*`.
//
// Content routes (feed/search/story/reader) serve public, already-sanitized
// data and work regardless of vault lock — the UI only reaches them unlocked
// anyway. Personal-state routes round-trip through the account worker so the
// sidecar is only ever touched by the holder of the master key.
// ---------------------------------------------------------------------------

/// One story in a list, annotated with the viewer's own markers so lists
/// render save/read state without a second round trip.
#[derive(Serialize)]
struct AnnotatedStory {
    #[serde(flatten)]
    story: reads::StoryDto,
    saved: bool,
    read: bool,
}

#[derive(Serialize)]
struct ReadsFeedResponse {
    stories: Vec<AnnotatedStory>,
    /// True when this page looks like the last one (short page heuristic).
    done: bool,
}

#[derive(Deserialize)]
struct ReadsFeedQuery {
    /// Which source to browse: `hk-news` (the built-in HN feed) or the id of
    /// a user-added RSS source. Defaults to the built-in.
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    page: Option<u32>,
}

#[derive(Deserialize)]
struct ReadsAddSourceRequest {
    url: String,
}

#[derive(Deserialize)]
struct ReadsReaderQuery {
    url: String,
}

#[derive(Deserialize)]
struct ReadsSaveRequest {
    story: reads::StoryDto,
}

#[derive(Deserialize)]
struct ReadsHiddenRequest {
    id: String,
}

#[derive(Deserialize)]
struct ReadsReadRequest {
    id: String,
    #[serde(default)]
    title: String,
}

fn reads_err(e: reads::ReadsError) -> (StatusCode, Json<ApiError>) {
    let code = match &e {
        reads::ReadsError::BadUrl(_) => StatusCode::BAD_REQUEST,
        reads::ReadsError::NoContent => StatusCode::NOT_FOUND,
        reads::ReadsError::Http(_)
        | reads::ReadsError::Blocked(_)
        | reads::ReadsError::Parse(_) => StatusCode::BAD_GATEWAY,
    };
    (
        code,
        Json(ApiError {
            error: e.to_string(),
        }),
    )
}

fn worker_err(msg: String) -> (StatusCode, Json<ApiError>) {
    if msg == "locked" {
        (StatusCode::UNAUTHORIZED, Json(ApiError { error: msg }))
    } else {
        (StatusCode::BAD_REQUEST, Json(ApiError { error: msg }))
    }
}

/// Fetch the raw sidecar bytes through the worker.
async fn reads_state_raw(state: &App) -> Result<Option<Vec<u8>>, (StatusCode, Json<ApiError>)> {
    match state.worker.reads_state_get().await {
        WorkResult::ReadsState(raw) => Ok(raw),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Lenient variant for content routes: while the vault is locked there is no
/// personal state to annotate with, and the public stories themselves are
/// still servable — the UI just never asks before unlocking.
async fn reads_state_raw_lenient(
    state: &App,
) -> Result<Option<Vec<u8>>, (StatusCode, Json<ApiError>)> {
    match state.worker.reads_state_get().await {
        WorkResult::ReadsState(raw) => Ok(raw),
        WorkResult::Err(msg) if msg == "locked" => Ok(None),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Load the state, apply one mutation, persist it back, and return the new
/// state as the response (the UI re-renders from it). A mutation may refuse
/// (`Err` names why); the state is then neither changed nor persisted.
async fn mutate_reads_state(
    state: &App,
    apply: impl FnOnce(&mut reads_store::ReadsState) -> Result<(), String>,
) -> Result<Json<reads_store::ReadsState>, (StatusCode, Json<ApiError>)> {
    let raw = reads_state_raw(state).await?;
    let mut parsed = reads_store::decode(raw.as_deref()).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    apply(&mut parsed).map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    let bytes = parsed.encode().map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    match state.worker.reads_state_put(bytes).await {
        WorkResult::Ok => Ok(Json(parsed)),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn reads_feed(
    State(state): State<Arc<App>>,
    Query(q): Query<ReadsFeedQuery>,
) -> Result<Json<ReadsFeedResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let page = q.page.unwrap_or(0);
    let source_id = q.source.unwrap_or_else(|| reads_store::HK_NEWS_ID.into());
    let personal = reads_state_raw_lenient(&state).await?;
    let view = reads_store::decode(personal.as_deref()).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    let (stories, done) = match source_id.as_str() {
        reads_store::HK_NEWS_ID => {
            let stories = state.reads.hknews_feed(page).await.map_err(reads_err)?;
            let done = stories.len() < reads::HITS_PER_PAGE as usize;
            (stories, done)
        }
        _ => {
            let source = view.source(&source_id).ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(ApiError {
                        error: "that feed is not in your sources".into(),
                    }),
                )
            })?;
            let (_, stories) = state.reads.rss_feed(&source.url).await.map_err(reads_err)?;
            (stories, true) // a feed arrives whole; there is no page 2
        }
    };
    let stories = view.filter_hidden(stories);
    let stories = stories
        .into_iter()
        .map(|story| AnnotatedStory {
            saved: view.is_saved(&story.id),
            read: view.read.iter().any(|m| m.id == story.id),
            story,
        })
        .collect();
    Ok(Json(ReadsFeedResponse { stories, done }))
}

/// One browsable source: the built-in HK News feed or a user RSS feed.
#[derive(Serialize)]
struct ReadsSourceDto {
    id: String,
    title: String,
    url: String,
    /// Built-ins cannot be removed; the UI hides the affordance.
    built_in: bool,
}

async fn reads_sources_list(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<ReadsSourceDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    reads_sources_view(&state).await.map(Json)
}

/// The browsable source list: the built-in first, then the user's feeds.
/// Every source route returns this same shape so the UI can swap its list
/// wholesale after any mutation.
async fn reads_sources_view(
    state: &App,
) -> Result<Vec<ReadsSourceDto>, (StatusCode, Json<ApiError>)> {
    let personal = reads_state_raw_lenient(state).await?;
    let view = reads_store::decode(personal.as_deref()).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    let mut sources = vec![ReadsSourceDto {
        id: reads_store::HK_NEWS_ID.into(),
        title: "HK News".into(),
        url: "https://news.ycombinator.com/best".into(),
        built_in: true,
    }];
    sources.extend(view.sources.iter().map(source_dto));
    Ok(sources)
}

/// Add an RSS source. The URL is validated against the SSRF guard and
/// fetched once now: an unreachable or unparseable feed is refused with the
/// reason, and a good one contributes its own title.
async fn reads_sources_add(
    State(state): State<Arc<App>>,
    Json(body): Json<ReadsAddSourceRequest>,
) -> Result<Json<Vec<ReadsSourceDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let raw_url = body.url.trim().to_string();
    let (title, _) = state
        .reads
        .rss_feed(&raw_url)
        .await
        .map_err(reads_err)
        .map_err(|(code, e)| {
            (
                if code == StatusCode::INTERNAL_SERVER_ERROR {
                    StatusCode::BAD_GATEWAY
                } else {
                    code
                },
                e,
            )
        })?;
    let _ = mutate_reads_state(&state, |v| {
        v.add_source(&raw_url, &title)?;
        Ok(())
    })
    .await?;
    reads_sources_view(&state).await.map(Json)
}

async fn reads_sources_remove(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ReadsSourceDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let _ = mutate_reads_state(&state, |v| {
        v.remove_source(&id);
        Ok(())
    })
    .await?;
    reads_sources_view(&state).await.map(Json)
}

fn source_dto(s: &reads_store::RssSource) -> ReadsSourceDto {
    ReadsSourceDto {
        id: s.id.clone(),
        title: s.title.clone(),
        url: s.url.clone(),
        built_in: false,
    }
}

async fn reads_story(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<reads::StoryDetailDto>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state.reads.story(&id).await.map_err(reads_err).map(Json)
}

async fn reads_reader(
    State(state): State<Arc<App>>,
    Query(q): Query<ReadsReaderQuery>,
) -> Result<Json<reads::ReaderDto>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .reads
        .reader(&q.url)
        .await
        .map_err(reads_err)
        .map(Json)
}

async fn reads_state(
    State(state): State<Arc<App>>,
) -> Result<Json<reads_store::ReadsState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let raw = reads_state_raw(&state).await?;
    let view = reads_store::decode(raw.as_deref()).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    Ok(Json(view))
}

async fn reads_save_toggle(
    State(state): State<Arc<App>>,
    Json(body): Json<ReadsSaveRequest>,
) -> Result<Json<reads_store::ReadsState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    mutate_reads_state(&state, |v| {
        v.toggle_saved(body.story);
        Ok(())
    })
    .await
}

async fn reads_hidden_toggle(
    State(state): State<Arc<App>>,
    Json(body): Json<ReadsHiddenRequest>,
) -> Result<Json<reads_store::ReadsState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    mutate_reads_state(&state, |v| {
        v.toggle_hidden(&body.id);
        Ok(())
    })
    .await
}

async fn reads_mark_read(
    State(state): State<Arc<App>>,
    Json(body): Json<ReadsReadRequest>,
) -> Result<Json<reads_store::ReadsState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    mutate_reads_state(&state, |v| {
        v.mark_read(&body.id, &body.title);
        Ok(())
    })
    .await
}

// ---------------------------------------------------------------------------
// Torrents — the TorBox-powered pipeline behind `/api/torrents/*`.
//
// Every call goes through [`torrents::TorrentsManager`]; the API key lives in
// the vault sidecar and is never part of a response. A locked vault is a 401,
// the same contract the Reads state routes use.
// ---------------------------------------------------------------------------

fn torrents_err(msg: String) -> (StatusCode, Json<ApiError>) {
    if msg == "locked" {
        (StatusCode::UNAUTHORIZED, Json(ApiError { error: msg }))
    } else {
        (StatusCode::BAD_REQUEST, Json(ApiError { error: msg }))
    }
}

// --- Wallet -----------------------------------------------------------------

fn wallet_err(msg: String) -> (StatusCode, Json<ApiError>) {
    if msg == "locked" {
        (StatusCode::UNAUTHORIZED, Json(ApiError { error: msg }))
    } else {
        (StatusCode::BAD_REQUEST, Json(ApiError { error: msg }))
    }
}

/// Whether the relay is switched to Tor. The wallet follows it, and is locked
/// on while it is, so this is read fresh on every wallet request.
fn relay_is_tor(state: &App) -> bool {
    ui_prefs::load(&state.dir).relay_tor
}

async fn wallet_state(
    State(state): State<Arc<App>>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .public_state(tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_create(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::CreateRequest>,
) -> Result<Json<wallet::CreateResult>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .create(&body.network, &body.ethereum_network, body.mode, tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_restore(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::RestoreRequest>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .restore(
            &body.mnemonic,
            &body.network,
            &body.ethereum_network,
            body.mode,
            tor,
        )
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_address(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::ChainRequest>,
) -> Result<Json<wallet::AddressResult>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .wallet
        .receive_address(body.chain)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_sync(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::ChainRequest>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .sync(body.chain, tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_send(
    State(state): State<Arc<App>>,
    Json(body): Json<hush_wallet::chain::SendRequest>,
) -> Result<Json<wallet::SendResult>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state.wallet.send(body).await.map(Json).map_err(wallet_err)
}

async fn wallet_mode(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::ModeRequest>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .set_mode(body.mode, tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

/// The shell reports the SOCKS proxy its Tor is listening on, for the wallet.
///
/// The wallet's Tor is the shell's Tor, the same process the relay and the
/// Drop window use, so this is a port announcement rather than a setting. It is
/// a separate route from the relay's because the wallet may be on Tor while the
/// relay is not.
async fn wallet_proxy(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::ProxyRequest>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    if let Some(socks) = body.socks.as_deref() {
        // Validated here rather than trusted: a proxy string the wallet cannot
        // parse would fail the build later, with a worse error.
        hush_core::validate_socks_proxy(socks).map_err(|e| wallet_err(e.to_string()))?;
    }
    *state.wallet_socks.lock().await = body.socks;
    sync_wallet_relay(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .public_state(tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_token_add(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::TokenRequest>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .add_token(&body.address, tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_token_remove(
    State(state): State<Arc<App>>,
    Json(body): Json<wallet::TokenRequest>,
) -> Result<Json<wallet::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let tor = relay_is_tor(&state);
    state
        .wallet
        .remove_token(&body.address, tor)
        .await
        .map(Json)
        .map_err(wallet_err)
}

async fn wallet_wipe(
    State(state): State<Arc<App>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .wallet
        .wipe()
        .await
        .map(|_| StatusCode::OK)
        .map_err(wallet_err)
}

#[derive(Deserialize)]
struct TorrentsKeyRequest {
    key: String,
}

#[derive(Deserialize)]
struct TorrentsAddRequest {
    source: String,
}

#[derive(Deserialize)]
struct TorrentsAddFileRequest {
    name: String,
    data_base64: String,
}

#[derive(Deserialize)]
struct TorrentsIdRequest {
    id: String,
}

#[derive(Deserialize)]
struct TorrentsConcurrencyRequest {
    max: u32,
}

async fn torrents_state(
    State(state): State<Arc<App>>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .public_state()
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_set_key(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsKeyRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .set_key(&body.key)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_test_key(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsKeyRequest>,
) -> Result<Json<torbox::UserInfo>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .test_key(&body.key)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_refresh(
    State(state): State<Arc<App>>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .refresh_account()
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_add(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsAddRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .add_source(&body.source)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_add_file(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsAddFileRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let bytes = STANDARD
        .decode(body.data_base64.as_bytes())
        .map_err(|_| (StatusCode::BAD_REQUEST, Json(ApiError { error: "bad torrent file data".into() })))?;
    state
        .torrents
        .add_torrent_file(&body.name, bytes)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_pause(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsIdRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .pause(&body.id)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_resume(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsIdRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .resume(&body.id)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_retry(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsIdRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .retry(&body.id)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_remove(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsIdRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .remove(&body.id)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_concurrency(
    State(state): State<Arc<App>>,
    Json(body): Json<TorrentsConcurrencyRequest>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .set_concurrency(body.max)
        .await
        .map(Json)
        .map_err(torrents_err)
}

async fn torrents_delete_library(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<torrents::PublicState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    state
        .torrents
        .delete_library(&id)
        .await
        .map(Json)
        .map_err(torrents_err)
}

/// The drops API, against a real (in-process) relay: key creation, an
/// outsider's sealed container parked on the rendezvous slot, a forced drain,
/// and the drop record surfacing through `GET /api/mail`.
///
/// This is also the daemon's first test of any kind — the router now lives in
/// `api_router` so it can be driven without binding a socket.
#[cfg(test)]
mod drops_api_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use hush_core::{DropAttachment, DropPackage, RelayClient};
    use hush_relay::{router, RelayConfig};
    use tempfile::TempDir;
    use tower::ServiceExt;

    async fn spawn_relay() -> (String, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("relay.db");
        let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        (base, dir)
    }

    fn test_app(dir: &std::path::Path) -> Arc<App> {
        let worker = AccountWorker::spawn();
        Arc::new(App {
            dir: dir.to_path_buf(),
            socket: None,
            tcp_addr: None,
            worker: worker.clone(),
            reads: reads::ReadsClient::new(&dir.join("reads-cache")),
            torrents: Arc::new(torrents::TorrentsManager::new(worker.clone(), dir)),
            wallet: Arc::new(wallet::WalletManager::new(worker, dir)),
            unlocked: Mutex::new(false),
            unlocked_at: Mutex::new(None),
            relay_socks: Mutex::new(None),
            wallet_socks: Mutex::new(None),
            relay_health: Mutex::new(None),
            relay_probe_inflight: AtomicBool::new(false),
        })
    }

    async fn post_json(
        app: &axum::Router,
        uri: &str,
        body: &str,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let res = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    async fn get_json(
        app: &axum::Router,
        uri: &str,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let res = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn the_drops_api_creates_drains_and_files_a_drop() {
        let (relay, _relay_dir) = spawn_relay().await;

        // The vault is created directly, then opened through the daemon's API
        // — the same unlock path the UI drives.
        let vault_dir = TempDir::new().unwrap();
        {
            hush_core::Account::create(vault_dir.path(), "member-pass", &relay)
                .await
                .unwrap();
        }

        let state = test_app(vault_dir.path());
        let app = api_router(state);

        let (status, _) = post_json(&app, "/api/unlock", r#"{"passphrase":"member-pass"}"#).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

        // No key yet, then create one.
        let (status, key) = get_json(&app, "/api/drops/key").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(key["exists"], false);
        let (status, key) = post_json(&app, "/api/drops/key", "{}").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(key["exists"], true);
        let pk = key["public_key"].as_str().unwrap();
        assert!(pk.starts_with("hush-drop-pk-v1:"), "{pk}");
        let page_url = key["page_url"].as_str().unwrap();
        assert!(page_url.ends_with("/drop"), "{page_url}");
        assert!(
            page_url.starts_with(&relay),
            "page_url follows the vault's relay: {page_url}"
        );

        // Creating again is refused — rotation is the deliberate act.
        let (status, _) = post_json(&app, "/api/drops/key", "{}").await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);

        // The outsider seals to the published key and parks it on the
        // rendezvous id.
        let recipient_pk = hush_core::decode_drop_pk(pk).unwrap();
        let slot = hush_core::rendezvous_id(&recipient_pk);
        let container = hush_core::seal_to_key(
            &DropPackage {
                v: 2,
                subject: "through the daemon".into(),
                body: "plain text".into(),
                attachments: vec![DropAttachment {
                    name: "evidence.txt".into(),
                    data: STANDARD.encode(b"attached bytes"),
                }],
                created_unix: 1_750_000_000,
            },
            &recipient_pk,
        )
        .unwrap();
        RelayClient::new(&relay)
            .put_drop(&slot, &container)
            .await
            .unwrap();

        // A forced drain files it and consumes the slot.
        let (status, report) = post_json(&app, "/api/drops/drain", "{}").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(report["hit_slots"].as_array().unwrap().len(), 1);
        assert_eq!(report["remaining"], 0);

        // The drop record surfaces through the mail listing, in its own
        // `drops` array — never inside the ordinary inbox `mail`.
        let (status, mailbox) = get_json(&app, "/api/mail").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(mailbox["mail"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["payload"]["t"] != "drop"));
        let drops: Vec<&serde_json::Value> = mailbox["drops"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["payload"]["t"] == "drop")
            .collect();
        assert_eq!(drops.len(), 1);
        let record = drops[0];
        assert_eq!(record["payload"]["rendezvous_id"], slot.as_str());
        assert_eq!(record["payload"]["subject"], "through the daemon");
        assert_eq!(record["payload"]["body"], "plain text");
        assert_eq!(
            record["payload"]["attachments"].as_array().unwrap().len(),
            1
        );
        assert_eq!(record["payload"]["attachments"][0]["name"], "evidence.txt");

        // A second drain finds nothing.
        let (_, report) = post_json(&app, "/api/drops/drain", "{}").await;
        assert_eq!(report["hit_slots"].as_array().unwrap().len(), 0);
    }

    /// The blob egress used by the Library's Open / Save as…: the daemon
    /// decrypts a stored blob to a temp file and returns the path, so the
    /// bytes never enter the webview. Regression guard for the OOM that
    /// killed the UI opening a 1.65 GiB torrent through the base64 route.
    #[tokio::test]
    async fn the_blob_route_materializes_a_file_for_open() {
        let vault_dir = TempDir::new().unwrap();
        {
            hush_core::Account::create(vault_dir.path(), "member-pass", "http://127.0.0.1:9")
                .await
                .unwrap();
        }

        let state = test_app(vault_dir.path());
        let worker = state.worker.clone();
        assert!(matches!(
            worker
                .unlock(&vault_dir.path().to_path_buf(), "member-pass")
                .await,
            WorkResult::Ok
        ));

        // Store a file the way the torrents pipeline does. It spans more than
        // one 8 MiB chunk so the streaming read loop is exercised, not just a
        // single short read.
        let src = TempDir::new().unwrap();
        let src_path = src.path().join("payload.bin");
        let data: Vec<u8> = (0..(8 * 1024 * 1024 + 12_345u32))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&src_path, &data).unwrap();
        let blob_id = match worker.blob_store_file(&src_path).await {
            WorkResult::BlobId(id) => id,
            _ => panic!("storing the blob failed"),
        };

        let app = api_router(state);
        let (status, json) = post_json(
            &app,
            &format!("/api/blob/{blob_id}/materialize"),
            r#"{"name":"Folder/Movie.mkv"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let path = std::path::PathBuf::from(json["path"].as_str().unwrap());
        // The base name survives (extension drives the OS handler); the
        // directory part of the torrent path is dropped, not trusted.
        assert_eq!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .split_once('-')
                .unwrap()
                .1,
            "Movie.mkv"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            data,
            "materialized bytes match the stored blob"
        );

        // The same blob materializes to a distinct path each time, so two
        // torrents with the same file name never overwrite each other.
        let (_, json2) = post_json(
            &app,
            &format!("/api/blob/{blob_id}/materialize"),
            r#"{"name":"Movie.mkv"}"#,
        )
        .await;
        assert_ne!(json["path"], json2["path"]);

        // A hash with no blob is a clean error, not a panic or an empty file.
        let missing = "0".repeat(64);
        let (status, err) = post_json(
            &app,
            &format!("/api/blob/{missing}/materialize"),
            r#"{"name":"x"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("no such file"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(json2["path"].as_str().unwrap());
    }
}

/// The wallet API: create, receive, and the relay lock. Runs against a real
/// unlocked vault so the sealed sidecar round trip is exercised for real.
#[cfg(test)]
mod wallet_api_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;

    /// The proxy the shell reports once its Tor is listening. Nothing connects
    /// to it here: building a client is not a request.
    const TEST_SOCKS: &str = "socks5h://127.0.0.1:9050";

    /// A daemon holding the route the shell reports once its Tor is listening.
    ///
    /// Async because priming the wallet is what `unlock` does — it applies the
    /// relay prefs, which hand the wallet its proxy. A wallet that defaults to
    /// Tor cannot be built before that, so a test that skipped it would be
    /// testing a state the real daemon never reaches.
    async fn wallet_app(
        dir: &std::path::Path,
        worker: AccountWorker,
        socket: Option<PathBuf>,
    ) -> Arc<App> {
        let app = Arc::new(App {
            dir: dir.to_path_buf(),
            socket,
            tcp_addr: None,
            worker: worker.clone(),
            reads: reads::ReadsClient::new(&dir.join("reads-cache")),
            torrents: Arc::new(torrents::TorrentsManager::new(worker.clone(), dir)),
            wallet: Arc::new(wallet::WalletManager::new(worker, dir)),
            unlocked: Mutex::new(true),
            unlocked_at: Mutex::new(None),
            relay_socks: Mutex::new(None),
            wallet_socks: Mutex::new(Some(TEST_SOCKS.to_string())),
            relay_health: Mutex::new(None),
            relay_probe_inflight: AtomicBool::new(false),
        });
        app.wallet
            .apply_relay(false, Some(TEST_SOCKS.to_string()))
            .await;
        app
    }

    async fn request(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        // A rejected body is plain text, not JSON: axum's own extractor
        // rejection (the 422 for a chain it cannot parse) never reaches a
        // handler. Parsing with `unwrap` turned that into a panic instead of
        // the status the test is asking about.
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            })
        };
        (status, json)
    }

    /// A created, unlocked vault plus the worker that holds it.
    async fn unlocked(dir: &std::path::Path) -> AccountWorker {
        hush_core::Account::create(dir, "pw", "http://127.0.0.1:9")
            .await
            .unwrap();
        let worker = AccountWorker::spawn();
        assert!(matches!(
            worker.unlock(&dir.to_path_buf(), "pw").await,
            WorkResult::Ok
        ));
        worker
    }

    #[tokio::test]
    async fn wallet_routes_are_not_mounted_without_a_unix_socket() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        // A TCP-only daemon must not expose endpoints that can move funds.
        let app = wallet_app(dir.path(), worker, None).await;
        let router = api_router(app);
        let (status, _) = request(&router, "GET", "/api/wallet", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_then_state_then_a_fresh_address() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let socket = Some(dir.path().join("daemon.sock"));
        let router = api_router(wallet_app(dir.path(), worker, socket).await);

        // Before creation: no wallet, no address.
        let (status, json) = request(&router, "GET", "/api/wallet", None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["exists"], false);
        assert!(json["chains"][0]["address"].is_null());
        // Both chains are always reported, so the UI can show a tab per chain.
        assert_eq!(json["chains"][0]["chain"], "bitcoin");
        assert_eq!(json["chains"][0]["unit"], "sat");
        assert_eq!(json["chains"][1]["chain"], "ethereum");
        assert_eq!(json["chains"][1]["unit"], "wei");
        assert_eq!(json["chains"][1]["available"], cfg!(feature = "ethereum"));
        assert_eq!(json["tokens"].as_array().unwrap().len(), 0);

        // Create returns the phrase exactly once.
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/create",
            Some(r#"{"network":"mainnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(
            json["mnemonic"].as_str().unwrap().split_whitespace().count(),
            12
        );
        assert_eq!(json["state"]["exists"], true);
        // Creation reveals one receive address for the UI to show. The phrase
        // is freshly generated, so only the shape is checkable here; the fixed
        // vector is asserted on the restore path below.
        let first = json["state"]["chains"][0]["address"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            first.starts_with("bc1q") && first.len() == 42,
            "expected a mainnet P2WPKH address, got {first}"
        );
        // A fresh wallet shows a zero balance on both chains, as strings: wei
        // is past what a JSON number can carry exactly.
        assert_eq!(json["state"]["chains"][0]["confirmed"], "0");
        assert_eq!(json["state"]["chains"][1]["confirmed"], "0");
        assert!(json["state"]["chains"][1]["confirmed"].is_string());

        // A second create is refused rather than overwriting the phrase.
        let (status, _) = request(
            &router,
            "POST",
            "/api/wallet/create",
            Some(r#"{"network":"mainnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A new receive address is fresh, not a repeat, and becomes the one
        // the state route hands out.
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/address",
            Some(r#"{"chain":"bitcoin"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["chain"], "bitcoin");
        let second = json["address"].as_str().unwrap().to_string();
        assert_ne!(second, first);

        let (_, json) = request(&router, "GET", "/api/wallet", None).await;
        assert_eq!(json["chains"][0]["address"].as_str().unwrap(), second);

        // The phrase is never handed out again by the state route.
        let (_, json) = request(&router, "GET", "/api/wallet", None).await;
        assert!(json.get("mnemonic").is_none());
    }

    #[tokio::test]
    async fn restore_rejects_a_bad_phrase_and_the_relay_locks_tor_on() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let socket = Some(dir.path().join("daemon.sock"));
        let app = wallet_app(dir.path(), worker, socket).await;
        let router = api_router(app.clone());

        // A bad phrase is refused with a 400, and leaves no wallet behind.
        let (status, _) = request(
            &router,
            "POST",
            "/api/wallet/restore",
            Some(r#"{"mnemonic":"not a real phrase at all","network":"mainnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Restore the canonical vector.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/restore",
            Some(&format!(r#"{{"mnemonic":"{phrase}","network":"mainnet"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        // The first BIP84 mainnet address is a fixed value, and so is the
        // first BIP44 Ethereum account: the same phrase, both chains.
        assert_eq!(
            json["chains"][0]["address"].as_str().unwrap(),
            "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"
        );
        assert_eq!(
            json["chains"][1]["address"].as_str().unwrap(),
            "0x9858EfFD232B4033E47d90003D41EC34EcaEda94"
        );

        // Turn the relay onto Tor; the wallet is locked on and clearnet is
        // refused.
        let prefs = ui_prefs::UiPrefs {
            relay_tor: true,
            ..Default::default()
        };
        ui_prefs::save(dir.path(), &prefs).unwrap();

        let (status, json) = request(&router, "GET", "/api/wallet", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["mode"], "tor");
        assert_eq!(json["mode_locked"], true);

        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/mode",
            Some(r#"{"mode":"clearnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("Tor"));

        // Turning the relay off lets the user choose clearnet again.
        let prefs = ui_prefs::UiPrefs {
            relay_tor: false,
            ..Default::default()
        };
        ui_prefs::save(dir.path(), &prefs).unwrap();
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/mode",
            Some(r#"{"mode":"clearnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["mode"], "clearnet");
        assert_eq!(json["mode_locked"], false);
    }

    #[tokio::test]
    async fn the_relay_switch_on_the_prefs_route_also_locks_the_wallet() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let socket = Some(dir.path().join("daemon.sock"));
        let router = api_router(wallet_app(dir.path(), worker, socket).await);

        // A wallet the user put on clearnet.
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/create",
            Some(r#"{"network":"mainnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/mode",
            Some(r#"{"mode":"clearnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["mode"], "clearnet");

        // The pre-login screen flips the relay switch through the prefs route,
        // not through /api/relay/mode. The wallet has to follow that path too,
        // or it would keep talking in the clear while the UI said Tor.
        let (status, _) = request(
            &router,
            "PUT",
            "/api/ui-prefs",
            Some(r#"{"relay_tor":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, json) = request(&router, "GET", "/api/wallet", None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["mode"], "tor");
        assert_eq!(json["mode_locked"], true);

        // And back to clearnet is refused while the relay is on Tor.
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/mode",
            Some(r#"{"mode":"clearnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("Tor"));
    }

    /// A wallet with a known phrase, so every derived address is a fixed value.
    async fn restored(dir: &std::path::Path, worker: AccountWorker) -> axum::Router {
        let socket = Some(dir.join("daemon.sock"));
        let router = api_router(wallet_app(dir, worker, socket).await);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/restore",
            Some(&format!(r#"{{"mnemonic":"{phrase}","network":"mainnet"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        router
    }

    #[tokio::test]
    async fn ethereum_refuses_a_bad_contract_and_a_bad_amount_before_any_network_use() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let router = restored(dir.path(), worker).await;

        // A contract address that is not an address is refused without a
        // request being made.
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/token",
            Some(r#"{"address":"not-an-address"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!err["error"].as_str().unwrap().is_empty());

        // Removing a token that is not tracked says so.
        let (status, _) = request(
            &router,
            "POST",
            "/api/wallet/token/remove",
            Some(r#"{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A send with an amount that is not a whole number of base units never
        // reaches the signing path.
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/send",
            Some(r#"{"chain":"ethereum","to":"0x9858EfFD232B4033E47d90003D41EC34EcaEda94","amount":"1.5"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("whole number"));

        // Bitcoin has no tokens, and saying so is cheaper than a confusing
        // failure deeper in.
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/send",
            Some(r#"{"chain":"bitcoin","to":"bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu","amount":"1000","token":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("no tokens"));

        // An amount past u64 is refused rather than truncated into the signer.
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/send",
            Some(r#"{"chain":"bitcoin","to":"bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu","amount":"18446744073709551616"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("larger than"));
    }

    #[tokio::test]
    async fn an_unknown_network_or_chain_is_refused_at_create() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let socket = Some(dir.path().join("daemon.sock"));
        let router = api_router(wallet_app(dir.path(), worker, socket).await);

        // A typo in the Ethereum network must not be read as mainnet.
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/create",
            Some(r#"{"network":"mainnet","ethereum_network":"sepoliaa"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        // And nothing was created by the failed attempt.
        let (_, json) = request(&router, "GET", "/api/wallet", None).await;
        assert_eq!(json["exists"], false);

        // A chain that does not exist is refused by name.
        let (status, _) = request(
            &router,
            "POST",
            "/api/wallet/create",
            Some(r#"{"network":"mainnet"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, err) = request(
            &router,
            "POST",
            "/api/wallet/send",
            Some(r#"{"chain":"litecoin","to":"x","amount":"1"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    }

    #[tokio::test]
    async fn the_state_names_the_ethereum_network_the_wallet_was_created_on() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let socket = Some(dir.path().join("daemon.sock"));
        let router = api_router(wallet_app(dir.path(), worker, socket).await);
        let (status, json) = request(
            &router,
            "POST",
            "/api/wallet/create",
            Some(r#"{"network":"mainnet","ethereum_network":"sepolia"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        // The Ethereum account is the same phrase, on the network asked for.
        assert_eq!(json["state"]["chains"][1]["address"].as_str().unwrap().len(), 42);
        // Nothing is persisted about a balance; a fresh wallet reads zero.
        assert_eq!(json["state"]["chains"][1]["confirmed"], "0");
        assert_eq!(json["state"]["tokens"].as_array().unwrap().len(), 0);
    }
}

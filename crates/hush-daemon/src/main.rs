mod account_worker;
mod notes_store;
mod paper_data;
mod paper_doc;
mod paper_store;
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
mod zine_store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use account_worker::{AccountWorker, ContactDto, PenNameDto, WorkResult};
use axum::{
    extract::{Path, Query, State},
    http::{header, Method, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::Parser;
use hush_core::{ChatRecord, ConclaveRecord, MessagePayload, PaperDescriptor, PendingInvite, ReceivedMessage};
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
    /// Conclave Paper's live DataChannel planes, keyed by conclave id, created
    /// on demand when a paper is opened.
    ///
    /// Runtime-only and safe to lose: a plane is a socket, not state. The relay
    /// fan-out underneath is what guarantees convergence, so dropping this map
    /// (a restart, a lock) costs latency, never data.
    paper_planes: Mutex<HashMap<String, Arc<paper_data::DataPlane>>>,
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
        .route("/api/notes/state", get(notes_state))
        .route("/api/notes/note", post(notes_save_note))
        .route("/api/notes/note/{id}", delete(notes_delete_note))
        .route("/api/paper/{conclave_id}", get(paper_get))
        .route("/api/paper/{conclave_id}/create", post(paper_create))
        .route("/api/paper/{conclave_id}/update", post(paper_update))
        .route(
            "/api/paper/{conclave_id}/presence",
            get(paper_presence).post(paper_publish_presence),
        )
        .route("/api/paper/{conclave_id}/hub/rehome", post(paper_rehome))
        // Zines (Task 21, Phase 1 — local only). The literal `pen-name` segment
        // is matched ahead of the `{zine_id}` parameter, and cannot collide with
        // a real zine: an id is a 32-byte pen-name key in hex.
        .route("/api/zines", get(zines_index).post(zines_write))
        .route("/api/zines/pen-name", get(zines_pen_name).post(zines_set_pen_name))
        .route("/api/zines/sections", get(zines_sections))
        .route(
            "/api/zines/{zine_id}",
            get(zines_get).delete(zines_forget),
        )
        .route("/api/zines/{zine_id}/mute", post(zines_mute))
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
        .route("/api/presence/offline", post(presence_offline))
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
        .route(
            "/api/vault/secret/{name}",
            get(get_secret).put(put_secret).delete(delete_secret),
        )
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
    // The paper planes go with the vault: a DataChannel is a live socket whose
    // state came out of the vault, and a locked vault must not keep one open.
    // Peers see the channel close and fall back to the relay with no user
    // action, which is the degradation the plan asks for — and nothing is lost,
    // because the relay already has every byte that went over it.
    paper_planes_shutdown(state).await;
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
        paper_planes: Mutex::new(HashMap::new()),
    };
    let state = Arc::new(app);

    tokio::spawn(auto_lock_loop(state.clone()));
    tokio::spawn(vault_mount_watch(state.clone()));
    tokio::spawn(outbox_loop(state.clone()));
    tokio::spawn(heartbeat_loop(state.clone()));
    tokio::spawn(torrents.run());
    suspend_lock::spawn_suspend_lock_watch(state.clone());
    spawn_paper_plane_loop(state.clone());

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
///
/// Appear-offline is the one exception: when the user asks to be seen as
/// offline, this loop skips the heartbeat entirely so the relay's timestamp
/// goes stale. The UI suppresses its own heartbeat the same way.
async fn heartbeat_loop(state: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        if !*state.unlocked.lock().await {
            continue;
        }
        if ui_prefs::load(&state.dir).appear_offline {
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
    force_lock(&state).await;
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
    // A kick can remove the paper's rendezvous, and this is the one roster
    // change `recv` cannot report (see `paper_inherit_hub`): the roster is
    // already updated by the time the next poll looks at it.
    if paper_inherit_hub(&state, &body.conclave_id).await {
        paper_plane_reset(&state, &body.conclave_id).await;
    }
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
    // The vault is out of the conclave, so its paper channels point at peers it
    // can no longer address. Dropping them is the honest state; the relay
    // refuses the fan-out either way.
    paper_plane_reset(&state, &body.conclave_id).await;
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

/// Mark this vault offline at the relay at once — the appear-offline switch.
///
/// Clearing removes the 90 s freshness window, so peers stop seeing the vault
/// online the moment the switch flips. A relay that predates the clear route
/// answers 404/405; the caller treats this as best-effort, and the stopped
/// heartbeat still takes the vault offline within the window.
async fn presence_offline(
    State(state): State<Arc<App>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.go_offline().await)?;
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
    // Paper payloads are not chat messages: they belong in the papers sidecar,
    // not the inbox. A payload this vault cannot use is logged rather than
    // failing the poll — the messages in the same batch have already landed,
    // and one peer's bad payload must not block the mailbox.
    if !inbox.papers.is_empty() || !inbox.joined.is_empty() || !inbox.departed.is_empty() {
        match paper_sidecar(&state).await {
            Ok(mut parsed) => {
                let now = paper_now_ms();
                let mut changed = false;
                // Conclave ids whose paper had its hub move during this poll, so
                // the plane can be dropped and the next open can dial the new
                // rendezvous instead of holding a dead channel to the old one.
                let mut moved: Vec<String> = Vec::new();
                // Deltas this vault holds the hub for, to re-fan after the
                // roster write below. A mesh can only reach the trustees the
                // sender's roster knew about, so an edit sent while that roster
                // had not caught up with a join is otherwise stranded forever.
                let mut relay: Vec<(String, String, String, Vec<u8>)> = Vec::new();
                // Papers whose whole document has to go out: a trustee that just
                // joined missed every delta before it, and — after an `adopt`
                // that re-identified this vault's own paper — the winner of a
                // create collision dropped the deltas this vault sent under the
                // id it used at the time. Both are "here is the document", so
                // they share the one send below.
                let mut catch_up: Vec<(String, PaperDescriptor, Vec<u8>)> = Vec::new();
                let me = paper_account_id(&state).await.ok();
                for received in &inbox.papers {
                    // Signalling moves the transport, not the text, so it is
                    // consumed before the document path can see it.
                    if paper_handle_signal(&state, &received.from_account, &received.payload).await {
                        continue;
                    }
                    let hub_claim = match &received.payload {
                        MessagePayload::PaperHub { paper } => Some(paper.conclave_id.clone()),
                        _ => None,
                    };
                    // The id this vault's paper for the payload's conclave has
                    // *before* the payload lands, so an `adopt` that moves it can
                    // be told apart from an ordinary create.
                    let identity_before = match &received.payload {
                        MessagePayload::PaperState { paper } => {
                            Some(parsed.get(&paper.conclave_id).map(|f| f.descriptor.paper_id.clone()))
                        }
                        _ => None,
                    };
                    match paper_apply_inbound(
                        &mut parsed,
                        &received.from_account,
                        &received.payload,
                        now,
                    ) {
                        Ok(did) => {
                            changed |= did;
                            if did {
                                if let Some(conclave_id) = hub_claim {
                                    moved.push(conclave_id);
                                }
                                if let MessagePayload::PaperState { paper } = &received.payload {
                                    // A `PaperState` that moved this vault's
                                    // paper to a different id means two trustees
                                    // started one at the same moment and the
                                    // earlier create won (see `adopt`). The
                                    // deltas already sent under the old id were
                                    // dropped by the winner as unknown, so the
                                    // folded document is handed over under the
                                    // winning descriptor — the one send that
                                    // means "here is the whole document".
                                    let moved_id = identity_before
                                        .flatten()
                                        .filter(|old_id| {
                                            parsed
                                                .get(&paper.conclave_id)
                                                .is_some_and(|f| &f.descriptor.paper_id != old_id)
                                        })
                                        .is_some();
                                    if moved_id {
                                        if let Some(file) = parsed.get(&paper.conclave_id) {
                                            let updates: Vec<Vec<u8>> =
                                                file.log.iter().map(|u| u.update.clone()).collect();
                                            match paper_doc::fold(&file.snapshot, &updates) {
                                                Ok(full) => catch_up.push((
                                                    paper.conclave_id.clone(),
                                                    file.descriptor.clone(),
                                                    full,
                                                )),
                                                Err(e) => eprintln!(
                                                    "hush-daemon: paper {} re-announce failed: {e}",
                                                    paper.conclave_id
                                                ),
                                            }
                                        }
                                    }
                                }
                                if let MessagePayload::PaperDelta { paper_id, update } =
                                    &received.payload
                                {
                                    if let Some(me) = &me {
                                        let held = parsed
                                            .conclave_of(paper_id)
                                            .and_then(|c| parsed.get(&c))
                                            .map(|f| &f.descriptor.hub == me)
                                            .unwrap_or(false);
                                        if held {
                                            relay.push((
                                                parsed.conclave_of(paper_id).unwrap_or_default(),
                                                paper_id.clone(),
                                                received.from_account.clone(),
                                                update.clone(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!(
                            "hush-daemon: paper payload from {} ignored: {e}",
                            received.from_nick
                        ),
                    }
                }
                // Default hub succession: a paper whose hub left or was kicked
                // has no rendezvous, so the conclave's convener inherits it.
                // Every daemon that saw the departure tries this; the send is
                // what decides, because that is where the convener is known —
                // so no second copy of the roster is needed here.
                for (conclave_id, _gone) in &inbox.departed {
                    if paper_inherit_hub(&state, conclave_id).await {
                        changed = true;
                        moved.push(conclave_id.clone());
                    }
                }
                // A trustee that just joined missed every delta that came
                // before it, and deltas are never replayed — so the hub hands
                // it the whole document. Only the hub does this: the paper's
                // descriptor names it, so N trustees cannot each send one.
                // Addressed to the joined set, which is idempotent to merge for
                // a trustee that already had the document.
                if !inbox.joined.is_empty() {
                    if let Ok(me) = paper_account_id(&state).await {
                        for conclave_id in &inbox.joined {
                            let Some(file) = parsed.get(conclave_id) else {
                                continue;
                            };
                            if file.descriptor.hub != me {
                                continue;
                            }
                            let updates: Vec<Vec<u8>> =
                                file.log.iter().map(|u| u.update.clone()).collect();
                            match paper_doc::fold(&file.snapshot, &updates) {
                                Ok(full) => catch_up.push((
                                    conclave_id.clone(),
                                    file.descriptor.clone(),
                                    full,
                                )),
                                Err(e) => eprintln!(
                                    "hush-daemon: paper {conclave_id} snapshot failed: {e}"
                                ),
                            }
                        }
                    }
                }
                if changed {
                    paper_persist(&state, &parsed).await?;
                }
                // Re-fan deltas this vault holds the hub for. Done after the
                // persist so a failure to fan out never costs the local merge.
                for (conclave_id, paper_id, from_account, update) in relay {
                    if let WorkResult::Err(e) = state
                        .worker
                        .relay_paper_delta(&conclave_id, &paper_id, &from_account, &update)
                        .await
                    {
                        eprintln!("hush-daemon: paper relay fan-out failed: {e}");
                    }
                }
                // A moved hub is a moved rendezvous: drop the plane so the next
                // open dials the new hub rather than holding a dead channel to
                // the one that just left.
                for conclave_id in moved {
                    paper_plane_reset(&state, &conclave_id).await;
                }
                for (conclave_id, paper, full) in catch_up {
                    if let WorkResult::Err(e) = state
                        .worker
                        .send_paper_snapshot(&conclave_id, paper, &full)
                        .await
                    {
                        eprintln!("hush-daemon: paper snapshot fan-out failed: {e}");
                    }
                }
            }
            Err(_) => eprintln!(
                "hush-daemon: papers sidecar unreadable; {} paper payload(s) lost",
                inbox.papers.len()
            ),
        }
    }
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
        // A paper payload is not a chat message either: it is routed into the
        // paper store, never the inbox. An unknown tag has no rendering at all.
        MessagePayload::PaperState { .. }
        | MessagePayload::PaperDelta { .. }
        | MessagePayload::PaperSnapshot { .. }
        | MessagePayload::PaperPresence { .. }
        | MessagePayload::PaperHub { .. }
        | MessagePayload::PaperOffer { .. }
        | MessagePayload::PaperAnswer { .. }
        | MessagePayload::Unknown => None,
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

/// The longest sidecar name the API accepts. The vault rejects separators and
/// traversal on its own; this cap is only so a client cannot name a file purely
/// to fill the vault directory.
const SECRET_NAME_MAX: usize = 64;

/// Check a sidecar name before it reaches the worker, so a bad one comes back
/// as a 400 with something the UI can show instead of a vault error string.
fn check_secret_name(name: &str) -> Result<(), (StatusCode, Json<ApiError>)> {
    let allowed = !name.is_empty()
        && name.len() <= SECRET_NAME_MAX
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    if allowed {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error:
                    "a secret name may only use letters, digits, '-', '_' and '.', 1-64 characters"
                        .into(),
            }),
        ))
    }
}

#[derive(Serialize)]
struct SecretResponse {
    /// Base64 of the sealed sidecar's plaintext.
    value_base64: String,
}

#[derive(Deserialize)]
struct SecretPutRequest {
    value_base64: String,
}

/// Read a named encrypted vault sidecar. 404 means the name is unused, which a
/// client must be able to tell from "stored, and empty".
async fn get_secret(
    State(state): State<Arc<App>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<SecretResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    check_secret_name(&name)?;
    match map_err(state.worker.secret_read(&name).await)? {
        WorkResult::Secret(Some(bytes)) => Ok(Json(SecretResponse {
            value_base64: STANDARD.encode(bytes),
        })),
        WorkResult::Secret(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("no secret named {name}"),
            }),
        )),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

/// Store a named encrypted vault sidecar, replacing any previous value.
async fn put_secret(
    State(state): State<Arc<App>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<SecretPutRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    check_secret_name(&name)?;
    let bytes = STANDARD.decode(&body.value_base64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!("value_base64 is not base64: {e}"),
            }),
        )
    })?;
    match map_err(state.worker.secret_write(&name, bytes).await)? {
        WorkResult::Ok => Ok(StatusCode::NO_CONTENT),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
}

/// Delete a named encrypted vault sidecar. Removing an unused name succeeds:
/// the caller wanted it gone, and it is.
async fn delete_secret(
    State(state): State<Arc<App>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    check_secret_name(&name)?;
    match map_err(state.worker.secret_remove(&name).await)? {
        WorkResult::SecretRemoved => Ok(StatusCode::NO_CONTENT),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "unexpected worker response".into(),
            }),
        )),
    }
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
// Notes — the private sticky-paper board behind `/api/notes/*`.
//
// Personal state only: every route round-trips through the account worker so
// the encrypted sidecar is touched only by the holder of the master key. A
// locked vault answers 401 on all of them, matching Reads. There is no network
// path — notes never leave the device.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct NotesNoteResponse {
    state: notes_store::NotesState,
    /// The id of the note just created or updated, so the UI can open it
    /// without guessing which one changed.
    id: String,
}

/// Fetch the raw notes sidecar bytes through the worker.
async fn notes_state_raw(state: &App) -> Result<Option<Vec<u8>>, (StatusCode, Json<ApiError>)> {
    match state.worker.secret_read(notes_store::STATE_FILE).await {
        WorkResult::Secret(raw) => Ok(raw),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Load and decode the notes state (an empty default until first saved).
async fn notes_decode(
    state: &App,
) -> Result<notes_store::NotesState, (StatusCode, Json<ApiError>)> {
    let raw = notes_state_raw(state).await?;
    notes_store::decode(raw.as_deref()).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })
}

/// Persist a notes state wholesale, replacing the sidecar.
async fn notes_persist(
    state: &App,
    parsed: &notes_store::NotesState,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let bytes = parsed.encode().map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    match state.worker.secret_write(notes_store::STATE_FILE, bytes).await {
        WorkResult::Ok => Ok(()),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn notes_state(
    State(state): State<Arc<App>>,
) -> Result<Json<notes_store::NotesState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    Ok(Json(notes_decode(&state).await?))
}

/// Create (no `id`) or update (matching `id`) a note, returning the new state
/// and the note's id so the UI can open the editor on it.
async fn notes_save_note(
    State(state): State<Arc<App>>,
    Json(draft): Json<notes_store::NoteDraft>,
) -> Result<Json<NotesNoteResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = notes_decode(&state).await?;
    let id = parsed
        .upsert_note(draft)
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    notes_persist(&state, &parsed).await?;
    Ok(Json(NotesNoteResponse { state: parsed, id }))
}

async fn notes_delete_note(
    State(state): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<notes_store::NotesState>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = notes_decode(&state).await?;
    parsed.delete_note(&id);
    notes_persist(&state, &parsed).await?;
    Ok(Json(parsed))
}

// ---------------------------------------------------------------------------
// Paper — the conclave's shared collaborative document behind `/api/paper/*`.
//
// The document is a Yjs doc owned by the webview's editor (ProseMirror); the
// daemon owns the merged CRDT state as opaque `yrs` updates (see
// [`paper_doc`]) and the encrypted `papers.enc` sidecar (see [`paper_store`]).
// One paper per conclave, created by the first trustee who starts it; the
// creator is the hub. Phase 1 is offline — no wire fan-out yet, so an update's
// `origin` is always this vault's account. Conclave-lifecycle coupling (create
// validating membership, `ConclaveEnd` read-only, deletion cleanup) lands with
// the fan-out in Phase 2, where the two-daemon script can exercise it. Like
// Notes, every route round-trips through the account worker and a locked vault
// answers 401.
// ---------------------------------------------------------------------------

/// Pending updates tolerated before the log folds into the snapshot. Small on
/// purpose: every fetch and every update re-folds the pending log, so this
/// bounds that work; the snapshot itself is capped in [`paper_store`].
const PAPER_COMPACT_AFTER: usize = 64;

fn paper_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct PaperCreateRequest {
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct PaperUpdateRequest {
    /// Base64 of a v1 Yjs update produced by the editor.
    update: String,
}

#[derive(Serialize)]
struct PaperUpdateView {
    rev: u64,
    origin: String,
    /// Base64 of the opaque v1 update.
    update: String,
    at_ms: u64,
}

#[derive(Serialize)]
struct PaperView {
    paper: paper_store::PaperDescriptor,
    /// Last applied revision; 0 on a fresh paper. Revisions are gapless and
    /// never restart, even across compaction.
    revision: u64,
    updated_ms: u64,
    /// Base64 of the full merged v1 state — what a second editor loads.
    state: String,
    /// Pending log updates (post-snapshot), oldest first.
    updates: Vec<PaperUpdateView>,
    /// Whether the write that produced this view folded the log into the
    /// snapshot.
    compacted: bool,
    /// Where this daemon's edits are going right now, for the UI's badge.
    fast_path: PaperPathView,
}

/// The fast path as the badge reads it.
#[derive(Serialize)]
struct PaperPathView {
    /// `live` when at least one trustee is on a DataChannel, `relay` when none
    /// are — i.e. whether the next edit goes out fast or at relay speed.
    ///
    /// One flag rather than a per-peer breakdown because that is the question
    /// the badge answers. A hub with some trustees live and some not reads
    /// `live`, which is true of its own edits and of the paper's latency; the
    /// count is here for a UI that wants to be more specific.
    path: &'static str,
    /// How many trustees are on the fast path right now.
    live_peers: usize,
}

/// Where the fast path stands for a paper, as the badge reads it.
async fn paper_path_view(state: &Arc<App>, conclave_id: &str) -> PaperPathView {
    let live_peers = match paper_open_plane(state, conclave_id).await {
        Some(plane) => plane.peers().len(),
        None => 0,
    };
    PaperPathView {
        path: if live_peers > 0 { "live" } else { "relay" },
        live_peers,
    }
}

fn paper_view_of(
    file: &paper_store::PaperFile,
    state: String,
    compacted: bool,
    fast_path: PaperPathView,
) -> PaperView {
    PaperView {
        paper: file.descriptor.clone(),
        revision: file.next_rev - 1,
        updated_ms: file.updated_ms,
        state,
        updates: file
            .log
            .iter()
            .map(|u| PaperUpdateView {
                rev: u.rev,
                origin: u.origin.clone(),
                update: STANDARD.encode(&u.update),
                at_ms: u.at_ms,
            })
            .collect(),
        compacted,
        fast_path,
    }
}

const PAPER_NOT_FOUND: (&str, StatusCode) = ("no paper in this conclave", StatusCode::NOT_FOUND);

// ---------------------------------------------------------------------------
// Zines (Task 21, Phase 1)
// ---------------------------------------------------------------------------
//
// Local and offline: authoring, storing, and reading a zine in this vault. The
// bulletin, the key epidemic, and comments arriving from other members are
// later phases; the routes below are the ones that need no network. Every route
// goes through the worker, so a locked vault answers 401 without any of these
// types being touched.

/// The whole zines pane in one response: the index, the section facet, and the
/// pen name. Three round trips to paint one screen would be three chances to
/// render a pane that disagrees with itself.
#[derive(serde::Serialize)]
struct ZinesView {
    zines: Vec<zine_store::ZineSummary>,
    sections: Vec<ZineSectionView>,
    pen_name: Option<PenNameDto>,
}

#[derive(serde::Serialize)]
struct ZineSectionView {
    name: String,
    count: usize,
}

/// The filters `GET /api/zines` accepts. Both are optional and both are
/// advisory: an unknown section is an empty list, not an error, because a
/// section that has gone away is not a failure to answer.
#[derive(serde::Deserialize)]
struct ZinesQuery {
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    pen: Option<String>,
}

#[derive(serde::Serialize)]
struct ZineView {
    zine: zine_store::ZineFile,
}

#[derive(serde::Deserialize)]
struct ZineMuteBody {
    /// Absent means "mute"; `false` unmutes. An explicit flag rather than a
    /// toggle, so a double-click cannot land in the state the user did not ask
    /// for.
    #[serde(default = "yes")]
    muted: bool,
}

fn yes() -> bool {
    true
}

/// Fetch the raw zines sidecar bytes through the worker and decode them.
///
/// Authored zines that predate the read key being minted at create time are
/// backfilled here and the result persisted, so the migration runs once, on the
/// first read after the upgrade, rather than waiting for the next edit.
async fn zines_sidecar(
    state: &App,
) -> Result<zine_store::ZinesState, (StatusCode, Json<ApiError>)> {
    let mut parsed = match state.worker.secret_read(zine_store::STATE_FILE).await {
        WorkResult::Secret(raw) => zine_store::decode(raw.as_deref()).map_err(|m| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: m }),
            )
        })?,
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };
    if parsed.backfill_read_keys() {
        zines_persist(state, &parsed).await?;
    }
    Ok(parsed)
}

/// Persist the zines sidecar wholesale.
async fn zines_persist(
    state: &App,
    parsed: &zine_store::ZinesState,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let bytes = parsed.encode().map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    match state.worker.secret_write(zine_store::STATE_FILE, bytes).await {
        WorkResult::Ok => Ok(()),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn zines_pen_name_dto(state: &App) -> Result<Option<PenNameDto>, (StatusCode, Json<ApiError>)> {
    match state.worker.pen_name().await {
        WorkResult::PenName(pen) => Ok(pen),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

fn zines_view(
    state: &zine_store::ZinesState,
    section: Option<&str>,
    pen: Option<&str>,
    pen_name: Option<PenNameDto>,
) -> ZinesView {
    ZinesView {
        zines: state.index(section, pen),
        sections: state
            .sections()
            .into_iter()
            .map(|(name, count)| ZineSectionView { name, count })
            .collect(),
        pen_name,
    }
}

/// The local corpus: every zine this vault holds, filtered by section and by
/// pen name. Pull-only — there is no feed and nothing to subscribe to.
async fn zines_index(
    State(state): State<Arc<App>>,
    Query(query): Query<ZinesQuery>,
) -> Result<Json<ZinesView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = zines_sidecar(&state).await?;
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(zines_view(
        &parsed,
        query.section.as_deref(),
        query.pen.as_deref(),
        pen_name,
    )))
}

/// The section facet on its own, for a caller that only wants the tags.
async fn zines_sections(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<ZineSectionView>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = zines_sidecar(&state).await?;
    Ok(Json(
        parsed
            .sections()
            .into_iter()
            .map(|(name, count)| ZineSectionView { name, count })
            .collect(),
    ))
}

/// One zine: the article, its history, and the merged comment thread.
async fn zines_get(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
) -> Result<Json<ZineView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = zines_sidecar(&state).await?;
    let zine = parsed
        .get(&zine_id)
        .ok_or_else(|| zine_not_held(&zine_id))?
        .clone();
    Ok(Json(ZineView { zine }))
}

/// Create the pen name's zine, or write a new revision of it.
///
/// The pen name is taken from the vault, never from the request: a caller does
/// not get to choose which byline it publishes under, because that would let a
/// stale or hostile caller write under an identity it does not hold. Publishing
/// before a pen name exists is refused with a reason rather than minting one
/// silently — a byline is a deliberate act, and one created by a stray POST
/// would be a byline nobody chose.
async fn zines_write(
    State(state): State<Arc<App>>,
    Json(draft): Json<zine_store::ZineDraft>,
) -> Result<Json<ZinesView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let Some(pen_name) = zines_pen_name_dto(&state).await? else {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "choose a pen name before writing a zine".to_string(),
            }),
        ));
    };
    let mut parsed = zines_sidecar(&state).await?;
    // The draft may name a zine, but only the caller's own: a held zine is
    // refused inside the store, and a draft naming someone else's id would
    // otherwise create a *new* zine under that id.
    let zine_id = draft
        .zine_id
        .as_deref()
        .filter(|id| *id == pen_name.id)
        .unwrap_or(&pen_name.id)
        .to_string();
    parsed
        .write(&zine_id, &pen_name.name, draft, paper_now_ms())
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    zines_persist(&state, &parsed).await?;
    Ok(Json(zines_view(&parsed, None, None, Some(pen_name))))
}

/// Set the local mute flag. Local only: no wire effect, and the zine is still
/// held and still counted.
async fn zines_mute(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
    Json(body): Json<ZineMuteBody>,
) -> Result<Json<ZinesView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = zines_sidecar(&state).await?;
    if !parsed.set_muted(&zine_id, body.muted, paper_now_ms()) {
        // Either there is no such zine, or it was already in that state. Both
        // answers are the same to the caller, but only the first is an error.
        if parsed.get(&zine_id).is_none() {
            return Err(zine_not_held(&zine_id));
        }
    }
    zines_persist(&state, &parsed).await?;
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(zines_view(&parsed, None, None, pen_name)))
}

/// Forget a zine locally. Not a retraction: in Phase 2 the blobs are on the
/// bulletin and in every holder's vault, and this only stops this vault from
/// keeping them.
async fn zines_forget(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
) -> Result<Json<ZinesView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = zines_sidecar(&state).await?;
    if !parsed.forget(&zine_id) {
        return Err(zine_not_held(&zine_id));
    }
    zines_persist(&state, &parsed).await?;
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(zines_view(&parsed, None, None, pen_name)))
}

/// The pen name this vault publishes under.
async fn zines_pen_name(
    State(state): State<Arc<App>>,
) -> Result<Json<Option<PenNameDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    Ok(Json(zines_pen_name_dto(&state).await?))
}

#[derive(serde::Deserialize)]
struct PenNameBody {
    #[serde(default)]
    name: String,
}

/// Mint or rename the pen name, returning the one now in force.
///
/// Both operations land here because the caller's question is the same either
/// way. The byline also moves on every zine this pen name already wrote, since a
/// display name is not an identity: the address does not move, so nothing
/// already published is invalidated.
async fn zines_set_pen_name(
    State(state): State<Arc<App>>,
    Json(body): Json<PenNameBody>,
) -> Result<Json<ZinesView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let name = body.name.trim();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "a pen name needs a byline".to_string(),
            }),
        ));
    }
    let pen_name = match state.worker.set_pen_name(name).await {
        WorkResult::PenName(Some(pen)) => pen,
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };
    // The byline the UI shows on existing articles follows the pen name, so the
    // rename is applied to the corpus in the same request that mints it.
    let mut parsed = zines_sidecar(&state).await?;
    if parsed.rename_byline(&pen_name.id, &pen_name.name, paper_now_ms()) {
        zines_persist(&state, &parsed).await?;
    }
    Ok(Json(zines_view(&parsed, None, None, Some(pen_name))))
}

fn zine_not_held(zine_id: &str) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: format!("no zine is held for {zine_id}"),
        }),
    )
}

/// Fetch the raw papers sidecar bytes through the worker and decode them.
async fn paper_sidecar(
    state: &App,
) -> Result<paper_store::PapersState, (StatusCode, Json<ApiError>)> {
    match state.worker.secret_read(paper_store::STATE_FILE).await {
        WorkResult::Secret(raw) => paper_store::decode(raw.as_deref()).map_err(|m| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: m }),
            )
        }),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Persist the papers sidecar wholesale.
async fn paper_persist(
    state: &App,
    parsed: &paper_store::PapersState,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let bytes = parsed.encode().map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    match state.worker.secret_write(paper_store::STATE_FILE, bytes).await {
        WorkResult::Ok => Ok(()),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn paper_account_id(state: &App) -> Result<String, (StatusCode, Json<ApiError>)> {
    match state.worker.account_id().await {
        WorkResult::AccountId(id) => Ok(id),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Apply one paper payload received from a peer to the local sidecar.
///
/// Authority mirrors the conclave's, and every check is on the **sender**:
/// - `PaperDelta` carries no authority at all. A CRDT merges an update
///   anywhere, in any order, so it is applied whoever sent it.
/// - `PaperState` is creator-authoritative at creation: a paper this vault has
///   never seen is materialised only when the sender is the creator the
///   descriptor names, so a trustee cannot invent a paper in someone else's
///   name.
/// - `PaperSnapshot` is merged exactly like a delta rather than replacing the
///   snapshot: a full state is a valid v1 update, and merging keeps local edits
///   the hub had not seen, where replacing would drop them.
/// - `PaperHub` (convener-authoritative succession) and `PaperPresence` (v1
///   sends presence on the DataChannel only) are accepted and ignored rather
///   than treated as errors.
///
/// Returns whether the local document changed. An unusable payload is an error
/// for the caller to log, never a reason to fail the whole poll.
fn paper_apply_inbound(
    parsed: &mut paper_store::PapersState,
    from_account: &str,
    payload: &MessagePayload,
    now_ms: u64,
) -> Result<bool, String> {
    match payload {
        MessagePayload::PaperState { paper } => {
            if paper.creator != from_account {
                return Err(format!(
                    "paper {} names {} as its creator but came from {}",
                    paper.paper_id, paper.creator, from_account
                ));
            }
            parsed.adopt(paper.clone(), now_ms)
        }
        MessagePayload::PaperDelta { paper_id, update } => {
            let conclave_id = parsed
                .conclave_of(paper_id)
                .ok_or_else(|| format!("no paper {paper_id} in this vault"))?;
            parsed.apply_update(&conclave_id, from_account, update, now_ms)?;
            Ok(true)
        }
        MessagePayload::PaperSnapshot { paper, state } => {
            // A snapshot is self-contained: the peer that needs one joined after
            // the paper existed, so it never saw the `PaperState` fan-out and
            // has no local record to apply against. Adopt the descriptor first,
            // under the same creator authority `PaperState` enforces, so a
            // snapshot cannot introduce a paper on a peer's behalf. An already
            // known paper keeps its local descriptor — only the state merges,
            // which is why a peer cannot retitle or re-home via a snapshot.
            let conclave_id = match parsed.conclave_of(&paper.paper_id) {
                Some(id) => id,
                None => {
                    if paper.creator != from_account {
                        return Err(format!(
                            "paper {} names {} as its creator but came from {}",
                            paper.paper_id, paper.creator, from_account
                        ));
                    }
                    parsed.adopt(paper.clone(), now_ms)?;
                    // `adopt` settles a create collision by conclave, so the id
                    // it resolved may not be the one the snapshot named: a
                    // snapshot for the paper that lost belongs to the conclave's
                    // paper all the same, because the two are one document at
                    // two ids and merging is what the loser needs anyway.
                    parsed
                        .conclave_of(&paper.paper_id)
                        .or_else(|| {
                            (!paper.conclave_id.is_empty()
                                && parsed.get(&paper.conclave_id).is_some())
                            .then(|| paper.conclave_id.clone())
                        })
                        .ok_or_else(|| format!("no paper {} in this vault", paper.paper_id))?
                }
            };
            parsed.apply_update(&conclave_id, from_account, state, now_ms)?;
            Ok(true)
        }
        // Convener-authoritative hub succession. The authority check already
        // happened in `recv`, where the conclave record is in hand — what is
        // left here is storage, and only the rendezvous moves.
        MessagePayload::PaperHub { paper } => parsed.rehome(&paper.paper_id, &paper.hub, now_ms),
        // Recognised paper payloads that change no document state. Signalling
        // belongs here rather than in the state path because it moves the
        // *transport*, not the text: the data plane consumes it before it
        // reaches this function (Task 6 Step 2). Until that wiring lands it is
        // accepted and ignored, not reported as a foreign payload.
        MessagePayload::PaperPresence { .. }
        | MessagePayload::PaperOffer { .. }
        | MessagePayload::PaperAnswer { .. } => Ok(false),
        other => Err(format!("not a paper payload: {other:?}")),
    }
}

/// Move a paper's hub to this vault when the one that held it is gone.
///
/// Two places see the joined set change, and only one of them is a `recv`:
///
/// - a peer's leave or kick arrives as a payload, and `recv` reports the
///   departure in `inbox.departed`;
/// - the convener's *own* kick changes the roster with no payload at all — the
///   roster is already updated by the time the next poll takes its "before"
///   snapshot, so the diff is empty and nothing is reported.
///
/// Both call this. The core decides: it is the convener's claim to make, and
/// only when the paper's rendezvous is really no longer a joined trustee — so a
/// kick of somebody who is not the hub moves nothing, and the caller does not
/// have to go and find the account id of whoever left.
///
/// Returns whether the rendezvous moved here, which is what the caller needs to
/// know to drop the plane that points at the old hub.
async fn paper_inherit_hub(state: &Arc<App>, conclave_id: &str) -> bool {
    let Ok(mut parsed) = paper_sidecar(state).await else {
        return false;
    };
    let Some(paper) = parsed
        .get(conclave_id)
        .map(|file| file.descriptor.clone())
    else {
        return false;
    };
    let hub = paper.hub.clone();
    let moved = match state
        .worker
        .inherit_paper_hub(conclave_id, &hub, paper)
        .await
    {
        WorkResult::Rehomed(Some(moved)) => moved,
        // `None` is the common case: a trustee that is not the convener, or a
        // hub that is still here. Not an error, and not worth logging.
        _ => return false,
    };
    match paper_apply_inbound(
        &mut parsed,
        "",
        &MessagePayload::PaperHub { paper: moved },
        paper_now_ms(),
    ) {
        Ok(true) => {
            if let Err(e) = paper_persist(state, &parsed).await {
                eprintln!(
                    "hush-daemon: paper {conclave_id} could not be saved: {}",
                    e.1.error
                );
            }
            true
        }
        Ok(false) => false,
        Err(e) => {
            eprintln!("hush-daemon: inherited hub for {conclave_id} ignored: {e}");
            false
        }
    }
}

/// The refusal a paper write gets when the conclave no longer allows one.
///
/// Two cases, and they are not the same sentence:
///
/// - the conclave has **ended** (`ConclaveEnd`): read-only on every peer, and
///   the relay would refuse the fan-out, so accepting an edit here would only
///   diverge this vault's copy from everyone else's;
/// - this vault is **not a joined trustee** — kicked, or an invitation that was
///   never accepted: the same fan-out refusal, and a local-only edit nobody
///   else will ever see is worse than a refusal.
///
/// Reads are untouched by both: the document is this vault's own copy, and
/// refusing to show it would not un-share it.
async fn paper_write_refusal(
    state: &Arc<App>,
    conclave_id: &str,
) -> Option<(StatusCode, Json<ApiError>)> {
    let list = match state.worker.conclaves().await {
        WorkResult::Conclaves(list) => list,
        // A worker that cannot answer is not a reason to refuse: a locked vault
        // fails the paper route that follows anyway, and a transient read error
        // must not turn into a vault that silently refuses to save an edit.
        _ => return None,
    };
    let Some(conclave) = list.iter().find(|c| c.id == conclave_id) else {
        // Not a conclave this vault knows: the paper route 404s on its own.
        return None;
    };
    if conclave.ended {
        return Some(paper_ended_error());
    }
    let me = paper_account_id(state).await.ok()?;
    if conclave.joined().any(|t| t.account_id == me) {
        return None;
    }
    Some((
        StatusCode::FORBIDDEN,
        Json(ApiError {
            error: "you are not a trustee of this conclave, so the paper is read-only".into(),
        }),
    ))
}

/// The refusal an ended conclave's paper writes get.
///
/// `ConclaveEnd` makes a conclave read-only on every peer, and the paper is part
/// of that: the relay would refuse the fan-out anyway, so accepting the edit
/// here would only diverge this vault's copy from everyone else's.
fn paper_ended_error() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::FORBIDDEN,
        Json(ApiError {
            error: "the conclave has ended, so the paper is read-only".into(),
        }),
    )
}

/// Re-home a paper: the convener's word settles where the rendezvous is.
#[derive(Deserialize)]
struct PaperRehomeRequest {
    /// The trustee that should become the hub.
    hub: String,
}

/// Move a paper's DataChannel rendezvous to another trustee.
///
/// The authority check is the core's, not the daemon's:
/// `send_conclave_paper_hub` refuses anyone but the conclave's convener, so
/// there is no second copy of the roster here to drift. A refusal is a 403
/// rather than a 400 — the caller may ask, they are simply not the convener —
/// which is the same distinction `change_passphrase` draws for a wrong
/// passphrase.
///
/// This is the *explicit* half of succession. The default is automatic: when a
/// paper's hub leaves or is kicked, the convener inherits it in `recv` with no
/// user action. This route exists for the case that has no roster event — the
/// hub is still a member but is gone for good (a dead machine, a lost device).
async fn paper_rehome(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
    Json(body): Json<PaperRehomeRequest>,
) -> Result<Json<PaperView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    if body.hub.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "a re-home needs the account id of the new hub".into(),
            }),
        ));
    }
    let mut parsed = paper_sidecar(&state).await?;
    let mut descriptor = parsed
        .get(&conclave_id)
        .map(|file| file.descriptor.clone())
        .ok_or_else(|| {
            (
                PAPER_NOT_FOUND.1,
                Json(ApiError {
                    error: PAPER_NOT_FOUND.0.into(),
                }),
            )
        })?;
    descriptor.hub = body.hub.trim().to_string();
    match state.worker.send_paper_hub(&conclave_id, descriptor).await {
        WorkResult::Ok => {}
        WorkResult::Err(msg) if msg.contains("convener") => {
            return Err((StatusCode::FORBIDDEN, Json(ApiError { error: msg })));
        }
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    }
    let paper_id = parsed
        .get(&conclave_id)
        .map(|file| file.descriptor.paper_id.clone())
        .expect("the paper existed a moment ago");
    parsed
        .rehome(&paper_id, &body.hub.trim().to_string(), paper_now_ms())
        .map_err(|m| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: m }),
            )
        })?;
    paper_persist(&state, &parsed).await?;
    // The open channel points at the rendezvous that just moved.
    paper_plane_reset(&state, &conclave_id).await;
    let path = paper_path_view(&state, &conclave_id).await;
    let file = parsed
        .get(&conclave_id)
        .expect("the paper existed a moment ago");
    let updates: Vec<Vec<u8>> = file.log.iter().map(|u| u.update.clone()).collect();
    let full = paper_doc::fold(&file.snapshot, &updates).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    Ok(Json(paper_view_of(
        file,
        STANDARD.encode(full),
        false,
        path,
    )))
}

/// One peer's presence frame.
#[derive(Serialize)]
struct PresenceView {
    /// The account the frame came from.
    account: String,
    /// The opaque awareness bytes, base64. The daemon never reads them.
    frame: String,
}

/// Who is in the document, as far as the fast path knows.
#[derive(Serialize)]
struct PaperPresenceView {
    peers: Vec<PresenceView>,
}

/// A presence frame the editor just produced.
#[derive(Deserialize)]
struct PaperPresenceRequest {
    /// Opaque awareness bytes, base64.
    frame: String,
}

/// Who else is in the paper right now.
///
/// Presence lives only on the fast path: there is no relay fallback and no
/// persistence, because a cursor that arrives a second and a half late is worse
/// than no cursor, and a stale one is a lie about who is reading. An empty list
/// is therefore a real answer — it means nobody else is reachable quickly —
/// rather than a failure, and the editor draws no cursors for it.
async fn paper_presence(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<PaperPresenceView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    paper_sidecar(&state).await?.get(&conclave_id).ok_or_else(|| {
        (
            PAPER_NOT_FOUND.1,
            Json(ApiError {
                error: PAPER_NOT_FOUND.0.into(),
            }),
        )
    })?;
    let peers = match paper_open_plane(&state, &conclave_id).await {
        Some(plane) => plane
            .presence()
            .into_iter()
            .map(|(account, bytes)| PresenceView {
                account,
                frame: STANDARD.encode(bytes),
            })
            .collect(),
        None => Vec::new(),
    };
    Ok(Json(PaperPresenceView { peers }))
}

/// Publish the editor's own presence frame along the star.
///
/// A trustee's frame goes to the hub, the hub's goes to every trustee, and the
/// hub forwards what it receives — which is what makes a cursor on one screen
/// visible on the others. Best-effort by construction, like every other use of
/// the fast path: no channel means the frame is dropped, and the editor keeps
/// typing at relay latency with no cursors, exactly as it did before Task 7.
async fn paper_publish_presence(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
    Json(body): Json<PaperPresenceRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let bytes = STANDARD.decode(body.frame.as_bytes()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "frame is not valid base64".into(),
            }),
        )
    })?;
    paper_sidecar(&state).await?.get(&conclave_id).ok_or_else(|| {
        (
            PAPER_NOT_FOUND.1,
            Json(ApiError {
                error: PAPER_NOT_FOUND.0.into(),
            }),
        )
    })?;
    let Some(plane) = paper_open_plane(&state, &conclave_id).await else {
        // No plane yet: the editor's next poll dials and presence resumes on its
        // own. Spawned sends below for the same reason an update's are — a
        // channel that has gone quiet must not hold the request open.
        return Ok(StatusCode::NO_CONTENT);
    };
    for peer in plane.peers() {
        let plane = Arc::clone(&plane);
        let bytes = bytes.clone();
        tokio::spawn(async move {
            let _ = plane.send_presence(&peer, &bytes).await;
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn paper_get(    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<PaperView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = paper_sidecar(&state).await?;
    let file = parsed
        .get(&conclave_id)
        .ok_or_else(|| (PAPER_NOT_FOUND.1, Json(ApiError { error: PAPER_NOT_FOUND.0.into() })))?;
    let updates: Vec<Vec<u8>> = file.log.iter().map(|u| u.update.clone()).collect();
    let full = paper_doc::fold(&file.snapshot, &updates).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    // Opening the paper is also where the fast path comes up: a trustee dials
    // its hub, a hub does nothing and waits to be dialled. Best-effort and
    // rate-limited, so a poll that finds no change costs a mutex.
    let descriptor = file.descriptor.clone();
    paper_dial(&state, &conclave_id, &descriptor).await;
    let fast_path = paper_path_view(&state, &conclave_id).await;
    Ok(Json(paper_view_of(
        file,
        STANDARD.encode(full),
        false,
        fast_path,
    )))
}

async fn paper_create(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
    Json(body): Json<PaperCreateRequest>,
) -> Result<Json<PaperView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    // A paper in a conclave this vault cannot write to would be local-only: the
    // `PaperState` fan-out is refused, so no peer would ever learn it exists.
    if let Some(refusal) = paper_write_refusal(&state, &conclave_id).await {
        return Err(refusal);
    }
    let creator = paper_account_id(&state).await?;
    let mut parsed = paper_sidecar(&state).await?;
    parsed
        .create(&conclave_id, &creator, &body.title, paper_now_ms())
        .map_err(|m| {
            if m.contains("already") {
                (StatusCode::CONFLICT, Json(ApiError { error: m }))
            } else {
                (StatusCode::BAD_REQUEST, Json(ApiError { error: m }))
            }
        })?;
    paper_persist(&state, &parsed).await?;
    // A brand-new paper has no plane yet — this daemon is its creator, so it is
    // the hub and waits to be dialled. The badge starts at `relay` and turns
    // `live` the first time a trustee connects.
    let path = paper_path_view(&state, &conclave_id).await;
    let file = parsed.get(&conclave_id).expect("just created");
    let descriptor = file.descriptor.clone();
    let view = paper_view_of(file, STANDARD.encode(yrs::Update::EMPTY_V1), false, path);
    // Tell the conclave's trustees the paper exists. Best-effort: the paper is
    // already local, and a send queues in the outbox, so a trustee that is
    // offline or locked still receives it on its next drain.
    if let WorkResult::Err(e) = state
        .worker
        .send_paper_state(&conclave_id, descriptor)
        .await
    {
        eprintln!("hush-daemon: paper state fan-out failed: {e}");
    }
    Ok(Json(view))
}

async fn paper_update(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
    Json(body): Json<PaperUpdateRequest>,
) -> Result<Json<PaperView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    if let Some(refusal) = paper_write_refusal(&state, &conclave_id).await {
        return Err(refusal);
    }
    let raw = STANDARD
        .decode(body.update.as_bytes())
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "update is not valid base64".into(),
                }),
            )
        })?;
    let origin = paper_account_id(&state).await?;
    let mut parsed = paper_sidecar(&state).await?;
    // Merge onto the document as it stands: fold the pending log, apply the
    // incoming update, then record it — the CRDT makes re-applying our own or
    // a peer's update a no-op, so the same path serves every sender.
    let (snapshot, pending): (Vec<u8>, Vec<Vec<u8>>) = {
        let file = parsed.get(&conclave_id).ok_or_else(|| {
            (
                PAPER_NOT_FOUND.1,
                Json(ApiError {
                    error: PAPER_NOT_FOUND.0.into(),
                }),
            )
        })?;
        (
            file.snapshot.clone(),
            file.log.iter().map(|u| u.update.clone()).collect(),
        )
    };
    let base = paper_doc::fold(&snapshot, &pending).map_err(|m| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError { error: m }),
        )
    })?;
    let full = paper_doc::apply(&base, &raw)
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;

    parsed
        .apply_update(&conclave_id, &origin, &raw, paper_now_ms())
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    let mut compacted = false;
    if parsed
        .get(&conclave_id)
        .map(|p| p.log.len())
        .unwrap_or(0)
        >= PAPER_COMPACT_AFTER
    {
        parsed
            .snapshot_replace(&conclave_id, &full, paper_now_ms())
            .map_err(|m| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError { error: m }),
                )
            })?;
        compacted = true;
    }
    paper_persist(&state, &parsed).await?;
    let path = paper_path_view(&state, &conclave_id).await;
    let file = parsed
        .get(&conclave_id)
        .expect("the paper existed a moment ago");
    let paper_id = file.descriptor.paper_id.clone();
    let view = paper_view_of(file, STANDARD.encode(full), compacted, path);
    // The fast path first, then the relay, with identical bytes: the only
    // question the channel answers is which copy arrives first, and the relay is
    // what makes that question unimportant. Spawned rather than awaited so a
    // channel that has gone quiet cannot hold the editor's request open — the
    // relay copy is going out below regardless.
    if let Some(plane) = paper_open_plane(&state, &conclave_id).await {
        for peer in plane.peers() {
            let plane = Arc::clone(&plane);
            let bytes = raw.clone();
            tokio::spawn(async move {
                let _ = plane.send(&peer, &bytes).await;
            });
        }
    }
    // Fan the update out to the trustees. This is the *local* edit path, so it
    // sends exactly the bytes the editor produced and nothing else; an update
    // that arrived from a peer is applied by `recv` and never re-fanned, which
    // is what keeps the fan-out from looping.
    if let WorkResult::Err(e) = state
        .worker
        .send_paper_delta(&conclave_id, &paper_id, &raw)
        .await
    {
        eprintln!("hush-daemon: paper delta fan-out failed: {e}");
    }
    Ok(Json(view))
}

// ---------------------------------------------------------------------------
// Conclave Paper — the DataChannel fast path (Task 6 Step 2).
//
// The relay fan-out above is the guarantee; this is the latency. One plane per
// paper, created the first time the paper is opened on this daemon and drained
// by a single background loop. Every path here is best-effort by construction:
// a failure ends in "the update rides the relay", never in a lost edit, which
// is why none of it returns an error to a caller.
// ---------------------------------------------------------------------------

/// How often the drain loop looks at the planes.
///
/// Two orders of magnitude below the relay's ~1–3 s cadence, and one wake-up
/// per plane per tick, so an idle daemon costs nothing.
const PAPER_PLANE_TICK_MS: u64 = 50;

/// The plane for `conclave_id`, created on first use.
///
/// `stun` is resolved from config here rather than stored on the plane, so an
/// operator who adds a STUN server gets it on the next paper without a restart
/// and there is still exactly one config system (DR-14).
async fn paper_plane(state: &Arc<App>, conclave_id: &str) -> Arc<paper_data::DataPlane> {
    let mut planes = state.paper_planes.lock().await;
    planes
        .entry(conclave_id.to_string())
        .or_insert_with(|| {
            Arc::new(paper_data::DataPlane::new(
                hush_core::config::resolve_stun_url(&state.dir),
            ))
        })
        .clone()
}

/// The plane for a paper, when one has already been opened here.
async fn paper_open_plane(
    state: &Arc<App>,
    conclave_id: &str,
) -> Option<Arc<paper_data::DataPlane>> {
    state
        .paper_planes
        .lock()
        .await
        .get(conclave_id)
        .map(Arc::clone)
}

/// Find a paper this vault holds, by the id the wire carries.
///
/// Signalling names the paper and not the conclave, because that is all a peer
/// knows; the sidecar is the mapping. `None` is a signal about a paper this
/// vault does not have, which is dropped rather than treated as an error.
async fn paper_lookup(state: &Arc<App>, paper_id: &str) -> Option<(String, PaperDescriptor)> {
    let parsed = paper_sidecar(state).await.ok()?;
    let conclave_id = parsed.conclave_of(paper_id)?;
    let descriptor = parsed.get(&conclave_id)?.descriptor.clone();
    Some((conclave_id, descriptor))
}

/// Bring the fast path up for a paper this daemon has open.
///
/// The trustee dials and the hub waits, because the star's shape follows from
/// who created the paper and the descriptor already names them. Called on every
/// open rather than once: a restart, a lock, or a hub that went away all leave
/// the plane empty, and the open is the natural moment to notice. The dial is
/// rate-limited by [`paper_data::DataPlane::should_dial`], so the UI's
/// one-second poll cannot turn into one offer per second.
///
/// Best-effort throughout — a paper with no channel is a paper at relay
/// latency, not a failure.
async fn paper_dial(state: &Arc<App>, conclave_id: &str, paper: &PaperDescriptor) {
    let Ok(me) = paper_account_id(state).await else {
        return;
    };
    // We are the hub: peers dial us. An offer that never comes is not ours to
    // chase, and there is nobody to offer to.
    if paper.hub == me {
        return;
    }
    let plane = paper_plane(state, conclave_id).await;
    if !plane.should_dial(&paper.hub) {
        return;
    }
    let sdp = match plane.offer(&paper.hub).await {
        Ok(sdp) => sdp,
        Err(e) => {
            eprintln!("hush-daemon: paper {conclave_id} dial failed: {e}");
            return;
        }
    };
    let payload = MessagePayload::PaperOffer {
        paper_id: paper.paper_id.clone(),
        sdp,
    };
    if let WorkResult::Err(e) = state
        .worker
        .send_paper_signal(conclave_id, &paper.hub, payload)
        .await
    {
        eprintln!("hush-daemon: paper {conclave_id} offer could not be sent: {e}");
    }
}

/// Move the transport for one signalling payload, if it is ours.
///
/// Returns whether the payload was a paper signal. Signals are consumed here
/// and never reach `paper_apply_inbound`: they move the *transport*, not the
/// text, so they are the one paper payload class that changes no document.
async fn paper_handle_signal(state: &Arc<App>, from: &str, payload: &MessagePayload) -> bool {
    match payload {
        // Hub half. Authority is the descriptor's, exactly as it is for
        // `PaperState`: only the daemon the paper names as its hub answers, so
        // a trustee cannot be talked into holding a channel for a paper whose
        // descriptor says someone else owns the rendezvous.
        MessagePayload::PaperOffer { paper_id, sdp } => {
            let Some((conclave_id, descriptor)) = paper_lookup(state, paper_id).await else {
                return true;
            };
            let Ok(me) = paper_account_id(state).await else {
                return true;
            };
            if descriptor.hub != me {
                return true;
            }
            let plane = paper_plane(state, &conclave_id).await;
            let answer = match plane.answer(from, sdp).await {
                Ok(answer) => answer,
                Err(e) => {
                    eprintln!("hush-daemon: paper {conclave_id} could not answer {from}: {e}");
                    return true;
                }
            };
            let payload = MessagePayload::PaperAnswer {
                paper_id: paper_id.clone(),
                sdp: answer,
            };
            if let WorkResult::Err(e) = state
                .worker
                .send_paper_signal(&conclave_id, from, payload)
                .await
            {
                // The answer could not be addressed, which in practice means
                // the sender is not a joined trustee of this conclave: the
                // addressed send refuses exactly that. Drop the connection with
                // it, because a half-open channel to a peer that cannot be
                // answered is only cost.
                eprintln!("hush-daemon: paper {conclave_id} answer to {from} failed: {e}");
                plane.forget(from);
            }
            true
        }
        // Trustee half: only an answer to a dial this daemon made has an
        // outstanding connection to match, and `accept_answer` is what says so.
        MessagePayload::PaperAnswer { paper_id, sdp } => {
            let Some((conclave_id, _)) = paper_lookup(state, paper_id).await else {
                return true;
            };
            let Some(plane) = paper_open_plane(state, &conclave_id).await else {
                return true;
            };
            if let Err(e) = plane.accept_answer(from, sdp).await {
                eprintln!("hush-daemon: paper {conclave_id} answer from {from} refused: {e}");
            }
            true
        }
        _ => false,
    }
}

/// Apply everything the channels have received, then pass it along the star.
///
/// The re-broadcast is the reason this loop exists. The relay fan-out is a full
/// mesh, but the channels are a star: without forwarding, an update that
/// reached the hub over one channel would reach the other trustees only at
/// relay speed, which is the whole thing the fast path is for.
///
/// "Every live peer except the sender" is the right fan-out for both roles
/// without asking which one this daemon is — a trustee's only peer *is* the hub,
/// so it forwards nothing.
///
/// A locked vault simply drops the batch: the relay copy is still queued and
/// applying it is what happens on unlock, so that is a delay, not a loss.
async fn paper_drain_once(state: &Arc<App>) {
    let planes: Vec<(String, Arc<paper_data::DataPlane>)> = {
        let planes = state.paper_planes.lock().await;
        planes
            .iter()
            .map(|(id, plane)| (id.clone(), Arc::clone(plane)))
            .collect()
    };
    for (conclave_id, plane) in planes {
        let inbound = plane.drain();
        if inbound.is_empty() {
            continue;
        }
        let Ok(mut parsed) = paper_sidecar(state).await else {
            continue;
        };
        let Some(paper_id) = parsed
            .get(&conclave_id)
            .map(|file| file.descriptor.paper_id.clone())
        else {
            continue;
        };
        let now = paper_now_ms();
        let mut changed = false;
        let mut forwards: Vec<(String, paper_data::InboundFrame)> = Vec::new();
        for paper_data::Inbound { from, frame } in inbound {
            match frame {
                paper_data::InboundFrame::Update(update) => {
                    let payload = MessagePayload::PaperDelta {
                        paper_id: paper_id.clone(),
                        update: update.clone(),
                    };
                    if let Err(e) = paper_apply_inbound(&mut parsed, &from, &payload, now) {
                        // Unappliable is also unforwardable: passing on an update
                        // this vault rejected would spread a bad payload along
                        // the star.
                        eprintln!("hush-daemon: paper update from {from} ignored: {e}");
                        continue;
                    }
                    changed = true;
                    forwards.push((from, paper_data::InboundFrame::Update(update)));
                }
                // Presence changes no document, so it is neither applied nor
                // persisted — it is forwarded, which is what makes a cursor on
                // one trustee visible to the others instead of only to the hub.
                presence @ paper_data::InboundFrame::Presence(_) => {
                    forwards.push((from, presence));
                }
            }
        }
        if changed {
            if let Err(e) = paper_persist(state, &parsed).await {
                eprintln!("hush-daemon: paper {conclave_id} could not be saved: {}", e.1.error);
            }
        }
        for (from, frame) in forwards {
            for peer in plane.peers() {
                if peer == from {
                    continue;
                }
                // `UseRelay` needs no handling: the relay fan-out is already
                // carrying these exact bytes to that peer for an update, and
                // presence has no relay copy at all — a cursor is not worth a
                // store-and-forward round trip.
                match &frame {
                    paper_data::InboundFrame::Update(bytes) => {
                        let _ = plane.send(&peer, bytes).await;
                    }
                    paper_data::InboundFrame::Presence(bytes) => {
                        let _ = plane.send_presence(&peer, bytes).await;
                    }
                }
            }
        }
    }
}

/// Spawn the paper drain loop. One per daemon, for the process's lifetime.
fn spawn_paper_plane_loop(app: Arc<App>) {
    tokio::spawn(async move {
        let tick = Duration::from_millis(PAPER_PLANE_TICK_MS);
        loop {
            tokio::time::sleep(tick).await;
            paper_drain_once(&app).await;
        }
    });
}

/// Close every paper plane and forget them.
///
/// The registry is emptied rather than just closed, so the next paper open
/// dials afresh instead of finding a plane full of dead links. Best-effort:
/// `shutdown` closes what it can and a connection that refuses to close is not
/// worth holding a lock over.
async fn paper_planes_shutdown(state: &Arc<App>) {
    let planes: Vec<Arc<paper_data::DataPlane>> = {
        let mut planes = state.paper_planes.lock().await;
        planes.drain().map(|(_, plane)| plane).collect()
    };
    for plane in planes {
        plane.shutdown();
    }
}

/// Drop one paper's plane, if it has one.
///
/// Used when the paper's hub moves: the channel that exists points at the old
/// rendezvous, so keeping it would leave the trustee talking to a daemon that
/// is no longer the hub.
async fn paper_plane_reset(state: &Arc<App>, conclave_id: &str) {
    let plane = state.paper_planes.lock().await.remove(conclave_id);
    if let Some(plane) = plane {
        plane.shutdown();
    }
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
            paper_planes: Mutex::new(HashMap::new()),
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
            paper_planes: Mutex::new(HashMap::new()),
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
    async fn a_named_vault_secret_round_trips_sealed_and_can_be_forgotten() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let router = api_router(wallet_app(dir.path(), worker, None).await);

        // An unused name is a 404, which a client must be able to tell apart
        // from "stored, and empty".
        let (status, _) = request(&router, "GET", "/api/vault/secret/irc-client-cert", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let body =
            serde_json::json!({ "value_base64": STANDARD.encode(b"pem-bytes") }).to_string();
        let (status, _) = request(
            &router,
            "PUT",
            "/api/vault/secret/irc-client-cert",
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, json) =
            request(&router, "GET", "/api/vault/secret/irc-client-cert", None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["value_base64"], STANDARD.encode(b"pem-bytes"));

        // The sidecar on disk is sealed: the plaintext we sent must not appear.
        let on_disk = std::fs::read(dir.path().join("irc-client-cert")).unwrap();
        assert!(
            !on_disk
                .windows(b"pem-bytes".len())
                .any(|w| w == &b"pem-bytes"[..]),
            "the sidecar must not hold the plaintext"
        );

        let (status, _) =
            request(&router, "DELETE", "/api/vault/secret/irc-client-cert", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = request(&router, "GET", "/api/vault/secret/irc-client-cert", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_secret_name_that_could_escape_the_vault_is_refused() {
        let dir = TempDir::new().unwrap();
        let worker = unlocked(dir.path()).await;
        let router = api_router(wallet_app(dir.path(), worker, None).await);

        // `..` and a separator are both refused before the vault sees them. An
        // empty name never reaches the handler at all (no path segment to
        // match), so it comes back as a plain 404.
        for name in ["..%2Fescape", "a%2Fb", ""] {
            let (status, json) = request(
                &router,
                "GET",
                &format!("/api/vault/secret/{name}"),
                None,
            )
            .await;
            assert!(
                status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
                "{name} should be refused, got {status}: {json}"
            );
        }
        // And nothing was written outside the vault.
        assert!(!dir.path().parent().unwrap().join("escape").exists());
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

/// The Notes routes driven end to end over a real unlocked vault, through
/// `api_router` so no socket is bound.
#[cfg(test)]
mod notes_api_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;

    pub(crate) fn build_app(dir: &std::path::Path, worker: AccountWorker, unlocked: bool) -> Arc<App> {
        Arc::new(App {
            dir: dir.to_path_buf(),
            socket: None,
            tcp_addr: None,
            worker: worker.clone(),
            reads: reads::ReadsClient::new(&dir.join("reads-cache")),
            torrents: Arc::new(torrents::TorrentsManager::new(worker.clone(), dir)),
            wallet: Arc::new(wallet::WalletManager::new(worker, dir)),
            unlocked: Mutex::new(unlocked),
            unlocked_at: Mutex::new(None),
            relay_socks: Mutex::new(None),
            wallet_socks: Mutex::new(None),
            relay_health: Mutex::new(None),
            relay_probe_inflight: AtomicBool::new(false),
            paper_planes: Mutex::new(HashMap::new()),
        })
    }

    /// A created vault plus the worker that holds it, unlocked.
    pub(crate) async fn unlocked_app(dir: &std::path::Path) -> Arc<App> {
        hush_core::Account::create(dir, "pw", "http://127.0.0.1:9")
            .await
            .unwrap();
        let worker = AccountWorker::spawn();
        assert!(matches!(
            worker.unlock(&dir.to_path_buf(), "pw").await,
            WorkResult::Ok
        ));
        build_app(dir, worker, true)
    }

    pub(crate) async fn request(
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
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            })
        };
        (status, json)
    }

    #[tokio::test]
    async fn notes_routes_round_trip_a_note() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);

        // The board starts empty.
        let (status, state) = request(&router, "GET", "/api/notes/state", None).await;
        assert_eq!(status, StatusCode::OK, "{state}");
        assert_eq!(state["board_id"], "default");
        assert_eq!(state["notes"].as_array().unwrap().len(), 0);

        // Create.
        let (status, created) = request(
            &router,
            "POST",
            "/api/notes/note",
            Some(r#"{"title":"Groceries","body":"milk"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["state"]["notes"].as_array().unwrap().len(), 1);

        // Update in place: same id, one note, new body.
        let update = format!(r#"{{"id":"{id}","title":"Groceries","body":"milk and eggs"}}"#);
        let (status, updated) = request(&router, "POST", "/api/notes/note", Some(&update)).await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["id"], id);
        assert_eq!(updated["state"]["notes"][0]["body"], "milk and eggs");

        // Persisted: a fresh read re-decodes the sidecar.
        let (status, state) = request(&router, "GET", "/api/notes/state", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(state["notes"][0]["id"], id);
        assert_eq!(state["notes"][0]["body"], "milk and eggs");

        // Delete.
        let (status, state) =
            request(&router, "DELETE", &format!("/api/notes/note/{id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{state}");
        assert_eq!(state["notes"].as_array().unwrap().len(), 0);

        // Updating a deleted id is refused, not silently re-created.
        let ghost = format!(r#"{{"id":"{id}","title":"x","body":""}}"#);
        let (status, _) = request(&router, "POST", "/api/notes/note", Some(&ghost)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn notes_state_is_401_while_locked() {
        let dir = TempDir::new().unwrap();
        let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
        let (status, _) = request(&router, "GET", "/api/notes/state", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

/// The Paper routes driven end to end over a real unlocked vault, through
/// `api_router` so no socket is bound. Phase 1 is offline: the paper belongs
/// to a conclave id the UI already has; conclave-lifecycle coupling (create
/// validating membership, `ConclaveEnd` read-only, deletion cleanup) lands
/// with the wire fan-out in Phase 2, where the two-daemon script can test it.
#[cfg(test)]
mod paper_api_tests {
    use super::*;
    use crate::notes_api_tests::{build_app, request, unlocked_app};
    use tempfile::TempDir;

    /// A real v1 Yjs update, encoded the way any peer (the webview editor
    /// included) produces them: one transaction on a fresh doc.
    fn yjs_update(text: &str) -> String {
        use yrs::{Doc, ReadTxn, StateVector, Text, Transact};
        let doc = Doc::new();
        let content = doc.get_or_insert_text("content");
        let mut txn = doc.transact_mut();
        content.insert(&mut txn, 0, text);
        STANDARD.encode(txn.encode_state_as_update_v1(&StateVector::default()))
    }

    #[tokio::test]
    async fn paper_routes_round_trip_a_document() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);

        // No paper yet.
        let (status, _) = request(&router, "GET", "/api/paper/c-1", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Create: creator and hub are this vault's own account id.
        let (status, created) = request(
            &router,
            "POST",
            "/api/paper/c-1/create",
            Some(r#"{"title":"Minutes"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let creator = created["paper"]["creator"].as_str().unwrap().to_string();
        assert!(!creator.is_empty(), "creator is the local account id");
        assert_eq!(created["paper"]["hub"], created["paper"]["creator"]);
        assert_eq!(created["paper"]["title"], "Minutes");
        assert_eq!(created["revision"], 0);
        // A brand-new paper's state must be a decodable update, never zero
        // bytes: the webview feeds it straight to Y.applyUpdate, which throws
        // "Unexpected end of array" on an empty buffer.
        let fresh = created["state"].as_str().unwrap();
        assert!(!fresh.is_empty(), "a fresh paper's state is not zero bytes");
        let fresh_bytes = STANDARD.decode(fresh).unwrap();
        use yrs::updates::decoder::Decode;
        assert!(
            yrs::Update::decode_v1(&fresh_bytes).is_ok(),
            "a fresh paper's state decodes as a v1 update"
        );

        // A second create is a conflict, not a silent reset.
        let (status, _) = request(
            &router,
            "POST",
            "/api/paper/c-1/create",
            Some(r#"{"title":"x"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // One edit.
        let body = format!(r#"{{"update":"{}"}}"#, yjs_update("hello"));
        let (status, upd) = request(&router, "POST", "/api/paper/c-1/update", Some(&body)).await;
        assert_eq!(status, StatusCode::OK, "{upd}");
        assert_eq!(upd["revision"], 1);
        assert_eq!(upd["compacted"], false);

        // The fetch reflects it: revision, one log entry authored locally,
        // and a non-empty merged state for a second editor to start from.
        let (status, got) = request(&router, "GET", "/api/paper/c-1", None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["revision"], 1);
        assert_eq!(got["updates"].as_array().unwrap().len(), 1);
        assert_eq!(got["updates"][0]["origin"], creator);
        assert!(!got["state"].as_str().unwrap().is_empty());

        // Compaction: at the threshold the pending log folds into the
        // snapshot; revisions keep counting up. First update + 63 = 64
        // pending, and the 64th triggers the fold.
        for i in 0..63 {
            let b = format!(r#"{{"update":"{}"}}"#, yjs_update(&format!("line {i}")));
            let (status, _) = request(&router, "POST", "/api/paper/c-1/update", Some(&b)).await;
            assert_eq!(status, StatusCode::OK, "update {i}");
        }
        let (status, got) = request(&router, "GET", "/api/paper/c-1", None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["revision"], 64);
        assert_eq!(
            got["updates"].as_array().unwrap().len(),
            0,
            "the log folded into the snapshot"
        );
        assert!(!got["state"].as_str().unwrap().is_empty());

        // A fresh editor folds the same sidecar into the same document bytes
        // (convergence itself is paper_doc's test); here we only prove the
        // state survives a re-decode of the persisted sidecar.
        let (status, again) = request(&router, "GET", "/api/paper/c-1", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["state"], got["state"], "stable across re-decodes");
    }

    #[tokio::test]
    async fn paper_update_refuses_garbage_bytes() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        request(&router, "POST", "/api/paper/c-1/create", Some("{}"))
            .await
            .0;
        // Not base64 at all.
        let (status, _) = request(
            &router,
            "POST",
            "/api/paper/c-1/update",
            Some(r#"{"update":"!!!"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Base64, but not a Yjs update.
        let b64 = STANDARD.encode(b"garbage bytes");
        let body = format!(r#"{{"update":"{b64}"}}"#);
        let (status, _) = request(&router, "POST", "/api/paper/c-1/update", Some(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn paper_routes_are_401_while_locked() {
        let dir = TempDir::new().unwrap();
        let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
        for (method, uri, body) in [
            ("GET", "/api/paper/c-1", None),
            ("POST", "/api/paper/c-1/create", Some("{}")),
            // A well-formed body: the 401 must come from the lock, not from
            // JSON extraction failing first.
            ("POST", "/api/paper/c-1/update", Some(r#"{"update":"AAA="}"#)),
        ] {
            let (status, _) = request(&router, method, uri, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    /// The sidecar lives in the vault dir for the whole life of the machine,
    /// so its bytes must never be the document. Distinctive markers in the
    /// title and in the body must not appear in the file at rest.
    #[tokio::test]
    async fn paper_sidecar_is_ciphertext_at_rest() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);

        const TITLE_MARKER: &str = "atrest-title-marker";
        const BODY_MARKER: &str = "atrest-body-marker";

        let (status, created) = request(
            &router,
            "POST",
            "/api/paper/c-1/create",
            Some(&format!(r#"{{"title":"{TITLE_MARKER}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let body = format!(r#"{{"update":"{}"}}"#, yjs_update(BODY_MARKER));
        let (status, upd) = request(&router, "POST", "/api/paper/c-1/update", Some(&body)).await;
        assert_eq!(status, StatusCode::OK, "{upd}");

        // The sidecar is a vault file named exactly `papers.enc`, written
        // atomically; find it under the account dir whatever the layout.
        fn find_sidecar(root: &std::path::Path) -> Option<std::path::PathBuf> {
            let mut out = None;
            for entry in walkdir(root) {
                if entry.file_name()?.to_string_lossy() == paper_store::STATE_FILE {
                    out = Some(entry);
                }
            }
            out
        }
        fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
            let mut all = Vec::new();
            let mut stack = vec![root.to_path_buf()];
            while let Some(d) = stack.pop() {
                let entries = match std::fs::read_dir(&d) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        all.push(p);
                    }
                }
            }
            all
        }

        let sidecar = find_sidecar(dir.path()).expect("papers.enc exists after a create");
        let raw = std::fs::read(&sidecar).expect("sidecar readable");
        assert!(!raw.is_empty(), "sidecar is not empty");
        assert!(
            !windows_are_subslice(&raw, TITLE_MARKER.as_bytes()),
            "title appears in plaintext on disk"
        );
        assert!(
            !windows_are_subslice(&raw, BODY_MARKER.as_bytes()),
            "body text appears in plaintext on disk"
        );
        // Sanity: the plaintext IS in the API's view, so the markers were real.
        let (status, got) = request(&router, "GET", "/api/paper/c-1", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(got.to_string().contains(TITLE_MARKER));
    }

    /// A marker could straddle a buffer edge; sliding windows make the
    /// substring check honest for any needle shorter than the file.
    pub(crate) fn windows_are_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || haystack.len() < needle.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    // -----------------------------------------------------------------------
    // The receiving half of the fan-out: what `recv` hands the daemon, and what
    // `paper_apply_inbound` makes of it.
    // -----------------------------------------------------------------------

    /// A yrs update as bytes, which is how it travels the wire (the helper
    /// above returns the base64 the HTTP API takes).
    fn yjs_update_bytes(text: &str) -> Vec<u8> {
        STANDARD.decode(yjs_update(text)).unwrap()
    }

    fn inbound_descriptor(paper_id: &str, conclave_id: &str, creator: &str) -> PaperDescriptor {
        PaperDescriptor {
            paper_id: paper_id.into(),
            conclave_id: conclave_id.into(),
            creator: creator.into(),
            hub: creator.into(),
            title: "Minutes".into(),
            created_ms: 1_760_000_000_000,
        }
    }

    /// Two trustees can each start the paper before either hears from the
    /// other, and `create` cannot refuse a second paper it has never seen. The
    /// collision is settled by the earlier `created_ms`, and the loser takes the
    /// winner's identity while keeping its own document — one paper per
    /// conclave, one hub, so the fast path has a rendezvous to dial.
    ///
    /// Without this the vault ends up with two entries for one conclave, each
    /// naming its own hub, and neither side dials — while the deltas still
    /// converge, because the fan-out is by conclave, so nothing looks wrong.
    #[test]
    fn a_create_collision_settles_on_the_earlier_paper() {
        // Bob started his own paper second, and has an edit in it already.
        let mut bob = paper_store::PapersState::default();
        bob.create("c-1", "bb", "Minutes", 2_000).unwrap();
        bob.apply_update("c-1", "bb", &yjs_update_bytes("bob's line"), 2_001)
            .unwrap();
        let bob_before = folded(&bob, "c-1");
        let loser_id = bob.get("c-1").unwrap().descriptor.paper_id.clone();

        // Alice's descriptor arrives: she started first, so hers wins.
        let mut winner = inbound_descriptor("p-alice", "c-1", "aa");
        winner.created_ms = 1_000;
        assert!(
            paper_apply_inbound(
                &mut bob,
                "aa",
                &MessagePayload::PaperState { paper: winner.clone() },
                2_002,
            )
            .unwrap(),
            "adopting the winner's identity is a change"
        );

        assert_eq!(bob.papers.len(), 1, "one paper per conclave, not two");
        let held = bob.get("c-1").unwrap();
        assert_eq!(held.descriptor.paper_id, "p-alice", "the winner's id");
        assert_eq!(held.descriptor.creator, "aa", "and the winner's creator");
        assert_eq!(held.descriptor.hub, "aa", "and a hub this vault can dial");
        assert_eq!(
            folded(&bob, "c-1"),
            bob_before,
            "the loser keeps the document it already had"
        );

        // The same descriptor again is idempotent, and the old id resolves to
        // nothing now — which is why the daemon re-announces the document.
        assert!(!paper_apply_inbound(
            &mut bob,
            "aa",
            &MessagePayload::PaperState { paper: winner },
            2_003,
        )
        .unwrap());
        assert_eq!(bob.conclave_of(&loser_id), None);
    }

    /// The other half of the rule: the vault that started first ignores the
    /// later claim, so both sides converge on the same winner from the same two
    /// descriptors without another round trip.
    #[test]
    fn the_later_create_loses_on_the_side_that_started_first() {
        let mut alice = paper_store::PapersState::default();
        alice.create("c-1", "aa", "Minutes", 1_000).unwrap();
        let winner_id = alice.get("c-1").unwrap().descriptor.paper_id.clone();

        let mut later = inbound_descriptor("p-bob", "c-1", "bb");
        later.created_ms = 2_000;
        assert!(
            !paper_apply_inbound(
                &mut alice,
                "bb",
                &MessagePayload::PaperState { paper: later },
                2_001,
            )
            .unwrap(),
            "the later create changes nothing here"
        );

        assert_eq!(alice.papers.len(), 1);
        assert_eq!(alice.get("c-1").unwrap().descriptor.paper_id, winner_id);
        assert_eq!(alice.get("c-1").unwrap().descriptor.hub, "aa");
    }

    /// Equal timestamps still have to pick one winner, and both sides have to
    /// pick the same one, so the tie falls back to the id.
    #[test]
    fn a_create_collision_on_the_same_millisecond_still_picks_one_winner() {
        let mut held = paper_store::PapersState::default();
        held.create("c-1", "bb", "Minutes", 1_000).unwrap();
        let mut mine = inbound_descriptor("p-zzz", "c-1", "aa");
        mine.created_ms = 1_000;
        // The vault's own id is a uuid, so make the comparison explicit: the
        // smaller id wins, whichever side it is on.
        let my_id = held.get("c-1").unwrap().descriptor.paper_id.clone();
        let expect_move = "p-zzz" < my_id.as_str();

        assert_eq!(
            paper_apply_inbound(
                &mut held,
                "aa",
                &MessagePayload::PaperState { paper: mine },
                1_001,
            )
            .unwrap(),
            expect_move
        );
        assert_eq!(held.papers.len(), 1);
        assert_eq!(
            held.get("c-1").unwrap().descriptor.paper_id,
            if expect_move { "p-zzz" } else { my_id.as_str() }
        );
    }

    /// The document as it stands in a sidecar.
    fn folded(state: &paper_store::PapersState, conclave_id: &str) -> Vec<u8> {
        let file = state.get(conclave_id).expect("paper exists");
        let updates: Vec<Vec<u8>> = file.log.iter().map(|u| u.update.clone()).collect();
        paper_doc::fold(&file.snapshot, &updates).unwrap()
    }

    /// The whole point of the fan-out: the creator's descriptor materialises the
    /// paper on a peer, the deltas that follow merge into it, and both vaults
    /// hold the same document.
    #[test]
    fn inbound_paper_payloads_converge_on_the_creators_document() {
        let mut alice = paper_store::PapersState::default();
        let paper_id = alice.create("c-1", "aa", "Minutes", 1).unwrap();
        let descriptor = alice.get("c-1").unwrap().descriptor.clone();
        let u1 = yjs_update_bytes("Phase 2 ");
        let u2 = yjs_update_bytes("fan-out.");
        alice.apply_update("c-1", "aa", &u1, 2).unwrap();
        alice.apply_update("c-1", "aa", &u2, 3).unwrap();
        let alice_state = folded(&alice, "c-1");

        // Bob has never seen the paper at all.
        let mut bob = paper_store::PapersState::default();
        assert!(
            paper_apply_inbound(
                &mut bob,
                "aa",
                &MessagePayload::PaperState {
                    paper: descriptor.clone()
                },
                4,
            )
            .unwrap(),
            "the descriptor materialised the paper"
        );
        assert_eq!(
            bob.get("c-1").unwrap().descriptor.creator,
            "aa",
            "the adopted paper keeps its creator"
        );
        for (n, update) in [u1, u2].into_iter().enumerate() {
            assert!(
                paper_apply_inbound(
                    &mut bob,
                    "aa",
                    &MessagePayload::PaperDelta {
                        paper_id: paper_id.clone(),
                        update
                    },
                    5 + n as u64,
                )
                .unwrap()
            );
        }

        assert_eq!(
            folded(&bob, "c-1"),
            alice_state,
            "the peer converges on the creator's document"
        );
    }

    /// Deltas arrive from the peer that made them, so the log records the
    /// sender — the sender's own account id, never the local one.
    #[test]
    fn an_inbound_delta_is_recorded_against_its_sender() {
        let mut bob = paper_store::PapersState::default();
        bob.adopt(inbound_descriptor("p-1", "c-1", "aa"), 1).unwrap();
        paper_apply_inbound(
            &mut bob,
            "aa",
            &MessagePayload::PaperDelta {
                paper_id: "p-1".into(),
                update: yjs_update_bytes("from alice"),
            },
            2,
        )
        .unwrap();
        assert_eq!(bob.get("c-1").unwrap().log[0].origin, "aa");
    }

    /// A paper state naming someone else as its creator is refused: otherwise
    /// any joined trustee could invent a paper in another trustee's name.
    #[test]
    fn a_paper_state_from_anyone_but_its_creator_is_refused() {
        let mut bob = paper_store::PapersState::default();
        let err = paper_apply_inbound(
            &mut bob,
            "cc",
            &MessagePayload::PaperState {
                paper: inbound_descriptor("p-1", "c-1", "aa"),
            },
            1,
        );
        assert!(err.is_err(), "cc may not create a paper credited to aa");
        assert!(bob.papers.is_empty(), "nothing was materialised");
    }

    /// A delta for a paper this vault does not hold is refused rather than
    /// materialising a paper with no descriptor.
    #[test]
    fn a_delta_for_an_unknown_paper_is_refused() {
        let mut bob = paper_store::PapersState::default();
        assert!(paper_apply_inbound(
            &mut bob,
            "aa",
            &MessagePayload::PaperDelta {
                paper_id: "p-nobody".into(),
                update: vec![1, 2, 3],
            },
            1,
        )
        .is_err());
        assert!(bob.papers.is_empty());
    }

    /// A snapshot **merges**, so a trustee's own unshared edits survive the
    /// hub's catch-up. Replacing the snapshot instead would silently drop them.
    #[test]
    fn an_inbound_snapshot_merges_and_keeps_local_edits() {
        let mut bob = paper_store::PapersState::default();
        bob.adopt(inbound_descriptor("p-1", "c-1", "aa"), 1).unwrap();
        let bob_local = yjs_update_bytes("bob's line");
        bob.apply_update("c-1", "bb", &bob_local, 2).unwrap();

        // The hub's full state, built without bob's edit.
        let mut hub = paper_store::PapersState::default();
        hub.create("c-1", "aa", "", 1).unwrap();
        hub.apply_update("c-1", "aa", &yjs_update_bytes("alice's line"), 2)
            .unwrap();
        let hub_state = folded(&hub, "c-1");

        assert!(paper_apply_inbound(
            &mut bob,
            "aa",
            &MessagePayload::PaperSnapshot {
                paper: inbound_descriptor("p-1", "c-1", "aa"),
                state: hub_state.clone(),
            },
            3,
        )
        .unwrap());

        let merged = folded(&bob, "c-1");
        assert_ne!(
            merged, hub_state,
            "a snapshot that replaced would have dropped bob's line"
        );
        assert_eq!(
            merged,
            paper_doc::apply(&hub_state, &bob_local).unwrap(),
            "the hub's state merged with the local edit, both changes kept"
        );

        // Idempotent: a re-sent snapshot is a no-op, like any CRDT update.
        paper_apply_inbound(
            &mut bob,
            "aa",
            &MessagePayload::PaperSnapshot {
                paper: inbound_descriptor("p-1", "c-1", "aa"),
                state: hub_state,
            },
            4,
        )
        .unwrap();
        assert_eq!(
            folded(&bob, "c-1"),
            merged,
            "a re-sent snapshot changes nothing"
        );
    }

    /// The peer that needs a snapshot is the one that joined after the paper
    /// existed, so it never saw the `PaperState` fan-out and holds no record to
    /// apply against. The snapshot has to materialise the paper itself, or the
    /// hub's catch-up lands as "no paper in this vault" and is dropped.
    #[test]
    fn a_snapshot_from_the_creator_materialises_an_unknown_paper() {
        let mut carol = paper_store::PapersState::default();
        assert!(carol.papers.is_empty(), "carol starts with no paper at all");

        let mut hub = paper_store::PapersState::default();
        hub.create("c-1", "aa", "Minutes", 1).unwrap();
        hub.apply_update("c-1", "aa", &yjs_update_bytes("alice's line"), 2)
            .unwrap();
        let hub_state = folded(&hub, "c-1");

        assert!(paper_apply_inbound(
            &mut carol,
            "aa",
            &MessagePayload::PaperSnapshot {
                paper: inbound_descriptor("p-1", "c-1", "aa"),
                state: hub_state.clone(),
            },
            3,
        )
        .unwrap());

        assert_eq!(
            folded(&carol, "c-1"),
            hub_state,
            "carol has the document the hub caught her up with"
        );
    }

    /// The same authority rule `PaperState` enforces: a third party may not
    /// introduce a paper credited to someone else, whether by state or by
    /// snapshot.
    #[test]
    fn a_snapshot_credited_to_another_peer_is_refused() {
        let mut bob = paper_store::PapersState::default();
        let err = paper_apply_inbound(
            &mut bob,
            "cc",
            &MessagePayload::PaperSnapshot {
                paper: inbound_descriptor("p-1", "c-1", "aa"),
                state: yjs_update_bytes("forged"),
            },
            1,
        );
        assert!(err.is_err(), "cc may not introduce a paper credited to aa");
        assert!(bob.papers.is_empty(), "nothing was materialised");
    }

    /// A hub claim that moves the rendezvous is applied; presence is still
    /// reserved and changes nothing. Neither is an error.
    #[test]
    fn a_hub_claim_moves_the_rendezvous_and_presence_changes_nothing() {
        let mut bob = paper_store::PapersState::default();
        bob.adopt(inbound_descriptor("p-1", "c-1", "aa"), 1).unwrap();
        assert_eq!(bob.get("c-1").unwrap().descriptor.hub, "aa");

        // Convener-authoritative succession, already checked in `recv`: the
        // storage half just moves the hub.
        assert!(
            paper_apply_inbound(
                &mut bob,
                "cc",
                &MessagePayload::PaperHub {
                    paper: inbound_descriptor("p-1", "c-1", "cc"),
                },
                2,
            )
            .unwrap(),
            "the rendezvous moved"
        );
        assert_eq!(bob.get("c-1").unwrap().descriptor.hub, "cc");
        // The title is not the claim's to change.
        assert_eq!(bob.get("c-1").unwrap().descriptor.title, "Minutes");

        // The same claim twice is a no-op, and a claim about a paper this vault
        // does not hold changes nothing — a hub is a rendezvous, not a way to
        // conjure a paper.
        assert!(
            !paper_apply_inbound(
                &mut bob,
                "cc",
                &MessagePayload::PaperHub {
                    paper: inbound_descriptor("p-1", "c-1", "cc"),
                },
                3,
            )
            .unwrap()
        );
        assert!(
            !paper_apply_inbound(
                &mut bob,
                "cc",
                &MessagePayload::PaperHub {
                    paper: inbound_descriptor("p-absent", "c-absent", "cc"),
                },
                4,
            )
            .unwrap()
        );

        // Presence is v1 DataChannel-only, so a relay copy is reserved, not
        // unknown: accepted and ignored.
        assert!(
            !paper_apply_inbound(
                &mut bob,
                "aa",
                &MessagePayload::PaperPresence {
                    paper_id: "p-1".into(),
                    peer: "aa".into(),
                    state: vec![1],
                },
                5,
            )
            .unwrap()
        );
    }

    // -----------------------------------------------------------------------
    // The fast path as the daemon drives it (Task 6 Step 2): who is allowed to
    // answer an offer, and what the drain loop does with what arrives on a
    // channel. `paper_data`'s own tests cover the transport; these cover the
    // wiring that puts it on a production path.
    // -----------------------------------------------------------------------

    async fn settle(ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }

    /// The sidecar routes return `ApiError`, which has no `Debug` — it is never
    /// logged by value in production — so `unwrap` cannot be used on them.
    async fn sidecar(app: &Arc<App>) -> paper_store::PapersState {
        match paper_sidecar(app).await {
            Ok(parsed) => parsed,
            Err((code, _)) => panic!("the sidecar was unreadable: {code}"),
        }
    }

    async fn persist(app: &Arc<App>, parsed: &paper_store::PapersState) {
        if let Err((code, _)) = paper_persist(app, parsed).await {
            panic!("the sidecar could not be written: {code}");
        }
    }

    /// This vault's own account id.
    async fn me(app: &Arc<App>) -> String {
        match paper_account_id(app).await {
            Ok(id) => id,
            Err((code, _)) => panic!("no account id: {code}"),
        }
    }

    /// Join a dialer to a hub the way the star joins them: the dialer offers,
    /// the hub answers, the dialer takes the answer. Polls rather than sleeping
    /// a fixed amount, so the test is not timing-fragile.
    async fn connected_pair(
        hub: &Arc<paper_data::DataPlane>,
        hub_peer: &str,
        dialer_peer: &str,
    ) -> Arc<paper_data::DataPlane> {
        let dialer = Arc::new(paper_data::DataPlane::new(None));
        let offer = dialer.offer(hub_peer).await.unwrap();
        let answer = hub.answer(dialer_peer, &offer).await.unwrap();
        dialer.accept_answer(hub_peer, &answer).await.unwrap();
        for _ in 0..200 {
            if hub.peers().iter().any(|p| p == dialer_peer)
                && dialer.peers().iter().any(|p| p == hub_peer)
            {
                return dialer;
            }
            settle(5).await;
        }
        panic!("the pair never came up");
    }

    /// A daemon that is not the paper's hub must not hold a channel for it.
    ///
    /// This is the descriptor's authority applied to the transport rather than
    /// the text, and it is the same rule `PaperState` is checked against: only
    /// the daemon the paper names as its hub answers.
    #[tokio::test]
    async fn only_the_papers_hub_answers_an_offer() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;

        let mut parsed = paper_store::PapersState::default();
        let paper_id = parsed.create("c-1", "someone-else", "Minutes", 1).unwrap();
        persist(&app, &parsed).await;

        let offerer = paper_data::DataPlane::new(None);
        let sdp = offerer.offer("hub").await.unwrap();
        let offer = MessagePayload::PaperOffer {
            paper_id: paper_id.clone(),
            sdp,
        };
        assert!(
            paper_handle_signal(&app, "bb", &offer).await,
            "signalling is consumed here and never reaches the document path"
        );
        assert!(
            paper_open_plane(&app, "c-1").await.is_none(),
            "a daemon that is not the hub holds no channel for the paper"
        );
    }

    /// The hub takes the connection — and drops it when the answer cannot be
    /// addressed, which is how a non-trustee's offer leaves nothing behind.
    #[tokio::test]
    async fn the_hub_drops_a_channel_it_cannot_answer() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;

        // Created here, so this vault *is* the hub. The descriptor's hub is the
        // creator, and it has to be this vault's real account id for the
        // authority check to pass.
        let mut parsed = paper_store::PapersState::default();
        let paper_id = parsed.create("c-1", &me(&app).await, "Minutes", 1).unwrap();
        persist(&app, &parsed).await;

        let offerer = paper_data::DataPlane::new(None);
        let sdp = offerer.offer("hub").await.unwrap();
        let offer = MessagePayload::PaperOffer {
            paper_id: paper_id.clone(),
            sdp,
        };
        assert!(paper_handle_signal(&app, "bb", &offer).await);

        let plane = paper_open_plane(&app, "c-1")
            .await
            .expect("the hub takes the connection");
        // "bb" is no trustee of any conclave in this vault, so the addressed
        // answer is refused. Without the `forget`, the channel would still come
        // up a moment later and the peer would be on the star for good.
        settle(300).await;
        assert!(
            plane.peers().is_empty(),
            "a peer the answer could not be addressed to never becomes live"
        );
    }

    /// The re-broadcast, which is the whole reason the drain loop exists.
    ///
    /// The relay fan-out is a full mesh, but the channels are a star: an update
    /// that reached the hub over one channel would reach the other trustees at
    /// relay speed without this, which is exactly the thing the fast path is
    /// for. The relay is not part of this test, so the second trustee can only
    /// learn about the edit by the hub forwarding it.
    #[tokio::test]
    async fn the_drain_loop_applies_a_channel_update_and_re_broadcasts_it() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        let router = api_router(app.clone());

        let (status, created) = request(
            &router,
            "POST",
            "/api/paper/c-1/create",
            Some(r#"{"title":"Minutes"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["revision"], 0);

        // The hub's plane, with two trustees on it: the star this test is
        // about. `paper_handle_signal` would have made the same plane.
        let hub = paper_plane(&app, "c-1").await;
        let t1 = connected_pair(&hub, "hub", "t1").await;
        let t2 = connected_pair(&hub, "hub", "t2").await;
        assert_eq!(hub.peers().len(), 2, "both trustees are on the star");

        // A trustee edits. It reaches the hub over one channel and nobody else.
        let edit = yjs_update_bytes("star.");
        assert_eq!(t1.send("hub", &edit).await, paper_data::Sent::Live);

        // The loop is what closes the gap, so poll it the way the daemon does.
        let mut applied = false;
        for _ in 0..200 {
            paper_drain_once(&app).await;
            let parsed = sidecar(&app).await;
            if parsed.get("c-1").map(|f| f.next_rev).unwrap_or(0) > 1 {
                applied = true;
                break;
            }
            settle(5).await;
        }
        assert!(applied, "the hub applied the update it received on a channel");

        let mut forwarded = false;
        for _ in 0..200 {
            if t2.drain().iter().any(|i| {
                matches!(&i.frame, paper_data::InboundFrame::Update(bytes) if bytes == &edit)
            }) {
                forwarded = true;
                break;
            }
            settle(5).await;
        }
        assert!(
            forwarded,
            "the hub re-broadcast the update to the other trustee"
        );

        // And the hub's own copy is the merged document, not the raw bytes.
        let parsed = sidecar(&app).await;
        assert_eq!(parsed.get("c-1").unwrap().log.len(), 1);
        assert!(!folded(&parsed, "c-1").is_empty());
    }

    /// An ended conclave's paper is read-only, locally as well as on the wire.
    ///
    /// The core already refuses the fan-out, so accepting the edit here would
    /// only diverge this vault's copy from every other peer's.
    #[tokio::test]
    async fn an_ended_conclave_paper_refuses_writes() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        let router = api_router(app.clone());

        // A conclave needs at least one other trustee, so the vault needs a
        // contact first — the roster is what "ended" is read from.
        let peer_dir = TempDir::new().unwrap();
        let mut peer = hush_core::Account::create(peer_dir.path(), "pw", "http://127.0.0.1:9")
            .await
            .unwrap();
        let invite = peer.invite().await.unwrap();
        assert!(matches!(
            app.worker.add_contact("peer", &invite).await,
            WorkResult::Ok
        ));

        // Creating a conclave also sends the invite, and this vault's relay is a
        // dead address on purpose — no test here reaches the network. The send
        // fails *after* the record is made, which is all this test needs:
        // "ended" is read from the roster, not from an invite.
        let _ = app
            .worker
            .create_conclave("Trip", vec!["peer".into()])
            .await;
        let conclave_id = match app.worker.conclaves().await {
            WorkResult::Conclaves(list) if !list.is_empty() => list[0].id.clone(),
            _ => panic!("the conclave record was not created"),
        };
        let (status, created) = request(
            &router,
            "POST",
            &format!("/api/paper/{conclave_id}/create"),
            Some(r#"{"title":"Minutes"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");

        // Editable while the conclave is live...
        let body = format!(r#"{{"update":"{}"}}"#, yjs_update("live"));
        let (status, _) = request(
            &router,
            "POST",
            &format!("/api/paper/{conclave_id}/update"),
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // ...and not once it has ended.
        assert!(matches!(
            app.worker.end_conclave(&conclave_id).await,
            WorkResult::Ok
        ));
        let (status, refused) = request(
            &router,
            "POST",
            &format!("/api/paper/{conclave_id}/update"),
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert!(
            refused["error"].as_str().unwrap().contains("ended"),
            "the refusal says why: {refused}"
        );

        // The document is still readable — read-only, not gone.
        let (status, got) = request(&router, "GET", &format!("/api/paper/{conclave_id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["revision"], 1);

        // A second paper cannot be started in an ended conclave either.
        let (status, _) = request(
            &router,
            "POST",
            &format!("/api/paper/{conclave_id}/create"),
            Some(r#"{"title":"Another"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// The re-home route is mounted and refuses what it should.
    ///
    /// The authority itself is the core's, and is tested there
    /// (`conclaves.rs`); what is pinned here is the route's shape — the 404 and
    /// 400 boundaries, that a refusal leaves the hub where it was, and that the
    /// badge field is always present rather than omitted when there is no plane.
    #[tokio::test]
    async fn the_rehome_route_refuses_before_it_moves_anything() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        let router = api_router(app.clone());

        let (status, _) = request(
            &router,
            "POST",
            "/api/paper/c-1/hub/rehome",
            Some(r#"{"hub":"zz"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no paper here yet");

        let (status, created) = request(
            &router,
            "POST",
            "/api/paper/c-1/create",
            Some(r#"{"title":"Minutes"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["fast_path"]["path"], "relay");
        assert_eq!(created["fast_path"]["live_peers"], 0);

        let (status, _) = request(
            &router,
            "POST",
            "/api/paper/c-1/hub/rehome",
            Some(r#"{"hub":"   "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "nobody to move to");

        // This vault is in no conclave, so the core refuses the send.
        let (status, _) = request(
            &router,
            "POST",
            "/api/paper/c-1/hub/rehome",
            Some(r#"{"hub":"zz"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (_, got) = request(&router, "GET", "/api/paper/c-1", None).await;
        assert_eq!(
            got["paper"]["hub"], created["paper"]["hub"],
            "a refused re-home leaves the hub where it was"
        );
    }

    /// Locking the vault takes the fast path with it.
    ///
    /// A plane is a live socket whose state came out of the vault, so a locked
    /// vault must not keep one. Both sides fall back to the relay with no user
    /// action, which is why nothing here has to be recovered on unlock.
    #[tokio::test]
    async fn locking_the_vault_closes_the_paper_planes() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        let router = api_router(app.clone());

        let hub = paper_plane(&app, "c-1").await;
        let _t1 = connected_pair(&hub, "hub", "t1").await;
        assert_eq!(hub.peers().len(), 1, "the channel is up before the lock");

        let (status, _) = request(&router, "POST", "/api/lock", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(
            app.paper_planes.lock().await.is_empty(),
            "the registry is emptied so the next open dials afresh"
        );
        assert!(hub.peers().is_empty(), "the channel is closed");
    }
}

/// Zines (Task 21, Phase 1): the local corpus over the routes.
#[cfg(test)]
mod zines_api_tests {
    use super::*;
    use crate::notes_api_tests::{build_app, request, unlocked_app};
    use crate::paper_api_tests::windows_are_subslice;
    use tempfile::TempDir;
// -----------------------------------------------------------------------
// Zines (Task 21, Phase 1)
// -----------------------------------------------------------------------

#[tokio::test]
async fn zines_route_is_empty_until_a_pen_name_exists() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);

    let (status, view) = request(&router, "GET", "/api/zines", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["zines"].as_array().unwrap().len(), 0);
    assert_eq!(view["sections"].as_array().unwrap().len(), 0);
    assert!(view["pen_name"].is_null(), "no byline yet");

    // Writing before choosing a byline is refused, not silently published
    // under one nobody chose.
    let (status, err) = request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"title":"First","body":"<p>hello</p>"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err["error"].as_str().unwrap().contains("pen name"), "{err}");
}

#[tokio::test]
async fn a_pen_name_is_minted_then_renamed_without_moving_the_address() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);

    let (status, view) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"  Ada  "}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let id = view["pen_name"]["id"].as_str().unwrap().to_string();
    assert_eq!(view["pen_name"]["name"], "Ada", "the byline is trimmed");
    assert_eq!(id.len(), 64);

    // A blank byline is refused rather than minting an unnamed identity.
    let (status, _) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"   "}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, renamed) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"Ada Lovelace"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["pen_name"]["name"], "Ada Lovelace");
    assert_eq!(
        renamed["pen_name"]["id"], id,
        "a byline is not an identity: the address did not move"
    );

    let (status, got) = request(&router, "GET", "/api/zines/pen-name", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["name"], "Ada Lovelace");
}

#[tokio::test]
async fn zines_round_trip_a_create_an_edit_a_mute_and_a_forget() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    let (_, pen) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"ada"}"#),
    )
    .await;
    let id = pen["pen_name"]["id"].as_str().unwrap().to_string();

    // Create.
    let (status, created) = request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"title":"On relays","sections":["Rust","rust","Networking"],"body":"<p>hello <strong>world</strong></p>"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let row = &created["zines"][0];
    assert_eq!(row["title"], "On relays");
    assert_eq!(row["revision"], 1);
    assert_eq!(row["zine_id"], id, "the id is the pen name");
    assert_eq!(row["authored"], true);
    assert_eq!(row["preview"], "hello world");
    assert_eq!(
        row["sections"].as_array().unwrap().len(),
        2,
        "`Rust` and `rust` are one section"
    );
        assert_eq!(created["sections"][0]["count"], 1);
        let names: Vec<&str> = created["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"rust") && names.contains(&"networking"), "{names:?}");

    // Edit: a new revision, chained to the one before it.
    let (status, edited) = request(
        &router,
        "POST",
        "/api/zines",
        Some(&format!(
            r#"{{"zine_id":"{id}","title":"On relays, again","sections":["rust"],"body":"<p>second</p>"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["zines"][0]["revision"], 2);
    assert_eq!(edited["zines"][0]["title"], "On relays, again");

    // The chain and the history are visible on the article.
    let (status, one) = request(&router, "GET", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["zine"]["descriptor"]["revision"], 2);
    assert_eq!(
        one["zine"]["descriptor"]["prev_hash"].as_str().unwrap().len(),
        64,
        "revision 2 points back at revision 1's content hash"
    );
    assert_eq!(one["zine"]["history"][0]["revision"], 1);
    assert_eq!(one["zine"]["history"][0]["title"], "On relays");
    assert_eq!(one["zine"]["authored"], true);

    // Filters.
    let (_, by_section) = request(&router, "GET", "/api/zines?section=rust", None).await;
    assert_eq!(by_section["zines"].as_array().unwrap().len(), 1);
    let (_, by_missing) = request(&router, "GET", "/api/zines?section=nope", None).await;
    assert_eq!(
        by_missing["zines"].as_array().unwrap().len(),
        0,
        "an unknown section is an empty list, not an error"
    );
    let (_, by_pen) = request(&router, "GET", "/api/zines?pen=AD", None).await;
    assert_eq!(by_pen["zines"].as_array().unwrap().len(), 1);

    // Mute, then unmute.
    let (status, muted) = request(&router, "POST", &format!("/api/zines/{id}/mute"), Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{muted}");
    assert_eq!(muted["zines"][0]["muted"], true);
    let (_, unmuted) = request(
        &router,
        "POST",
        &format!("/api/zines/{id}/mute"),
        Some(r#"{"muted":false}"#),
    )
    .await;
    assert_eq!(unmuted["zines"][0]["muted"], false);

    // Forget.
    let (status, gone) = request(&router, "DELETE", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{gone}");
    assert_eq!(gone["zines"].as_array().unwrap().len(), 0);
    let (status, _) = request(&router, "GET", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_draft_naming_someone_elses_zine_writes_under_our_own_byline() {
    // The pen name comes from the vault, never from the request: a caller
    // does not choose which byline it publishes under, so a stale id must
    // not create a second zine under an identity we do not hold.
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    let (_, pen) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"ada"}"#),
    )
    .await;
    let mine = pen["pen_name"]["id"].as_str().unwrap().to_string();
    let theirs = "b".repeat(64);

    let (status, view) = request(
        &router,
        "POST",
        "/api/zines",
        Some(&format!(
            r#"{{"zine_id":"{theirs}","title":"Not mine","body":"<p>x</p>"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["zines"].as_array().unwrap().len(), 1);
    assert_eq!(view["zines"][0]["zine_id"], mine, "it landed under our byline");
    let (status, _) = request(&router, "GET", &format!("/api/zines/{theirs}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_zine_that_is_not_held_is_404_with_a_reason() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    let ghost = "c".repeat(64);
    for (method, uri, body) in [
        ("GET", format!("/api/zines/{ghost}"), None),
        ("DELETE", format!("/api/zines/{ghost}"), None),
        ("POST", format!("/api/zines/{ghost}/mute"), Some("{}")),
    ] {
        let (status, err) = request(&router, method, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert!(err["error"].as_str().unwrap().contains("no zine"), "{err}");
    }
}

#[tokio::test]
async fn every_zine_route_is_401_while_locked() {
    let dir = TempDir::new().unwrap();
    let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
    let id = "a".repeat(64);
    for (method, uri, body) in [
        ("GET", "/api/zines".to_string(), None),
        ("GET", "/api/zines/sections".to_string(), None),
        ("GET", "/api/zines/pen-name".to_string(), None),
        (
            "POST",
            "/api/zines/pen-name".to_string(),
            Some(r#"{"name":"ada"}"#),
        ),
        // Well-formed bodies: the 401 must come from the lock, not from
        // JSON extraction failing first.
        (
            "POST",
            "/api/zines".to_string(),
            Some(r#"{"title":"t","body":"<p>b</p>"}"#),
        ),
        ("GET", format!("/api/zines/{id}"), None),
        ("DELETE", format!("/api/zines/{id}"), None),
        ("POST", format!("/api/zines/{id}/mute"), Some("{}")),
    ] {
        let (status, _) = request(&router, method, &uri, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

/// A draft zine is as private as a draft message: the sidecar lives in the
/// vault dir for the life of the machine, so its bytes must never be the
/// article. The byline is a marker too — a pen name is an identity, and a
/// cleartext byline next to a cleartext title would be the whole graph.
#[tokio::test]
async fn zines_sidecar_is_ciphertext_at_rest() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);

    const BYLINE: &str = "atrest-byline-marker";
    const TITLE: &str = "atrest-title-marker";
    const BODY: &str = "atrest-body-marker";
    const SECTION: &str = "atrest-section-marker";

    let (status, pen) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(&format!(r#"{{"name":"{BYLINE}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pen}");
    let (status, created) = request(
        &router,
        "POST",
        "/api/zines",
        Some(&format!(
            r#"{{"title":"{TITLE}","sections":["{SECTION}"],"body":"<p>{BODY}</p>"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");

    let sidecar = std::fs::read(dir.path().join(zine_store::STATE_FILE))
        .expect("zines.enc exists after a write");
    assert!(!sidecar.is_empty(), "sidecar is not empty");
    for marker in [BYLINE, TITLE, BODY, SECTION] {
        assert!(
            !windows_are_subslice(&sidecar, marker.as_bytes()),
            "{marker} appears in plaintext on disk"
        );
    }
    // The pen name's own file is a key, so it is ciphertext too.
    let pen_file = std::fs::read(dir.path().join("pen-names.enc")).expect("pen-names.enc");
    assert!(!windows_are_subslice(&pen_file, BYLINE.as_bytes()));

    // Sanity: the markers were real, so the check above was not vacuous.
    let (status, got) = request(&router, "GET", "/api/zines", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(got.to_string().contains(TITLE));
    assert!(got.to_string().contains(BODY));
}

#[tokio::test]
async fn a_hostile_body_is_sanitized_on_the_way_in() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"ada"}"#),
    )
    .await;
    let (status, view) = request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"title":"t","body":"<p>ok</p><script>alert(1)</script><a href=\"javascript:alert(1)\">x</a>"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let id = view["zines"][0]["zine_id"].as_str().unwrap();
    let (_, one) = request(&router, "GET", &format!("/api/zines/{id}"), None).await;
    let body = one["zine"]["descriptor"]["body"].as_str().unwrap();
    assert!(body.contains("<p>ok</p>"), "{body}");
    assert!(!body.contains("script"), "{body}");
    assert!(!body.contains("javascript:"), "{body}");
}

#[tokio::test]
async fn a_zine_survives_a_restart() {
    // The acceptance criterion for Phase 1: author a zine, restart, and it
    // is still there — which means the sidecar has to be read back through
    // a fresh worker, not just held in memory.
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    let (_, pen) = request(
        &router,
        "POST",
        "/api/zines/pen-name",
        Some(r#"{"name":"ada"}"#),
    )
    .await;
    let id = pen["pen_name"]["id"].as_str().unwrap().to_string();
    request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"title":"Persisted","sections":["rust"],"body":"<p>still here</p>"}"#),
    )
    .await;

    // A brand-new worker over the same vault dir: the daemon restarting.
    let worker = AccountWorker::spawn();
    assert!(matches!(
        worker.unlock(&dir.path().to_path_buf(), "pw").await,
        WorkResult::Ok
    ));
    let restarted = api_router(build_app(dir.path(), worker, true));
    let (status, view) = request(&restarted, "GET", "/api/zines", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["zines"][0]["title"], "Persisted");
    assert_eq!(view["zines"][0]["zine_id"], id);
    assert_eq!(view["pen_name"]["name"], "ada");
    assert_eq!(view["sections"][0]["name"], "rust");
    let (status, one) = request(&restarted, "GET", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(one["zine"]["descriptor"]["body"]
        .as_str()
        .unwrap()
        .contains("still here"));
}
}

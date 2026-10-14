mod account_worker;
mod notes_store;
mod paper_data;
mod voice_data;
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

use account_worker::{
  AccountWorker, ContactDto, IntroLists, PenNameDto, PublicIdentityDto, WorkResult,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, Method, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::Parser;
use hush_core::{
    ChatRecord, ConclaveRecord, Error, MessagePayload, PaperDescriptor, PendingInvite,
    ReceivedMessage, VoiceEffect, VoiceManager, VoiceSession,
};
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
    /// Conclave voice's live rooms (Task 23), keyed by conclave id through the
    /// manager.
    ///
    /// In-memory on purpose — a voice session is live-only and stores nothing
    /// — so a restart or a lock *is* the leave, on every route that reads
    /// this. Inbound payloads are applied from the recv poll; the routes apply
    /// the local user's actions before fanning them out.
    voices: Mutex<VoiceManager>,
    /// Conclave voice's media planes (Task 23 Phase 2), keyed by session id,
    /// created on demand when a room opens or an offer arrives. Same contract
    /// as `paper_planes`: runtime-only, a peer-connection map, safe to lose.
    voice_planes: Mutex<HashMap<String, Arc<voice_data::VoicePlane>>>,
    /// The neighborhoods read lane's transport (Task 22). `None` until a Tor
    /// relay client is wired in; a refresh with no source is a clear refusal, not
    /// a silent no-op. Runtime-only and safe to lose.
    neighborhoods_source: Mutex<Option<Arc<dyn hush_articles::RelaySource>>>,
    /// The neighborhoods write half: publishing an export or a comment. Separate
    /// from the source because reading the network and speaking on the member's
    /// behalf are not the same capability, and the daemon wires them
    /// independently. `None` until a Tor relay client is wired in.
    neighborhoods_sink: Mutex<Option<Arc<dyn hush_articles::RelaySink>>>,
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
        // Conclave voice (Task 23). State is in-memory: a room lives and dies
        // with the sessions and the lock, and nothing here writes to disk.
        .route("/api/voice/{conclave_id}/open", post(voice_open))
        .route("/api/voice/{conclave_id}/join", post(voice_join))
        .route("/api/voice/{conclave_id}/leave", post(voice_leave))
        .route("/api/voice/{conclave_id}/mute", post(voice_mute))
        .route("/api/voice/{conclave_id}/stats", get(voice_stats))
        .route("/api/voice/{conclave_id}", get(voice_get))
        .route("/api/voice/state", get(voice_state))
        // Zines (Task 21). The literal `pen-name` segment is matched ahead of
        // the `{zine_id}` parameter, and cannot collide with a real zine: an id
        // is a minted uuid.
        .route("/api/zines", get(zines_index).post(zines_write))
        .route("/api/zines/pen-name", get(zines_pen_name).post(zines_set_pen_name))
        .route("/api/zines/sections", get(zines_sections))
        .route(
            "/api/zines/{zine_id}",
            get(zines_get).delete(zines_forget),
        )
        .route("/api/zines/{zine_id}/mute", post(zines_mute))
        // The key epidemic, sender side: hand this post's read key to a contact.
        .route("/api/zines/{zine_id}/grant", post(zines_grant))
        // Comments go the other way: sealed to the author's pen-name key.
        .route("/api/zines/{zine_id}/comment", post(zines_comment))
        .route(
            "/api/zines/{zine_id}/accept-comment",
            post(zines_accept_comment),
        )
        .route(
            "/api/zines/{zine_id}/discard-comment",
            post(zines_discard_comment),
        )
        // Task 11: publish the zine to the neighborhoods as a NIP-23 article.
        // The same post can be exported again after an edit: NIP-23 is editable
        // by address, so a re-export replaces the article rather than adding one.
        .route("/api/zines/{zine_id}/export", post(zines_export))
        // Neighborhoods (Task 22): the public Nostr read lane. `identity` is
        // minted on an explicit create, never on a read, so a stray GET cannot
        // publish under a key nobody chose.
        .route(
            "/api/neighborhoods/identity",
            get(neighborhoods_identity).post(neighborhoods_create_identity),
        )
        // The read lane: the cached index, one article, a forced pull, and the
        // lane's settings. The literal segments are matched ahead of `{addr}`.
        .route("/api/neighborhoods", get(neighborhoods_index))
        .route(
            "/api/neighborhoods/settings",
            get(neighborhoods_settings).post(neighborhoods_set_settings),
        )
        .route("/api/neighborhoods/refresh", post(neighborhoods_refresh))
        .route("/api/neighborhoods/{addr}", get(neighborhoods_get))
        // Task 22, comments: one way out, signed by the public identity, and
        // read-only replies back in. Separate from the zines comment thread in
        // every way that matters — different store, different key, different
        // page — so a public comment can never be mistaken for a sealed one.
        .route(
            "/api/neighborhoods/{addr}/comment",
            post(neighborhoods_comment),
        )
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
        .route("/api/conclaves/intros", get(intro_lists))
        .route("/api/conclaves/intros/introduce", post(intro_introduce))
        .route("/api/conclaves/intros/request", post(intro_request))
        .route("/api/conclaves/intros/relay", post(intro_relay))
        .route("/api/conclaves/intros/answer", post(intro_answer))
        .route("/api/conclaves/intros/accept", post(intro_accept))
        .route("/api/conclaves/intros/dismiss", post(intro_dismiss))
        .route(
            "/api/conclaves/intros/requests/dismiss",
            post(intro_request_dismiss),
        )
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
        .route("/api/chats/delete", post(delete_chats))
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
    // Voice rooms end with the lock outright (Task 23): a session is
    // live-only, so there is nothing to fall back to and nothing to save —
    // this *is* the leave, and peers see it as one. The media planes go the
    // same way: every peer connection closes, every capture stream stops.
    voice_planes_shutdown(state).await;
    state.voices.lock().await.clear();
}

/// Installs the process-level TLS crypto provider.
///
/// rustls 0.23 refuses to choose when more than one provider is enabled by
/// feature unification, and this binary enables two: alloy and electrum pull
/// `aws-lc-rs`, while `webrtc` and the reqwest paths pull `ring`. Left unset,
/// the first TLS handshake panics inside a runtime worker — the message is
/// "Could not automatically determine the process-level CryptoProvider from
/// Rustls crate features" — and the request that triggered it never answers,
/// which leaves the socket listening and every later call timing out. That is
/// how one neighborhoods refresh over `wss://` took the whole daemon down.
///
/// Installing one is rustls's documented remedy, and `ring` is the same choice
/// the desktop shell makes for its own client (`hush-tauri/src-tauri/src/irc/tls.rs`).
/// The result is discarded on purpose: the only thing `install_default` returns
/// on failure is the provider that was already installed, which means the job
/// is done either way.
///
/// Called once at the top of `main`, before anything can dial out.
fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// libsignal protocol futures are not `Send`; account ops run on a dedicated worker thread.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_tls_provider();

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
        voices: Mutex::new(VoiceManager::new()),
        voice_planes: Mutex::new(HashMap::new()),
        neighborhoods_source: Mutex::new(None),
        neighborhoods_sink: Mutex::new(None),
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

/// Both members of an introduction this vault can see: the seeker it wants to
/// meet and the target whose invite would be shared.
#[derive(Deserialize)]
struct IntroPairRequest {
    conclave_id: String,
    seeker: String,
    target: String,
}

/// A request whose seeker is this vault.
#[derive(Deserialize)]
struct IntroTargetRequest {
    conclave_id: String,
    target: String,
}

#[derive(Deserialize)]
struct IntroAnswerRequest {
    conclave_id: String,
    seeker: String,
    accept: bool,
}

/// A request identified by the seeker that sent it.
#[derive(Deserialize)]
struct IntroSeekerRequest {
    conclave_id: String,
    seeker: String,
}

#[derive(Deserialize)]
struct IntroAcceptRequest {
    conclave_id: String,
    target: String,
    nickname: String,
}

/// The introduction queues: requests to answer or relay, invites to accept.
async fn intro_lists(
    State(state): State<Arc<App>>,
) -> Result<Json<IntroLists>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.pending_intros().await)?;
    match result {
        WorkResult::PendingIntros(lists) => Ok(Json(lists)),
        _ => Err(unexpected_worker_response()),
    }
}

/// The "Introduce to…" action: ask a member this vault knows to share their
/// invite with another member it also knows.
async fn intro_introduce(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroPairRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .introduce_members(&body.conclave_id, &body.seeker, &body.target)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// The "Request introduction" action: ask the conclave's reachable members to
/// carry the request to someone this vault cannot reach.
async fn intro_request(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroTargetRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.request_intro(&body.conclave_id, &body.target).await)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The courier's relay action on a request it is holding.
async fn intro_relay(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroPairRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .relay_intro_request(&body.conclave_id, &body.seeker, &body.target)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// The target's consent: share the standing invite, or decline silently.
async fn intro_answer(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroAnswerRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .answer_intro_request(&body.conclave_id, &body.seeker, body.accept)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// Consume a delivered introduction: the contact is added vault-side, so the
/// page only learns the fresh contact list afterwards.
async fn intro_accept(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroAcceptRequest>,
) -> Result<Json<Vec<ContactDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .accept_intro(&body.conclave_id, &body.target, &body.nickname)
            .await,
    )?;
    let result = map_err(state.worker.contacts().await)?;
    match result {
        WorkResult::Contacts(contacts) => Ok(Json(contacts)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Dismiss a delivered introduction.
async fn intro_dismiss(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroTargetRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.dismiss_intro(&body.conclave_id, &body.target).await)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Drop a held introduction request without relaying or answering it.
async fn intro_request_dismiss(
    State(state): State<Arc<App>>,
    Json(body): Json<IntroSeekerRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .dismiss_intro_request(&body.conclave_id, &body.seeker)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
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
    // Voice payloads (Task 23) are applied to the in-memory manager, not the
    // inbox — a room's state is session truth, never chat history. The same
    // pass sweeps rooms whose conclave has ended or that this vault can no
    // longer speak for: a `ConclaveEnd` carries no voice payload of its own,
    // so the room would otherwise outlive the conclave until a restart.
    if !inbox.voices.is_empty() || !inbox.joined.is_empty() || !inbox.departed.is_empty() {
        let conclaves: HashMap<String, ConclaveRecord> = voice_conclaves(&state)
            .await
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        let lookup = |id: &str| conclaves.get(id).cloned();
        let now = paper_now_ms();
        let mut answers: Vec<(String, MessagePayload)> = Vec::new();
        // Rooms doomed by the sweep, with their session ids, so the planes
        // drop after the manager lock releases.
        let mut to_drop: Vec<String> = Vec::new();
        {
            let mut mgr = state.voices.lock().await;
            for received in &inbox.voices {
                if let Some(answer) = voice_apply_inbound(
                    &state,
                    &mut mgr,
                    &lookup,
                    &received.from_account,
                    &received.payload,
                    now,
                )
                .await
                {
                    answers.push(answer);
                }
            }
            // The sweep. The account id may be unavailable mid-lock; skipping
            // the sweep then is safe — the next poll sweeps again.
            if let Ok(me) = paper_account_id(&state).await {
                let live: Vec<String> = mgr
                    .rooms()
                    .iter()
                    .map(|s| s.session.conclave_id.clone())
                    .collect();
                for conclave_id in live {
                    let keep = match conclaves.get(&conclave_id) {
                        Some(record) => {
                            !record.ended
                                && record.joined().any(|t| t.account_id == me)
                        }
                        None => false,
                    };
                    if !keep {
                        if let Some(session) = mgr.session(&conclave_id) {
                            to_drop.push(session.session.session_id.clone());
                        }
                        mgr.end_conclave(&conclave_id);
                    }
                }
            }
        }
        for session_id in to_drop {
            voice_plane_drop(&state, &session_id).await;
        }
        for (conclave_id, answer) in answers {
            let (session_id, sdp) = match &answer {
                MessagePayload::VoiceAnswer {
                    session_id, sdp, ..
                } => (session_id.clone(), sdp.clone()),
                _ => continue,
            };
            if let WorkResult::Err(e) = state
                .worker
                .send_voice_answer(&conclave_id, &session_id, &sdp)
                .await
            {
                eprintln!("hush-daemon: voice answer fan-out failed: {e}");
            }
        }
    }
    // The zines bulletin lane is drained on the same poll as the inbox: it is
    // the same relay, the same unlocked account and the same sidecar, and a
    // second timer would only be a second thing racing to write `zines.enc`. A
    // failure is logged and swallowed — the poll's job is the mailbox, and a
    // zine that could not be synced is still held and still on the lane for the
    // next pass.
    if let Err((_, Json(err))) = zines_sync(&state, &inbox.zine_grants).await {
        eprintln!("hush-daemon: zines sync failed: {}", err.error);
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
        | MessagePayload::ZineGrant { .. }
        | MessagePayload::ConclaveInvite { .. }
        | MessagePayload::ConclaveAccept { .. }
        | MessagePayload::ConclaveDecline { .. }
        | MessagePayload::ConclaveLeave { .. }
        | MessagePayload::ConclaveUpdate { .. }
        | MessagePayload::ConclaveEnd { .. }
        | MessagePayload::ConclaveIntroReq { .. }
        | MessagePayload::ConclaveIntro { .. } => None,
        // A paper payload is not a chat message either: it is routed into the
        // paper store, never the inbox. A voice payload (Task 23) likewise
        // never renders in the inbox — its state surfaces through /api/voice/*.
        // An unknown tag has no rendering at all.
        MessagePayload::PaperState { .. }
        | MessagePayload::PaperDelta { .. }
        | MessagePayload::PaperSnapshot { .. }
        | MessagePayload::PaperPresence { .. }
        | MessagePayload::PaperHub { .. }
        | MessagePayload::PaperOffer { .. }
        | MessagePayload::PaperAnswer { .. }
        | MessagePayload::VoiceOpen { .. }
        | MessagePayload::VoiceOffer { .. }
        | MessagePayload::VoiceAnswer { .. }
        | MessagePayload::VoiceMute { .. }
        | MessagePayload::VoiceLeave { .. }
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

#[derive(Deserialize)]
struct DeleteChatsRequest {
    contact: String,
    /// Inclusive lower bound, ms since the epoch; absent = since forever.
    #[serde(default)]
    from_ms: Option<u64>,
    /// Inclusive upper bound; absent = up to now.
    #[serde(default)]
    to_ms: Option<u64>,
}

/// Delete 1:1 chat messages with a contact — the Chat history dialog's
/// "delete entire history" (no bounds) and "delete date range" (with one).
/// Mail, drafts and drops share the underlying log and are never touched.
async fn delete_chats(
    State(state): State<Arc<App>>,
    Json(body): Json<DeleteChatsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    if body.contact.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "contact is required".into(),
            }),
        ));
    }
    let result = map_err(
        state
            .worker
            .delete_chats(body.contact, body.from_ms, body.to_ms)
            .await,
    )?;
    match result {
        WorkResult::ChatsDeleted(n) => Ok(Json(serde_json::json!({ "deleted": n }))),
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
    if body.chat_auto_delete.keep_lines.is_some_and(|n| n > 100_000) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "chat_auto_delete.keep_lines must be at most 100000".into(),
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
    // The chat log enforces the auto-delete policy live, so the worker has to
    // hold the one that was just saved.
    state
        .worker
        .set_chat_retention(
            body.chat_auto_delete.daily,
            body.chat_auto_delete.keep_lines,
        )
        .await;
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
// Local, plus the wire: authoring, storing, reading and publishing a zine, and
// the bulletin lane that carries revisions, read keys and comments. Every route
// goes through the worker, so a locked vault answers 401 without any of these
// types being touched.
//
// The split is the same one the papers use: the worker owns the wire and the
// keys, this layer owns the sidecar and the policy. So the seal key crosses the
// channel — "is this blob mine, and for which post?" is a question about the
// sidecar — and the store itself never does crypto.

/// The whole zines pane in one response: the index, the section facet, the pen
/// name, and the state of the blob pool. Four round trips to paint one screen
/// would be four chances to render a pane that disagrees with itself.
#[derive(serde::Serialize)]
struct ZinesView {
    zines: Vec<zine_store::ZineSummary>,
    sections: Vec<ZineSectionView>,
    pen_name: Option<PenNameDto>,
    /// Blobs this vault holds, opened or not. Held bytes are what a re-post
    /// sweep sends, so this is the corpus this vault is keeping alive.
    held_blobs: usize,
    /// Of those, the ones no key has opened yet. Shown rather than hidden: they
    /// cost space, they are kept on purpose, and a key may still arrive.
    unreadable_blobs: usize,
}

/// The answer to a write: the index the UI renders, plus which post was written
/// and whether the relay took it.
///
/// A create mints the post's id, so the caller cannot know it in advance — and
/// without it the UI would publish a second post and then open nothing, or
/// guess. Flattened rather than nested so the response is still a `ZinesView`
/// to every reader that only wants the index.
#[derive(serde::Serialize)]
struct ZineWriteView {
    zine_id: String,
    revision: u64,
    /// Whether the sealed blob reached the relay. `false` is not a lost write:
    /// the bytes are held and the re-post sweep will send them again, which is
    /// why the article is still stored and still returned.
    published: bool,
    /// Why the publish failed, when it did. Empty on success.
    publish_error: String,
    #[serde(flatten)]
    view: ZinesView,
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
/// Records written by an older build are upgraded here and the result persisted,
/// so the migration runs once, on the first read after the upgrade, rather than
/// waiting for the next edit: a missing read key is minted, and a record whose id
/// still doubles as the author's identity is re-addressed as a post.
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
    if parsed.migrate() {
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
        held_blobs: state.blobs.len(),
        unreadable_blobs: state.unreadable_blobs(),
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

/// Publish a new post under the vault's pen name, or write a new revision of one
/// this vault already wrote.
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
) -> Result<Json<ZineWriteView>, (StatusCode, Json<ApiError>)> {
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
    // A draft with no id publishes a new post; one with an id revises that post.
    // The store refuses an id this vault does not hold rather than treating it as
    // a create, so a stale UI cannot resurrect a post that was forgotten locally,
    // and it refuses a post this vault only holds, so a draft cannot be written
    // under someone else's pen name.
    let (zine_id, revision) = parsed
        .write(&pen_name.id, &pen_name.name, draft, paper_now_ms())
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    // The write is already durable before the wire is touched: a relay that is
    // unreachable, or that refuses, must not cost the author the article. The
    // blob is held either way, and the re-post sweep is what sends it later.
    let (published, publish_error) = match zines_publish(&state, &mut parsed, &zine_id, revision).await
    {
        Ok(_) => (true, String::new()),
        Err(e) => {
            eprintln!("hush-daemon: publishing zine {zine_id} rev {revision} failed: {e}");
            (false, e)
        }
    };
    zines_persist(&state, &parsed).await?;
    Ok(Json(ZineWriteView {
        zine_id,
        revision,
        published,
        publish_error,
        view: zines_view(&parsed, None, None, Some(pen_name)),
    }))
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

// ------------------------------------------------------------ the bulletin
//
// The wire half of zines. Delivery is broadcast: a sealed blob goes onto the
// relay's bulletin lane, which carries no author, no tag and no pen name, and
// every member drains the whole lane. Readability is epidemic: the read key
// travels hand to hand over the 1:1 sessions (`ZineGrant`), because there is no
// "all members" key to encrypt a post to a stranger with. Comments go the other
// way, sealed to the author's pen-name public key, so a commenter needs no
// session with the author to write one.

/// How many held blobs one pass re-posts.
///
/// The relay rate-limits a member to 60 bulletin publishes a minute, and the
/// daemon polls every few seconds, so a couple per pass stays well inside the
/// budget even when a corpus comes due all at once.
const ZINES_REPOST_PER_PASS: usize = 2;

/// How long a held blob may go unrefreshed before the sweep sends it again.
///
/// The relay's window is 30 days, so a quarter of it is the slack: a member that
/// was offline for a week still has a fortnight before anything this vault holds
/// could expire. Re-posting every poll instead would spend the member's whole
/// publish budget on traffic nobody needs.
const ZINES_REPOST_AFTER_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Seal, sign and publish one revision, and record the address it went out as.
///
/// The bytes on the wire are the blob — descriptor, signer, signature — sealed
/// under the post's read key, so the relay sees only ciphertext. Authorship is
/// the signature over the *canonical plaintext*, not over the sealed form, which
/// is what lets a holder verify who wrote a blob without holding the read key;
/// integrity is the AEAD's job.
///
/// A publish that fails is not a lost write. The blob is held either way and the
/// sweep sends it again, which is why the caller records the revision first and
/// treats the relay's answer as a status rather than a precondition.
async fn zines_publish(
    state: &App,
    parsed: &mut zine_store::ZinesState,
    zine_id: &str,
    revision: u64,
) -> Result<i64, String> {
    let Some(key) = parsed.read_key(zine_id) else {
        return Err("this post has no read key, so nothing can be published for it".to_string());
    };
    let Some(descriptor) = parsed.get(zine_id).map(|z| z.descriptor.clone()) else {
        return Err("no post with that id is held".to_string());
    };
    let Some(pen_name) = zines_pen_name_dto(state)
        .await
        .map_err(|(_, Json(err))| err.error)?
    else {
        return Err("choose a pen name before publishing".to_string());
    };
    // The signature is over the canonical bytes of the descriptor *as the store
    // holds it*, so a reader's verification and this signature agree on the same
    // bytes rather than on two nearly-identical serializations.
    let mut blob = zine_store::ZineBlob::new(descriptor, pen_name.sign_pk, String::new());
    let signature = match state.worker.zine_sign(blob.signed_bytes()).await {
        WorkResult::ZineSignature(Some(sig)) => sig,
        WorkResult::ZineSignature(None) => return Err("no pen name to sign with".to_string()),
        WorkResult::Err(msg) => return Err(msg),
        _ => return Err("unexpected worker response".to_string()),
    };
    blob.signature = STANDARD.encode(&signature);
    let plaintext =
        serde_json::to_vec(&blob).map_err(|e| format!("blob could not be encoded: {e}"))?;
    let sealed = hush_core::seal_symmetric(&key, &plaintext)
        .map_err(|e| format!("blob could not be sealed: {e}"))?;
    let hash = zine_store::blob_hash(&sealed);
    // Recorded before the relay is asked: the address is what a grant names and
    // what a re-post sends, and it is a fact about the bytes rather than about
    // the relay's answer.
    parsed.record_published(zine_id, revision, &hash, &sealed, paper_now_ms())?;
    match state.worker.bulletin_put(sealed).await {
        WorkResult::BulletinSeq(seq) => Ok(seq),
        WorkResult::Err(msg) => Err(msg),
        _ => Err("unexpected worker response".to_string()),
    }
}

/// Take one blob off the drain and place it.
///
/// The dedup guard is the drain's, not the placement's: see [`zines_place`] for
/// why the two are apart.
async fn zines_absorb(
    parsed: &mut zine_store::ZinesState,
    seal: Option<&hush_core::DropKeyPair>,
    body: &[u8],
    at_ms: u64,
    now_ms: u64,
) -> bool {
    let hash = zine_store::blob_hash(body);
    if parsed.holds_blob(&hash) {
        return false;
    }
    zines_place(parsed, seal, &hash, body, at_ms, now_ms).await
}

/// Open a blob's bytes against the keys this vault holds and file what it says.
///
/// A blob is opaque, so this is trial decryption and the order matters: a read
/// key this vault holds first (an article), then the pen name's own seal key (a
/// comment addressed to it). A comment never opens with a read key, so trying
/// keys in this order keeps a lane full of other people's comments from being
/// trial-decrypted against every zine key. Nothing that opens neither is
/// dropped: it is held unreadable, because a key may still arrive and because
/// holding it is what keeps it in the relay's window for the readers who can.
///
/// Separate from the drain's dedup guard because the retry sweep exists to
/// revisit a blob that is *held and unopened*, which a "do I already have this"
/// check would skip — and a held blob that still opens to nothing must answer
/// "nothing changed", or every poll would rewrite the sidecar.
///
/// Answers whether anything about the corpus changed.
async fn zines_place(
    parsed: &mut zine_store::ZinesState,
    seal: Option<&hush_core::DropKeyPair>,
    hash: &str,
    body: &[u8],
    at_ms: u64,
    now_ms: u64,
) -> bool {
    // 1. An article, sealed under a post's read key.
    if let Some((key, plaintext)) = zines_open_with_a_read_key(parsed, hash, body) {
        let Ok(blob) = serde_json::from_slice::<zine_store::ZineBlob>(&plaintext) else {
            // Opened with a key this vault holds, but not a zine envelope. Kept
            // rather than dropped: it is sealed to us, so a later build may know
            // what it is.
            eprintln!("hush-daemon: zine blob {hash} is not a zine envelope");
            parsed.hold_blob(hash, body, at_ms, false);
            return true;
        };
        return match parsed.ingest_blob(&blob, hash, body, key, now_ms) {
            Ok(_) => true,
            Err(e) => {
                // It opened, so it was sealed for a post this vault can read,
                // which makes the signature or the chain what failed. Not held:
                // a re-post would hand every other member a revision none of
                // them can trust.
                eprintln!("hush-daemon: zine blob {hash} refused: {e}");
                false
            }
        };
    }
    // 2. A comment, sealed to this vault's pen name rather than to a session.
    if let Some(pair) = seal {
        if let Ok(plaintext) = hush_core::open_bytes_with_key(body, pair) {
            match serde_json::from_slice::<zine_store::ZineCommentBlob>(&plaintext) {
                // Held either way: a comment that cannot be filed — for a post
                // this vault has forgotten, or past the pile's cap — is still a
                // blob worth keeping in the lane, and the reason is logged
                // rather than swallowed.
                Ok(comment) => {
                    if let Err(e) = parsed.pull_comment(&comment, hash, now_ms) {
                        eprintln!("hush-daemon: comment {hash} could not be filed: {e}");
                    }
                    parsed.hold_blob(hash, body, at_ms, true);
                    return true;
                }
                Err(e) => {
                    eprintln!("hush-daemon: blob {hash} opened as the pen name but is not a comment: {e}");
                    parsed.hold_blob(hash, body, at_ms, true);
                    return true;
                }
            }
        }
    }
    // 3. Nothing this vault holds opens it. Held unreadable, never discarded.
    //    A blob already held unreadable is a no-op, so the retry sweep can run
    //    this on every pass without reporting a change it did not make.
    let held = parsed.hold_blob(hash, body, at_ms, false);
    held.added || held.dropped > 0
}

/// The read key that opens a blob, and the plaintext it opened to.
///
/// A grant names the address of the newest blob the sender held, so a blob that
/// matches one is opened with that one key rather than with every key in turn —
/// which is the point of putting a hash in the grant, and the common case for a
/// reader that has just been handed a post.
fn zines_open_with_a_read_key(
    parsed: &zine_store::ZinesState,
    hash: &str,
    body: &[u8],
) -> Option<([u8; 32], Vec<u8>)> {
    for file in &parsed.zines {
        if file.expected_blob == hash {
            if let Some(key) = parsed.read_key(&file.descriptor.zine_id) {
                if let Ok(plain) = hush_core::open_symmetric(&key, body) {
                    return Some((key, plain));
                }
            }
        }
    }
    for (_, key) in parsed.read_keys() {
        if let Ok(plain) = hush_core::open_symmetric(&key, body) {
            return Some((key, plain));
        }
    }
    None
}

/// Try every blob this vault holds unopened against the keys it now holds.
///
/// The second half of the epidemic, and a sweep rather than a hook because the
/// two halves arrive in either order: a grant can land before its blob (the post
/// exists as a placeholder until the bytes turn up) or after it (the blob was
/// held unreadable until the key arrived). Both end here.
async fn zines_retry_unreadable(
    parsed: &mut zine_store::ZinesState,
    seal: Option<&hush_core::DropKeyPair>,
    now_ms: u64,
) -> bool {
    let pending: Vec<(String, Vec<u8>)> = parsed
        .blobs
        .iter()
        .filter(|b| !b.readable)
        .filter_map(|b| {
            STANDARD
                .decode(&b.body)
                .ok()
                .map(|raw| (b.hash.clone(), raw))
        })
        .collect();
    let mut changed = false;
    for (hash, body) in pending {
        // The relay's own inserted_at is not kept beside the bytes here, so the
        // local clock stands in: it only drives the re-post interval.
        if zines_place(parsed, seal, &hash, &body, now_ms, now_ms).await {
            changed = true;
        }
    }
    changed
}

/// One pass of the bulletin lane: take the keys, drain, place, retry, re-post.
///
/// Runs on the poll rather than on a timer of its own: it is the same relay, the
/// same unlocked account and the same sidecar, and a second timer would only be a
/// second thing racing to write `zines.enc`. The cost is that the lane moves
/// while the app is polling and stands still when it is not — a daemon with no UI
/// attached does not sweep, which is the same shape the papers already have.
async fn zines_sync(
    state: &Arc<App>,
    grants: &[hush_core::ReceivedZineGrant],
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let mut parsed = zines_sidecar(state).await?;
    let now = paper_now_ms();
    let mut changed = false;

    // 1. Keys first. A grant may be the only reason a blob already held is
    //    readable, and the retry below runs against the keys this vault holds
    //    *now*, so applying them before the drain means one pass is enough.
    for grant in grants {
        if parsed.apply_grant(&grant.zine_id, &grant.key, &grant.latest_hash, now) {
            changed = true;
        }
    }

    // 2. The pen name's seal key, for comments. Absent when this vault has no pen
    //    name, in which case nothing can be sealed to it and there is nothing to
    //    try.
    let seal = match state.worker.zine_seal_key().await {
        WorkResult::ZineSealKey(pair) => pair,
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };

    // 3. Drain from where the last pass stopped. The cursor is persisted, so a
    //    member that was away for a week reads the lane from where it stopped
    //    rather than from the start.
    let blobs = match state.worker.bulletin_list(parsed.bulletin_seq).await {
        WorkResult::Bulletin(blobs) => blobs,
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };
    for blob in &blobs {
        parsed.bulletin_seq = parsed.bulletin_seq.max(blob.seq);
        // The relay stamps `inserted_at` in seconds; everything on this side is
        // milliseconds, and a mixed unit would make every drained blob look
        // overdue for a re-post the moment it arrived.
        let at_ms = blob.inserted_at.max(0) as u64 * 1000;
        changed |= zines_absorb(&mut parsed, seal.as_ref(), &blob.body, at_ms, now).await;
    }

    // 4. Blobs that arrived before their key did.
    changed |= zines_retry_unreadable(&mut parsed, seal.as_ref(), now).await;

    // 5. Re-post what this vault holds that the relay has not seen for a while,
    //    which is what keeps a zine in the window with its author gone. Done
    //    last so a drain that filled the pool is already accounted for, and the
    //    timestamps are persisted so a restart does not send the same batch
    //    again.
    for (hash, body) in parsed.repost_due(now, ZINES_REPOST_AFTER_MS, ZINES_REPOST_PER_PASS) {
        match state.worker.bulletin_put(body).await {
            WorkResult::BulletinSeq(_) => {
                parsed.note_published(&hash, now);
                changed = true;
            }
            WorkResult::Err(msg) => {
                eprintln!("hush-daemon: zine re-post of {hash} failed: {msg}")
            }
            _ => eprintln!("hush-daemon: zine re-post of {hash} got an unexpected answer"),
        }
    }

    if changed {
        zines_persist(state, &parsed).await?;
    }
    // A cap that evicted something is a loss the member should hear about, in
    // the log rather than in a count nobody reads: the honest statement is which
    // cap and how much, and the answer is the same one the store's docs give.
    let dropped = parsed.take_dropped();
    if dropped > 0 {
        eprintln!(
            "hush-daemon: dropped {dropped} held zine blob(s) to stay inside the caps \
             ({} blobs, {} bytes); superseded revisions go first",
            zine_store::BLOBS_CAP,
            zine_store::BLOB_BYTES_CAP
        );
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct ZineGrantBody {
    /// The contact's nickname, the same handle every other route takes.
    contact: String,
}

/// Hand one contact this post's read key (the key epidemic, sender side).
///
/// Any holder may share, not just the author: that is what makes readability
/// spread to members the author has no session with. There is nothing to
/// authorize and nothing to sign, because a grant can only confer read of a post
/// the sender could already read — so the refusal here is about *having* the key,
/// not about being entitled to pass it on.
async fn zines_grant(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
    Json(body): Json<ZineGrantBody>,
) -> Result<Json<ZinesView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = zines_sidecar(&state).await?;
    let Some(key) = parsed.read_key(&zine_id) else {
        // Either the post is not held or no key for it has arrived. Both mean
        // the same thing to the caller: there is nothing here to hand over.
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "this vault has no read key for that post, so it cannot be shared".to_string(),
            }),
        ));
    };
    let latest_hash = parsed.head_blob(&zine_id);
    let contact = body.contact.trim();
    if contact.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "a grant needs a contact to send it to".to_string(),
            }),
        ));
    }
    match state
        .worker
        .zine_grant_send(contact, &zine_id, key.to_vec(), &latest_hash)
        .await
    {
        WorkResult::Ok => {}
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    }
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(zines_view(&parsed, None, None, pen_name)))
}

#[derive(serde::Deserialize)]
struct ZineCommentBody {
    #[serde(default)]
    body: String,
}

/// Write a comment: seal it to the author's pen-name public key and publish it.
///
/// No session with the author is needed, which is the whole reason a comment is
/// sealed to a key rather than sent to an inbox: the pen name is a public key
/// that travels in the article's descriptor. What the commenter writes is
/// sanitized here with the same rule the author applies on ingest, so the body
/// that gets hashed is the body that will be read.
///
/// The commenter keeps its own blob. That is not vanity: every holder is part of
/// the availability story, and a commenter is a holder.
async fn zines_comment(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
    Json(body): Json<ZineCommentBody>,
) -> Result<Json<ZineWriteView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = zines_sidecar(&state).await?;
    let Some(file) = parsed.get(&zine_id) else {
        return Err(zine_not_held(&zine_id));
    };
    let pen_id = file.descriptor.pen_id.clone();
    let revision = file.descriptor.revision;
    let Some(pen_pk) = hex::decode(&pen_id).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()) else {
        // A post with no usable pen-name key is one whose author cannot be
        // addressed at all. Refused with the reason rather than published into
        // the void.
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "that post names no pen name to seal a comment to".to_string(),
            }),
        ));
    };
    let text = zine_store::normalize_comment_body(&body.body);
    if text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "an empty comment is not worth publishing".to_string(),
            }),
        ));
    }
    // The byline is this vault's own pen name when it has one, so a reader knows
    // who is talking; a commenter without one is anonymous rather than refused,
    // because a pen name is a deliberate act and a comment is not the place to
    // demand one.
    let byline = zines_pen_name_dto(&state)
        .await?
        .map(|p| p.name)
        .unwrap_or_default();
    let comment = zine_store::ZineCommentBlob::new(zine_id.clone(), byline, text, paper_now_ms());
    let plaintext =
        serde_json::to_vec(&comment).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: e.to_string() })))?;
    let sealed = hush_core::seal_bytes_to_key(&plaintext, &pen_pk).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: format!("the comment could not be sealed: {e}"),
            }),
        )
    })?;
    let hash = zine_store::blob_hash(&sealed);
    let (published, publish_error) = match state.worker.bulletin_put(sealed.clone()).await {
        WorkResult::BulletinSeq(_) => (true, String::new()),
        WorkResult::Err(msg) => {
            eprintln!("hush-daemon: publishing a comment on {zine_id} failed: {msg}");
            (false, msg)
        }
        _ => (false, "unexpected worker response".to_string()),
    };
    // Held as readable: this vault wrote it, so it knows what it is, and holding
    // it is what lets the commenter's own sweep keep it alive.
    parsed.hold_blob(&hash, &sealed, paper_now_ms(), true);
    zines_persist(&state, &parsed).await?;
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(ZineWriteView {
        zine_id,
        revision,
        published,
        publish_error,
        view: zines_view(&parsed, None, None, pen_name),
    }))
}

#[derive(serde::Deserialize)]
struct ZineCommentRefBody {
    comment_hash: String,
}

/// Accept a pulled comment: merge it, then republish the post it belongs to.
///
/// The thread lives inside the descriptor, so the merge is a revision — and the
/// republish is not optional, because until it happens nobody else can see the
/// comment. Author-side only by construction: only the pen name that wrote a post
/// can sign a revision of it.
async fn zines_accept_comment(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
    Json(body): Json<ZineCommentRefBody>,
) -> Result<Json<ZineWriteView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = zines_sidecar(&state).await?;
    let revision = parsed
        .accept_comment(&zine_id, &body.comment_hash, paper_now_ms())
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    let (published, publish_error) = match zines_publish(&state, &mut parsed, &zine_id, revision).await
    {
        Ok(_) => (true, String::new()),
        Err(e) => {
            eprintln!("hush-daemon: republishing {zine_id} after a merge failed: {e}");
            (false, e)
        }
    };
    zines_persist(&state, &parsed).await?;
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(ZineWriteView {
        zine_id,
        revision,
        published,
        publish_error,
        view: zines_view(&parsed, None, None, pen_name),
    }))
}

/// Refuse a pulled comment: drop it from the pile.
///
/// Not a retraction, because nothing was ever published: the comment is sealed to
/// this vault alone and a refusal is simply never merged and never republished.
/// The blob itself stays on the lane until it ages out, and any other holder
/// keeps re-posting it — which is the honest limit of author-side moderation.
async fn zines_discard_comment(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
    Json(body): Json<ZineCommentRefBody>,
) -> Result<Json<ZineView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = zines_sidecar(&state).await?;
    if !parsed.discard_comment(&zine_id, &body.comment_hash) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "no comment with that hash is waiting".to_string(),
            }),
        ));
    }
    zines_persist(&state, &parsed).await?;
    let zine = parsed
        .get(&zine_id)
        .ok_or_else(|| zine_not_held(&zine_id))?
        .clone();
    Ok(Json(ZineView { zine }))
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

/// The public identity this vault publishes and comments under on the public
/// Nostr network (neighborhoods, Task 22). `null` before one exists.
async fn neighborhoods_identity(
    State(state): State<Arc<App>>,
) -> Result<Json<Option<PublicIdentityDto>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match state.worker.public_identity().await {
        WorkResult::PublicIdentity(id) => Ok(Json(id)),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Mint the public identity, returning the one now in force.
///
/// A create when one already exists returns it rather than erroring: the
/// caller's question is "which identity do I publish under", and the answer is
/// the same either way.
async fn neighborhoods_create_identity(
    State(state): State<Arc<App>>,
) -> Result<Json<PublicIdentityDto>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match state.worker.create_public_identity().await {
        WorkResult::PublicIdentity(Some(id)) => Ok(Json(id)),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

// --- Neighborhoods: the read lane (Task 22) -------------------------------
//
// The crate (`hush-articles`) holds the lane's logic — the gate, the dedupe, the
// index — and knows nothing of the vault or the network. The daemon owns the
// sidecar (`neighborhoods.enc`), the routes, and the transport (`App`'s
// `neighborhoods_source`), which stays `None` until a Tor relay client is wired
// in. A refresh with no transport is a clear refusal, not a silent no-op.

/// The cap on the allowlist. A curated set, not a firehose.
const NEIGHBORHOOD_ALLOWLIST_CAP: usize = 50;
/// The cap on the reading-time floor, in minutes.
const NEIGHBORHOOD_MINUTES_CAP: u32 = 180;

/// The lane's settings as the UI sees them.
#[derive(Serialize)]
struct NeighborhoodSettingsDto {
    allowlist: Vec<String>,
    min_minutes: u32,
    default_show: bool,
}

impl From<&hush_articles::Settings> for NeighborhoodSettingsDto {
    fn from(s: &hush_articles::Settings) -> Self {
        Self {
            allowlist: s.allowlist.clone(),
            min_minutes: s.min_minutes,
            default_show: s.default_show,
        }
    }
}

/// A patch to the settings: the caller sends only the fields it changed.
#[derive(Deserialize)]
struct NeighborhoodSettingsBody {
    allowlist: Option<Vec<String>>,
    min_minutes: Option<u32>,
    default_show: Option<bool>,
}

/// One article in the index. No body, so the index stays a list.
#[derive(Serialize)]
struct NeighborhoodSummaryDto {
    address: String,
    event_id: String,
    pubkey: String,
    d: String,
    title: String,
    summary: Option<String>,
    sections: Vec<String>,
    created_at: u64,
    published_at: Option<u64>,
    reading_minutes: u32,
}

impl From<&hush_articles::Article> for NeighborhoodSummaryDto {
    fn from(a: &hush_articles::Article) -> Self {
        Self {
            address: a.address(),
            event_id: a.event_id.clone(),
            pubkey: a.pubkey.clone(),
            d: a.d.clone(),
            title: a.title.clone(),
            summary: a.summary.clone(),
            sections: a.sections.clone(),
            created_at: a.created_at,
            published_at: a.published_at,
            reading_minutes: hush_articles::reading_minutes(&a.content),
        }
    }
}

/// One article in full, with its Markdown body. The body is stored as the relay
/// sent it; rendering and sanitising happen in the UI's one renderer.
#[derive(Serialize)]
struct NeighborhoodArticleDto {
    #[serde(flatten)]
    summary: NeighborhoodSummaryDto,
    content: String,
    /// The thread under this article, oldest first. Empty for an article whose
    /// replies this vault does not read — which is every article but the ones it
    /// published itself.
    replies: Vec<NeighborhoodCommentDto>,
}

/// One public comment, as the page reads it.
///
/// No `mine` flag: whether a comment is this vault's own is a comparison the
/// page makes against the public identity it already holds, and asking the
/// worker for that identity on every article open would be a second read of the
/// vault to answer a question the reader can answer itself.
#[derive(Serialize)]
struct NeighborhoodCommentDto {
    event_id: String,
    pubkey: String,
    /// Plaintext, by the spec. The page renders it as text, never as markup: a
    /// comment is not Markdown, and a stranger's text is not markup here.
    content: String,
    created_at: u64,
    /// The comment this answers, when it answers one rather than the article.
    parent_id: Option<String>,
}

impl From<&hush_articles::Comment> for NeighborhoodCommentDto {
    fn from(c: &hush_articles::Comment) -> Self {
        Self {
            event_id: c.event_id.clone(),
            pubkey: c.pubkey.clone(),
            content: c.content.clone(),
            created_at: c.created_at,
            parent_id: c.parent_id.clone(),
        }
    }
}

fn neighborhood_thread(
    parsed: &hush_articles::NeighborhoodsState,
    address: &str,
) -> Vec<NeighborhoodCommentDto> {
    parsed
        .replies_for(address)
        .iter()
        .map(NeighborhoodCommentDto::from)
        .collect()
}

/// The index payload: what the page shows, plus the facets it can filter by.
#[derive(Serialize)]
struct NeighborhoodsView {
    articles: Vec<NeighborhoodSummaryDto>,
    sections: Vec<NeighborhoodSectionView>,
    settings: NeighborhoodSettingsDto,
}

#[derive(Serialize)]
struct NeighborhoodSectionView {
    name: String,
    count: usize,
}

fn neighborhoods_view(
    parsed: &hush_articles::NeighborhoodsState,
    section: Option<&str>,
    pubkey: Option<&str>,
) -> NeighborhoodsView {
    NeighborhoodsView {
        articles: parsed
            .index(section, pubkey)
            .iter()
            .map(NeighborhoodSummaryDto::from)
            .collect(),
        sections: parsed
            .sections()
            .into_iter()
            .map(|(name, count)| NeighborhoodSectionView { name, count })
            .collect(),
        settings: NeighborhoodSettingsDto::from(parsed.settings()),
    }
}

/// Read the cache and settings sidecar. A missing file is an empty cache, not an
/// error: that is the state before the first refresh.
async fn neighborhoods_state(
    state: &App,
) -> Result<hush_articles::NeighborhoodsState, (StatusCode, Json<ApiError>)> {
    match state.worker.secret_read(hush_articles::STATE_FILE).await {
        WorkResult::Secret(raw) => hush_articles::NeighborhoodsState::decode(raw.as_deref())
            .map_err(neighborhoods_internal),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn neighborhoods_persist(
    state: &App,
    parsed: &hush_articles::NeighborhoodsState,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let bytes = parsed.encode().map_err(neighborhoods_internal)?;
    match state.worker.secret_write(hush_articles::STATE_FILE, bytes).await {
        WorkResult::Ok => Ok(()),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

fn neighborhoods_internal(message: String) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError { error: message }),
    )
}

fn neighborhoods_bad_request(message: String) -> (StatusCode, Json<ApiError>) {
    (StatusCode::BAD_REQUEST, Json(ApiError { error: message }))
}

/// A relay URL must be a websocket URL with a host. `ws://` is allowed for a
/// local or onion relay; the shipped allowlist is all `wss://`.
fn validate_relay_url(url: &str) -> Result<(), (StatusCode, Json<ApiError>)> {
    let trimmed = url.trim();
    let host = trimmed
        .strip_prefix("wss://")
        .or_else(|| trimmed.strip_prefix("ws://"));
    match host {
        Some(host) if !host.trim_matches('/').is_empty() => Ok(()),
        _ => Err(neighborhoods_bad_request(format!(
            "`{url}` is not a websocket relay URL"
        ))),
    }
}

#[derive(Deserialize)]
struct NeighborhoodIndexQuery {
    section: Option<String>,
    #[serde(rename = "pub")]
    pubkey: Option<String>,
}

/// The cached public index, newest first, filtered by section and author.
async fn neighborhoods_index(
    State(state): State<Arc<App>>,
    Query(query): Query<NeighborhoodIndexQuery>,
) -> Result<Json<NeighborhoodsView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = neighborhoods_state(&state).await?;
    Ok(Json(neighborhoods_view(
        &parsed,
        query.section.as_deref(),
        query.pubkey.as_deref(),
    )))
}

/// One article with its body. 404 before it is held: this lane never fetches on
/// demand, so an address the cache does not carry is simply not here yet.
async fn neighborhoods_get(
    State(state): State<Arc<App>>,
    Path(addr): Path<String>,
) -> Result<Json<NeighborhoodArticleDto>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = neighborhoods_state(&state).await?;
    match parsed.get(&addr) {
        Some(article) => Ok(Json(NeighborhoodArticleDto {
            summary: NeighborhoodSummaryDto::from(article),
            content: article.content.clone(),
            replies: neighborhood_thread(&parsed, &addr),
        })),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("no neighborhood article is held for {addr}"),
            }),
        )),
    }
}

/// Force a pull from the allowlisted relays (the switch-on path).
async fn neighborhoods_refresh(
    State(state): State<Arc<App>>,
) -> Result<Json<NeighborhoodsView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    // Read the sidecar first. There is no lock middleware: a locked vault is a
    // 401 because the worker says so, so a route must reach the worker before it
    // can answer anything else. Doing this after the transport check would let a
    // locked vault learn whether a transport is configured.
    let mut parsed = neighborhoods_state(&state).await?;

    // The lane reads over Tor or not at all. The shell reports the SOCKS port
    // when it starts Tor; without it there is no route, and a direct connection
    // is not one the daemon will take on the user's behalf.
    let source: Arc<dyn hush_articles::RelaySource> =
        match state.neighborhoods_source.lock().await.clone() {
            Some(source) => source,
            None => {
                let Some(proxy) = state.relay_socks.lock().await.clone() else {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(ApiError {
                            error: "Tor is not running; neighborhoods are read over Tor only"
                                .to_string(),
                        }),
                    ));
                };
                Arc::new(hush_articles::RelayClient::via_tor(proxy))
            }
        };

    hush_articles::refresh(&source, &mut parsed, &export_scopes(&state).await?)
        .await
        .map_err(|m| (StatusCode::BAD_GATEWAY, Json(ApiError { error: m })))?;
    neighborhoods_persist(&state, &parsed).await?;
    Ok(Json(neighborhoods_view(&parsed, None, None)))
}

/// The threads this vault has a stake in: the addresses of the articles it
/// published.
///
/// The reply pass reads only these. A member who has exported nothing has no
/// thread anyone could be answering them on, so there is no second request to
/// make — which is the egress rule applied to the read lane's second half.
///
/// Read through the same worker seam as everything else, so a locked vault
/// refuses here rather than reading a sidecar the caller is not entitled to.
async fn export_scopes(state: &App) -> Result<Vec<String>, (StatusCode, Json<ApiError>)> {
    Ok(zines_sidecar(state).await?.export_scopes())
}

async fn neighborhoods_settings(
    State(state): State<Arc<App>>,
) -> Result<Json<NeighborhoodSettingsDto>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let parsed = neighborhoods_state(&state).await?;
    Ok(Json(NeighborhoodSettingsDto::from(parsed.settings())))
}

/// Persist a settings patch. Changing the allowlist does not itself read from it;
/// egress is still the switch, and the switch is not this route.
async fn neighborhoods_set_settings(
    State(state): State<Arc<App>>,
    Json(body): Json<NeighborhoodSettingsBody>,
) -> Result<Json<NeighborhoodSettingsDto>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = neighborhoods_state(&state).await?;
    let mut settings = parsed.settings().clone();

    if let Some(allowlist) = body.allowlist {
        if allowlist.len() > NEIGHBORHOOD_ALLOWLIST_CAP {
            return Err(neighborhoods_bad_request(format!(
                "an allowlist holds at most {NEIGHBORHOOD_ALLOWLIST_CAP} relays"
            )));
        }
        for relay in &allowlist {
            validate_relay_url(relay)?;
        }
        settings.allowlist = allowlist;
    }
    if let Some(minutes) = body.min_minutes {
        if minutes > NEIGHBORHOOD_MINUTES_CAP {
            return Err(neighborhoods_bad_request(format!(
                "the reading-time floor is at most {NEIGHBORHOOD_MINUTES_CAP} minutes"
            )));
        }
        settings.min_minutes = minutes;
    }
    if let Some(default_show) = body.default_show {
        settings.default_show = default_show;
    }

    parsed.set_settings(settings);
    neighborhoods_persist(&state, &parsed).await?;
    Ok(Json(NeighborhoodSettingsDto::from(parsed.settings())))
}

// ------------------------------------------------ the public comment (Task 22)
//
// The write half of the comment lane. Three things keep it separate from the
// private thread, and all three are structural:
//
// 1. It is signed by the **public identity**, not the pen name, and the two keys
//    are never derived from one another.
// 2. It goes to the **public relays** as plaintext. Nothing here seals, and
//    nothing here can: `CommentPlan` has no read key and no recipient.
// 3. It is stored in the **neighborhoods** sidecar, beside the articles it
//    answers, so the private thread's store never learns a public comment
//    exists.
//
// The order of the refusals is the export's: the sidecar first, so a locked
// vault is a 401 rather than a hint about which keys and relays are configured;
// then the article, then the body, then the parent, then the identity, and only
// then is anything signed. A comment that could not go out must not be cached as
// though it had.

/// The cap on a comment, in characters. A comment is a reply, not an article:
/// the long-form lane is where writing lives.
const NEIGHBORHOOD_COMMENT_CHARS: usize = 2000;

/// A comment to post, and what it answers.
#[derive(Deserialize)]
struct NeighborhoodCommentBody {
    body: String,
    /// The comment this answers, when it answers one rather than the article.
    /// Absent or empty is a top-level comment.
    #[serde(default)]
    parent_id: Option<String>,
}

/// What posting a comment answers with: the thread as it now stands.
///
/// The thread rather than the one comment, because the page shows a thread and
/// the caller should not have to splice its own words into a list it did not
/// build.
#[derive(Serialize)]
struct NeighborhoodCommentView {
    address: String,
    /// The id of the event that went out, hex.
    event_id: String,
    /// Whether a relay took it. `false` means the comment is not public: unlike
    /// an export there is no address to retry, so it is not cached either.
    published: bool,
    /// Why the publish failed, when it did. Empty on success.
    publish_error: String,
    replies: Vec<NeighborhoodCommentDto>,
}

/// Post a NIP-22 comment on a public article, signed by the public identity.
async fn neighborhoods_comment(
    State(state): State<Arc<App>>,
    Path(addr): Path<String>,
    Json(body): Json<NeighborhoodCommentBody>,
) -> Result<Json<NeighborhoodCommentView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = neighborhoods_state(&state).await?;

    // The article must be held. A comment names the version it was written
    // against, so commenting on something this vault does not hold would mean
    // guessing at an event id — and a guessed one points a reader at nothing.
    let article = parsed.get(&addr).cloned().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("no neighborhood article is held for {addr}"),
            }),
        )
    })?;

    let content = body.body.trim();
    if content.is_empty() {
        return Err(neighborhoods_bad_request(
            "a comment needs something in it".to_string(),
        ));
    }
    if content.chars().count() > NEIGHBORHOOD_COMMENT_CHARS {
        return Err(neighborhoods_bad_request(format!(
            "a comment is at most {NEIGHBORHOOD_COMMENT_CHARS} characters"
        )));
    }

    // A reply has to answer a comment this vault holds **in this thread**.
    // NIP-22 keeps the article as the root whatever is answered, so a parent
    // from another thread would file the reply under the wrong article — and a
    // parent this vault cannot read is one whose author it cannot name.
    let parent = match body
        .parent_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => {
            let held = parsed
                .replies_for(&addr)
                .into_iter()
                .find(|c| c.event_id == id)
                .ok_or_else(|| {
                    neighborhoods_bad_request(format!(
                        "no comment {id} is held in the thread under {addr}"
                    ))
                })?;
            Some(hush_articles::CommentParent {
                event_id: held.event_id,
                pubkey: held.pubkey,
            })
        }
        None => None,
    };

    // The same refusal as the export: the identity is minted deliberately, so a
    // comment refuses rather than creating the key it would speak under.
    let identity = match state.worker.public_identity().await {
        WorkResult::PublicIdentity(Some(id)) => id,
        WorkResult::PublicIdentity(None) => {
            return Err((
                StatusCode::CONFLICT,
                Json(ApiError {
                    error: "mint a public identity before commenting in the neighborhoods"
                        .to_string(),
                }),
            ))
        }
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };

    let relays = parsed.settings().allowlist.clone();
    if relays.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "no relays are configured; add one in Neighborhoods settings before commenting"
                    .to_string(),
            }),
        ));
    }

    let sink: Arc<dyn hush_articles::RelaySink> = match state.neighborhoods_sink.lock().await.clone()
    {
        Some(sink) => sink,
        None => {
            let Some(proxy) = state.relay_socks.lock().await.clone() else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ApiError {
                        error: "Tor is not running; comments are posted over Tor only".to_string(),
                    }),
                ));
            };
            Arc::new(hush_articles::RelayClient::via_tor(proxy))
        }
    };

    let now_s = paper_now_ms() / 1000;
    let plan = hush_articles::CommentPlan {
        root: hush_articles::CommentRoot {
            address: addr.clone(),
            event_id: article.event_id.clone(),
            pubkey: article.pubkey.clone(),
        },
        parent,
        content: content.to_string(),
        relays: relays.clone(),
    };

    let unsigned = plan
        .event(&identity.pubkey, now_s)
        .map_err(neighborhoods_bad_request)?;
    let id_bytes = unsigned.id_bytes().map_err(neighborhoods_internal)?;
    let signature = match state.worker.public_identity_sign(id_bytes.to_vec()).await {
        WorkResult::PublicIdentitySignature(Some(sig)) => sig,
        WorkResult::PublicIdentitySignature(None) => {
            return Err((
                StatusCode::CONFLICT,
                Json(ApiError {
                    error: "mint a public identity before commenting in the neighborhoods"
                        .to_string(),
                }),
            ))
        }
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };
    let event = unsigned.signed(&signature).map_err(neighborhoods_internal)?;

    // A comment that no relay took is not public, so it is not cached: unlike an
    // export there is no address to come back to, and showing the member their
    // own words in a thread they cannot point anyone at would be a lie the page
    // has no way to correct later.
    let (published, publish_error) = match sink.publish(&relays, &event).await {
        Ok(report) => {
            eprintln!("hush-daemon: commented on {addr}: {}", report.summary());
            (true, String::new())
        }
        Err(message) => {
            eprintln!("hush-daemon: commenting on {addr} failed: {message}");
            (false, message)
        }
    };

    if published {
        parsed.absorb_replies(vec![hush_articles::Comment {
            event_id: event.id.clone(),
            pubkey: identity.pubkey.clone(),
            content: content.to_string(),
            created_at: now_s,
            root: addr.clone(),
            parent_id: plan.parent.as_ref().map(|p| p.event_id.clone()),
        }]);
        neighborhoods_persist(&state, &parsed).await?;
    }

    Ok(Json(NeighborhoodCommentView {
        address: addr.clone(),
        event_id: event.id,
        published,
        publish_error,
        replies: neighborhood_thread(&parsed, &addr),
    }))
}

fn zine_not_held(zine_id: &str) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: format!("no zine is held for {zine_id}"),
        }),
    )
}

// ------------------------------------------------------ the export (Task 11)
//
// The one-way door from a private zine to a public NIP-23 article. Three things
// make it one-way, and all three are structural rather than checks:
//
// 1. `ExportPlan` has no field for a comment thread, a revision history, a read
//    key or a pen name, so there is nowhere for the private parts to be carried.
// 2. The body is re-encoded from the sanitized HTML to Markdown, so the wire
//    carries text rather than the author's stored markup.
// 3. The `d` that links the private post to the public article is stored on
//    `ZineFile` and never in the descriptor, which is what is sealed and
//    published — so the link does not travel with the blob.
//
// A publish that fails is not a lost article: the address is recorded either
// way, and a retry publishes the same address rather than a second one.

/// What an export answers with: the article's address, and the post as it now
/// stands.
///
/// `ZineView` is flattened rather than nested so the reader page can show
/// "published" without a second request, and so the shape is still a zine to a
/// caller that only reads that half.
#[derive(serde::Serialize)]
struct ZineExportView {
    zine_id: String,
    /// The NIP-23 `d` this article lives at. Minted once, stable across every
    /// re-export.
    d: String,
    /// The NIP-01 address `30023:pubkey:d`. This is what a NIP-22 comment
    /// references, and it is stable across edits.
    address: String,
    /// The shareable handle, with the allowlist as relay hints.
    naddr: String,
    /// The id of the event that was published, hex.
    event_id: String,
    /// The public key the article is signed by, hex.
    pubkey: String,
    /// When the article first went public, seconds. Set once.
    published_at: u64,
    /// Whether a relay took it. `false` is not a lost article: the address is
    /// recorded and the retry publishes the same one.
    published: bool,
    /// Why the publish failed, when it did. Empty on success.
    publish_error: String,
    #[serde(flatten)]
    view: ZineView,
}

/// Publish a zine to the neighborhoods as a NIP-23 article.
///
/// The order of the refusals matters as much as the publish does: the sidecar is
/// read first, so a locked vault is a 401 rather than a hint about which keys
/// and relays are configured; then the post, then the public identity, then the
/// allowlist, and only then is an address minted. An export that could not go
/// out must not leave a `d` behind for the next attempt to find and reuse.
async fn zines_export(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
) -> Result<Json<ZineExportView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut parsed = zines_sidecar(&state).await?;

    let zine = parsed
        .get(&zine_id)
        .cloned()
        .ok_or_else(|| zine_not_held(&zine_id))?;
    // A held zine is someone else's post. Publishing it would sign the author's
    // words under this vault's public identity, which is a different person
    // saying it.
    if !zine.authored {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "only a zine this vault wrote can be published; a held one belongs to its author"
                    .to_string(),
            }),
        ));
    }

    // The article is signed by the public identity, which is a separate key from
    // the pen name and from the account. Minting it is a deliberate act, so a
    // stray export refuses rather than creating the identity it would publish
    // under.
    let identity = match state.worker.public_identity().await {
        WorkResult::PublicIdentity(Some(id)) => id,
        WorkResult::PublicIdentity(None) => {
            return Err((
                StatusCode::CONFLICT,
                Json(ApiError {
                    error: "mint a public identity before publishing to the neighborhoods"
                        .to_string(),
                }),
            ))
        }
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };

    let relays = neighborhoods_state(&state).await?.settings().allowlist.clone();
    if relays.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "no relays are configured; add one in Neighborhoods settings before publishing"
                    .to_string(),
            }),
        ));
    }

    // The write half of the lane, wired the same way the read half is: over Tor
    // or not at all. A direct connection is not one the daemon takes on the
    // member's behalf.
    let sink: Arc<dyn hush_articles::RelaySink> = match state.neighborhoods_sink.lock().await.clone()
    {
        Some(sink) => sink,
        None => {
            let Some(proxy) = state.relay_socks.lock().await.clone() else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ApiError {
                        error: "Tor is not running; articles are published over Tor only".to_string(),
                    }),
                ));
            };
            Arc::new(hush_articles::RelayClient::via_tor(proxy))
        }
    };

    // The body is re-encoded from the sanitized HTML the daemon stores to
    // Markdown, which is what NIP-23 clients render. The stored HTML never
    // travels: a public client would show the tags as text.
    let markdown = reads::rich_text_to_markdown(&zine.descriptor.body);
    let now_ms = paper_now_ms();
    let now_s = now_ms / 1000;

    // Minted and persisted before the event is built, so a publish that failed
    // does not move the address on the retry.
    let export = parsed
        .export_address(&zine_id, &identity.pubkey, now_s)
        .map_err(|m| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: m })))?;
    zines_persist(&state, &parsed).await?;

    let plan = hush_articles::ExportPlan {
        d: export.d.clone(),
        title: zine.descriptor.title.clone(),
        // A zine has no author-written summary, so the card preview is the
        // article's own opening prose rather than a placeholder.
        summary: hush_articles::summary_of(&markdown),
        sections: zine.descriptor.sections.clone(),
        markdown,
        published_at: export.published_at,
    };

    let unsigned = plan
        .event(&identity.pubkey, now_s)
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    // The digest, not the serialized event: NIP-01 signs `sha256(...)` of the
    // array, and hashing it again here would produce a signature that verifies
    // against nothing a relay computes.
    let id_bytes = unsigned
        .id_bytes()
        .map_err(|m| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: m })))?;
    let signature = match state.worker.public_identity_sign(id_bytes.to_vec()).await {
        WorkResult::PublicIdentitySignature(Some(sig)) => sig,
        WorkResult::PublicIdentitySignature(None) => {
            return Err((
                StatusCode::CONFLICT,
                Json(ApiError {
                    error: "mint a public identity before publishing to the neighborhoods"
                        .to_string(),
                }),
            ))
        }
        WorkResult::Err(msg) => return Err(worker_err(msg)),
        _ => return Err(unexpected_worker_response()),
    };
    let event = unsigned
        .signed(&signature)
        .map_err(|m| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: m })))?;

    // A refusal is a status, not a lost article: the address is recorded either
    // way, and the next export publishes the same one.
    let (published, publish_error) = match sink.publish(&relays, &event).await {
        Ok(report) => {
            eprintln!("hush-daemon: exported zine {zine_id}: {}", report.summary());
            (true, String::new())
        }
        Err(message) => {
            eprintln!("hush-daemon: exporting zine {zine_id} failed: {message}");
            (false, message)
        }
    };

    parsed.record_export(&zine_id, &event.id, now_ms);
    zines_persist(&state, &parsed).await?;

    let naddr = plan
        .naddr(&identity.pubkey, &relays)
        .map_err(|m| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: m })))?;
    let updated = parsed
        .get(&zine_id)
        .cloned()
        .ok_or_else(|| zine_not_held(&zine_id))?;

    Ok(Json(ZineExportView {
        zine_id,
        d: plan.d.clone(),
        address: plan.address(&identity.pubkey),
        naddr,
        event_id: event.id,
        pubkey: identity.pubkey,
        published_at: export.published_at,
        published,
        publish_error,
        view: ZineView { zine: updated },
    }))
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
// Voice — conclave voice rooms (Task 23) behind `/api/voice/*`.
//
// The room's state machine (`VoiceManager`) is authoritative and in-memory;
// the control payloads fan out over the same 1:1 sessions as everything else
// in a conclave. Phase 2 replaced the Phase 1 stub SDP with the real media
// plane (`voice_data::VoicePlane`, one per live session): an offer or answer
// now carries a gathered SDP and the audio flows peer to peer. A locked vault
// is a 401 (a locked vault holds no keys and must not even learn a room
// exists); a restart is a leave; `ConclaveEnd` ends any room with the
// conclave.
// ---------------------------------------------------------------------------

/// The media mode the daemon's voice planes run in: the synthetic tone when
/// `HUSH_VOICE_TEST_TONE` is set (the dogfood's headless mode), the real
/// devices otherwise.
fn voice_media_mode() -> voice_data::MediaMode {
    if std::env::var_os("HUSH_VOICE_TEST_TONE").is_some() {
        voice_data::MediaMode::Tone
    } else {
        voice_data::MediaMode::Device
    }
}

/// The plane for `session_id`, created on demand (a room's opener never
/// dials, so its plane stays idle until the first inbound offer).
async fn voice_plane_for(state: &Arc<App>, session_id: &str) -> Arc<voice_data::VoicePlane> {
    let mut planes = state.voice_planes.lock().await;
    planes
        .entry(session_id.to_string())
        .or_insert_with(|| {
            Arc::new(voice_data::VoicePlane::new(
                hush_core::config::resolve_stun_url(&state.dir),
                voice_media_mode(),
            ))
        })
        .clone()
}

/// The plane for `session_id` when one already exists — never creates. A
/// payload that arrives for a session with no plane has nothing on the media
/// side to act on.
async fn voice_plane_get(state: &Arc<App>, session_id: &str) -> Option<Arc<voice_data::VoicePlane>> {
    state.voice_planes.lock().await.get(session_id).cloned()
}

/// Drop one session's plane and close its connections.
async fn voice_plane_drop(state: &Arc<App>, session_id: &str) {
    let plane = state.voice_planes.lock().await.remove(session_id);
    if let Some(plane) = plane {
        plane.shutdown();
    }
}

/// Close every voice plane. Locking the vault calls this beside
/// `paper_planes_shutdown`: a locked vault holds no sessions and no sockets.
async fn voice_planes_shutdown(state: &Arc<App>) {
    let planes: Vec<Arc<voice_data::VoicePlane>> = {
        let mut planes = state.voice_planes.lock().await;
        planes.drain().map(|(_, plane)| plane).collect()
    };
    for plane in planes {
        plane.shutdown();
    }
}

#[derive(Serialize)]
struct VoiceMemberView {
    account: String,
    muted: bool,
}

#[derive(Serialize)]
struct VoiceView {
    session_id: String,
    conclave_id: String,
    opener: String,
    started_ms: u64,
    /// Joined trustees in join order; the opener is first.
    joined: Vec<VoiceMemberView>,
    /// Whether this vault's own account is in the room right now.
    i_am_in: bool,
}

fn voice_view_of(session: &VoiceSession, me: &str) -> VoiceView {
    VoiceView {
        session_id: session.session.session_id.clone(),
        conclave_id: session.session.conclave_id.clone(),
        opener: session.session.opener.clone(),
        started_ms: session.session.started_ms,
        joined: session
            .joined
            .iter()
            .map(|m| VoiceMemberView {
                account: m.account.clone(),
                muted: m.muted,
            })
            .collect(),
        i_am_in: session.joined.iter().any(|m| m.account == me),
    }
}

/// The status a voice refusal maps to. The manager's messages are the `Error`
/// display strings and are stable, so the mapping is explicit here rather
/// than sniffed per handler.
fn voice_err(msg: String) -> (StatusCode, Json<ApiError>) {
    if msg == "locked" {
        return (StatusCode::UNAUTHORIZED, Json(ApiError { error: msg }));
    }
    let code = if msg.contains("already open") || msg.contains("full") {
        StatusCode::CONFLICT
    } else if msg.contains("no voice session") || msg.contains("conclave not found") {
        StatusCode::NOT_FOUND
    } else if msg.contains("not a trustee") || msg.contains("has ended") {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::BAD_REQUEST
    };
    (code, Json(ApiError { error: msg }))
}

/// The conclave roster, once per voice action — the manager's rules need it.
///
/// Call only after the lock check (`paper_account_id`): the non-answer cases
/// are swallowed here, so a locked vault must already have 401'd rather than
/// read an empty roster as "no conclave".
async fn voice_conclaves(state: &Arc<App>) -> Vec<ConclaveRecord> {
    match state.worker.conclaves().await {
        WorkResult::Conclaves(list) => list,
        _ => Vec::new(),
    }
}

/// The descriptor-shaped view of a record, for the manager's authority checks.
fn voice_conclave_descriptor(record: &ConclaveRecord) -> hush_core::ConclaveDescriptor {
    hush_core::ConclaveDescriptor {
        conclave_id: record.id.clone(),
        name: record.name.clone(),
        convener: record.convener.clone(),
        trustees: record.trustees.clone(),
        ended: record.ended,
    }
}

async fn voice_open(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<VoiceView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    // The lock check first: a locked vault is a 401, never a 404 — a locked
    // vault must not learn whether a conclave exists.
    let me = paper_account_id(&state).await?;
    let conclaves = voice_conclaves(&state).await;
    let record = conclaves.iter().find(|c| c.id == conclave_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("no conclave {conclave_id} in this vault"),
            }),
        )
    })?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let now = paper_now_ms();
    let descriptor = hush_core::VoiceDescriptor {
        session_id: session_id.clone(),
        conclave_id: conclave_id.clone(),
        opener: me.clone(),
        started_ms: now,
    };
    let view = {
        let mut mgr = state.voices.lock().await;
        mgr.open(&voice_conclave_descriptor(record), &me, session_id, now)
            .map_err(|e| voice_err(e.to_string()))?;
        let live = mgr
            .session(&conclave_id)
            .expect("the room was just opened");
        let view = voice_view_of(live, &me);
        // The opener's media plane exists from the open: it has nothing to
        // dial yet, but the first joiner's offer must find it ready to
        // answer.
        voice_plane_for(&state, &view.session_id).await;
        view
    };
    // Best-effort fan-out: the room is already local, and a send queues in the
    // outbox, so a trustee that is offline or locked receives it on drain.
    if let WorkResult::Err(e) = state.worker.send_voice_open(&conclave_id, descriptor).await {
        eprintln!("hush-daemon: voice open fan-out failed: {e}");
    }
    Ok(Json(view))
}

async fn voice_join(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<VoiceView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    // Lock before existence, as in `voice_open`.
    let me = paper_account_id(&state).await?;
    let conclaves = voice_conclaves(&state).await;
    let record = conclaves
        .iter()
        .find(|c| c.id == conclave_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: format!("no conclave {conclave_id} in this vault"),
                }),
            )
        })?;
    // The offer is the join announcement: joining dials every trustee already
    // in the room (the spec's §7 join row), and a peer that receives an offer
    // treats its sender as in the room. Each dialed peer gets its own offer —
    // one peer connection per pair is the mesh — so the dial loop is over the
    // roster as it stood before this join.
    let (view, session_id, existing) = {
        let mut mgr = state.voices.lock().await;
        let session_id = mgr
            .session(&conclave_id)
            .map(|s| s.session.session_id.clone())
            .ok_or_else(|| voice_err(Error::VoiceNoSession.to_string()))?;
        let existing: Vec<String> = mgr
            .session(&conclave_id)
            .map(|s| {
                s.joined
                    .iter()
                    .map(|m| m.account.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        mgr.join(&voice_conclave_descriptor(record), &me, &session_id)
            .map_err(|e| voice_err(e.to_string()))?;
        let live = mgr
            .session(&conclave_id)
            .expect("the join just landed");
        (voice_view_of(live, &me), session_id, existing)
    };
    let plane = voice_plane_for(&state, &session_id).await;
    for peer in existing.iter().filter(|p| *p != &me) {
        // One gathered offer per pair, sent over the room's fan-out. The
        // wire format carries no destination (Phase 3's known seam): with
        // two trustees the fan-out's only recipient *is* the dialed peer.
        let offer = match plane.offer(&peer).await {
            Ok(offer) => offer,
            Err(e) => {
                eprintln!("hush-daemon: voice offer to {peer} failed: {e}");
                continue;
            }
        };
        if let WorkResult::Err(e) = state
            .worker
            .send_voice_offer(&conclave_id, &session_id, &offer)
            .await
        {
            eprintln!("hush-daemon: voice offer fan-out failed: {e}");
        }
    }
    Ok(Json(view))
}

async fn voice_leave(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<VoiceView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let me = paper_account_id(&state).await?;
    // The room as it stood before the leave, so a leave that *ended* the room
    // can still answer with the shape the UI expects — emptied out.
    let (before, session_id) = {
        let mgr = state.voices.lock().await;
        let before = mgr
            .session(&conclave_id)
            .cloned()
            .ok_or_else(|| voice_err(Error::VoiceNoSession.to_string()))?;
        let session_id = before.session.session_id.clone();
        (before, session_id)
    };
    {
        let mut mgr = state.voices.lock().await;
        let effect = mgr
            .leave(&conclave_id, &session_id, &me)
            .map_err(|e| voice_err(e.to_string()))?;
        // Media teardown follows the room's state: the opener's (or the
        // below-two) leave ends the session for everyone — every peer
        // connection in it goes; any other leave only drops this vault's
        // own link. The peer learns the rest from the fan-out below.
        if effect == VoiceEffect::Ended {
            voice_plane_drop(&state, &session_id).await;
        } else {
            let plane = voice_plane_for(&state, &session_id).await;
            plane.forget(&me);
        }
    }
    if let WorkResult::Err(e) = state
        .worker
        .send_voice_leave(&conclave_id, &session_id, &me)
        .await
    {
        eprintln!("hush-daemon: voice leave fan-out failed: {e}");
    }
    let mgr = state.voices.lock().await;
    Ok(Json(match mgr.session(&conclave_id) {
        // The room survived this leave: show it without this vault's member.
        Some(live) => voice_view_of(live, &me),
        // The leave ended the room (opener or below two): the honest answer
        // is the room emptied out, not an error.
        None => VoiceView {
            session_id,
            conclave_id,
            opener: before.session.opener,
            started_ms: before.session.started_ms,
            joined: Vec::new(),
            i_am_in: false,
        },
    }))
}

#[derive(Deserialize)]
struct VoiceMuteRequest {
    muted: bool,
}

async fn voice_mute(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
    Json(body): Json<VoiceMuteRequest>,
) -> Result<Json<VoiceView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let me = paper_account_id(&state).await?;
    let session_id = {
        let mut mgr = state.voices.lock().await;
        let session_id = mgr
            .session(&conclave_id)
            .map(|s| s.session.session_id.clone())
            .ok_or_else(|| voice_err(Error::VoiceNoSession.to_string()))?;
        mgr.mute(&conclave_id, &session_id, &me, body.muted)
            .map_err(|e| voice_err(e.to_string()))?;
        session_id
    };
    // Capture stops before the flag fans out (the spec's `VoiceMute` note):
    // the plane's gate is set first, so the wire never carries audio that
    // the flag says is muted.
    voice_plane_for(&state, &session_id)
        .await
        .set_muted(body.muted);
    if let WorkResult::Err(e) = state
        .worker
        .send_voice_mute(&conclave_id, &session_id, &me, body.muted)
        .await
    {
        eprintln!("hush-daemon: voice mute fan-out failed: {e}");
    }
    let mgr = state.voices.lock().await;
    let view = mgr
        .session(&conclave_id)
        .map(|s| voice_view_of(s, &me))
        .ok_or_else(|| voice_err(Error::VoiceNoSession.to_string()))?;
    Ok(Json(view))
}

async fn voice_get(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<VoiceView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let me = paper_account_id(&state).await?;
    let mgr = state.voices.lock().await;
    mgr.session(&conclave_id)
        .map(|s| voice_view_of(s, &me))
        .map(Json)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: "no voice session is open in this conclave".into(),
                }),
            )
        })
}

#[derive(Serialize)]
struct VoiceStateResponse {
    /// One view per live room, keyed by conclave id.
    rooms: Vec<VoiceView>,
}

async fn voice_state(
    State(state): State<Arc<App>>,
) -> Result<Json<VoiceStateResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let me = paper_account_id(&state).await?;
    let mgr = state.voices.lock().await;
    Ok(Json(VoiceStateResponse {
        rooms: mgr.rooms().iter().map(|s| voice_view_of(s, &me)).collect(),
    }))
}

#[derive(Serialize)]
struct VoiceStatsResponse {
    session_id: String,
    sent_frames: usize,
    decoded_frames: usize,
    played_frames: usize,
    peers_live: usize,
}

/// The media plane's counters for a live room: is audio actually flowing.
/// Read-only and memory-only, like everything else under `/api/voice/*`.
async fn voice_stats(
    State(state): State<Arc<App>>,
    Path(conclave_id): Path<String>,
) -> Result<Json<VoiceStatsResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    paper_account_id(&state).await?;
    let session_id = state
        .voices
        .lock()
        .await
        .session(&conclave_id)
        .map(|s| s.session.session_id.clone())
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: "no voice session is open in this conclave".into(),
                }),
            )
        })?;
    let plane = voice_plane_get(&state, &session_id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "no live media plane for this room".into(),
            }),
        )
    })?;
    let s = plane.stats();
    Ok(Json(VoiceStatsResponse {
        session_id,
        sent_frames: s.sent_frames,
        decoded_frames: s.decoded_frames,
        played_frames: s.played_frames,
        peers_live: s.peers_live,
    }))
}

/// Apply one voice payload received from a peer to the local manager and the
/// media plane.
///
/// Authority was checked in `recv` (the sender is who the payload claims);
/// what remains is the room's own rules, and an offer **is** the sender's join:
/// a peer that offers is announcing itself into the room, so it is applied as
/// one. Two joiners racing at the cap can therefore hold asymmetric views
/// until the next leave — accepted for Phase 2 (two trustees), revisit at the
/// mesh phase where the spec's refusal surface lands.
///
/// Returns `(conclave id, answer payload)` when the payload asks for one: an
/// offer gets an answer built by the media plane — real gathered SDP since
/// Phase 2. An error is logged and dropped by the caller — the messages in
/// the same poll batch have already landed, and one peer's bad payload must
/// not block the mailbox.
async fn voice_apply_inbound(
    state: &Arc<App>,
    mgr: &mut VoiceManager,
    conclave_of: &impl Fn(&str) -> Option<ConclaveRecord>,
    from_account: &str,
    payload: &MessagePayload,
    now: u64,
) -> Option<(String, MessagePayload)> {
    match payload {
        MessagePayload::VoiceOpen { session } => {
            let record = conclave_of(&session.conclave_id)?;
            mgr.open(
                &voice_conclave_descriptor(&record),
                &session.opener,
                session.session_id.clone(),
                now,
            )
            .ok()?;
            // Have the answering side ready: the joiner's offer can arrive
            // in the same batch as the open.
            voice_plane_for(state, &session.session_id).await;
            None
        }
        MessagePayload::VoiceOffer { session_id, sdp } => {
            let conclave_id = mgr.conclave_of(session_id)?.to_string();
            let record = conclave_of(&conclave_id)?;
            // A join already on the roster is fine — the media handshake
            // still needs its answer (a duplicated offer over the fan-out).
            // Any other refusal (not a trustee, ended) must not be answered.
            match mgr.join(&voice_conclave_descriptor(&record), from_account, session_id) {
                Ok(_) | Err(Error::AlreadyConclaveTrustee) => {}
                Err(_) => return None,
            }
            // The real answer: the media plane builds an answerer connection
            // with its own outbound track against the joiner's gathered
            // offer, and the answer carries this side's candidates.
            let plane = voice_plane_for(state, session_id).await;
            let answer = plane.answer(from_account, sdp).await.ok()?;
            Some((
                conclave_id,
                MessagePayload::VoiceAnswer {
                    session_id: session_id.clone(),
                    sdp: answer,
                },
            ))
        }
        MessagePayload::VoiceAnswer { session_id, sdp } => {
            // The answerer is in the room — evidence, not authority. The
            // join it carries is usually already on this vault's roster
            // (the joiner joined locally first), and that must not stop the
            // media half: the answer's real job is completing the handshake.
            let conclave_id = mgr.conclave_of(session_id)?.to_string();
            let record = conclave_of(&conclave_id)?;
            let _ = mgr.join(&voice_conclave_descriptor(&record), from_account, session_id);
            // The PC this vault dialed with gets its remote description.
            let plane = voice_plane_for(state, session_id).await;
            if let Err(e) = plane.accept_answer(from_account, sdp).await {
                eprintln!(
                    "hush-daemon: voice answer from {from_account} did not apply: {e}"
                );
            }
            None
        }
        MessagePayload::VoiceMute {
            session_id, who, muted, ..
        } => {
            let conclave_id = mgr.conclave_of(session_id)?.to_string();
            mgr.mute(&conclave_id, session_id, who, *muted).ok()?;
            None
        }
        MessagePayload::VoiceLeave { session_id, who } => {
            let conclave_id = mgr.conclave_of(session_id)?.to_string();
            let ended = matches!(
                mgr.leave(&conclave_id, session_id, who),
                Ok(VoiceEffect::Ended)
            );
            // The leaver's own link goes either way; a room that ended takes
            // the whole plane with it.
            if ended {
                voice_plane_drop(state, session_id).await;
            } else if let Some(plane) = voice_plane_get(state, session_id).await {
                plane.forget(who);
            }
            None
        }
        _ => None,
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
            voices: Mutex::new(VoiceManager::new()),
        voice_planes: Mutex::new(HashMap::new()),
            neighborhoods_source: Mutex::new(None),
            neighborhoods_sink: Mutex::new(None),
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
            voices: Mutex::new(VoiceManager::new()),
        voice_planes: Mutex::new(HashMap::new()),
            neighborhoods_source: Mutex::new(None),
            neighborhoods_sink: Mutex::new(None),
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
            voices: Mutex::new(VoiceManager::new()),
        voice_planes: Mutex::new(HashMap::new()),
            neighborhoods_source: Mutex::new(None),
            neighborhoods_sink: Mutex::new(None),
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

/// The voice routes (Task 23, Phase 1). The room's own rules are the core's
/// (`voice.rs`, tested there, and the fan-out is tested in `conclaves.rs`);
/// what is pinned here is the route contract: 401 while locked, 404 for a
/// conclave or room this vault does not hold. The happy path needs two
/// vaults, so it is the two-daemon script's to exercise.
#[cfg(test)]
mod voice_api_tests {
    use super::*;
    use crate::notes_api_tests::{build_app, request, unlocked_app};
    use tempfile::TempDir;

    #[tokio::test]
    async fn voice_routes_are_401_while_locked() {
        let dir = TempDir::new().unwrap();
        let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
        for (method, path, body) in [
            ("GET", "/api/voice/c-1", None),
            ("GET", "/api/voice/state", None),
            ("GET", "/api/voice/c-1/stats", None),
            ("POST", "/api/voice/c-1/open", None),
            ("POST", "/api/voice/c-1/join", None),
            ("POST", "/api/voice/c-1/leave", None),
            ("POST", "/api/voice/c-1/mute", Some(r#"{"muted":true}"#)),
        ] {
            let (status, _) = request(&router, method, path, body).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} must not even learn a room exists"
            );
        }
    }

    #[tokio::test]
    async fn voice_routes_refuse_a_conclave_this_vault_does_not_have() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);

        // Open needs a real conclave: the room's authority is its roster, so
        // there is nothing to open a room in.
        let (status, err) = request(&router, "POST", "/api/voice/c-1/open", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
        assert!(err["error"].as_str().unwrap().contains("no conclave"));

        // Stats is a read over the same rooms: nothing open, nothing to count.
        let (status, err) = request(&router, "GET", "/api/voice/c-1/stats", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
        assert!(err["error"].as_str().unwrap().contains("no voice session"));

        // Join, leave and mute name a room that cannot exist either.
        for path in ["/api/voice/c-1/join", "/api/voice/c-1/leave"] {
            let (status, _) = request(&router, "POST", path, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
        let (status, _) = request(
            &router,
            "POST",
            "/api/voice/c-1/mute",
            Some(r#"{"muted":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // And the state read says exactly what is there: no room.
        let (status, state) = request(&router, "GET", "/api/voice/state", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(state["rooms"].as_array().unwrap().len(), 0);
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
async fn neighborhoods_identity_routes_are_401_while_locked() {
    let dir = TempDir::new().unwrap();
    let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
    for (method, uri, body) in [
        ("GET", "/api/neighborhoods/identity", None),
        ("POST", "/api/neighborhoods/identity", Some("{}")),
    ] {
        let (status, _) = request(&router, method, uri, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_public_identity_is_minted_once_then_read_back() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);

    // No identity before one is minted: a read does not create one.
    let (status, view) = request(&router, "GET", "/api/neighborhoods/identity", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view.is_null(), "a fresh vault has no public identity");

    // An explicit create mints it.
    let (status, created) =
        request(&router, "POST", "/api/neighborhoods/identity", Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let npub = created["npub"].as_str().unwrap().to_string();
    let pubkey = created["pubkey"].as_str().unwrap().to_string();
    assert!(npub.starts_with("npub1"), "{npub}");
    assert_eq!(pubkey.len(), 64);

    // A second create returns the same identity rather than minting a new one,
    // so a retried request does not publish under a new key.
    let (status, again) =
        request(&router, "POST", "/api/neighborhoods/identity", Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pubkey"], pubkey);

    // And the read now returns it.
    let (status, view) = request(&router, "GET", "/api/neighborhoods/identity", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["pubkey"], pubkey);
}

// --- Neighborhoods: the read lane (Task 22) -------------------------------

/// A relay that returns a fixed batch per kind, so the lane is exercised without
/// a network. The transport is what this stands in for; the gate, dedupe, cache,
/// index, and routes are real.
pub(super) struct FakeRelay {
    events: Vec<hush_articles::RawEvent>,
    /// What the reply pass is served. The lane asks with a different filter, so
    /// one stub answers both halves of a refresh.
    comments: Vec<hush_articles::RawEvent>,
}

impl FakeRelay {
    /// A relay with articles and no comments: what a refresh reads before
    /// anything has been exported.
    pub(super) fn articles(events: Vec<hush_articles::RawEvent>) -> Arc<Self> {
        Self::new(events, Vec::new())
    }

    /// A relay that answers both halves of a refresh.
    pub(super) fn new(
        events: Vec<hush_articles::RawEvent>,
        comments: Vec<hush_articles::RawEvent>,
    ) -> Arc<Self> {
        Arc::new(Self { events, comments })
    }
}

#[async_trait::async_trait]
impl hush_articles::RelaySource for FakeRelay {
    async fn fetch(
        &self,
        _relays: &[String],
        filter: &hush_articles::Filter,
    ) -> Result<Vec<hush_articles::RawEvent>, String> {
        if filter.kinds == vec![hush_articles::KIND_LONG_FORM] {
            Ok(self.events.clone())
        } else {
            Ok(self.comments.clone())
        }
    }
}

pub(super) fn long_form_event(
    pubkey: &str,
    d: &str,
    created_at: u64,
    body: &str,
) -> hush_articles::RawEvent {
    serde_json::from_value(serde_json::json!({
        "id": format!("{pubkey}-{d}-{created_at}"),
        "pubkey": pubkey,
        "created_at": created_at,
        "kind": hush_articles::KIND_LONG_FORM,
        "tags": [["d", d], ["title", "A title"], ["t", "Rust"]],
        "content": body,
    }))
    .unwrap()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn neighborhoods_read_routes_are_401_while_locked() {
    let dir = TempDir::new().unwrap();
    let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
    for (method, uri, body) in [
        ("GET", "/api/neighborhoods", None),
        ("GET", "/api/neighborhoods/settings", None),
        ("POST", "/api/neighborhoods/settings", Some("{}")),
        ("POST", "/api/neighborhoods/refresh", None),
        ("GET", "/api/neighborhoods/30023:aa:kept", None),
    ] {
        let (status, _) = request(&router, method, uri, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn the_settings_start_curated_and_a_patch_touches_only_what_it_names() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);

    let (status, view) = request(&router, "GET", "/api/neighborhoods/settings", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["min_minutes"], 3);
    assert_eq!(view["default_show"], false, "public posts are hidden until opted in");
    assert!(view["allowlist"].as_array().unwrap().len() >= 3);

    let (status, view) = request(
        &router,
        "POST",
        "/api/neighborhoods/settings",
        Some(r#"{"min_minutes": 5, "default_show": true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["min_minutes"], 5);
    assert_eq!(view["default_show"], true);
    assert!(
        view["allowlist"].as_array().unwrap().len() >= 3,
        "an unmentioned field is untouched"
    );

    // And it survives a fresh read of the sidecar.
    let (status, view) = request(&router, "GET", "/api/neighborhoods/settings", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["min_minutes"], 5);
    assert_eq!(view["default_show"], true);
}

#[tokio::test]
async fn an_allowlist_entry_that_is_not_a_relay_is_refused() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    let (status, view) = request(
        &router,
        "POST",
        "/api/neighborhoods/settings",
        Some(r#"{"allowlist": ["https://relay.example"]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{view}");
}

#[tokio::test]
async fn a_refresh_without_tor_is_refused_not_silent() {
    let dir = TempDir::new().unwrap();
    let router = api_router(unlocked_app(dir.path()).await);
    let (status, view) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "with no Tor there is no route, and the lane says so rather than reading in the clear: {view}"
    );
}

/// The daemon builds the real transport when the shell has reported a Tor SOCKS
/// port. With the proxy dead, every relay fails and the refresh is a 502 naming
/// them — not a silent success, and not a direct connection.
#[tokio::test]
async fn a_refresh_with_tor_reported_but_unreachable_is_a_gateway_error() {
    let dir = TempDir::new().unwrap();
    let app = unlocked_app(dir.path()).await;
    // A port that was bound and released: nothing is listening on it.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    };
    *app.relay_socks.lock().await = Some(dead.to_string());
    let router = api_router(app);

    let (status, view) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "the real Tor transport is built and every relay fails: {view}"
    );
    assert!(
        view["error"].as_str().unwrap().contains("relay"),
        "the failure names the relays: {view}"
    );
}

#[tokio::test]
async fn a_refresh_gates_caches_and_serves_the_index() {
    let dir = TempDir::new().unwrap();
    let app = unlocked_app(dir.path()).await;
    *app.neighborhoods_source.lock().await = Some(FakeRelay::articles(vec![
        long_form_event("aa", "kept", 20, &"word ".repeat(800)),
        long_form_event("aa", "stub", 20, "too short"),
        hush_articles::RawEvent {
            kind: 1,
            ..long_form_event("bb", "note", 20, &"word ".repeat(800))
        },
    ]));
    let router = api_router(app);

    let (status, view) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        view["articles"].as_array().unwrap().len(),
        1,
        "the stub and the note are refused: {view}"
    );
    assert_eq!(view["articles"][0]["address"], "30023:aa:kept");
    assert_eq!(view["articles"][0]["reading_minutes"], 4);
    assert!(
        view["articles"][0].get("content").is_none(),
        "the index carries no body"
    );
    assert_eq!(view["sections"][0]["name"], "rust");
    assert_eq!(view["sections"][0]["count"], 1);

    // The index reads back from the sidecar and filters by section.
    let (status, view) = request(&router, "GET", "/api/neighborhoods?section=rust", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["articles"].as_array().unwrap().len(), 1);
    let (status, view) = request(&router, "GET", "/api/neighborhoods?section=nope", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["articles"].as_array().unwrap().is_empty());

    // The article page carries the body; an address not held is a 404.
    let (status, view) = request(&router, "GET", "/api/neighborhoods/30023:aa:kept", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["content"].as_str().unwrap().starts_with("word"));
    let (status, _) = request(&router, "GET", "/api/neighborhoods/30023:aa:missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_neighborhoods_sidecar_is_ciphertext_at_rest() {
    let dir = TempDir::new().unwrap();
    let app = unlocked_app(dir.path()).await;
    const TITLE: &str = "atrest-title-marker";
    let mut event = long_form_event("aa", "kept", 20, &"word ".repeat(800));
    event.tags[1][1] = TITLE.to_string();
    *app.neighborhoods_source.lock().await = Some(FakeRelay::articles(vec![event]));
    let router = api_router(app);

    let (status, _) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
    assert_eq!(status, StatusCode::OK);

    let sidecar = std::fs::read(dir.path().join(hush_articles::STATE_FILE))
        .expect("neighborhoods.enc exists after a refresh");
    assert!(!sidecar.is_empty(), "sidecar is not empty");
    assert!(
        !contains(&sidecar, TITLE.as_bytes()),
        "the cached title is not in the clear on disk"
    );
    assert!(
        !contains(&sidecar, b"word word word"),
        "the cached body is not in the clear on disk"
    );
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
    let pen_id = pen["pen_name"]["id"].as_str().unwrap().to_string();

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
    assert_eq!(row["authored"], true);
    assert_eq!(row["preview"], "hello world");
    // A create mints the post's id, and the answer carries it so the caller can
    // open what it just published.
    let id = created["zine_id"].as_str().unwrap().to_string();
    assert_eq!(row["zine_id"], id, "the answer names the post it wrote");
    assert_eq!(created["revision"], 1);
    assert_ne!(
        id, pen_id,
        "the id addresses the post, not the pen name: that is what lets one \
         pen name hold many posts"
    );
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

    // A second post under the same pen name: the first is not replaced.
    let (status, second) = request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"title":"On gossip","body":"<p>another one</p>"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_id = second["zine_id"].as_str().unwrap().to_string();
    assert_ne!(second_id, id, "a second post gets its own id");
    assert_eq!(
        second["zines"].as_array().unwrap().len(),
        2,
        "the pen name now holds two posts"
    );
    assert_eq!(
        second["zines"][0]["zine_id"], second_id,
        "the newest is first, and both are this vault's"
    );

    // Edit: a new revision of the first post, chained to the one before it.
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
    assert_eq!(edited["zine_id"], id, "the same post was revised");
    assert_eq!(edited["revision"], 2);
    let edited_first = edited["zines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|z| z["zine_id"] == id.as_str())
        .unwrap();
    assert_eq!(edited_first["title"], "On relays, again");
    let untouched = edited["zines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|z| z["zine_id"] == second_id.as_str())
        .unwrap();
    assert_eq!(untouched["revision"], 1, "the other post did not move");
    assert_eq!(untouched["title"], "On gossip");

    // The chain and the history are visible on the article.
    let (status, one) = request(&router, "GET", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["zine"]["descriptor"]["revision"], 2);
    assert_eq!(
        one["zine"]["descriptor"]["pen_id"], pen_id,
        "the article carries the identity the post was written under"
    );
    assert_eq!(
        one["zine"]["descriptor"]["prev_hash"].as_str().unwrap().len(),
        64,
        "revision 2 points back at revision 1's content hash"
    );
    assert_eq!(one["zine"]["history"][0]["revision"], 1);
    assert_eq!(one["zine"]["history"][0]["title"], "On relays");
    assert_eq!(one["zine"]["authored"], true);

    // A revision naming a post this vault does not hold is refused rather than
    // treated as a create, so a stale UI cannot resurrect a forgotten post.
    let (status, err) = request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"zine_id":"nope","title":"Ghost","body":"<p>x</p>"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(err["error"].as_str().unwrap().contains("no post"), "{err}");
    let (_, after) = request(&router, "GET", "/api/zines", None).await;
    assert_eq!(
        after["zines"].as_array().unwrap().len(),
        2,
        "the refused revision stored nothing"
    );

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
    assert_eq!(by_pen["zines"].as_array().unwrap().len(), 2);

    // Mute, then unmute. The row is found by id: the index is newest first, and
    // which post that is depends on the corpus rather than on this test.
    let (status, muted) = request(&router, "POST", &format!("/api/zines/{id}/mute"), Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{muted}");
    let muted_row = muted["zines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|z| z["zine_id"] == id.as_str())
        .unwrap();
    assert_eq!(muted_row["muted"], true);
    let (_, unmuted) = request(
        &router,
        "POST",
        &format!("/api/zines/{id}/mute"),
        Some(r#"{"muted":false}"#),
    )
    .await;
    let unmuted_row = unmuted["zines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|z| z["zine_id"] == id.as_str())
        .unwrap();
    assert_eq!(unmuted_row["muted"], false);

    // Forget. The other post is untouched: forgetting is per post.
    let (status, gone) = request(&router, "DELETE", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{gone}");
    assert_eq!(gone["zines"].as_array().unwrap().len(), 1);
    assert_eq!(gone["zines"][0]["zine_id"], second_id);
    let (status, _) = request(&router, "GET", &format!("/api/zines/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(&router, "GET", &format!("/api/zines/{second_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "the other post is still held");
}

#[tokio::test]
async fn a_draft_naming_a_post_this_vault_does_not_hold_is_refused() {
    // The pen name comes from the vault, never from the request, so a caller
    // does not choose which byline it publishes under. A draft naming an id
    // this vault does not hold is a stale UI — and treating it as a create
    // would let that stale UI write a second post under an id it made up.
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

    let (status, err) = request(
        &router,
        "POST",
        "/api/zines",
        Some(&format!(
            r#"{{"zine_id":"{theirs}","title":"Not mine","body":"<p>x</p>"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(err["error"].as_str().unwrap().contains("no post"), "{err}");
    let (_, view) = request(&router, "GET", "/api/zines", None).await;
    assert_eq!(
        view["zines"].as_array().unwrap().len(),
        0,
        "nothing was written under any id"
    );
    let (status, _) = request(&router, "GET", &format!("/api/zines/{theirs}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // And the pen name's own id is not a post id either, so it is refused the
    // same way: the identity is not an address.
    let (status, err) = request(
        &router,
        "POST",
        "/api/zines",
        Some(&format!(
            r#"{{"zine_id":"{mine}","title":"Not mine either","body":"<p>x</p>"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(err["error"].as_str().unwrap().contains("no post"), "{err}");
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
    assert_eq!(pen["pen_name"]["name"], "ada");
    let (_, created) = request(
        &router,
        "POST",
        "/api/zines",
        Some(r#"{"title":"Persisted","sections":["rust"],"body":"<p>still here</p>"}"#),
    )
    .await;
    let id = created["zine_id"].as_str().unwrap().to_string();

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
    assert_eq!(
        view["zines"][0]["zine_id"], id,
        "the minted post id survives a restart, so it is the id Phase 2 can \
         address a blob by"
    );
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

/// Zines over the wire, with two daemons and a real relay.
///
/// The bulletin lane is the one part of zines that exists only on the wire, so
/// a stub would test nothing: what has to hold is that two vaults converge
/// through the relay, that a blob nobody can open yet is held rather than
/// dropped, and that the relay's own database holds no plaintext. The contact
/// dance is real too (invite and accept, both ways) because a grant rides a 1:1
/// session and a session needs the recipient's queue, which is in their invite.
#[cfg(test)]
mod zines_bulletin_tests {
    use super::*;
    use crate::notes_api_tests::{build_app, request};
    use hush_relay::RelayConfig;
    use tempfile::{NamedTempFile, TempDir};

    /// A real relay, on a real port, with its own database on disk.
    async fn relay_on_a_port() -> (String, NamedTempFile) {
        let db = NamedTempFile::new().unwrap();
        let app =
            hush_relay::router(db.path().to_str().unwrap(), RelayConfig::default()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), db)
    }

    /// One member: a vault, its worker, and the routes over them.
    struct Member {
        /// Held only so the vault directory outlives the test.
        _dir: TempDir,
        app: Arc<App>,
    }

    impl Member {
        async fn new(relay: &str) -> Self {
            let dir = TempDir::new().unwrap();
            hush_core::Account::create(dir.path(), "pw", relay)
                .await
                .unwrap();
            let worker = AccountWorker::spawn();
            assert!(matches!(
                worker.unlock(&dir.path().to_path_buf(), "pw").await,
                WorkResult::Ok
            ));
            let app = build_app(dir.path(), worker, true);
            Self { _dir: dir, app }
        }

        async fn post(&self, uri: &str, body: Option<&str>) -> (StatusCode, serde_json::Value) {
            request(&api_router(self.app.clone()), "POST", uri, body).await
        }

        async fn get(&self, uri: &str) -> (StatusCode, serde_json::Value) {
            request(&api_router(self.app.clone()), "GET", uri, None).await
        }

        /// Poll, the way the UI does: the bulletin drain rides on it.
        async fn poll(&self) -> serde_json::Value {
            let (status, view) = self.post("/api/recv", None).await;
            assert_eq!(status, StatusCode::OK, "recv failed: {view}");
            view
        }
    }

    /// Make two members contacts of each other.
    ///
    /// Both directions, because a session needs the *recipient's* queue and the
    /// only place it travels is their invite.
    async fn connect(a: &Member, b: &Member, a_nick: &str, b_nick: &str) {
        for (me, other, nick) in [(a, b, b_nick), (b, a, a_nick)] {
            let (status, invite) = other.post("/api/invite", None).await;
            assert_eq!(status, StatusCode::OK, "{invite}");
            let body = serde_json::json!({
                "nickname": nick,
                "invite": invite["invite"].as_str().unwrap(),
            })
            .to_string();
            let (status, err) = me.post("/api/contacts/add", Some(&body)).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{err}");
        }
    }

    /// Two members on one relay, each holding the other's invite.
    async fn pair() -> (String, NamedTempFile, Member, Member) {
        let (relay, db) = relay_on_a_port().await;
        let a = Member::new(&relay).await;
        let b = Member::new(&relay).await;
        connect(&a, &b, "ada", "bo").await;
        (relay, db, a, b)
    }

    /// A raw byte scan, so the claim covers the whole file: a marker in a freed
    /// page or a WAL frame is still plaintext on the relay's disk.
    fn holds(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[tokio::test]
    async fn a_published_zine_reaches_a_reader_unreadable_and_the_relay_holds_no_plaintext() {
        let (_, relay_db, a, b) = pair().await;

        a.post("/api/zines/pen-name", Some(r#"{"name":"BYLINE-MARKER"}"#))
            .await;
        let (status, created) = a
            .post(
                "/api/zines",
                Some(
                    r#"{"title":"TITLE-MARKER","sections":["SECTION-MARKER"],"body":"<p>BODY-MARKER</p>"}"#,
                ),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["published"], true, "the relay took the blob: {created}");
        assert_eq!(created["publish_error"], "", "{created}");

        // The reader has no key, so the blob is held and counted rather than
        // dropped or rendered as an empty post.
        b.poll().await;
        let (_, index) = b.get("/api/zines").await;
        assert_eq!(
            index["zines"].as_array().unwrap().len(),
            0,
            "an unopenable blob is not a post yet: {index}"
        );
        assert_eq!(index["held_blobs"], 1, "{index}");
        assert_eq!(index["unreadable_blobs"], 1, "{index}");

        // Nothing readable reached the relay, anywhere in its database file.
        let bytes = std::fs::read(relay_db.path()).unwrap();
        for marker in [
            "BYLINE-MARKER",
            "TITLE-MARKER",
            "SECTION-MARKER",
            "BODY-MARKER",
        ] {
            assert!(
                !holds(&bytes, marker.as_bytes()),
                "{marker} is on the relay's disk"
            );
        }

        // A second poll does not hold a second copy, and the cursor did not
        // rewind: the lane is read from where it stopped.
        b.poll().await;
        let (_, again) = b.get("/api/zines").await;
        assert_eq!(again["held_blobs"], 1, "the blob is not duplicated");
    }

    #[tokio::test]
    async fn a_grant_makes_a_held_blob_readable_and_a_comment_comes_back_merged() {
        // The relay's database is bound to a name, not to `_`: dropping the
        // `NamedTempFile` deletes the file, and SQLite would then be writing to
        // an unlinked inode.
        let (_relay, _relay_db, a, b) = pair().await;

        a.post("/api/zines/pen-name", Some(r#"{"name":"ada"}"#)).await;
        let (_, created) = a
            .post(
                "/api/zines",
                Some(r#"{"title":"On relays","sections":["rust"],"body":"<p>hello</p>"}"#),
            )
            .await;
        let id = created["zine_id"].as_str().unwrap().to_string();

        // The reader drains before the key arrives.
        b.poll().await;
        assert_eq!(b.get("/api/zines").await.1["unreadable_blobs"], 1);

        // The author hands the key over their session.
        let (status, granted) = a
            .post(&format!("/api/zines/{id}/grant"), Some(r#"{"contact":"bo"}"#))
            .await;
        assert_eq!(status, StatusCode::OK, "{granted}");

        // The next poll takes the key and re-tries the blob already held, which
        // is the second half of the epidemic.
        b.poll().await;
        let (status, index) = b.get("/api/zines").await;
        assert_eq!(status, StatusCode::OK, "{index}");
        assert_eq!(index["zines"].as_array().unwrap().len(), 1, "{index}");
        assert_eq!(index["zines"][0]["title"], "On relays");
        assert_eq!(index["zines"][0]["unreadable"], false);
        assert_eq!(index["unreadable_blobs"], 0, "the held blob is readable now");
        assert_eq!(index["zines"][0]["sections"][0], "rust");

        // The reader comments: sealed to the author's pen-name key, published to
        // the lane, and kept by the commenter as a holder.
        let (status, comment) = b
            .post(
                &format!("/api/zines/{id}/comment"),
                Some(r#"{"body":"<p>nice one</p>"}"#),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{comment}");
        assert_eq!(comment["published"], true, "{comment}");

        // The author pulls it. It waits: a comment is merged by the author, not
        // by arriving.
        a.poll().await;
        let (_, one) = a.get(&format!("/api/zines/{id}")).await;
        let pending = one["zine"]["pending_comments"].as_array().unwrap();
        assert_eq!(pending.len(), 1, "{one}");
        assert_eq!(pending[0]["body"], "<p>nice one</p>");
        assert_eq!(
            one["zine"]["descriptor"]["comments"].as_array().unwrap().len(),
            0,
            "pulled is not merged: {one}"
        );
        let hash = pending[0]["hash"].as_str().unwrap().to_string();

        // Accepting it is a revision — the thread lives in the descriptor, so a
        // merge that kept its revision number would be refused by half its
        // readers — and the republish is what makes it visible.
        let body = serde_json::json!({ "comment_hash": hash }).to_string();
        let (status, accepted) = a
            .post(&format!("/api/zines/{id}/accept-comment"), Some(&body))
            .await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        assert_eq!(accepted["revision"], 2, "a merge opens a revision");
        assert_eq!(accepted["published"], true, "{accepted}");
        assert!(
            accepted["zines"][0]["pending"] == 0,
            "the pile is empty once merged: {accepted}"
        );

        // The reader drains the republished revision and sees the thread.
        b.poll().await;
        let (_, seen) = b.get(&format!("/api/zines/{id}")).await;
        assert_eq!(seen["zine"]["descriptor"]["revision"], 2, "{seen}");
        let thread = seen["zine"]["descriptor"]["comments"].as_array().unwrap();
        assert_eq!(thread.len(), 1, "{seen}");
        assert_eq!(thread[0]["body"], "<p>nice one</p>");
        assert_eq!(thread[0]["author"], "", "the commenter had no pen name");

        // A refusal is the author's other option, and it is not a retraction:
        // the comment was never published.
        let (status, refused) = b
            .post(
                &format!("/api/zines/{id}/comment"),
                Some(r#"{"body":"<p>spam</p>"}"#),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{refused}");
        a.poll().await;
        let (_, one) = a.get(&format!("/api/zines/{id}")).await;
        let spam = one["zine"]["pending_comments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"] == "<p>spam</p>")
            .expect("the second comment arrived")
            .clone();
        let body = serde_json::json!({ "comment_hash": spam["hash"] }).to_string();
        let (status, after) = a
            .post(&format!("/api/zines/{id}/discard-comment"), Some(&body))
            .await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert_eq!(
            after["zine"]["pending_comments"].as_array().unwrap().len(),
            0,
            "the refusal took it out of the pile"
        );
        assert_eq!(
            after["zine"]["descriptor"]["comments"].as_array().unwrap().len(),
            1,
            "and it is not in the thread"
        );
        assert_eq!(after["zine"]["descriptor"]["revision"], 2, "a refusal is not an edit");
    }

    #[tokio::test]
    async fn a_holder_re_shares_a_key_it_was_given() {
        // The epidemic's point: C has no session with A, and reads the post
        // because B — a reader, not the author — passed the key on. All three are
        // on one relay, because the bulletin is per-relay: reach across relays is
        // the re-post sweep, not a relay-to-relay path.
        let (relay, _relay_db, a, b) = pair().await;
        let c = Member::new(&relay).await;
        connect(&b, &c, "bo", "cy").await;

        a.post("/api/zines/pen-name", Some(r#"{"name":"ada"}"#)).await;
        let (_, created) = a
            .post(
                "/api/zines",
                Some(r#"{"title":"On gossip","body":"<p>spread it</p>"}"#),
            )
            .await;
        let id = created["zine_id"].as_str().unwrap().to_string();

        // B reads it the ordinary way: drain, grant, drain.
        b.poll().await;
        a.post(&format!("/api/zines/{id}/grant"), Some(r#"{"contact":"bo"}"#))
            .await;
        b.poll().await;
        assert_eq!(b.get("/api/zines").await.1["zines"][0]["title"], "On gossip");

        // B re-shares to C, who has never spoken to A.
        let (status, shared) = b
            .post(&format!("/api/zines/{id}/grant"), Some(r#"{"contact":"cy"}"#))
            .await;
        assert_eq!(status, StatusCode::OK, "{shared}");

        // C drains the same lane and opens the blob with the key it was handed.
        // The key alone would be useless without the blob, and the blob alone
        // useless without the key: both arrive on C's own schedule.
        c.poll().await;
        let (_, index) = c.get("/api/zines").await;
        assert_eq!(index["zines"].as_array().unwrap().len(), 1, "{index}");
        assert_eq!(index["zines"][0]["title"], "On gossip");
        assert_eq!(index["zines"][0]["authored"], false, "C is a reader, not the author");

        // A post this vault holds no key for cannot be passed on: the refusal is
        // about having the key, not about being entitled to share it.
        let (status, refused) = c
            .post("/api/zines/no-such-post/grant", Some(r#"{"contact":"bo"}"#))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert!(
            refused["error"].as_str().unwrap().contains("read key"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn a_zine_route_that_needs_the_wire_fails_with_a_reason_rather_than_a_lost_write() {
        // No relay reachable: the write still lands locally, and the caller is
        // told the publish did not. The bytes are held, so the re-post sweep is
        // what sends them later.
        let dir = TempDir::new().unwrap();
        let app = crate::notes_api_tests::unlocked_app(dir.path()).await;
        let router = api_router(app);
        request(
            &router,
            "POST",
            "/api/zines/pen-name",
            Some(r#"{"name":"ada"}"#),
        )
        .await;
        let (status, created) = request(
            &router,
            "POST",
            "/api/zines",
            Some(r#"{"title":"Offline","body":"<p>held</p>"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["published"], false, "the relay is unreachable");
        assert!(
            !created["publish_error"].as_str().unwrap().is_empty(),
            "and the reason is given: {created}"
        );
        assert_eq!(
            created["zines"][0]["title"], "Offline",
            "the article is stored either way"
        );
        assert_eq!(created["zines"][0]["revision"], 1);
    }
}

/// The export (Task 11): a private zine re-encoded as a public NIP-23 article.
///
/// The transport is a stub, because what has to hold is not that a relay was
/// reached but that the *event* is one a relay would accept: the right kind, the
/// tags the gate reads, the body as Markdown rather than stored HTML, no private
/// state anywhere in it, and a signature that verifies under the pubkey the
/// event names. A fake sink is what lets all of that be asserted.
#[cfg(test)]
mod zine_export_api_tests {
    use super::*;
    use crate::notes_api_tests::{request, unlocked_app};
    use hush_articles::{Event, PublishReport, RelaySink};
    use tempfile::TempDir;

    /// A sink that keeps what it was handed and answers a fixed verdict.
    ///
    /// Shared with the comment tests: a comment is published the same way an
    /// export is, so the stub that stands in for the network is the same one.
    pub(super) struct FakeSink {
        /// The event ids it was asked to publish, in order.
        seen: std::sync::Mutex<Vec<Event>>,
        /// What to answer with. `Err` is a total failure, as a network with no
        /// reachable relay is.
        verdict: Result<(), String>,
    }

    impl FakeSink {
        pub(super) fn accepting() -> Arc<Self> {
            Arc::new(Self {
                seen: std::sync::Mutex::new(Vec::new()),
                verdict: Ok(()),
            })
        }

        pub(super) fn refusing(reason: &str) -> Arc<Self> {
            Arc::new(Self {
                seen: std::sync::Mutex::new(Vec::new()),
                verdict: Err(reason.to_string()),
            })
        }

        pub(super) fn events(&self) -> Vec<Event> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl RelaySink for FakeSink {
        async fn publish(
            &self,
            relays: &[String],
            event: &Event,
        ) -> Result<PublishReport, String> {
            self.seen.lock().unwrap().push(event.clone());
            match &self.verdict {
                Ok(()) => Ok(PublishReport {
                    accepted: relays.to_vec(),
                    refused: Vec::new(),
                }),
                Err(reason) => Err(reason.clone()),
            }
        }
    }

    /// A vault with a byline, a post, a public identity, an allowlist and a
    /// stubbed sink: the whole state an export needs, in one place.
    ///
    /// The `TempDir` is returned rather than dropped: the worker holds the vault
    /// path, so deleting the directory while the app is still in use would pull
    /// the vault out from under it.
    async fn exportable(
        sink: Arc<FakeSink>,
    ) -> (TempDir, Arc<App>, axum::Router, String, String) {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        *app.neighborhoods_sink.lock().await = Some(sink);
        let router = api_router(app.clone());

        let (status, pen) = request(
            &router,
            "POST",
            "/api/zines/pen-name",
            Some(r#"{"name":"ada"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{pen}");

        let (status, created) = request(
            &router,
            "POST",
            "/api/zines",
            Some(r#"{"title":"On relays","sections":["Rust","Nostr"],"body":"<p>hello <strong>world</strong></p><p>Second.</p>"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let zine_id = created["zine_id"].as_str().unwrap().to_string();

        let (status, identity) = request(
            &router,
            "POST",
            "/api/neighborhoods/identity",
            Some("{}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{identity}");
        let pubkey = identity["pubkey"].as_str().unwrap().to_string();

        // One relay, so the event's relay hints and the publish target are both
        // non-empty and known.
        let (status, _) = request(
            &router,
            "POST",
            "/api/neighborhoods/settings",
            Some(r#"{"allowlist":["wss://relay.example"],"min_minutes":1,"default_show":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        (dir, app, router, zine_id, pubkey)
    }

    pub(super) fn verify(event: &Event) {
        use k256::schnorr::signature::hazmat::PrehashVerifier;
        let xonly: [u8; 32] = hex::decode(&event.pubkey).unwrap().try_into().unwrap();
        let verifying = k256::schnorr::VerifyingKey::from_bytes(&xonly).unwrap();
        let signature = k256::schnorr::Signature::try_from(
            hex::decode(&event.sig).unwrap().as_slice(),
        )
        .unwrap();
        let id: [u8; 32] = hex::decode(&event.id).unwrap().try_into().unwrap();
        verifying
            .verify_prehash(&id, &signature)
            .expect("a relay checks exactly this, against exactly this key");
    }

    /// The article that goes out is the one the gate and every other NIP-23
    /// client will read: the kind, the `d` and `title` the gate requires, the
    /// sections as `t`, a derived summary, the body as Markdown, and a signature
    /// over the id under the pubkey the event names.
    #[tokio::test]
    async fn an_export_publishes_a_signed_nip23_article() {
        let sink = FakeSink::accepting();
        let (_dir, _app, router, zine_id, pubkey) = exportable(sink.clone()).await;

        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["published"], true, "{out}");
        assert_eq!(out["publish_error"], "", "{out}");
        assert_eq!(out["pubkey"], pubkey);
        assert_eq!(out["d"].as_str().unwrap().len(), 32, "a 16-byte mint in hex");
        assert_eq!(
            out["address"],
            format!("30023:{pubkey}:{}", out["d"].as_str().unwrap())
        );
        assert!(out["naddr"].as_str().unwrap().starts_with("naddr1"), "{out}");
        assert_eq!(out["event_id"].as_str().unwrap().len(), 64);
        assert!(out["zine"]["export"].is_object(), "the address is recorded");

        let events = sink.events();
        assert_eq!(events.len(), 1, "one event, to the one relay");
        let event = &events[0];
        assert_eq!(event.kind, 30023);
        assert_eq!(event.pubkey, pubkey);
        assert_eq!(event.id, out["event_id"].as_str().unwrap());
        verify(event);

        let tag = |name: &str| -> Option<String> {
            event
                .tags
                .iter()
                .find(|t| t[0] == name)
                .map(|t| t[1].clone())
        };
        assert_eq!(tag("d"), Some(out["d"].as_str().unwrap().to_string()));
        assert_eq!(tag("title"), Some("On relays".to_string()));
        assert_eq!(tag("published_at"), Some(out["published_at"].to_string()));
        let sections: Vec<String> = event
            .tags
            .iter()
            .filter(|t| t[0] == "t")
            .map(|t| t[1].clone())
            .collect();
        assert_eq!(sections, vec!["rust", "nostr"], "sections travel as `t`");
        assert!(
            tag("summary").unwrap().starts_with("hello world"),
            "a card preview is derived from the body: {event:?}"
        );

        // The body is Markdown, not the stored HTML: a public client would show
        // the tags as text.
        assert!(event.content.contains("hello **world**"), "{}", event.content);
        assert!(!event.content.contains("<p>"), "{}", event.content);
        assert!(event.content.contains("Second."), "{}", event.content);
        // And the address the app recorded is the one it published.
        let (_, one) = request(&router, "GET", &format!("/api/zines/{zine_id}"), None).await;
        assert_eq!(one["zine"]["export"]["d"], out["d"]);
        assert_eq!(one["zine"]["export"]["last_event_id"], out["event_id"]);
        assert_eq!(one["zine"]["export"]["published_at"], out["published_at"]);
    }

    /// The one-way door, asserted on the bytes that leave: a comment thread, a
    /// revision history, the read key and the byline have nowhere to go. The
    /// `d` is the interesting one — it is the only thing that could tie the
    /// private post to the public article, and it must not be the zine id.
    #[tokio::test]
    async fn nothing_private_reaches_the_article() {
        let sink = FakeSink::accepting();
        let (_dir, _app, router, zine_id, _) = exportable(sink.clone()).await;

        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        let (_, one) = request(&router, "GET", &format!("/api/zines/{zine_id}"), None).await;
        let read_key = one["zine"]["read_key"].as_str().unwrap().to_string();

        let event = &sink.events()[0];
        let haystack = format!("{}{}", event.content, serde_json::to_string(&event.tags).unwrap());
        for forbidden in [
            "read_key",
            "pen_name",
            "pen_id",
            "zine_id",
            "prev_hash",
            "comments",
            "signature",
            &read_key,
            &zine_id,
        ] {
            assert!(
                !haystack.contains(forbidden),
                "`{forbidden}` reached the article: {haystack}"
            );
        }
        assert_ne!(
            out["d"].as_str().unwrap(),
            zine_id,
            "the address is not the post id, or the link would be public"
        );
        assert!(
            !event.tags.iter().any(|t| t[0] == "a" || t[0] == "e"),
            "no reference back to anything private: {:?}",
            event.tags
        );
    }

    /// A re-export is an edit at the same address. The `d` and `published_at`
    /// must not move — a new `d` would publish a second article beside the
    /// first, and a moved `published_at` would claim the article was written
    /// later than it was — while the content and the id do.
    #[tokio::test]
    async fn a_second_export_reuses_the_address_and_moves_only_the_content() {
        let sink = FakeSink::accepting();
        let (_dir, _app, router, zine_id, _) = exportable(sink.clone()).await;

        let (status, first) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");

        let (status, edited) = request(
            &router,
            "POST",
            "/api/zines",
            Some(&format!(
                r#"{{"zine_id":"{zine_id}","title":"On relays, again","sections":["rust"],"body":"<p>rewritten</p>"}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["revision"], 2);

        let (status, second) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{second}");

        assert_eq!(second["d"], first["d"], "the address is the article's");
        assert_eq!(second["address"], first["address"]);
        assert_eq!(second["published_at"], first["published_at"]);
        assert_ne!(second["event_id"], first["event_id"], "an edit is a new event");

        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, 30023);
        assert_eq!(events[1].content, "rewritten");
        assert_eq!(
            events[0].tags.iter().find(|t| t[0] == "d"),
            events[1].tags.iter().find(|t| t[0] == "d"),
            "the same `d` both times"
        );
        verify(&events[1]);
    }

    /// A refusal is a status, not a lost article: the address is recorded either
    /// way, so the retry publishes the same one rather than a second article.
    #[tokio::test]
    async fn a_refused_publish_keeps_the_address_for_the_retry() {
        let sink = FakeSink::refusing("wss://relay.example: connection refused");
        let (_dir, _app, router, zine_id, _) = exportable(sink).await;

        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["published"], false);
        assert!(
            out["publish_error"].as_str().unwrap().contains("connection refused"),
            "{out}"
        );
        assert_eq!(
            out["zine"]["export"]["d"], out["d"],
            "the address is minted before the publish, so a failure does not move it"
        );

        let (status, retry) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{retry}");
        assert_eq!(retry["d"], out["d"], "the retry publishes the same article");
        assert_eq!(retry["published_at"], out["published_at"]);
    }

    /// An export must not be the thing that mints the identity it publishes
    /// under, and it must not leave an address behind for a retry to find when
    /// it could not go out at all.
    #[tokio::test]
    async fn an_export_before_a_public_identity_is_refused_and_mints_nothing() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        *app.neighborhoods_sink.lock().await = Some(FakeSink::accepting());
        let router = api_router(app);

        let (status, pen) = request(
            &router,
            "POST",
            "/api/zines/pen-name",
            Some(r#"{"name":"ada"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{pen}");
        let (status, created) = request(
            &router,
            "POST",
            "/api/zines",
            Some(r#"{"title":"On relays","body":"<p>hello</p>"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let zine_id = created["zine_id"].as_str().unwrap().to_string();

        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
        assert!(
            err["error"].as_str().unwrap().contains("public identity"),
            "{err}"
        );

        let (_, one) = request(&router, "GET", &format!("/api/zines/{zine_id}"), None).await;
        assert!(one["zine"]["export"].is_null(), "no address was minted");
        let (status, identity) = request(&router, "GET", "/api/neighborhoods/identity", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(identity.is_null(), "and no identity was created either");
    }

    /// An empty allowlist is a configuration problem, not a gateway problem, and
    /// it is refused before an address is minted: an export that cannot go out
    /// must leave nothing behind.
    #[tokio::test]
    async fn an_export_with_no_relays_is_refused_and_mints_nothing() {
        let sink = FakeSink::accepting();
        let (_dir, _app, router, zine_id, _) = exportable(sink.clone()).await;
        let (status, _) = request(
            &router,
            "POST",
            "/api/neighborhoods/settings",
            Some(r#"{"allowlist":[],"min_minutes":1,"default_show":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
        assert!(err["error"].as_str().unwrap().contains("no relays"), "{err}");
        assert!(sink.events().is_empty(), "nothing was sent anywhere");

        let (_, one) = request(&router, "GET", &format!("/api/zines/{zine_id}"), None).await;
        assert!(one["zine"]["export"].is_null(), "no address was minted");
    }

    /// The export route is as private as the sidecar: a locked vault is a 401
    /// before any of the policy above runs.
    #[tokio::test]
    async fn the_export_route_is_401_while_locked() {
        use crate::notes_api_tests::build_app;
        let dir = TempDir::new().unwrap();
        let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
        let (status, _) = request(
            &router,
            "POST",
            &format!("/api/zines/{}/export", "a".repeat(64)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

/// Public commenting (Task 22, Phase 4): the write half of the comment lane.
///
/// The transport is a stub, because what has to hold is not that a relay was
/// reached but that the *event* is the NIP-22 one other clients read: the right
/// kind, the root tags that scope it to the article, the parent tags that scope a
/// reply to the comment it answers, plaintext, and a signature that verifies
/// under the public identity. And that the private thread is not involved at all.
#[cfg(test)]
mod neighborhood_comment_api_tests {
    use super::*;
    use crate::notes_api_tests::{request, unlocked_app};
    use crate::zine_export_api_tests::{verify, FakeSink};
    use crate::zines_api_tests::{long_form_event, FakeRelay};
    use tempfile::TempDir;

    /// A comment event as a relay would serve it.
    fn comment_event(
        id: &str,
        root: &str,
        parent: Option<&str>,
        created_at: u64,
        body: &str,
    ) -> hush_articles::RawEvent {
        let mut tags = vec![
            serde_json::json!(["A", root]),
            serde_json::json!(["K", hush_articles::KIND_LONG_FORM.to_string()]),
            serde_json::json!(["P", "cc"]),
        ];
        match parent {
            Some(parent) => {
                tags.push(serde_json::json!(["e", parent]));
                tags.push(serde_json::json!(["k", hush_articles::KIND_COMMENT.to_string()]));
                tags.push(serde_json::json!(["p", "dd"]));
            }
            None => tags.push(serde_json::json!(["k", hush_articles::KIND_LONG_FORM.to_string()])),
        }
        serde_json::from_value(serde_json::json!({
            "id": id,
            "pubkey": "cc",
            "created_at": created_at,
            "kind": hush_articles::KIND_COMMENT,
            "tags": tags,
            "content": body,
        }))
        .unwrap()
    }

    async fn set_allowlist(router: &axum::Router) {
        let (status, _) = request(
            router,
            "POST",
            "/api/neighborhoods/settings",
            Some(r#"{"allowlist":["wss://relay.example"],"min_minutes":1,"default_show":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// A vault with one article published and its thread readable: a byline, a
    /// post, a public identity, an allowlist, a stubbed sink, and a stubbed relay
    /// holding the article the export produced plus whatever `comments` builds.
    ///
    /// The export runs first so the address is the real one — the address is what
    /// a comment is scoped to, so a made-up one would test the mapping against
    /// something no member will ever have. `comments` is a function of that
    /// address for the same reason.
    ///
    /// Returns the address, the public key, and the id of the article version the
    /// relay serves — the last because a top-level comment's `e` tag names it.
    async fn published(
        sink: Arc<FakeSink>,
        comments: impl FnOnce(&str) -> Vec<hush_articles::RawEvent>,
    ) -> (TempDir, axum::Router, String, String, String) {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        *app.neighborhoods_sink.lock().await = Some(sink);
        let router = api_router(app.clone());

        let (status, pen) = request(
            &router,
            "POST",
            "/api/zines/pen-name",
            Some(r#"{"name":"ada"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{pen}");
        let (status, created) = request(
            &router,
            "POST",
            "/api/zines",
            Some(r#"{"title":"On relays","body":"<p>hello</p>"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let zine_id = created["zine_id"].as_str().unwrap().to_string();

        let (status, identity) = request(&router, "POST", "/api/neighborhoods/identity", Some("{}")).await;
        assert_eq!(status, StatusCode::OK, "{identity}");
        let pubkey = identity["pubkey"].as_str().unwrap().to_string();

        set_allowlist(&router).await;

        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/export"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        let address = out["address"].as_str().unwrap().to_string();
        let d = out["d"].as_str().unwrap().to_string();

        // The relay now serves back the article that was published, plus
        // whatever the test wants the thread to hold.
        let article = long_form_event(&pubkey, &d, 20, &"word ".repeat(800));
        let article_id = article.id.clone();
        *app.neighborhoods_source.lock().await = Some(FakeRelay::new(
            vec![article],
            comments(&address),
        ));

        let (status, view) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(
            view["articles"].as_array().unwrap().len(),
            1,
            "the published article is cached: {view}"
        );

        (dir, router, address, pubkey, article_id)
    }

    fn tag(event: &hush_articles::Event, name: &str) -> Option<String> {
        event
            .tags
            .iter()
            .find(|t| t[0] == name)
            .map(|t| t[1].clone())
    }

    /// The comment that goes out is the one NIP-22 clients read: kind 1111,
    /// plaintext, scoped to the article with both the root and the parent tags,
    /// and signed by the public identity rather than the pen name.
    #[tokio::test]
    async fn a_comment_is_published_as_a_signed_nip22_event() {
        let sink = FakeSink::accepting();
        let (_dir, router, address, pubkey, article_id) = published(sink.clone(), |_| vec![]).await;

        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/neighborhoods/{address}/comment"),
            Some(r#"{"body":"  Worth reading.  "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["published"], true, "{out}");
        assert_eq!(out["publish_error"], "", "{out}");
        assert_eq!(out["address"], address);
        assert_eq!(out["event_id"].as_str().unwrap().len(), 64);

        // The thread comes back with the comment already in it: the page shows a
        // thread, so it should not have to splice its own words in.
        let replies = out["replies"].as_array().unwrap();
        assert_eq!(replies.len(), 1, "{out}");
        assert_eq!(replies[0]["content"], "Worth reading.");
        assert_eq!(replies[0]["pubkey"], pubkey);
        assert!(replies[0]["parent_id"].is_null(), "a top-level comment");

        // The export went out first; the comment is the second event.
        let events = sink.events();
        assert_eq!(events.len(), 2, "{events:?}");
        let event = &events[1];
        assert_eq!(event.kind, 1111);
        assert_eq!(event.pubkey, pubkey);
        assert_eq!(event.content, "Worth reading.", "plaintext, trimmed");
        assert_eq!(event.id, out["event_id"].as_str().unwrap());
        verify(event);

        assert_eq!(tag(event, "A").as_deref(), Some(address.as_str()));
        assert_eq!(tag(event, "K").as_deref(), Some("30023"));
        assert_eq!(tag(event, "P").as_deref(), Some(pubkey.as_str()));
        assert_eq!(
            tag(event, "a").as_deref(),
            Some(address.as_str()),
            "a top-level comment's parent is the article"
        );
        assert_eq!(tag(event, "k").as_deref(), Some("30023"));
        assert_eq!(tag(event, "p").as_deref(), Some(pubkey.as_str()));
        assert_eq!(
            tag(event, "e").as_deref(),
            Some(article_id.as_str()),
            "the parent event id is the article version's, not the comment's"
        );
    }

    /// A reply keeps the article as its root and moves only the parent, which is
    /// the whole of NIP-22 threading.
    #[tokio::test]
    async fn a_reply_scopes_to_the_article_and_names_the_comment_it_answers() {
        let sink = FakeSink::accepting();
        let (_dir, router, address, pubkey, _article_id) = published(sink.clone(), |address| {
            vec![
                // A comment in a thread this vault did not ask for. A relay
                // answers what it likes; it is not one of its threads.
                comment_event("other-thread", "30023:zz:elsewhere", None, 5, "not ours"),
                comment_event("parent-1", address, None, 10, "first"),
                comment_event("child-1", address, Some("parent-1"), 11, "answering"),
            ]
        })
        .await;

        // The article read carries the thread, oldest first, with the reply's
        // parent named — and nothing from a thread this vault did not ask for.
        let (status, article) =
            request(&router, "GET", &format!("/api/neighborhoods/{address}"), None).await;
        assert_eq!(status, StatusCode::OK, "{article}");
        let replies = article["replies"].as_array().unwrap();
        assert_eq!(replies.len(), 2, "{article}");
        assert_eq!(replies[0]["content"], "first");
        assert!(replies[0]["parent_id"].is_null());
        assert_eq!(replies[1]["content"], "answering");
        assert_eq!(replies[1]["parent_id"], "parent-1");

        // Replying to the comment, not the article.
        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/neighborhoods/{address}/comment"),
            Some(r#"{"body":"Same here.","parent_id":"parent-1"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");

        let events = sink.events();
        let event = &events[events.len() - 1];
        assert_eq!(event.kind, 1111);
        assert_eq!(
            tag(event, "A").as_deref(),
            Some(address.as_str()),
            "the root stays the article"
        );
        assert_eq!(tag(event, "K").as_deref(), Some("30023"));
        assert_eq!(tag(event, "P").as_deref(), Some(pubkey.as_str()));
        assert_eq!(
            tag(event, "e").as_deref(),
            Some("parent-1"),
            "the parent is the comment"
        );
        assert_eq!(
            tag(event, "k").as_deref(),
            Some("1111"),
            "the parent kind is what tells a reply from a top-level comment"
        );
        assert_eq!(tag(event, "p").as_deref(), Some("cc"), "the parent's author");
        assert!(
            event.tags.iter().all(|t| t[0] != "a"),
            "a reply has no lowercase address tag: the parent is a comment: {:?}",
            event.tags
        );
        verify(event);

        // And the thread now holds three, with the reply where it belongs.
        let (_, article) =
            request(&router, "GET", &format!("/api/neighborhoods/{address}"), None).await;
        let replies = article["replies"].as_array().unwrap();
        assert_eq!(replies.len(), 3, "{article}");
        assert_eq!(replies[2]["parent_id"], "parent-1");
    }

    /// A comment names the version it was written against, so commenting on
    /// something this vault does not hold would mean guessing at an event id.
    #[tokio::test]
    async fn commenting_on_an_article_this_vault_does_not_hold_is_a_404() {
        let sink = FakeSink::accepting();
        let (_dir, router, _address, _pubkey, _article_id) =
            published(sink.clone(), |_| vec![]).await;

        let (status, err) = request(
            &router,
            "POST",
            "/api/neighborhoods/30023:aa:missing/comment",
            Some(r#"{"body":"hello"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
        assert!(sink.events().len() == 1, "nothing was sent");
    }

    /// A parent this vault cannot read is one whose author it cannot name, and a
    /// parent from another thread would file the reply under the wrong article.
    #[tokio::test]
    async fn a_reply_to_a_comment_that_is_not_held_is_refused() {
        let sink = FakeSink::accepting();
        let (_dir, router, address, _, _article_id) = published(sink.clone(), |_| vec![]).await;

        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/neighborhoods/{address}/comment"),
            Some(r#"{"body":"hello","parent_id":"not-held"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert!(
            err["error"].as_str().unwrap().contains("not-held"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_empty_or_oversized_comment_is_refused() {
        let sink = FakeSink::accepting();
        let (_dir, router, address, _, _article_id) = published(sink.clone(), |_| vec![]).await;

        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/neighborhoods/{address}/comment"),
            Some(r#"{"body":"   "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

        let huge = "x".repeat(NEIGHBORHOOD_COMMENT_CHARS + 1);
        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/neighborhoods/{address}/comment"),
            Some(&format!(r#"{{"body":"{huge}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert!(err["error"].as_str().unwrap().contains("at most"), "{err}");
        assert_eq!(sink.events().len(), 1, "only the export was ever sent");
    }

    /// A comment must not be the thing that mints the identity it speaks under.
    #[tokio::test]
    async fn commenting_before_a_public_identity_is_refused_and_mints_nothing() {
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        *app.neighborhoods_sink.lock().await = Some(FakeSink::accepting());
        // An article in the cache, with no identity in the vault: the cache is
        // filled by a refresh, which mints nothing.
        *app.neighborhoods_source.lock().await =
            Some(FakeRelay::articles(vec![long_form_event(
                "aa",
                "kept",
                20,
                &"word ".repeat(800),
            )]));
        let router = api_router(app);

        let (status, _) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
        assert_eq!(status, StatusCode::OK);

        let (status, err) = request(
            &router,
            "POST",
            "/api/neighborhoods/30023:aa:kept/comment",
            Some(r#"{"body":"hello"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
        assert!(
            err["error"].as_str().unwrap().contains("public identity"),
            "{err}"
        );

        let (status, identity) = request(&router, "GET", "/api/neighborhoods/identity", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(identity.is_null(), "and no identity was created either");
    }

    /// A comment no relay took is not public, so it is not cached. Unlike an
    /// export there is no address to come back to, and showing the member their
    /// own words in a thread they cannot point anyone at would be a lie the page
    /// has no way to correct later.
    #[tokio::test]
    async fn a_comment_no_relay_took_is_not_cached_as_though_it_had_been() {
        let sink = FakeSink::refusing("wss://relay.example: connection refused");
        let dir = TempDir::new().unwrap();
        let app = unlocked_app(dir.path()).await;
        *app.neighborhoods_sink.lock().await = Some(sink.clone());
        let router = api_router(app.clone());

        request(&router, "POST", "/api/zines/pen-name", Some(r#"{"name":"ada"}"#)).await;
        let (_, created) = request(
            &router,
            "POST",
            "/api/zines",
            Some(r#"{"title":"On relays","body":"<p>hello</p>"}"#),
        )
        .await;
        let zine_id = created["zine_id"].as_str().unwrap().to_string();
        let (_, identity) = request(&router, "POST", "/api/neighborhoods/identity", Some("{}")).await;
        let pubkey = identity["pubkey"].as_str().unwrap().to_string();
        set_allowlist(&router).await;

        // The export refuses too, so seed the cache with the article directly:
        // what is under test is the comment, not the export.
        *app.neighborhoods_source.lock().await = Some(FakeRelay::articles(vec![
            long_form_event(&pubkey, "seed", 20, &"word ".repeat(800)),
        ]));
        let (status, _) = request(&router, "POST", "/api/neighborhoods/refresh", None).await;
        assert_eq!(status, StatusCode::OK);
        let address = format!("30023:{pubkey}:seed");

        let (status, out) = request(
            &router,
            "POST",
            &format!("/api/neighborhoods/{address}/comment"),
            Some(r#"{"body":"hello"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["published"], false);
        assert!(
            out["publish_error"].as_str().unwrap().contains("connection refused"),
            "{out}"
        );
        assert!(
            out["replies"].as_array().unwrap().is_empty(),
            "nothing is cached for a comment that never went out: {out}"
        );

        // And it is not there on a fresh read either.
        let (_, article) =
            request(&router, "GET", &format!("/api/neighborhoods/{address}"), None).await;
        assert!(article["replies"].as_array().unwrap().is_empty(), "{article}");
        let _ = zine_id;
    }

    /// The comment route is as private as the sidecar: a locked vault is a 401
    /// before any of the policy above runs.
    #[tokio::test]
    async fn the_comment_route_is_401_while_locked() {
        use crate::notes_api_tests::build_app;
        let dir = TempDir::new().unwrap();
        let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
        let (status, _) = request(
            &router,
            "POST",
            "/api/neighborhoods/30023:aa:kept/comment",
            Some(r#"{"body":"hello"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[cfg(test)]
mod tls_provider_tests {
    use super::*;

    /// Guards the install `main` performs before the daemon can dial out.
    ///
    /// Covers the mechanism rather than the call site: it runs the same function
    /// `main` runs, so changing the provider or emptying the function fails here.
    /// Deleting only the call from `main` would not — that is what the comment on
    /// the function is for, and it is why this test names the failure it stands in
    /// for rather than assuming it cannot return.
    #[test]
    fn the_process_default_tls_provider_is_installed() {
        install_tls_provider();

        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "no process-level rustls provider: the first TLS handshake would panic"
        );

        // The path that panicked in the field — a client asking for the process
        // default instead of naming a provider. With two providers enabled and
        // none installed, `builder()` panics here rather than failing a request.
        let _ = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }
}

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

use account_worker::{AccountWorker, ContactDto, IntroLists, PenNameDto, WorkResult};
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
    QueueMap, ReceivedMessage, TrusteeReach, VoiceEffect, VoiceManager, VoiceSession,
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
    /// Conclave voice's ring stamps, keyed by session id: the deadline until
    /// which an inbound open should ring this vault, so the UI can offer an
    /// answer instead of a silent pill. Runtime-only and self-expiring — the
    /// stamp is a timestamp, so it dies without a timer; a lock or restart
    /// clears it beside the planes, and the room it names never outlives it.
    voice_rings: Mutex<HashMap<String, u64>>,
    /// A device probe for Settings — a plane in Device mode with no peers,
    /// opened so the meter, the speaker test, and the health panel have
    /// something real to read between calls. Runtime-only; a watchdog drops
    /// it after the panel stops asking (a mic left hot with no panel is a
    /// lie the user can see in their hardware).
    device_probe: Mutex<Option<DeviceProbe>>,
}

/// The Settings-side device probe and its last-use stamp. The watchdog takes
/// the whole entry when the panel has not touched it for a minute.
struct DeviceProbe {
    plane: Arc<voice_data::VoicePlane>,
    last_used: Arc<Mutex<Instant>>,
}

/// How long an untouched probe survives before the watchdog closes its
/// streams.
const PROBE_IDLE_AFTER: Duration = Duration::from_secs(60);

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
    /// Whether this directory holds a vault at all. False means first run: the
    /// UI offers to create one instead of asking for a passphrase that cannot
    /// exist yet. Read from the directory, so it is known while locked.
    vault_present: bool,
}

#[derive(Deserialize)]
struct UnlockRequest {
    passphrase: String,
}

#[derive(Deserialize)]
struct VaultCreateRequest {
    passphrase: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    relay: Option<String>,
}

#[derive(Serialize)]
struct VaultCreateResponse {
    /// The recovery code, shown once. `None` when the core had none to hand over.
    recovery_code: Option<String>,
    vault_name: String,
    relay: String,
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
    /// The reach to use with the new trustee (Task 24). Absent means `Full`,
    /// which is the default and needs no announcement.
    #[serde(default)]
    reach: Option<TrusteeReach>,
}

#[derive(Deserialize)]
struct SetContactReachRequest {
    nickname: String,
    reach: TrusteeReach,
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
        .route("/api/vault/create", post(vault_create))
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
        // Voice devices (Settings). The literal `devices` segment matches
        // ahead of the `{conclave_id}` parameter, and cannot collide with a
        // room's conclave: an id is a minted uuid.
        .route(
            "/api/voice/devices",
            get(voice_devices).post(voice_select_devices),
        )
        .route("/api/voice/devices/meter", get(voice_input_meter))
        .route("/api/voice/devices/test", post(voice_test_output))
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
        // Task 11: publish the zine to the open web as a Standard.site document.
        // The record key is derived from the zine id, so a re-publish replaces
        // the record rather than adding a second one beside it.
        .route("/api/zines/{zine_id}/publish", post(zines_publish_public))
        // The public lane (Task 22): the member's own ATProto connection, the
        // publications they read, and the cached index of what those published.
        // `connect` is the only route that carries a credential.
        .route("/api/atproto", get(atproto_view))
        .route("/api/atproto/connect", post(atproto_connect))
        .route("/api/atproto/disconnect", post(atproto_disconnect))
        .route(
            "/api/atproto/publication",
            post(atproto_set_publication),
        )
        .route("/api/atproto/follows", post(atproto_set_follows))
        .route("/api/atproto/refresh", post(atproto_refresh))
        .route("/api/atproto/explore", get(atproto_explore))
        .route("/api/atproto/explore/refresh", post(atproto_explore_refresh))
        .route("/api/atproto/document", get(atproto_document))
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
        .route("/api/queues", get(queue_map))
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
        .route("/api/contacts/reach", post(set_contact_reach))
        .route("/api/outbox/retry", post(outbox_retry))
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
/// how one relay handshake took the whole daemon down.
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
        voice_rings: Mutex::new(HashMap::new()),
        device_probe: Mutex::new(None),
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
///
/// Every unix has `flock`, so this is not Linux-only. Leaving it out on macOS
/// let a force-quit leave a daemon behind that the next launch could not see,
/// and the two then ran against one vault.
#[cfg(unix)]
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

/// No-op on Windows, which has no `flock`; the socket check still applies.
#[cfg(not(unix))]
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
/// out on its own. Reach announcements (Task 24) ride the same pass: the user's
/// toggle records the choice locally and returns, and the peer hears about it
/// from here rather than from a user request blocking on a slow relay.
/// A tick with nothing queued touches no network at all.
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
        let _ = state.worker.retry_pending_reach().await;
        let _ = state.worker.retry_pending_conclave().await;
        let _ = state.worker.retry_regrants().await;
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
        vault_present: hush_core::vault_exists(&state.dir),
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

/// First run: create this directory's vault.
///
/// A directory that already holds a vault is refused (409) rather than reopened
/// or replaced, so a running app cannot be talked into overwriting an identity.
/// The relay is the request's, else the effective one for this directory
/// (`HUSH_RELAY_URL`, then client-config.json, then the default). It is written
/// into client-config.json too: the vault's own copy is encrypted, so this
/// plaintext one is what a later run reads to see which relay the vault is on.
///
/// Creating does not unlock. The caller still has the passphrase it just sent
/// and calls `/api/unlock`, so there is one unlock path rather than two.
async fn vault_create(
    State(state): State<Arc<App>>,
    Json(body): Json<VaultCreateRequest>,
) -> Result<Json<VaultCreateResponse>, (StatusCode, String)> {
    if hush_core::vault_exists(&state.dir) {
        return Err((
            StatusCode::CONFLICT,
            "This device already has a vault. Unlock it instead.".into(),
        ));
    }
    if body.passphrase.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "A vault needs a passphrase.".into()));
    }
    match state
        .worker
        .create(&state.dir, &body.passphrase, body.name.clone(), body.relay.clone())
        .await
    {
        WorkResult::Created {
            recovery_code,
            vault_name,
            relay,
        } => Ok(Json(VaultCreateResponse {
            recovery_code,
            vault_name,
            relay,
        })),
        WorkResult::Exists => Err((
            StatusCode::CONFLICT,
            "This device already has a vault. Unlock it instead.".into(),
        )),
        WorkResult::Err(message) => Err((StatusCode::BAD_REQUEST, message)),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "The vault could not be created.".into(),
        )),
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

/// The relay queues this vault drains and delivers into.
///
/// A local diagnostic, and the only way to attribute a queue seen filling on
/// the relay: the ids are HMACs over the device read token, derived on demand
/// and never stored, so no vault file names them. A queue that is not in this
/// list belongs to a peer, not to this vault.
async fn queue_map(
    State(state): State<Arc<App>>,
) -> Result<Json<QueueMap>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let result = map_err(state.worker.queues().await)?;
    match result {
        WorkResult::Queues(map) => Ok(Json(map)),
        _ => Err(unexpected_worker_response()),
    }
}

async fn create_conclave(
    State(state): State<Arc<App>>,
    Json(body): Json<CreateConclaveRequest>,
) -> Result<Json<ConclaveRecord>, (StatusCode, Json<ApiError>)> {    touch(&state).await;
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
    let reach = body.reach.unwrap_or_default();
    map_err(
        state
            .worker
            .add_contact(&body.nickname, &body.invite, reach)
            .await,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// Set how this vault reaches a trustee, and announce it (Task 24).
///
/// The choice is this vault's own; the effective reach for the pair is the
/// narrower of the two sides' choices, so this can only ever remove surfaces.
async fn set_contact_reach(
    State(state): State<Arc<App>>,
    Json(body): Json<SetContactReachRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(
        state
            .worker
            .set_contact_reach(&body.nickname, body.reach)
            .await,
    )?;
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

/// Flush the Outbox on demand: queued mail, queued reach announcements and
/// conclave copies the relay refused.
///
/// The outbox timer drains all four every 30 s and is the path everything
/// normally rides. This route exists so a drain can be triggered explicitly
/// (an Outbox screen's "send now", and the API tests): delivery stays
/// something nobody's request waits on.
async fn outbox_retry(
    State(state): State<Arc<App>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let _ = state.worker.retry_outbox().await;
    let _ = state.worker.retry_pending_reach().await;
    let _ = state.worker.retry_pending_conclave().await;
    let _ = state.worker.retry_regrants().await;
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
        | MessagePayload::Reach { .. }
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
// The public lane — AT Protocol, Standard.site (Task 22)
// ---------------------------------------------------------------------------
//
// A zine stays private and local. What goes public is one document in the
// member's own ATProto repo, written under the lexicons the open social web
// agreed on (`site.standard.publication` and `site.standard.document`), so it is
// readable by `pckt.blog`, `leaflet.pub`, `offprint.app` and anything else that
// speaks them — and their posts are readable here.
//
// Three properties hold by construction rather than by care:
//
// - No key is minted here. The identity is a DID the member already owns and the
//   credential is an app password they can revoke from their own account
//   settings, so this vault is a guest and never the holder of the key.
// - Nothing private can leave. The record shape has fields for a title, a body,
//   tags and a date and nowhere to put a pen name, a read key or a comment
//   thread, and the body is reduced to plaintext on the way out.
// - The lane is curated. Reads go to the publications the member named, one PDS
//   at a time; there is no firehose to subscribe to and no AppView in the path,
//   which is what keeps this app from holding the social graph it refuses to
//   hold.
//
// One thing publishing *does* disclose, and it belongs in the UI as much as
// here: the record is written to the member's own repo, so a public zine links
// this vault to their ATProto account. The pen name still covers the bulletin.

/// The one place the lane's route is decided.
///
/// The same switch the relay and the wallet answer to: with the relay on Tor the
/// lane goes through the same SOCKS proxy, and with it off the lane is direct,
/// like the Reads pane. A second place to decide would be a second answer, and
/// the two would drift.
async fn atproto_xrpc(state: &App) -> Result<hush_atproto::Xrpc, (StatusCode, Json<ApiError>)> {
    let socks = state.relay_socks.lock().await.clone();
    // The directory is resolved here rather than defaulted inside the client, so
    // a deployment that must use a mirror has one place to say so and the
    // default stays beside the lexicon constants it belongs to.
    let plc = hush_core::config::resolve_atproto_plc_url(&state.dir)
        .unwrap_or_else(|| hush_atproto::DEFAULT_PLC_DIRECTORY.to_string());
    hush_atproto::Xrpc::with_plc(socks.as_deref(), &plc).map_err(|e| atproto_err(&e))
}

/// A failure on the lane, as a status the UI can act on.
///
/// The distinction that matters is the one the crate draws: a refusal is the
/// member's to fix (a revoked app password, a handle that does not exist) and is
/// a 400, while a transport failure is the network's and is a 502. Collapsing
/// the two is how a UI ends up telling someone to check their password when the
/// real answer is that the proxy is not running.
fn atproto_err(error: &hush_atproto::Error) -> (StatusCode, Json<ApiError>) {
    let status = match error {
        hush_atproto::Error::Refused { .. } | hush_atproto::Error::Invalid(_) => {
            StatusCode::BAD_REQUEST
        }
        hush_atproto::Error::Transport(_) | hush_atproto::Error::Json(_) => StatusCode::BAD_GATEWAY,
        hush_atproto::Error::Proxy(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        Json(ApiError {
            error: error.to_string(),
        }),
    )
}

fn atproto_not_connected() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::CONFLICT,
        Json(ApiError {
            error: "connect an ATProto account before publishing".to_string(),
        }),
    )
}

fn atproto_no_publication() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::CONFLICT,
        Json(ApiError {
            error: "set the publication a zine belongs to before publishing".to_string(),
        }),
    )
}

fn atproto_not_held(uri: &str) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: format!("no document is held for {uri}"),
        }),
    )
}

/// Fetch the public lane's connection through the worker.
async fn atproto_connection(
    state: &App,
) -> Result<hush_atproto::Connection, (StatusCode, Json<ApiError>)> {
    match state.worker.secret_read(hush_atproto::CONNECTION_FILE).await {
        WorkResult::Secret(raw) => hush_atproto::Connection::decode(raw.as_deref())
            .map_err(|e| atproto_err(&e)),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Persist the connection wholesale.
async fn atproto_persist_connection(
    state: &App,
    connection: &hush_atproto::Connection,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let bytes = connection.encode().map_err(|e| atproto_err(&e))?;
    match state
        .worker
        .secret_write(hush_atproto::CONNECTION_FILE, bytes)
        .await
    {
        WorkResult::Ok => Ok(()),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Fetch the lane's cached state through the worker.
async fn atproto_state(
    state: &App,
) -> Result<hush_atproto::AtprotoState, (StatusCode, Json<ApiError>)> {
    match state.worker.secret_read(hush_atproto::STATE_FILE).await {
        WorkResult::Secret(raw) => hush_atproto::AtprotoState::decode(raw.as_deref())
            .map_err(|e| atproto_err(&e)),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// Persist the lane's cached state wholesale.
async fn atproto_persist_state(
    state: &App,
    lane: &hush_atproto::AtprotoState,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let bytes = lane.encode().map_err(|e| atproto_err(&e))?;
    match state.worker.secret_write(hush_atproto::STATE_FILE, bytes).await {
        WorkResult::Ok => Ok(()),
        WorkResult::Err(msg) => Err(worker_err(msg)),
        _ => Err(unexpected_worker_response()),
    }
}

/// The account this vault speaks as, as the pane sees it.
///
/// The app password and the tokens are deliberately absent: the webview has no
/// use for them, and a token in a response is a token in a log.
#[derive(Serialize)]
struct AtprotoAccountView {
    did: String,
    handle: String,
    pds: String,
}

/// One document in the index, without its body.
///
/// A page of fifty full articles would be megabytes of JSON for a screen that
/// shows titles, so the body is one request away
/// (`GET /api/atproto/document`) and the row carries a taste of it.
#[derive(Serialize)]
struct AtprotoDocumentView {
    uri: String,
    did: String,
    handle: String,
    title: String,
    tags: Vec<String>,
    /// The author's own string, kept verbatim for display.
    published_at: String,
    /// The same instant as a sortable number, parsed at ingest.
    published_secs: i64,
    path: String,
    /// The author's own summary, when they wrote one. Empty otherwise.
    description: String,
    /// The document's own page on the author's site, when it has one.
    ///
    /// Empty when the record is not a pointer, or when the publication it
    /// belongs to could not be read. A reader that finds a body in the record
    /// shows it; one that does not has somewhere to send the member instead of a
    /// bare title.
    url: String,
    preview: String,
    /// The Bluesky thread this document was announced as, when there is one.
    ///
    /// Absent rather than null when the author linked no thread, so a client can
    /// branch on presence without knowing the shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    bsky_post_ref: Option<hush_atproto::BskyPostRef>,
    /// The publication this document belongs to, as an `at://` URI. Empty when
    /// the record names none.
    site: String,
}

#[derive(Serialize)]
struct AtprotoTagView {
    name: String,
    count: usize,
}

/// The whole public pane in one response: who this vault speaks as, where its
/// posts are filed, who it reads, and what it has read.
#[derive(Serialize)]
struct AtprotoView {
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<AtprotoAccountView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publication: Option<hush_atproto::Publication>,
    follows: Vec<hush_atproto::Follow>,
    documents: Vec<AtprotoDocumentView>,
    /// The tag facet of the cache, most used first.
    tags: Vec<AtprotoTagView>,
}

/// The answer to a refresh: the pane, plus what the pass did.
///
/// Flattened rather than nested so the response is still an [`AtprotoView`] to
/// every reader that only wants the index.
#[derive(Serialize)]
struct AtprotoRefreshView {
    /// Documents that were new or had changed.
    ///
    /// Named `changed` rather than `documents` because the flattened view
    /// already carries a `documents` — the index itself — and a flattened field
    /// with the same name is silently overwritten by the one it flattens.
    changed: usize,
    /// Follows that could not be read, each with its reason. Reported rather
    /// than raised: one publication whose PDS is down must not cost the member
    /// every other page in the lane, and a lane that stops updating should say
    /// why.
    failed: Vec<String>,
    #[serde(flatten)]
    view: AtprotoView,
}

/// One document with its body, for the reading pane.
#[derive(Serialize)]
struct AtprotoDocumentFullView {
    #[serde(flatten)]
    document: AtprotoDocumentView,
    /// The body as published: plaintext, because the lexicon's field forbids
    /// markup.
    text: String,
}

fn atproto_view_of(
    connection: &hush_atproto::Connection,
    lane: &hush_atproto::AtprotoState,
) -> AtprotoView {
    AtprotoView {
        account: connection.session.as_ref().map(|s| AtprotoAccountView {
            did: s.did.clone(),
            handle: s.handle.clone(),
            pds: s.pds.clone(),
        }),
        publication: connection.publication.clone(),
        follows: lane.follows().to_vec(),
        documents: lane
            .index(None, None)
            .into_iter()
            .map(atproto_document_view)
            .collect(),
        tags: lane
            .tags()
            .into_iter()
            .map(|(name, count)| AtprotoTagView { name, count })
            .collect(),
    }
}

fn atproto_document_view(document: &hush_atproto::Document) -> AtprotoDocumentView {
    AtprotoDocumentView {
        uri: document.uri.clone(),
        did: document.did.clone(),
        handle: document.handle.clone(),
        title: document.title.clone(),
        tags: document.tags.clone(),
        published_at: document.published_at.clone(),
        published_secs: document.published_secs,
        path: document.path.clone(),
        description: document.description.clone(),
        url: document.url.clone(),
        preview: atproto_preview(&document.text, &document.description),
        bsky_post_ref: document.bsky_post_ref.clone(),
        site: document.site.clone(),
    }
}

/// A plain-text taste of a body, for the index row.
///
/// Falls back to the author's own description when the record carries no body. A
/// **pointer** document — 48 of the 105 in one vault's live cache — keeps its
/// words on the author's own site and only a description in the record, so
/// without the fallback every one of those rows read as a title with nothing
/// beside it.
fn atproto_preview(text: &str, description: &str) -> String {
    let source = if text.trim().is_empty() {
        description
    } else {
        text
    };
    let collapsed = source.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(160).collect()
}

/// A short taste of a body, for the lexicon's optional `description`.
///
/// Derived rather than asked for: the author already wrote the opening, and a
/// second field to fill in would be one more thing between a zine and its
/// publication. Long enough to be a sentence, short enough to be a card.
fn atproto_description(text: &str) -> Option<String> {
    const CARD_CHARS: usize = 200;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(CARD_CHARS).collect())
}

/// The pane, in one response.
async fn atproto_view(
    State(state): State<Arc<App>>,
) -> Result<Json<AtprotoView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let connection = atproto_connection(&state).await?;
    let lane = atproto_state(&state).await?;
    Ok(Json(atproto_view_of(&connection, &lane)))
}

#[derive(Deserialize)]
struct AtprotoConnectBody {
    /// The PDS host, e.g. `https://bsky.social`.
    #[serde(default)]
    pds: String,
    /// The handle or DID to sign in as.
    #[serde(default)]
    identifier: String,
    /// An app password, never the account password: scoped to this use and
    /// revocable from the member's own settings, which is the whole reason this
    /// lane holds no key.
    #[serde(default)]
    password: String,
}

/// Sign in to a PDS with an app password and keep the session.
///
/// The password crosses this route once and is stored in the vault's encrypted
/// sidecar: an app password is revocable and scoped by the member, and holding
/// it is what lets a rotated access token cost a refresh rather than a
/// re-sign-in. Nothing about it reaches the webview or a log.
async fn atproto_connect(
    State(state): State<Arc<App>>,
    Json(body): Json<AtprotoConnectBody>,
) -> Result<Json<AtprotoView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    // The vault is read *before* the credential is used, so a locked daemon
    // refuses the route instead of relaying an app password to a PDS it has no
    // business talking to yet — and so every route on the lane answers the lock
    // the same way.
    let mut connection = atproto_connection(&state).await?;
    let xrpc = atproto_xrpc(&state).await?;
    let session = xrpc
        .create_session(&body.pds, &body.identifier, &body.password)
        .await
        .map_err(|e| atproto_err(&e))?;
    // A different account is a different repo, and the publication pointer
    // belonged to the old one: kept, it would address a record this session
    // cannot write.
    if connection.session.as_ref().map(|s| s.did.as_str()) != Some(session.did.as_str()) {
        connection.publication = None;
    }
    connection.session = Some(session);
    atproto_persist_connection(&state, &connection).await?;
    let lane = atproto_state(&state).await?;
    Ok(Json(atproto_view_of(&connection, &lane)))
}

/// Forget the connection, and revoke the session it was holding.
///
/// The revocation is best effort and happens first: it is what makes a restored
/// backup of the sidecar worthless, because the refresh token inside it is dead.
/// A PDS that cannot be reached must not keep a credential here, so the local
/// half is cleared either way.
///
/// The follows and the cached index are deliberately kept. They are a reading
/// list, not a credential, and clearing them would cost a member their lane for
/// disconnecting an account they may reconnect a minute later.
async fn atproto_disconnect(
    State(state): State<Arc<App>>,
) -> Result<Json<AtprotoView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let connection = atproto_connection(&state).await?;
    if let Some(session) = connection.session.clone() {
        if let Ok(xrpc) = atproto_xrpc(&state).await {
            let _ = xrpc.delete_session(&session).await;
        }
    }
    let cleared = hush_atproto::Connection::default();
    atproto_persist_connection(&state, &cleared).await?;
    let lane = atproto_state(&state).await?;
    Ok(Json(atproto_view_of(&cleared, &lane)))
}

#[derive(Deserialize)]
struct AtprotoPublicationBody {
    /// The masthead: what the blog is called.
    #[serde(default)]
    name: String,
    /// The member's own web address. The lexicon combines it with each
    /// document's path to build a reader's link, and Hush neither hosts it nor
    /// can invent one, so setup asks.
    #[serde(default)]
    url: String,
}

/// Write (or replace) the publication this vault's documents belong to.
///
/// Written at a fixed record key, so running setup again replaces the blog
/// rather than starting a second one beside it.
async fn atproto_set_publication(
    State(state): State<Arc<App>>,
    Json(body): Json<AtprotoPublicationBody>,
) -> Result<Json<AtprotoView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut connection = atproto_connection(&state).await?;
    let session = connection
        .session
        .clone()
        .filter(|s| s.can_write())
        .ok_or_else(atproto_not_connected)?;
    let xrpc = atproto_xrpc(&state).await?;
    let (publication, rotated) =
        hush_atproto::ensure_publication(&xrpc, &session, &body.name, &body.url)
            .await
            .map_err(|e| atproto_err(&e))?;
    connection.session = Some(rotated);
    connection.publication = Some(publication);
    atproto_persist_connection(&state, &connection).await?;
    let lane = atproto_state(&state).await?;
    Ok(Json(atproto_view_of(&connection, &lane)))
}

#[derive(Deserialize)]
struct AtprotoFollowBody {
    /// The account's DID. Takes precedence when given: it is the half that
    /// cannot move.
    #[serde(default)]
    did: String,
    /// The handle to resolve when no DID is given. Resolved against the
    /// connected account's PDS, because a DID is what addresses a repo and
    /// nobody knows their favourite blog's by heart.
    #[serde(default)]
    handle: String,
    /// What to show on the page when the handle is not known.
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct AtprotoFollowsBody {
    #[serde(default)]
    follows: Vec<AtprotoFollowBody>,
}

/// Replace the follow list: the whole list, because that is what a curated lane
/// is. A patch would make "what am I reading" a diff of a diff.
async fn atproto_set_follows(
    State(state): State<Arc<App>>,
    Json(body): Json<AtprotoFollowsBody>,
) -> Result<Json<AtprotoView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let connection = atproto_connection(&state).await?;
    let xrpc = atproto_xrpc(&state).await?;
    let mut follows = Vec::new();
    for entry in body.follows {
        let did = if !entry.did.trim().is_empty() {
            entry.did.trim().to_string()
        } else {
            // Resolving a handle needs a PDS to ask, and the one this vault
            // already speaks to is the member's own. A DID needs nobody's help,
            // so a member who pastes one is not made to connect first.
            let session = connection
                .session
                .as_ref()
                .filter(|s| s.can_write())
                .ok_or_else(atproto_not_connected)?;
            xrpc.resolve_handle(&session.pds, &entry.handle)
                .await
                .map_err(|e| atproto_err(&e))?
        };
        follows.push(hush_atproto::Follow {
            did,
            handle: entry.handle.trim().to_string(),
            name: entry.name.trim().to_string(),
        });
    }
    let mut lane = atproto_state(&state).await?;
    lane.set_follows(follows)
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    atproto_persist_state(&state, &lane).await?;
    Ok(Json(atproto_view_of(&connection, &lane)))
}

/// Read every followed publication's documents and merge them into the cache.
///
/// The follow list is what bounds this: an empty lane is a no-op, not an error,
/// and one unreachable PDS costs its own publication and nothing else.
async fn atproto_refresh(
    State(state): State<Arc<App>>,
) -> Result<Json<AtprotoRefreshView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let connection = atproto_connection(&state).await?;
    let xrpc = atproto_xrpc(&state).await?;
    let mut lane = atproto_state(&state).await?;
    let report = hush_atproto::refresh(&xrpc, &mut lane, hush_atproto::REFRESH_LIMIT).await;
    atproto_persist_state(&state, &lane).await?;
    Ok(Json(AtprotoRefreshView {
        changed: report.documents,
        failed: report.failed,
        view: atproto_view_of(&connection, &lane),
    }))
}

/// The discovery directory: what the last crawl found.
#[derive(Serialize)]
struct AtprotoExploreView {
    /// Every publication the crawl found, sorted by name.
    directory: Vec<hush_atproto::PublicationSummary>,
    /// When it was crawled, unix seconds. `0` means never, which is the state
    /// before the first crawl rather than a fault.
    crawled_unix: u64,
    /// Accounts that could not be probed, each with its reason. Reported rather
    /// than raised: one unreachable PDS must not cost the member the directory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failed: Vec<String>,
}

impl AtprotoExploreView {
    fn of(lane: &hush_atproto::AtprotoState) -> Self {
        let (directory, crawled_unix) = lane.directory();
        Self {
            directory: directory.to_vec(),
            crawled_unix,
            failed: Vec::new(),
        }
    }
}

/// The cached directory, and nothing else.
///
/// Deliberately not a crawl: this is what a page loads on open, and it must cost
/// no network. A crawl is the explicit button beside it.
async fn atproto_explore(
    State(state): State<Arc<App>>,
) -> Result<Json<AtprotoExploreView>, (StatusCode, Json<ApiError>)> {
    let lane = atproto_state(&state).await?;
    Ok(Json(AtprotoExploreView::of(&lane)))
}

/// Crawl one hop from the shipped seeds and replace the directory.
///
/// Needs no session, by design: discovery is for the member who will never sign
/// up, so requiring an account here would defeat the point of having it.
async fn atproto_explore_refresh(
    State(state): State<Arc<App>>,
) -> Result<Json<AtprotoExploreView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let xrpc = atproto_xrpc(&state).await?;
    let report = hush_atproto::explore(&xrpc, hush_atproto::SEEDS).await;
    let mut lane = atproto_state(&state).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    lane.set_directory(report.found, now);
    atproto_persist_state(&state, &lane).await?;
    let mut view = AtprotoExploreView::of(&lane);
    view.failed = report.failed;
    Ok(Json(view))
}

#[derive(Deserialize)]
struct AtprotoDocumentQuery {
    #[serde(default)]
    uri: String,
}

/// One document with its body, from the cache.
///
/// Served from what the last refresh read rather than re-fetched, so opening a
/// page costs no network and no PDS learns which article was opened.
async fn atproto_document(
    State(state): State<Arc<App>>,
    Query(query): Query<AtprotoDocumentQuery>,
) -> Result<Json<AtprotoDocumentFullView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let lane = atproto_state(&state).await?;
    let document = lane
        .get(query.uri.trim())
        .ok_or_else(|| atproto_not_held(&query.uri))?;
    Ok(Json(AtprotoDocumentFullView {
        document: atproto_document_view(document),
        text: document.text.clone(),
    }))
}

/// The answer to a public publish: what went out, and the pane either way.
#[derive(Serialize)]
struct ZinePublishView {
    /// Whether the PDS accepted the record. `false` is not a lost article: the
    /// zine is untouched and the publish can be tried again, which is why the
    /// whole pane comes back either way rather than an error status.
    published: bool,
    /// Why it did not, when it did not. Empty on success.
    publish_error: String,
    /// The record's `at://` address. Empty when nothing was written.
    uri: String,
    cid: String,
    /// The record key. Derived from the zine id, so it is the same on every
    /// publish and a retry replaces the record rather than adding one.
    rkey: String,
    /// The web address a reader opens: the publication's base URL and the
    /// document's path. Empty when no publication is set.
    url: String,
    zine_id: String,
    #[serde(flatten)]
    view: ZinesView,
}

/// Publish one zine to the open social web as a `site.standard.document`.
///
/// The private half stays private: what is built here is a title, a plaintext
/// body, the sections as tags and a date, and the record shape has nowhere to put
/// a pen name, a read key or a comment thread. The record key is derived from the
/// zine id, so a re-publish after an edit replaces the record rather than adding
/// a second document beside it — and the first publish time is held, so an edit
/// does not claim the post was written later than it was.
///
/// One thing this discloses, and it is the member's call to make: the record goes
/// into their own ATProto repo, so a public zine links this vault to that
/// account. The pen name still covers the bulletin.
async fn zines_publish_public(
    State(state): State<Arc<App>>,
    Path(zine_id): Path<String>,
) -> Result<Json<ZinePublishView>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut connection = atproto_connection(&state).await?;
    let session = connection
        .session
        .clone()
        .filter(|s| s.can_write())
        .ok_or_else(atproto_not_connected)?;
    let publication = connection
        .publication
        .clone()
        .ok_or_else(atproto_no_publication)?;
    let mut parsed = zines_sidecar(&state).await?;
    let Some(zine) = parsed.get(&zine_id) else {
        return Err(zine_not_held(&zine_id));
    };
    if !zine.authored {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "only the pen name that wrote a zine can publish it".to_string(),
            }),
        ));
    }
    let title = zine.descriptor.title.clone();
    let tags = zine.descriptor.sections.clone();
    let text = reads::rich_text_to_text(&zine.descriptor.body);
    let description = atproto_description(&text);

    let now_ms = paper_now_ms();
    // The address is minted and held *before* the record is built, so a publish
    // that failed does not move it on the retry.
    let address = parsed
        .publish_address(&zine_id, &session.did, now_ms)
        .map_err(|m| (StatusCode::BAD_REQUEST, Json(ApiError { error: m })))?;
    let rkey = hush_atproto::Post::rkey_for(&zine_id);
    let post = hush_atproto::Post {
        rkey: rkey.clone(),
        title,
        text,
        description,
        tags,
        published_at_ms: address.published_at_ms,
        updated_at_ms: (address.published_ms > 0).then_some(now_ms),
    };
    let xrpc = atproto_xrpc(&state).await?;
    let (published, publish_error, uri, cid) =
        match hush_atproto::publish(&xrpc, &session, &publication.uri, &post).await {
            Ok((reference, rotated)) => {
                connection.session = Some(rotated);
                atproto_persist_connection(&state, &connection).await?;
                parsed.record_publish(&zine_id, &reference.uri, &reference.cid, now_ms);
                (true, String::new(), reference.uri, reference.cid)
            }
            Err(e) => (false, e.to_string(), String::new(), String::new()),
        };
    zines_persist(&state, &parsed).await?;
    let pen_name = zines_pen_name_dto(&state).await?;
    Ok(Json(ZinePublishView {
        published,
        publish_error,
        uri,
        cid,
        rkey,
        url: format!("{}/{}", publication.url.trim_end_matches('/'), post.rkey),
        zine_id,
        view: zines_view(&parsed, None, None, pen_name),
    }))
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

/// How long an inbound open keeps ringing this vault. Long enough to be heard
/// across the ~4 s recv poll, short enough that a missed ring degrades to the
/// pill's "Join voice" affordance — the room itself is live-only either way,
/// so there is no missed-call record to tidy up.
const VOICE_RING_TTL_MS: u64 = 30_000;

/// The plane for `session_id`, created on demand (a room's opener never
/// dials, so its plane stays idle until the first inbound offer). The device
/// preference comes from the config file, so a Settings choice applies to
/// the next room a vault opens.
async fn voice_plane_for(state: &Arc<App>, session_id: &str) -> Arc<voice_data::VoicePlane> {
    let mut planes = state.voice_planes.lock().await;
    planes
        .entry(session_id.to_string())
        .or_insert_with(|| {
            Arc::new(voice_data::VoicePlane::with_selector(
                hush_core::config::resolve_stun_url(&state.dir),
                voice_media_mode(),
                voice_data::DeviceSelector {
                    input: hush_core::config::resolve_voice_input_device(&state.dir),
                    output: hush_core::config::resolve_voice_output_device(&state.dir),
                },
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
/// The ring stamps go with them — a locked vault cannot answer, so there is
/// nothing to keep ringing.
async fn voice_planes_shutdown(state: &Arc<App>) {
    let planes: Vec<Arc<voice_data::VoicePlane>> = {
        let mut planes = state.voice_planes.lock().await;
        planes.drain().map(|(_, plane)| plane).collect()
    };
    state.voice_rings.lock().await.clear();
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
    /// The deadline until which an inbound open should ring this vault,
    /// absent when there is no live ring. It rides every view so both the
    /// single-room and the state poll can render the same prompt.
    ringing_until_ms: Option<u64>,
}

fn voice_view_of(session: &VoiceSession, me: &str, rings: &HashMap<String, u64>) -> VoiceView {
    let now = paper_now_ms();
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
        ringing_until_ms: rings
            .get(&session.session.session_id)
            .copied()
            .filter(|until| *until > now),
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

/// A copy of the current ring stamps, so view construction never serializes
/// under a lock it does not need.
async fn voice_rings_of(state: &Arc<App>) -> HashMap<String, u64> {
    state.voice_rings.lock().await.clone()
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
        let view = voice_view_of(live, &me, &voice_rings_of(&state).await);
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
        // Answering the knock ends it: the stamp is only ever meaningful
        // while this vault has not joined, and a view that still rings an
        // answered room would be a lie the UI can see.
        state.voice_rings.lock().await.remove(&session_id);
        let rings = voice_rings_of(&state).await;
        (voice_view_of(live, &me, &rings), session_id, existing)
    };
    // Dial in the background. `plane.offer` gathers ICE inline — up to the
    // handshake deadline per dialed peer — and awaiting that before the join's
    // response made the UI's Voice click read as dead for seconds. The room
    // state is committed and the view computed above; the offers are
    // best-effort fan-out and can land after the response returns. A leave
    // that races the dial just fails the offer (logged, skipped) — the same
    // degraded posture the dial already had.
    let plane = voice_plane_for(&state, &session_id).await;
    let worker = state.worker.clone();
    let dial_conclave = conclave_id.clone();
    let dial_session = session_id.clone();
    let peers: Vec<String> = existing.iter().filter(|p| *p != &me).cloned().collect();
    tokio::spawn(async move {
        for peer in peers {
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
            if let WorkResult::Err(e) = worker
                .send_voice_offer(&dial_conclave, &dial_session, &offer)
                .await
            {
                eprintln!("hush-daemon: voice offer fan-out failed: {e}");
            }
        }
    });
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
    let rings = voice_rings_of(&state).await;
    Ok(Json(match mgr.session(&conclave_id) {
        // The room survived this leave: show it without this vault's member.
        Some(live) => voice_view_of(live, &me, &rings),
        // The leave ended the room (opener or below two): the honest answer
        // is the room emptied out, not an error.
        None => VoiceView {
            session_id,
            conclave_id,
            opener: before.session.opener,
            started_ms: before.session.started_ms,
            joined: Vec::new(),
            i_am_in: false,
            ringing_until_ms: None,
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
    let rings = voice_rings_of(&state).await;
    let view = mgr
        .session(&conclave_id)
        .map(|s| voice_view_of(s, &me, &rings))
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
    let rings = voice_rings_of(&state).await;
    mgr.session(&conclave_id)
        .map(|s| voice_view_of(s, &me, &rings))
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
    let rings = voice_rings_of(&state).await;
    Ok(Json(VoiceStateResponse {
        rooms: mgr
            .rooms()
            .iter()
            .map(|s| voice_view_of(s, &me, &rings))
            .collect(),
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

// --- Voice devices (Settings): what is in use, what is available, and a way
// --- to test it without a live call. The daemon owns the devices (cpal
// --- streams are !Send and live on its device threads), so every device
// --- question is a daemon question.

/// The probe used by the Settings panel: a Device-mode plane with no peers.
/// Its device thread opens the selected devices exactly as a room would, so
/// the meter and the speaker test exercise the same path a call would.
async fn voice_probe_ensure(
    state: &Arc<App>,
) -> Result<(Arc<voice_data::VoicePlane>, Arc<Mutex<Instant>>), (StatusCode, Json<ApiError>)> {
    if voice_media_mode() != voice_data::MediaMode::Device {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "this daemon's voice runs the test tone (HUSH_VOICE_TEST_TONE); \
                        there are no devices to probe"
                    .into(),
            }),
        ));
    }
    let mut probe = state.device_probe.lock().await;
    if let Some(p) = probe.as_ref() {
        *p.last_used.lock().await = Instant::now();
        return Ok((p.plane.clone(), p.last_used.clone()));
    }
    let plane = Arc::new(voice_data::VoicePlane::with_selector(
        hush_core::config::resolve_stun_url(&state.dir),
        voice_data::MediaMode::Device,
        voice_data::DeviceSelector {
            input: hush_core::config::resolve_voice_input_device(&state.dir),
            output: hush_core::config::resolve_voice_output_device(&state.dir),
        },
    ));
    let last_used = Arc::new(Mutex::new(Instant::now()));
    // The watchdog: a probe whose panel stopped asking is closed whole —
    // streams and all. A mic left hot with no panel watching it is visible
    // on the hardware and is not ours to hold.
    let wd_state = Arc::clone(state);
    let wd_last = Arc::clone(&last_used);
    let wd_plane = Arc::clone(&plane);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let last = *wd_last.lock().await;
            if last.elapsed() > PROBE_IDLE_AFTER {
                wd_plane.shutdown();
                let mut probe = wd_state.device_probe.lock().await;
                if probe
                    .as_ref()
                    .is_some_and(|cur| Arc::ptr_eq(&cur.plane, &wd_plane))
                {
                    *probe = None;
                }
                break;
            }
        }
    });
    *probe = Some(DeviceProbe {
        plane: plane.clone(),
        last_used: last_used.clone(),
    });
    Ok((plane, last_used))
}

/// The current selection, read back from the config — what the pickers show.
fn voice_selected(state: &Arc<App>) -> voice_data::DeviceSelector {
    voice_data::DeviceSelector {
        input: hush_core::config::resolve_voice_input_device(&state.dir),
        output: hush_core::config::resolve_voice_output_device(&state.dir),
    }
}

#[derive(Serialize)]
struct VoiceDevicesResponse {
    /// Whether this daemon's voice runs on real devices (false: the tone
    /// test mode — there is nothing to pick or test).
    device_mode: bool,
    /// The system's device names, as cpal reports them.
    inputs: Vec<String>,
    outputs: Vec<String>,
    /// The names on file in the config; `null` is the OS default.
    selected_input: Option<String>,
    selected_output: Option<String>,
    /// What a live room's device thread actually opened, when a room exists.
    /// Every session's plane follows the same selection, so the first is
    /// what the panel shows.
    session: Option<voice_data::DeviceHealth>,
    /// What the Settings probe holds, when one is open.
    probe: Option<voice_data::DeviceHealth>,
}

async fn voice_devices(
    State(state): State<Arc<App>>,
) -> Result<Json<VoiceDevicesResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    paper_account_id(&state).await?;
    let list = if voice_media_mode() == voice_data::MediaMode::Device {
        voice_data::list_devices()
    } else {
        voice_data::DeviceList::default()
    };
    let session = {
        let planes = state.voice_planes.lock().await;
        planes.values().next().and_then(|p| p.device_health())
    };
    let probe = state
        .device_probe
        .lock()
        .await
        .as_ref()
        .and_then(|p| p.plane.device_health());
    let selected = voice_selected(&state);
    Ok(Json(VoiceDevicesResponse {
        device_mode: voice_media_mode() == voice_data::MediaMode::Device,
        inputs: list.inputs,
        outputs: list.outputs,
        selected_input: selected.input,
        selected_output: selected.output,
        session,
        probe,
    }))
}

#[derive(Deserialize)]
struct VoiceDeviceSelectRequest {
    /// `Some(Some(name))` picks a device, `Some(None)` resets to the OS
    /// default, absent leaves the side unchanged.
    input: Option<Option<String>>,
    output: Option<Option<String>>,
}

/// Persist a device choice and reopen every live device thread on it — a
/// selection takes effect now, not "on the next call".
async fn voice_select_devices(
    State(state): State<Arc<App>>,
    Json(req): Json<VoiceDeviceSelectRequest>,
) -> Result<Json<VoiceDevicesResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    paper_account_id(&state).await?;
    if voice_media_mode() != voice_data::MediaMode::Device {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "this daemon's voice runs the test tone; there are no \
                        devices to select"
                    .into(),
            }),
        ));
    }
    let mut cfg = hush_core::config::load(&state.dir);
    if let Some(v) = req.input {
        cfg.voice_input_device = v;
    }
    if let Some(v) = req.output {
        cfg.voice_output_device = v;
    }
    hush_core::config::save(&state.dir, &cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: e })))?;
    let selector = voice_selected(&state);
    {
        let planes = state.voice_planes.lock().await;
        for plane in planes.values() {
            plane.reopen_devices(selector.clone());
        }
    }
    if let Some(p) = state.device_probe.lock().await.as_ref() {
        p.plane.reopen_devices(selector.clone());
        *p.last_used.lock().await = Instant::now();
    }
    voice_devices(State(state)).await
}

#[derive(Serialize)]
struct VoiceMeterResponse {
    /// Whether a real input device is open (false: silence by degradation,
    /// and the meter says so rather than sitting still for no reason).
    input_live: bool,
    input_name: Option<String>,
    /// Peak |sample| of the latest input callback, 0.0..1.0.
    level: f32,
}

/// The input meter's snapshot. Polled by the panel while it is open; the
/// poll is also what keeps the probe alive.
async fn voice_input_meter(
    State(state): State<Arc<App>>,
) -> Result<Json<VoiceMeterResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    paper_account_id(&state).await?;
    let (plane, _) = voice_probe_ensure(&state).await?;
    let health = plane.device_health().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError { error: "no device queues".into() }),
        )
    })?;
    Ok(Json(VoiceMeterResponse {
        input_live: health.input_live,
        input_name: health.input_name,
        level: health.input_level,
    }))
}

#[derive(Deserialize)]
struct VoiceTestRequest {
    /// How long the tone plays. Default: long enough to hear and let go.
    ms: Option<u64>,
}

#[derive(Serialize)]
struct VoiceTestResponse {
    /// Whether the tone will actually be audible — false when no output
    /// device opened, which is the panel's answer, not an error to hide.
    audible: bool,
    output_name: Option<String>,
}

/// The speaker test: a tone on the selected output, through the same device
/// thread a call would use.
async fn voice_test_output(
    State(state): State<Arc<App>>,
    Json(req): Json<VoiceTestRequest>,
) -> Result<Json<VoiceTestResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    paper_account_id(&state).await?;
    let (plane, last_used) = voice_probe_ensure(&state).await?;
    // A probe created by this very request needs a beat to open its output
    // stream; poll briefly rather than reporting a false "not audible".
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut health = plane.device_health();
    while health.as_ref().is_none_or(|h| !h.output_live) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        health = plane.device_health();
    }
    let audible = plane.play_test_tone(req.ms.unwrap_or(2_000));
    *last_used.lock().await = Instant::now();
    Ok(Json(VoiceTestResponse {
        audible,
        output_name: health.and_then(|h| h.output_name),
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
            // Ring the vault: a peer's open is a knock the UI answers. The
            // stamp is what every voice view reports as `ringing_until_ms`;
            // it expires on its own, and answering or dismissing is the UI's
            // business — the room's rules are untouched by it.
            state
                .voice_rings
                .lock()
                .await
                .insert(session.session_id.clone(), now + VOICE_RING_TTL_MS);
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
            // the whole plane — and any un-answered ring — with it.
            if ended {
                voice_plane_drop(state, session_id).await;
                state.voice_rings.lock().await.remove(session_id);
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
            voice_rings: Mutex::new(HashMap::new()),
            device_probe: Mutex::new(None),
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
            voice_rings: Mutex::new(HashMap::new()),
            device_probe: Mutex::new(None),
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
            voice_rings: Mutex::new(HashMap::new()),
            device_probe: Mutex::new(None),
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
        // A full v2 invite, not the pointer: this test's relay is a dead
        // address on purpose, and a pointer add resolves the bundle from the
        // relay's directory, which needs the network. The full payload carries
        // everything inline, so the add stays offline.
        let invite = peer.fresh_invite().await.unwrap();
        assert!(matches!(
            app.worker
                .add_contact("peer", &invite, TrusteeReach::Full)
                .await,
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
            ("GET", "/api/voice/devices", None),
            (
                "POST",
                "/api/voice/devices",
                Some(r#"{"input":"microphone"}"#),
            ),
            ("GET", "/api/voice/devices/meter", None),
            ("POST", "/api/voice/devices/test", Some(r#"{}"#)),
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

    /// The ring is a view concern, not a room rule: `voice_view_of` reports a
    /// stamp that is still ahead of the clock and omits one that has passed.
    /// The stamp itself is applied in `voice_apply_inbound` (a peer's open),
    /// which the two-daemon dogfood exercises end to end.
    #[test]
    fn voice_view_reports_a_live_ring_and_drops_an_expired_one() {
        let session = hush_core::VoiceSession {
            session: hush_core::VoiceDescriptor {
                session_id: "v-ring".into(),
                conclave_id: "c-1".into(),
                opener: "aa".into(),
                started_ms: 1_000,
            },
            joined: vec![hush_core::VoiceMember {
                account: "aa".into(),
                muted: false,
            }],
        };
        let now = paper_now_ms();
        let mut rings = HashMap::new();
        rings.insert("v-ring".to_string(), now + 10_000);

        let view = voice_view_of(&session, "bb", &rings);
        assert_eq!(view.ringing_until_ms, Some(now + 10_000));
        assert!(!view.i_am_in, "the rung vault has not answered yet");

        // A stamp behind the clock is not a ring: the prompt must not survive
        // its own deadline by any route that renders the view.
        rings.insert("v-ring".to_string(), now - 1);
        let view = voice_view_of(&session, "bb", &rings);
        assert_eq!(view.ringing_until_ms, None);

        // The opener's own view is in the room — the UI's answer state, which
        // stops the ring regardless of what a stale stamp says.
        let view = voice_view_of(&session, "aa", &rings);
        assert!(view.i_am_in);
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

/// Shared harness for daemon API tests: a real relay on a port and members
/// whose routes run over a real vault and worker.
///
/// Extracted so more than one API suite can drive the same two-vault setup
/// without a second copy of it.
#[cfg(test)]
mod api_test_harness {
    use super::*;
    use crate::notes_api_tests::{build_app, request};
    use hush_relay::RelayConfig;
    use tempfile::{NamedTempFile, TempDir};

    /// A real relay, on a real port, with its own database on disk.
    pub(crate) async fn relay_on_a_port() -> (String, NamedTempFile) {
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
    pub(crate) struct Member {
        /// Held only so the vault directory outlives the test.
        _dir: TempDir,
        pub(crate) app: Arc<App>,
    }

    impl Member {
        pub(crate) async fn new(relay: &str) -> Self {
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

        pub(crate) async fn post(
            &self,
            uri: &str,
            body: Option<&str>,
        ) -> (StatusCode, serde_json::Value) {
            request(&api_router(self.app.clone()), "POST", uri, body).await
        }

        pub(crate) async fn get(&self, uri: &str) -> (StatusCode, serde_json::Value) {
            request(&api_router(self.app.clone()), "GET", uri, None).await
        }

        /// Poll, the way the UI does: the bulletin drain rides on it.
        pub(crate) async fn poll(&self) -> serde_json::Value {
            let (status, view) = self.post("/api/recv", None).await;
            assert_eq!(status, StatusCode::OK, "recv failed: {view}");
            view
        }
    }

    /// Make two members contacts of each other.
    ///
    /// Both directions, because a session needs the *recipient's* queue and the
    /// only place it travels is their invite.
    pub(crate) async fn connect(a: &Member, b: &Member, a_nick: &str, b_nick: &str) {
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
    pub(crate) async fn pair() -> (String, NamedTempFile, Member, Member) {
        let (relay, db) = relay_on_a_port().await;
        let a = Member::new(&relay).await;
        let b = Member::new(&relay).await;
        connect(&a, &b, "ada", "bo").await;
        (relay, db, a, b)
    }
}

/// Contact readiness over the daemon API, which is what the Chats list renders.
///
/// Adding each other is the ordinary way a trust starts, and it leaves neither
/// side having sent a message. The list must still report the relationship as
/// established, or a mutual, working trust reads as "Waiting for them to add
/// you" in the UI.
#[cfg(test)]
mod contacts_api_tests {
    use super::api_test_harness::pair;
    use super::*;

    #[tokio::test]
    async fn a_mutual_add_reads_ready_without_any_message() {
        let (_relay, _db, a, b) = pair().await;

        // Both sides hold the other's invite; nothing has been sent either way.
        let (status, before) = a.get("/api/contacts").await;
        assert_eq!(status, StatusCode::OK, "{before}");
        assert_eq!(before[0]["nickname"], "bo", "{before}");
        assert_eq!(
            before[0]["inbound_ready"], false,
            "nothing has been drained yet: {before}"
        );

        // Draining consumes the peer's queue grant, which is their proof of
        // having added our invite.
        a.poll().await;
        b.poll().await;

        for (who, member) in [("ada", &a), ("bo", &b)] {
            let (status, list) = member.get("/api/contacts").await;
            assert_eq!(status, StatusCode::OK, "{list}");
            assert_eq!(
                list[0]["inbound_ready"], true,
                "{who} must read ready after draining the peer's grant: {list}"
            );
        }
    }
}

/// Reach over the daemon API (Task 24): the choice is set, announced, and
/// enforced at the routes the UI actually calls.
#[cfg(test)]
mod reach_api_tests {
    use super::api_test_harness::{pair, Member};
    use super::*;

    /// Adding with `mail_only` records the choice, and the chat route refuses
    /// it with a 400 rather than a 500 — this is a user error, not a crash.
    #[tokio::test]
    async fn add_with_mail_only_refuses_the_chat_route() {
        let (relay, _db) = super::api_test_harness::relay_on_a_port().await;
        let a = Member::new(&relay).await;
        let b = Member::new(&relay).await;
        let (status, invite) = b.post("/api/invite", None).await;
        assert_eq!(status, StatusCode::OK, "{invite}");

        let body = serde_json::json!({
            "nickname": "bo",
            "invite": invite["invite"].as_str().unwrap(),
            "reach": "mail_only",
        })
        .to_string();
        let (status, err) = a.post("/api/contacts/add", Some(&body)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{err}");

        let (status, list) = a.get("/api/contacts").await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert_eq!(list[0]["reach"], "mail_only");
        assert_eq!(list[0]["my_reach"], "mail_only");

        // Bob must heartbeat for chat to even be attempted, but the reach gate
        // runs first, so the refusal does not depend on presence.
        let (status, err) = a
            .post("/api/send", Some(r#"{"to":"bo","text":"hi"}"#))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert!(
            err["error"].as_str().unwrap().contains("mail-only"),
            "got {err}"
        );
    }

    /// The reach route flips a relationship both ways and the gate follows it:
    /// mail-only refuses chat, full allows it again.
    #[tokio::test]
    async fn the_reach_route_opens_and_closes_chat() {
        let (_relay, _db, a, b) = pair().await;
        b.poll().await; // drain the grant, so chat has somewhere to go

        let (status, err) = a
            .post(
                "/api/contacts/reach",
                Some(r#"{"nickname":"bo","reach":"mail_only"}"#),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{err}");
        let (status, err) = a
            .post("/api/send", Some(r#"{"to":"bo","text":"hi"}"#))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

        let (status, err) = a
            .post(
                "/api/contacts/reach",
                Some(r#"{"nickname":"bo","reach":"full"}"#),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{err}");

        // Full again: the send is no longer refused for reach. Bob is offline,
        // so it fails on presence instead — a different error, which is the
        // point: reach is no longer what stands in the way.
        let (status, err) = a
            .post("/api/send", Some(r#"{"to":"bo","text":"hi"}"#))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert!(
            err["error"].as_str().unwrap().contains("offline"),
            "reach must not be the blocker once full, got {err}"
        );
    }

    /// The announcement reaches the peer's contact list, so the UI on both
    /// sides shows the same effective reach.
    #[tokio::test]
    async fn the_announcement_reaches_the_peers_contact_list() {
        let (_relay, _db, a, b) = pair().await;
        let (status, err) = a
            .post(
                "/api/contacts/reach",
                Some(r#"{"nickname":"bo","reach":"mail_only"}"#),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{err}");

        // The announcement is queued, not delivered inline: flush it the way
        // the outbox timer (or an Outbox "send now") would.
        let (status, err) = a.post("/api/outbox/retry", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{err}");

        b.poll().await;
        let (status, list) = b.get("/api/contacts").await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert_eq!(
            list[0]["reach"], "mail_only",
            "the peer's effective reach narrows to match"
        );
        assert_eq!(
            list[0]["my_reach"], "full",
            "the peer's own choice is untouched"
        );
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
    use crate::notes_api_tests::request;
    // The two-vault harness is shared with the reach API suite.
    use super::api_test_harness::{connect, pair, Member};
    use tempfile::TempDir;

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


#[cfg(test)]
mod atproto_api_tests {
    use super::*;
    use crate::notes_api_tests::{build_app, request, unlocked_app};
    use axum::{
        extract::{Path, Query, State},
        http::HeaderMap,
        routing::{get, post},
        Json, Router,
    };
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    /// One stub PDS that also answers as the PLC directory.
    ///
    /// The same shape the crate's own tests use, and for the same reason: the
    /// whole lane then runs over real HTTP through the real client, so what is
    /// exercised is the route's wiring rather than a mock of it. It answers as
    /// the directory too because `refresh` resolves each follow's DID before it
    /// can ask that follow's repo anything.
    struct Stub {
        /// This stub's own base, filled in once the listener is bound.
        base: String,
        /// repo → its documents, as `listRecords` returns them.
        repos: HashMap<String, Vec<serde_json::Value>>,
        /// repo → its publication records. Separate from `repos` because the two
        /// collections answer differently and a pointer document needs the
        /// publication to be resolvable at all.
        publications: Mutex<HashMap<String, Vec<serde_json::Value>>>,
        /// A PDS endpoint to answer for a DID that is not this stub.
        foreign: HashMap<String, String>,
        /// Every `putRecord` body the stub was handed, in order.
        written: Mutex<Vec<serde_json::Value>>,
        /// Every `Authorization` header a write arrived with.
        auth: Mutex<Vec<String>>,
    }

    async fn plc(State(stub): State<Arc<Stub>>, Path(did): Path<String>) -> Json<serde_json::Value> {
        let endpoint = stub
            .foreign
            .get(&did)
            .cloned()
            .unwrap_or_else(|| stub.base.clone());
        Json(serde_json::json!({
            "id": did,
            "service": [{ "id": "#atproto_pds", "serviceEndpoint": endpoint }],
        }))
    }

    async fn create_session() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "accessJwt": "access-1",
            "refreshJwt": "refresh-1",
            "did": "did:plc:writer",
            "handle": "ada.test",
        }))
    }

    async fn delete_session() -> StatusCode {
        StatusCode::OK
    }

    async fn resolve_handle(Query(query): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
        let handle = query.get("handle").cloned().unwrap_or_default();
        Json(serde_json::json!({ "did": format!("did:plc:{handle}") }))
    }

    async fn list_records(
        State(stub): State<Arc<Stub>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let repo = query.get("repo").cloned().unwrap_or_default();
        let collection = query
            .get("collection")
            .cloned()
            .unwrap_or_else(|| "site.standard.document".to_string());
        // The two collections answer from different maps: a repo's publications
        // are what a pointer document needs to become a link, and returning
        // documents for both is what would hide the bug this covers.
        let values: Vec<serde_json::Value> = if collection == "site.standard.publication" {
            stub.publications
                .lock()
                .unwrap()
                .get(&repo)
                .cloned()
                .unwrap_or_default()
        } else {
            stub.repos.get(&repo).cloned().unwrap_or_default()
        };
        let records: Vec<serde_json::Value> = values
            .iter()
            .enumerate()
            .map(|(i, value)| {
                serde_json::json!({
                    "uri": format!("at://{repo}/{collection}/{i}"),
                    "cid": "bafy",
                    "value": value,
                })
            })
            .collect();
        Json(serde_json::json!({ "records": records }))
    }

    async fn put_record(
        State(stub): State<Arc<Stub>>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        stub.auth.lock().unwrap().push(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string(),
        );
        let collection = body["collection"].as_str().unwrap_or_default().to_string();
        let rkey = body["rkey"].as_str().unwrap_or_default().to_string();
        let repo = body["repo"].as_str().unwrap_or_default().to_string();
        stub.written.lock().unwrap().push(body.clone());
        Json(serde_json::json!({
            "uri": format!("at://{repo}/{collection}/{rkey}"),
            "cid": "bafyrecord",
        }))
    }

    /// Bind the stub, serve it, and point this vault's `did:plc` resolution at
    /// it.
    ///
    /// The directory is named through the config file rather than the
    /// environment, so the setting is per-vault and two tests in one process
    /// cannot race each other for it.
    async fn stub_pds(
        dir: &std::path::Path,
        repos: HashMap<String, Vec<serde_json::Value>>,
        foreign: HashMap<String, String>,
    ) -> Arc<Stub> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stub = Arc::new(Stub {
            base: base.clone(),
            repos,
            publications: Mutex::new(HashMap::new()),
            foreign,
            written: Mutex::new(Vec::new()),
            auth: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route(
                "/xrpc/com.atproto.server.createSession",
                post(create_session),
            )
            .route(
                "/xrpc/com.atproto.server.deleteSession",
                post(delete_session),
            )
            .route("/xrpc/com.atproto.repo.putRecord", post(put_record))
            .route("/xrpc/com.atproto.repo.listRecords", get(list_records))
            .route(
                "/xrpc/com.atproto.identity.resolveHandle",
                get(resolve_handle),
            )
            .route("/{did}", get(plc))
            .with_state(stub.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let config = hush_core::config::ClientConfig {
            atproto_plc_url: Some(base.clone()),
            ..Default::default()
        };
        hush_core::config::save(dir, &config).unwrap();
        stub
    }

    fn document(title: &str, secs: u64, tags: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "title": title,
            "textContent": format!("The body of {title}."),
            "tags": tags,
            "publishedAt": hush_atproto::rfc3339(secs * 1000),
            "path": "/p",
        })
    }

    /// Connect the vault to the stub, which is what every test past setup needs.
    async fn connect(router: &axum::Router, pds: &str) -> serde_json::Value {
        let (status, view) = request(
            router,
            "POST",
            "/api/atproto/connect",
            Some(&format!(
                r#"{{"pds":"{pds}","identifier":"ada.test","password":"app-pw"}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        view
    }

    /// Set the publication a publish needs, and return the stub's record of it.
    async fn set_publication(router: &axum::Router) -> serde_json::Value {
        let (status, view) = request(
            router,
            "POST",
            "/api/atproto/publication",
            Some(r#"{"name":"Ada's zine","url":"https://ada.example/"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        view
    }

    /// Choose a byline and write one zine, answering its id.
    async fn write_a_zine(router: &axum::Router) -> String {
        request(
            router,
            "POST",
            "/api/zines/pen-name",
            Some(r#"{"name":"ada"}"#),
        )
        .await;
        let (status, created) = request(
            router,
            "POST",
            "/api/zines",
            Some(r#"{"title":"On relays","sections":["Rust"],"body":"<p>hello <strong>world</strong></p>"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        created["zine_id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn the_lane_starts_empty_and_names_no_account() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);

        let (status, view) = request(&router, "GET", "/api/atproto", None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert!(view["account"].is_null(), "no account before one is connected");
        assert!(view["publication"].is_null());
        assert_eq!(view["follows"].as_array().unwrap().len(), 0);
        assert_eq!(view["documents"].as_array().unwrap().len(), 0);
        assert_eq!(view["tags"].as_array().unwrap().len(), 0);

        // Nothing on the lane needs the network before an account exists, so a
        // refresh with no follows is a no-op rather than a failure.
        let (status, refreshed) = request(&router, "POST", "/api/atproto/refresh", None).await;
        assert_eq!(status, StatusCode::OK, "{refreshed}");
        assert_eq!(refreshed["changed"], 0);
        assert_eq!(refreshed["failed"].as_array().unwrap().len(), 0);
    }

    /// The credential is the one thing this route carries, and the one thing it
    /// must not hand back. The webview has no use for an app password or a token
    /// pair, and a token in a response is a token in a log.
    #[tokio::test]
    async fn a_connect_keeps_the_password_and_the_tokens_out_of_the_answer() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let stub = stub_pds(dir.path(), HashMap::new(), HashMap::new()).await;

        let (status, view) = request(
            &router,
            "POST",
            "/api/atproto/connect",
            Some(&format!(
                r#"{{"pds":"{}","identifier":"ada.test","password":"app-pw"}}"#,
                stub.base
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["account"]["did"], "did:plc:writer");
        assert_eq!(view["account"]["handle"], "ada.test");
        assert_eq!(view["account"]["pds"], stub.base);

        let wire = view.to_string();
        for secret in ["app-pw", "access-1", "refresh-1"] {
            assert!(!wire.contains(secret), "{secret} reached the webview: {wire}");
        }

        // The session survives the request: the vault holds it, not the webview.
        let (status, again) = request(&router, "GET", "/api/atproto", None).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again["account"]["did"], "did:plc:writer");
    }

    #[tokio::test]
    async fn a_pds_that_is_not_https_is_refused_before_a_request_is_made() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);

        for bad in [
            // Cleartext to a host that is not this machine: an app password on
            // the wire is the whole risk.
            "http://pds.example",
            "http://10.0.0.5:9876",
            "bsky.social",
            "ftp://pds.example",
            // A name that merely begins with a loopback literal is not loopback.
            "http://127.0.0.1.example.com",
        ] {
            let (status, err) = request(
                &router,
                "POST",
                "/api/atproto/connect",
                Some(&format!(
                    r#"{{"pds":"{bad}","identifier":"ada.test","password":"app-pw"}}"#
                )),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {err}");
            assert!(err["error"].as_str().unwrap().contains("PDS"), "{bad}: {err}");
        }
    }

    /// A publish needs two things that are not the zine: an account to write as
    /// and a publication to write under. Both refusals are conflicts rather than
    /// errors, because both are states the member can leave.
    #[tokio::test]
    async fn publishing_needs_an_account_and_then_a_publication() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let stub = stub_pds(dir.path(), HashMap::new(), HashMap::new()).await;
        let zine_id = write_a_zine(&router).await;

        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/publish"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
        assert!(err["error"].as_str().unwrap().contains("connect"), "{err}");

        connect(&router, &stub.base).await;
        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/publish"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
        assert!(err["error"].as_str().unwrap().contains("publication"), "{err}");

        // A post this vault does not hold is 404, not a conflict: there is
        // nothing to publish and nothing the member can change.
        set_publication(&router).await;
        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/zines/{}/publish", "c".repeat(64)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
    }

    /// The whole point of the lane: what goes out is a title, a plaintext body,
    /// the sections as tags and a date, and nothing else. The record shape has
    /// nowhere to put the private half, so the assertion is on the bytes the PDS
    /// was actually handed.
    #[tokio::test]
    async fn a_publish_writes_a_document_and_the_private_half_stays_home() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let stub = stub_pds(dir.path(), HashMap::new(), HashMap::new()).await;
        connect(&router, &stub.base).await;
        set_publication(&router).await;
        let zine_id = write_a_zine(&router).await;

        let (status, published) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/publish"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{published}");
        assert_eq!(published["published"], true, "{published}");
        assert_eq!(published["publish_error"], "");

        let rkey = published["rkey"].as_str().unwrap().to_string();
        assert_eq!(rkey.len(), 32, "the record key is derived, not the zine id");
        assert_eq!(
            published["uri"],
            format!("at://did:plc:writer/site.standard.document/{rkey}")
        );
        assert_eq!(
            published["url"],
            format!("https://ada.example/{rkey}"),
            "the reader's link is the publication's address and the path"
        );

        // The record itself, as the PDS received it.
        let written = stub.written.lock().unwrap().clone();
        assert_eq!(written.len(), 2, "the publication, then the document");
        let record = &written[1];
        assert_eq!(record["collection"], "site.standard.document");
        assert_eq!(record["record"]["$type"], "site.standard.document");
        assert_eq!(record["record"]["title"], "On relays");
        assert_eq!(
            record["record"]["textContent"], "hello world",
            "the body arrives reduced to plaintext, with no markup left"
        );
        assert_eq!(record["record"]["tags"][0], "rust", "a section is normalized");
        assert_eq!(
            record["record"]["site"],
            "at://did:plc:writer/site.standard.publication/self"
        );
        assert!(
            record["record"].get("updatedAt").is_none(),
            "a first publish has no update time: {record}"
        );
        let encoded = record.to_string();
        for private in [
            "pen", "zine_id", "read_key", "revision", "prev_hash", "comment", "read key",
        ] {
            assert!(
                !encoded.contains(private),
                "{private} reached the public record: {encoded}"
            );
        }

        // The local half remembers where it went, so a row can link to it.
        assert_eq!(
            published["zines"][0]["public"]["uri"],
            published["uri"],
            "the index carries the public address back to the UI"
        );
        assert_eq!(published["zines"][0]["public"]["did"], "did:plc:writer");

        // And it survives a restart, because it lives in the vault and not in
        // the response.
        let (status, index) = request(&router, "GET", "/api/zines", None).await;
        assert_eq!(status, StatusCode::OK, "{index}");
        assert_eq!(index["zines"][0]["public"]["uri"], published["uri"]);
    }

    /// A re-publish replaces the record rather than adding one beside it, and an
    /// edit does not claim the post was written later than it was.
    #[tokio::test]
    async fn a_re_publish_replaces_the_record_and_holds_the_first_publish_time() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let stub = stub_pds(dir.path(), HashMap::new(), HashMap::new()).await;
        connect(&router, &stub.base).await;
        set_publication(&router).await;
        let zine_id = write_a_zine(&router).await;

        let (_, first) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/publish"),
            None,
        )
        .await;
        // Edit, then publish again.
        let (status, edited) = request(
            &router,
            "POST",
            "/api/zines",
            Some(&format!(
                r#"{{"zine_id":"{zine_id}","title":"On relays","body":"<p>hello again</p>"}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        let (status, second) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/publish"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{second}");

        assert_eq!(second["rkey"], first["rkey"], "one record, replaced in place");
        assert_eq!(second["uri"], first["uri"]);
        let written = stub.written.lock().unwrap().clone();
        let first_doc = written[1]["record"].clone();
        let second_doc = written[2]["record"].clone();
        assert_eq!(
            second_doc["publishedAt"], first_doc["publishedAt"],
            "the first publish time is held: an edit is not a new post"
        );
        assert_eq!(second_doc["textContent"], "hello again");
        assert!(
            second_doc["updatedAt"].is_string(),
            "the revision's own time goes in updatedAt: {second_doc}"
        );
        // One document write per publish, and the publication was written once.
        assert_eq!(written.len(), 3);
    }

    /// A handle is what a member reads and a DID is what addresses a repo, so
    /// the route crosses that gap against the PDS this vault already speaks to —
    /// and a DID needs nobody's help, so pasting one does not require an account.
    #[tokio::test]
    async fn follows_resolve_a_handle_against_the_connected_pds_and_a_did_needs_nobody() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let stub = stub_pds(dir.path(), HashMap::new(), HashMap::new()).await;

        // A DID is the answer already: no account, no request.
        let (status, view) = request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"did":"did:plc:known","name":"Known"}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["follows"][0]["did"], "did:plc:known");
        assert_eq!(view["follows"][0]["name"], "Known");

        // A handle needs a PDS to ask, and there is not one without an account.
        let (status, err) = request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"handle":"ada.test"}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");

        connect(&router, &stub.base).await;
        let (status, view) = request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"handle":"@ada.test"}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(
            view["follows"][0]["did"], "did:plc:ada.test",
            "the `@` is a display habit, not part of a name"
        );
        assert_eq!(view["follows"][0]["handle"], "@ada.test");

        // The whole list is replaced, because that is what a curated lane is.
        let (_, view) = request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"did":"did:plc:one"},{"did":"did:plc:one"}]}"#),
        )
        .await;
        assert_eq!(view["follows"].as_array().unwrap().len(), 1, "deduplicated");

        // Over the cap is refused with a reason, not silently truncated.
        let many: Vec<String> = (0..hush_atproto::FOLLOWS_CAP + 1)
            .map(|i| format!(r#"{{"did":"did:plc:{i}"}}"#))
            .collect();
        let (status, err) = request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(&format!(r#"{{"follows":[{}]}}"#, many.join(","))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert!(err["error"].as_str().unwrap().contains("at most"), "{err}");
    }

    /// The read half: every followed repo is read from its own PDS, cached, and
    /// the failures are reported per publication rather than raised, because one
    /// unreachable host must not cost the member the rest of the lane.
    #[tokio::test]
    async fn a_refresh_reads_every_followed_repo_and_reports_what_it_could_not() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let mut repos = HashMap::new();
        repos.insert("did:plc:one".to_string(), vec![document("One", 100, &["Rust"])]);
        repos.insert(
            "did:plc:two".to_string(),
            vec![document("Two", 200, &["rust", "notes"]), document("Three", 50, &[])],
        );
        let mut foreign = HashMap::new();
        // A PDS nothing is listening on: reading this follow must fail alone.
        foreign.insert("did:plc:down".to_string(), "http://127.0.0.1:1".to_string());
        let stub = stub_pds(dir.path(), repos, foreign).await;

        let (status, view) = request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(
                r#"{"follows":[
                    {"did":"did:plc:one","handle":"one.test"},
                    {"did":"did:plc:two","handle":"two.test"},
                    {"did":"did:plc:down","handle":"down.test"}
                ]}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");

        let (status, refreshed) = request(&router, "POST", "/api/atproto/refresh", None).await;
        assert_eq!(status, StatusCode::OK, "{refreshed}");
        assert_eq!(refreshed["changed"], 3, "{refreshed}");
        let failed = refreshed["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1, "{refreshed}");
        assert!(
            failed[0].as_str().unwrap().starts_with("down.test: "),
            "{refreshed}"
        );

        // The index is newest first across follows, and carries a taste rather
        // than a body: a page of fifty full articles is megabytes for a screen
        // that shows titles.
        let titles: Vec<&str> = refreshed["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["Two", "One", "Three"]);
        assert_eq!(refreshed["documents"][0]["preview"], "The body of Two.");
        assert!(refreshed["documents"][0].get("text").is_none(), "no body in the index");

        // The tag facet counts case-insensitively and is most-used first.
        let tags: Vec<(String, u64)> = refreshed["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["name"].as_str().unwrap().to_string(), t["count"].as_u64().unwrap()))
            .collect();
        assert_eq!(tags, vec![("rust".to_string(), 2), ("notes".to_string(), 1)]);

        // A second pass over unchanged repos is not a change, which is what
        // makes a poll cheap enough to run on a timer.
        let (_, again) = request(&router, "POST", "/api/atproto/refresh", None).await;
        assert_eq!(again["changed"], 0);

        // The body is one request away, served from the cache: opening a page
        // costs no network and no PDS learns which article was opened.
        let uri = refreshed["documents"][0]["uri"].as_str().unwrap();
        let (status, full) = request(
            &router,
            "GET",
            &format!("/api/atproto/document?uri={}", url_encode(uri)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{full}");
        assert_eq!(full["title"], "Two");
        assert_eq!(full["text"], "The body of Two.");
        assert_eq!(
            full["handle"], "two.test",
            "the handle comes from the follow, because a record carries none"
        );

        let (status, err) = request(
            &router,
            "GET",
            "/api/atproto/document?uri=at%3A%2F%2Fnope",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
        assert!(err["error"].as_str().unwrap().contains("no document"), "{err}");

        let _ = stub;
    }

    /// A **pointer** document: the record carries a title, a description and a
    /// `path`, and the words live on the author's own site rather than in the
    /// record. Measured live, 48 of the 105 documents in one vault's cache were
    /// this shape and every one opened as a title with nothing under it, because
    /// only `textContent` and `content` were read and the `path` was never
    /// joined to the publication's `url`.
    #[tokio::test]
    async fn a_pointer_document_becomes_a_link_instead_of_a_bare_title() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let mut repos = HashMap::new();
        repos.insert(
            "did:plc:pointer".to_string(),
            vec![serde_json::json!({
                "title": "The space to breathe",
                "description": "A short note about slowing down.",
                "path": "/the-space-to-breathe",
                "site": "at://did:plc:pointer/site.standard.publication/0",
                "publishedAt": hush_atproto::rfc3339(100_000),
            })],
        );
        let stub = stub_pds(dir.path(), repos, HashMap::new()).await;
        // The publication carries the half the document does not.
        stub.publications.lock().unwrap().insert(
            "did:plc:pointer".to_string(),
            vec![serde_json::json!({
                "name": "Ben Werdmuller",
                "url": "https://werd.io",
            })],
        );

        request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"did":"did:plc:pointer","handle":"werd.test"}]}"#),
        )
        .await;

        let (status, refreshed) = request(&router, "POST", "/api/atproto/refresh", None).await;
        assert_eq!(status, StatusCode::OK, "{refreshed}");
        assert_eq!(refreshed["changed"], 1, "{refreshed}");

        // The index row has something to show, from the author's own words.
        assert_eq!(
            refreshed["documents"][0]["preview"],
            "A short note about slowing down."
        );
        // And the reader has somewhere to send the member, resolved at ingest.
        assert_eq!(
            refreshed["documents"][0]["url"],
            "https://werd.io/the-space-to-breathe"
        );

        // The body is genuinely absent — this is not a parse failure, it is the
        // record's shape, and the full view says so rather than pretending.
        let uri = refreshed["documents"][0]["uri"].as_str().unwrap();
        let (status, full) = request(
            &router,
            "GET",
            &format!("/api/atproto/document?uri={}", url_encode(uri)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{full}");
        assert_eq!(full["text"], "");
        assert_eq!(full["url"], "https://werd.io/the-space-to-breathe");
    }

    /// The same record with no publication to resolve against: the body is still
    /// absent, and the link is left empty rather than guessed. A wrong link sends
    /// a reader somewhere the author never published.
    #[tokio::test]
    async fn a_pointer_document_with_no_readable_publication_has_no_link() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let mut repos = HashMap::new();
        repos.insert(
            "did:plc:pointer".to_string(),
            vec![serde_json::json!({
                "title": "Orphaned",
                "path": "/orphaned",
                "site": "at://did:plc:pointer/site.standard.publication/0",
                "publishedAt": hush_atproto::rfc3339(100_000),
            })],
        );
        let _stub = stub_pds(dir.path(), repos, HashMap::new()).await;

        request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"did":"did:plc:pointer"}]}"#),
        )
        .await;

        let (status, refreshed) = request(&router, "POST", "/api/atproto/refresh", None).await;
        assert_eq!(status, StatusCode::OK, "{refreshed}");
        assert_eq!(refreshed["changed"], 1, "{refreshed}");
        assert_eq!(refreshed["documents"][0]["url"], "");
        assert_eq!(refreshed["documents"][0]["preview"], "");
    }

    /// A third content shape, read from the live network: the words of a block in
    /// an `html` field rather than a `plaintext` one. Two documents in one
    /// vault's cache were exactly this and both opened with nothing under the
    /// title.
    #[tokio::test]
    async fn an_html_content_block_reaches_the_reader() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let mut repos = HashMap::new();
        repos.insert(
            "did:plc:idno".to_string(),
            vec![serde_json::json!({
                "title": "Inkwell",
                "content": { "$type": "co.idno.html",
                    "html": "Inkwell is a <b>very</b> nicely implemented fediverse journaling platform." },
                "publishedAt": hush_atproto::rfc3339(100_000),
            })],
        );
        let _stub = stub_pds(dir.path(), repos, HashMap::new()).await;

        request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"did":"did:plc:idno"}]}"#),
        )
        .await;

        let (status, refreshed) = request(&router, "POST", "/api/atproto/refresh", None).await;
        assert_eq!(status, StatusCode::OK, "{refreshed}");
        assert_eq!(
            refreshed["documents"][0]["preview"],
            "Inkwell is a very nicely implemented fediverse journaling platform."
        );
    }

    /// Disconnecting drops the credential and nothing else. The reading list is
    /// not a secret, and clearing it would cost a member their lane for
    /// disconnecting an account they may reconnect a minute later.
    #[tokio::test]
    async fn disconnecting_forgets_the_session_and_keeps_the_reading_list() {
        let dir = TempDir::new().unwrap();
        let router = api_router(unlocked_app(dir.path()).await);
        let stub = stub_pds(dir.path(), HashMap::new(), HashMap::new()).await;
        connect(&router, &stub.base).await;
        request(
            &router,
            "POST",
            "/api/atproto/follows",
            Some(r#"{"follows":[{"did":"did:plc:known","name":"Known"}]}"#),
        )
        .await;

        let (status, view) = request(&router, "POST", "/api/atproto/disconnect", None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert!(view["account"].is_null(), "the credential is gone");
        assert!(view["publication"].is_null(), "and the blog it wrote to");
        assert_eq!(view["follows"].as_array().unwrap().len(), 1, "the list is not a credential");

        // With no account there is nothing to write as, and the refusal says so.
        let zine_id = write_a_zine(&router).await;
        let (status, err) = request(
            &router,
            "POST",
            &format!("/api/zines/{zine_id}/publish"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
    }

    #[tokio::test]
    async fn every_lane_route_is_401_while_locked() {
        let dir = TempDir::new().unwrap();
        let router = api_router(build_app(dir.path(), AccountWorker::spawn(), false));
        for (method, uri, body) in [
            ("GET", "/api/atproto".to_string(), None),
            (
                "POST",
                "/api/atproto/connect".to_string(),
                Some(r#"{"pds":"https://bsky.social","identifier":"a","password":"b"}"#),
            ),
            ("POST", "/api/atproto/disconnect".to_string(), None),
            (
                "POST",
                "/api/atproto/publication".to_string(),
                Some(r#"{"name":"n","url":"https://a.example"}"#),
            ),
            (
                "POST",
                "/api/atproto/follows".to_string(),
                Some(r#"{"follows":[{"did":"did:plc:x"}]}"#),
            ),
            ("POST", "/api/atproto/refresh".to_string(), None),
            ("GET", "/api/atproto/document?uri=at%3A%2F%2Fx".to_string(), None),
            ("POST", format!("/api/zines/{}/publish", "a".repeat(64)), None),
        ] {
            let (status, _) = request(&router, method, &uri, body).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    /// Percent-encode a query value, so a `?` or a `/` in a URI cannot split the
    /// request line.
    fn url_encode(value: &str) -> String {
        value
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
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

/// One daemon per vault, and the lock is what enforces it.
///
/// The socket check cannot stand in for this: a daemon that is alive but not
/// answering looks dead, and the second daemon unlinks its socket and takes
/// over. These assert the lock itself rather than the socket path, and they are
/// `unix` rather than `linux` because macOS is where the lock was missing — a
/// force-quit there left a daemon behind that the next launch could not see.
#[cfg(unix)]
mod vault_lock_tests {
    use super::*;

    #[test]
    fn the_lock_is_a_held_file_beside_the_vault() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let held = acquire_vault_lock(dir.path()).expect("first daemon takes the lock");

        assert!(
            held.is_some(),
            "acquire_vault_lock returned a no-op: this platform has flock and has to use it"
        );
        assert!(dir.path().join(".hush-daemon.lock").exists());
    }

    #[test]
    fn a_second_daemon_on_one_vault_is_refused_by_name() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let _first = acquire_vault_lock(dir.path()).expect("first daemon takes the lock");
        let second = acquire_vault_lock(dir.path())
            .expect_err("a second daemon on one vault must not be allowed to start");

        assert!(
            second.contains("another hush-daemon is already running"),
            "the refusal has to say what is wrong, got: {second}"
        );
    }

    #[test]
    fn a_daemon_that_exits_leaves_no_stale_lock() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let held = acquire_vault_lock(dir.path()).expect("first daemon takes the lock");
        drop(held);

        let next = acquire_vault_lock(dir.path()).expect("a dead daemon leaves no stale lock");
        assert!(next.is_some());
    }
}

//! The Tauri command surface for the IRC pane.
//!
//! Names match the plan: `connect_server`, `disconnect_server`, `join_channel`,
//! `part_channel`, `send_message`, `send_raw`, `change_nick`, `open_query`,
//! `list_channels`, `get_state`, plus settings load/save. Operations that are
//! just a raw line (`/whois`, `/mode`, `/kick`, ...) are built by the frontend
//! and sent through `send_raw`.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};

use crate::irc::client;
use crate::irc::settings::{self, IrcSettings, SettingsBundle};
use crate::irc::state::ServerSnapshot;
use crate::irc::{ConnectRequest, IrcState, OutCmd, ServerEntry};
use crate::tor::TorManager;

/// Open a connection. Returns the server id the UI uses for every later call
/// and every event. The connection runs in the background; failures surface as
/// `irc://error`/`irc://server` events, not as this call's result.
#[tauri::command]
pub async fn connect_server(
    app: AppHandle,
    state: State<'_, IrcState>,
    tor: State<'_, TorManager>,
    request: ConnectRequest,
) -> Result<String, String> {
    let cfg = request.into_config();
    if state.snapshot(&cfg.id).is_some() {
        return Err(format!("already connected to {}", cfg.id));
    }

    let snapshot = Arc::new(Mutex::new(ServerSnapshot {
        id: cfg.id.clone(),
        host: cfg.host.clone(),
        port: cfg.port,
        tls: cfg.tls,
        use_tor: cfg.use_tor,
        nick: cfg.nick.clone(),
        phase: "Connecting".into(),
        ..Default::default()
    }));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<OutCmd>();
    state.insert(ServerEntry {
        snapshot: snapshot.clone(),
        out: tx,
        config: cfg.clone(),
    });

    let id = cfg.id.clone();
    let app_handle = app.clone();
    let tor_handle = tor.inner().clone();
    tauri::async_runtime::spawn(async move {
        client::run(app_handle, snapshot, cfg, tor_handle, rx).await;
    });

    Ok(id)
}

/// Close a connection and forget it. A `reason` sends a proper `QUIT` first;
/// without one the socket is dropped, and in neither case does the client
/// reconnect.
#[tauri::command]
pub fn disconnect_server(
    state: State<'_, IrcState>,
    server_id: String,
    reason: Option<String>,
) -> Result<(), String> {
    if let Some(entry) = state.remove(&server_id) {
        let cmd = match reason.filter(|r| !r.trim().is_empty()) {
            Some(r) => OutCmd::Quit(Some(r)),
            None => OutCmd::Disconnect,
        };
        let _ = entry.out.send(cmd);
    }
    Ok(())
}

#[tauri::command]
pub fn join_channel(
    state: State<'_, IrcState>,
    server_id: String,
    channel: String,
    key: Option<String>,
) -> Result<(), String> {
    state.send(&server_id, OutCmd::Join { channel, key })
}

#[tauri::command]
pub fn part_channel(
    state: State<'_, IrcState>,
    server_id: String,
    channel: String,
    reason: Option<String>,
) -> Result<(), String> {
    state.send(&server_id, OutCmd::Part { channel, reason })
}

/// Send a message. `action` wraps it as a CTCP ACTION (`/me`).
#[tauri::command]
pub fn send_message(
    state: State<'_, IrcState>,
    server_id: String,
    target: String,
    text: String,
    action: Option<bool>,
) -> Result<(), String> {
    state.send(
        &server_id,
        OutCmd::Privmsg {
            target,
            text,
            action: action.unwrap_or(false),
        },
    )
}

/// Send a raw IRC line, for the many commands that need no backend logic.
#[tauri::command]
pub fn send_raw(state: State<'_, IrcState>, server_id: String, line: String) -> Result<(), String> {
    state.send(&server_id, OutCmd::Raw(line))
}

#[tauri::command]
pub fn change_nick(
    state: State<'_, IrcState>,
    server_id: String,
    nick: String,
) -> Result<(), String> {
    state.send(&server_id, OutCmd::Nick(nick))
}

/// Record that a query window is open. The window itself is the frontend's;
/// this keeps `get_state` honest across a reload.
#[tauri::command]
pub fn open_query(
    state: State<'_, IrcState>,
    server_id: String,
    nick: String,
) -> Result<(), String> {
    state.update(&server_id, |s| {
        s.ensure_query(&nick);
    })
}

/// Ask the server for its channel list; the reply arrives as `irc://list`
/// events followed by `irc://list_end`.
#[tauri::command]
pub fn list_channels(state: State<'_, IrcState>, server_id: String) -> Result<(), String> {
    state.send(&server_id, OutCmd::Raw("LIST".into()))
}

/// Every server's snapshot, for the initial paint and after a reload.
#[tauri::command]
pub fn get_state(state: State<'_, IrcState>) -> Vec<ServerSnapshot> {
    state.all()
}

#[tauri::command]
pub fn irc_settings_load(app: AppHandle) -> SettingsBundle {
    SettingsBundle {
        settings: settings::load(&app),
        defaults: settings::IrcDefaults::current(),
    }
}

#[tauri::command]
pub fn irc_settings_save(app: AppHandle, settings: IrcSettings) -> Result<(), String> {
    settings::save(&app, &settings)
}

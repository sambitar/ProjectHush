use tauri::Manager;

mod daemon_client;
mod drops;
mod sound;
mod tor;

use daemon_client::{ApiReply, DaemonClient, Transport};
use drops::DropsState;
use tor::TorManager;

/// Holds the socket-backed client; absent in TCP mode, where JS uses `fetch`.
struct ProxyState {
    client: Option<DaemonClient>,
}

fn sanitize_file_name(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() || safe == "." || safe == ".." {
        "download".into()
    } else {
        safe
    }
}

#[tauri::command]
fn hush_save_file(default_name: String, bytes: Vec<u8>) -> Result<(), String> {
    let name = sanitize_file_name(&default_name);
    let path = rfd::FileDialog::new()
        .set_file_name(&name)
        .save_file()
        .ok_or_else(|| "Save cancelled".to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn hush_open_file(file_name: String, bytes: Vec<u8>) -> Result<(), String> {
    let name = sanitize_file_name(&file_name);
    let dir = std::env::temp_dir().join("hush-open");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    open::that(&path).map_err(|e| e.to_string())?;
    Ok(())
}

/// A decoded daemon `MaterializeResponse`.
#[derive(serde::Deserialize)]
struct MaterializeReply {
    path: String,
}

/// Ask the daemon to decrypt a stored blob to a temp file and return its path.
///
/// This is the streaming egress: the daemon writes chunk by chunk, so a
/// multi-GB file never materialises in the webview or crosses IPC as bytes.
async fn materialize_blob(
    state: &tauri::State<'_, ProxyState>,
    id: &str,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    let client = state.client.as_ref().ok_or_else(|| {
        "opening vault files needs the desktop Unix-socket transport".to_string()
    })?;
    let body = serde_json::json!({ "name": name }).to_string();
    let reply = client
        .request("POST", &format!("/api/blob/{id}/materialize"), Some(body))
        .await?;
    if !(200..300).contains(&reply.status) {
        return Err(daemon_error(&reply));
    }
    let parsed: MaterializeReply =
        serde_json::from_str(&reply.body).map_err(|e| format!("bad materialize reply: {e}"))?;
    Ok(std::path::PathBuf::from(parsed.path))
}

/// The daemon's `{ "error": "..." }` body as a human string, or the raw body.
fn daemon_error(reply: &ApiReply) -> String {
    serde_json::from_str::<serde_json::Value>(&reply.body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("daemon returned HTTP {}", reply.status))
}

/// Open a vault blob in the user's OS handler without pulling the bytes into
/// the webview. The daemon materializes the file; the shell opens the path.
#[tauri::command]
async fn hush_open_blob(
    state: tauri::State<'_, ProxyState>,
    id: String,
    name: String,
) -> Result<(), String> {
    let path = materialize_blob(&state, &id, &name).await?;
    open::that(&path).map_err(|e| e.to_string())
}

/// Save a vault blob to a user-chosen path, again without the webview holding
/// the bytes: the daemon materializes, the shell copies to the chosen path.
#[tauri::command]
async fn hush_save_blob(
    state: tauri::State<'_, ProxyState>,
    id: String,
    name: String,
    default_name: String,
) -> Result<(), String> {
    let path = materialize_blob(&state, &id, &name).await?;
    let suggested = if default_name.trim().is_empty() {
        sanitize_file_name(&name)
    } else {
        sanitize_file_name(&default_name)
    };
    let dest = rfd::FileDialog::new()
        .set_file_name(&suggested)
        .save_file()
        .ok_or_else(|| "Save cancelled".to_string())?;
    let copied = std::fs::copy(&path, &dest).map_err(|e| e.to_string());
    // The materialized copy has served its purpose once it is on disk.
    let _ = std::fs::remove_file(&path);
    copied.map(|_| ())
}

/// Open one URL in the user's browser, after the UI's own confirm step.
/// Only http(s) is passed through: the webview must never become a launcher
/// for file:// or other schemes a hostile page might have snuck into a link.
#[tauri::command]
fn hush_open_url(url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|e| format!("bad url: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("refused to open {} link", parsed.scheme()));
    }
    open::that(&url).map_err(|e| e.to_string())
}

/// Proxy one daemon API call over the Unix socket (DR-02).
#[tauri::command]
async fn hush_api(
    state: tauri::State<'_, ProxyState>,
    method: String,
    path: String,
    body: Option<String>,
) -> Result<ApiReply, String> {
    let client = state
        .client
        .as_ref()
        .ok_or_else(|| "daemon proxy unavailable in TCP mode".to_string())?;
    client.request(&method, &path, body).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let transport = daemon_client::transport_from_env();
    let proxy = match &transport {
        Transport::Unix { path } => ProxyState {
            client: Some(DaemonClient::new(std::path::Path::new(path))),
        },
        Transport::Tcp { .. } => ProxyState { client: None },
    };
    let window_title = std::env::var("HUSH_WINDOW_TITLE").unwrap_or_else(|_| "Hush".to_string());
    let display = match &transport {
        Transport::Unix { path } => format!("unix://{}", path),
        Transport::Tcp { base } => base.clone(),
    };
    let init_js = format!(
        "window.__HUSH_API__ = {}; window.__HUSH_TRANSPORT__ = {};",
        serde_json::to_string(&display).expect("api url json"),
        serde_json::to_string(&transport).expect("transport json")
    );

    let app = tauri::Builder::default()
        .manage(proxy)
        .invoke_handler(tauri::generate_handler![
            hush_save_file,
            hush_open_file,
            hush_open_blob,
            hush_save_blob,
            hush_open_url,
            hush_api,
            drops::drops_open,
            drops::drops_close,
            drops::drops_open_state,
            drops::drops_dev_enabled,
            drops::drops_tor_status,
            drops::drops_tor_start,
            drops::drops_tor_stop,
            sound::play_chat_sound
        ])
        .setup(move |app| {
            // Tor lives under the app's local data dir, not in the vault, and is
            // owned by this process. It is managed at the app level: the relay
            // switch keeps it alive for the whole session, and the Drop view
            // shares the same process rather than owning it.
            let data_dir = app
                .path()
                .app_local_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("hush-tauri"))
                .join("tor");
            let resource_dir = app.path().resource_dir().ok();
            app.manage(TorManager::new(data_dir, resource_dir));
            app.manage(DropsState::new());

            if let Some(window) = app.get_webview_window("main") {
                window.set_title(&window_title)?;
                window.eval(&init_js)?;
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(tor) = app_handle.try_state::<TorManager>() {
                tor.stop();
            }
        }
    });
}

use tauri::Manager;

mod daemon_client;

use daemon_client::{ApiReply, DaemonClient, Transport};

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

    tauri::Builder::default()
        .manage(proxy)
        .invoke_handler(tauri::generate_handler![
            hush_save_file,
            hush_open_file,
            hush_api
        ])
        .setup(move |app| {
            if let Some(window) = app.get_webview_window("main") {
                window.set_title(&window_title)?;
                window.eval(&init_js)?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

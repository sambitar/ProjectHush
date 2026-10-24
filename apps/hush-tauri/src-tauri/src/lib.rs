use tauri::Manager;

#[cfg(desktop)]
mod daemon_client;
#[cfg(desktop)]
mod daemon_supervisor;
#[cfg(desktop)]
mod drops;
// The phone backend compiles on the host under `test` so its vault lifecycle
// can be exercised without an emulator.
#[cfg(any(target_os = "android", target_os = "ios", test))]
mod mobile;
mod sound;
#[cfg(desktop)]
mod tor;

#[cfg(desktop)]
use daemon_client::{ApiReply, DaemonClient, Transport};
#[cfg(desktop)]
use daemon_supervisor::{DaemonState, Plan};
#[cfg(desktop)]
use drops::DropsState;
#[cfg(desktop)]
use tor::TorManager;

/// Holds the socket-backed client; absent in TCP mode, where JS uses `fetch`.
#[cfg(desktop)]
struct ProxyState {
    client: Option<DaemonClient>,
}

#[cfg(desktop)]
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

#[cfg(desktop)]
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

#[cfg(desktop)]
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
#[cfg(desktop)]
#[derive(serde::Deserialize)]
struct MaterializeReply {
    path: String,
}

/// Ask the daemon to decrypt a stored blob to a temp file and return its path.
///
/// This is the streaming egress: the daemon writes chunk by chunk, so a
/// multi-GB file never materialises in the webview or crosses IPC as bytes.
#[cfg(desktop)]
async fn materialize_blob(
    state: &tauri::State<'_, ProxyState>,
    id: &str,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    let client = state
        .client
        .as_ref()
        .ok_or_else(|| "opening vault files needs the desktop Unix-socket transport".to_string())?;
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
#[cfg(desktop)]
fn daemon_error(reply: &ApiReply) -> String {
    serde_json::from_str::<serde_json::Value>(&reply.body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("daemon returned HTTP {}", reply.status))
}

/// Open a vault blob in the user's OS handler without pulling the bytes into
/// the webview. The daemon materializes the file; the shell opens the path.
#[cfg(desktop)]
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
#[cfg(desktop)]
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

/// One file the desktop picker returned.
#[cfg(desktop)]
#[derive(serde::Serialize)]
struct PickedVaultFile {
    path: String,
    name: String,
    size: u64,
}

/// Pick files from this computer for the vault's own file library.
///
/// Only the path crosses into the webview. The daemon opens and encrypts the
/// file itself, so a multi-GB import never becomes a JS string or a Tauri IPC
/// payload. Cancelling is an empty list, not an error.
#[cfg(desktop)]
#[tauri::command]
fn hush_pick_vault_files() -> Result<Vec<PickedVaultFile>, String> {
    let Some(paths) = rfd::FileDialog::new().pick_files() else {
        return Ok(Vec::new());
    };
    Ok(paths
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().to_string();
            let size = std::fs::metadata(&path).ok()?.len();
            Some(PickedVaultFile {
                path: path.to_string_lossy().to_string(),
                name,
                size,
            })
        })
        .collect())
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
#[cfg(desktop)]
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
    let window_title = std::env::var("HUSH_WINDOW_TITLE").unwrap_or_else(|_| "Hush".to_string());

    #[cfg(desktop)]
    let mut builder = tauri::Builder::default()
        .manage(DaemonState::default())
        .invoke_handler(tauri::generate_handler![
            hush_save_file,
            hush_open_file,
            hush_open_blob,
            hush_save_blob,
            hush_pick_vault_files,
            hush_open_url,
            hush_api,
            drops::drops_open,
            drops::drops_close,
            drops::drops_open_state,
            drops::drops_dev_enabled,
            drops::drops_tor_status,
            drops::drops_tor_start,
            drops::drops_tor_stop,
            sound::play_chat_sound,
            sound::set_voice_cue
        ]);

    // Updating in place is a desktop concern: the mobile targets ship through
    // their own stores, so the plugins are compiled and registered only here.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    // Android runs no daemon process (docs/MOBILE_APP_PLAN.md §5.1), so its
    // handler list is the in-process vault API plus the controls that really do
    // work on a phone. File dialogs and Drops stay desktop-only rather than
    // being registered as buttons that cannot work here.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let builder = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            hush_open_url,
            sound::play_chat_sound,
            sound::set_voice_cue,
            mobile::hush_mobile_api
        ])
        // The phone has no socket to name. Inject on every page load: the
        // WebView navigates to the app URL after `setup`, which discards an
        // `eval` issued there.
        .on_page_load(|webview, _payload| {
            if let Some(state) = webview.try_state::<mobile::MobileBackend>() {
                let stun = state.stun_url().map(str::to_string);
                let _ = webview.eval(&mobile::init_js(stun.as_deref(), state.turn_config()));
            }
        });

    // Calls need the microphone on every target. WebKit asks the app first and
    // then the OS; without a handler the media request never settles on Linux,
    // which is indistinguishable from a call button that does nothing. Allow
    // the microphone only, and leave every other kind on the platform default.
    let builder = builder.on_permission_request(|_webview, kind| match kind {
        tauri::webview::PermissionKind::Microphone => tauri::webview::PermissionResponse::Allow,
        _ => tauri::webview::PermissionResponse::Default,
    });

    let app = builder
        .setup(move |app| {
            #[cfg(desktop)]
            {
                // The daemon comes first, because the transport the window is told
                // about depends on whether this process starts one. A bundled copy
                // has a daemon staged beside its executable and no run script behind
                // it, so it starts the daemon here; a repository run is handed
                // `HUSH_API_URL` by `scripts/run-one-ui.sh` and keeps that.
                let transport = match daemon_supervisor::plan_for_app(app.handle()) {
                    Plan::External => daemon_client::transport_from_env(),
                    Plan::Sidecar { bin, socket, vault } => {
                        let log_dir = socket
                            .parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_else(std::env::temp_dir);
                        match daemon_supervisor::Daemon::start(&bin, &socket, &vault, &log_dir) {
                            Ok(mut daemon) => {
                                // The window opens either way; a daemon that never
                                // answers leaves its reason in daemon.log, and the
                                // UI reports the socket as unreachable.
                                if !daemon.wait_ready(daemon_supervisor::READY_TIMEOUT) {
                                    eprintln!(
                                        "hush: the bundled daemon did not answer on {}; \
                                         see {}",
                                        socket.display(),
                                        log_dir.join("daemon.log").display()
                                    );
                                }
                                let socket = daemon.socket().display().to_string();
                                if let Some(state) = app.try_state::<DaemonState>() {
                                    if let Ok(mut slot) = state.0.lock() {
                                        *slot = Some(daemon);
                                    }
                                }
                                Transport::Unix { path: socket }
                            }
                            Err(why) => {
                                eprintln!(
                                    "hush: {why}; the window will report the daemon unreachable"
                                );
                                Transport::Unix {
                                    path: socket.display().to_string(),
                                }
                            }
                        }
                    }
                };

                match &transport {
                    Transport::Unix { path } => app.manage(ProxyState {
                        client: Some(DaemonClient::new(std::path::Path::new(path))),
                    }),
                    Transport::Tcp { .. } => app.manage(ProxyState { client: None }),
                };

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
                    let display = match &transport {
                        Transport::Unix { path } => format!("unix://{}", path),
                        Transport::Tcp { base } => base.clone(),
                    };
                    let init_js = format!(
                        "window.__HUSH_API__ = {}; window.__HUSH_TRANSPORT__ = {};",
                        serde_json::to_string(&display).expect("api url json"),
                        serde_json::to_string(&transport).expect("transport json")
                    );
                    window.set_title(&window_title)?;
                    window.eval(&init_js)?;
                }
            }

            #[cfg(any(target_os = "android", target_os = "ios"))]
            {
                // The phone's vault lives in app-private storage. Android's
                // backup rules must exclude it (docs/MOBILE_APP_PLAN.md §8).
                let dir = app
                    .path()
                    .app_local_data_dir()
                    .unwrap_or_else(|_| std::env::temp_dir().join("hush-mobile"))
                    .join("vault");
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    eprintln!("hush: could not create {}: {e}", dir.display());
                }
                app.manage(mobile::MobileBackend::spawn(dir));

                if let Some(window) = app.get_webview_window("main") {
                    window.set_title(&window_title)?;
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        #[cfg(desktop)]
        {
            if let tauri::RunEvent::Exit = event {
                if let Some(tor) = app_handle.try_state::<TorManager>() {
                    tor.stop();
                }
                // The daemon this process started goes with it: left running it would
                // hold the vault lock, and the next launch could not unlock.
                if let Some(state) = app_handle.try_state::<DaemonState>() {
                    if let Ok(mut slot) = state.0.lock() {
                        if let Some(mut daemon) = slot.take() {
                            daemon.stop();
                        }
                    }
                }
            }
        }
        #[cfg(not(desktop))]
        {
            // The in-process backend owns no child process and needs no teardown.
            let _ = (app_handle, event);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// The command names in every `generate_handler![...]` list, with any
    /// `module::` prefix stripped — a permission names the command alone.
    ///
    /// There is one list per target now (desktop and mobile), so every list is
    /// read; stopping at the first would let a phone-only command ship without a
    /// permission.
    fn registered_commands(lib_rs: &str) -> BTreeSet<String> {
        // Only the production half: the parser's own source below mentions the
        // macro name in strings, and scanning the test module would read those.
        let production = lib_rs.split("#[cfg(test)]").next().unwrap_or(lib_rs);
        let mut commands = BTreeSet::new();
        let mut rest = production;
        let mut lists = 0;
        while let Some(open) = rest.find("generate_handler![") {
            let body = &rest[open..];
            let close = body
                .find("])")
                .expect("a generate_handler list ends with `])`");
            for line in body[..close].lines().skip(1) {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                commands.insert(
                    line.trim_end_matches(',')
                        .rsplit("::")
                        .next()
                        .expect("rsplit always yields one item")
                        .trim()
                        .to_string(),
                );
            }
            lists += 1;
            rest = &body[close + 2..];
        }
        assert!(
            lists > 0,
            "lib.rs registers its commands with generate_handler!"
        );
        commands
    }

    /// The commands named by every `commands.allow` under `permissions/`.
    ///
    /// Hand-parsed rather than with a TOML crate: the value is a flat array of
    /// quoted strings, and the shape is asserted below so a parser that silently
    /// matched nothing fails loudly instead of passing. A file may hold several
    /// `[[permission]]` blocks, each with its own array, so every array is read
    /// and not just the first — reading one made a permission that lives in a
    /// second block invisible to this guard.
    fn permitted_commands(dir: &Path) -> BTreeSet<String> {
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("permissions/ at {dir:?} is readable: {e}"))
            .map(|entry| entry.expect("a readable directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no permission files found in {dir:?}");

        let mut allowed = BTreeSet::new();
        for path in files {
            let text = fs::read_to_string(&path).expect("a permission file reads");
            let mut rest = text.as_str();
            let mut arrays = 0;
            while let Some(start) = rest.find("commands.allow") {
                let open = rest[start..]
                    .find('[')
                    .map(|i| start + i)
                    .unwrap_or_else(|| panic!("{}: commands.allow has no array", path.display()));
                let close = rest[open..].find(']').map(|i| open + i).unwrap_or_else(|| {
                    panic!("{}: commands.allow array is unclosed", path.display())
                });

                let mut named = 0;
                for entry in rest[open + 1..close].split('"').skip(1).step_by(2) {
                    let name = entry.trim();
                    assert!(
                        !name.is_empty(),
                        "{}: an empty command name in commands.allow",
                        path.display()
                    );
                    allowed.insert(name.to_string());
                    named += 1;
                }
                assert!(
                    named > 0,
                    "{}: a commands.allow array parsed to nothing — the parser is out of \
                     date with the file, not the file with the parser",
                    path.display()
                );
                arrays += 1;
                rest = &rest[close + 1..];
            }
            assert!(
                arrays > 0,
                "{}: no commands.allow — a permission file must name the commands it allows",
                path.display()
            );
        }
        allowed
    }

    /// Tauri refuses a command the webview was never granted, with
    /// `"command not found"` — at click time, in the UI only. Nothing else
    /// catches it: the command is registered, the frontend type-checks, and
    /// `cargo test` has no opinion. This has now shipped more than once (Drops,
    /// the chat sound), so it is checked mechanically.
    #[test]
    fn every_registered_command_is_named_by_a_permission() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let lib_rs = fs::read_to_string(manifest.join("src/lib.rs")).expect("lib.rs reads");
        let registered = registered_commands(&lib_rs);
        let permitted = permitted_commands(&manifest.join("permissions"));

        let ungranted: Vec<&String> = registered.difference(&permitted).collect();
        assert!(
            ungranted.is_empty(),
            "these commands are registered but no permission allows them, so the \
             webview gets \"command not found\" when it calls them: {ungranted:?}"
        );
        assert!(
            registered.len() > 10,
            "the handler list parsed to {} entries — the parser is out of date",
            registered.len()
        );
    }
}

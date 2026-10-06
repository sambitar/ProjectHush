use tauri::Manager;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let api_url = std::env::var("HUSH_API_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9876".to_string());
    let window_title = std::env::var("HUSH_WINDOW_TITLE").unwrap_or_else(|_| "Hush".to_string());
    let init_js = format!(
        "window.__HUSH_API__ = {};",
        serde_json::to_string(&api_url).expect("api url json")
    );

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![hush_save_file, hush_open_file])
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

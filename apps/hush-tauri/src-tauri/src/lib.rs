use tauri::Manager;

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

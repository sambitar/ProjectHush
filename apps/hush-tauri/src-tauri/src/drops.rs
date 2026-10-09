//! Drops: anonymous submission destinations opened in an isolated, Tor-routed
//! window.
//!
//! A Drop is an external website (typically a SecureDrop `.onion`) that the user
//! opens in a dedicated window. The window is created with three properties that
//! together keep it out of Hush:
//!
//! 1. **No capability.** Tauri scopes command access to window labels, and the
//!    `drop-view` window is deliberately absent from every capability, so the
//!    page inside it can call no Hush command.
//! 2. **An ephemeral web context.** `incognito(true)` gives the window its own
//!    throwaway WebKit context, so it shares no cookies, no local storage, and
//!    leaves no browsing history behind.
//! 3. **A per-webview proxy.** `proxy_url` points the window's WebKit network
//!    context at the local Tor SOCKS port. Only this window is proxied; the main
//!    window and the daemon path are untouched.
//!
//! The relay is never involved: the page is fetched by the user's own machine
//! through Tor, not proxied by any Hush server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, Url, WebviewUrl, WebviewWindowBuilder};

use crate::tor::{TorManager, TorPhase, DEFAULT_BOOTSTRAP_TIMEOUT};

/// Window label for the Drop view. Fixed, because there is one Drop at a time;
/// opening a new one replaces the old.
pub const DROP_WINDOW_LABEL: &str = "drop-view";

/// State managed by Tauri for the Drops feature.
///
/// The Tor process is *not* here: it is managed at the app level so the relay
/// switch can keep it alive for the whole session and Drops share the same
/// process. This state only tracks which Drop is open.
pub struct DropsState {
    /// The `(id, name)` of the Drop currently open, so the UI can restore which
    /// card to mark after a re-render. Not persisted: it is session-only.
    pub open: Arc<Mutex<Option<(String, String)>>>,
}

impl DropsState {
    pub fn new() -> Self {
        Self {
            open: Arc::new(Mutex::new(None)),
        }
    }
}

/// What the frontend asks to open. `url` is validated here rather than trusted:
/// the UI is not the security boundary.
#[derive(Debug, Deserialize)]
pub struct OpenDropRequest {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Marks the development/test Drop, which may target a local mock.
    #[serde(default)]
    pub dev: bool,
}

/// A truthful status line for the open Drop, used by the UI panel.
#[derive(Debug, Serialize)]
pub struct DropOpenState {
    pub open: bool,
    pub name: Option<String>,
    pub id: Option<String>,
}

/// True only when the operator has explicitly opted into development Drops.
/// A compromised frontend cannot open a localhost destination without it.
pub fn dev_enabled() -> bool {
    matches!(
        std::env::var("HUSH_DROPS_DEV").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// A v3 onion host: exactly 56 base32 characters followed by `.onion`.
pub fn is_v3_onion(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    match host.strip_suffix(".onion") {
        Some(label) => {
            label.len() == 56
                && label
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
        }
        None => false,
    }
}

/// Accept a destination only if it is safe to open.
///
/// Production: `http` or `https` to a v3 `.onion`. SecureDrop serves its onion
/// over plain `http` — Tor's onion service layer already authenticates the
/// address and encrypts the link end to end, so there is no TLS layer to
/// require, and demanding `https` would refuse every real instance.
/// Development (and only when the operator opted in): `http`/`https` to
/// loopback, so a local mock can stand in for a real SecureDrop.
pub fn validate_target(raw: &str, allow_dev: bool) -> Result<Url, String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|_| {
        "That Drop address is not a valid URL. Your Drop has not been opened.".to_string()
    })?;

    let host = url
        .host_str()
        .ok_or_else(|| "That Drop address has no host. Your Drop has not been opened.".to_string())?
        .to_ascii_lowercase();

    match url.scheme() {
        "http" | "https" => {
            if is_v3_onion(&host) {
                Ok(url)
            } else if allow_dev && is_loopback_host(&host) {
                Ok(url)
            } else {
                Err("Only verified v3 .onion destinations can be opened as Drops.".to_string())
            }
        }
        _ => Err("Drops must use http or https to a .onion address.".to_string()),
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Navigation guard for the Drop window. The proxy is applied to the whole web
/// context, so this is defence in depth rather than the only control: it keeps
/// the top-level document on the onion it was opened for. Non-network schemes
/// (`about:`, `blob:`, `data:`) are allowed because they cannot egress.
fn allowed_navigation(url: &Url, dev: bool) -> bool {
    match url.scheme() {
        "about" | "blob" | "data" => true,
        "http" | "https" => url
            .host_str()
            .map(|h| {
                let host = h.to_ascii_lowercase();
                is_v3_onion(&host) || (dev && is_loopback_host(&host))
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// A small, non-privileged banner injected into the Drop page. It shares the
/// page's JS context but has no access to Tauri, so it can only draw pixels. Its
/// job is to state, honestly, how the traffic is routed.
fn security_banner(dev: bool) -> String {
    let message = if dev {
        "Development test Drop — this is not a real SecureDrop and is not routed through Tor."
    } else {
        "This Drop is routed through Tor. Hush relay is not used."
    };
    // No interpolation of untrusted data; the only input is a boolean.
    format!(
        r#"(function(){{
  var id='__hush_drop_banner__';
  function add(){{
    if(document.getElementById(id)) return;
    var d=document.createElement('div');
    d.id=id;
    d.setAttribute('role','status');
    d.textContent={msg:?};
    d.style.cssText='position:fixed;left:12px;bottom:12px;z-index:2147483647;max-width:420px;padding:8px 12px;border-radius:12px;font:13px/1.4 system-ui,sans-serif;color:#fff;background:rgba(20,20,30,.86);box-shadow:0 4px 16px rgba(0,0,0,.35);pointer-events:none';
    (document.body||document.documentElement).appendChild(d);
  }}
  if(document.readyState==='loading'){{document.addEventListener('DOMContentLoaded',add);}}else{{add();}}
}})();"#,
        msg = message
    )
}

/// Build (or replace) the Drop window. Must run on the main thread.
fn build_drop_window(
    app: &AppHandle,
    req: &OpenDropRequest,
    url: Url,
    proxy_port: Option<u16>,
    dev: bool,
) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(DROP_WINDOW_LABEL) {
        let _ = existing.close();
    }

    let mut builder = WebviewWindowBuilder::new(app, DROP_WINDOW_LABEL, WebviewUrl::External(url))
        .title(format!("Hush — Secure Drop — {}", req.name))
        .inner_size(1024.0, 768.0)
        .min_inner_size(640.0, 480.0)
        // Throwaway context: no shared cookies, no local storage, no history.
        .incognito(true)
        .initialization_script(security_banner(dev))
        .on_navigation(move |u| allowed_navigation(u, dev));

    if let Some(port) = proxy_port {
        let proxy = Url::parse(&format!("socks5://127.0.0.1:{port}"))
            .map_err(|_| "Could not configure the Tor proxy.".to_string())?;
        builder = builder.proxy_url(proxy);
    }

    builder
        .build()
        .map(|_| ())
        .map_err(|e| format!("The Drop window could not be opened: {e}"))
}

/// The full open flow: validate, make sure Tor is ready, then open the window.
///
/// The Tor wait happens off the main thread; window creation is marshalled back
/// onto it, which is where GTK requires it.
#[tauri::command]
pub async fn drops_open(
    app: AppHandle,
    state: State<'_, DropsState>,
    tor: State<'_, TorManager>,
    req: OpenDropRequest,
) -> Result<(), String> {
    let dev = req.dev && dev_enabled();
    let target = validate_target(&req.url, dev)?;

    let proxy_port = if dev {
        None
    } else {
        let tor = tor.inner().clone();
        let status = tauri::async_runtime::spawn_blocking(move || {
            tor.ensure_blocking(DEFAULT_BOOTSTRAP_TIMEOUT)
        })
        .await
        .map_err(|e| e.to_string())?;
        if status.phase != TorPhase::Ready {
            return Err(status
                .message
                .unwrap_or_else(|| "Tor could not connect. Your Drop has not been opened.".into()));
        }
        Some(
            status
                .socks_port
                .ok_or_else(|| "Tor is not listening. Your Drop has not been opened.".to_string())?,
        )
    };

    let drop_id = req.id.clone();
    let drop_name = req.name.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let app_main = app.clone();
    app.run_on_main_thread(move || {
        let result = build_drop_window(&app_main, &req, target, proxy_port, dev);
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;

    let opened = rx
        .recv()
        .map_err(|_| "The Drop window could not be opened.".to_string())?;
    if opened.is_ok() {
        if let Ok(mut slot) = state.open.lock() {
            *slot = Some((drop_id, drop_name));
        }
    }
    opened
}

/// Close the Drop window, if open.
#[tauri::command]
pub fn drops_close(app: AppHandle, state: State<'_, DropsState>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(DROP_WINDOW_LABEL) {
        window.close().map_err(|e| e.to_string())?;
    }
    if let Ok(mut slot) = state.open.lock() {
        *slot = None;
    }
    Ok(())
}

/// Whether the Drop window is currently open, and which Drop it is.
#[tauri::command]
pub fn drops_open_state(app: AppHandle, state: State<'_, DropsState>) -> DropOpenState {
    // The window is the source of truth: it can be closed with its own title-bar
    // control, which we do not get a callback for.
    if app.get_webview_window(DROP_WINDOW_LABEL).is_none() {
        if let Ok(mut slot) = state.open.lock() {
            *slot = None;
        }
        return DropOpenState {
            open: false,
            id: None,
            name: None,
        };
    }
    let (id, name) = state
        .open
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .map(|(id, name)| (Some(id), Some(name)))
        .unwrap_or((None, None));
    DropOpenState {
        open: true,
        id,
        name,
    }
}

/// Whether development/test Drops are enabled on this machine.
#[tauri::command]
pub fn drops_dev_enabled() -> bool {
    dev_enabled()
}

/// Current Tor status, for the indicator and the open flow.
#[tauri::command]
pub fn drops_tor_status(tor: State<'_, TorManager>) -> crate::tor::TorStatus {
    tor.status()
}

/// Bring Tor up (idempotent) and wait until it is ready or the timeout elapses.
#[tauri::command]
pub async fn drops_tor_start(
    tor: State<'_, TorManager>,
    timeout_ms: Option<u64>,
) -> Result<crate::tor::TorStatus, String> {
    let tor = tor.inner().clone();
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(90_000).min(180_000));
    tauri::async_runtime::spawn_blocking(move || tor.ensure_blocking(timeout))
        .await
        .map_err(|e| e.to_string())
}

/// Stop Tor and report the resulting status.
#[tauri::command]
pub fn drops_tor_stop(tor: State<'_, TorManager>) -> crate::tor::TorStatus {
    tor.stop();
    tor.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v3_https() -> String {
        // 56 'a's is a syntactically valid v3 label (checksum aside; we do not
        // verify the checksum, only the shape).
        let label = "a".repeat(56);
        format!("https://{label}.onion/")
    }

    #[test]
    fn accepts_v3_onion_over_https() {
        let url = validate_target(&v3_https(), false).expect("valid v3 onion");
        assert_eq!(url.scheme(), "https");
        assert!(is_v3_onion(url.host_str().unwrap()));
    }

    /// SecureDrop serves its onion over plain http; Tor provides the encryption
    /// and authenticates the address. Requiring https refused every real
    /// instance, so http must be accepted in production.
    #[test]
    fn accepts_http_onion_in_production() {
        let label = "a".repeat(56);
        let raw = format!("http://{label}.onion/");
        let url = validate_target(&raw, false).expect("http v3 onion is a valid Drop target");
        assert_eq!(url.scheme(), "http");
    }

    #[test]
    fn rejects_plain_web_addresses() {
        assert!(validate_target("https://example.com/", false).is_err());
        assert!(validate_target("https://example.com/", true).is_err());
    }

    #[test]
    fn rejects_short_and_malformed_onions() {
        // v2 (16 chars) is dead and refused.
        assert!(validate_target("https://abcdefghijklmnop.onion/", false).is_err());
        // A label with a character outside base32 is refused.
        let bad = format!("https://{}.onion/", "1".repeat(56));
        assert!(validate_target(&bad, false).is_err());
        // Too long.
        let long = format!("https://{}.onion/", "a".repeat(57));
        assert!(validate_target(&long, false).is_err());
    }

    #[test]
    fn allows_localhost_only_in_dev() {
        assert!(validate_target("http://127.0.0.1:8791/", false).is_err());
        assert!(validate_target("http://127.0.0.1:8791/", true).is_ok());
        assert!(validate_target("http://localhost:8791/", true).is_ok());
        // Even in dev, a remote host is refused.
        assert!(validate_target("http://example.com/", true).is_err());
    }

    #[test]
    fn navigation_guard_matches_production_policy() {
        let ok: Url = v3_https().parse().unwrap();
        assert!(allowed_navigation(&ok, false));
        // The same onion over http, which is what SecureDrop serves.
        let label = "a".repeat(56);
        let http_onion: Url = format!("http://{label}.onion/").parse().unwrap();
        assert!(allowed_navigation(&http_onion, false));
        let bad: Url = "https://example.com/".parse().unwrap();
        assert!(!allowed_navigation(&bad, false));
        // A loopback mock is only reachable in dev, over either scheme.
        let mock: Url = "http://127.0.0.1:8791/".parse().unwrap();
        assert!(!allowed_navigation(&mock, false));
        assert!(allowed_navigation(&mock, true));
        let about: Url = "about:blank".parse().unwrap();
        assert!(allowed_navigation(&about, false));
    }
}

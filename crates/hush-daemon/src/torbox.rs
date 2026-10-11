//! A thin, typed client for the TorBox debrid API.
//!
//! Everything TorBox-specific lives here so the rest of the daemon never sees
//! an endpoint, a query parameter or a response shape. The module owns three
//! things the callers would otherwise repeat:
//!
//! - **The API key never leaves this process.** `requestdl` puts the key in a
//!   query string, so the daemon fetches the CDN URL itself and hands the
//!   caller only bytes — the key (and the signed URL) are never returned to the
//!   UI, logged, or persisted.
//! - **Errors are normalized.** TorBox answers with an HTTP status *and* an
//!   envelope `{success, error, detail, data}`; both are folded into
//!   [`TorboxError`] so the UI can render a human sentence per class (bad key,
//!   plan limit, rate limited, …).
//! - **Rate limits are respected.** A 429 carries `Retry-After` where TorBox
//!   provides it, and the caller backs off instead of hammering.
//!
//! Verified against the official docs (api-docs.torbox.app) and SDKs, not
//! memory: `user/me`, `torrents/createtorrent`, `torrents/mylist`,
//! `torrents/checkcached`, `torrents/requestdl`, `torrents/controltorrent`.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.torbox.app/v1/api";

/// One API request must not hang forever; a stalled TorBox call would freeze
/// the pipeline tick.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Downloads get their own client. A CDN transfer can legitimately run for
/// hours, so a *total* timeout would abort any file that takes longer than the
/// API budget to stream — which is exactly what a 1 GB file does. These are the
/// two timeouts that still apply: a connection that never opens, and a
/// connection that goes silent mid-transfer. The idle window resets on every
/// chunk, so an active download is never cut off.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A TorBox call failed, classified so the caller can react and the UI can
/// explain.
#[derive(Debug, Clone)]
pub enum TorboxError {
    /// No API key is configured yet.
    NoKey,
    /// The key is missing, malformed, or rejected (BAD_TOKEN / NO_AUTH / 403).
    Auth(String),
    /// The account's plan does not allow this (PLAN_RESTRICTED_FEATURE) or a
    /// metered limit was hit (MONTHLY_LIMIT / ACTIVE_LIMIT / COOLDOWN_LIMIT).
    Plan(String),
    /// HTTP 429. `retry_after` is seconds when the server said so.
    RateLimited(Option<u64>),
    /// TorBox answered with a structured error code.
    Api { code: String, detail: String },
    /// The request never completed (DNS, TLS, timeout, connection reset).
    Network(String),
    /// The response was not the expected shape.
    Decode(String),
}

impl std::fmt::Display for TorboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TorboxError::NoKey => write!(f, "No TorBox API key is set."),
            TorboxError::Auth(m) => write!(f, "{m}"),
            TorboxError::Plan(m) => write!(f, "{m}"),
            TorboxError::RateLimited(Some(s)) => {
                write!(f, "TorBox is rate limiting us; retrying in about {s}s.")
            }
            TorboxError::RateLimited(None) => {
                write!(f, "TorBox is rate limiting us; backing off.")
            }
            TorboxError::Api { code, detail } => {
                if detail.is_empty() {
                    write!(f, "TorBox error: {code}")
                } else {
                    write!(f, "{detail}")
                }
            }
            TorboxError::Network(m) => write!(f, "Could not reach TorBox: {m}"),
            TorboxError::Decode(m) => write!(f, "Unexpected reply from TorBox: {m}"),
        }
    }
}

impl std::error::Error for TorboxError {}

impl TorboxError {
    /// Whether another attempt could plausibly succeed. A network blip or a
    /// rate limit will clear on its own; a rejected key, a plan limit, or a
    /// malformed reply will not, so the pipeline should stop and surface them
    /// instead of retrying forever.
    pub fn is_transient(&self) -> bool {
        matches!(self, TorboxError::Network(_) | TorboxError::RateLimited(_))
    }
}

/// The standard TorBox envelope. `data` is often an object, sometimes a list,
/// and `null` on success for control operations.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    data: serde_json::Value,
}

/// Map a non-2xx status plus the envelope to a classified error.
///
/// TorBox sometimes answers 200 with `success: false`, so the caller checks the
/// envelope too; this handles the transport-level failures.
fn classify(status: u16, env: Option<&Envelope>, retry_after: Option<u64>) -> TorboxError {
    let code = env.and_then(|e| e.error.clone()).unwrap_or_default();
    let detail = env
        .and_then(|e| e.detail.clone())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("TorBox returned HTTP {status}"));

    if status == 429 {
        return TorboxError::RateLimited(retry_after);
    }
    if status == 401 || status == 403 || code == "BAD_TOKEN" || code == "NO_AUTH" {
        return TorboxError::Auth(if detail.is_empty() {
            "TorBox rejected the API key.".into()
        } else {
            detail
        });
    }
    match code.as_str() {
        "PLAN_RESTRICTED_FEATURE" | "MONTHLY_LIMIT" | "ACTIVE_LIMIT" | "COOLDOWN_LIMIT"
        | "DOWNLOAD_TOO_LARGE" => TorboxError::Plan(detail),
        "ITEM_NOT_FOUND" => TorboxError::Api {
            code,
            detail: detail.clone(),
        },
        _ => TorboxError::Api { code, detail },
    }
}

/// Deserialize a list that may arrive as an explicit `null`.
///
/// TorBox omits `files` until it has parsed a torrent, and some replies carry
/// `"files": null` instead — a freshly created one among them. serde's
/// `default` covers a *missing* field only, so an explicit null must be folded
/// to an empty list here or the whole reply fails to decode.
fn nullable_seq<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    let v: Option<Vec<T>> = serde::Deserialize::deserialize(de)?;
    Ok(v.unwrap_or_default())
}

/// Deserialize a size that the API may report as negative.
///
/// TorBox uses `-1` for "no size known" (a torrent it has not parsed yet), which
/// cannot land in a `u64` — serde rejects it and the whole reply fails. Fold any
/// negative to 0, which the rest of the pipeline already treats as "unknown".
fn lenient_u64<'de, D>(de: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: i64 = serde::Deserialize::deserialize(de)?;
    Ok(v.max(0) as u64)
}

/// One file inside a torrent, as TorBox reports it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TorrentFile {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub short_name: String,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub size: u64,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub absolute_path: Option<String>,
}

/// One torrent in the account, from `torrents/mylist`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TorrentInfo {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub size: u64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub download_state: String,
    #[serde(default)]
    pub download_finished: bool,
    #[serde(default)]
    pub download_present: bool,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub seeds: i64,
    #[serde(default)]
    pub peers: i64,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub download_speed: f64,
    #[serde(default)]
    pub eta: i64,
    #[serde(default, deserialize_with = "nullable_seq")]
    pub files: Vec<TorrentFile>,
}

impl TorrentInfo {
    /// A 0..1 completion fraction. TorBox reports `progress` as a fraction, but
    /// be tolerant of a percentage if a future server sends one.
    pub fn progress_fraction(&self) -> f64 {
        normalize_progress(self.progress)
    }

    /// Whether the torrent's data is on TorBox's servers and ready to download.
    /// `download_state == "completed"` alone is not enough (the docs say not to
    /// use it for completion); `download_finished`/`cached`/`download_present`
    /// are the real signals.
    pub fn is_ready(&self) -> bool {
        self.cached || self.download_finished || self.download_present
    }
}

/// Normalize TorBox's progress to a 0..1 fraction.
pub fn normalize_progress(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 0.0;
    }
    let frac = if raw > 1.0 { raw / 100.0 } else { raw };
    frac.clamp(0.0, 1.0)
}

/// The result of adding a torrent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CreatedTorrent {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub torrent_id: i64,
    #[serde(default)]
    pub auth_id: Option<String>,
}

/// Account info from `user/me`, reduced to what the UI shows.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UserInfo {
    /// 0 Free, 1 Essential, 3 Standard, 2 Pro.
    pub plan: i64,
    pub plan_label: String,
    pub email: Option<String>,
    pub expires: Option<String>,
}

impl UserInfo {
    fn from_value(v: &serde_json::Value) -> Self {
        let plan = v.get("plan").and_then(|p| p.as_i64()).unwrap_or(0);
        Self {
            plan,
            plan_label: plan_label(plan).to_string(),
            email: v
                .get("email")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string()),
            expires: v
                .get("premium_expires_at")
                .or_else(|| v.get("plan_expires_at"))
                .and_then(|e| e.as_str())
                .map(|s| s.to_string()),
        }
    }
}

/// Human label for TorBox's numeric plan id.
pub fn plan_label(plan: i64) -> &'static str {
    match plan {
        0 => "Free",
        1 => "Essential",
        2 => "Pro",
        3 => "Standard",
        _ => "Unknown",
    }
}

/// One cached torrent's metadata from `torrents/checkcached`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CachedInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub size: u64,
    #[serde(default)]
    pub hash: String,
    #[serde(default, deserialize_with = "nullable_seq")]
    pub files: Vec<CachedFile>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CachedFile {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub size: u64,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub short_name: Option<String>,
}

/// Decode a `checkcached` reply (`format=object`) into the cached map.
///
/// A hash TorBox cannot resolve maps to an explicit `null` in the object; only
/// the real entries make it into the result, so an uncached hash is simply
/// absent rather than failing the whole reply.
fn decode_cached_map(
    data: serde_json::Value,
) -> Result<HashMap<String, CachedInfo>, TorboxError> {
    let raw: HashMap<String, Option<CachedInfo>> =
        serde_json::from_value(data).map_err(|e| TorboxError::Decode(e.to_string()))?;
    Ok(raw
        .into_iter()
        .filter_map(|(hash, info)| info.map(|i| (hash, i)))
        .collect())
}

/// A control operation TorBox accepts on a torrent.
#[derive(Debug, Clone, Copy)]
pub enum Control {
    Pause,
    Resume,
    Reannounce,
    Delete,
}

impl Control {
    fn as_str(self) -> &'static str {
        match self {
            Control::Pause => "pause",
            Control::Resume => "resume",
            Control::Reannounce => "reannounce",
            Control::Delete => "delete",
        }
    }
}

/// The typed TorBox client. Cheap to clone (the inner `reqwest::Client` shares
/// a connection pool).
#[derive(Clone)]
pub struct TorboxClient {
    http: reqwest::Client,
    /// Separate from `http`: no total timeout, only connect + idle, so a long
    /// CDN download is not torn down by the 30 s API budget.
    download_http: reqwest::Client,
}

impl TorboxClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("hush-daemon/0.1")
            .build()
            .expect("torbox http client");
        let download_http = reqwest::Client::builder()
            .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
            .read_timeout(DOWNLOAD_IDLE_TIMEOUT)
            .user_agent("hush-daemon/0.1")
            .build()
            .expect("torbox download client");
        Self {
            http,
            download_http,
        }
    }

    fn get(&self, key: &str, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{API_BASE}/{path}"))
            .bearer_auth(key)
    }

    /// Send a request and unwrap the envelope, classifying any failure.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<serde_json::Value, TorboxError> {
        let resp = req
            .send()
            .await
            .map_err(|e| TorboxError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = resp
            .text()
            .await
            .map_err(|e| TorboxError::Network(e.to_string()))?;

        let env: Option<Envelope> = serde_json::from_str(&body).ok();
        let ok_status = (200..300).contains(&status);
        match &env {
            Some(e) if e.success && ok_status => Ok(e.data.clone()),
            Some(e) if e.success => Ok(e.data.clone()),
            _ => Err(classify(status, env.as_ref(), retry_after)),
        }
    }

    /// `GET /user/me` — validate the key and read the account's plan.
    pub async fn user_me(&self, key: &str) -> Result<UserInfo, TorboxError> {
        let data = self.send(self.get(key, "user/me")).await?;
        Ok(UserInfo::from_value(&data))
    }

    /// `POST /torrents/createtorrent` with a magnet link.
    pub async fn create_torrent_magnet(
        &self,
        key: &str,
        magnet: &str,
        name: Option<&str>,
    ) -> Result<CreatedTorrent, TorboxError> {
        let mut form = reqwest::multipart::Form::new().text("magnet", magnet.to_string());
        if let Some(n) = name {
            form = form.text("name", n.to_string());
        }
        self.create_torrent(key, form).await
    }

    /// `POST /torrents/createtorrent` with a URL to a `.torrent` file.
    pub async fn create_torrent_link(
        &self,
        key: &str,
        link: &str,
        name: Option<&str>,
    ) -> Result<CreatedTorrent, TorboxError> {
        let mut form = reqwest::multipart::Form::new().text("link", link.to_string());
        if let Some(n) = name {
            form = form.text("name", n.to_string());
        }
        self.create_torrent(key, form).await
    }

    /// `POST /torrents/createtorrent` with a `.torrent` file's bytes.
    pub async fn create_torrent_file(
        &self,
        key: &str,
        filename: &str,
        bytes: Vec<u8>,
        name: Option<&str>,
    ) -> Result<CreatedTorrent, TorboxError> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str("application/x-bittorrent")
            .map_err(|e| TorboxError::Decode(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if let Some(n) = name {
            form = form.text("name", n.to_string());
        }
        self.create_torrent(key, form).await
    }

    async fn create_torrent(
        &self,
        key: &str,
        form: reqwest::multipart::Form,
    ) -> Result<CreatedTorrent, TorboxError> {
        let req = self
            .http
            .post(format!("{API_BASE}/torrents/createtorrent"))
            .bearer_auth(key)
            .multipart(form);
        let data = self.send(req).await?;
        serde_json::from_value(data).map_err(|e| TorboxError::Decode(e.to_string()))
    }

    /// `GET /torrents/mylist` — the whole account list, or one torrent by id.
    /// `bypass_cache` asks for fresh stats.
    pub async fn list(
        &self,
        key: &str,
        id: Option<i64>,
        bypass_cache: bool,
    ) -> Result<Vec<TorrentInfo>, TorboxError> {
        let mut req = self.get(key, "torrents/mylist");
        if let Some(id) = id {
            req = req.query(&[("id", id.to_string())]);
        }
        if bypass_cache {
            req = req.query(&[("bypass_cache", "true")]);
        }
        let data = self.send(req).await?;
        // With `id`, TorBox returns a single object; without, a list.
        if data.is_array() {
            serde_json::from_value(data).map_err(|e| TorboxError::Decode(e.to_string()))
        } else if data.is_object() {
            let one: TorrentInfo =
                serde_json::from_value(data).map_err(|e| TorboxError::Decode(e.to_string()))?;
            Ok(vec![one])
        } else {
            Ok(Vec::new())
        }
    }

    /// `GET /torrents/checkcached` — which of these hashes are already cached.
    pub async fn check_cached(
        &self,
        key: &str,
        hashes: &[String],
    ) -> Result<HashMap<String, CachedInfo>, TorboxError> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let req = self.get(key, "torrents/checkcached").query(&[
            ("hash", hashes.join(",")),
            ("format", "object".to_string()),
            ("list_files", "true".to_string()),
        ]);
        let data = self.send(req).await?;
        decode_cached_map(data)
    }

    /// `GET /torrents/requestdl` — a direct CDN URL for one file (or the zip).
    ///
    /// The key travels in the query string here, which is why this URL is
    /// consumed inside the daemon and never returned upward.
    pub async fn request_dl(
        &self,
        key: &str,
        torrent_id: i64,
        file_id: i64,
    ) -> Result<String, TorboxError> {
        let req = self.get(key, "torrents/requestdl").query(&[
            ("token", key.to_string()),
            ("torrent_id", torrent_id.to_string()),
            ("file_id", file_id.to_string()),
            ("redirect", "false".to_string()),
        ]);
        let data = self.send(req).await?;
        data.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| TorboxError::Decode("requestdl returned no URL".into()))
    }

    /// `POST /torrents/controltorrent` — pause/resume/reannounce/delete.
    pub async fn control(
        &self,
        key: &str,
        torrent_id: i64,
        op: Control,
    ) -> Result<(), TorboxError> {
        let req = self
            .http
            .post(format!("{API_BASE}/torrents/controltorrent"))
            .bearer_auth(key)
            .json(&serde_json::json!({
                "torrent_id": torrent_id,
                "operation": op.as_str(),
            }));
        self.send(req).await.map(|_| ())
    }

    /// Download a URL (a CDN link from [`Self::request_dl`]) into `sink`,
    /// resuming from `offset` with an HTTP Range request when possible.
    ///
    /// Returns the bytes written this call. Streams chunk by chunk so a large
    /// file never sits in memory.
    pub async fn fetch_to<W: std::io::Write>(
        &self,
        url: &str,
        offset: u64,
        mut sink: W,
    ) -> Result<u64, TorboxError> {
        use futures_util::StreamExt;
        // The download client: connect + idle timeouts only, never a total one.
        let mut req = self.download_http.get(url);
        if offset > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TorboxError::Network(e.to_string()))?;
        let status = resp.status();
        // A server that ignores Range answers 200 and restarts from zero; we
        // must then not append, or the file would be corrupted. Signal the
        // caller to truncate by reporting a fresh start.
        if offset > 0 && status == reqwest::StatusCode::OK {
            return Err(TorboxError::Api {
                code: "RANGE_UNSUPPORTED".into(),
                detail: "the download server does not support resuming".into(),
            });
        }
        if !status.is_success() {
            return Err(TorboxError::Network(format!("download HTTP {status}")));
        }
        let mut stream = resp.bytes_stream();
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| TorboxError::Network(e.to_string()))?;
            sink.write_all(&chunk)
                .map_err(|e| TorboxError::Network(format!("writing download: {e}")))?;
            written += chunk.len() as u64;
        }
        Ok(written)
    }
}

impl Default for TorboxClient {
    fn default() -> Self {
        Self::new()
    }
}

/// The info-hash of a magnet link's `xt=urn:btih:` parameter, lowercased.
/// Returns `None` for anything that is not a magnet with a btih.
pub fn magnet_hash(magnet: &str) -> Option<String> {
    let lower = magnet.to_ascii_lowercase();
    if !lower.starts_with("magnet:") {
        return None;
    }
    for part in magnet.split(['?', '&']) {
        if let Some(rest) = part.strip_prefix("xt=urn:btih:") {
            let hash = rest.split('&').next().unwrap_or(rest);
            if !hash.is_empty() {
                return Some(hash.to_ascii_lowercase());
            }
        }
    }
    None
}

/// Whether a pasted string looks like a magnet link.
pub fn looks_like_magnet(s: &str) -> bool {
    s.trim().to_ascii_lowercase().starts_with("magnet:")
}

/// Whether a pasted string looks like a `.torrent` URL.
pub fn looks_like_torrent_url(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    (t.starts_with("http://") || t.starts_with("https://")) && t.contains(".torrent")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_normalizes_fractions_and_percentages() {
        assert_eq!(normalize_progress(0.0), 0.0);
        assert_eq!(normalize_progress(0.42), 0.42);
        assert_eq!(normalize_progress(1.0), 1.0);
        assert_eq!(normalize_progress(42.0), 0.42);
        assert_eq!(normalize_progress(120.0), 1.0);
        assert_eq!(normalize_progress(-1.0), 0.0);
        assert_eq!(normalize_progress(f64::NAN), 0.0);
    }

    #[test]
    fn readiness_uses_the_real_signals_not_the_state_string() {
        let mut t = TorrentInfo {
            download_state: "completed".into(),
            ..Default::default()
        };
        assert!(!t.is_ready(), "a completed state alone is not readiness");
        t.download_finished = true;
        assert!(t.is_ready());
        t.download_finished = false;
        t.cached = true;
        assert!(t.is_ready());
        t.cached = false;
        t.download_present = true;
        assert!(t.is_ready());
    }

    #[test]
    fn magnet_hash_extracts_and_lowercases() {
        let m = "magnet:?xt=urn:btih:ABCDEF1234567890&dn=Something&tr=udp://x";
        assert_eq!(magnet_hash(m).as_deref(), Some("abcdef1234567890"));
        assert_eq!(magnet_hash("https://example.com/a.torrent"), None);
        assert_eq!(magnet_hash("magnet:?dn=no-hash"), None);
    }

    #[test]
    fn classify_maps_the_error_classes() {
        let auth = Envelope {
            success: false,
            error: Some("BAD_TOKEN".into()),
            detail: Some("bad key".into()),
            data: serde_json::Value::Null,
        };
        assert!(matches!(
            classify(403, Some(&auth), None),
            TorboxError::Auth(_)
        ));

        let plan = Envelope {
            success: false,
            error: Some("ACTIVE_LIMIT".into()),
            detail: Some("too many active downloads".into()),
            data: serde_json::Value::Null,
        };
        assert!(matches!(
            classify(400, Some(&plan), None),
            TorboxError::Plan(_)
        ));

        assert!(matches!(
            classify(429, None, Some(30)),
            TorboxError::RateLimited(Some(30))
        ));
        assert!(matches!(
            classify(500, None, None),
            TorboxError::Api { .. }
        ));
    }

    #[test]
    fn only_network_and_rate_limit_errors_are_transient() {
        // A dropped connection or a 429 will clear on its own; a bad key, a
        // plan limit, a rejected API call, or a malformed reply will not, so
        // the pipeline must stop and surface those instead of retrying forever.
        assert!(TorboxError::Network("reset".into()).is_transient());
        assert!(TorboxError::RateLimited(None).is_transient());
        assert!(!TorboxError::Auth("bad key".into()).is_transient());
        assert!(!TorboxError::Plan("limit".into()).is_transient());
        assert!(!TorboxError::Api {
            code: "RANGE_UNSUPPORTED".into(),
            detail: "no resume".into()
        }
        .is_transient());
        assert!(!TorboxError::Decode("garbage".into()).is_transient());
        assert!(!TorboxError::NoKey.is_transient());
    }

    #[test]
    fn user_info_reads_plan_and_is_lenient_about_extra_fields() {
        let v: serde_json::Value = serde_json::json!({
            "id": 7, "email": "a@b.c", "plan": 3, "premium_expires_at": "2027-01-01T00:00:00Z",
            "something_new": true
        });
        let info = UserInfo::from_value(&v);
        assert_eq!(info.plan, 3);
        assert_eq!(info.plan_label, "Standard");
        assert_eq!(info.email.as_deref(), Some("a@b.c"));

        let empty = UserInfo::from_value(&serde_json::json!({}));
        assert_eq!(empty.plan, 0);
        assert_eq!(empty.plan_label, "Free");
    }

    #[test]
    fn torrent_info_decodes_the_documented_shape() {
        let v: serde_json::Value = serde_json::json!({
            "id": 42, "hash": "abc", "name": "Example", "size": 1000,
            "download_state": "downloading", "progress": 0.5,
            "download_speed": 2048.0, "seeds": 12, "peers": 30,
            "files": [{ "id": 1, "name": "a.mkv", "size": 1000, "short_name": "a.mkv" }]
        });
        let t: TorrentInfo = serde_json::from_value(v).unwrap();
        assert_eq!(t.id, 42);
        assert_eq!(t.progress_fraction(), 0.5);
        assert_eq!(t.seeds, 12);
        assert_eq!(t.files.len(), 1);
        assert_eq!(t.files[0].name, "a.mkv");
    }

    #[test]
    fn torrent_info_tolerates_an_explicit_null_file_list() {
        // Regression: a torrent TorBox has not parsed yet reports
        // "files": null, and `default` only covers a *missing* field, so the
        // whole mylist reply used to fail with
        // "invalid type: null, expected a sequence".
        let v: serde_json::Value =
            serde_json::json!({ "id": 7, "name": "Example", "files": null });
        let t: TorrentInfo = serde_json::from_value(v).unwrap();
        assert!(t.files.is_empty());

        let c: CachedInfo = serde_json::from_value(serde_json::json!({ "files": null })).unwrap();
        assert!(c.files.is_empty());
    }

    #[test]
    fn negative_sizes_mean_unknown_not_a_decode_error() {
        // Regression: TorBox reports size -1 for a torrent it has not parsed
        // yet; a u64 field rejects it and the whole reply failed with
        // "invalid value: integer -1, expected u64".
        let t: TorrentInfo = serde_json::from_value(
            serde_json::json!({ "id": 7, "name": "Example", "size": -1,
                               "files": [{ "id": 1, "name": "a.mkv", "size": -1 }] }),
        )
        .unwrap();
        assert_eq!(t.size, 0);
        assert_eq!(t.files[0].size, 0);

        let c: CachedInfo =
            serde_json::from_value(serde_json::json!({ "name": "C", "size": -1 })).unwrap();
        assert_eq!(c.size, 0);
        let cf: CachedFile = serde_json::from_value(serde_json::json!({ "id": 1, "size": -1 }))
            .unwrap();
        assert_eq!(cf.size, 0);
    }

    #[test]
    fn cached_map_skips_hashes_that_map_to_null() {
        let v: serde_json::Value = serde_json::json!({
            "cached-hash": { "name": "Cached", "size": 10, "hash": "cached-hash", "files": null },
            "uncached-hash": null
        });
        let map = decode_cached_map(v).unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("cached-hash"));
        assert!(map["cached-hash"].files.is_empty());
    }

    #[test]
    fn looks_like_helpers_classify_pasted_input() {
        assert!(looks_like_magnet("magnet:?xt=urn:btih:aa"));
        assert!(!looks_like_magnet("https://x/y.torrent"));
        assert!(looks_like_torrent_url("https://x/y.torrent"));
        assert!(looks_like_torrent_url("http://x/y.TORRENT?token=1"));
        assert!(!looks_like_torrent_url("https://x/y.mkv"));
    }
}

//! Central client configuration surface (DR-14).
//!
//! One place decides which relay this client talks to and where the daemon
//! listens, so Tor / firewall policy does not get re-implemented per binary or
//! per script.
//!
//! Precedence, highest first:
//!   1. explicit argument (CLI flag, or an env var bound by clap)
//!   2. environment (`HUSH_RELAY_URL` / `HUSH_DAEMON_ADDR`)
//!   3. `{data_dir}/client-config.json`
//!   4. built-in default
//!
//! The file is advisory for *new* vaults: a vault stores the relay it was
//! created with, and that stored value wins at runtime (see [`crate::Account`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::default_runtime_dir;
use crate::relay_client::normalize_relay_base;

/// Default relay for a fresh vault, in every build.
///
/// The project uses the production relay only. There is deliberately no
/// local-relay default: a debug build and a packaged build must reach the same
/// host, or an invite minted on one is useless on the other. Automated tests
/// still start an in-process relay, but no app or run script does. A vault
/// stores the relay it was created with; this default only decides what a fresh
/// install dials first. Onion twin: [`DEFAULT_RELAY_ONION_URL`].
pub const DEFAULT_RELAY_URL: &str = "https://hushrelay.space";
/// The production relay's onion twin, baked into every build so Tor relay mode
/// works on a fresh install with no configuration. Precedence:
/// `HUSH_RELAY_ONION_URL` > `client-config.json` > this.
pub const DEFAULT_RELAY_ONION_URL: &str =
    "http://qvo7toe6urizihhbj7tp4ibsf2rhkilsrwvnkdk5jlwubjm3e7rnrlad.onion";
/// Default daemon HTTP bind address, used only when TCP is explicitly enabled.
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:9876";
/// Environment override for the relay URL.
pub const ENV_RELAY_URL: &str = "HUSH_RELAY_URL";
/// Environment override for the relay's onion address.
pub const ENV_RELAY_ONION_URL: &str = "HUSH_RELAY_ONION_URL";
/// Environment override for the STUN server used by the Conclave Paper data
/// channel.
pub const ENV_STUN_URL: &str = "HUSH_STUN_URL";
/// Environment override for the PLC directory `did:plc` identifiers resolve
/// against.
pub const ENV_ATPROTO_PLC_URL: &str = "HUSH_ATPROTO_PLC_URL";
/// Environment override for the daemon bind address (enables the TCP listener).
pub const ENV_DAEMON_ADDR: &str = "HUSH_DAEMON_ADDR";
/// Environment override for the daemon Unix socket path.
pub const ENV_DAEMON_SOCKET: &str = "HUSH_DAEMON_SOCKET";

pub const CONFIG_FILE_NAME: &str = "client-config.json";
/// Unix socket file name inside the runtime dir (DR-02).
pub const SOCKET_FILE_NAME: &str = "hush-daemon.sock";

/// Where a resolved value came from, for `config show` and status reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    Argument,
    Env,
    File,
    Default,
}

impl ConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigSource::Argument => "flag/env",
            ConfigSource::Env => "env",
            ConfigSource::File => "config file",
            ConfigSource::Default => "default",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientConfig {
    #[serde(default = "default_version")]
    pub v: u32,
    /// Relay used when creating a new vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// The relay's onion address. Operator config: when present, vaults pick
    /// it up automatically in Tor relay mode and the user never types one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onion_relay: Option<String>,
    /// STUN server for the Conclave Paper data channel. Operator config, and
    /// deliberately without a default — see [`resolve_stun_url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stun_url: Option<String>,
    /// The directory a `did:plc` resolves against. Operator config: a deployment
    /// that must go through a mirror names one here. Absent means the public
    /// directory, which the daemon supplies so the default lives in one place
    /// (`hush_atproto::DEFAULT_PLC_DIRECTORY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atproto_plc_url: Option<String>,
    /// TCP bind address for the daemon. Setting this opts the TCP listener back in;
    /// the default is Unix socket only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_addr: Option<String>,
    /// Unix socket path for the daemon API, overriding the runtime-dir default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_socket: Option<String>,
    /// Preferred input (microphone) device name for conclave voice, as cpal
    /// reports it. Absent means the OS default device. A name the system no
    /// longer has falls back to the default, with the mismatch surfaced
    /// through the voice device endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_input_device: Option<String>,
    /// Preferred output (speaker) device name for conclave voice. Same rules
    /// as [`ClientConfig::voice_input_device`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_output_device: Option<String>,
}

fn default_version() -> u32 {
    1
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            v: 1,
            relay_url: None,
            onion_relay: None,
            stun_url: None,
            atproto_plc_url: None,
            daemon_addr: None,
            daemon_socket: None,
            voice_input_device: None,
            voice_output_device: None,
        }
    }
}

pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE_NAME)
}

/// Load the config file, falling back to defaults when missing or unreadable.
pub fn load(data_dir: &Path) -> ClientConfig {
    std::fs::read(config_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Validate and persist the config file (atomic rename, `0600` on Unix).
pub fn save(data_dir: &Path, config: &ClientConfig) -> Result<(), String> {
    if let Some(relay) = config.relay_url.as_deref() {
        normalize_relay_base(relay).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    let path = config_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Effective relay URL plus the source that supplied it.
pub fn resolve_relay_url_with_source(
    data_dir: &Path,
    explicit: Option<&str>,
) -> (String, ConfigSource) {
    if let Some(v) = non_empty(explicit) {
        return (v.to_string(), ConfigSource::Argument);
    }
    if let Some(v) = non_empty(std::env::var(ENV_RELAY_URL).ok().as_deref()) {
        return (v.to_string(), ConfigSource::Env);
    }
    if let Some(v) = non_empty(load(data_dir).relay_url.as_deref()) {
        return (v.to_string(), ConfigSource::File);
    }
    (DEFAULT_RELAY_URL.to_string(), ConfigSource::Default)
}

/// Effective relay URL for creating a new vault.
pub fn resolve_relay_url(data_dir: &Path, explicit: Option<&str>) -> String {
    resolve_relay_url_with_source(data_dir, explicit).0
}

/// Effective onion address for the relay, when one is on file (env or config
/// file; release builds also carry a baked-in production default). No clearnet
/// fallback exists — an onion address is either configured or absent.
pub fn resolve_onion_relay_url(data_dir: &Path) -> Option<String> {
    if let Some(v) = non_empty(std::env::var(ENV_RELAY_ONION_URL).ok().as_deref()) {
        return Some(v.to_string());
    }
    let file = load(data_dir);
    if let Some(v) = non_empty(file.onion_relay.as_deref()) {
        return Some(v.to_string());
    }
    #[cfg(not(debug_assertions))]
    return Some(DEFAULT_RELAY_ONION_URL.to_string());
    #[cfg(debug_assertions)]
    return None;
}

/// Whether this onion address is the baked-in production default rather than
/// something the user configured. Release builds bake the production relay's
/// onion twin so Tor relay mode works on a fresh install; that twin belongs
/// only to a vault still running on the baked-in relay — staging it for a
/// vault pointed at another relay would publish a drop page (and route
/// traffic) for a relay the vault does not use. Callers staging an onion
/// must refuse the default in that case; explicitly configured onions are
/// always honored.
pub fn is_baked_default_onion(url: &str) -> bool {
    #[cfg(not(debug_assertions))]
    {
        url == DEFAULT_RELAY_ONION_URL
    }
    #[cfg(debug_assertions)]
    {
        let _ = url;
        false
    }
}

/// Effective STUN server for the Conclave Paper data channel, when one is on
/// file (env or config file).
///
/// There is deliberately no default. A public STUN server would learn the
/// public address of every peer that opens a paper — the exact metadata this
/// project refuses to hand a third party — so an operator either runs one (the
/// same self-hosted one Task 17 needs) or peers gather host candidates only and
/// connect where they can see each other directly. With no STUN and no TURN, a
/// symmetric NAT falls back to the relay path rather than failing; see
/// `hush_daemon::paper_data`.
pub fn resolve_stun_url(data_dir: &Path) -> Option<String> {
    if let Some(v) = non_empty(std::env::var(ENV_STUN_URL).ok().as_deref()) {
        return Some(v.to_string());
    }
    let file = load(data_dir);
    non_empty(file.stun_url.as_deref()).map(str::to_string)
}

/// The PLC directory a `did:plc` resolves against, when an operator configured
/// one (env or config file).
///
/// `None` means the public directory. The public lane reads the repos the member
/// named and asks the directory only about accounts they already chose to
/// follow, so the default is the honest one — but a deployment that must reach
/// the directory through a mirror needs a name for it, and this is that name.
/// The default itself is supplied by the caller, so it lives beside the client
/// that uses it.
pub fn resolve_atproto_plc_url(data_dir: &Path) -> Option<String> {
    if let Some(v) = non_empty(std::env::var(ENV_ATPROTO_PLC_URL).ok().as_deref()) {
        return Some(v.to_string());
    }
    let file = load(data_dir);
    non_empty(file.atproto_plc_url.as_deref()).map(str::to_string)
}

/// Effective daemon bind address plus the source that supplied it.
///
/// `None` means "no TCP listener": the Unix socket is the default API (DR-02),
/// and TCP only opens when a flag, env var, or the config file asks for it.
pub fn resolve_daemon_addr_with_source(
    data_dir: &Path,
    explicit: Option<&str>,
) -> (Option<String>, ConfigSource) {
    if let Some(v) = non_empty(explicit) {
        return (Some(v.to_string()), ConfigSource::Argument);
    }
    if let Some(v) = non_empty(std::env::var(ENV_DAEMON_ADDR).ok().as_deref()) {
        return (Some(v.to_string()), ConfigSource::Env);
    }
    if let Some(v) = non_empty(load(data_dir).daemon_addr.as_deref()) {
        return (Some(v.to_string()), ConfigSource::File);
    }
    (None, ConfigSource::Default)
}

/// Effective daemon bind address, or `None` when TCP is not enabled.
pub fn resolve_daemon_addr(data_dir: &Path, explicit: Option<&str>) -> Option<String> {
    resolve_daemon_addr_with_source(data_dir, explicit).0
}

/// Default Unix socket path: `{runtime_dir}/hush-daemon.sock`.
pub fn default_socket_path() -> PathBuf {
    default_runtime_dir().join(SOCKET_FILE_NAME)
}

/// Effective daemon Unix socket path plus the source that supplied it.
pub fn resolve_daemon_socket_with_source(
    data_dir: &Path,
    explicit: Option<&str>,
) -> (PathBuf, ConfigSource) {
    if let Some(v) = non_empty(explicit) {
        return (PathBuf::from(v), ConfigSource::Argument);
    }
    if let Some(v) = non_empty(std::env::var(ENV_DAEMON_SOCKET).ok().as_deref()) {
        return (PathBuf::from(v), ConfigSource::Env);
    }
    if let Some(v) = non_empty(load(data_dir).daemon_socket.as_deref()) {
        return (PathBuf::from(v), ConfigSource::File);
    }
    (default_socket_path(), ConfigSource::Default)
}

/// Effective daemon Unix socket path.
pub fn resolve_daemon_socket(data_dir: &Path, explicit: Option<&str>) -> PathBuf {
    resolve_daemon_socket_with_source(data_dir, explicit).0
}

/// The preferred voice input device name, when one is on file.
///
/// Unlike the relay and STUN settings there is no env override and no
/// default: a device is a per-machine preference, not a deployment decision,
/// and `None` honestly means "the OS default device".
pub fn resolve_voice_input_device(data_dir: &Path) -> Option<String> {
    non_empty(load(data_dir).voice_input_device.as_deref()).map(str::to_string)
}

/// The preferred voice output device name, when one is on file.
pub fn resolve_voice_output_device(data_dir: &Path) -> Option<String> {
    non_empty(load(data_dir).voice_output_device.as_deref()).map(str::to_string)
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `resolve_*` reads process-global env; serialize tests that touch it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_env<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_RELAY_URL);
        std::env::remove_var(ENV_RELAY_ONION_URL);
        std::env::remove_var(ENV_DAEMON_ADDR);
        std::env::remove_var(ENV_DAEMON_SOCKET);
        std::env::remove_var(ENV_STUN_URL);
        f()
    }

    #[test]
    fn stun_is_absent_by_default_and_follows_env_then_file() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            // No default, by design: a public STUN server would learn the public
            // address of every peer that opens a paper.
            assert_eq!(resolve_stun_url(dir.path()), None);

            save(
                dir.path(),
                &ClientConfig {
                    stun_url: Some("stun:stun.internal:3478".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                resolve_stun_url(dir.path()),
                Some("stun:stun.internal:3478".to_string())
            );

            std::env::set_var(ENV_STUN_URL, "stun:stun.from-env:3478");
            assert_eq!(
                resolve_stun_url(dir.path()),
                Some("stun:stun.from-env:3478".to_string())
            );
        });
    }

    #[test]
    fn defaults_when_nothing_is_set() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let (relay, source) = resolve_relay_url_with_source(dir.path(), None);
            assert_eq!(relay, DEFAULT_RELAY_URL);
            assert_eq!(source, ConfigSource::Default);
            // DR-02: no TCP unless asked for; socket is the default API.
            assert_eq!(resolve_daemon_addr(dir.path(), None), None);
            assert_eq!(resolve_daemon_socket(dir.path(), None), default_socket_path());
        });
    }

    #[test]
    fn socket_path_precedence_is_argument_then_env_then_file() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            save(
                dir.path(),
                &ClientConfig {
                    daemon_socket: Some("/tmp/from-file.sock".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                resolve_daemon_socket(dir.path(), None),
                PathBuf::from("/tmp/from-file.sock")
            );

            std::env::set_var(ENV_DAEMON_SOCKET, "/tmp/from-env.sock");
            assert_eq!(
                resolve_daemon_socket(dir.path(), None),
                PathBuf::from("/tmp/from-env.sock")
            );
            assert_eq!(
                resolve_daemon_socket(dir.path(), Some("/tmp/from-flag.sock")),
                PathBuf::from("/tmp/from-flag.sock")
            );
        });
    }

    #[test]
    fn file_beats_default_and_loses_to_argument() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            save(
                dir.path(),
                &ClientConfig {
                    // Loopback so the URL passes the release build's
                    // HTTPS-except-loopback policy: these tests exercise
                    // precedence, not URL validation.
                    relay_url: Some("http://127.0.0.1:8785".into()),
                    daemon_addr: Some("127.0.0.1:9999".into()),
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(
                resolve_relay_url_with_source(dir.path(), None),
                ("http://127.0.0.1:8785".into(), ConfigSource::File)
            );
            assert_eq!(
                resolve_daemon_addr(dir.path(), None),
                Some("127.0.0.1:9999".to_string())
            );
            assert_eq!(
                resolve_relay_url_with_source(dir.path(), Some("http://127.0.0.1:1")),
                ("http://127.0.0.1:1".into(), ConfigSource::Argument)
            );
        });
    }

    #[test]
    fn env_beats_file_and_loses_to_argument() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            save(
                dir.path(),
                &ClientConfig {
                    relay_url: Some("http://127.0.0.1:8785".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            std::env::set_var(ENV_RELAY_URL, "http://127.0.0.1:8786");

            assert_eq!(
                resolve_relay_url_with_source(dir.path(), None),
                ("http://127.0.0.1:8786".into(), ConfigSource::Env)
            );
            assert_eq!(
                resolve_relay_url_with_source(dir.path(), Some("http://127.0.0.1:8787")),
                ("http://127.0.0.1:8787".into(), ConfigSource::Argument)
            );
        });
    }

    #[test]
    fn empty_strings_fall_through_to_file() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            save(
                dir.path(),
                &ClientConfig {
                    relay_url: Some("http://127.0.0.1:8785".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                resolve_relay_url(dir.path(), Some("   ")),
                "http://127.0.0.1:8785"
            );
        });
    }

    #[test]
    fn save_rejects_insecure_relay_url() {
        let dir = tempfile::tempdir().unwrap();
        let err = save(
            dir.path(),
            &ClientConfig {
                relay_url: Some("ftp://relay.example".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("http:// or https://"), "unexpected: {err}");
    }

    #[test]
    fn unreadable_file_falls_back_to_default() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(config_path(dir.path()), b"{ not json").unwrap();
            assert_eq!(load(dir.path()), ClientConfig::default());
            assert_eq!(resolve_relay_url(dir.path(), None), DEFAULT_RELAY_URL);
        });
    }

    /// Voice devices round-trip through the file and stay absent by default.
    /// No env override exists on purpose: a device is per-machine, not a
    /// deployment setting like the relay or STUN.
    #[test]
    fn voice_devices_absent_by_default_and_round_trip() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(resolve_voice_input_device(dir.path()), None);
            assert_eq!(resolve_voice_output_device(dir.path()), None);

            save(
                dir.path(),
                &ClientConfig {
                    voice_input_device: Some("ALC236 Analog".into()),
                    voice_output_device: Some("  ".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                resolve_voice_input_device(dir.path()),
                Some("ALC236 Analog".to_string())
            );
            // A blank name is not a choice — it resolves back to the default.
            assert_eq!(resolve_voice_output_device(dir.path()), None);
        });
    }
}

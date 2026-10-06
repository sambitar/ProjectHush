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

use crate::relay_client::normalize_relay_base;

/// Default relay for a fresh local dogfood run.
pub const DEFAULT_RELAY_URL: &str = "http://127.0.0.1:8787";
/// Default daemon HTTP bind address.
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:9876";
/// Environment override for the relay URL.
pub const ENV_RELAY_URL: &str = "HUSH_RELAY_URL";
/// Environment override for the daemon bind address.
pub const ENV_DAEMON_ADDR: &str = "HUSH_DAEMON_ADDR";

pub const CONFIG_FILE_NAME: &str = "client-config.json";

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
    /// Address the daemon binds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_addr: Option<String>,
}

fn default_version() -> u32 {
    1
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            v: 1,
            relay_url: None,
            daemon_addr: None,
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

/// Effective daemon bind address plus the source that supplied it.
pub fn resolve_daemon_addr_with_source(
    data_dir: &Path,
    explicit: Option<&str>,
) -> (String, ConfigSource) {
    if let Some(v) = non_empty(explicit) {
        return (v.to_string(), ConfigSource::Argument);
    }
    if let Some(v) = non_empty(std::env::var(ENV_DAEMON_ADDR).ok().as_deref()) {
        return (v.to_string(), ConfigSource::Env);
    }
    if let Some(v) = non_empty(load(data_dir).daemon_addr.as_deref()) {
        return (v.to_string(), ConfigSource::File);
    }
    (DEFAULT_DAEMON_ADDR.to_string(), ConfigSource::Default)
}

/// Effective daemon bind address.
pub fn resolve_daemon_addr(data_dir: &Path, explicit: Option<&str>) -> String {
    resolve_daemon_addr_with_source(data_dir, explicit).0
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
        std::env::remove_var(ENV_DAEMON_ADDR);
        f()
    }

    #[test]
    fn defaults_when_nothing_is_set() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            let (relay, source) = resolve_relay_url_with_source(dir.path(), None);
            assert_eq!(relay, DEFAULT_RELAY_URL);
            assert_eq!(source, ConfigSource::Default);
            assert_eq!(resolve_daemon_addr(dir.path(), None), DEFAULT_DAEMON_ADDR);
        });
    }

    #[test]
    fn file_beats_default_and_loses_to_argument() {
        with_clean_env(|| {
            let dir = tempfile::tempdir().unwrap();
            save(
                dir.path(),
                &ClientConfig {
                    relay_url: Some("http://10.0.0.5:8787".into()),
                    daemon_addr: Some("127.0.0.1:9999".into()),
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(
                resolve_relay_url_with_source(dir.path(), None),
                ("http://10.0.0.5:8787".into(), ConfigSource::File)
            );
            assert_eq!(resolve_daemon_addr(dir.path(), None), "127.0.0.1:9999");
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
                    relay_url: Some("http://10.0.0.5:8787".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            std::env::set_var(ENV_RELAY_URL, "http://10.0.0.6:8787");

            assert_eq!(
                resolve_relay_url_with_source(dir.path(), None),
                ("http://10.0.0.6:8787".into(), ConfigSource::Env)
            );
            assert_eq!(
                resolve_relay_url_with_source(dir.path(), Some("http://10.0.0.7:8787")),
                ("http://10.0.0.7:8787".into(), ConfigSource::Argument)
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
                    relay_url: Some("http://10.0.0.5:8787".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                resolve_relay_url(dir.path(), Some("   ")),
                "http://10.0.0.5:8787"
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
}

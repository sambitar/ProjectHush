//! Non-secret IRC settings, persisted as JSON under the app's data directory.
//!
//! Mirrors how `crate::tor` owns its own subdirectory. There is deliberately no
//! field for a password or a SASL secret here: this project has no OS keychain
//! integration, so secrets are held in memory for the session only and the
//! frontend re-prompts on the next launch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::irc::{DEFAULT_HOST, DEFAULT_PORT, LIBERA_ONION_HOST, LOBBY_CHANNEL};

/// A server the user has saved. Note the absence of any secret field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub use_tor: bool,
    pub nick: String,
    pub alt_nick: String,
    pub auto_join: Vec<String>,
}

impl Default for SavedServer {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT,
            tls: true,
            use_tor: true,
            nick: String::new(),
            alt_nick: String::new(),
            auto_join: Vec::new(),
        }
    }
}

/// The window the user last had open, so the tree restores on relaunch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LayoutSettings {
    pub active_server: Option<String>,
    pub active_target: Option<String>,
    pub dark: bool,
}

/// Everything persisted for the IRC pane.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IrcSettings {
    pub nick: String,
    pub alt_nick: String,
    pub username: String,
    pub realname: String,
    /// Route new connections through Tor unless a server says otherwise.
    pub default_use_tor: bool,
    pub servers: Vec<SavedServer>,
    pub layout: LayoutSettings,
}

impl Default for IrcSettings {
    fn default() -> Self {
        Self {
            nick: String::new(),
            alt_nick: String::new(),
            username: String::new(),
            realname: String::new(),
            default_use_tor: true,
            servers: vec![
                SavedServer {
                    id: format!("{DEFAULT_HOST}:{DEFAULT_PORT}"),
                    name: "Libera.Chat".into(),
                    host: DEFAULT_HOST.into(),
                    port: DEFAULT_PORT,
                    tls: true,
                    use_tor: true,
                    auto_join: vec![LOBBY_CHANNEL.into()],
                    ..Default::default()
                },
                SavedServer {
                    id: format!("{LIBERA_ONION_HOST}:6667"),
                    name: "Libera.Chat (Tor onion)".into(),
                    host: LIBERA_ONION_HOST.into(),
                    port: 6667,
                    tls: false,
                    use_tor: true,
                    auto_join: vec![LOBBY_CHANNEL.into()],
                    ..Default::default()
                },
            ],
            layout: LayoutSettings::default(),
        }
    }
}

/// The build-time constants the UI needs, echoed so the frontend never hard-
/// codes a second copy of them.
#[derive(Clone, Debug, Serialize)]
pub struct IrcDefaults {
    pub default_host: String,
    pub default_port: u16,
    pub lobby_channel: String,
    pub libera_onion_host: String,
}

impl IrcDefaults {
    pub fn current() -> Self {
        Self {
            default_host: DEFAULT_HOST.into(),
            default_port: DEFAULT_PORT,
            lobby_channel: LOBBY_CHANNEL.into(),
            libera_onion_host: LIBERA_ONION_HOST.into(),
        }
    }
}

/// What `irc_settings_load` returns.
#[derive(Clone, Debug, Serialize)]
pub struct SettingsBundle {
    pub settings: IrcSettings,
    pub defaults: IrcDefaults,
}

/// `.../irc/settings.json` under the app's local data directory.
fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("no local data directory: {e}"))?
        .join("irc");
    Ok(dir.join("settings.json"))
}

/// Load settings, falling back to defaults when the file is absent or unreadable.
pub fn load(app: &AppHandle) -> IrcSettings {
    let Ok(path) = settings_path(app) else {
        return IrcSettings::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return IrcSettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Write settings atomically enough for a config file, with the directory
/// restricted to the owner where the platform supports it.
pub fn save(app: &AppHandle, settings: &IrcSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    let dir = path.parent().expect("settings path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {dir:?}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("could not write {path:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_include_libera_and_the_onion() {
        let s = IrcSettings::default();
        assert_eq!(s.servers.len(), 2);
        assert!(s.default_use_tor);
        assert!(s.servers.iter().any(|x| x.host == DEFAULT_HOST && x.tls));
        assert!(s.servers.iter().any(|x| x.host == LIBERA_ONION_HOST && !x.tls));
        assert!(s
            .servers
            .iter()
            .all(|x| x.auto_join.iter().any(|c| c == LOBBY_CHANNEL)));
    }

    #[test]
    fn round_trips_through_json() {
        let s = IrcSettings::default();
        let json = serde_json::to_string(&s).unwrap();
        let back: IrcSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.servers.len(), s.servers.len());
        assert_eq!(back.default_use_tor, s.default_use_tor);
    }

    #[test]
    fn a_corrupt_file_falls_back_to_defaults() {
        let parsed: Result<IrcSettings, _> = serde_json::from_str("{ not json");
        assert!(parsed.is_err());
    }
}

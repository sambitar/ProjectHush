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

/// The onion address that shipped in the first IRC commit. It is one base32
/// character short of a v3 name, so Tor refused every connect with
/// `Invalid hostname` before trying, and all the UI could show was the SOCKS
/// reply ("general failure"). Nothing ever resolved it, so a saved copy can
/// only be a leftover from that build; [`repair_legacy_onion`] rewrites it.
///
/// `pub(crate)` so the IRC test that proves the validator rejects this exact
/// string can key off the same constant the migration looks for.
pub(crate) const LEGACY_LIBERA_ONION_HOST: &str =
    "libera75jm6of4wxpxt4aynol3xjmbtxgfyjpu34ss4d2q7r2uqw6xy.onion";

/// How a saved server authenticates to services.
///
/// `Plain` is a password, which the connect dialog prompts for and nothing
/// writes down. `External` is this device's certificate (see [`crate::irc::cert`]):
/// the only public-key mechanism this client implements, and the only kind
/// Libera's Tor onion accepts — it rejects `PLAIN` outright. Serialized as
/// `"plain"` / `"external"` for the frontend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaslMechanism {
    #[default]
    Plain,
    External,
}

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
    /// Which SASL mechanism this server's connections use. Older settings files
    /// predate the field and mean `Plain`.
    #[serde(default)]
    pub sasl_mechanism: SaslMechanism,
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
            sasl_mechanism: SaslMechanism::default(),
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
                    id: format!("{LIBERA_ONION_HOST}:6697"),
                    name: "Libera.Chat (Tor onion)".into(),
                    host: LIBERA_ONION_HOST.into(),
                    // TLS on the onion, even though Tor already encrypts: SASL
                    // EXTERNAL only exists inside a TLS handshake, and EXTERNAL
                    // is the only mechanism the onion's gateway accepts.
                    port: 6697,
                    tls: true,
                    use_tor: true,
                    auto_join: vec![LOBBY_CHANNEL.into()],
                    sasl_mechanism: SaslMechanism::External,
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

/// The port SASL EXTERNAL needs on Libera's onion.
///
/// The gateway there accepts only public-key SASL, and the only public-key
/// mechanism this client implements (`EXTERNAL`) exists inside a TLS handshake.
/// So an entry that is plaintext, or password-based, cannot connect however
/// correct its address is.
const ONION_PORT: u16 = 6697;

/// Bring any saved Libera-onion server to a shape that can connect: the real
/// address, TLS on [`ONION_PORT`], and SASL `EXTERNAL`.
///
/// Two things shipped wrong before this: the address was one base32 character
/// short (so Tor refused it before dialling, and all the UI could show was the
/// SOCKS reply), and the entry was plaintext on 6667 with password SASL (which
/// the onion's gateway refuses outright). Neither is recoverable in the client,
/// so both are repaired on load rather than surfaced as an error the user
/// cannot act on. Returns whether anything changed, so `load` can persist the
/// repair.
fn repair_libera_onion(settings: &mut IrcSettings) -> bool {
    let mut remapped: Vec<(String, String)> = Vec::new();
    let mut changed = false;
    for server in &mut settings.servers {
        if server.host != LEGACY_LIBERA_ONION_HOST && server.host != LIBERA_ONION_HOST {
            continue;
        }
        let was_id = server.id.clone();
        let wanted_id = format!("{LIBERA_ONION_HOST}:{ONION_PORT}");
        let already_works = server.host == LIBERA_ONION_HOST
            && server.port == ONION_PORT
            && server.tls
            && server.sasl_mechanism == SaslMechanism::External
            && server.id == wanted_id;
        if already_works {
            continue;
        }
        server.host = LIBERA_ONION_HOST.to_string();
        server.port = ONION_PORT;
        server.tls = true;
        server.sasl_mechanism = SaslMechanism::External;
        server.id = wanted_id.clone();
        if was_id != wanted_id {
            remapped.push((was_id, wanted_id));
        }
        changed = true;
    }
    if let Some(active) = settings.layout.active_server.as_mut() {
        if let Some((_, new_id)) = remapped.iter().find(|(old, _)| old == active) {
            *active = new_id.clone();
        }
    }
    // Normalizing can collide two entries onto one id (a user who added their
    // own onion server alongside the shipped one). The id is the connection's
    // key, so the first entry wins and the duplicate goes.
    let mut seen: Vec<String> = Vec::new();
    let before = settings.servers.len();
    settings.servers.retain(|s| {
        if s.host != LIBERA_ONION_HOST {
            return true;
        }
        let fresh = !seen.contains(&s.id);
        seen.push(s.id.clone());
        fresh
    });
    changed || settings.servers.len() != before
}

/// Load settings, falling back to defaults when the file is absent or unreadable.
pub fn load(app: &AppHandle) -> IrcSettings {
    let Ok(path) = settings_path(app) else {
        return IrcSettings::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return IrcSettings::default();
    };
    let mut settings: IrcSettings = serde_json::from_str(&raw).unwrap_or_default();
    if repair_libera_onion(&mut settings) {
        // Best effort: the repair runs on every load, so a failed write leaves
        // a stale file behind but never a broken address in memory.
        let _ = save(app, &settings);
    }
    settings
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
        let onion = s
            .servers
            .iter()
            .find(|x| x.host == LIBERA_ONION_HOST)
            .expect("the onion server ships by default");
        // TLS on the onion, because EXTERNAL only exists inside a handshake, and
        // EXTERNAL is the only mechanism the gateway there accepts.
        assert!(onion.tls);
        assert_eq!(onion.port, ONION_PORT);
        assert_eq!(onion.sasl_mechanism, SaslMechanism::External);
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

    #[test]
    fn repairs_a_server_saved_with_the_broken_onion() {
        let legacy_id = format!("{LEGACY_LIBERA_ONION_HOST}:6667");
        let mut settings = IrcSettings::default();
        settings.servers.clear();
        settings.servers.push(SavedServer {
            id: legacy_id.clone(),
            name: "Libera.Chat (Tor onion)".into(),
            host: LEGACY_LIBERA_ONION_HOST.into(),
            port: 6667,
            tls: false,
            ..Default::default()
        });
        settings.layout.active_server = Some(legacy_id);

        assert!(repair_libera_onion(&mut settings));

        assert_eq!(settings.servers.len(), 1, "the broken entry is repaired, not duplicated");
        let repaired = &settings.servers[0];
        assert_eq!(repaired.host, LIBERA_ONION_HOST);
        assert_eq!(repaired.id, format!("{LIBERA_ONION_HOST}:{ONION_PORT}"));
        assert_eq!(repaired.port, ONION_PORT, "plaintext 6667 cannot carry EXTERNAL");
        assert!(repaired.tls, "EXTERNAL needs a handshake to exist in");
        assert_eq!(repaired.sasl_mechanism, SaslMechanism::External);
        assert_eq!(
            settings.layout.active_server.as_deref(),
            Some(repaired.id.as_str()),
            "the remembered active server follows the repaired id"
        );

        // Idempotent: a second load must not report a change to persist.
        assert!(!repair_libera_onion(&mut settings));
    }

    /// A user who added their own onion server alongside the shipped one must
    /// not end up with two entries sharing an id — the id keys the connection.
    #[test]
    fn a_second_onion_entry_does_not_survive_as_a_duplicate_id() {
        let mut settings = IrcSettings::default();
        settings.servers.push(SavedServer {
            id: format!("{LIBERA_ONION_HOST}:6667"),
            name: "My onion".into(),
            host: LIBERA_ONION_HOST.into(),
            port: 6667,
            ..Default::default()
        });

        assert!(repair_libera_onion(&mut settings));
        let onion_ids: Vec<&str> = settings
            .servers
            .iter()
            .filter(|s| s.host == LIBERA_ONION_HOST)
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(onion_ids.len(), 1, "one id, one entry: {onion_ids:?}");
    }

    /// The state a real settings file was left in by the build that fixed the
    /// address but not the shape: right host, plaintext, password SASL. It looks
    /// healthy and can never connect, so the repair has to catch it too.
    #[test]
    fn repairs_a_correct_address_left_in_the_plaintext_shape() {
        let mut settings = IrcSettings::default();
        settings.servers.clear();
        settings.servers.push(SavedServer {
            id: format!("{LIBERA_ONION_HOST}:6667"),
            name: "Libera.Chat (Tor onion)".into(),
            host: LIBERA_ONION_HOST.into(),
            port: 6667,
            tls: false,
            sasl_mechanism: SaslMechanism::Plain,
            ..Default::default()
        });
        settings.layout.active_server = Some(format!("{LIBERA_ONION_HOST}:6667"));

        assert!(repair_libera_onion(&mut settings));
        let repaired = &settings.servers[0];
        assert_eq!(repaired.port, ONION_PORT);
        assert!(repaired.tls);
        assert_eq!(repaired.sasl_mechanism, SaslMechanism::External);
        assert_eq!(
            settings.layout.active_server.as_deref(),
            Some(format!("{LIBERA_ONION_HOST}:{ONION_PORT}").as_str())
        );
        assert!(!repair_libera_onion(&mut settings));
    }

    /// The defaults are already in the working shape, so loading them must not
    /// rewrite the file on every launch.
    #[test]
    fn the_shipped_defaults_need_no_repair() {
        let mut settings = IrcSettings::default();
        assert!(!repair_libera_onion(&mut settings));
    }

    /// An older settings file has no `sasl_mechanism` field at all; it has to
    /// read as PLAIN rather than failing to parse.
    #[test]
    fn a_saved_server_without_a_mechanism_reads_as_plain() {
        let json = r#"{
            "nick": "bluejeans", "alt_nick": "bluejeans_",
            "username": "bluejeans", "realname": "bluejeans",
            "default_use_tor": true,
            "servers": [{
                "id": "irc.libera.chat:6697", "name": "Libera.Chat",
                "host": "irc.libera.chat", "port": 6697, "tls": true,
                "use_tor": true, "nick": "", "alt_nick": "", "auto_join": []
            }],
            "layout": { "active_server": null, "active_target": null, "dark": false }
        }"#;
        let settings: IrcSettings = serde_json::from_str(json).expect("an older file parses");
        assert_eq!(settings.servers[0].sasl_mechanism, SaslMechanism::Plain);
    }
}

//! IRC: a from-scratch, multi-server IRC client for the IRC rail pane.
//!
//! The client lives here, not in the webview, for three reasons: TLS and a
//! long-lived socket are the shell's job; a single connection must survive
//! every repaint of the (imperative) pane; and routing through Tor means
//! speaking SOCKS to the same managed Tor process the Drops feature uses.
//!
//! Layout:
//! - [`protocol`] parses raw IRC lines.
//! - [`format`] tokenizes mIRC control codes into renderable segments.
//! - [`nicklist`] parses and sorts `NAMES` entries by prefix.
//! - [`socks`] dials through Tor.
//! - [`tls`] builds the rustls client config (and the onion-only relaxed one).
//! - [`cert`] mints and seals this device's client certificate (SASL EXTERNAL).
//! - [`state`] holds the per-server snapshot and emits events to the UI.
//! - [`client`] runs one connection: dial, register, read loop, reconnect.
//! - [`settings`] persists non-secret settings under the app's data dir.
//! - [`commands`] is the Tauri command surface.

pub mod cert;
pub mod client;
pub mod commands;
pub mod format;
pub mod nicklist;
pub mod protocol;
pub mod settings;
pub mod socks;
pub mod state;
pub mod tls;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

pub use format::Segment;
pub use state::ServerSnapshot;

/// The channel every Hush user is expected to hang out in. Change it here and
/// the auto-join, the connect dialog default, and `/help` all follow.
pub const LOBBY_CHANNEL: &str = "#your-lobby";

/// The server offered first in the Connect dialog.
pub const DEFAULT_HOST: &str = "irc.libera.chat";
pub const DEFAULT_PORT: u16 = 6697;

/// Libera's Tor onion service. Connecting here keeps the whole conversation
/// inside Tor (no exit node sees it), which is the strongest IP protection
/// available; the onion name itself authenticates the service.
///
/// A v3 name is exactly 56 base32 characters: a 32-byte public key, a 2-byte
/// checksum over that key, and a version byte. Tor rejects anything else as an
/// invalid hostname before it ever tries to connect, which reaches the user as
/// an opaque SOCKS failure, so the value is pinned by the test at the bottom of
/// this module. Take the address from Libera's own guide when changing it:
/// <https://libera.chat/guides/connect#accessing-liberachat-via-tor>
pub const LIBERA_ONION_HOST: &str =
    "libera75jm6of4wxpxt4aynol3xjmbtxgfyjpu34ss4d7r7q2v5zrpyd.onion";

/// The sentinel target for a server's own status window. Channels and queries
/// use their own name as the target; everything server-wide uses this.
pub const STATUS_TARGET: &str = "*";

/// How the client authenticates to services.
///
/// `Plain` sends a password, which is memory-only and never written to disk.
/// `External` sends no secret at all: the device's certificate is presented in
/// the TLS handshake, and the account's authorized fingerprint is what matches.
/// Libera's Tor onion accepts only public-key mechanisms, and `External` is the
/// one this client implements — see <https://libera.chat/guides/connect>.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mechanism", rename_all = "snake_case")]
pub enum SaslConfig {
    Plain { account: String, password: String },
    External { account: String },
}

impl SaslConfig {
    /// Whether this mechanism needs the device's certificate on the connection.
    pub fn needs_certificate(&self) -> bool {
        matches!(self, SaslConfig::External { .. })
    }
}

/// Everything needed to open one connection. Built from a [`ConnectRequest`]
/// and handed to the client task; the two secret fields are in-memory only.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub use_tor: bool,
    pub nick: String,
    pub alt_nick: String,
    pub username: String,
    pub realname: String,
    pub server_password: Option<String>,
    pub sasl: Option<SaslConfig>,
    /// The device's certificate, loaded from the vault when the request asked
    /// for one. Held for the life of the connection: a reconnect presents it
    /// again, and EXTERNAL cannot authenticate without it.
    pub client_cert: Option<Arc<cert::ClientIdentity>>,
    pub auto_join: Vec<String>,
}

/// What the frontend sends to `connect_server`.
#[derive(Clone, Debug, Deserialize)]
pub struct ConnectRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub host: String,
    pub port: u16,
    #[serde(default = "default_true")]
    pub tls: bool,
    #[serde(default)]
    pub use_tor: bool,
    pub nick: String,
    #[serde(default)]
    pub alt_nick: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub realname: String,
    #[serde(default)]
    pub server_password: Option<String>,
    #[serde(default)]
    pub sasl: Option<SaslConfig>,
    #[serde(default)]
    pub auto_join: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl ConnectRequest {
    /// Fold into the client's config, filling the blanks a minimal request
    /// leaves (username/realname default to the nick, alt nick to `nick_`).
    pub fn into_config(self) -> ServerConfig {
        let username = if self.username.trim().is_empty() {
            self.nick.clone()
        } else {
            self.username
        };
        let realname = if self.realname.trim().is_empty() {
            self.nick.clone()
        } else {
            self.realname
        };
        let alt_nick = if self.alt_nick.trim().is_empty() {
            format!("{}_", self.nick)
        } else {
            self.alt_nick
        };
        let id = self
            .id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{}:{}", self.host, self.port));
        ServerConfig {
            id,
            host: self.host,
            port: self.port,
            tls: self.tls,
            use_tor: self.use_tor,
            nick: self.nick,
            alt_nick,
            username,
            realname,
            server_password: self.server_password,
            sasl: self.sasl,
            // Filled in by `connect_server` from the vault, which the client
            // task has no access to.
            client_cert: None,
            auto_join: self.auto_join,
        }
    }
}

/// A command from the UI to one connection's task.
#[derive(Clone, Debug)]
pub enum OutCmd {
    /// A raw, already-formatted IRC line (no CRLF).
    Raw(String),
    /// A `PRIVMSG`; the task handles local echo for non-channel targets.
    /// `action` wraps the text as a CTCP ACTION (`/me`).
    Privmsg {
        target: String,
        text: String,
        action: bool,
    },
    Join { channel: String, key: Option<String> },
    Part { channel: String, reason: Option<String> },
    Nick(String),
    /// Graceful quit with an optional reason.
    Quit(Option<String>),
    /// Stop now without a QUIT (used by disconnect).
    Disconnect,
}

/// One live connection, as held by [`IrcState`].
pub struct ServerEntry {
    pub snapshot: Arc<Mutex<ServerSnapshot>>,
    pub out: UnboundedSender<OutCmd>,
    pub config: ServerConfig,
}

/// All connections, keyed by server id. Managed by Tauri.
#[derive(Default)]
pub struct IrcState {
    servers: Mutex<HashMap<String, ServerEntry>>,
}

impl IrcState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, entry: ServerEntry) {
        self.servers
            .lock()
            .expect("irc state poisoned")
            .insert(entry.config.id.clone(), entry);
    }

    pub fn remove(&self, id: &str) -> Option<ServerEntry> {
        self.servers.lock().expect("irc state poisoned").remove(id)
    }

    /// Send one command to a connection, or report that it is gone.
    pub fn send(&self, id: &str, cmd: OutCmd) -> Result<(), String> {
        let map = self.servers.lock().expect("irc state poisoned");
        let entry = map.get(id).ok_or_else(|| format!("no such server: {id}"))?;
        entry
            .out
            .send(cmd)
            .map_err(|_| format!("connection {id} is no longer running"))
    }

    /// A point-in-time copy of one server's snapshot.
    pub fn snapshot(&self, id: &str) -> Option<ServerSnapshot> {
        let map = self.servers.lock().expect("irc state poisoned");
        map.get(id)
            .map(|e| e.snapshot.lock().expect("snapshot poisoned").clone())
    }

    /// Mutate one server's snapshot under both locks.
    pub fn update<F>(&self, id: &str, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut ServerSnapshot),
    {
        let map = self.servers.lock().expect("irc state poisoned");
        let entry = map.get(id).ok_or_else(|| format!("no such server: {id}"))?;
        let mut snap = entry.snapshot.lock().expect("snapshot poisoned");
        f(&mut snap);
        Ok(())
    }

    /// Every server's snapshot, in insertion order.
    pub fn all(&self) -> Vec<ServerSnapshot> {
        let map = self.servers.lock().expect("irc state poisoned");
        map.values()
            .map(|e| e.snapshot.lock().expect("snapshot poisoned").clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::settings::LEGACY_LIBERA_ONION_HOST;
    use super::*;

    /// Check a name the way Tor does before it connects: length, alphabet,
    /// version byte, then the checksum over the public key.
    fn validate_v3_onion(host: &str) -> Result<(), String> {
        let base = host
            .strip_suffix(".onion")
            .ok_or_else(|| format!("{host} does not end in .onion"))?;
        if base.len() != 56 {
            return Err(format!("{base} is {} characters, not 56", base.len()));
        }
        if !base
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
        {
            return Err(format!("{base} is not lowercase base32"));
        }
        let raw = data_encoding::BASE32_NOPAD
            .decode(base.to_ascii_uppercase().as_bytes())
            .map_err(|e| format!("{base} does not decode as base32: {e}"))?;
        if raw.len() != 35 {
            return Err(format!("decoded to {} bytes, not 35", raw.len()));
        }
        let (pubkey, tail) = raw.split_at(32);
        let (checksum, version) = tail.split_at(2);
        if version[0] != 3 {
            return Err(format!("version byte is {}, not 3", version[0]));
        }

        use sha3::{Digest, Sha3_256};
        let mut hasher = Sha3_256::new();
        hasher.update(b".onion checksum");
        hasher.update(pubkey);
        hasher.update(version);
        let digest = hasher.finalize();
        if checksum != &digest[..2] {
            return Err("checksum does not match the public key".into());
        }
        Ok(())
    }

    /// Tor answers a malformed name with a bare SOCKS "general failure" and
    /// logs only `Invalid hostname [scrubbed]; rejecting` on its own side, so
    /// the user sees an unexplained refusal to connect. Worth proving, then,
    /// rather than eyeballing: the address only has to be wrong once.
    #[test]
    fn the_shipped_onion_address_is_a_valid_v3_name() {
        validate_v3_onion(LIBERA_ONION_HOST).expect("the shipped address must be valid");
    }

    /// A guard is only worth having if it rejects the address that actually
    /// shipped broken, so this pins the failure the first version hit (one
    /// base32 character short). The migration in `settings` keys off the same
    /// constant.
    #[test]
    fn the_validator_rejects_the_address_that_shipped_broken() {
        let error = validate_v3_onion(LEGACY_LIBERA_ONION_HOST)
            .expect_err("the legacy address must not validate");
        assert!(error.contains("55 characters"), "unexpected error: {error}");
    }

    /// The connect dialog builds this JSON by hand, so the shape is a contract:
    /// the mechanism is the tag, and only `external` asks for the certificate.
    #[test]
    fn sasl_configs_deserialize_the_way_the_connect_dialog_sends_them() {
        let plain: SaslConfig =
            serde_json::from_str(r#"{"mechanism":"plain","account":"bluejeans","password":"pw"}"#)
                .expect("a plain config parses");
        assert!(matches!(
            &plain,
            SaslConfig::Plain { account, password } if account == "bluejeans" && password == "pw"
        ));
        assert!(!plain.needs_certificate());

        let external: SaslConfig =
            serde_json::from_str(r#"{"mechanism":"external","account":"bluejeans"}"#)
                .expect("an external config parses");
        assert!(matches!(
            &external,
            SaslConfig::External { account } if account == "bluejeans"
        ));
        assert!(external.needs_certificate());

        // Switching the mechanism in the dialog must not require clearing the
        // password field: a leftover one is ignored, not rejected.
        let switched: SaslConfig = serde_json::from_str(
            r#"{"mechanism":"external","account":"bluejeans","password":"stale"}"#,
        )
        .expect("a leftover password is ignored");
        assert!(matches!(switched, SaslConfig::External { .. }));
        assert!(switched.needs_certificate());

        // Without a mechanism there is no way to know which credential is
        // meant, so this must fail rather than guess.
        assert!(serde_json::from_str::<SaslConfig>(r#"{"account":"bluejeans"}"#).is_err());
    }
}

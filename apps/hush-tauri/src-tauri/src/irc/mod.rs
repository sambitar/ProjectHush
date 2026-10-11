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
//! - [`state`] holds the per-server snapshot and emits events to the UI.
//! - [`client`] runs one connection: dial, register, read loop, reconnect.
//! - [`settings`] persists non-secret settings under the app's data dir.
//! - [`commands`] is the Tauri command surface.

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
pub const LIBERA_ONION_HOST: &str =
    "libera75jm6of4wxpxt4aynol3xjmbtxgfyjpu34ss4d2q7r2uqw6xy.onion";

/// The sentinel target for a server's own status window. Channels and queries
/// use their own name as the target; everything server-wide uses this.
pub const STATUS_TARGET: &str = "*";

/// A user's credentials for SASL PLAIN. Never written to disk.
#[derive(Clone, Debug, Deserialize)]
pub struct SaslConfig {
    pub account: String,
    pub password: String,
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

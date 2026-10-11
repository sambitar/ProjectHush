//! Per-server snapshot and the event bridge to the UI.
//!
//! The snapshot is the backend's authoritative view of one connection (nick,
//! channels with topic and names, queries, MOTD). Every change is pushed to the
//! webview as an `irc://*` event tagged with the server id and a target (a
//! channel, a query nick, or [`STATUS_TARGET`]); the frontend never polls.

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::irc::nicklist::NickEntry;
use crate::irc::{Segment, STATUS_TARGET};

/// One channel's state.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ChannelState {
    pub name: String,
    pub topic: String,
    pub topic_who: Option<String>,
    pub names: Vec<NickEntry>,
    pub modes: String,
    /// False between the user asking to join and the server confirming.
    pub joined: bool,
}

/// One open query (private message) window.
#[derive(Clone, Debug, Default, Serialize)]
pub struct QueryState {
    pub nick: String,
}

/// Everything the backend knows about one connection.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ServerSnapshot {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub use_tor: bool,
    pub nick: String,
    pub connected: bool,
    pub registered: bool,
    /// A human phase for the tree: "Connecting", "Connected", "Reconnecting"…
    pub phase: String,
    pub channels: Vec<ChannelState>,
    pub queries: Vec<QueryState>,
}

impl ServerSnapshot {
    /// A channel by name, case-insensitively.
    pub fn channel(&self, name: &str) -> Option<&ChannelState> {
        self.channels
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn channel_mut(&mut self, name: &str) -> Option<&mut ChannelState> {
        self.channels
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// A channel, created if it does not exist yet (e.g. a JOIN we asked for).
    pub fn ensure_channel(&mut self, name: &str) -> &mut ChannelState {
        if let Some(i) = self
            .channels
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
        {
            return &mut self.channels[i];
        }
        self.channels.push(ChannelState {
            name: name.to_string(),
            ..Default::default()
        });
        self.channels.last_mut().expect("just pushed")
    }

    /// Whether `name` is a channel we are in (or joining).
    pub fn is_channel(&self, name: &str) -> bool {
        self.channels
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Add or update one nick in a channel's list, keeping it sorted.
    pub fn upsert_nick(&mut self, channel: &str, entry: NickEntry) {
        let Some(ch) = self.channel_mut(channel) else {
            return;
        };
        match ch
            .names
            .iter_mut()
            .find(|n| n.nick.eq_ignore_ascii_case(&entry.nick))
        {
            Some(existing) => *existing = entry,
            None => ch.names.push(entry),
        }
        crate::irc::nicklist::sort(&mut ch.names);
    }

    /// Remove a nick from every channel (on PART/QUIT/KICK).
    pub fn remove_nick(&mut self, nick: &str) {
        for ch in &mut self.channels {
            ch.names.retain(|n| !n.nick.eq_ignore_ascii_case(nick));
        }
    }

    /// Rename a nick everywhere (on NICK).
    pub fn rename_nick(&mut self, old: &str, new: &str) {
        for ch in &mut self.channels {
            for n in &mut ch.names {
                if n.nick.eq_ignore_ascii_case(old) {
                    n.nick = new.to_string();
                }
            }
            crate::irc::nicklist::sort(&mut ch.names);
        }
        for q in &mut self.queries {
            if q.nick.eq_ignore_ascii_case(old) {
                q.nick = new.to_string();
            }
        }
    }

    /// Open a query window if it is not already open.
    pub fn ensure_query(&mut self, nick: &str) -> bool {
        if self.queries.iter().any(|q| q.nick.eq_ignore_ascii_case(nick)) {
            return false;
        }
        self.queries.push(QueryState {
            nick: nick.to_string(),
        });
        true
    }
}

/// Whether `plain_text` mentions `nick` as a whole word (case-insensitive).
pub fn mentions_me(plain_text: &str, nick: &str) -> bool {
    if nick.is_empty() {
        return false;
    }
    let hay = plain_text.to_lowercase();
    let needle = nick.to_lowercase();
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(rel) = hay[from..].find(&needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.len();
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// The event emitter for one connection.
#[derive(Clone)]
pub struct Events {
    app: AppHandle,
    server_id: String,
}

impl Events {
    pub fn new(app: AppHandle, server_id: impl Into<String>) -> Self {
        Self {
            app,
            server_id: server_id.into(),
        }
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        // The window may already be gone during shutdown; a failed emit is not
        // worth surfacing or retrying.
        let _ = self.app.emit(event, payload);
    }

    /// The connection's own state changed (connect/disconnect/phase/nick).
    pub fn server(&self, snap: &ServerSnapshot) {
        self.emit(
            "irc://server",
            json!({
                "server_id": self.server_id,
                "id": snap.id,
                "host": snap.host,
                "port": snap.port,
                "tls": snap.tls,
                "use_tor": snap.use_tor,
                "nick": snap.nick,
                "connected": snap.connected,
                "registered": snap.registered,
                "phase": snap.phase,
            }),
        );
    }

    /// A line for a status window or a channel/query window.
    pub fn status(&self, target: &str, text: &str, level: &str) {
        self.emit(
            "irc://status",
            json!({
                "server_id": self.server_id,
                "target": target,
                "text": text,
                "level": level,
            }),
        );
    }

    /// A chat message. `segments` is the styled body; `kind` is
    /// `say`/`action`/`notice`.
    pub fn message(
        &self,
        target: &str,
        from: &str,
        kind: &str,
        segments: &[Segment],
        highlight: bool,
    ) {
        self.emit(
            "irc://message",
            json!({
                "server_id": self.server_id,
                "target": target,
                "from": from,
                "kind": kind,
                "segments": segments,
                "highlight": highlight,
                "ts": now_ms(),
            }),
        );
    }

    pub fn join(&self, channel: &str, nick: &str) {
        self.emit(
            "irc://join",
            json!({ "server_id": self.server_id, "target": channel, "nick": nick }),
        );
    }

    pub fn part(&self, channel: &str, nick: &str, reason: &str) {
        self.emit(
            "irc://part",
            json!({ "server_id": self.server_id, "target": channel, "nick": nick, "reason": reason }),
        );
    }

    pub fn quit(&self, nick: &str, reason: &str) {
        self.emit(
            "irc://quit",
            json!({ "server_id": self.server_id, "target": STATUS_TARGET, "nick": nick, "reason": reason }),
        );
    }

    pub fn nick(&self, old: &str, new: &str) {
        self.emit(
            "irc://nick",
            json!({ "server_id": self.server_id, "target": STATUS_TARGET, "old": old, "new": new }),
        );
    }

    pub fn topic(&self, channel: &str, topic: &str, who: Option<&str>) {
        self.emit(
            "irc://topic",
            json!({ "server_id": self.server_id, "target": channel, "topic": topic, "who": who }),
        );
    }

    pub fn names(&self, channel: &str, names: &[NickEntry]) {
        self.emit(
            "irc://names",
            json!({ "server_id": self.server_id, "target": channel, "names": names }),
        );
    }

    pub fn mode(&self, target: &str, by: &str, modes: &str, args: &str) {
        self.emit(
            "irc://mode",
            json!({ "server_id": self.server_id, "target": target, "by": by, "modes": modes, "args": args }),
        );
    }

    pub fn error(&self, target: &str, text: &str) {
        self.emit(
            "irc://error",
            json!({ "server_id": self.server_id, "target": target, "text": text }),
        );
    }

    /// One entry of a `/LIST` reply.
    pub fn list_entry(&self, channel: &str, users: usize, topic: &str) {
        self.emit(
            "irc://list",
            json!({ "server_id": self.server_id, "channel": channel, "users": users, "topic": topic }),
        );
    }

    pub fn list_end(&self) {
        self.emit("irc://list_end", json!({ "server_id": self.server_id }));
    }
}

/// Milliseconds since the Unix epoch, for message timestamps.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_mention_as_a_whole_word() {
        assert!(mentions_me("hey alice, are you there?", "alice"));
        assert!(mentions_me("ALICE: ping", "alice"));
        assert!(!mentions_me("malice is not a mention", "alice"));
        assert!(!mentions_me("alicebob said hi", "alice"));
        assert!(!mentions_me("nothing here", "alice"));
        assert!(!mentions_me("alice", ""));
    }

    #[test]
    fn channel_lookup_is_case_insensitive() {
        let mut snap = ServerSnapshot::default();
        snap.ensure_channel("#Room");
        assert!(snap.is_channel("#room"));
        assert!(snap.channel("#ROOM").is_some());
    }

    #[test]
    fn nick_upsert_keeps_the_list_sorted() {
        let mut snap = ServerSnapshot::default();
        snap.ensure_channel("#c");
        snap.upsert_nick("#c", NickEntry { nick: "carol".into(), prefixes: String::new() });
        snap.upsert_nick("#c", NickEntry { nick: "alice".into(), prefixes: "@".into() });
        snap.upsert_nick("#c", NickEntry { nick: "bob".into(), prefixes: String::new() });
        // Re-adding alice with new prefixes must not duplicate her.
        snap.upsert_nick("#c", NickEntry { nick: "alice".into(), prefixes: "@+".into() });
        let names: Vec<_> = snap
            .channel("#c")
            .unwrap()
            .names
            .iter()
            .map(|n| n.nick.as_str())
            .collect();
        assert_eq!(names, ["alice", "bob", "carol"]);
        assert_eq!(snap.channel("#c").unwrap().names[0].prefixes, "@+");
    }

    #[test]
    fn rename_and_remove_touch_every_channel() {
        let mut snap = ServerSnapshot::default();
        snap.ensure_channel("#a");
        snap.ensure_channel("#b");
        snap.upsert_nick("#a", NickEntry { nick: "old".into(), prefixes: String::new() });
        snap.upsert_nick("#b", NickEntry { nick: "old".into(), prefixes: String::new() });
        snap.rename_nick("old", "new");
        assert!(snap.channel("#a").unwrap().names.iter().any(|n| n.nick == "new"));
        assert!(snap.channel("#b").unwrap().names.iter().any(|n| n.nick == "new"));
        snap.remove_nick("new");
        assert!(snap.channel("#a").unwrap().names.is_empty());
        assert!(snap.channel("#b").unwrap().names.is_empty());
    }
}

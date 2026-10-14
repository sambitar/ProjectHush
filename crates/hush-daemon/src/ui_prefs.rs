use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiPrefs {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub avatars: HashMap<String, String>,
    #[serde(default)]
    pub muted: Vec<String>,
    #[serde(default)]
    pub blocked: Vec<String>,
    #[serde(default)]
    pub nick_overrides: HashMap<String, String>,
    /// Minutes of inactivity before the daemon locks; `0` disables idle auto-lock.
    #[serde(default = "default_auto_lock_minutes")]
    pub auto_lock_minutes: u32,
    /// Mail record ids the user has opened.
    ///
    /// Read/star/archive/trash are *UI* state, not vault state, so they sit
    /// beside mute and block instead of in the encrypted log: they are ids, not
    /// content, and rewriting the log on every read receipt would be a lot of
    /// disk churn for a checkbox. Ids are opaque, so this leaks counts at most.
    #[serde(default)]
    pub mail_read: Vec<String>,
    #[serde(default)]
    pub mail_starred: Vec<String>,
    #[serde(default)]
    pub mail_archived: Vec<String>,
    #[serde(default)]
    pub mail_trashed: Vec<String>,
    /// Whether arriving chat messages play a sound (global switch).
    #[serde(default = "default_true")]
    pub chat_sound: bool,
    /// Contacts whose arrival sound is off, set from the chat's own menu.
    #[serde(default)]
    pub chat_sound_muted: Vec<String>,
    /// When true, the daemon stops heartbeating presence so every contact sees
    /// this vault as offline while it is actually online.
    #[serde(default)]
    pub appear_offline: bool,
    /// The relay switch: the relay path should use the onion address through
    /// Tor. Lives here rather than only in the vault so the pre-login screen
    /// can read and flip it while the vault is locked; the vault's own copy
    /// (which decides actual traffic) is applied from this at unlock.
    #[serde(default)]
    pub relay_tor: bool,
    /// An onion address staged while the vault was locked (the pre-login
    /// attach field). Written into the vault at unlock, then cleared, so
    /// this stays a handoff and never a second source of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_onion_staged: Option<String>,
    /// Chat auto-delete policy: what the log trims on its own. Lives here so
    /// it survives lock/unlock and reads like the rest of the user's prefs.
    #[serde(default)]
    pub chat_auto_delete: ChatAutoDelete,
}

/// The chat auto-delete rules, as the Chat history dialog sets them. Both may
/// be off (keep everything), on together, or anything between: `daily` is the
/// "every day" switch, `keep_lines` the "old chats past XX lines go first"
/// cap.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAutoDelete {
    /// Delete every chat message older than the current UTC day.
    #[serde(default)]
    pub daily: bool,
    /// Keep at most this many messages per contact (oldest deleted first).
    /// `None` keeps everything the daily rule does not cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_lines: Option<u32>,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            avatars: HashMap::new(),
            muted: Vec::new(),
            blocked: Vec::new(),
            nick_overrides: HashMap::new(),
            auto_lock_minutes: default_auto_lock_minutes(),
            mail_read: Vec::new(),
            mail_starred: Vec::new(),
            mail_archived: Vec::new(),
            mail_trashed: Vec::new(),
            chat_sound: true,
            chat_sound_muted: Vec::new(),
            appear_offline: false,
            relay_tor: false,
            relay_onion_staged: None,
            chat_auto_delete: ChatAutoDelete::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "system".into()
}

fn default_auto_lock_minutes() -> u32 {
    15
}

pub fn auto_lock_duration(data_dir: &Path) -> Option<Duration> {
    let mins = load(data_dir).auto_lock_minutes;
    if mins == 0 {
        None
    } else {
        Some(Duration::from_secs(mins as u64 * 60))
    }
}

pub fn prefs_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ui-prefs.json")
}

pub fn load(data_dir: &Path) -> UiPrefs {
    let path = prefs_path(data_dir);
    let bytes = std::fs::read(&path).ok();
    bytes
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(data_dir: &Path, prefs: &UiPrefs) -> Result<(), String> {
    let path = prefs_path(data_dir);
    let json = serde_json::to_vec_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

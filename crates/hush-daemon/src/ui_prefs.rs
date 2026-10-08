use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
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

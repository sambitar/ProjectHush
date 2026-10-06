use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
}

fn default_theme() -> String {
    "system".into()
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

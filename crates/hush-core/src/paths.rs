use std::path::{Path, PathBuf};

/// Default persistent data root: `$XDG_DATA_HOME/hush` or `~/.local/share/hush`.
pub fn default_data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("hush");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/share/hush");
    }
    PathBuf::from(".hush-data")
}

/// Runtime dir for sockets and temp: `$HUSH_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/hush`, else `/tmp/hush-$USER`.
pub fn default_runtime_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HUSH_RUNTIME_DIR") {
        return PathBuf::from(p);
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("hush");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    PathBuf::from(format!("/tmp/hush-{}", user))
}

/// Resolve vault/data directory: explicit CLI path (not `.`) wins, then `HUSH_DATA_DIR`, then default.
pub fn resolve_data_dir(cli_dir: &Path) -> PathBuf {
    if cli_dir != Path::new(".") {
        return cli_dir.to_path_buf();
    }
    if let Ok(d) = std::env::var("HUSH_DATA_DIR") {
        return PathBuf::from(d);
    }
    default_data_dir()
}

/// Default relay SQLite path under the data root.
pub fn default_relay_db_path() -> PathBuf {
    resolve_data_dir(Path::new(".")).join("relay").join("relay.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cli_dir_wins() {
        assert_eq!(
            resolve_data_dir(Path::new("/vault/mount")),
            PathBuf::from("/vault/mount")
        );
    }
}

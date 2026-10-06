use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const BACKUP_FORMAT_VERSION: u32 = 1;
const MANIFEST_NAME: &str = "hush-backup-manifest.json";

#[derive(Serialize, Deserialize)]
struct BackupManifest {
    version: u32,
    created_unix: u64,
}

/// Tar archive of the vault directory (relative paths preserved).
pub fn export_vault(dir: &Path) -> Result<Vec<u8>, Error> {
    if !dir.join("vault.lock.json").exists() {
        return Err(Error::NotFound);
    }
    let mut buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut buf);
        let manifest = BackupManifest {
            version: BACKUP_FORMAT_VERSION,
            created_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        let manifest_bytes =
            serde_json::to_vec(&manifest).map_err(|_| Error::Corrupt)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, MANIFEST_NAME, &manifest_bytes[..])
            .map_err(|e| Error::Io(e))?;
        for entry in walk_vault_files(dir)? {
            let rel = entry
                .strip_prefix(dir)
                .map_err(|_| Error::Corrupt)?
                .to_string_lossy();
            builder
                .append_path_with_name(&entry, rel.as_ref())
                .map_err(|e| Error::Io(e))?;
        }
        builder.finish().map_err(|e| Error::Io(e))?;
    }
    Ok(buf)
}

/// Replace vault directory contents from a backup archive (vault must be locked / not in use).
pub fn import_vault(dir: &Path, archive: &[u8]) -> Result<(), Error> {
    let mut archive = tar::Archive::new(archive);
    let mut version_ok = false;
    let mut entries: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for entry in archive.entries().map_err(|e| Error::Io(e))? {
        let mut entry = entry.map_err(|e| Error::Io(e))?;
        let path = entry.path().map_err(|e| Error::Io(e))?.to_path_buf();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| Error::Io(e))?;
        entries.push((path, data));
    }

    for (path, data) in &entries {
        if path.to_string_lossy() == MANIFEST_NAME {
            let m: BackupManifest = serde_json::from_slice(data).map_err(|_| Error::Corrupt)?;
            if m.version != BACKUP_FORMAT_VERSION {
                return Err(Error::InvalidBackup);
            }
            version_ok = true;
        }
    }
    if !version_ok {
        return Err(Error::InvalidBackup);
    }

    if dir.exists() {
        for entry in walk_vault_files(dir)? {
            if entry.is_file() {
                fs::remove_file(entry)?;
            }
        }
    } else {
        fs::create_dir_all(dir)?;
    }

    for (path, data) in entries {
        if path.to_string_lossy() == MANIFEST_NAME {
            continue;
        }
        let dest = dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, data)?;
    }
    Ok(())
}

fn walk_vault_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(walk_vault_files(&path)?);
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(out)
}

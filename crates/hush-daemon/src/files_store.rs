//! The personal file library: every file this vault has imported from the
//! device it runs on.
//!
//! Encrypted at rest in a vault sidecar (`vault-files.enc`, see
//! [`Vault::write_encrypted_file`](hush_core::Vault)) — file names are user
//! secrets, and the bytes themselves are content-addressed vault blobs that are
//! already encrypted by the vault key. Nothing here is plaintext on disk or in
//! the webview.
//!
//! Unlike Notes and Torrents this store owns no key material. It is the index
//! the Files pane reads; the bytes live under `blobs/<hex>` and are opened and
//! saved through the existing blob routes.
//!
//! There is no network path: imported files never leave the device.

use serde::{Deserialize, Serialize};

/// The vault sidecar file name.
pub const STATE_FILE: &str = "vault-files.enc";

/// A bound on the sidecar (and the UI's list). Importing past the cap is
/// refused with a reason, never silently dropped.
const FILES_CAP: usize = 5000;

/// Longest display name kept. Longer names are truncated at a char boundary;
/// the extension is usually what the row and the Save dialog need.
const NAME_CAP: usize = 255;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One file in the vault's own library.
///
/// `blob_id` is the content address of the plaintext, in lowercase hex — the
/// same value the blob routes take. Two entries may share it: importing the
/// same bytes under two names is allowed, and deleting one must not remove the
/// other's bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultFile {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub blob_id: String,
    /// Unix seconds. `0` on a sidecar that predates the field.
    #[serde(default)]
    pub added_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VaultFilesState {
    #[serde(default)]
    pub files: Vec<VaultFile>,
}

impl VaultFilesState {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("vault files state unwritable: {e}"))
    }

    /// Record one imported blob. `blob_id` must be a 32-byte hex digest.
    ///
    /// The name is reduced to a base name so a path can never be shown or
    /// suggested where a file name belongs. Adding the same bytes twice is
    /// allowed: this is a library of the user's choosing, not a dedupe pass.
    pub fn add(&mut self, name: &str, size: u64, blob_id: &str) -> Result<VaultFile, String> {
        if self.files.len() >= FILES_CAP {
            return Err(format!("at most {FILES_CAP} files are kept"));
        }
        if blob_id.len() != 64 || !blob_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid blob id".into());
        }
        let file = VaultFile {
            id: uuid::Uuid::new_v4().to_string(),
            name: sanitize_name(name),
            size,
            blob_id: blob_id.to_ascii_lowercase(),
            added_at: now_secs(),
        };
        self.files.push(file.clone());
        Ok(file)
    }

    /// Remove one entry, returning it when it was there. The blob is the
    /// caller's business: another entry — or another feature — may reference
    /// the same content address.
    pub fn remove(&mut self, id: &str) -> Option<VaultFile> {
        let pos = self.files.iter().position(|f| f.id == id)?;
        Some(self.files.remove(pos))
    }

    /// How many entries still point at a content address.
    pub fn reference_count(&self, blob_id: &str) -> usize {
        self.files
            .iter()
            .filter(|f| f.blob_id.eq_ignore_ascii_case(blob_id))
            .count()
    }
}

/// Decode sidecar bytes; `None` (never saved) decodes to the empty default.
/// A corrupt sidecar is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<VaultFilesState, String> {
    match bytes {
        None | Some([]) => Ok(VaultFilesState::default()),
        Some(b) => {
            serde_json::from_slice(b).map_err(|e| format!("vault files state unreadable: {e}"))
        }
    }
}

/// A display-safe base name: no directory components, no control characters,
/// a non-empty fallback, and a bounded length.
fn sanitize_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    let trimmed = base.trim();
    let name = if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "file"
    } else {
        trimmed
    };
    if name.chars().count() <= NAME_CAP {
        return name.to_string();
    }
    name.chars().take(NAME_CAP).collect()
}

#[cfg(test)]
mod files_store_tests {
    use super::*;

    const BLOB_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BLOB_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn empty_and_missing_sidecars_decode_to_default() {
        assert!(decode(None).unwrap().files.is_empty());
        assert!(decode(Some(b"")).unwrap().files.is_empty());
        assert!(
            decode(Some(b"garbage")).is_err(),
            "corrupt state is an error, not a reset"
        );
    }

    #[test]
    fn add_assigns_id_stamps_time_and_lowercases_the_blob() {
        let mut state = VaultFilesState::default();
        let file = state.add("Report.PDF", 42, &BLOB_A.to_uppercase()).unwrap();
        assert!(!file.id.is_empty());
        assert_eq!(file.name, "Report.PDF");
        assert_eq!(file.size, 42);
        assert_eq!(file.blob_id, BLOB_A);
        assert!(file.added_at > 0);
        assert_eq!(state.files.len(), 1);
    }

    #[test]
    fn names_are_reduced_to_a_base_name() {
        let mut state = VaultFilesState::default();
        let file = state.add("/home/sam/secret/notes.txt", 1, BLOB_A).unwrap();
        assert_eq!(file.name, "notes.txt");

        let win = state
            .add(r"C:\Users\sam\Desktop\photo.jpg", 1, BLOB_B)
            .unwrap();
        assert_eq!(win.name, "photo.jpg");
    }

    #[test]
    fn a_blank_or_dot_name_falls_back_to_file() {
        let mut state = VaultFilesState::default();
        assert_eq!(state.add("", 1, BLOB_A).unwrap().name, "file");
        assert_eq!(state.add("..", 1, BLOB_B).unwrap().name, "file");
    }

    #[test]
    fn a_bad_blob_id_is_refused() {
        let mut state = VaultFilesState::default();
        assert!(state.add("a.txt", 1, "not-a-hash").is_err());
        assert!(state.add("a.txt", 1, &BLOB_A[..62]).is_err());
        assert!(state.files.is_empty());
    }

    #[test]
    fn delete_removes_one_entry_and_leaves_the_other_reference() {
        let mut state = VaultFilesState::default();
        let first = state.add("one.txt", 1, BLOB_A).unwrap();
        let second = state.add("two.txt", 2, BLOB_A).unwrap();
        assert_eq!(state.reference_count(BLOB_A), 2);

        let removed = state.remove(&first.id).expect("entry present");
        assert_eq!(removed.name, "one.txt");
        assert_eq!(state.reference_count(BLOB_A), 1);
        assert!(state.files.iter().any(|f| f.id == second.id));
        assert!(
            state.remove(&first.id).is_none(),
            "deleting twice is a no-op"
        );
    }

    #[test]
    fn create_over_the_cap_is_refused() {
        let mut state = VaultFilesState::default();
        for i in 0..FILES_CAP {
            // The same blob on purpose: the cap is on entries, not bytes.
            state.add(&format!("f{i}"), 0, BLOB_A).unwrap();
        }
        assert_eq!(state.files.len(), FILES_CAP);
        assert!(state.add("one more", 0, BLOB_A).is_err());
    }

    #[test]
    fn state_roundtrips_through_bytes() {
        let mut state = VaultFilesState::default();
        let file = state.add("Groceries.txt", 3, BLOB_A).unwrap();
        let bytes = state.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.files[0].id, file.id);
        assert_eq!(back.files[0].name, "Groceries.txt");
    }

    #[test]
    fn a_long_name_is_truncated_on_a_char_boundary() {
        let mut state = VaultFilesState::default();
        let long = "é".repeat(NAME_CAP + 10);
        let file = state.add(&long, 1, BLOB_A).unwrap();
        assert_eq!(file.name.chars().count(), NAME_CAP);
    }
}

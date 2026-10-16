use std::fs;

use hush_core::{Error, Vault};
use tempfile::TempDir;

/// A vault directory that deletes itself when the test ends: a vault holds an
/// `identity.secret`, so leaving one in the system temp dir leaves a private key
/// there for as long as the machine lives.
fn temp_vault_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("hush-core-test-")
        .tempdir()
        .expect("a scratch directory under the system temp dir")
}

#[test]
fn create_refuses_empty_passphrase() {
    let dir = temp_vault_dir();
    let err = Vault::create(dir.path(), "").unwrap_err();
    assert!(matches!(err, Error::EmptyPassphrase));
}

#[test]
fn create_and_reopen_same_identity() {
    let dir = temp_vault_dir();
    let created = Vault::create(dir.path(), "correct-horse").unwrap();
    let pub_a = created.identity_public();
    drop(created);

    let opened = Vault::open(dir.path(), "correct-horse").unwrap();
    assert_eq!(pub_a, opened.identity_public());
}

#[test]
fn wrong_passphrase_fails() {
    let dir = temp_vault_dir();
    Vault::create(dir.path(), "correct-horse").unwrap();
    let err = Vault::open(dir.path(), "wrong-passphrase").unwrap_err();
    assert!(matches!(err, Error::WrongPassphrase));
}

#[test]
fn blob_roundtrip() {
    let dir = temp_vault_dir();
    let vault = Vault::create(dir.path(), "correct-horse").unwrap();
    let id = vault.put_blob(b"hello").unwrap();
    assert_eq!(vault.get_blob(&id).unwrap(), b"hello");
}

#[test]
fn blob_not_plaintext_on_disk() {
    let dir = temp_vault_dir();
    let vault = Vault::create(dir.path(), "correct-horse").unwrap();
    let marker = b"hello-unique-marker";
    vault.put_blob(marker).unwrap();

    fn any_file_contains(path: &std::path::Path, needle: &[u8]) -> bool {
        if path.is_file() {
            let bytes = fs::read(path).unwrap();
            return bytes.windows(needle.len()).any(|w| w == needle);
        }
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                if any_file_contains(&entry.unwrap().path(), needle) {
                    return true;
                }
            }
        }
        false
    }

    assert!(
        !any_file_contains(dir.path(), marker),
        "plaintext marker found under vault dir"
    );
}

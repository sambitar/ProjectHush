use hush_core::{export_vault, import_vault, Account, Error};
use tempfile::TempDir;

#[tokio::test]
async fn backup_roundtrip_and_recovery_unlock() {
    let relay = "http://127.0.0.1:8787";
    let dir = TempDir::new().unwrap();
    let mut account = Account::create(dir.path(), "test-pass", relay)
        .await
        .unwrap();
    let recovery_code = account
        .take_pending_recovery_code()
        .expect("recovery code at create");

    let archive = account.export_backup().await.unwrap();
    assert!(!archive.is_empty());

    let restore_dir = TempDir::new().unwrap();
    import_vault(restore_dir.path(), &archive).unwrap();

    let reopened = Account::open(restore_dir.path(), "test-pass")
        .await
        .unwrap();
    assert_eq!(reopened.protocol_name(), account.protocol_name());

    let via_recovery = Account::open_with_recovery(restore_dir.path(), &recovery_code)
        .await
        .unwrap();
    assert_eq!(via_recovery.protocol_name(), account.protocol_name());
}

#[tokio::test]
async fn export_vault_contains_lockfile() {
    let dir = TempDir::new().unwrap();
    Account::create(dir.path(), "p", "http://127.0.0.1:1")
        .await
        .unwrap();
    let archive = export_vault(dir.path()).unwrap();
    assert!(archive.len() > 100);
}

/// A backup is the thing we hand to another device, so it must not carry this
/// device's identity private key. That is what makes a linked device revocable:
/// a device holding our identity key could impersonate us forever.
#[tokio::test]
async fn backup_excludes_device_key_material() {
    let dir = TempDir::new().unwrap();
    Account::create(dir.path(), "pw", "http://127.0.0.1:1")
        .await
        .unwrap();

    let archive = export_vault(dir.path()).unwrap();
    let mut tar = tar::Archive::new(&archive[..]);
    let names: Vec<String> = tar
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("device.enc") || n.ends_with("signal.enc")),
        "backup must not carry this device's private identity: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with("account.enc")),
        "shared vault data must still be included: {names:?}"
    );
}

/// The recovery wrap pins the master key, so a passphrase change must re-wrap
/// that same key. If it derived a new master instead, recovery would silently
/// stop opening the vault and the account would be one forgotten passphrase
/// away from permanent loss.
#[tokio::test]
async fn recovery_code_still_opens_after_passphrase_change() {
    let dir = TempDir::new().unwrap();
    let mut account = Account::create(dir.path(), "first-pass", "http://127.0.0.1:1")
        .await
        .unwrap();
    let recovery_code = account
        .take_pending_recovery_code()
        .expect("recovery code at create");
    let name = account.protocol_name().to_string();

    account
        .change_passphrase("first-pass", "second-pass")
        .unwrap();
    drop(account);

    let err = match Account::open(dir.path(), "first-pass").await {
        Ok(_) => panic!("the old passphrase must no longer open the vault"),
        Err(err) => err,
    };
    assert!(matches!(err, Error::WrongPassphrase));
    let reopened = Account::open(dir.path(), "second-pass").await.unwrap();
    assert_eq!(reopened.protocol_name(), name);

    let via_recovery = Account::open_with_recovery(dir.path(), &recovery_code)
        .await
        .unwrap();
    assert_eq!(via_recovery.protocol_name(), name);
}

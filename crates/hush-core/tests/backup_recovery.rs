use hush_core::{Account, export_vault, import_vault};
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

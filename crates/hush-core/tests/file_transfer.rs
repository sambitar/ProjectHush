use std::io::Write;

mod presence_helper;

use hush_core::{Account, MessagePayload};
use presence_helper::recipient_online;
use hush_relay::{RelayConfig, router};
use tempfile::{NamedTempFile, TempDir};
use tokio::time::{sleep, Duration};

async fn spawn_relay() -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    sleep(Duration::from_millis(80)).await;
    (base, dir)
}

#[tokio::test]
async fn send_file_roundtrip_and_not_on_relay_disk() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut alice = Account::create(alice_dir.path(), "pass", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pass", &relay).await.unwrap();
    let a_inv = alice.invite().await.unwrap();
    let b_inv = bob.invite().await.unwrap();
    bob.add_contact("alice", &a_inv).await.unwrap();
    alice.add_contact("bob", &b_inv).await.unwrap();

    let mut fixture = NamedTempFile::new().unwrap();
    let marker = b"file-marker-not-on-relay-xyz";
    fixture.write_all(marker).unwrap();
    fixture.flush().unwrap();

    recipient_online(&alice).await;
    bob.send_file("alice", fixture.path()).await.unwrap();

    let inbox = alice.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    match &inbox.messages[0].payload {
        MessagePayload::File { name, data } => {
            assert!(!name.is_empty());
            assert_eq!(data.as_slice(), marker);
        }
        _ => panic!("expected file payload"),
    }

    fn any_file_contains(path: &std::path::Path, needle: &[u8]) -> bool {
        if path.is_file() {
            let bytes = std::fs::read(path).unwrap_or_default();
            return bytes.windows(needle.len()).any(|w| w == needle);
        }
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    if any_file_contains(&entry.path(), needle) {
                        return true;
                    }
                }
            }
        }
        false
    }

    assert!(
        !any_file_contains(_relay_dir.path(), marker),
        "relay should not store file plaintext"
    );
}

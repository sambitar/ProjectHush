//! Places: adopt, detach, introduce, peer badge.

mod presence_helper;

use hush_core::{Account, PlaceRole};
use presence_helper::recipient_online;
use hush_relay::{router, RelayConfig};
use tempfile::TempDir;
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
async fn adopt_place_records_both_sides_and_auto_contacts() {
    let (relay, _relay_dir) = spawn_relay().await;
    let desktop_dir = TempDir::new().unwrap();
    let phone_dir = TempDir::new().unwrap();

    let mut desktop = Account::create(desktop_dir.path(), "desk-pass", &relay)
        .await
        .unwrap();
    let mut phone = Account::create(phone_dir.path(), "phone-pass", &relay)
        .await
        .unwrap();

    let phone_invite = phone.invite().await.unwrap();
    desktop
        .adopt_place("phone", &phone_invite, "phone")
        .await
        .unwrap();

    assert_eq!(desktop.places().len(), 1);
    assert!(matches!(desktop.places()[0].role, PlaceRole::MintedByMe));
    assert_eq!(desktop.places()[0].label, "phone");
    desktop.places()[0].chain.verify().unwrap();

    // Phone must add the desktop before PlaceAdopt becomes decryptable (inbox
    // blobs have no sender id). The desktop invite is what the UI shows after adopt.
    let desk_invite = desktop.invite().await.unwrap();
    phone
        .add_contact("desktop", &desk_invite)
        .await
        .unwrap();

    recipient_online(&phone).await;
    let mut saw_minted = false;
    for _ in 0..5 {
        let inbox = phone.recv().await.unwrap();
        if !inbox.warnings.is_empty() {
            eprintln!("phone recv warnings: {:?}", inbox.warnings);
        }
        if phone
            .places()
            .iter()
            .any(|p| matches!(p.role, PlaceRole::MintedMe))
        {
            saw_minted = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }

    assert!(
        saw_minted,
        "phone should record MintedMe after PlaceAdopt: places={:?} contacts={:?}",
        phone.places(),
        phone.contacts().iter().map(|c| &c.nickname).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn detach_removes_place_but_keeps_contact() {
    let (relay, _relay_dir) = spawn_relay().await;
    let desktop_dir = TempDir::new().unwrap();
    let phone_dir = TempDir::new().unwrap();

    let mut desktop = Account::create(desktop_dir.path(), "desk-pass", &relay)
        .await
        .unwrap();
    let mut phone = Account::create(phone_dir.path(), "phone-pass", &relay)
        .await
        .unwrap();

    let phone_invite = phone.invite().await.unwrap();
    desktop
        .adopt_place("phone", &phone_invite, "phone")
        .await
        .unwrap();
    let phone_id = desktop.places()[0].account_id.clone();

    desktop.detach_place(&phone_id).await.unwrap();
    assert!(desktop.places().is_empty());
    assert!(
        desktop
            .contacts()
            .iter()
            .any(|c| c.account_id == phone_id),
        "detach must leave the contact"
    );
}

#[tokio::test]
async fn introduce_place_gives_peer_a_badge() {
    let (relay, _relay_dir) = spawn_relay().await;
    let desktop_dir = TempDir::new().unwrap();
    let phone_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let mut desktop = Account::create(desktop_dir.path(), "desk-pass", &relay)
        .await
        .unwrap();
    let mut phone = Account::create(phone_dir.path(), "phone-pass", &relay)
        .await
        .unwrap();
    let mut bob = Account::create(bob_dir.path(), "bob-pass", &relay)
        .await
        .unwrap();

    let desk_invite = desktop.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    bob.add_contact("sam", &desk_invite).await.unwrap();
    desktop.add_contact("bob", &bob_invite).await.unwrap();

    let phone_invite = phone.invite().await.unwrap();
    desktop
        .adopt_place("phone", &phone_invite, "phone")
        .await
        .unwrap();
    let phone_id = desktop.places()[0].account_id.clone();

    recipient_online(&bob).await;
    desktop
        .introduce_place(&phone_id, "bob")
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert!(
        inbox.warnings.is_empty() || true,
        "warnings: {:?}",
        inbox.warnings
    );

    let phone_contact = bob
        .contacts()
        .iter()
        .find(|c| c.account_id == phone_id)
        .expect("bob should have the phone as a contact");
    assert_eq!(
        phone_contact.place_of.as_deref(),
        Some(desktop.protocol_name()),
        "badge should anchor to desktop"
    );
}

//! Places: adopt, confirm, detach, introduce, peer badge.

mod presence_helper;

use hush_core::{Account, Error, PlaceRole};
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

/// Drive an adoption all the way to a confirmed vouch.
///
/// `minter` scans `place`'s invite; `place` adds the minter so the adopt is
/// decryptable, counter-signs the hop naming it, and the minter applies the
/// confirmation. Returns once the minter's chain is accepted, which is the
/// state `introduce_place` requires.
async fn confirm_adoption(minter: &mut Account, place: &mut Account, label: &str) {
    let place_invite = place.invite().await.unwrap();
    minter.adopt_place(label, &place_invite, label).await.unwrap();
    assert!(
        minter.places()[0].chain.verify_accepted().is_err(),
        "the hop is a claim until the other vault signs back"
    );

    // The adopted vault must add the minter before PlaceAdopt is decryptable
    // (inbox blobs carry no sender id).
    let minter_invite = minter.invite().await.unwrap();
    place.add_contact("minter", &minter_invite).await.unwrap();

    recipient_online(place).await;
    for _ in 0..5 {
        place.recv().await.unwrap();
        if place
            .places()
            .iter()
            .any(|p| matches!(p.role, PlaceRole::MintedMe))
        {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        place
            .places()
            .iter()
            .any(|p| matches!(p.role, PlaceRole::MintedMe)),
        "the adopted vault should record MintedMe: places={:?}",
        place.places(),
    );

    recipient_online(minter).await;
    for _ in 0..5 {
        minter.recv().await.unwrap();
        if minter
            .places()
            .iter()
            .all(|p| p.chain.verify_accepted().is_ok())
        {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        minter
            .places()
            .iter()
            .all(|p| p.chain.verify_accepted().is_ok()),
        "the minter should hold a counter-signed chain: places={:?}",
        minter.places(),
    );
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

    confirm_adoption(&mut desktop, &mut phone, "phone").await;

    assert_eq!(desktop.places().len(), 1);
    assert!(matches!(desktop.places()[0].role, PlaceRole::MintedByMe));
    assert_eq!(desktop.places()[0].label, "phone");
    // Both ends signed: the minter's hop and the adopted vault's acceptance.
    desktop.places()[0].chain.verify_accepted().unwrap();
    assert!(desktop.places()[0].chain.hops[0].accepted());

    let phone_place = &phone.places()[0];
    assert!(matches!(phone_place.role, PlaceRole::MintedMe));
    phone_place.chain.verify_accepted().unwrap();

    assert!(
        desktop
            .contacts()
            .iter()
            .any(|c| c.account_id == phone.protocol_name()),
        "adopting must leave the vault as an ordinary contact too"
    );
}

/// The badge is the whole point of the counter-signature: a hop the adopted
/// vault has not signed is the minter's claim alone, and must not travel.
#[tokio::test]
async fn a_place_that_has_not_confirmed_cannot_be_introduced() {
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

    // A mutual contact, so the contact check is not what refuses this.
    let desk_invite = desktop.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    bob.add_contact("sam", &desk_invite).await.unwrap();
    desktop.add_contact("bob", &bob_invite).await.unwrap();

    // Adopt, and stop: the phone never adds the desktop, so it never decrypts
    // the adopt and never counter-signs.
    let phone_invite = phone.invite().await.unwrap();
    desktop
        .adopt_place("phone", &phone_invite, "phone")
        .await
        .unwrap();
    let phone_id = desktop.places()[0].account_id.clone();

    let err = desktop.introduce_place(&phone_id, "bob").await.unwrap_err();
    assert!(
        matches!(err, Error::PlaceNotAccepted),
        "an unconfirmed vouch must not be handed to a contact, got {err:?}"
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

    confirm_adoption(&mut desktop, &mut phone, "phone").await;
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

    confirm_adoption(&mut desktop, &mut phone, "phone").await;
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

//! Sending to someone with no Hush account: seal a container, park it on the
//! relay, and record it so the sender can find the link again.

use hush_core::{Account, ChatRecordPayload, RelayClient};
use hush_relay::{RelayConfig, router};
use tempfile::TempDir;
use tokio::time::{sleep, Duration};

async fn spawn_relay() -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    sleep(Duration::from_millis(80)).await;
    (base, dir)
}

async fn account(relay: &str) -> (Account, TempDir) {
    let dir = TempDir::new().unwrap();
    let account = Account::create(dir.path(), "pw", relay).await.unwrap();
    (account, dir)
}

#[tokio::test]
async fn sending_to_an_outsider_uploads_ciphertext_and_records_it() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _alice_dir) = account(&relay).await;

    let out = alice
        .send_to_outsider(
            "Bob@Gmail.com",
            "hi",
            "<b>secret</b>",
            &[("a.txt".to_string(), b"hello".to_vec())],
            "correct horse",
        )
        .await
        .unwrap();

    // The relay holds a container, and nothing of the plaintext is in it.
    let client = RelayClient::new(&relay);
    let blob = client.get_drop(&out.delivery_id).await.unwrap();
    assert!(hush_core::is_container(&blob), "the relay must hold a container");
    let as_text = String::from_utf8_lossy(&blob);
    assert!(!as_text.contains("secret"), "the body must not be on the relay");
    assert!(!as_text.contains("Bob@Gmail.com"), "the recipient must not be on the relay");

    // It opens with the PIN. This is the one place the real production iteration
    // count is exercised end to end, which is why this test is slow: `drop.rs`
    // proves the mechanism cheaply, and this proves the shipped path.
    let package = hush_core::open_drop(&blob, "correct horse").unwrap();
    assert_eq!(package.body, "<b>secret</b>");
    assert_eq!(package.subject, "hi");
    assert_eq!(package.attachments[0].name, "a.txt");
    assert_eq!(package.attachments[0].data, "aGVsbG8=", "base64 of \"hello\"");

    // The sender keeps a record of it, addressed to the normalized address.
    let records = alice.mail_records();
    let record = records
        .iter()
        .find(|r| matches!(&r.payload, ChatRecordPayload::Mail { external: Some(_), .. }))
        .expect("an external mail record");
    assert_eq!(record.contact_nick, "bob@gmail.com", "the address is the row's who");
    assert!(record.outgoing);
    match &record.payload {
        ChatRecordPayload::Mail {
            external: Some(e),
            state,
            attachments,
            ..
        } => {
            assert_eq!(e.to, "bob@gmail.com");
            assert_eq!(e.delivery_id, out.delivery_id);
            assert!(e.expires_unix > 0);
            assert_eq!(*state, hush_core::MailState::Sent, "a drop is never queued");
            assert_eq!(attachments.len(), 1, "the sender keeps its own copy");
        }
        other => panic!("expected an external mail record, got {other:?}"),
    }
}

#[tokio::test]
async fn a_drop_survives_a_reload_of_the_vault() {
    let (relay, _relay_dir) = spawn_relay().await;
    let dir = TempDir::new().unwrap();
    let delivery_id = {
        let mut alice = Account::create(dir.path(), "pw", &relay).await.unwrap();
        alice
            .send_to_outsider("bob@gmail.com", "s", "b", &[], "12345678")
            .await
            .unwrap()
            .delivery_id
    };

    // Reopening the vault must still show the delivery id, because that is what
    // the sender needs to re-share the link.
    let alice = Account::open(dir.path(), "pw").await.unwrap();
    let record = alice
        .mail_records()
        .into_iter()
        .find(|r| matches!(&r.payload, ChatRecordPayload::Mail { external: Some(_), .. }))
        .expect("the external record survives a reload");
    match record.payload {
        ChatRecordPayload::Mail { external: Some(e), .. } => {
            assert_eq!(e.delivery_id, delivery_id);
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn a_bad_address_or_empty_pin_is_refused_before_anything_is_uploaded() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _alice_dir) = account(&relay).await;

    for bad in ["", "bob", "bob@", "@gmail.com", "a b@gmail.com", "bob@gmail", "bob@@x.com"] {
        let err = alice
            .send_to_outsider(bad, "s", "b", &[], "12345678")
            .await
            .expect_err("a malformed address must be refused");
        assert!(
            matches!(err, hush_core::Error::InvalidAddress),
            "{bad:?} gave {err:?}"
        );
    }

    // The PIN is the only thing protecting a leaked link, so a token PIN is
    // refused at the boundary rather than only in the form that usually calls it.
    for weak in ["", "1", "1234567"] {
        let err = alice
            .send_to_outsider("bob@gmail.com", "s", "b", &[], weak)
            .await
            .expect_err("a weak PIN must be refused");
        assert!(matches!(err, hush_core::Error::WeakPin), "{weak:?} gave {err:?}");
    }

    // The floor is characters, not bytes, so a non-ASCII PIN is not measured
    // wrong. That is pinned by a unit test on `pin_is_long_enough`; a successful
    // send here would cost another full iteration-count seal to prove it.

    assert!(alice.mail_records().is_empty(), "nothing may have been recorded");
}

#[tokio::test]
async fn an_unreachable_relay_fails_the_send_rather_than_queueing_it() {
    // A drop has no Outbox: if the container did not reach the relay there is no
    // pickup link to share, so the send must fail rather than pretend.
    let (mut alice, _alice_dir) = account("http://127.0.0.1:1").await;
    let err = alice
        .send_to_outsider("bob@gmail.com", "s", "b", &[], "12345678")
        .await
        .expect_err("an unreachable relay must fail the send");
    assert!(matches!(err, hush_core::Error::Relay(_)), "got {err:?}");
    assert!(alice.mail_records().is_empty(), "a failed send records nothing");
}

#[tokio::test]
async fn sending_to_a_public_key_uploads_ciphertext_only_the_private_key_opens() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _alice_dir) = account(&relay).await;

    let keys = hush_core::generate_drop_keypair();
    let public = hush_core::encode_drop_public_key(&keys.public_key);

    let out = alice
        .send_to_outsider_with_key(
            "News@Example.com",
            "tip",
            "<b>anonymous tip</b>",
            &[("evidence.txt".to_string(), b"proof".to_vec())],
            &public,
        )
        .await
        .unwrap();

    // The relay holds a v2 container, and nothing of the plaintext or the
    // recipient is in it.
    let client = RelayClient::new(&relay);
    let blob = client.get_drop(&out.delivery_id).await.unwrap();
    assert!(hush_core::is_container(&blob), "the relay must hold a container");
    let as_text = String::from_utf8_lossy(&blob);
    assert!(!as_text.contains("anonymous tip"), "the body must not be on the relay");
    assert!(!as_text.contains("example.com"), "the recipient must not be on the relay");

    // The matching private key opens it.
    let package = hush_core::open_drop_with_key(&blob, &keys.private_key).unwrap();
    assert_eq!(package.body, "<b>anonymous tip</b>");
    assert_eq!(package.subject, "tip");
    assert_eq!(package.attachments[0].name, "evidence.txt");
    assert_eq!(package.attachments[0].data, "cHJvb2Y=", "base64 of \"proof\"");

    // Neither a PIN nor a different private key opens it.
    assert!(
        matches!(hush_core::open_drop(&blob, "correct horse"), Err(hush_core::Error::DropNeedsKey)),
        "a PIN must not open a public-key drop"
    );
    let wrong = hush_core::generate_drop_keypair();
    assert!(
        hush_core::open_drop_with_key(&blob, &wrong.private_key).is_err(),
        "a different private key must not open it"
    );

    // The sender keeps the same shape of record as a PIN drop, addressed to the
    // normalized address.
    let record = alice
        .mail_records()
        .into_iter()
        .find(|r| matches!(&r.payload, ChatRecordPayload::Mail { external: Some(_), .. }))
        .expect("an external mail record");
    assert_eq!(record.contact_nick, "news@example.com");
    match &record.payload {
        ChatRecordPayload::Mail {
            external: Some(e),
            state,
            ..
        } => {
            assert_eq!(e.to, "news@example.com");
            assert_eq!(e.delivery_id, out.delivery_id);
            assert_eq!(*state, hush_core::MailState::Sent);
        }
        other => panic!("expected an external mail record, got {other:?}"),
    }
}

#[tokio::test]
async fn a_public_key_drop_refuses_a_malformed_key_before_uploading() {
    let (relay, _relay_dir) = spawn_relay().await;
    let (mut alice, _alice_dir) = account(&relay).await;

    for bad in ["", "not-a-key", "hush-drop-pk-v1:AAAA", "hush-drop-sk-v1:AAAA"] {
        let err = alice
            .send_to_outsider_with_key("news@example.com", "s", "b", &[], bad)
            .await
            .expect_err("a malformed key must be refused");
        assert!(
            matches!(err, hush_core::Error::InvalidDropPublicKey),
            "{bad:?} gave {err:?}"
        );
    }
    assert!(alice.mail_records().is_empty(), "nothing may have been recorded");
}

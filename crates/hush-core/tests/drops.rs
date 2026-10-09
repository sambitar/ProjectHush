use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hush_core::{seal_to_key, DropAttachment, DropPackage};
use hush_relay::{RelayConfig, router};
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

/// A member account with its drop keys already created.
async fn member(base: &str) -> (hush_core::Account, TempDir) {
    let dir = TempDir::new().unwrap();
    let mut account = hush_core::Account::create(dir.path(), "member-pass", base)
        .await
        .unwrap();
    account.ensure_drop_keys().unwrap();
    (account, dir)
}

/// Seal a package to a published drop key the way an outsider would.
fn seal(pk_encoded: &str, pkg: &DropPackage) -> Vec<u8> {
    let pk_raw = hush_core::decode_drop_pk(pk_encoded).unwrap();
    seal_to_key(pkg, &pk_raw).unwrap()
}

fn pkg(body: &str, attachments: Vec<DropAttachment>) -> DropPackage {
    DropPackage {
        v: 2,
        subject: String::new(),
        body: body.to_string(),
        attachments,
        created_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    }
}

/// A drop parked on the member's rendezvous slot, drained into a quarantined
/// record: attachments land in the vault blob store, the slot is consumed,
/// and a second pass finds nothing.
#[tokio::test]
async fn a_drop_drains_into_a_quarantined_record() {
    let (relay, _dir) = spawn_relay().await;
    let (mut member, _member_dir) = member(&relay).await;
    let pk = member.drop_public_key().unwrap();
    let slot = hush_core::rendezvous_id(&hush_core::decode_drop_pk(&pk).unwrap());

    let container = seal(
        &pk,
        &DropPackage {
            v: 2,
            subject: "leaks".to_string(),
            body: "screenshot attached".to_string(),
            attachments: vec![DropAttachment {
                name: "shot.png".to_string(),
                data: B64.encode(b"png-bytes-here"),
            }],
            created_unix: 1_700_000_000,
        },
    );
    let client = hush_core::RelayClient::new(&relay);
    client.put_drop(&slot, &container).await.unwrap();

    // Nothing is filed before the member drains.
    assert_eq!(member.drop_records().len(), 0);

    let report = member.drain_drops().await.unwrap();
    assert_eq!(report.hit_slots, vec![slot.clone()]);
    assert_eq!(report.remaining, 0);

    let drops = member.drop_records();
    assert_eq!(drops.len(), 1);
    let record = &drops[0];
    assert!(!record.outgoing);
    assert!(record.contact_nick.is_empty(), "a drop has no peer");
    match &record.payload {
        hush_core::ChatRecordPayload::Drop {
            rendezvous_id,
            subject,
            body,
            attachments,
            created_unix,
        } => {
            assert_eq!(rendezvous_id.as_str(), slot.as_str());
            assert_eq!(subject.as_deref(), Some("leaks"));
            assert_eq!(body, "screenshot attached");
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0].name, "shot.png");
            assert_eq!(attachments[0].size, 14);
            assert_eq!(created_unix, &1_700_000_000u64);
            // The attachment bytes made it into the vault, addressable by the
            // recorded id, and the member renders none of it inline.
            let raw: [u8; 32] = hex::decode(&attachments[0].blob_id_hex)
                .unwrap()
                .try_into()
                .unwrap();
            let blob = member.read_blob(&hush_core::BlobId(raw)).unwrap();
            assert_eq!(blob, b"png-bytes-here");
        }
        _ => panic!("wrong payload kind"),
    }

    // The slot was consumed after the record filed.
    assert!(client.get_drop(&slot).await.unwrap().is_none());
    // And a second pass finds nothing.
    let again = member.drain_drops().await.unwrap();
    assert!(again.hit_slots.is_empty());
}

/// A container sealed to some other key stays parked rather than being
/// deleted or filed, and the member's own keys are untouched by it.
#[tokio::test]
async fn a_container_not_for_this_key_stays_parked() {
    let (relay, _dir) = spawn_relay().await;
    let (mut member, _dir) = member(&relay).await;

    // Sealed to a key that is not the member's.
    let stranger = hush_core::drop_keypair();
    let container = seal_to_key(&pkg("not for you", Vec::new()), &stranger.pk).unwrap();
    let slot = hush_core::rendezvous_id(&stranger.pk);
    let client = hush_core::RelayClient::new(&relay);
    client.put_drop(&slot, &container).await.unwrap();

    let report = member.drain_drops().await.unwrap();
    assert!(report.hit_slots.is_empty());
    assert_eq!(member.drop_records().len(), 0);
    // The stranger's slot never was the member's address; their container
    // survives because deleting on a failed open would be evidence loss.
    assert!(client.get_drop(&slot).await.unwrap().is_some());
}

/// Hostile content inside an opened container is clamped, not fatal: path
/// components in names, control characters in text, too many or too large
/// attachments, base64 that does not decode. The drain succeeds, the record
/// is filed, the slot is consumed, and the tampering is visible as notes.
#[tokio::test]
async fn a_hostile_container_is_clamped_not_fatal() {
    let (relay, _dir) = spawn_relay().await;
    let (mut member, _dir) = member(&relay).await;
    let pk = member.drop_public_key().unwrap();
    let slot = hush_core::rendezvous_id(&hush_core::decode_drop_pk(&pk).unwrap());

    // Ten files: the first is over the 256 KiB inline cap, the second does not
    // decode as base64, and the remaining eight are small and valid — exactly
    // filling the per-drop attachment cap, so every clamping path fires.
    let big = vec![0xAAu8; 300 * 1024]; // over the 256 KiB inline cap
    let attachments: Vec<DropAttachment> = (0..10)
        .map(|i| DropAttachment {
            name: format!("../../evil{}/../etc/passwd\x00{}", i, i),
            data: match i {
                0 => B64.encode(&big),
                1 => "!!!not base64!!!".to_string(),
                _ => B64.encode(b"tiny"),
            },
        })
        .collect();

    let container = seal(
        &pk,
        &DropPackage {
            v: 2,
            subject: "subject\u{0}with-control".to_string(),
            body: "body\u{1}\nline\u{7}".to_string(),
            attachments,
            created_unix: 42,
        },
    );
    hush_core::RelayClient::new(&relay)
        .put_drop(&slot, &container)
        .await
        .unwrap();

    let report = member.drain_drops().await.unwrap();
    assert_eq!(report.hit_slots, vec![slot.clone()]);
    assert_eq!(report.remaining, 0);

    let records = member.drop_records();
    match &records[0].payload {
        hush_core::ChatRecordPayload::Drop {
            subject,
            body,
            attachments,
            ..
        } => {
            assert_eq!(subject.as_deref(), Some("subjectwith-control"));
            assert!(body.contains("body\nline"), "control chars out, newline kept: {body:?}");
            assert!(body.contains("[attachments]"), "skips are disclosed: {body:?}");
            assert!(body.contains("larger than"), "the oversized file is named: {body:?}");
            assert!(body.contains("not decodable"), "the broken file is named: {body:?}");
            // Eight kept: the oversized and undecodable ones were skipped, and
            // the remaining eight small files exactly fill the cap.
            assert_eq!(attachments.len(), 8);
            for a in attachments {
                assert!(!a.name.contains('/'), "no path separators: {}", a.name);
                assert!(!a.name.contains('\\'), "no path separators: {}", a.name);
                assert!(!a.name.contains('\0'), "no control chars: {}", a.name);
            }
        }
        _ => panic!("wrong payload kind"),
    }

    // Filed successfully, so the slot was consumed: the crafted drop did not
    // wedge the drain.
    assert!(hush_core::RelayClient::new(&relay)
        .get_drop(&slot)
        .await
        .unwrap()
        .is_none());
}

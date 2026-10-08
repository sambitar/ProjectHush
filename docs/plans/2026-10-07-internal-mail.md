# Internal Mail (Task 5d) Implementation Plan

**Status:** Implemented. Verified by `crates/hush-core/tests/internal_mail.rs`, `scripts/smoke-mail.sh`,
`cargo test --workspace`, `cargo clippy --workspace --all-targets`, and a DOM-level two-peer UI run
(screenshots were unavailable — see `DEEPSEEKPROBLEMS.MD`). The `[ ]` boxes below are the original
build checklist and are left as written; the residual v1 limitations are listed in
`docs/plans/2026-10-04-v1-milestones.md` under Task 5d.

**Phase 5 (2026-10-07)** adds the mail-client shell on top of this transport: a seven-folder rail,
message list with search, reading pane, full-pane composer, encrypted drafts, and a retrying Outbox.
See [Phase 5](#phase-5--mail-client-ui-folders-drafts-and-the-outbox-2026-10-07) at the end of this
document for the design deltas and the verification evidence.

**Goal:** Two members on the same relay can send each other email-shaped messages — a subject, a body,
and multiple attachments — that arrive even when the recipient is offline, over the existing
end-to-end Signal transport, with an `name@myserver.com` handle that is a label bound to a key and not
a public mailbox.

**Architecture:** Mail is a third payload kind on the rail chat already uses. It rides the same
libsignal session, the same opaque relay blob, and the same encrypted local log; what changes is that
mail does **not** check presence before sending, so it lands in the recipient's store-and-forward queue
while they are away. A handle (`sandy@myserver.com`) is chosen locally and travels inside the invite,
so there is no relay-side directory — compose is limited to contacts you already added, exactly like
chat.

**Tech Stack:** Rust (`hush-core`, `hush-daemon`), `libsignal-protocol`, axum, TypeScript + Vite (UI).
The relay is unchanged: it stays a dumb ciphertext store.

**Spec:** Task **5d** in [`docs/plans/2026-10-04-v1-milestones.md`](2026-10-04-v1-milestones.md);
constraints from [`docs/DESIGN.md`](../DESIGN.md), [`docs/THREAT_MODEL.md`](../THREAT_MODEL.md),
[`docs/PROTOCOL.md`](../PROTOCOL.md).

## Base, and the multi-device dependency

This branch (`feat/internal-mail`) is cut from `61f3fcb` — multi-device **Phase 3**. Multi-device
**Phase 4** (contacts as device sets, send/recv fan-out, the per-device prekey endpoint) is
*uncommitted* in the main worktree at the time of writing. That matters in exactly two places:

- `Contact` gains a `devices: Vec<ContactDevice>` field. Mail's `mail_handle` is a **top-level
  `Contact` field** in both shapes, so the two changes compose. When Phase 4 lands, add
  `mail_handle: Option<String>` next to `account_id` and `devices`.
- `Account::send_payload` grows a per-device loop. Mail goes through a new
  `send_payload_inner(.., require_online)` that keeps `send_payload`'s existing signature, so mail
  inherits fan-out for free when Phase 4 lands and neither change has to be rewritten.

Rebase onto the multi-device branch before opening the PR. If Phase 4 has already merged, the only
edit is placing `mail_handle` on the new `Contact` shape and re-running the suite.

## Global Constraints

- Never commit to `master`; this phase lands on `feat/internal-mail`.
- No plaintext of subject, body, attachment name, or attachment bytes on disk, ever — same rule as
  chat (5c). Mail rides the encrypted log and encrypted blobs.
- The relay stays content-blind and **unchanged**: no new relay route, no subject in the wire JSON.
- No relay-side directory, no handle lookup, no "find anyone on Hush". A handle is a label on a key
  you already exchanged an invite with.
- Chat stays live-only (5h). Mail is the async path and must **not** require the recipient online.
- Every task ends with a runnable test. No task may leave the tree unable to build.

## Settled design

| # | Decision |
|---|---|
| M1 | A **handle** is a user-chosen local part; the full address is `handle@MAIL_DOMAIN`. It travels inside the `InvitePayload`, so a peer learns it out of band. No relay lookup, no directory. |
| M2 | `MAIL_DOMAIN` is a constant in `hush-core`, exposed to the UI through `/api/status` so the suffix is not hardcoded twice. |
| M3 | Mail is stored in the **existing encrypted `chat.enc` log** as a third `ChatRecordPayload` kind. One log, one unlock path, one backup. The UI renders a **separate Mail rail** filtered to mail records. |
| M4 | A **thread** is a `thread_id` UUID. The first message mints it; replies reuse it. Subject is per-message and optional. |
| M5 | Mail is **store-and-forward**: `send_mail` does not call `contact_is_online`. This is the difference from chat and the reason the feature exists. |
| M6 | Attachments are inlined in the Signal message (like `File` today), capped at `MAX_MAIL_ATTACHMENTS = 8` and `MAX_INLINE_FILE_BYTES` total; each is also stored as an encrypted vault blob on both ends so it is openable after lock/unlock. |
| M7 | The handle rides in `InvitePayload` as an optional field. No `INVITE_VERSION` bump: serde ignores unknown fields, so old invites decode (handle `None`) and old clients ignore the new field. |

### Known limitations to record, not hide

- Mail is between **mutual contacts**. You can only address someone whose invite you have, and a reply
  needs the other side to have added you — same asymmetry chat already has.
- A handle change does not propagate to contacts who already added you until you hand them a fresh
  invite. v1 accepts this; a later slice can publish the handle on the device list.
- The recipient must poll `recv` (the UI already does) to drain queued mail; there is no push.

## File map

| File | Responsibility |
|---|---|
| `crates/hush-core/src/mail.rs` *(new)* | `MAIL_DOMAIN`, handle normalization, address formatting |
| `crates/hush-core/src/payload.rs` | `MessagePayload::Mail`, `MailAttachment` |
| `crates/hush-core/src/chat_log.rs` | `ChatRecordPayload::Mail`, `append_mail`, `mail_records` |
| `crates/hush-core/src/message.rs` | `ReceivedMessage.mail_attachments`, `ReceivedAttachment` |
| `crates/hush-core/src/invite.rs` | optional `handle` on `InvitePayload` |
| `crates/hush-core/src/account.rs` | `mail_handle` state, `set_mail_handle`, `send_mail`, recv arm, invite carries handle |
| `crates/hush-core/src/error.rs` | `InvalidHandle`, `TooManyAttachments` |
| `crates/hush-core/src/signal.rs` | `invite_from_bundle` takes the handle |
| `crates/hush-core/src/lib.rs` | re-exports |
| `crates/hush-daemon/src/account_worker.rs` | `SetMailHandle`, `SendMail`, `MailRecords` work items |
| `crates/hush-daemon/src/main.rs` | `POST /api/mail/send`, `GET /api/mail`, `POST /api/mail/handle`; status/contact fields |
| `apps/hush-tauri/src/lib/api/types.ts` | mail DTOs and record union |
| `apps/hush-tauri/src/lib/api/daemon.ts` | `sendMail`, `mail`, `setMailHandle` |
| `apps/hush-tauri/src/lib/mailStore.ts` *(new)* | thread grouping for the Mail rail |
| `apps/hush-tauri/src/lib/icons.ts` | `mail` icon |
| `apps/hush-tauri/src/app.ts` | Mail rail, thread view, compose, handle in Settings |
| `scripts/smoke-mail.sh` *(new)* | end-to-end daemon smoke |

---

## Phase 1 — Handles and address book (5d.1)

### Task 1: Handle primitives

**Files:**
- Create: `crates/hush-core/src/mail.rs`
- Modify: `crates/hush-core/src/lib.rs`, `crates/hush-core/src/error.rs`
- Test: in-crate `#[cfg(test)] mod tests` in `mail.rs`

**Interfaces:**
- Produces: `MAIL_DOMAIN: &str`, `MAX_HANDLE_LEN: usize`, `normalize_handle(&str) -> Result<String, Error>`, `format_address(&str) -> String`
- Consumes: `Error`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/hush-core/src/mail.rs (tail)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_lowercased_and_trimmed() {
        assert_eq!(normalize_handle("  Sandy ").unwrap(), "sandy");
    }

    #[test]
    fn handles_reject_the_shapes_that_would_be_ambiguous() {
        for bad in ["", "a b", "sandy@x", "..", ".sandy", "sandy.", "-s", "s-", "s/a", "s'andy"] {
            assert!(normalize_handle(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn a_valid_handle_formats_with_the_domain() {
        assert_eq!(format_address("sandy"), "sandy@myserver.com");
    }
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --lib mail`
Expected: compile error, `mail` module not found.

- [ ] **Step 3: Minimal implementation**

```rust
// crates/hush-core/src/mail.rs
use crate::error::Error;

/// Fixed domain suffix for internal member mail.
///
/// Members-only mail never leaves Hush: this is a display convention, not an MX
/// record. It is a constant rather than per-vault config so two members on the
/// same relay always render the same address, and the UI reads it from
/// `/api/status` instead of hardcoding a second copy.
pub const MAIL_DOMAIN: &str = "myserver.com";

pub const MAX_HANDLE_LEN: usize = 64;

/// Validate and canonicalize a local part.
///
/// Lowercase, `[a-z0-9._-]`, 1..=64 chars, must start and end alphanumeric, no
/// `..` — the conservative email-local-part shape. A handle is a label bound to
/// a key, so two handles that differ only by case would be a real ambiguity.
pub fn normalize_handle(input: &str) -> Result<String, Error> {
    let h = input.trim().to_ascii_lowercase();
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if h.is_empty() || h.len() > MAX_HANDLE_LEN {
        return Err(Error::InvalidHandle);
    }
    let allowed = h
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !allowed
        || !h.chars().next().is_some_and(alnum)
        || !h.chars().last().is_some_and(alnum)
        || h.contains("..")
    {
        return Err(Error::InvalidHandle);
    }
    Ok(h)
}

pub fn format_address(handle: &str) -> String {
    format!("{handle}@{MAIL_DOMAIN}")
}
```

Add to `error.rs`:

```rust
#[error("handle must be 1-64 characters of a-z, 0-9, dot, underscore or dash")]
InvalidHandle,
```

Add to `lib.rs`: `mod mail;` and `pub use mail::{format_address, normalize_handle, MAIL_DOMAIN, MAX_HANDLE_LEN};`

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --lib mail`

---

### Task 2: The handle lives on the account, travels in the invite, lands on the contact

**Files:**
- Modify: `crates/hush-core/src/account.rs`, `crates/hush-core/src/invite.rs`, `crates/hush-core/src/signal.rs`, `crates/hush-core/src/lib.rs`
- Test: `crates/hush-core/tests/internal_mail.rs` *(new)*

**Interfaces:**
- Produces: `Account::mail_handle() -> Option<&str>`, `Account::mail_address() -> Option<String>`, `Account::set_mail_handle(&str) -> Result<(), Error>`; `Contact.mail_handle: Option<String>`; `InvitePayload.handle: Option<String>`
- Consumes: `mail::normalize_handle`, `mail::format_address`

- [ ] **Step 1: Write the failing test**

```rust
// crates/hush-core/tests/internal_mail.rs
use hush_core::{Account, InvitePayload};
use tempfile::TempDir;

#[tokio::test]
async fn a_handle_round_trips_through_an_invite() {
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let relay = "http://127.0.0.1:1";

    let mut alice = Account::create(alice_dir.path(), "pw", relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", relay).await.unwrap();

    alice.set_mail_handle("Sandy").await.unwrap();
    assert_eq!(alice.mail_handle(), Some("sandy"));
    assert_eq!(alice.mail_address().as_deref(), Some("sandy@myserver.com"));

    let invite = alice.invite().await.unwrap();
    let decoded = InvitePayload::decode(&invite).unwrap();
    assert_eq!(decoded.handle.as_deref(), Some("sandy"));

    bob.add_contact("alice", &invite).await.unwrap();
    assert_eq!(bob.contacts()[0].mail_handle.as_deref(), Some("sandy"));
}

#[tokio::test]
async fn setting_a_handle_invalidates_a_cached_invite() {
    let dir = TempDir::new().unwrap();
    let mut account = Account::create(dir.path(), "pw", "http://127.0.0.1:1")
        .await
        .unwrap();
    let before = account.invite().await.unwrap();
    account.set_mail_handle("sandy").await.unwrap();
    let after = account.invite().await.unwrap();
    assert_ne!(before, after, "a new handle must go out on the next invite");
    assert_eq!(
        InvitePayload::decode(&after).unwrap().handle.as_deref(),
        Some("sandy")
    );
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test internal_mail`
Expected: compile error, `set_mail_handle` not found.

- [ ] **Step 3: Minimal implementation**

`invite.rs` — add an optional field (serde default keeps old invites loadable):

```rust
    /// The sender's chosen mail handle (local part only), if they set one.
    /// Optional so invites minted before mail still decode.
    #[serde(default)]
    pub handle: Option<String>,
```

`signal.rs` — `invite_from_bundle` takes it and copies it into the payload:

```rust
pub fn invite_from_bundle(
    relay: &str,
    protocol_name: &str,
    queue_id: &str,
    queue_token: &str,
    handle: Option<&str>,
    bundle: &PreKeyBundle,
) -> Result<InvitePayload, Error> {
    // ... unchanged ...
    Ok(InvitePayload {
        // ...
        handle: handle.map(|h| h.to_string()),
    })
}
```

`account.rs` — `AccountState` gains

```rust
    /// This member's chosen mail handle (local part only). Shared vault state,
    /// not device material: it is a label, and a restored vault should keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mail_handle: Option<String>,
```

and every `AccountState { .. }` literal (in `create`, and the test helper `write_legacy_vault`) gets
`mail_handle: None,`. `Contact` gains

```rust
    #[serde(default)]
    pub mail_handle: Option<String>,
```

`invite()` passes the handle, and a handle change drops the cached invite:

```rust
    pub async fn invite(&mut self) -> Result<String, Error> {
        if let Some(cached) = &self.state.invite_cache {
            return Ok(cached.clone());
        }
        let bundle = mint_prekey_bundle(self.device.store_mut()).await?;
        let payload = invite_from_bundle(
            &self.state.relay,
            &self.state.protocol_name,
            self.device.queue_id(),
            self.device.queue_token(),
            self.state.mail_handle.as_deref(),
            &bundle,
        )?;
        // ... unchanged ...
    }

    pub fn mail_handle(&self) -> Option<&str> {
        self.state.mail_handle.as_deref()
    }

    pub fn mail_address(&self) -> Option<String> {
        self.state.mail_handle.as_deref().map(crate::mail::format_address)
    }

    /// Set or change this member's mail handle.
    ///
    /// The cached invite is dropped so the next `invite()` mints one carrying
    /// the new handle; otherwise "Copy my invite" would keep advertising the old
    /// address until the process restarted.
    pub async fn set_mail_handle(&mut self, handle: &str) -> Result<(), Error> {
        self.state.mail_handle = Some(crate::mail::normalize_handle(handle)?);
        self.state.invite_cache = None;
        self.save().await
    }
```

`add_contact` stores the handle from the invite:

```rust
        self.state.contacts.push(Contact {
            nickname: nickname.to_string(),
            protocol_name: payload.protocol_name.clone(),
            queue_id: payload.queue_id,
            queue_token: payload.queue_token,
            mail_handle: payload.handle.clone(),
            inbound_ready: false,
        });
```

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test internal_mail`

- [ ] **Step 5: Confirm nothing else broke**

Run: `cargo test --workspace`
Expected: all suites pass. Existing invite tests still compile because `handle` is `#[serde(default)]`.

---

### Task 3: Daemon surfaces the handle

**Files:**
- Modify: `crates/hush-daemon/src/main.rs`, `crates/hush-daemon/src/account_worker.rs`
- Test: `scripts/smoke-mail.sh` *(new)*

**Interfaces:**
- Produces: `POST /api/mail/handle { handle }`; `mail_handle` on `/api/status`; `mail_handle` on each `ContactDto`; `mail_domain` on `/api/status`
- Consumes: `Account::set_mail_handle`, `Account::mail_handle`, `mail::MAIL_DOMAIN`

- [ ] **Step 1: Write the failing smoke check**

```bash
#!/usr/bin/env bash
# scripts/smoke-mail.sh — unlock, set a handle, read it back.
set -euo pipefail
VAULT=$(mktemp -d)
API="http://127.0.0.1:19911"
cargo run -q -p hush-daemon -- --dir "$VAULT" --addr 127.0.0.1:19911 --no-socket &
DAEMON=$!
trap 'kill $DAEMON 2>/dev/null || true; rm -rf "$VAULT"' EXIT
for _ in $(seq 1 40); do curl -sf "$API/api/status" >/dev/null && break; sleep 0.25; done

curl -sf -X POST "$API/api/unlock" -H 'content-type: application/json' \
  -d '{"passphrase":"pw"}' || { echo "unlock failed"; exit 1; }
curl -sf -X POST "$API/api/mail/handle" -H 'content-type: application/json' \
  -d '{"handle":"sandy"}'
STATUS=$(curl -sf "$API/api/status")
echo "$STATUS" | grep -q '"mail_handle":"sandy"' || { echo "handle missing: $STATUS"; exit 1; }
echo "$STATUS" | grep -q '"mail_domain":"myserver.com"' || { echo "domain missing"; exit 1; }

curl -s -o /dev/null -w '%{http_code}' -X POST "$API/api/mail/handle" \
  -H 'content-type: application/json' -d '{"handle":"bad handle"}' | grep -q 400 \
  || { echo "invalid handle was not rejected"; exit 1; }
echo "smoke-mail: ok"
```

Note: creating a vault from scratch needs an unlocked vault. The daemon expects an existing vault dir;
mirror `scripts/smoke-daemon-socket.sh` for how it creates one (run `hush-cli`/`Account::create` first,
or reuse the helper that script uses). Keep the same pattern rather than inventing a new one.

- [ ] **Step 2: Run — expect FAIL**

Run: `chmod +x scripts/smoke-mail.sh && ./scripts/smoke-mail.sh`
Expected: `404` on `/api/mail/handle`.

- [ ] **Step 3: Minimal implementation**

`account_worker.rs`: add a `SetMailHandle` work item, a `MailHandle` accessor on `Meta`, and the DTO
field.

```rust
    SetMailHandle {
        handle: String,
        reply: oneshot::Sender<WorkResult>,
    },
```

```rust
// in the dispatch loop
Work::SetMailHandle { handle, reply } => {
    let result = match account.as_mut() {
        None => WorkResult::Err("locked".into()),
        Some(a) => match a.set_mail_handle(&handle).await {
            Ok(()) => WorkResult::Ok,
            Err(e) => WorkResult::Err(e.to_string()),
        },
    };
    (reply, result)
}
```

```rust
// Meta gains mail_handle
WorkResult::Meta {
    protocol_name: String,
    vault_bytes: u64,
    contacts: usize,
    recovery_acknowledged: bool,
    last_backup_unix: u64,
    relay: String,
    mail_handle: Option<String>,
},
```

```rust
// ContactDto gains mail_handle
pub struct ContactDto {
    pub nickname: String,
    pub protocol_name: String,
    pub inbound_ready: bool,
    pub online: bool,
    pub mail_handle: Option<String>,
}
```

Fill both in the `Contacts` and `Meta` arms, and add:

```rust
    pub async fn set_mail_handle(&self, handle: &str) -> WorkResult {
        self.call(|reply| Work::SetMailHandle {
            handle: handle.to_string(),
            reply,
        })
        .await
    }
```

`main.rs`: add the route and handler, and the status fields.

```rust
        .route("/api/mail/handle", post(set_mail_handle))
```

```rust
#[derive(Deserialize)]
struct SetMailHandleRequest {
    handle: String,
}

async fn set_mail_handle(
    State(state): State<Arc<App>>,
    Json(body): Json<SetMailHandleRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    map_err(state.worker.set_mail_handle(&body.handle).await)?;
    Ok(StatusCode::NO_CONTENT)
}
```

`StatusResponse` gains `mail_handle: Option<String>` and `mail_domain: String`; set them in the
`status` handler (`mail_domain: hush_core::MAIL_DOMAIN.to_string()`, and `mail_handle` from `Meta`,
`None` while locked).

- [ ] **Step 4: Run — expect PASS**

Run: `./scripts/smoke-mail.sh`

---

## Phase 2 — Mail payload, log, and async transport (5d.3)

### Task 4: `MessagePayload::Mail` and `ChatRecordPayload::Mail`

**Files:**
- Modify: `crates/hush-core/src/payload.rs`, `crates/hush-core/src/chat_log.rs`, `crates/hush-core/src/error.rs`, `crates/hush-core/src/lib.rs`
- Test: in-crate tests in both modules

**Interfaces:**
- Produces: `MessagePayload::Mail { thread_id, subject, body, attachments }`, `MailAttachment { name, data }`, `ChatRecordPayload::Mail { thread_id, subject, body, attachments }`, `MailAttachmentRecord { name, blob_id_hex, size }`, `ChatLog::append_mail`, `ChatLog::mail_records`
- Consumes: existing `b64_bytes` module, `Vault` blob ids (as hex)

- [ ] **Step 1: Write the failing tests**

```rust
// payload.rs
#[test]
fn a_mail_payload_round_trips_with_attachments() {
    let p = MessagePayload::Mail {
        thread_id: "t-1".into(),
        subject: Some("hi".into()),
        body: "body".into(),
        attachments: vec![MailAttachment { name: "a.txt".into(), data: vec![1, 2, 3] }],
    };
    let decoded = MessagePayload::decode(&p.encode().unwrap()).unwrap();
    assert_eq!(decoded, p);
}

#[test]
fn a_mail_payload_without_subject_or_attachments_round_trips() {
    let p = MessagePayload::Mail {
        thread_id: "t-2".into(),
        subject: None,
        body: "body".into(),
        attachments: vec![],
    };
    assert_eq!(MessagePayload::decode(&p.encode().unwrap()).unwrap(), p);
}
```

```rust
// chat_log.rs
#[test]
fn a_mail_record_survives_a_save_and_load() {
    let dir = std::env::temp_dir().join(format!("hush-mail-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let master = [7u8; crate::crypto::MASTER_KEY_LEN];

    let mut log = ChatLog::load(&dir, &master).unwrap();
    log.append_mail(
        &master,
        "alice",
        true,
        "t-1",
        Some("hi"),
        "body",
        &[MailAttachmentRecord {
            name: "a.txt".into(),
            blob_id_hex: "ab".into(),
            size: 3,
        }],
    )
    .unwrap();

    let reloaded = ChatLog::load(&dir, &master).unwrap();
    assert_eq!(reloaded.mail_records().count(), 1);
    assert_eq!(reloaded.messages().len(), 1);
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --lib`
Expected: compile errors, `Mail` variant and `append_mail` not found.

- [ ] **Step 3: Minimal implementation**

`error.rs`:

```rust
#[error("too many attachments (max {0})")]
TooManyAttachments(usize),
```

`payload.rs` — a wire attachment and the variant. `b64_bytes` already exists and is reusable as-is
because it is in the same module:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailAttachment {
    pub name: String,
    #[serde(with = "b64_bytes")]
    pub data: Vec<u8>,
}

pub const MAX_MAIL_ATTACHMENTS: usize = 8;

#[serde(tag = "t", rename_all = "snake_case")]
pub enum MessagePayload {
    Text { body: String },
    File { name: String, #[serde(with = "b64_bytes")] data: Vec<u8> },
    /// An internal mail message. Distinct from `Text`/`File` so the Mail rail
    /// can render threads, subjects and multiple attachments while riding the
    /// same encrypted session and relay queue.
    Mail {
        thread_id: String,
        #[serde(default)]
        subject: Option<String>,
        body: String,
        #[serde(default)]
        attachments: Vec<MailAttachment>,
    },
    OwnCopy { to_account_id: String, inner: Box<MessagePayload> },
}
```

`chat_log.rs` — the record shapes and the two methods:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailAttachmentRecord {
    pub name: String,
    pub blob_id_hex: String,
    pub size: u64,
}

#[serde(tag = "t", rename_all = "snake_case")]
pub enum ChatRecordPayload {
    Text { body: String },
    File { name: String, blob_id_hex: String, size: u64 },
    Mail {
        thread_id: String,
        subject: Option<String>,
        body: String,
        attachments: Vec<MailAttachmentRecord>,
    },
}
```

```rust
    pub fn append_mail(
        &mut self,
        master: &[u8; MASTER_KEY_LEN],
        contact_nick: &str,
        outgoing: bool,
        thread_id: &str,
        subject: Option<&str>,
        body: &str,
        attachments: &[MailAttachmentRecord],
    ) -> Result<(), Error> {
        self.push(
            master,
            ChatRecord {
                id: Uuid::new_v4().to_string(),
                contact_nick: contact_nick.to_string(),
                outgoing,
                ts_ms: now_ms(),
                payload: ChatRecordPayload::Mail {
                    thread_id: thread_id.to_string(),
                    subject: subject.map(|s| s.to_string()),
                    body: body.to_string(),
                    attachments: attachments.to_vec(),
                },
            },
        )
    }

    /// Mail records only, in log order. The Mail rail filters the same encrypted
    /// log the chat rail reads rather than keeping a second store.
    pub fn mail_records(&self) -> impl Iterator<Item = &ChatRecord> {
        self.file
            .messages
            .iter()
            .filter(|r| matches!(r.payload, ChatRecordPayload::Mail { .. }))
    }
```

Re-export from `lib.rs`: `pub use chat_log::{ChatRecord, ChatRecordPayload, MailAttachmentRecord};`
and `pub use payload::{MailAttachment, MessagePayload, MAX_INLINE_FILE_BYTES, MAX_MAIL_ATTACHMENTS};`

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --lib`

---

### Task 5: `Account::send_mail`, recv, and the async send path

**Files:**
- Modify: `crates/hush-core/src/account.rs`, `crates/hush-core/src/message.rs`
- Test: `crates/hush-core/tests/internal_mail.rs`

**Interfaces:**
- Produces: `Account::send_mail(&str, Option<&str>, Option<&str>, &str, &[(String, Vec<u8>)]) -> Result<String, Error>` (returns the thread id), `Account::mail_records() -> Vec<ChatRecord>`; `ReceivedMessage.mail_attachments: Vec<ReceivedAttachment>`
- Consumes: `MessagePayload::Mail`, `ChatLog::append_mail`, `send_payload_inner`

- [ ] **Step 1: Write the failing test**

```rust
// crates/hush-core/tests/internal_mail.rs
use hush_core::MessagePayload;
use hush_relay::{router, RelayConfig};
use tokio::time::{sleep, Duration};

async fn spawn_relay() -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("relay.db");
    let app = router(db.to_str().unwrap(), RelayConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    sleep(Duration::from_millis(80)).await;
    (base, dir)
}

#[tokio::test]
async fn mail_arrives_while_the_recipient_is_offline() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "pw", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", &relay).await.unwrap();

    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();

    // No presence heartbeat: this is the whole point of mail.
    let thread = alice
        .send_mail("bob", None, Some("lunch"), "are you free?", &[("plan.txt".into(), b"12:30".to_vec())])
        .await
        .unwrap();

    let inbox = bob.recv().await.unwrap();
    assert_eq!(inbox.messages.len(), 1);
    match &inbox.messages[0].payload {
        MessagePayload::Mail { thread_id, subject, body, attachments } => {
            assert_eq!(thread_id, &thread);
            assert_eq!(subject.as_deref(), Some("lunch"));
            assert_eq!(body, "are you free?");
            assert_eq!(attachments.len(), 1);
        }
        other => panic!("expected mail, got {other:?}"),
    }
    assert_eq!(inbox.messages[0].mail_attachments.len(), 1);

    // The received attachment is readable from the vault blob after the fact.
    let blob_id = inbox.messages[0].mail_attachments[0].blob_id.clone();
    assert_eq!(bob.read_blob(&blob_id).unwrap(), b"12:30");

    // And the record is in Bob's encrypted log.
    assert_eq!(bob.mail_records().len(), 1);
}

#[tokio::test]
async fn a_reply_lands_in_the_same_thread() {
    let (relay, _relay_dir) = spawn_relay().await;
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();
    let mut alice = Account::create(alice_dir.path(), "pw", &relay).await.unwrap();
    let mut bob = Account::create(bob_dir.path(), "pw", &relay).await.unwrap();
    let alice_invite = alice.invite().await.unwrap();
    let bob_invite = bob.invite().await.unwrap();
    bob.add_contact("alice", &alice_invite).await.unwrap();
    alice.add_contact("bob", &bob_invite).await.unwrap();

    let thread = alice.send_mail("bob", None, Some("lunch"), "free?", &[]).await.unwrap();
    bob.recv().await.unwrap();
    let reply = bob
        .send_mail("alice", Some(&thread), Some("Re: lunch"), "yes", &[])
        .await
        .unwrap();
    assert_eq!(reply, thread, "a reply must reuse the thread id");
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test internal_mail mail_arrives`
Expected: compile error, `send_mail` not found.

- [ ] **Step 3: Minimal implementation**

`message.rs` — carry attachment blob ids out of `recv`:

```rust
#[derive(Debug, Clone)]
pub struct ReceivedAttachment {
    pub name: String,
    pub blob_id: crate::vault::BlobId,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub from_nick: String,
    pub payload: crate::payload::MessagePayload,
    pub file_blob_id: Option<crate::vault::BlobId>,
    /// Blob ids for mail attachments, in payload order. Empty for text and file.
    pub mail_attachments: Vec<ReceivedAttachment>,
}
```

`account.rs` — split the presence check out of `send_payload` so mail can skip it without changing the
signature chat already uses:

```rust
    async fn send_payload(&mut self, nickname: &str, payload: MessagePayload) -> Result<(), Error> {
        self.send_payload_inner(nickname, payload, true).await
    }

    /// `require_online` is false for mail: mail is delivered into the recipient's
    /// store-and-forward queue while they are away, which is exactly the
    /// difference from live-only chat (5h).
    async fn send_payload_inner(
        &mut self,
        nickname: &str,
        payload: MessagePayload,
        require_online: bool,
    ) -> Result<(), Error> {
        let contact = self
            .find_contact(nickname)
            .ok_or(Error::ContactNotFound)?
            .clone();
        if require_online && !self.contact_is_online(nickname).await? {
            return Err(Error::ContactOffline);
        }
        let plain = payload.encode()?;
        let ct = encrypt_message(
            self.device.store_mut(),
            &self.state.protocol_name,
            &contact.protocol_name,
            plain.as_slice(),
        )
        .await?;
        let body = encode_relay_blob(&ct);
        let relay = RelayClient::try_new(&self.state.relay)?;
        relay
            .put(&contact.queue_id, &contact.queue_token, &body)
            .await?;
        self.save().await?;
        Ok(())
    }
```

Then `send_mail` and `mail_records`:

```rust
    /// Send an internal mail message, returning the thread id.
    ///
    /// Mail is async by design: unlike `send_text`, this does not check whether
    /// the contact is online. Attachments are inlined on the wire (capped) and
    /// also stored as encrypted vault blobs on our side so the sent message is
    /// openable after a restart.
    pub async fn send_mail(
        &mut self,
        nickname: &str,
        thread_id: Option<&str>,
        subject: Option<&str>,
        body: &str,
        attachments: &[(String, Vec<u8>)],
    ) -> Result<String, Error> {
        if attachments.len() > MAX_MAIL_ATTACHMENTS {
            return Err(Error::TooManyAttachments(MAX_MAIL_ATTACHMENTS));
        }
        let total: usize = attachments.iter().map(|(_, d)| d.len()).sum();
        if total > MAX_INLINE_FILE_BYTES {
            return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES));
        }
        let thread_id = thread_id
            .map(|t| t.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let wire: Vec<MailAttachment> = attachments
            .iter()
            .map(|(name, data)| MailAttachment {
                name: name.clone(),
                data: data.clone(),
            })
            .collect();
        self.send_payload_inner(
            nickname,
            MessagePayload::Mail {
                thread_id: thread_id.clone(),
                subject: subject.map(|s| s.to_string()),
                body: body.to_string(),
                attachments: wire,
            },
            false,
        )
        .await?;

        // Store our own copies as blobs, then record the message.
        let mut records = Vec::with_capacity(attachments.len());
        for (name, data) in attachments {
            let id = self.vault.put_blob(data)?;
            records.push(MailAttachmentRecord {
                name: name.clone(),
                blob_id_hex: hex::encode(id.0),
                size: data.len() as u64,
            });
        }
        let master = *self.vault.master_key();
        self.chat_log
            .append_mail(&master, nickname, true, &thread_id, subject, body, &records)?;
        Ok(thread_id)
    }

    pub fn mail_records(&self) -> Vec<crate::chat_log::ChatRecord> {
        self.chat_log.mail_records().cloned().collect()
    }
```

`recv` — replace the `let file_blob_id = if let MessagePayload::File ... else ...` block with a match
that also handles `Mail`, and construct the `ReceivedMessage` with `mail_attachments`:

```rust
            let (file_blob_id, mail_attachments) = match &payload {
                MessagePayload::File { data, name } => {
                    let id = self.vault.put_blob(data)?;
                    let master = *self.vault.master_key();
                    let nick = self
                        .contact_nick_for_protocol(&from_protocol)
                        .unwrap_or_else(|| short_name(&from_protocol));
                    self.chat_log
                        .append_file(&master, &nick, false, name, &id, data.len() as u64)?;
                    (Some(id), Vec::new())
                }
                MessagePayload::Mail {
                    thread_id,
                    subject,
                    body,
                    attachments,
                } => {
                    let mut blobs = Vec::with_capacity(attachments.len());
                    let mut records = Vec::with_capacity(attachments.len());
                    for a in attachments {
                        let id = self.vault.put_blob(&a.data)?;
                        records.push(MailAttachmentRecord {
                            name: a.name.clone(),
                            blob_id_hex: hex::encode(id.0),
                            size: a.data.len() as u64,
                        });
                        blobs.push(ReceivedAttachment {
                            name: a.name.clone(),
                            blob_id: id,
                            size: a.data.len() as u64,
                        });
                    }
                    let master = *self.vault.master_key();
                    let nick = self
                        .contact_nick_for_protocol(&from_protocol)
                        .unwrap_or_else(|| short_name(&from_protocol));
                    self.chat_log.append_mail(
                        &master,
                        &nick,
                        false,
                        thread_id,
                        subject.as_deref(),
                        body,
                        &records,
                    )?;
                    (None, blobs)
                }
                MessagePayload::Text { body } => {
                    let master = *self.vault.master_key();
                    let nick = self
                        .contact_nick_for_protocol(&from_protocol)
                        .unwrap_or_else(|| short_name(&from_protocol));
                    self.chat_log.append_text(&master, &nick, false, body)?;
                    (None, Vec::new())
                }
                // `OwnCopy` is handled above and never reaches here.
                MessagePayload::OwnCopy { .. } => (None, Vec::new()),
            };
```

and:

```rust
            out.push(ReceivedMessage {
                from_nick: nick,
                payload,
                file_blob_id,
                mail_attachments,
            });
```

Add `use uuid::Uuid;` and the new imports to `account.rs`. Re-export `ReceivedAttachment` from
`lib.rs`.

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test internal_mail`
Then the whole core suite: `cargo test -p hush-core`

---

### Task 6: Daemon mail endpoints

**Files:**
- Modify: `crates/hush-daemon/src/main.rs`, `crates/hush-daemon/src/account_worker.rs`
- Test: extend `scripts/smoke-mail.sh`

**Interfaces:**
- Produces: `POST /api/mail/send { to, thread_id?, subject?, body, attachments: [{name, data_base64}] } -> { thread_id }`, `GET /api/mail -> Vec<ChatRecord>`; `kind: "mail"` in `/api/recv`
- Consumes: `Account::send_mail`, `Account::mail_records`

- [ ] **Step 1: Extend the failing smoke check**

Add to `scripts/smoke-mail.sh`, after the handle checks: create a second vault/daemon (or reuse the
`setup-two-peers.sh` pattern), exchange invites, then:

```bash
THREAD=$(curl -sf -X POST "$API/api/mail/send" -H 'content-type: application/json' \
  -d '{"to":"bob","subject":"hi","body":"body","attachments":[]}' | sed -n 's/.*"thread_id":"\([^"]*\)".*/\1/p')
[ -n "$THREAD" ] || { echo "no thread id"; exit 1; }
curl -sf "$API/api/mail" | grep -q "\"thread_id\":\"$THREAD\"" || { echo "mail not listed"; exit 1; }
```

- [ ] **Step 2: Run — expect FAIL**

Run: `./scripts/smoke-mail.sh`
Expected: `404` on `/api/mail/send`.

- [ ] **Step 3: Minimal implementation**

`account_worker.rs`: `SendMail` and `MailRecords` work items, `MailSent`/`Mail` results, and the
methods, mirroring `SendFileBytes`/`Messages`.

```rust
    SendMail {
        to: String,
        thread_id: Option<String>,
        subject: Option<String>,
        body: String,
        attachments: Vec<(String, Vec<u8>)>,
        reply: oneshot::Sender<WorkResult>,
    },
    MailRecords {
        reply: oneshot::Sender<WorkResult>,
    },
```

```rust
WorkResult::MailSent(String),
WorkResult::Mail(Vec<ChatRecord>),
```

```rust
    pub async fn send_mail(
        &self,
        to: &str,
        thread_id: Option<String>,
        subject: Option<String>,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
    ) -> WorkResult {
        self.call(|reply| Work::SendMail {
            to: to.to_string(),
            thread_id,
            subject,
            body: body.to_string(),
            attachments,
            reply,
        })
        .await
    }
```

`main.rs`: request/response types, handlers, routes.

```rust
#[derive(Deserialize)]
struct MailAttachmentRequest {
    name: String,
    data_base64: String,
}

#[derive(Deserialize)]
struct SendMailRequest {
    to: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    body: String,
    #[serde(default)]
    attachments: Vec<MailAttachmentRequest>,
}

#[derive(Serialize)]
struct SendMailResponse {
    thread_id: String,
}
```

```rust
async fn send_mail(
    State(state): State<Arc<App>>,
    Json(body): Json<SendMailRequest>,
) -> Result<Json<SendMailResponse>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    let mut attachments = Vec::with_capacity(body.attachments.len());
    for a in &body.attachments {
        let data = STANDARD.decode(a.data_base64.trim()).map_err(|_| {
            (StatusCode::BAD_REQUEST, Json(ApiError { error: "invalid base64 attachment".into() }))
        })?;
        attachments.push((a.name.clone(), data));
    }
    let result = map_err(
        state
            .worker
            .send_mail(&body.to, body.thread_id, body.subject, &body.body, attachments)
            .await,
    )?;
    match result {
        WorkResult::MailSent(thread_id) => Ok(Json(SendMailResponse { thread_id })),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: "unexpected worker response".into() }))),
    }
}

async fn mail(
    State(state): State<Arc<App>>,
) -> Result<Json<Vec<ChatRecord>>, (StatusCode, Json<ApiError>)> {
    touch(&state).await;
    match map_err(state.worker.mail_records().await)? {
        WorkResult::Mail(list) => Ok(Json(list)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error: "unexpected worker response".into() }))),
    }
}
```

Routes:

```rust
        .route("/api/mail/send", post(send_mail))
        .route("/api/mail", get(mail))
```

Finally, `ReceivedDto` gains `thread_id`, `subject`, `attachments: Vec<MailAttachmentDto>` and `to_dto`
gains the `Mail` arm:

```rust
#[derive(Serialize)]
struct MailAttachmentDto {
    name: String,
    size: u64,
    blob_id: String,
}
```

```rust
fn to_dto(msg: ReceivedMessage) -> Option<ReceivedDto> {
    let from = msg.from_nick;
    let mail_attachments = msg.mail_attachments;
    match msg.payload {
        MessagePayload::Text { body } => Some(ReceivedDto {
            from, kind: "text".into(), text: Some(body),
            file_name: None, file_size: None, blob_id: None,
            thread_id: None, subject: None, attachments: Vec::new(),
        }),
        MessagePayload::File { name, data } => Some(ReceivedDto {
            from, kind: "file".into(), text: None,
            file_name: Some(name), file_size: Some(data.len() as u64),
            blob_id: msg.file_blob_id.map(|id| hex::encode(id.0)),
            thread_id: None, subject: None, attachments: Vec::new(),
        }),
        MessagePayload::Mail { thread_id, subject, body, .. } => Some(ReceivedDto {
            from, kind: "mail".into(), text: Some(body),
            file_name: None, file_size: None, blob_id: None,
            thread_id: Some(thread_id), subject,
            attachments: mail_attachments
                .into_iter()
                .map(|a| MailAttachmentDto {
                    name: a.name, size: a.size, blob_id: hex::encode(a.blob_id.0),
                })
                .collect(),
        }),
        MessagePayload::OwnCopy { .. } => None,
    }
}
```

`msg.file_blob_id` is used inside the `File` arm after `msg.payload` is matched by value; if the borrow
checker complains, destructure `msg` into its parts before the match, as above.

- [ ] **Step 4: Run — expect PASS**

Run: `./scripts/smoke-mail.sh` and `cargo test --workspace`

---

## Phase 3 — Mail UX (5d.2)

### Task 7: UI types, daemon client, and the mail store

**Files:**
- Modify: `apps/hush-tauri/src/lib/api/types.ts`, `apps/hush-tauri/src/lib/api/daemon.ts`, `apps/hush-tauri/src/lib/icons.ts`
- Create: `apps/hush-tauri/src/lib/mailStore.ts`
- Test: `npx tsc --noEmit`

**Interfaces:**
- Produces: `MailAttachmentDto`, `MailMessage`, `mailStore` helpers, `api.sendMail/mail/setMailHandle`
- Consumes: `ChatRecordDto` union, `ReceivedDto`

- [ ] **Step 1: Extend the types**

```ts
// types.ts
export interface MailAttachmentDto {
  name: string;
  size: number;
  blob_id: string;
}

export interface MailAttachmentRef {
  name: string;
  size: number;
  blobId: string;
}

export interface ChatRecordDto {
  id: string;
  contact_nick: string;
  outgoing: boolean;
  ts_ms: number;
  payload:
    | { t: "text"; body: string }
    | { t: "file"; name: string; blob_id_hex: string; size: number }
    | {
        t: "mail";
        thread_id: string;
        subject?: string | null;
        body: string;
        attachments: { name: string; blob_id_hex: string; size: number }[];
      };
}

export interface ReceivedDto {
  from: string;
  kind: string;
  text?: string;
  file_name?: string;
  file_size?: number;
  blob_id?: string;
  thread_id?: string;
  subject?: string;
  attachments?: MailAttachmentDto[];
}
```

Add `mail_handle?: string | null` to `ContactDto`, and `mail_handle?: string | null` +
`mail_domain?: string` to `StatusResponse`.

- [ ] **Step 2: Create `mailStore.ts`**

```ts
import type { ChatRecordDto, ReceivedDto } from "./api/types";

export interface MailMessage {
  id: string;
  threadId: string;
  contactNick: string;
  outgoing: boolean;
  subject?: string;
  body: string;
  attachments: { name: string; size: number; blobId: string }[];
  createdAt: number;
}

export interface MailThread {
  threadId: string;
  contactNick: string;
  subject: string;
  lastAt: number;
  count: number;
}

const byThread = new Map<string, MailMessage[]>();
let seq = 0;

function nextId(): string {
  seq += 1;
  return `mail-${Date.now()}-${seq}`;
}

export function clearMailStore(): void {
  byThread.clear();
  seq = 0;
}

function push(m: MailMessage): void {
  const list = byThread.get(m.threadId) ?? [];
  list.push(m);
  byThread.set(m.threadId, list);
}

export function loadMail(records: ChatRecordDto[]): void {
  clearMailStore();
  for (const r of records) {
    if (r.payload.t !== "mail") continue;
    push({
      id: r.id,
      threadId: r.payload.thread_id,
      contactNick: r.contact_nick,
      outgoing: r.outgoing,
      subject: r.payload.subject ?? undefined,
      body: r.payload.body,
      attachments: r.payload.attachments.map((a) => ({
        name: a.name,
        size: a.size,
        blobId: a.blob_id_hex.toLowerCase(),
      })),
      createdAt: r.ts_ms,
    });
  }
}

export function ingestIncomingMail(dto: ReceivedDto): MailMessage | null {
  if (dto.kind !== "mail" || !dto.thread_id) return null;
  const m: MailMessage = {
    id: nextId(),
    threadId: dto.thread_id,
    contactNick: dto.from,
    outgoing: false,
    subject: dto.subject,
    body: dto.text ?? "",
    attachments: (dto.attachments ?? []).map((a) => ({
      name: a.name,
      size: a.size,
      blobId: a.blob_id.toLowerCase(),
    })),
    createdAt: Date.now(),
  };
  push(m);
  return m;
}

export function addOutgoingMail(
  threadId: string,
  contactNick: string,
  subject: string | undefined,
  body: string,
  attachments: { name: string; size: number; blobId: string }[],
): MailMessage {
  const m: MailMessage = {
    id: nextId(),
    threadId,
    contactNick,
    outgoing: true,
    subject,
    body,
    attachments,
    createdAt: Date.now(),
  };
  push(m);
  return m;
}

export function getThread(threadId: string): MailMessage[] {
  return byThread.get(threadId) ?? [];
}

export function listThreads(): MailThread[] {
  const out: MailThread[] = [];
  for (const [threadId, msgs] of byThread) {
    if (!msgs.length) continue;
    const last = msgs[msgs.length - 1];
    out.push({
      threadId,
      contactNick: last.contactNick,
      subject: msgs[0].subject || "(no subject)",
      lastAt: last.createdAt,
      count: msgs.length,
    });
  }
  return out.sort((a, b) => b.lastAt - a.lastAt);
}
```

- [ ] **Step 3: Add the client calls and the icon**

```ts
// daemon.ts, inside api
  mail: () => request<ChatRecordDto[]>("/api/mail"),
  sendMail: (
    to: string,
    body: { thread_id?: string; subject?: string; body: string; attachments: { name: string; data_base64: string }[] },
  ) =>
    request<{ thread_id: string }>("/api/mail/send", {
      method: "POST",
      body: JSON.stringify({ to, ...body }),
    }),
  setMailHandle: (handle: string) =>
    request<null>("/api/mail/handle", {
      method: "POST",
      body: JSON.stringify({ handle }),
    }),
```

```ts
// icons.ts
import { /* ... */ Mail } from "lucide";
// ICONS:
  mail: Mail,
```

- [ ] **Step 4: Typecheck**

Run: `cd apps/hush-tauri && npx tsc --noEmit`
Expected: no errors.

---

### Task 8: The Mail rail — thread list, thread view, compose, and the handle in Settings

**Files:**
- Modify: `apps/hush-tauri/src/app.ts`, `apps/hush-tauri/src/styles/app.css` (rail/mail styles)
- Test: `npx tsc --noEmit && npm run build`, then browser verification

**Interfaces:**
- Consumes: `mailStore`, `api.mail/sendMail/setMailHandle`, `api.blob`
- Produces: `Rail` includes `"mail"`; a `Mail` rail with threads, a compose form, and a Settings
  profile field

- [ ] **Step 1: Wire the rail and state**

```ts
type Rail = "chats" | "mail" | "files" | "vault" | "settings";
```

Add fields: `private mailThreadId: string | null = null;`, `private mailHandle: string | null = null;`,
`private mailDomain = "myserver.com";`.

In `renderRail`, insert after Chats: `rail.appendChild(this.railBtn("mail", "Mail", "mail"));`. In
`renderMain`, branch: `else if (this.rail === "mail") { shell.appendChild(this.renderMail()); }`. On
lock, clear `mailThreadId` and call `clearMailStore()` alongside `clearChatStore()`.

- [ ] **Step 2: Load mail and drain incoming**

After unlock, alongside the existing `api.messages()` load:

```ts
loadMail((await api.mail()) ?? []);
```

In the existing `recv` poll, for each dto: if `dto.kind === "mail"`, call
`ingestIncomingMail(dto)`; otherwise the existing `ingestIncoming`. After a `mail` dto, re-render if
the Mail rail is active.

- [ ] **Step 3: Implement `renderMail()`**

Two states, mirroring `renderFiles`:

- **Thread list** (`mailThreadId === null`): `listThreads()` mapped to rows showing subject, contact
  `displayNick`, relative time, and message count; empty state "No mail yet — write to a contact".
  A "New mail" button opens compose.
- **Thread view** (`mailThreadId !== null`): a header with the subject and contact, then the messages
  oldest-first, each rendering subject (if it changes), body, and attachment chips. Each attachment
  chip calls `api.blob(blobId)` and saves via the existing file-save helper used by chat file cards.
  A reply composer at the bottom: body + attachments, calling `api.sendMail(to, { thread_id, subject: "Re: ...", body, attachments })` then `addOutgoingMail(...)`.

- [ ] **Step 4: Compose**

Compose form: a contact `<select>` from `this.contacts` (label = `displayNick` + `mail_handle` when
present), an optional subject input, a body `<textarea>`, a multiple-file input
(`<input type="file" multiple>`), and Send. On send: read each file with the existing
`fileBytes`/`FileReader` helper, base64-encode, call `api.sendMail`, then `addOutgoingMail(threadId,
contact, subject, body, refs)` and switch to the new thread. Enforce the same caps client-side
(≤8 files, ≤256 KiB total) with an inline error, matching the daemon's rejection.

- [ ] **Step 5: Handle in Settings**

Add `"profile"` to `SettingsSection` and a rail entry. The section shows the current
`mail_handle@mail_domain` (or "No address yet") and an input + Save calling `api.setMailHandle`.
Validate client-side with the same rule (lowercase `[a-z0-9._-]`, 1–64, no leading/trailing
separator), and surface the daemon's 400 message if it rejects.

Read `mail_handle`/`mail_domain` from `/api/status` into the fields on unlock.

- [ ] **Step 6: Typecheck and build**

Run: `cd apps/hush-tauri && npx tsc --noEmit && npm run build`
Expected: no errors.

---

### Task 9: End-to-end verification

**Files:**
- Test: `scripts/run-two-ui.sh` (existing harness)

- [ ] **Step 1: Run the two-peer UI flow**

Run: `./scripts/run-two-ui.sh`
Manual checks (capture a screenshot for each):

1. Set a handle on both peers; each sees the other's `@` address in the Mail compose picker.
2. **Lock the recipient's vault** (so they are offline for chat), then send mail with a subject, body,
   and two attachments from the other peer. The send must succeed — no "offline" error.
3. Unlock the recipient; the mail appears in their Mail rail under a thread, attachments openable.
4. Reply; it lands in the same thread on both sides.
5. Lock both, restart, unlock; threads and attachments survive (5c.3).
6. Inspect the relay DB for the queued blob and confirm neither subject nor body appears as plaintext:
   `strings instances/relay/relay.db | rg -i "lunch|are you free"` returns nothing.

- [ ] **Step 2: Full suite**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: 0 failures, no new clippy errors (the repo has pre-existing warnings; do not add new ones).

---

## Phase 4 — Docs and close-out

### Task 10: Update the docs

**Files:**
- Modify: `docs/PROTOCOL.md`, `apps/hush-tauri/API_GAPS.md`, `docs/plans/2026-10-04-v1-milestones.md`, `DEEPSEEKPROBLEMS.MD`

- [ ] `docs/PROTOCOL.md` — add a "Mail" section: mail is a payload kind on the same session and queue;
  the handle is an invite-carried label with no relay directory; mail is store-and-forward while chat
  is live-only.
- [ ] `apps/hush-tauri/API_GAPS.md` — remove "async mail = Task 5d" from the live-presence gap and note
  the Mail rail is real.
- [ ] `docs/plans/2026-10-04-v1-milestones.md` — mark Task **5d** done in the build-order table, with
  the residual limitations from this plan.
- [ ] `DEEPSEEKPROBLEMS.MD` — log anything that cost a round trip during implementation, per the log's
  convention (append when it happens, not at the end).

---

## Acceptance criteria

- [ ] Two members set handles; each sees the other's `handle@myserver.com` in compose.
- [ ] Mail with a subject, a body, and multiple attachments is sent while the recipient is **offline**
  and is delivered when they next drain the queue.
- [ ] Replies land in the same thread; threads and attachments survive lock, restart, and unlock.
- [ ] The relay blob contains no plaintext subject, body, or attachment name.
- [ ] A handle change is reflected in the next invite; an invalid handle is rejected with a clear error.
- [ ] `cargo test --workspace` passes; `./scripts/smoke-mail.sh`, `./scripts/smoke-change-passphrase.sh`
  and `./scripts/smoke-daemon-socket.sh` pass; `npx tsc --noEmit` and `npm run build` are clean.

## Self-review notes

- **Base dependency.** Written against `61f3fcb` (multi-device Phase 3). Phase 4's `Contact.devices` and
  `send_payload` fan-out are uncommitted; `mail_handle` is a top-level `Contact` field and mail calls
  `send_payload_inner`, so both changes compose. Rebase before the PR and re-run the suite.
- **Why one log, not two.** A separate `mail.enc` would duplicate load/save, encryption, and backup
  handling for no benefit; the payload kind already discriminates the two rails. If mail later needs
  independent retention, split then (Rule of Three, not before).
- **Why attachments are inline.** v1 files are small by design (`MAX_INLINE_FILE_BYTES`); chunked
  attachments and an out-of-band blob fetch are a separate slice, and the plan for `5e`/`5f` already
  owns the container work.
- **What this plan deliberately does not do.** No relay directory, no handle search, no SMTP, no
  receiving from non-members (that is `5e`), no sending to outsiders (that is `5f`), no group mail.
- **Untested-by-construction risk.** The Mail rail's real behaviour needs the two-peer UI run; a green
  `cargo test` only covers the core and daemon. Task 9 is the evidence, not the unit suite.

---

## Phase 5 — Mail client UI: folders, drafts, and the Outbox (2026-10-07)

**Status:** Implemented and verified in a browser against a live two-daemon harness. This phase turns
the single thread list from Phase 3 into an email client: a folder rail, a message list with search, a
reading pane, a full-pane composer, encrypted drafts, and a retrying Outbox.

**Goal:** Mail should behave like mail. Everything a user does to a message — reading it, starring it,
archiving it, trashing it, drafting a reply — has a home in the UI, and a message that cannot reach the
relay is not lost.

### Settled design (continued)

| # | Decision |
|---|---|
| M8 | **Seven folders**: Inbox, Sent, Outbox, Drafts, Starred, Archive, Trash. Starred is a flag view, not a location, so a starred archived message appears in both. |
| M9 | **Folder membership is derived, not stored.** `mail_read`, `mail_starred`, `mail_archived`, `mail_trashed` are id lists in `ui-prefs.json`; the encrypted log is never rewritten to file a message. Deleting forever is the one operation that drops a record. |
| M10 | **Drafts live in the encrypted log**, as `ChatRecordPayload::Draft` alongside `Mail`. A draft is a message you have not sent; keeping it in `ui-prefs.json` would put its subject and body on disk in cleartext. |
| M11 | **The log is mutable for drafts and for the Outbox state flip.** `ChatLog` gained `replace_payload` and `remove_record`, so editing a draft rewrites its record instead of appending a second one, and a queued message flips to `Sent` in place. |
| M12 | **A retryable send failure queues instead of failing.** `send_mail` returns `MailOutcome { thread_id, queued }`; `Error::Relay` and `Error::Io` are recorded as `MailState::Queued` and the message lands in Outbox. Anything else (unknown contact, bad attachment) still fails loudly. |
| M13 | **The daemon retries the Outbox on a timer**, not from the UI: `outbox_loop` flushes queued mail every 30s while unlocked, so a message goes out even if the window is closed. |
| M14 | **`RelayClient` has a 15s request timeout.** A relay that accepts the connection and never answers would otherwise hang the `AccountWorker` — and with it every other work item. |
| M15 | **Compose is a full pane**, not a modal: To (contact picker), Subject, Body, attachments, Save draft, Discard draft (only when editing a saved draft), Send. |

### What changed

| File | Change |
|---|---|
| `crates/hush-core/src/chat_log.rs` | `MailState { Sent, Queued }`, `state` on `Mail`, `ChatRecordPayload::Draft`, `append_draft`, `replace_payload`, `remove_record`, `mail_records`, `draft_records`, `chat_records` |
| `crates/hush-core/src/account.rs` | `send_mail -> MailOutcome`, `retry_outbox`, `save_draft`, `delete_draft`, `delete_mail`, `queued_mail_count`, `draft_records` |
| `crates/hush-core/src/relay_client.rs` | `REQUEST_TIMEOUT` (15s) on the HTTP client |
| `crates/hush-core/tests/internal_mail.rs` | queue-and-retry, draft round-trip, draft not plaintext, draft is not chat history |
| `crates/hush-daemon/src/account_worker.rs` | `SaveDraft`, `DeleteDraft`, `DeleteMail`, `RetryOutbox`, `DraftRecords`; `MailSent { thread_id, queued }` |
| `crates/hush-daemon/src/main.rs` | `POST /api/mail/draft`, `DELETE /api/mail/draft/{id}`, `DELETE /api/mail/{id}`, `GET /api/mail` returns `{ mail, drafts }`, `outbox_loop`, `PUT`/`DELETE` in the dev CORS layer |
| `crates/hush-daemon/src/ui_prefs.rs` | the four mail flag lists |
| `apps/hush-tauri/src/lib/mailStore.ts` | rebuilt around folders, drafts, and flag-derived membership; `isUnread`, `inFolder`, `listFolder`, `searchMessages`, `folderCount`, `unreadCount` |
| `apps/hush-tauri/src/app.ts` | folder sidebar, message list + search, reading pane with action toolbar, draft list, full-pane composer, discard |
| `apps/hush-tauri/src/lib/icons.ts` | `inbox`, `clock`, `file-text`, `star`, `archive`, `trash-2`, `reply`, `chevron-left`, `x` |
| `apps/hush-tauri/src/styles/app.css` | folder rail, message rows, reading pane, composer |

### Verification (fresh output, this session)

- `cargo test -p hush-core` — all suites pass, including
  `mail_queues_while_the_relay_is_down_and_goes_out_on_retry`, `a_draft_is_saved_edited_and_deleted`,
  `a_draft_body_is_not_plaintext_on_disk`, `a_draft_is_not_chat_history`,
  `a_queued_message_keeps_its_state_across_a_reload`.
- `bash scripts/smoke-mail.sh` — 13 checks pass end to end over a real relay.
- `cd apps/hush-tauri && npx tsc --noEmit && npm run build` — clean.
- `cargo clippy --workspace --all-targets` — the pre-existing baseline only (7 `redundant_closure` in
  `backup.rs`, 4 `ptr_arg`, 1 `items_after_test_module`); the `type_complexity` this phase introduced in
  `decode_attachments` was fixed with an `ApiErrorResponse` alias.
- Browser run against a live harness (daemon `127.0.0.1:9876`, second peer `9877`, relay `8799`):
  - archive moved a message out of Inbox into Archive, and it stayed in Starred;
  - Move to Trash → Trash → Restore to Inbox returned it to Inbox with the Trash badge cleared;
  - Move to Trash → Delete forever removed the record: `chat.enc` shrank, Trash emptied, and the id was
    dropped from all four flag lists in `ui-prefs.json`;
  - Save draft wrote a `Draft` record (`chat.enc` 1006 → 1318 bytes) with no plaintext subject on disk,
    the Drafts badge appeared, and reopening the draft refilled To/Subject/Body; re-saving replaced the
    record in place (no duplicate, log roughly unchanged);
  - Discard draft removed the record and returned `chat.enc` to 1006 bytes;
  - with the relay killed, Send produced `queued: true`, the message appeared in Outbox with the
    "Queued — the relay is unreachable" toast, and after the relay restarted the daemon's `outbox_loop`
    delivered it: the record flipped to `state: "sent"`, the sender's Outbox badge cleared, Sent went to
    3, and the recipient ingested it under the same `thread_id`.

### Known limitations to record, not hide

- **Flag lists are pruned only on delete forever.** Ids in `mail_read`/`mail_starred`/`mail_archived`/
  `mail_trashed` are dropped by the delete-forever path; a record removed by any future route would
  leave its id behind. Harmless today (the lists are only read as sets) but worth a collection pass if a
  second removal route appears.
- **Delete forever orphans attachment blobs.** The vault has no collection pass, so the bytes stay
  until the vault is rewritten. Same trade chat already makes for deleted files.
- **Attachment blobs are not garbage collected** for the same reason, and `put_blob` is
  content-addressed, so re-saving a draft with the same file does not grow the vault.
- **No drag-and-drop, no multi-select, no per-message delete inside a thread.** Actions apply to the
  message you opened, falling back to the newest message in the thread.
- **The Inbox badge shows unread when there is any, otherwise the total**, so it reads as a count of
  what wants attention. The other folders always show their total.

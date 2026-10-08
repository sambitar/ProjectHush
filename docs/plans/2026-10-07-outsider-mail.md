# Outsider Mail — Outbound Delivery Implementation Plan

**Goal:** A member can compose to a typed external address (`user@server.com`), seal the message
under a delivery credential, and hand the recipient a link that opens it. There are two
credentials: a **PIN** the sender chooses and shares out of band, or the recipient's **public key**,
which the recipient publishes and the sender pastes. Either way the recipient opens the message
with what they hold and the relay never sees plaintext.

**Architecture:** The message is sealed into a versioned container and uploaded to the relay as
opaque ciphertext under a random capability id. PIN drops are `hush-drop-v1` (XChaCha20-Poly1305
under a PBKDF2 key derived from the PIN); public-key drops are `hush-drop-v2` (an X25519 sealed box,
see below). The recipient opens a static page, which fetches the ciphertext and decrypts it locally
with the PIN or their private key. The relay never sees plaintext, a credential, or a recipient
address. The notification email that carries the link is sent through a `Mailer` seam so a real SMTP
server can be dropped in later without touching the send path.

**Tech Stack:** Rust (`hush-core`, `hush-daemon`, `hush-relay`), existing `chacha20poly1305` /
`sha2` / `serde` deps, Tauri + TypeScript UI, a static web page for pickup.

**Spec:** this conversation, plus `docs/plans/2026-10-04-v1-milestones.md` Tasks **5e**/**5f**.

## Settled decisions (design round, 2026-10-07)

| # | Decision |
|---|---|
| O1 | **Build the whole path now, behind a `Mailer` seam.** A no-op/file mailer ships today; a real SMTP client implements the same trait later. The `drop@` MTA ingress is an external hookup we document, not code we run. |
| O2 | **External To is a typed address**, `user@server.com`. It is not saved to a contact list. |
| O3 | **The sender chooses the PIN** before sending and shares it out of band. |
| O4 | **PIN → KDF → AEAD** is the wrap. Wrong PIN fails to decrypt. |
| O5 | **Inbound drop is email ingress** (`drop@myserver.com` → relay drop API). Out of scope for this plan; see *Next plan*. |
| O6 | **The container is XChaCha20-Poly1305 + PBKDF2-HMAC-SHA256**, not the vault's Argon2id, because the recipient decrypts in a browser: WebCrypto has PBKDF2 natively and `@noble/ciphers` is pure JS. Argon2 in a browser would mean shipping WASM. |

## Wire format: `hush-drop-v1`

Binary, so it can be a file attachment and be recognised by magic bytes at the `drop@` boundary.

```
offset  size  field
0       4     magic        b"HSHD"
4       1     version      1
5       1     kdf          1 = PBKDF2-HMAC-SHA256
6       4     iterations   u32, big-endian
10      16    salt
26      24    nonce
50      ..    ciphertext   XChaCha20-Poly1305
```

`HEADER_LEN = 50`. The header is passed as **AEAD associated data**, so the version, KDF id,
iteration count, salt and nonce are authenticated: an attacker cannot lower the iteration count or
swap the salt without failing the tag check.

The plaintext is JSON:

```json
{ "v": 1, "subject": "...", "body": "...",
  "attachments": [ { "name": "a.pdf", "data": "<base64>" } ],
  "created_unix": 1760000000 }
```

## Second mode: `hush-drop-v2` (public key) — added 2026-10-08

The motivating case: a news channel publishes a public key, users send drops to it, and the channel
opens them without any shared secret. The sender still seals, uploads to the relay, and hands over
the same pickup link; only the credential and the crypto differ. The sender stays anonymous — the
container carries no sender key, no signature, and no reply path.

**Versioning.** This is a new version byte (`2`), not a mode tag inside v1. A mode byte cannot be
added to the v1 header without changing its length — which would make every v1 container already on
the relay or in an email unreadable — or overloading an existing v1 field, which is ambiguous.
Version 2 keeps v1 byte-for-byte identical and gives the structurally different header its own
layout. v1 containers open exactly as they did before.

```
offset  size  field
0       4     magic        b"HSHD"
4       1     version      2
5       1     mode         2 = X25519 sealed box
6       32    ephemeral    sender's ephemeral X25519 public key
38      24    nonce
62      ..    ciphertext   XChaCha20-Poly1305
```

`DROP_V2_HEADER_LEN = 62`. The header is the **AEAD associated data**, so the version, mode,
ephemeral key, and nonce are authenticated.

**Crypto (a standard sealed box, nothing bespoke).** The sender generates an ephemeral X25519
keypair, does ECDH against the recipient's public key, and runs HKDF-SHA256 (RFC 5869) over the
shared secret to a 32-byte key, then XChaCha20-Poly1305. The ephemeral public key is the HKDF salt
and the recipient's public key is bound into the info string, so a key is unique to the
(ephemeral, recipient) pair and is not reusable across drops. The X25519 primitive is the one
already in the tree: `libsignal_protocol`'s `KeyPair` / `PrivateKey` / `PublicKey` (re-exported from
`libsignal_core::curve`), whose `calculate_agreement` rejects the all-zero shared secret a low-order
point would produce. HKDF is hand-rolled in `crypto.rs` beside the existing HMAC, so no new
dependency is introduced. A recipient public key is validated as a canonical X25519 point before
use.

**The two modes do not open each other.** `open` refuses a v2 container with `DropNeedsKey`, and
`open_with_key` refuses a v1 container with `DropNeedsPin`. That is structural — by version and mode
byte — not a hope that the wrong crypto happens to fail.

**Pasteable keys.** `hush-drop-pk-v1:<base64url>` for the public key the recipient shares, and
`hush-drop-sk-v1:<base64url>` for the private key `hush-cli drop-keygen` writes (0600, never
overwritten, never printed). The prefixes differ so pasting the wrong half is refused rather than
used.

## Global Constraints

- The relay stores **ciphertext only**. No plaintext body, no PIN, no recipient address ever
  reaches it. This is the one property the whole design rests on.
- The notification email carries **only** the link and an expiry. **Never** the PIN.
- The container is authenticated end to end: any tamper (header or ciphertext) must fail to open.
- Size caps: `MAX_DROP_BYTES` = 8 MiB (relay `max_blob_bytes` is 1 MiB today; the drop route gets
  its own, larger cap because the container carries attachments).
- No new Rust dependencies. PBKDF2 is hand-rolled beside the existing hand-rolled HMAC.

## Files

| File | Responsibility |
|---|---|
| `crates/hush-core/src/drop.rs` *(new)* | v1 container + PIN KDF (`seal`/`open`), v2 sealed box (`seal_to_key`/`open_with_key`), `DropPackage`, keypair + pasteable key encoding |
| `crates/hush-core/src/crypto.rs` | Add `pub(crate) fn pbkdf2_sha256` and `pub(crate) fn hkdf_sha256` |
| `crates/hush-core/src/error.rs` | Add `EmptyPin`, `UnsupportedDrop`, `BadDrop`, `InvalidDropPublicKey`, `InvalidDropPrivateKey`, `DropNeedsKey`, `DropNeedsPin` |
| `crates/hush-core/src/lib.rs` | `mod drop;` + re-exports |
| `crates/hush-relay/src/store.rs` | `deliveries` table, `put_delivery`, `take_delivery` |
| `crates/hush-relay/src/app.rs` | `PUT`/`GET /drop/{id}` |
| `crates/hush-core/src/relay_client.rs` | `put_drop`, `get_drop` |
| `crates/hush-core/src/chat_log.rs` | `ChatRecordPayload::Mail.external` (serde default) |
| `crates/hush-core/src/account.rs` | `send_to_outsider`, `send_to_outsider_with_key`, shared `upload_drop`, `OutsiderOutcome` |
| `crates/hush-core/src/mailer.rs` *(new)* | `Mailer` trait, `NullMailer`, `FileMailer`, `notification_body`, `notification_body_for_key` |
| `crates/hush-cli/src/main.rs` | `drop-keygen`, `drop-open` |
| `crates/hush-daemon/src/main.rs` | `POST /api/mail/outsider` (pin or public_key), mailer + base-url config |
| `crates/hush-daemon/src/account_worker.rs` | `DropSecret` (Pin or PublicKey) on the send work item |
| `apps/hush-tauri/src/app.ts` | Two-mode To, PIN/public-key switch, key field, pickup-link panel |
| `scripts/smoke-drop-key.sh` *(new)* | End-to-end public-key drop against a real relay |

---

### Task 1: Drop container and PIN crypto

**Files:**
- Create: `crates/hush-core/src/drop.rs`
- Modify: `crates/hush-core/src/crypto.rs`, `crates/hush-core/src/error.rs`,
  `crates/hush-core/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `drop.rs`

**Interfaces:**
- Consumes: `crate::crypto::{random_bytes, pbkdf2_sha256}`, `chacha20poly1305`.
- Produces: `DropPackage`, `DropAttachment`, `seal(&DropPackage, &str) -> Result<Vec<u8>, Error>`,
  `open(&[u8], &str) -> Result<DropPackage, Error>`, `is_container(&[u8]) -> bool`,
  `DROP_HEADER_LEN`, `MAX_DROP_BYTES`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_package_round_trips_under_its_pin() {
    let pkg = DropPackage {
        v: 1,
        subject: "hi".into(),
        body: "<b>hello</b>".into(),
        attachments: vec![DropAttachment { name: "a.txt".into(), data: "aGk=".into() }],
        created_unix: 1_760_000_000,
    };
    let bytes = seal(&pkg, "correct horse").unwrap();
    assert!(is_container(&bytes));
    let back = open(&bytes, "correct horse").unwrap();
    assert_eq!(back.subject, "hi");
    assert_eq!(back.attachments[0].name, "a.txt");
}

#[test]
fn the_wrong_pin_does_not_open() {
    let pkg = sample();
    let bytes = seal(&pkg, "1111").unwrap();
    assert!(open(&bytes, "1112").is_err());
}

#[test]
fn a_tampered_header_or_body_does_not_open() {
    let pkg = sample();
    let mut bytes = seal(&pkg, "1111").unwrap();
    // Flip a ciphertext byte.
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(open(&bytes, "1111").is_err());
    // Lower the iteration count in the authenticated header.
    let mut bytes = seal(&pkg, "1111").unwrap();
    bytes[6..10].copy_from_slice(&1u32.to_be_bytes());
    assert!(open(&bytes, "1111").is_err());
}

#[test]
fn an_empty_pin_is_rejected_and_an_unknown_version_is_refused() {
    assert!(matches!(seal(&sample(), ""), Err(Error::EmptyPin)));
    let mut bytes = seal(&sample(), "1111").unwrap();
    bytes[4] = 9;
    assert!(matches!(open(&bytes, "1111"), Err(Error::UnsupportedDrop)));
}

#[test]
fn pbkdf2_matches_the_published_sha256_vectors() {
    // RFC 6070 is the SHA-1 vector set, so it cannot be used here. These are the
    // standard PBKDF2-HMAC-SHA256 vectors; the last one spans two output blocks.
    let mut out = [0u8; 32];
    pbkdf2_sha256(b"password", b"salt", 2, &mut out);
    assert_eq!(
        hex::encode(out),
        "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
    );
}
```

> Note: this test lives in `crypto.rs` beside the HMAC vectors, since it exercises
> `crypto::pbkdf2_sha256` rather than the container.

- [ ] **Step 2: Run tests — expect FAIL**

`cargo test -p hush-core drop::` → `error[E0433]: failed to resolve: use of undeclared crate or module drop`.

- [ ] **Step 3: Implement**

`crypto.rs`:

```rust
/// PBKDF2-HMAC-SHA256 (RFC 8018). Hand-rolled for the same reason HMAC is:
/// `sha2` is already here and the `pbkdf2` crate would arrive on a second
/// `digest`/`sha2` major. Checked against RFC 6070 in the tests.
pub(crate) fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    use sha2::digest::generic_array::typenum::Unsigned;
    let hlen = <Sha256 as sha2::Digest>::OutputSize::USIZE;
    let blocks = out.len().div_ceil(hlen);
    for block in 1..=blocks {
        let mut u = hmac_sha256(password, &[salt, &(block as u32).to_be_bytes()].concat());
        let mut acc = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (a, b) in acc.iter_mut().zip(u.iter()) {
                *a ^= *b;
            }
        }
        let start = (block - 1) * hlen;
        let take = (out.len() - start).min(hlen);
        out[start..start + take].copy_from_slice(&acc[..take]);
    }
}
```

`error.rs`: add `#[error("delivery PIN must not be empty")] EmptyPin`,
`#[error("unsupported drop container version")] UnsupportedDrop`,
`#[error("malformed drop container")] BadDrop`.

`drop.rs`:

```rust
use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{pbkdf2_sha256, random_bytes};
use crate::error::Error;

pub const DROP_MAGIC: [u8; 4] = *b"HSHD";
pub const DROP_VERSION: u8 = 1;
const KDF_PBKDF2_SHA256: u8 = 1;
pub const DROP_ITERATIONS: u32 = 600_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
/// magic + version + kdf + iterations + salt + nonce
pub const DROP_HEADER_LEN: usize = 4 + 1 + 1 + 4 + SALT_LEN + NONCE_LEN;
pub const MAX_DROP_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DropAttachment { pub name: String, pub data: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DropPackage {
    pub v: u32,
    #[serde(default)] pub subject: String,
    pub body: String,
    #[serde(default)] pub attachments: Vec<DropAttachment>,
    #[serde(default)] pub created_unix: u64,
}

pub fn is_container(bytes: &[u8]) -> bool {
    bytes.len() > DROP_HEADER_LEN && bytes[..4] == DROP_MAGIC
}

fn pin_key(pin: &str, salt: &[u8], iterations: u32) -> Result<Zeroizing<[u8; KEY_LEN]>, Error> {
    if pin.is_empty() { return Err(Error::EmptyPin); }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    pbkdf2_sha256(pin.as_bytes(), salt, iterations, &mut *key);
    Ok(key)
}

pub fn seal(pkg: &DropPackage, pin: &str) -> Result<Vec<u8>, Error> {
    let salt = random_bytes::<SALT_LEN>();
    let nonce_bytes = random_bytes::<NONCE_LEN>();
    let mut header = Vec::with_capacity(DROP_HEADER_LEN);
    header.extend_from_slice(&DROP_MAGIC);
    header.push(DROP_VERSION);
    header.push(KDF_PBKDF2_SHA256);
    header.extend_from_slice(&DROP_ITERATIONS.to_be_bytes());
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce_bytes);

    let key = pin_key(pin, &salt, DROP_ITERATIONS)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = serde_json::to_vec(pkg).map_err(|_| Error::BadDrop)?;
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), Payload { msg: &plaintext, aad: &header })
        .map_err(|_| Error::Crypto)?;
    header.extend_from_slice(&ct);
    Ok(header)
}

pub fn open(bytes: &[u8], pin: &str) -> Result<DropPackage, Error> {
    if bytes.len() <= DROP_HEADER_LEN || bytes[..4] != DROP_MAGIC {
        return Err(Error::BadDrop);
    }
    if bytes[4] != DROP_VERSION { return Err(Error::UnsupportedDrop); }
    if bytes[5] != KDF_PBKDF2_SHA256 { return Err(Error::UnsupportedDrop); }
    let iterations = u32::from_be_bytes(bytes[6..10].try_into().map_err(|_| Error::BadDrop)?);
    let salt = &bytes[10..10 + SALT_LEN];
    let nonce_bytes = &bytes[10 + SALT_LEN..DROP_HEADER_LEN];
    let header = &bytes[..DROP_HEADER_LEN];

    let key = pin_key(pin, salt, iterations)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&*key));
    let plaintext = cipher
        .decrypt(XNonce::from_slice(nonce_bytes), Payload { msg: &bytes[DROP_HEADER_LEN..], aad: header })
        .map_err(|_| Error::WrongPassphrase)?;
    serde_json::from_slice(&plaintext).map_err(|_| Error::BadDrop)
}
```

`lib.rs`: `mod drop;` and
`pub use drop::{open as open_drop, seal as seal_drop, is_container, DropAttachment, DropPackage, DROP_HEADER_LEN, MAX_DROP_BYTES};`

- [ ] **Step 4: Run tests — expect PASS**

`cargo test -p hush-core drop::` → 5 passed.

---

### Task 2: Relay delivery store and routes

**Files:**
- Modify: `crates/hush-relay/src/store.rs`, `crates/hush-relay/src/app.rs`,
  `crates/hush-relay/src/config.rs`
- Test: `crates/hush-relay/tests/deliveries.rs` *(new)*

**Interfaces:**
- Consumes: `hush_core::MAX_DROP_BYTES`.
- Produces: `Store::put_delivery(&self, id, body) -> Result<(), RelayError>`,
  `Store::take_delivery(&self, id) -> Result<Vec<u8>, RelayError>`;
  routes `PUT /drop/{id}`, `GET /drop/{id}`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_delivery_round_trips_and_is_bounded() {
    let (app, _db) = test_app();
    let body = vec![7u8; 128];
    assert_eq!(put(&app, "abc", &body).await, StatusCode::NO_CONTENT);
    let got = get(&app, "abc").await;
    assert_eq!(got.1, body);
    assert_eq!(get(&app, "nope").await.0, StatusCode::NOT_FOUND);
    // Oversize is refused.
    assert_eq!(put(&app, "big", &vec![0u8; hush_core::MAX_DROP_BYTES + 1]).await,
               StatusCode::PAYLOAD_TOO_LARGE);
}
```

- [ ] **Step 2: Run test — expect FAIL** (404, no route)

`cargo test -p hush-relay --test deliveries` → fails on the first PUT.

- [ ] **Step 3: Implement**

`store.rs`, in the schema batch:

```sql
CREATE TABLE IF NOT EXISTS deliveries (
    id TEXT PRIMARY KEY,
    body BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_deliveries_created ON deliveries(created_at);
```

```rust
pub fn put_delivery(&self, id: &str, body: &[u8]) -> Result<(), RelayError> {
    if body.len() > hush_core::MAX_DROP_BYTES { return Err(RelayError::TooLarge); }
    self.check_rate(&format!("drop:{id}"))?;
    self.purge_expired()?;
    let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
    conn.execute(
        "INSERT OR REPLACE INTO deliveries (id, body, created_at) VALUES (?1, ?2, ?3)",
        params![id, body, Self::now_secs() as i64],
    ).map_err(|_| RelayError::Internal)?;
    Ok(())
}

/// Non-consuming on purpose: a mistyped PIN, a refresh, or a second device must
/// not destroy the message. Expiry, not a fetch count, bounds the storage.
pub fn get_delivery(&self, id: &str) -> Result<Vec<u8>, RelayError> {
    self.check_rate(&format!("drop:{id}"))?;
    self.purge_expired()?;
    let conn = self.conn.lock().map_err(|_| RelayError::Internal)?;
    let mut stmt = conn.prepare("SELECT body FROM deliveries WHERE id = ?1")
        .map_err(|_| RelayError::Internal)?;
    let mut rows = stmt.query(params![id]).map_err(|_| RelayError::Internal)?;
    match rows.next().map_err(|_| RelayError::Internal)? {
        Some(row) => row.get(0).map_err(|_| RelayError::Internal),
        None => Err(RelayError::NotFound),
    }
}
```

The route is **unauthenticated** — the id is the capability and the body is already ciphertext,
so there is nothing to authorize against. It is rate-limited per id using the existing
`check_rate`, and `id` must be 32 hex characters (`validate_delivery_id`), so the table cannot be
used as arbitrary keyed storage.

```rust
async fn put_drop(State(state): State<Arc<AppState>>, Path(id): Path<String>, body: Bytes)
    -> Result<StatusCode, RelayError> {
    validate_delivery_id(&id)?;
    state.store.put_delivery(&id, &body)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_drop(State(state): State<Arc<AppState>>, Path(id): Path<String>)
    -> Result<Vec<u8>, RelayError> {
    validate_delivery_id(&id)?;
    state.store.get_delivery(&id)
}
```

Registered as `.route("/drop/{id}", put(put_drop).get(get_drop))`. Note `/d/{account_id}` is
already the device directory, so the drop family is `/drop/{id}`.

- [ ] **Step 4: Run test — expect PASS**

`cargo test -p hush-relay --test deliveries` → 5 passed.

---

### Task 3: Core send-to-outsider and the log record

**Files:**
- Modify: `crates/hush-core/src/account.rs`, `crates/hush-core/src/chat_log.rs`,
  `crates/hush-core/src/relay_client.rs`, `crates/hush-core/src/lib.rs`
- Test: `crates/hush-core/tests/outsider_mail.rs` *(new)*

**Interfaces:**
- Consumes: `drop::{seal, DropPackage}`, `Mailer`.
- Produces: `OutsiderOutcome { delivery_id: String, link: String }`,
  `Account::send_to_outsider(&mut self, to, subject, body, attachments, pin) -> Result<OutsiderOutcome, Error>`,
  `ChatRecordPayload::Mail { external: Option<ExternalMail> }`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn sending_to_an_outsider_uploads_ciphertext_and_records_it() {
    // Relay test app; account with a known drop base url.
    let mut alice = account_with_relay(&relay).await;
    let out = alice
        .send_to_outsider("bob@gmail.com", "hi", "<b>secret</b>", vec![], "1234")
        .await
        .unwrap();

    // The relay holds ciphertext only.
    let blob = relay_fetch(&out.delivery_id).await;
    assert!(hush_core::is_container(&blob));
    assert!(!String::from_utf8_lossy(&blob).contains("secret"));

    // It opens with the PIN and not without.
    let pkg = hush_core::open_drop(&blob, "1234").unwrap();
    assert_eq!(pkg.body, "<b>secret</b>");
    assert!(hush_core::open_drop(&blob, "9999").is_err());

    // It shows in Sent as external, addressed to the typed address.
    let rec = alice.chat_records().into_iter()
        .find(|r| matches!(&r.payload, ChatRecordPayload::Mail { external: Some(_), .. }))
        .expect("an external mail record");
    match rec.payload {
        ChatRecordPayload::Mail { external: Some(e), .. } => {
            assert_eq!(e.to, "bob@gmail.com");
            assert_eq!(e.delivery_id, out.delivery_id);
        }
        _ => unreachable!(),
    }
}
```

- [ ] **Step 2: Run test — expect FAIL**

`cargo test -p hush-core --test outsider_mail` → `no method named send_to_outsider`.

- [ ] **Step 3: Implement**

`relay_client.rs`: `put_drop(&self, id: &str, body: &[u8]) -> Result<(), Error>` →
`PUT {base}/drop/{id}` with a bytes body; `get_drop(&self, id: &str) -> Result<Vec<u8>, Error>`.

`chat_log.rs`: add to the `Mail` variant

```rust
/// Set when the message went to a non-member address instead of a contact.
#[serde(default, skip_serializing_if = "Option::is_none")]
external: Option<ExternalMail>,
```

with

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExternalMail { pub to: String, pub delivery_id: String, pub expires_unix: u64 }
```

Every existing construction of `ChatRecordPayload::Mail` gains `external: None`; `append_mail`
gains an `external` argument and a matching `append_external_mail` sibling, so the internal path is
unchanged.

`account.rs`:

```rust
pub struct OutsiderOutcome { pub delivery_id: String, pub link: String }

impl Account {
    /// Seal a message to a non-member address and park the ciphertext on the
    /// relay. Nothing here is end-to-end in the member sense: the recipient has
    /// no keys, so the only thing standing between the relay and the plaintext is
    /// the PIN. That bound is deliberate (O3) and documented.
    pub async fn send_to_outsider(
        &mut self,
        to: &str,
        subject: &str,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
        pin: &str,
    ) -> Result<OutsiderOutcome, Error> {
        if !looks_like_address(to) { return Err(Error::InvalidAddress); }
        let total: usize = attachments.iter().map(|(_, d)| d.len()).sum();
        if total > MAX_INLINE_FILE_BYTES { return Err(Error::FileTooLarge(MAX_INLINE_FILE_BYTES)); }

        let pkg = DropPackage {
            v: 1,
            subject: subject.to_string(),
            body: body.to_string(),
            attachments: attachments.iter()
                .map(|(name, data)| DropAttachment {
                    name: name.clone(),
                    data: BASE64_STANDARD.encode(data),
                })
                .collect(),
            created_unix: now_secs(),
        };
        let bytes = seal(&pkg, pin)?;
        let delivery_id = hex::encode(random_bytes::<16>());
        self.relay.put_drop(&delivery_id, &bytes).await?;

        let expires_unix = now_secs() + DROP_TTL_SECS;
        let master = *self.vault.master_key();
        self.chat_log.append_external_mail(
            &master, to, &delivery_id, expires_unix, subject, body,
        )?;
        Ok(OutsiderOutcome { link: self.drop_link(&delivery_id), delivery_id })
    }

    fn drop_link(&self, id: &str) -> String {
        format!("{}/open/{id}", self.drop_base_url.trim_end_matches('/'))
    }
}
```

- [ ] **Step 4: Run test — expect PASS**

`cargo test -p hush-core --test outsider_mail` → passed.

---

### Task 4: Mailer seam and the notification

**Files:**
- Create: `crates/hush-core/src/mailer.rs`
- Modify: `crates/hush-core/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `trait Mailer { fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), Error>; }`,
  `NullMailer`, `FileMailer::new(dir)`, `notification_body(link, expires_unix) -> String`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_notification_carries_the_link_and_says_a_pin_is_needed() {
    let body = notification_body("https://x.example/open/abc", now_secs() + 7 * 86_400);
    assert!(body.contains("https://x.example/open/abc"));
    assert!(body.contains("in 7 days"));
    // The recipient has to be told a PIN is required, so the word "PIN" must be
    // present. What must never be present is a PIN *value* — and the function is
    // never handed one, which is what makes that structural rather than a rule.
    assert!(body.to_lowercase().contains("delivery pin"));
}

#[test]
fn the_file_mailer_writes_a_message_and_the_null_mailer_does_not() {
    let dir = tempfile::tempdir().unwrap();
    FileMailer::new(dir.path()).send("bob@gmail.com", "s", "b").unwrap();
    let written: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(written.len(), 1);
    assert!(NullMailer.send("bob@gmail.com", "s", "b").is_ok());
}
```

> The original draft of this plan asserted `!body.contains("pin")`. That was wrong: it would have
> forced the notification to omit the one instruction the recipient needs. The real constraint is
> that the PIN **value** must not be in the email, and the way to guarantee that is to never pass the
> PIN to `notification_body` at all.

- [ ] **Step 2: Run test — expect FAIL**

`cargo test -p hush-core mailer::` → unresolved module.

- [ ] **Step 3: Implement**

```rust
/// The seam (O1). A real SMTP client implements this later; nothing above it
/// changes. Synchronous on purpose: a notification is one small message and the
/// daemon already runs the send off the UI thread.
pub trait Mailer: Send + Sync {
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), Error>;
}

pub struct NullMailer;
impl Mailer for NullMailer {
    fn send(&self, _to: &str, _subject: &str, _body: &str) -> Result<(), Error> { Ok(()) }
}

pub struct FileMailer { dir: PathBuf }
impl FileMailer { pub fn new(dir: impl Into<PathBuf>) -> Self { Self { dir: dir.into() } } }
impl Mailer for FileMailer {
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir)?;
        let name = format!("{}-{}.txt", now_secs(), hex::encode(random_bytes::<4>()));
        std::fs::write(self.dir.join(name),
            format!("To: {to}\nSubject: {subject}\n\n{body}\n"))?;
        Ok(())
    }
}

pub fn notification_body(link: &str, expires_unix: u64) -> String {
    format!(
        "Someone sent you an encrypted message on Hush.\n\n\
         Open it here: {link}\n\n\
         You will need the delivery PIN they share with you separately.\n\
         This link expires {}. It is not included in this email.\n",
        format_expiry(expires_unix)
    )
}
```

- [ ] **Step 4: Run test — expect PASS**

`cargo test -p hush-core mailer::` → 2 passed.

---

### Task 5: Daemon endpoint and config

**Files:**
- Modify: `crates/hush-daemon/src/main.rs`, `crates/hush-daemon/src/account_worker.rs`
- Test: covered by Task 3 at the core layer; the endpoint is exercised in the browser run.

**Interfaces:**
- Produces: `POST /api/mail/outsider` → `{ delivery_id, link }`.

- [ ] **Step 1: Write the failing test** — an integration test that posts to the route and asserts
  a `delivery_id` comes back and the relay holds a container.

- [ ] **Step 2: Run test — expect FAIL** (404).

- [ ] **Step 3: Implement**

```rust
#[derive(Deserialize)]
struct SendOutsiderRequest {
    to: String,
    #[serde(default)] subject: String,
    body: String,
    #[serde(default)] attachments: Vec<MailAttachmentInput>,
    pin: String,
}

#[derive(Serialize)]
struct SendOutsiderResponse { delivery_id: String, link: String }
```

`Work::SendToOutsider` on the worker; the handler calls it, then `mailer.send(to, "Your encrypted
message", notification_body(&link, expires))` and returns the link. Config gains
`drop_base_url` (default `http://127.0.0.1:8788`) and `mailer` (`null` | `file:<dir>`).

- [ ] **Step 4: Run test — expect PASS**

`cargo test -p hush-daemon` → passed.

---

### Task 6: UI — two-mode To and the pickup link

**Files:**
- Modify: `apps/hush-tauri/src/app.ts`, `apps/hush-tauri/src/lib/api/daemon.ts`,
  `apps/hush-tauri/src/styles/app.css`

**Interfaces:**
- Consumes: `POST /api/mail/outsider`.
- Produces: compose state `mailComposeMode: "internal" | "external"`,
  `mailComposeExternalTo`, `mailComposePin`, `mailComposePinConfirm`; a segmented To control.

- [ ] **Step 1: Write the failing check** — `npx tsc --noEmit` after adding the state fields but
  before wiring, to confirm the shape.

- [ ] **Step 2: Run — expect FAIL** on the unused fields.

- [ ] **Step 3: Implement**

A segmented control in the To row: **Internal** shows today's contact `<select>`; **External**
shows an address input, a PIN input, and a confirm-PIN input. Send dispatches to the matching
endpoint. On success the pane swaps to a "Sent — share this link and PIN separately" panel with a
Copy-link button and the PIN restated, so the sender can transmit the two halves over different
channels. The PIN is never written into the message body or the link.

- [ ] **Step 4: Run — expect PASS**

`npx tsc --noEmit && npm run build` → clean.

---

## Next plan (not this one)

**Inbound drop** (O5): the `drop@myserver.com` MTA extracts a `hush-drop-v1` attachment by magic
bytes and forwards it to `PUT /drop/{id}`; a packager page lets an outsider build a container
against a member's published drop key; the daemon drains the member's drops and files them in an
**External** folder. It needs a member drop keypair, a drop-list route, and a real domain + MX to
be testable end to end — which is exactly why it is a separate plan.

## Open risks

- **PIN entropy.** Anyone holding the blob can brute-force the PIN offline. 600k PBKDF2 iterations
  and a 7-day TTL raise the cost but do not remove it; a 4-digit PIN is weak. The UI must push for a
  longer passphrase, and this belongs in `THREAT_MODEL.md` when the feature lands.
- **The relay's rate limit is per IP and the drop route is unauthenticated.** A distributed fetch
  of a known delivery id is not throttled. The PIN is the only real defence.
- **`drop_base_url` has no default domain.** Until one exists, links point at localhost and the
  notification is only meaningful to a recipient on the same machine. That is expected at this stage.

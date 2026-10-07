# Multi-device (per-device keys) Implementation Plan

**Goal:** Give each device its own identity key and relay queue under a shared account id, so a
linked device can be listed and revoked honestly.

**Architecture:** `account_id` stays equal to device 1's identity public key hash, which is exactly
today's `protocol_name`, so existing invites and contacts are unaffected. Each device owns its own
identity key pair, prekeys, sessions and relay queue; peers encrypt to every device in the account's
signed device list, which is published to the relay and cached by peers with a TTL. Device-list
writes are authorized by device 1's signature on the blob, verified by the relay against the account
id, so a revoked device cannot re-publish itself.

**Tech Stack:** Rust (`hush-core`, `hush-relay`, `hush-daemon`), `libsignal-protocol`, axum, SQLite
(relay), TypeScript + Vite (UI).

**Spec:** this conversation. Decisions Q1–Q10 recorded under "Settled design" below.

## Global Constraints

- Never commit to `master`; this phase lands on a feature branch.
- No plaintext of message/file content on disk, ever. Device key material is encrypted at rest with
  the vault master key, like everything else.
- The relay stays content-blind: it stores opaque blobs and must never need to parse a device list.
- `protocol_name` must remain valid for existing contacts. Existing vaults migrate silently on
  unlock and must not require any user action.
- libsignal provides no device-list primitive. `ProtocolAddress(name, device_id)` is all there is.
- Every task ends with a runnable test. No task may leave the tree unable to build.

## Settled design

| # | Decision |
|---|---|
| Q1 | Per-device identity keys + stable account id |
| Q2 | Signed device-list blob published to the relay, cached by peers |
| Q3 | One relay queue per device |
| Q4 | Linking transfers history **minus** device key material |
| Q5 | Peers refresh the device list on a ~5 min TTL, and on a send failure |
| Q6 | Sent messages fan out a copy to the sender's own other devices |
| Q7 | `account_id` = device 1's identity public key hash (= today's `protocol_name`) |
| Q8 | No device cap |
| Q9 | ~~Device-list writes require a shared account token, rotated on every revocation~~ **Superseded 2026-10-07: writes are authorized by device 1's signature on the blob. The token was deleted — see Phase 3.** |
| Q10 | Existing vaults migrate silently on unlock; the device list is published lazily |
| Q11 | Device 1 is the device-list **signing root**: it must be online and unlocked to add or revoke, and cannot itself be revoked |

### Known limitations to document in the UI, not hide

- Revoking a device stops it receiving **new** messages and stops it impersonating you (its identity
  key is its own), but it **cannot erase history that device already holds**.
- Up to ~5 minutes after a revocation a peer may still encrypt to the revoked device.
- Messages already queued for a revoked device are destroyed when its queue is burned.

## File map

| File | Responsibility |
|---|---|
| `crates/hush-core/src/device.rs` *(new)* | `DeviceKeys`, this device's identity/prekeys, device id, queue |
| `crates/hush-core/src/device_list.rs` *(new)* | `DeviceList`, `DeviceRecord`, sign/verify, account token |
| `crates/hush-core/src/vault.rs` | Per-device material stored separately from shared vault data |
| `crates/hush-core/src/backup.rs` | Exclude per-device material from export |
| `crates/hush-core/src/account.rs` | `Contact` as a device set; send/recv fan-out; migration |
| `crates/hush-core/src/signal.rs` | `protocol_address` takes a real `device_id` |
| `crates/hush-core/src/relay_client.rs` | Device-directory get/put; queue burn |
| `crates/hush-relay/src/app.rs` | `GET`/`PUT /d/{account_id}` |
| `crates/hush-relay/src/store.rs` | Device-directory table + account-token check |
| `crates/hush-daemon/src/main.rs` | Device list / revoke / link endpoints |
| `apps/hush-tauri/src/app.ts` | Settings → Devices UI |

---

## Phase 1 — Per-device key material

Splits the material that must never be copied to another device out of the shared vault.

> **Status: done 2026-10-07** on `feat/multi-device-phase1`. Deviations from the plan as written:
>
> - The `DeviceKeys` tests live in-crate (`src/device.rs`), not in `tests/device_identity.rs`:
>   the plan called `Vault::master_key()`, which is `pub(crate)` and unreachable from an
>   integration test.
> - The legacy migration (`signal.enc` -> `device.enc`, planned as Task 10 in Phase 5) landed
>   here instead. Phase 1 stops writing `signal.enc`, so without migration an existing vault
>   would have neither file and every `Account::open` would fail.
> - Restoring a backup now mints a fresh device identity. That is required once `device.enc` is
>   excluded from backups, and it means a restored device has a different identity key from the
>   one contacts have pinned. Not yet surfaced in the UI.
> - `identity.secret` is still included in backups. It is unused dead weight today, but needs a
>   decision before it is adopted as an account signing key. See `DEEPSEEKPROBLEMS.MD`.
>
> Evidence: `cargo test --workspace` (all suites, 0 failures) and the smoke scripts, with
> `smoke-change-passphrase.sh` updated to fingerprint `device.enc` rather than `signal.enc`.

### Task 1: Separate this device's identity from the shared vault

**Files:**
- Create: `crates/hush-core/src/device.rs`
- Modify: `crates/hush-core/src/vault.rs`, `crates/hush-core/src/lib.rs`
- Test: `crates/hush-core/tests/device_identity.rs`

**Interfaces:**
- Produces: `DeviceKeys { device_id: DeviceId, store: InMemSignalProtocolStore, queue_id: String, queue_token: String }`, `DeviceKeys::load(dir, master)`, `DeviceKeys::create(dir, master, device_id)`
- Consumes: `crypto::{encrypt, decrypt}`, `signal::new_protocol_store`

- [ ] **Step 1: Write the failing test**

```rust
// crates/hush-core/tests/device_identity.rs
use hush_core::Vault;
use tempfile::TempDir;

#[test]
fn device_identity_is_not_in_the_shared_vault() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::create(dir.path(), "pw").unwrap();
    let device = hush_core::DeviceKeys::create(dir.path(), vault.master_key(), 1).unwrap();

    // The device identity must live outside identity.secret, which is shared.
    assert!(dir.path().join("device.enc").exists());

    let reloaded = hush_core::DeviceKeys::load(dir.path(), vault.master_key()).unwrap();
    assert_eq!(reloaded.device_id(), device.device_id());
    assert_eq!(
        reloaded.identity_public(),
        device.identity_public(),
        "the device identity must survive a reload"
    );
}

#[test]
fn device_identity_survives_a_passphrase_change() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::create(dir.path(), "pw").unwrap();
    let device = hush_core::DeviceKeys::create(dir.path(), vault.master_key(), 1).unwrap();
    let before = device.identity_public();

    vault.change_passphrase("pw", "pw2").unwrap();

    let reopened = Vault::open(dir.path(), "pw2").unwrap();
    let reloaded = hush_core::DeviceKeys::load(dir.path(), reopened.master_key()).unwrap();
    assert_eq!(reloaded.identity_public(), before);
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test device_identity`
Expected: compile error, `DeviceKeys` not found.

- [ ] **Step 3: Minimal implementation**

```rust
// crates/hush-core/src/device.rs
use std::path::Path;

use libsignal_protocol::InMemSignalProtocolStore;
use zeroize::Zeroizing;

use crate::crypto::{decrypt, encrypt, MASTER_KEY_LEN};
use crate::error::Error;

const DEVICE_FILE: &str = "device.enc";

/// Material belonging to *this* device only.
///
/// This must never be copied to another device. A linked device that received
/// these bytes would hold our identity private key and could impersonate us
/// forever, which is exactly what revocation has to prevent.
pub struct DeviceKeys {
    device_id: u32,
    store: InMemSignalProtocolStore,
    queue_id: String,
    queue_token: String,
}

impl DeviceKeys {
    pub fn load(dir: &Path, master: &[u8; MASTER_KEY_LEN]) -> Result<Self, Error> {
        let packed = std::fs::read(dir.join(DEVICE_FILE)).map_err(|_| Error::NotFound)?;
        let plain = decrypt(master, &packed)?;
        let dto: DeviceKeysDto = serde_json::from_slice(&plain).map_err(|_| Error::Corrupt)?;
        let store = crate::signal_persist::import_store(&dto.signal).await?; // see note
        Ok(Self { device_id: dto.device_id, store, queue_id: dto.queue_id, queue_token: dto.queue_token })
    }
    // ...
}
```

Note: `DeviceKeys::load` must be async to reuse `signal_persist::import_store`. Make it
`pub async fn load`. `create` likewise.

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test device_identity`

- [ ] **Step 5: Confirm the shared vault is unaffected**

Run: `cargo test -p hush-core`
Expected: all existing tests still pass — `identity.secret` still exists and is still what
`Vault::open` uses.

---

### Task 2: Keep per-device material out of backups

**Files:**
- Modify: `crates/hush-core/src/backup.rs`
- Test: `crates/hush-core/tests/backup_recovery.rs`

**Interfaces:**
- Consumes: `DeviceKeys` file name from Task 1
- Produces: `export_vault` excluding per-device files; `import_vault` unaffected

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn backup_excludes_device_key_material() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::create(dir.path(), "pw").unwrap();
    hush_core::DeviceKeys::create(dir.path(), vault.master_key(), 1).unwrap();
    assert!(dir.path().join("device.enc").exists());

    let archive = export_vault(dir.path()).unwrap();
    let mut tar = tar::Archive::new(&archive[..]);
    let names: Vec<String> = tar
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(
        !names.iter().any(|n| n.contains("device.enc")),
        "backup must not carry this device's private identity: {names:?}"
    );
    assert!(names.iter().any(|n| n.contains("chat.enc")), "history must still be included");
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test backup_recovery backup_excludes`
Expected: FAIL — `device.enc` is present in the archive.

- [ ] **Step 3: Minimal implementation**

Add an exclusion set to `walk_vault_files`' caller in `export_vault`:

```rust
/// Per-device material is deliberately not part of a backup: a backup is the
/// thing we hand to another device, and handing over our identity private key
/// would make that device impossible to revoke.
const DEVICE_LOCAL_FILES: &[&str] = &["device.enc"];

fn is_device_local(rel: &str) -> bool {
    DEVICE_LOCAL_FILES.iter().any(|f| rel.ends_with(f))
}
```

Skip those entries in the `for entry in walk_vault_files(dir)?` loop.

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test backup_recovery`

---

## Phase 2 — Device list and account token

> **Status: done 2026-10-07** on `feat/multi-device-phase1`. Deviations from the plan as written:
>
> - The signature is taken from the **tail of the blob by its fixed length** (XEd25519 signatures are
>   always 64 bytes) rather than found by scanning for a newline delimiter. A delimiter that could
>   appear inside the payload is a parsing bug waiting to happen, and the length is known.
> - `verify` also rejects an unknown `v` and a blob with no payload, and `AccountToken` gets a
>   hand-written `Debug` that redacts the secret.
> - The plan's tamper test asserted on the JSON parser rather than the signature, so it passed even
>   with `verify_signature` removed. Rewritten to tamper inside a string value so the payload stays
>   valid JSON. Confirmed by mutation: both signature tests now fail when the check is disabled.
> - `subtle = "2"` added to `crates/hush-core/Cargo.toml`; it was only present transitively before.
>
> Evidence: `cargo test --workspace` (65 tests, 0 failures), including a wrong-key test proving a list
> signed by anything other than the account root is rejected.

### Task 3: `DeviceList` with signing and verification

**Files:**
- Create: `crates/hush-core/src/device_list.rs`
- Modify: `crates/hush-core/src/lib.rs`
- Test: `crates/hush-core/tests/device_list.rs`

**Interfaces:**
- Produces: `DeviceRecord`, `DeviceList { v, account_id, devices, token_epoch, updated_unix }`, `DeviceList::sign(&IdentityKeyPair) -> Vec<u8>`, `DeviceList::verify(&[u8], expected_account_id) -> Result<DeviceList>`, `DeviceList::without(device_id)`, `DeviceList::add(DeviceRecord)`

> **Corrected 2026-10-07.** This task originally signed with `ed25519_dalek::SigningKey`. The device
> identity is a **libsignal Curve25519** key, which cannot be passed to `ed25519_dalek` at all, so the
> original would not have compiled. Signing goes through
> `PrivateKey::calculate_signature(msg, csprng)` (XEd25519, the same primitive
> `mint_prekey_bundle` uses for signed prekeys) and verification through
> `PublicKey::verify_signature(msg, sig) -> bool`.
>
> **Resolved 2026-10-07 — device 1 is the signing root (Q11).** The list is signed with device 1's
> libsignal identity private key and verified against the account id, which *is* device 1's identity
> key hash (Q7). That means no new pinned trust material: a peer verifies with the one thing it
> already has. Consequences to hold onto:
>
> - Device 1 must be **online and unlocked** to add or revoke a device. It holds the only signing key.
> - Device 1 **cannot be revoked**; it is the root. Losing it means rotating the account and
>   re-inviting every contact — the Q4 alternative, applied to the one case that genuinely needs it.
> - A linked device cannot sign, because device 1's private key is never transferred. Phase 1
>   guarantees exactly that by keeping `device.enc` out of backups.
> - The account token (Q9) is still needed, but for a different job: it gates *who may write* to the
>   relay, so a revoked device cannot overwrite the list with garbage. The signature proves
>   authenticity to peers; the token protects availability. They are independent questions, which is
>   what the original design conflated.
> - `identity.secret` is therefore **not** the account signing key and remains unused dead code. That
>   is still an open cleanup decision, tracked in `DEEPSEEKPROBLEMS.MD`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/hush-core/tests/device_list.rs
use libsignal_protocol::IdentityKeyPair;
use rand::rngs::OsRng;
use rand::TryRngCore;

use hush_core::device_list::{DeviceList, DeviceRecord};

fn account_key() -> (IdentityKeyPair, String) {
    let mut csprng = OsRng.unwrap_err();
    let pair = IdentityKeyPair::generate(&mut csprng);
    let id = hex::encode(pair.identity_key().serialize());
    (pair, id)
}

fn record(device_id: u32, seed: u8) -> DeviceRecord {
    DeviceRecord {
        device_id,
        identity_key: vec![seed; 33],
        registration_id: device_id,
        queue_id: format!("queue-{device_id}"),
        queue_token: format!("token-{device_id}"),
        added_unix: 1_700_000_000,
    }
}

#[test]
fn signed_list_verifies_and_detects_tampering() {
    let (key, account_id) = account_key();
    let list = DeviceList::new(account_id.clone(), vec![record(1, 1), record(2, 2)]);
    let signed = list.sign(&key).unwrap();

    let parsed = DeviceList::verify(&signed, &account_id).unwrap();
    assert_eq!(parsed.devices.len(), 2);

    // Flip a byte in the payload; verification must fail rather than trust it.
    let mut tampered = signed.clone();
    let idx = tampered.len() / 2;
    tampered[idx] ^= 0x01;
    assert!(DeviceList::verify(&tampered, &account_id).is_err());
}

#[test]
fn removing_a_device_drops_it_and_bumps_the_epoch() {
    let (key, account_id) = account_key();
    let _ = key;
    let list = DeviceList::new(account_id, vec![record(1, 1), record(2, 2)]);

    let after = list.without(2);
    assert_eq!(after.devices.len(), 1);
    assert_eq!(after.devices[0].device_id, 1);
    assert!(after.token_epoch > list.token_epoch, "revocation must rotate the token epoch");
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test device_list`
Expected: compile error, module not found.

- [ ] **Step 3: Minimal implementation**

```rust
// crates/hush-core/src/device_list.rs
use libsignal_protocol::{IdentityKey, IdentityKeyPair};
use rand::rngs::OsRng;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const DEVICE_LIST_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceRecord {
    pub device_id: u32,
    pub identity_key: Vec<u8>,
    pub registration_id: u32,
    pub queue_id: String,
    pub queue_token: String,
    pub added_unix: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceList {
    pub v: u32,
    pub account_id: String,
    pub devices: Vec<DeviceRecord>,
    /// Bumped on every revocation. The account token is rotated whenever this
    /// moves, which is what stops a revoked device re-publishing itself.
    pub token_epoch: u64,
    pub updated_unix: u64,
}

impl DeviceList {
    pub fn new(account_id: String, devices: Vec<DeviceRecord>) -> Self {
        Self { v: DEVICE_LIST_VERSION, account_id, devices, token_epoch: 0, updated_unix: now_unix() }
    }

    pub fn without(&self, device_id: u32) -> Self {
        let mut next = self.clone();
        next.devices.retain(|d| d.device_id != device_id);
        next.token_epoch += 1;
        next.updated_unix = now_unix();
        next
    }

    /// Payload, then signature, separated by a newline.
    ///
    /// Signed with the account key's libsignal private key (XEd25519) — the same
    /// primitive the signed prekeys use. The signature is what lets a peer
    /// detect a relay that swapped the list out from under it.
    pub fn sign(&self, key: &IdentityKeyPair) -> Result<Vec<u8>, Error> {
        let payload = serde_json::to_vec(self).map_err(|_| Error::Corrupt)?;
        let mut csprng = OsRng.unwrap_err();
        let sig = key
            .private_key()
            .calculate_signature(&payload, &mut csprng)
            .map_err(|_| Error::Corrupt)?;
        let mut out = payload;
        out.push(b'\n');
        out.extend_from_slice(&sig);
        Ok(out)
    }

    pub fn verify(signed: &[u8], expected_account_id: &str) -> Result<Self, Error> {
        let split = signed.iter().rposition(|b| *b == b'\n').ok_or(Error::Corrupt)?;
        let (payload, sig) = signed.split_at(split);
        let list: Self = serde_json::from_slice(payload).map_err(|_| Error::Corrupt)?;
        if list.account_id != expected_account_id {
            return Err(Error::Corrupt);
        }
        let key_bytes = hex::decode(expected_account_id).map_err(|_| Error::Corrupt)?;
        let key = IdentityKey::try_from(key_bytes.as_slice()).map_err(|_| Error::Corrupt)?;
        if !key.public_key().verify_signature(payload, &sig[1..]) {
            return Err(Error::Corrupt);
        }
        Ok(list)
    }
}
```

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test device_list`

---

### Task 4: Account token, rotated on revocation

> **Dropped 2026-10-07 — this task was not implemented, and its machinery was deleted.** The
> token could not do the job it was written for. Rotating it to revoke a device cannot work under a
> trust-on-first-use rule: the relay stores the *old* token's hash, so the account's very next publish
> with the rotated token is rejected `401`. `rotating_the_token_currently_breaks_publishing` proved
> this — the account could not perform the rotation that revokes anyone. Even patched, the window
> between rotating locally and the relay learning the new hash leaves the revoked device holding a
> working credential.
>
> Authorization now comes from the same signature that gives the list its authenticity. The relay
> verifies device 1's signature on the body against the account id, and a revoked device does not hold
> device 1's private key, so it cannot produce an acceptable body. No credential to rotate means no
> rotation race and no window. `AccountToken`, `DeviceList::token` and `DeviceList::token_epoch` were
> deleted; `epoch` survives as a peer-visible freshness counter so a peer holding a cached list can
> tell that revocation happened.
>
> The steps below are kept for the record and should not be executed.

**Files:**
- Modify: `crates/hush-core/src/device_list.rs`
- Test: `crates/hush-core/tests/device_list.rs`

**Interfaces:**
- Produces: `AccountToken::generate()`, `AccountToken::rotate(&self) -> AccountToken`, `AccountToken::expose() -> &str`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rotating_the_token_invalidates_the_old_one() {
    let old = hush_core::device_list::AccountToken::generate();
    let new = old.rotate();
    assert_ne!(old.expose(), new.expose());
    assert!(old.matches(old.expose()), "the old token still matches itself");
    assert!(!old.matches(new.expose()), "a rotated token must not accept the previous value");
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test device_list rotating`

- [ ] **Step 3: Minimal implementation**

```rust
/// Shared secret authorizing device-list writes for one account.
///
/// Rotated on every revocation: a revoked device keeps a copy of the old token,
/// so leaving it valid would let that device re-add itself to the list.
pub struct AccountToken(String);

impl AccountToken {
    pub fn generate() -> Self {
        Self(hex::encode(crate::crypto::random_bytes::<32>()))
    }
    pub fn rotate(&self) -> Self {
        Self::generate()
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn matches(&self, candidate: &str) -> bool {
        use subtle::ConstantTimeEq;
        self.0.as_bytes().ct_eq(candidate.as_bytes()).into()
    }
}
```

Add `subtle` to `crates/hush-core/Cargo.toml` if not already present (check with
`cargo tree -p hush-core | rg subtle`).

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test device_list`

---

## Phase 3 — Relay device directory

> **Status: done 2026-10-07** on `feat/multi-device-phase1`. Deviations from the plan as written:
>
> - **Task 5's auth model was replaced.** The route is no longer token-authenticated. `PUT` is
>   authorized by verifying device 1's signature on the body against the account id
>   (`hush_core::DeviceList::verify(body, account_id)`), and `GET` is deliberately unauthenticated
>   because a peer fetching a contact's list holds no credential for that account and does not need
>   one — the signature is what makes the contents trustworthy. See Task 4's note for why the token
>   could not work.
> - The relay therefore does parse enough of the blob to check the signature, which narrows the
>   "relay stays content-blind" constraint: it must know the device-list format to verify it. It still
>   does not *trust* the contents — a peer verifies the same signature independently — so a relay that
>   stored something else would have it rejected on the far side.
> - `device_dirs.token_hash` does not exist; the column was removed before the table was ever
>   committed, so there is no migration to write.
> - Tests: 7 in `crates/hush-relay/tests/device_directory.rs`, including the revocation path
>   (a smaller list publishes with no rotation step) and the negative case that a list signed by any
>   key other than device 1's is rejected `401` and stores nothing.

### Task 5: `GET`/`PUT /d/{account_id}` with account-token auth

> **Corrected 2026-10-07 — auth is signature-based, not token-based.** `PUT` carries no bearer
> credential; the relay rejects the write with `401` unless the body carries a valid device-1
> signature over the payload for that `account_id`. `GET` requires no auth at all. The steps below
> still describe the token model and are kept only for the record.

**Files:**
- Modify: `crates/hush-relay/src/app.rs`, `crates/hush-relay/src/store.rs`
- Test: `crates/hush-relay/tests/device_directory.rs`

**Interfaces:**
- Produces: `PUT /d/{account_id}` (bearer account token, stores opaque bytes), `GET /d/{account_id}` (returns the stored bytes, 404 when absent)

- [ ] **Step 1: Write the failing test**

```rust
// crates/hush-relay/tests/device_directory.rs
#[tokio::test]
async fn device_directory_round_trips_and_requires_the_token() {
    // Build the router the same way crates/hush-relay/tests/relay_queues.rs does,
    // then:
    // 1. PUT /d/acct with bearer "tok" and body b"signed" -> 204
    // 2. GET /d/acct -> 200 and body b"signed"
    // 3. PUT /d/acct with bearer "wrong" -> 401 and the stored body is unchanged
    // 4. GET /d/missing -> 404
}
```

Follow the existing setup in `crates/hush-relay/tests/relay_queues.rs` for spawning the app.

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-relay --test device_directory`
Expected: 404/405 — route does not exist.

- [ ] **Step 3: Minimal implementation**

```rust
// crates/hush-relay/src/app.rs, in router()
.route("/d/{account_id}", get(get_device_dir).put(put_device_dir))

// Handlers: the relay never parses the blob. It checks the account token the
// first time a directory is written, then requires the same token for updates.
async fn put_device_dir(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, RelayError> {
    let token = bearer_token(&headers)?;
    state.store.put_device_dir(&account_id, token, &body)?;
    Ok(StatusCode::NO_CONTENT)
}
```

`store.put_device_dir` rejects a mismatched token with 401 once a row exists. Add a
`device_dirs(account_id PRIMARY KEY, token, blob, updated_unix)` table.

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-relay --test device_directory`

---

### Task 6: Burn a revoked device's queue

> **Status: done 2026-10-07.** `RelayClient::report_queue` added as a thin wrapper over the existing
> `POST /q/{queue_id}/report`, mirroring `RelayClient::delete`. The relay side already existed and was
> already tested in `crates/hush-relay/tests/relay_queues.rs`.
>
> The test landed in `crates/hush-core/tests/two_party_relay.rs` as `reporting_a_queue_burns_it`, and
> covers slightly more than the plan asked: after the burn, `list` fails, a second `report` fails, and
> a peer's `send_text` fails — so nothing further can be delivered into the burned inbox, not just
> read out of it.

**Files:**
- Modify: `crates/hush-core/src/relay_client.rs`
- Test: `crates/hush-core/tests/two_party_relay.rs`

**Interfaces:**
- Consumes: existing `POST /q/{queue_id}/report`
- Produces: `RelayClient::report_queue(queue_id, token)`

- [ ] **Step 1: Write the failing test** — assert that after `report_queue`, `list()` on that queue
  returns empty and a subsequent `put` is rejected.
- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test two_party_relay burn`

- [ ] **Step 3: Minimal implementation** — thin wrapper over the existing relay route, mirroring
  `RelayClient::delete`.
- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test two_party_relay`

---

## Phase 4 — Contacts as device sets, and send/recv fan-out

### Task 7: `Contact` holds a device set

**Files:**
- Modify: `crates/hush-core/src/account.rs`
- Test: `crates/hush-core/tests/multi_device.rs` *(new)*

**Interfaces:**
- Produces: `ContactDevice { device_id, identity_key, registration_id, queue_id, queue_token, pre_key_* }`, `Contact { nickname, account_id, devices: Vec<ContactDevice>, inbound_ready }`
- Migration: `#[serde(alias = "protocol_name")] account_id`, and a `#[serde(default)]` upgrade that
  turns a legacy single-queue contact into a one-device set.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_legacy_contact_deserializes_into_a_single_device_set() {
    let legacy = r#"{
        "nickname":"bob",
        "protocol_name":"ab12",
        "queue_id":"q1",
        "queue_token":"t1",
        "inbound_ready":true
    }"#;
    let contact: hush_core::Contact = serde_json::from_str(legacy).unwrap();
    assert_eq!(contact.account_id, "ab12");
    assert_eq!(contact.devices.len(), 1);
    assert_eq!(contact.devices[0].queue_id, "q1");
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test multi_device legacy_contact`

- [ ] **Step 3: Minimal implementation** — the new structs plus a `Deserialize` shim that detects
  the flat legacy shape (presence of `queue_id`) and folds it into one `ContactDevice`.
- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test multi_device`

---

### Task 8: Send to every device of a contact, and fan out to our own

**Files:**
- Modify: `crates/hush-core/src/account.rs`, `crates/hush-core/src/signal.rs`
- Test: `crates/hush-core/tests/multi_device.rs`

**Interfaces:**
- Consumes: `Contact.devices`, `DeviceKeys.device_id`
- Produces: `Account::send_text` encrypting once per recipient device and `PUT`ting to each queue; an additional copy to each of our own other devices' queues

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_message_reaches_every_device_of_the_recipient_and_our_own() {
    // Three accounts: alice (device 1 + device 2), bob (device 1).
    // alice-device-1 sends "hi".
    // Expect: bob-device-1 receives it, AND alice-device-2 receives a copy.
}
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test multi_device fan_out`

- [ ] **Step 3: Minimal implementation** — loop over `contact.devices`, calling
  `encrypt_message(store, local_name, remote_name, device_id, plaintext)`; then loop over
  `self_devices` excluding our own `device_id`. Change `protocol_address` to accept a real
  `device_id` instead of the hardcoded `device_id()`.

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test multi_device`

---

## Phase 5 — Linking and migration

### Task 9: A new device generates its own keys

**Files:**
- Modify: `crates/hush-daemon/src/link_device.rs`, `crates/hush-core/src/account.rs`
- Test: `crates/hush-core/tests/multi_device.rs`

**Interfaces:**
- Consumes: `export_vault` minus device material (Task 2)
- Produces: `Account::adopt_linked_vault(dir, passphrase)` — imports history, then generates a new
  `DeviceKeys` with the next free `device_id` and publishes an updated device list

- [ ] **Step 1: Write the failing test** — link a vault to a second directory, assert the second
  device's `identity_public()` differs from the first's, and that it can still read the chat history.
- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p hush-core --test multi_device new_device_keys`

- [ ] **Step 3: Minimal implementation**
- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test multi_device`

---

### Task 10: Silent migration on unlock

**Files:**
- Modify: `crates/hush-core/src/account.rs`
- Test: `crates/hush-core/tests/multi_device.rs`

**Interfaces:**
- Produces: on `finish_open`, if `device.enc` is absent, create `DeviceKeys` with `device_id = 1`
  from the existing identity material, set `account_id = protocol_name`, and store the account token.
  No device list is published until a second device links (Q10).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_pre_multi_device_vault_migrates_on_unlock_without_changing_its_name() {
    // Build a vault with the pre-migration layout (identity.secret + signal.enc,
    // no device.enc), open it, and assert:
    //  - account_id == the old protocol_name
    //  - device.enc now exists
    //  - contacts and chat history are unchanged
}
```

- [ ] **Step 2: Run test — expect FAIL**
- [ ] **Step 3: Minimal implementation**
- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test multi_device`

---

### Task 11: Device-list fetch, cache and refresh

**Files:**
- Modify: `crates/hush-core/src/account.rs`, `crates/hush-core/src/relay_client.rs`
- Test: `crates/hush-core/tests/multi_device.rs`

**Interfaces:**
- Produces: `RelayClient::get_device_dir(account_id)`, `put_device_dir(account_id, token, bytes)`; a
  ~5 min TTL cache on `Contact.devices` with re-fetch when a send fails (Q5)

- [ ] **Step 1: Write the failing test** — serve a device list, send, then serve a list with the
  device removed and assert the next send after TTL expiry skips it.
- [ ] **Step 2: Run test — expect FAIL**
- [ ] **Step 3: Minimal implementation**
- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test multi_device`

---

## Phase 6 — Revocation, daemon API and UI

### Task 12: `Account::revoke_device`

**Files:**
- Modify: `crates/hush-core/src/account.rs`
- Test: `crates/hush-core/tests/multi_device.rs`

**Interfaces:**
- Produces: `Account::revoke_device(device_id) -> Result<()>`: reject `device_id == self.device_id`;
  build `list.without(device_id)`; rotate the account token; publish the new list; burn the revoked
  device's queue.

- [ ] **Step 1: Write the failing test** — revoke device 2, assert the published list omits it, the
  token epoch advanced, the old token no longer authorizes a `PUT`, and device 2's queue is empty.
- [ ] **Step 2: Run test — expect FAIL**
- [ ] **Step 3: Minimal implementation**
- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p hush-core --test multi_device revoke`

---

### Task 13: Daemon endpoints

**Files:**
- Modify: `crates/hush-daemon/src/main.rs`, `crates/hush-daemon/src/account_worker.rs`
- Test: `scripts/smoke-devices.sh` *(new)*

**Interfaces:**
- Produces: `GET /api/devices`, `POST /api/devices/revoke { device_id }`, and a real `POST /api/link-device/complete` so the source records the new device

- [ ] **Step 1: Write the failing smoke check** — unlock, `GET /api/devices` returns exactly one
  entry with `"is_this_device":true`; revoking it returns 400.
- [ ] **Step 2: Run — expect FAIL**

Run: `./scripts/smoke-devices.sh`

- [ ] **Step 3: Minimal implementation** — worker variants plus routes, following the
  `change_passphrase` handler pattern (401 when locked, typed `WorkResult` for refusals).
- [ ] **Step 4: Run — expect PASS**

Run: `./scripts/smoke-devices.sh`

---

### Task 14: Settings → Devices UI

**Files:**
- Modify: `apps/hush-tauri/src/app.ts`, `apps/hush-tauri/src/lib/api/daemon.ts`, `apps/hush-tauri/src/lib/api/types.ts`

**Interfaces:**
- Consumes: `GET /api/devices`, `POST /api/devices/revoke`

- [ ] **Step 1: Implement the list** — each row shows device id, added date and a "this device"
  badge; "Revoke" opens a confirmation naming the limitation that the device keeps history it
  already has.
- [ ] **Step 2: Typecheck and build**

Run: `cd apps/hush-tauri && npx tsc --noEmit && npm run build`

- [ ] **Step 3: Verify in a browser** — unlock, revoke a linked device, confirm the list updates and
  the warning text is present. Capture a screenshot.

---

## Phase 7 — Docs and close-out

### Task 15: Update protocol and gap docs

- [ ] `docs/PROTOCOL.md` — replace the "one-time transfer, not a live second device" note with the
  per-device model, the device directory, and the revocation window.
- [ ] `apps/hush-tauri/API_GAPS.md` — remove the trusted-devices gap.
- [ ] `docs/plans/2026-10-04-v1-milestones.md` — record the phase as done, with the residual
  limitations.
- [ ] `DEEPSEEKPROBLEMS.MD` — log anything that cost a round trip during implementation.

---

## Acceptance criteria

- [ ] Two devices under one account; a message from a third party reaches **both**.
- [ ] A message sent from device 1 appears on device 2 (fan-out, Q6).
- [ ] A new device links, receives history, and has a **different** identity key from device 1.
- [ ] Revoking device 2 removes it from the published list, rotates the token, and burns its queue.
- [ ] A revoked device can no longer receive new messages, and its stale token cannot re-publish the list.
- [ ] A pre-multi-device vault opens unchanged, keeps its `protocol_name`, and migrates silently.
- [ ] `cargo test --workspace` passes; `./scripts/smoke-change-passphrase.sh` and
  `./scripts/smoke-daemon-socket.sh` still pass.

## Self-review notes

- Phase 1–2 tasks carry full code because they are next and they change on-disk format. Phases 3–7
  have concrete steps, exact paths and commands, but their code blocks will be expanded to the same
  depth when each phase starts, since later phases depend on the exact shape the earlier ones land
  with. This is a deliberate choice, not a placeholder.
- `DeviceKeys::load`/`create` must be `async`; Task 1's test is written as `#[test]` and should be
  `#[tokio::test]`.
- Task 4 depends on `subtle`. It is present in the tree only as a **transitive** dependency
  (`v2.6.1`), so `subtle = "2"` must be added to `[dependencies]` in `crates/hush-core/Cargo.toml`
  before the constant-time comparison will compile.
- Verified: `ed25519-dalek 2.2.0` re-exports `Signer`/`Verifier`, `SigningKey: Signer<Signature>`
  and `VerifyingKey: Verifier<Signature>`, so the imports in Task 3 compile as written.

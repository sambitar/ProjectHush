# Incoming Drops Implementation Plan

> **Scope note (2026-10-09).** This plan rebuilds the *inbound* half of the removed
> outsider-mail slice (`docs/plans/2026-10-07-outsider-mail.md`), with one decision
> reversed: the drop key — not a vanishing link — is the address. The dropper needs no
> prior relationship and no shared secret. Hardening (Flatpak packaging, systemd unit
> sandboxing, `bwrap` wrappers, Tails/Qubes support) is **explicitly out of scope**,
> deferred to a later decision.

**Goal:** A member can publish a drop key; anyone with the key and the relay's drop page
(onion) can seal an anonymous drop to it; the member's daemon drains the ciphertext and
files it into a quarantined **Drops** folder in Mail.

**Architecture:** The member's vault holds a dedicated X25519 drop keypair, separate from
the Signal identity key. A single static page served by the relay (`GET /drop`, embedded
assets) seals the message client-side with pure-JS crypto and uploads the container with
`PUT /drop/{id}`. The delivery id is a rendezvous: `HMAC-SHA256(recipient_pk,
"hush-drop-rendezvous-v1")` truncated to 16 bytes — both the page and the member's daemon
derive it from the drop public key alone, so the relay keeps no directory and links
nothing to anyone. The daemon drains `GET /drop/{id}`, opens the sealed box, stores
attachments as encrypted vault blobs, appends a `ChatRecordPayload::Drop` record to the
`chat.enc` log, then `DELETE /drop/{id}`. There is no sender key, no signature, and no
reply path.

**Tech Stack:** Rust (`hush-core`, `hush-relay`, `hush-daemon`, `hush-cli`), libsignal's
X25519 `KeyPair` (already in tree), hand-rolled HKDF beside the existing HMAC,
`chacha20poly1305` XChaCha (already in tree), embedded static page with `@noble/curves` +
`@noble/ciphers` + `@noble/hashes` (pure JS, bundled by esbuild, committed as a single
inline-asset HTML file), Tauri + TypeScript UI.

**Spec:** this conversation (2026-10-09), plus `docs/plans/2026-10-07-outsider-mail.md`
for the v2 sealed-box format it kept verbatim.

## Global Constraints

- The relay stores **ciphertext only** and serves **one stateless page**. No plaintext,
  no key, no recipient address, no directory ever reaches it.
- No new Rust dependencies. HKDF is hand-rolled beside the existing hand-rolled HMAC;
  the JS crypto comes from the noble family, bundled into the page asset.
- The page never loads anything from another origin: strict CSP, all JS and CSS inline.
- Drops render as **plain text** in the UI (no rich text, no linkified anchors, no remote
  fetch). Attachments are **save-only**: no preview, no auto-open, sha256 shown.
- Size caps: `MAX_DROP_BYTES` = 8 MiB per container (relay-enforced), drop attachment
  count/size inside bounded by the container cap. Filenames are sanitized to a basename
  on save.
- The drop key lives in the vault (wrapped by the master key, like `identity.secret`), so
  backups and recovery keep the published drop address alive across devices.
- Rotation archives the retired key; the daemon keeps draining retired rendezvous ids
  until the relay TTL expires them, so rotation never loses drops in flight.
- **Out of scope for this plan:** Flatpak/systemd/bwrap hardening, Tails/Qubes support,
  mail composition to outsiders (outbound), the v1 PIN container mode.
- No commits — the working tree already carries an unrelated in-flight slice.

## Files

| File | Responsibility |
|---|---|
| `crates/hush-core/src/drop.rs` *(new)* | `hush-drop-v2` sealed box (`seal_to_key`/`open_with_key`), `DropPackage`, drop keypair, pasteable key encodings, `rendezvous_id`, `MAX_DROP_BYTES` |
| `crates/hush-core/src/crypto.rs` | `pub(crate) fn hkdf_sha256` beside the existing HMAC; make `hmac_sha256` crate-visible |
| `crates/hush-core/src/error.rs` | `BadDrop`, `UnsupportedDrop`, `DropNotForThisKey`, `InvalidDropKey`, `DropKeyExists`, `DropKeyMissing` |
| `crates/hush-core/src/lib.rs` | `mod drop;` + re-exports |
| `crates/hush-core/src/vault.rs` | Wrapped `drop-keys.enc` (current + retired), create/rotate/accessors |
| `crates/hush-core/src/chat_log.rs` | `ChatRecordPayload::Drop(DropRecord)` variant + `append_drop` |
| `crates/hush-core/src/relay_client.rs` | `put_drop`, `get_drop` (404 → `None`), `delete_drop` |
| `crates/hush-core/src/account.rs` | `drain_drops()` |
| `crates/hush-core/tests/fixtures/drop-v2-node.hshd` *(new)* | Container sealed by `scripts/drops-vector.mjs`; proves JS↔Rust wire compat |
| `crates/hush-core/tests/drops.rs` *(new)* | Core integration tests (uses in-process `hush-relay`) |
| `crates/hush-relay/src/store.rs` | `drops` table, `put_drop`/`get_drop`/`delete_drop`, TTL purge |
| `crates/hush-relay/src/app.rs` | `GET /drop` (page), `PUT`/`GET`/`DELETE /drop/{id}`, id validation |
| `crates/hush-relay/assets/drop/` *(new)* | `main.js` (page logic), `page.html` (template), `build.sh` (esbuild → committed `page.built.html`) |
| `crates/hush-relay/tests/drops.rs` *(new)* | Route tests: round-trip, caps, id validation, page headers |
| `scripts/drops-vector.mjs` *(new)* | Node+noble seal/open; emits and verifies the wire-compat fixture |
| `crates/hush-daemon/src/account_worker.rs` | `drain_drops` work; runs after `recv`; DTO mapping for `Drop` records |
| `crates/hush-daemon/src/main.rs` | `GET /api/drops/key`, `POST /api/drops/key`, `POST /api/drops/rotate`, `POST /api/drops/drain` |
| `crates/hush-cli/src/main.rs` | `drop-keygen`, `drop-key-show`, `drop-rotate` |
| `apps/hush-tauri/src/lib/api/types.ts` | `ChatRecordDto` drop variant |
| `apps/hush-tauri/src/lib/mailStore.ts` | `MailFolder` += `drops`; folder listing/filtering |
| `apps/hush-tauri/src/lib/api/daemon.ts` | `dropsKey`, `dropsKeyGenerate`, `dropsKeyRotate`, `dropsDrain` |
| `apps/hush-tauri/src/app.ts` | Drops folder + quarantine rendering; Settings → Drops section |

---

### Task 1: Drop container and key material (`hush-core`)

> **Done (2026-10-09).** 8/8 tests pass, including the noble-sealed fixture
> (`tests/fixtures/drop-v2-node.*`, generated by `scripts/drops-vector.mjs seal`). Two
> corrections to the plan text: the pinned libsignal's `KeyPair` fields are
> `public_key`/`private_key` and the all-zero rejection lives in the re-exported
> wrapper (see `DEEPSEEKPROBLEMS.MD`); `@noble/curves` 1.9 exports `x25519` from the
> `ed25519` module. The reload test dropped the backup clause from its name and asserts
> reload only.

**Files:**
- Create: `crates/hush-core/src/drop.rs`, `scripts/drops-vector.mjs`,
  `crates/hush-core/tests/fixtures/drop-v2-node.hshd`
- Modify: `crates/hush-core/src/crypto.rs`, `crates/hush-core/src/error.rs`,
  `crates/hush-core/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `drop.rs`

**Interfaces:**
- Consumes: `libsignal_protocol::KeyPair` (X25519), `chacha20poly1305::XChaCha20Poly1305`,
  `crypto::hmac_sha256`, `crypto::random_bytes`, `base64::URL_SAFE_NO_PAD`.
- Produces: `DropPackage { v, subject, body, attachments: Vec<DropAttachment { name,
  data }> , created_unix }`, `DropKeyPair { sk: [u8; 32], pk: [u8; 32] }`,
  `drop_keypair()`, `seal_to_key(&DropPackage, &[u8; 32]) -> Result<Vec<u8>, Error>`,
  `open_with_key(&[u8], &DropKeyPair) -> Result<DropPackage, Error>`,
  `rendezvous_id(pk: &[u8; 32]) -> String` (32 lowercase hex),
  `encode_drop_pk`/`decode_drop_pk`/`encode_drop_sk`/`decode_drop_sk` (`hush-drop-pk-v1:`
  / `hush-drop-sk-v1:` base64url), `is_drop_container(&[u8]) -> bool`,
  `DROP_V2_HEADER_LEN`, `MAX_DROP_BYTES`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_package_round_trips_under_its_drop_key() {
    let recipient = drop_keypair();
    let pkg = sample();
    let bytes = seal_to_key(&pkg, &recipient.pk).unwrap();
    assert!(is_drop_container(&bytes));
    let back = open_with_key(&bytes, &recipient).unwrap();
    assert_eq!(back.subject, "hi");
    assert_eq!(back.attachments[0].name, "a.txt");
}

#[test]
fn the_wrong_drop_key_does_not_open() {
    let recipient = drop_keypair();
    let other = drop_keypair();
    let bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
    assert!(matches!(
        open_with_key(&bytes, &other),
        Err(Error::DropNotForThisKey)
    ));
}

#[test]
fn a_tampered_header_or_body_does_not_open() {
    let recipient = drop_keypair();
    let mut bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(open_with_key(&bytes, &recipient).is_err());
    let mut bytes = seal_to_key(&sample(), &recipient.pk).unwrap();
    bytes[5] = 9; // mode byte, authenticated via AAD
    assert!(matches!(open_with_key(&bytes, &recipient), Err(Error::UnsupportedDrop)));
    bytes[4] = 2;
    bytes[5] = 1; // right version, unknown mode
    assert!(matches!(open_with_key(&bytes, &recipient), Err(Error::UnsupportedDrop)));
}

#[test]
fn a_garbage_container_is_refused_cleanly() {
    assert!(matches!(open_with_key(b"nope", &drop_keypair()), Err(Error::BadDrop)));
    let mut short = vec![b'H', b'S', b'H', b'D', 2, 2];
    short.extend_from_slice(&[0u8; 40]);
    assert!(matches!(open_with_key(&short, &drop_keypair()), Err(Error::BadDrop)));
}

#[test]
fn hkdf_sha256_matches_the_rfc5869_vectors() {
    // Test Case 1: 22 octets of 0x0b, salt 0x00..0x0c, info 0xf0..0xf9, 42 out.
    let ikm = [0x0b; 22];
    let salt: [u8; 13] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c];
    let info: [u8; 10] = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
    let mut out = [0u8; 42];
    hkdf_sha256(&ikm, &salt, &info, &mut out);
    assert_eq!(
        hex::encode(out),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
    );
    // Test Case 3: empty salt and info, 82 octets out — exercises the
    // zero-salt path and multi-block expand in one vector.
    let mut long = [0u8; 82];
    hkdf_sha256(&ikm, b"", b"", &mut long);
    assert_eq!(
        hex::encode(long),
        "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8b2fb61057244b36c6ddd287f634795e7d80d5fe26bfc36def6dc129c29271a0eb7ab14bd2ca88259"
    );
}

#[test]
fn the_rendezvous_id_is_stable_and_keyed_by_the_public_key() {
    let a = drop_keypair();
    let b = drop_keypair();
    assert_eq!(rendezvous_id(&a.pk), rendezvous_id(&a.pk));
    assert_ne!(rendezvous_id(&a.pk), rendezvous_id(&b.pk));
    assert_eq!(rendezvous_id(&a.pk).len(), 32);
    assert!(rendezvous_id(&a.pk).chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn pasteable_keys_refuse_the_wrong_half_and_bad_lengths() {
    let kp = drop_keypair();
    let pk = encode_drop_pk(&kp.pk);
    let sk = encode_drop_sk(&kp.sk);
    assert!(pk.starts_with("hush-drop-pk-v1:"));
    assert!(sk.starts_with("hush-drop-sk-v1:"));
    assert!(matches!(decode_drop_pk(&sk), Err(Error::InvalidDropKey)));
    assert!(matches!(decode_drop_pk("hush-drop-pk-v1:!!!"), Err(Error::InvalidDropKey)));
    assert!(matches!(
        decode_drop_pk("hush-drop-pk-v1:AAAA"),
        Err(Error::InvalidDropKey)
    ));
}

#[test]
fn opens_a_node_sealed_container() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(dir.join("tests/fixtures/drop-v2-node.hshd")).unwrap();
    // The fixture's private key lives beside it (hex) and must open the bytes
    // the noble stack sealed.
    let sk_hex =
        std::fs::read_to_string(dir.join("tests/fixtures/drop-v2-node.sk")).unwrap();
    let mut sk = [0u8; 32];
    sk.copy_from_slice(&hex::decode(sk_hex.trim()).unwrap());
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&hex::decode(
        std::fs::read_to_string(dir.join("tests/fixtures/drop-v2-node.pk")).unwrap().trim(),
    ).unwrap());
    let pkg = open_with_key(&bytes, &DropKeyPair { sk, pk }).unwrap();
    assert_eq!(pkg.subject, "from the page");
    assert_eq!(pkg.attachments[0].name, "note.txt");
}
```

- [ ] **Step 2: Run tests — expect FAIL**

`cargo test -p hush-core drop::` → unresolved module `drop`.

- [ ] **Step 3: Implement**

`crypto.rs`: change `fn hmac_sha256` to `pub(crate) fn`, and add beside it:

```rust
/// HKDF-SHA256 (RFC 5869), extract-then-expand. Hand-rolled for the same
/// reason HMAC is: `sha2` is already here and a crate would arrive on a second
/// `digest` major. Checked against the RFC 5869 vectors in the tests.
pub(crate) fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    // extract: with an empty salt, RFC 5869 says HMAC with zeros.
    let mut salt_block = [0u8; 32];
    if !salt.is_empty() {
        salt_block[..salt.len().min(32)].copy_from_slice(&salt[..salt.len().min(32)]);
    }
    let prk = hmac_sha256(&salt_block, ikm);
    // expand
    let mut t = [0u8; 32];
    let mut counter = 1u8;
    let mut done = 0;
    while done < out.len() {
        let mut input = Vec::with_capacity(33 + info.len());
        input.extend_from_slice(&t[..done.min(32)]);
        input.extend_from_slice(info);
        input.push(counter);
        t = hmac_sha256(&prk, &input);
        let take = (out.len() - done).min(32);
        out[done..done + take].copy_from_slice(&t[..take]);
        done += take;
        counter += 1;
    }
}
```

(Note: the first expand iteration's input is `info || 0x01` with an empty `t`; the loop
above feeds `t` empty on the first pass, which is the same bytes.)

`error.rs`:

```rust
#[error("malformed drop container")]
BadDrop,
#[error("unsupported drop container version")]
UnsupportedDrop,
#[error("this drop was not sealed to this drop key")]
DropNotForThisKey,
#[error("not a valid drop key (expected hush-drop-pk-v1: or hush-drop-sk-v1:)")]
InvalidDropKey,
#[error("a drop key already exists — rotate it instead")]
DropKeyExists,
#[error("no drop key exists yet")]
DropKeyMissing,
```

`drop.rs`: the header is exactly the removed plan's v2 layout — magic `HSHD`, version
`2`, mode `2`, 32-byte ephemeral public key, 24-byte nonce, `DROP_V2_HEADER_LEN = 62`;
the full header is the AEAD associated data. Seal: ephemeral `KeyPair::generate`
(`OsRng.unwrap_err()` per `signal.rs`), `calculate_agreement` against the recipient
public key (rejects the all-zero shared secret a low-order point produces →
`Error::Crypto`), `hkdf_sha256(ikm = shared, salt = ephemeral_pk, info =
b"hush-drop-v2" || recipient_pk)` → 32-byte key, XChaCha20-Poly1305 over the JSON
`DropPackage`. Open: the inverse; any AEAD failure → `Error::DropNotForThisKey`.
`rendezvous_id` = `hex(hmac_sha256(recipient_pk, b"hush-drop-rendezvous-v1")[..16])`.
Pasteable encodings: base64url-no-pad of the raw 32 bytes under the two distinct
prefixes; `decode` refuses wrong prefix, wrong alphabet, or wrong length with
`Error::InvalidDropKey`.

`lib.rs`: `mod drop;` and
`pub use drop::{drop_keypair, open_with_key, rendezvous_id, seal_to_key, DropAttachment,
DropKeyPair, DropPackage, MAX_DROP_BYTES};`

Generate the JS↔Rust fixture with `scripts/drops-vector.mjs` (node + `@noble/curves` +
`@noble/ciphers` + `@noble/hashes`, resolving the same versions the page uses — see
Task 3's assets): `node scripts/drops-vector.mjs seal
crates/hush-core/tests/fixtures/drop-v2-node` writes the container and the hex sk/pk
sidecars; `… open <prefix>` verifies a Rust-sealed container (run once by hand after
Task 3's page lands and recorded as verification output).

- [ ] **Step 4: Run tests — expect PASS**

`cargo test -p hush-core drop::` → 8 passed.

---

### Task 2: Vault drop keypair storage and rotation

> **Done (2026-10-09).** 10/10 vault tests pass (two new: reload survival, rotation
> archive + cap). `rotate` clones the current entry — `ZeroizeOnDrop` forbids moving
> out. Full `hush-core` suite green (83 lib + all integration).

**Files:**
- Modify: `crates/hush-core/src/vault.rs`
- Test: inline `#[cfg(test)] mod tests` additions in `vault.rs`

**Interfaces:**
- Consumes: `drop::drop_keypair`, `drop::encode_drop_pk`, `crypto::encrypt/decrypt`.
- Produces: `Vault::create_drop_keypair(&self) -> Result<String, Error>` (pasteable pk),
  `Vault::rotate_drop_keypair(&self) -> Result<String, Error>`,
  `Vault::drop_public_key(&self) -> Option<String>` (pasteable),
  `Vault::drop_keys_for_drain(&self) -> Vec<DropDrainKey>` where
  `DropDrainKey { public_key: String, pk: [u8; 32], sk: [u8; 32] }` (current first, then
  retired, newest first, capped at the 3 most recent).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_drop_key_survives_a_reload_and_a_backup_round_trip() {
    let dir = tempdir();
    let vault = Vault::create(&dir, "passphrase").unwrap();
    assert!(vault.drop_public_key().is_none());
    let pk = vault.create_drop_keypair().unwrap();
    assert!(pk.starts_with("hush-drop-pk-v1:"));
    assert!(vault.create_drop_keypair().is_err()); // DropKeyExists

    let reloaded = Vault::open(&dir, "passphrase").unwrap();
    assert_eq!(reloaded.drop_public_key().unwrap(), pk);
    assert_eq!(reloaded.drop_keys_for_drain().len(), 1);
}

#[test]
fn rotating_the_drop_key_archives_the_old_one_and_caps_the_history() {
    let vault = Vault::create(&dir, "passphrase").unwrap();
    let first = vault.create_drop_keypair().unwrap();
    let second = vault.rotate_drop_keypair().unwrap();
    assert_ne!(first, second);
    let keys = vault.drop_keys_for_drain();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].public_key, second, "current first");
    assert_eq!(keys[1].public_key, first, "retired second");
    // Three more rotations: the retired list stays capped at three.
    for _ in 0..3 { vault.rotate_drop_keypair().unwrap(); }
    assert_eq!(vault.drop_keys_for_drain().len(), 4); // current + 3 retired
    assert!(!vault.drop_keys_for_drain().iter().any(|k| k.public_key == first));
}
```

- [ ] **Step 2: Run tests — expect FAIL**

`cargo test -p hush-core vault::tests::the_drop_key` → no method `create_drop_keypair`.

- [ ] **Step 3: Implement**

One wrapped file in the vault dir, `drop-keys.enc` — the same `encrypt(&master, …)`
treatment `identity.secret` gets:

```rust
#[derive(Serialize, Deserialize)]
struct DropKeysFile {
    current: DropKeyEntry,
    /// Retired keys the daemon still drains (relay TTL bounds their lifetime).
    /// Capped at three; the oldest falls off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired: Vec<DropKeyEntry>,
}
#[derive(Serialize, Deserialize)]
struct DropKeyEntry { sk_hex: String, pk_hex: String, #[serde(default)] retired_unix: u64 }
```

`create_drop_keypair` errors with `Error::DropKeyExists` when the file exists; else
generates a pair, writes the file, returns `encode_drop_pk`. `rotate_drop_keypair`
errors with `Error::DropKeyMissing` when absent; else loads, moves `current` into
`retired` (stamping `retired_unix = now`), truncates `retired` to the newest three,
writes the new pair, returns the new pasteable pk. `drop_keys_for_drain` decodes hex to
byte arrays. The in-memory sk bytes are held only for the duration of a call; nothing
keeps them cached in the `Vault` struct.

- [ ] **Step 4: Run tests — expect PASS**

`cargo test -p hush-core vault::` → all green.

---

### Task 3: Relay drops store, routes, and the drop page

> **Done (2026-10-09).** 2/2 route tests pass; full `hush-relay` suite green (18 across
> 4 binaries). Notes: the page ships as `assets/drop/{main.js,page.html,build.sh}` with
> the committed `page.built.html` (53 KB, esbuild bundle inlined at the marker) and
> serves with `default-src 'none'`, `no-referrer`, `no-store`, `nosniff`. Id shape is
> validated in `Store::validate_drop_id` (32 lowercase hex); TTL purge covers `drops`;
> `GET /drop/{id}` is non-consuming, `DELETE` is always 204 (idempotent for multi-device
> drains). The `connect-src 'self'` directive was added beyond the plan's list so the
> page can only ever talk back to its own origin. **Deferred:** the manual
> Rust-sealed → node-opens wire-compat run waits for the Task 7 CLI (`drop-` tools give
> a way to produce a Rust fixture); the noble → Rust direction is already covered by
> the Task 1 fixture test.

**Files:**
- Modify: `crates/hush-relay/src/store.rs`, `crates/hush-relay/src/app.rs`,
  `crates/hush-relay/src/error.rs` (only if id validation needs a variant)
- Create: `crates/hush-relay/assets/drop/main.js`, `…/page.html`, `…/build.sh`,
  `…/page.built.html` (committed build output), `crates/hush-relay/package.json`
  (dev deps: `esbuild`, `@noble/curves`, `@noble/ciphers`, `@noble/hashes`)
- Test: `crates/hush-relay/tests/drops.rs` *(new)*

**Interfaces:**
- Consumes: `hush_core::MAX_DROP_BYTES`.
- Produces: `GET /drop` → the page; `PUT /drop/{id}` (204; 413 over cap; 400 on a bad
  id; rate-limited per id); `GET /drop/{id}` (container bytes; 404); `DELETE /drop/{id}`
  (204; idempotent 404-tolerant). `Store::put_drop`, `Store::get_drop`,
  `Store::delete_drop`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_drop_round_trips_and_is_bounded() {
    let (app, _db) = test_app();
    let body = vec![7u8; 128];
    assert_eq!(put_drop(&app, &valid_id(), &body).await, StatusCode::NO_CONTENT);
    let got = get_drop(&app, &valid_id()).await;
    assert_eq!(got.1, body);
    assert_eq!(delete_drop(&app, &valid_id()).await, StatusCode::NO_CONTENT);
    assert_eq!(get_drop(&app, &valid_id()).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        put_drop(&app, &valid_id(), &vec![0u8; hush_core::MAX_DROP_BYTES + 1]).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(put_drop(&app, "short", &body).await, StatusCode::BAD_REQUEST);
    assert_eq!(
        put_drop(&app, &"A".repeat(32), &body).await, // not hex
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_drop_page_serves_static_with_a_closed_csp() {
    let (app, _db) = test_app();
    let res = app.oneshot(get("/drop")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res.headers()["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'"), "closed by default");
    assert!(!csp.contains("http"), "no external origins may be named");
    let body = into_body(res).await;
    assert!(body.contains("hush-drop-pk-v1:"), "the page explains the key format");
    assert!(body.contains("PUT"), "the page uploads by PUT");
}
```

- [ ] **Step 2: Run tests — expect FAIL** (404 on every route)

`cargo test -p hush-relay --test drops`

- [ ] **Step 3: Implement**

`store.rs`:

```sql
CREATE TABLE IF NOT EXISTS drops (
    id TEXT PRIMARY KEY,
    body BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_drops_created ON drops(created_at);
```

`put_drop` refuses bodies over `hush_core::MAX_DROP_BYTES` (`RelayError::TooLarge`),
rate-limits on the key `format!("drop:{id}")` via the existing `check_rate`, and relies
on `purge_expired` for the same TTL queues use (extend it to delete `drops` rows older
than the TTL — drops are 8 MiB, so an expired-row sweep matters more here than for
queue blobs). `get_drop` and `delete_drop` are plain fetch/remove; **get is
non-consuming on purpose** — the client deletes only after it has successfully opened.

`app.rs`: `validate_drop_id(&id)` — exactly 32 lowercase hex characters, else
`RelayError` bad-request. Routes:

```rust
.route("/drop", get(drop_page))
.route("/drop/{id}", put(put_drop).get(get_drop).delete(delete_drop))
```

`drop_page` serves the committed `crates/hush-relay/assets/drop/page.built.html` via
`include_str!` with: `content-type: text/html; charset=utf-8`,
`content-security-policy: default-src 'none'; script-src 'unsafe-inline'; style-src
'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'`,
`referrer-policy: no-referrer`, `cache-control: no-store`, `x-content-type-options:
nosniff`.

**The page.** Single template, one inlined bundle:

- `page.html` — form: paste-your-key field (validates the `hush-drop-pk-v1:` prefix in
  JS), subject, body, file picker (advisory only; rejects nothing the user insists on),
  total-size meter against the 8 MiB cap, "Seal and upload" button, and a final panel
  that reports success **without echoing anything identifying**. Static copy states:
  the recipient is anonymous to the relay; use the onion address in Tor Browser.
- `main.js` — pure-JS crypto from the noble family: `x25519.utils.randomPrivateKey()`
  for the ephemeral key, `x25519.getSharedSecret(eph_sk, recipient_pk)`,
  `hkdf(sha256, ikm = shared, salt = eph_pk, info = "hush-drop-v2" || recipient_pk)`,
  `xchacha20poly1305(key, nonce, aad = header)` — byte layouts identical to Task 1
  (`HSHD`/2/2/eph-pk/nonce, ct||tag). Rendezvous id computed in JS: 16-byte truncation
  of HMAC-SHA256(key = recipient_pk, msg = "hush-drop-rendezvous-v1"), hex. Upload:
  `fetch('drop/' + id, { method: 'PUT', body })` (relative URL — works on both relay
  bases). No cookies, no storage, no external requests, one PUT, done.
- `build.sh` — `esbuild main.js --bundle --minify --format=iife --outfile=page.js`,
  then inline `page.js` into the template at a `<!--__DROP_JS__-->` marker →
  `page.built.html`. The built file is committed so Rust builds need no node.

**Wire-compat check (manual, recorded):** `node scripts/drops-vector.mjs open <file>`
against a Rust-sealed fixture, and the Task 1 fixture test in the other direction. Both
outputs pasted into the PR description.

- [ ] **Step 4: Run tests — expect PASS**

`cargo test -p hush-relay --test drops` → 2 passed.

---

### Task 4: Client drain — relay client, log record, account

> **Done (2026-10-09).** 3/3 drain tests pass (`a_drop_drains_into_a_quarantined_record`,
> `a_container_not_for_this_key_stays_parked`, `a_hostile_container_is_clamped_not_fatal`);
> full `hush-core` suite green (83 lib + all integration). Three deliberate deviations
> from the plan text, all recorded:
>
> 1. **No separate `DropAttachmentRecord`.** `Drop` reuses `MailAttachmentRecord
>    {name, blob_id_hex, size}` — the vault's blob id already *is* the SHA-256, so the
>    plan's `sha256_hex` field was redundant duplication.
> 2. **A container that will not open stays parked and is counted, not an error.** The
>    plan said "surfaces as an error"; that would let a hostile container wedge the
>    daemon's recv loop forever (same container, same failure, every poll).
>    `DropInboxReport { hit_slots, remaining }` replaces `DrainDropsOutcome { drained }`:
>    wrong-key/garbage containers are left on the relay (evidence preserved, deletable
>    by hand later) and reported as `remaining`.
> 3. **Content inside an opened container is clamped, not rejected** — subject 256
>    chars, body 1 MiB chars, control characters stripped (newlines/tabs kept),
>    attachment names reduced to a 255-char basename, attachments beyond 8 or over the
>    256 KiB inline cap skipped *with a disclosure note appended to the body*. The
>    hostile-container test asserts all of it.
>
> Also: `Account::ensure_drop_keys` is idempotent (returns the existing key) where the
> plan sketched an erroring `create_drop_keypair` — the vault method keeps the strict
> behavior underneath. `drop_records()` accessor added beside `mail_records()`/`
> `draft_records()` for the daemon DTO mapping.

**Files:**
- Modify: `crates/hush-core/src/relay_client.rs`, `crates/hush-core/src/chat_log.rs`,
  `crates/hush-core/src/account.rs`
- Test: `crates/hush-core/tests/drops.rs` *(new)*

**Interfaces:**
- Consumes: `drop::{seal_to_key, open_with_key, rendezvous_id}`, `Vault::
  drop_keys_for_drain`, `Vault::put_blob`.
- Produces: `RelayClient::put_drop(&self, id, body)`, `get_drop(&self, id) -> Result<
  Option<Vec<u8>>, Error>` (404 → `None`), `delete_drop(&self, id)`;
  `ChatRecordPayload::Drop(DropRecord)` with `DropRecord { drop_id: String, subject:
  String, body: String, attachments: Vec<DropAttachmentRecord { name, blob_id_hex, size,
  sha256_hex }>, received_unix: u64, sealed_to: String }`; `ChatLog::append_drop`;
  `Account::drain_drops() -> Result<DrainDropsOutcome { drained: usize }, Error>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_drop_sealed_to_the_published_key_drains_into_the_log() {
    let relay = test_relay().await;
    let mut member = account_with_relay(&relay).await;
    let pk_paste = member.create_drop_keypair().unwrap();

    // The "dropper": seal to the published key under the rendezvous id.
    let recipient_pk = decode_drop_pk(&pk_paste).unwrap();
    let pkg = DropPackage { v: 2, subject: "scoop".into(), body: "plain text only".into(),
        attachments: vec![DropAttachment { name: "a.txt".into(), data: base64("hi") }],
        created_unix: now_secs() };
    let id = rendezvous_id(&recipient_pk);
    let client = RelayClient::new(member.relay()); // `relay()` is the base URL string
    client.put_drop(&id, &seal_to_key(&pkg, &recipient_pk).unwrap()).await.unwrap();

    let out = member.drain_drops().await.unwrap();
    assert_eq!(out.drained, 1);

    // Filed as a drop record, body intact, attachment in the blob store, sha256 present.
    let rec = member.mail_records().into_iter()
        .find(|r| matches!(r.payload, ChatRecordPayload::Drop(_))).unwrap();
    match rec.payload {
        ChatRecordPayload::Drop(d) => {
            assert_eq!(d.drop_id, id);
            assert_eq!(d.subject, "scoop");
            assert_eq!(d.body, "plain text only");
            assert_eq!(d.attachments[0].name, "a.txt");
            assert_eq!(d.attachments[0].sha256_hex, hex::encode(sha256(b"hi")));
        }
        _ => unreachable!(),
    }

    // Consumed: a second drain finds nothing, and the relay slot is gone.
    assert_eq!(member.drain_drops().await.unwrap().drained, 0);
    assert!(client.get_drop(&id).await.unwrap().is_none());
}

#[tokio::test]
async fn a_drop_for_a_retired_key_still_drains_and_a_wrong_key_lands_nothing() {
    // Rotate after sealing: the drop sealed to the retired key still arrives.
    // A drop sealed to an unrelated key is fetched by nobody: the rendezvous
    // id is derived from a key this vault does not hold.
}
```

- [ ] **Step 2: Run tests — expect FAIL**

`cargo test -p hush-core --test drops` → no `create_drop_keypair` on `Account`
(expose thin `Account::create_drop_keypair`/`rotate`/`drop_public_key` pass-throughs),
no `drain_drops`, no `ChatRecordPayload::Drop`.

- [ ] **Step 3: Implement**

`relay_client.rs`: three methods following the `put`/`get`/`delete` shapes already
present (no queue token — the id is the capability); `get_drop` maps 404 to `Ok(None)`
like `list_optional`.

`chat_log.rs`: the new enum variant with `DropAttachmentRecord { name, blob_id_hex,
size, sha256_hex }` (a superset of `MailAttachmentRecord`; separate type so the mail
shape stays untouched). `append_drop(&mut self, master, record…) -> Result<(), Error>`
uses a fresh uuid for the record id, `contact_nick` = `"drop"` (a stable synthetic
label the UI renders as *Anonymous via drop*), `outgoing: false`, `ts_ms = now`.
Old logs decode unchanged: a new enum variant needs no serde aliasing.

`account.rs`:

```rust
pub async fn drain_drops(&mut self) -> Result<DrainDropsOutcome, Error> {
    let mut drained = 0;
    for key in self.vault.drop_keys_for_drain() {
        let id = rendezvous_id(&key.pk);
        let Some(bytes) = self.relay.get_drop(&id).await? else { continue };
        let pkg = drop::open_with_key(&bytes, &key.drop_keypair())?;
        // Store attachments as vault blobs, then the record, then consume.
        let attachments = pkg.attachments.iter().map(|a| {
            let data = BASE64_STANDARD.decode(&a.data).map_err(|_| Error::BadDrop)?;
            let sha = sha256(&data);
            let blob = self.vault.put_blob(&data)?;
            Ok(DropAttachmentRecord { name: a.name.clone(), blob_id_hex: hex::encode(blob.0),
                size: data.len() as u64, sha256_hex: hex::encode(sha) })
        }).collect::<Result<Vec<_>, Error>>()?;
        self.chat_log.append_drop(&master, &id, &pkg, attachments)?;
        self.relay.delete_drop(&id).await?;
        drained += 1;
    }
    Ok(DrainDropsOutcome { drained })
}
```

The get→open→append→delete order means a crash between fetch and open loses nothing
(the slot refills), and a crash after append re-fetches once and re-opens — the record
is content-identical, and the delete still lands. A drop sealed to a key we hold but
that fails to open is **left in place** and surfaces as an error, never silently
deleted.

- [ ] **Step 4: Run tests — expect PASS**

`cargo test -p hush-core --test drops` → 2 passed.

---

### Task 5: Daemon — drain worker and endpoints

> **Done (2026-10-09).** `the_drops_api_creates_drains_and_files_a_drop` passes — the
> daemon's first test ever. Two structural notes for later slices:
>
> 1. **The router moved into `api_router(state)`** so tests drive the real routes
>    without binding a socket (the daemon is a bin-only crate; unit tests live in
>    `main.rs`). New dev-deps: `hush-relay`, `http-body-util`, `tempfile`, `tower/util`.
> 2. **`MailboxResponse` grew a `drops` array** (`{mail, drafts, drops}`): the Drop
>    payload would otherwise be invisible to the UI, and the plan's "the mail records
>    response carries the Drop payload through" is this, with drops kept out of `mail`
>    so ordinary inbox rendering never needs the variant.
>
> Endpoint semantics as planned, with one sharpening: `POST /api/drops/key` is **strict**
> (`Account::create_drop_keys` → 400 on `DropKeyExists`) — a second "Generate" press is
> a mistake the caller must see, not something to quietly absorb. The idempotent
> `Account::ensure_drop_keys` stays for affordances that mean "make sure I have one".
> The worker drains right after each successful `recv` poll (best-effort, logged on
> failure), and `POST /api/drops/drain` forces a pass for the Settings pane.

**Files:**
- Modify: `crates/hush-daemon/src/account_worker.rs`, `crates/hush-daemon/src/main.rs`
- Test: `crates/hush-daemon` unit/integration tests

**Interfaces:**
- Produces: worker `drain_drops()` (also invoked right after each successful `recv`
  cycle so drops arrive with the normal poll); routes `GET /api/drops/key` →
  `{ exists, public_key, page_url }`, `POST /api/drops/key` → `{ public_key }`,
  `POST /api/drops/rotate` → `{ public_key }`, `POST /api/drops/drain` → `{ drained }`.
  `page_url` prefers the vault's `onion_relay` when set, else the clearnet base, plus
  `/drop`. The mail records response carries the `Drop` payload through to the UI DTO.

- [ ] **Step 1: Write the failing test**

A daemon test with a test relay: unlock an account, `POST /api/drops/key`, seal to the
returned key under its rendezvous id (using `hush_core` directly, like the Task 4
test), `POST /api/drops/drain`, then `GET /api/mail` and assert a drop record comes
back; assert `GET /api/drops/key` reports `exists: true` with a `page_url` ending in
`/drop` and preferring the onion base when one is staged.

- [ ] **Step 2: Run test — expect FAIL** (404s)

- [ ] **Step 3: Implement**

Worker `drain_drops` mirrors `recv`'s lock/unlock handling; the `recv` path calls it
after the normal drain so arrival needs no extra timer. DTO: the existing mail-records
mapping grows the `Drop` arm (subject, body, attachments with `sha256Hex`, `dropId`,
`sealedTo`, `receivedUnix`).

- [ ] **Step 4: Run test — expect PASS**

`cargo test -p hush-daemon` → green.

---

### Task 6: UI — the quarantined Drops folder

**Files:**
- Modify: `apps/hush-tauri/src/lib/api/types.ts`, `apps/hush-tauri/src/lib/mailStore.ts`,
  `apps/hush-tauri/src/app.ts`, `apps/hush-tauri/src/styles/app.css`
- Check: `npx tsc --noEmit && npm run build`

**Interfaces:**
- Consumes: the daemon endpoints and the drop DTO.
- Produces: `MailFolder` += `"drops"` (label **Drops**, listed under Inbox); drop
  records render: provenance banner (*"From outside Hush — anonymous sender, cannot be
  replied to"*), body as **escaped plain text** (`escapeHtml` + `pre-wrap`; the rich
  text pipeline is never invoked), links shown as text with a Copy affordance,
  attachments as save-only chips with a `sha256` line (`downloadBase64File` + basename
  sanitization), no reply/compose actions, no inline image previews. Empty state: with
  no drop key, "No drop key yet — publish one so outsiders can send you drops" linking
  to Settings → Drops; with a key, "Drops sent to your published key will appear here."

- [x] **Step 1: Write the failing check** — `npx tsc --noEmit` after adding the folder
  id and DTO variant but before the rendering wiring.
- [x] **Step 2: Run — expect FAIL** on the unhandled folder.
- [x] **Step 3: Implement** the folder, the filter in `mailStore.listFolder`, the
  thread-render branch, and the save-only chips.
- [x] **Step 4: Run — expect PASS** — `npx tsc --noEmit && npm run build` clean.

> Done (2026-10-09). Note: `mailStore` names the lookup `getDropById` — `dropById`
> was already exported by `lib/drops/registry` for the SecureDrop rail. Body
> rendering uses `textContent` + `white-space: pre-wrap` (equivalent escaping to
> the `escapeHtml` wording). `npm run build` verified via `npx tsc --noEmit`
> plus the vite bundle in the session's earlier build.

---

### Task 7: UI — Settings → Drops, and the CLI

**Files:**
- Modify: `apps/hush-tauri/src/app.ts` (`SettingsSection` += `"drops"`, section pane),
  `apps/hush-tauri/src/lib/api/daemon.ts`, `crates/hush-cli/src/main.rs`
- Check: `npx tsc --noEmit && npm run build`, `cargo test -p hush-cli`

**Interfaces:**
- Produces: Settings → Drops pane: the pasteable public key with Copy; the drop page
  URL with Copy (and a "publish both of these together" hint); **Generate drop key**
  when none exists; **Rotate** with a confirm that states the retired key keeps
  draining until the relay TTL. CLI: `hush-cli drop-keygen` (`Error::DropKeyExists`
  surfaces as a clear message), `drop-key-show`, `drop-rotate`.

- [x] **Step 1: Write the failing checks** — tsc on the new section id; `cargo test
  -p hush-cli` for the subcommand plumbing (keygen on a temp vault asserts the
  pasteable output and the second call's error).
- [x] **Step 2: Run — expect FAIL.**
- [x] **Step 3: Implement.**
- [x] **Step 4: Run — expect PASS** — both commands green; manual browser pass of the
  compose page against the dev relay recorded in `DEEPSEEKPROBLEMS.MD` if anything
  bites.

> Done (2026-10-09). Live browser pass recorded: the page sealed and uploaded against
> a scratch relay (127.0.0.1:8799); a scratch vault's daemon (`hush-cli init` +
> `drop-keygen`, daemon on 8798) drained the container on
> `POST /api/drops/drain` (`hit_slots` matched the page's rendezvous id), the slot
> came back 404 after the drain (consumed), and `GET /api/mail` returned the drop
> record with subject/body intact. Reverse wire check (Rust seal → JS open) done via
> `scripts/drops-vector.mjs open` on a throwaway `scratch_rust_seal` test, then
> deleted. Full `cargo test --workspace` green after fixing the pre-existing
> environment-dependent `tor::tests::missing_binary_reports_unavailable` (see
> `DEEPSEEKPROBLEMS.MD`).

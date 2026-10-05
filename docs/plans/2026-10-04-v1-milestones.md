# v1 Capsule Implementation Plan

**Goal:** Two people on Linux can add each other by QR/invite, send encrypted text and a small file through a dumb queue relay, and open a local vault after unlock — without the relay knowing their names.

**Architecture:** Rust core owns keys and vault. Local daemon exposes an API. Tauri UI is a local client. Relay stores ciphertext on invite-minted queue IDs. Signal protocol for 1:1; no groups.

**Tech stack:** Rust, libsignal, SQLite, axum (or equivalent), Tauri, signed updater (later milestone).

**Spec:** `docs/DESIGN.md`, `docs/THREAT_MODEL.md`, `docs/PROTOCOL.md`, `docs/WORKSPACE.md`.

## Global Constraints

- No code until the user says to implement (this file is the plan only).
- Do not invent a ratchet; use libsignal.
- UI must not fetch JS/HTML from the network.
- Relay has no user directory.
- No seed phrases on the home screen.
- Unpaid users must still open the vault (payments are after dogfood).
- Tests on core and relay before polishing UI.

## Files to create (when implementation starts)

| Path | Responsibility |
|---|---|
| `crates/hush-core/` | Identity, vault, sessions, queue client |
| `crates/hush-relay/` | Queue HTTP API, TTL, rate limits |
| `crates/hush-daemon/` | Unlock, local API |
| `crates/hush-cli/` | Send/recv without GUI |
| `apps/hush-tauri/` | Contacts, conversation, send file, vault status |

---

### Task 1: Workspace and vault identity

**Files:** `crates/hush-core/` (new)

- Cargo workspace as in `WORKSPACE.md`.
- Generate identity key, wrap with passphrase, open vault.
- SQLite + encrypted blobs directory.
- Tests: wrong passphrase fails; round-trip a blob.

**Verify:** `cargo test -p hush-core`

---

### Task 2: Relay queues

**Files:** `crates/hush-relay/`

- `PUT/GET/DELETE` for a queue id with a slot secret.
- 7-day TTL, 1 MiB cap, per-queue rate limit.
- No user table.
- Tests: unauthorized put fails; expire drops blob; report burns queue.

**Verify:** `cargo test -p hush-relay` and a local binary on localhost.

---

### Task 3: Two local clients through the relay

**Files:** `hush-core` session layer, `hush-cli`

- Two vaults on one machine.
- Invite mints queues; Signal session established using invite data.
- CLI send text; other CLI drain queue and decrypt.
- Fan-out not required yet.

**Verify:** script: start relay, two CLIs, message appears decrypted only on the recipient.

---

### Task 4: Files

**Files:** `hush-core`

- Encrypt file to recipient, store in sender vault, send over session (or chunk if over 1 MiB — prefer small-file v1).
- Recipient stores in their vault.

**Verify:** test with a small fixture file; plaintext never on relay disk (assert stored body is not the file bytes).

---

### Task 5: Daemon + minimal Tauri UI

**Files:** `hush-daemon`, `apps/hush-tauri`

- Unlock, contacts, conversation, send file, vault size.
- Auto-lock.
- No remote UI assets.

**Verify:** manual on Linux: two app instances, QR/invite, send text and file. (Browser tools are not the app.)

---

### Task 6: Recovery paths (no social recovery)

**Files:** `hush-core`, settings UI

- Buried recovery code at first unlock.
- Export/import encrypted vault to a folder (stand-in for WebDAV/S3).
- Device-link QR if time; else document as next slice.

**Verify:** destroy local vault copy, restore with passphrase + backup file; recover keys with recovery code in a test.

---

### Task 7: Signed builds (ship path)

**Files:** CI or a `scripts/` sign+verify helper, Tauri updater config

- minisign/ed25519 (or Tauri’s scheme) key pinned in the app.
- Refuse unsigned update.

**Verify:** verify command on a built artifact; a tampered file fails.

---

### Task 8: Deliberately later (do not start)

- Stripe
- WebDAV/S3 live providers
- Android
- Groups, follow-model
- Flathub
- Expert review pass on queue auth before calling transport “done”

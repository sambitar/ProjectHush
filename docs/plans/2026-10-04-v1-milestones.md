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

### Task 5b: Per-contact files (exchange view)

**Goal:** Files you share with someone feel like a **folder for that contact**, but stay **1:1 E2E** — two private vault copies linked by encrypted sends, not a synced cloud drive or group share.

**Product copy (UI):** e.g. “Files you’ve exchanged with Alice — stored only on your devices.”

**Not in this task (defer):** mutable shared folders, multi-writer sync, relay-side folder listings, conflict resolution, group shares.

**Files:** `crates/hush-daemon/`, `apps/hush-tauri/` (and `hush-core` only if inbox layout moves under `vault/contacts/<nick>/`).

**Slices (do in order while dogfooding):**

| Slice | Work | Verify |
|-------|------|--------|
| **5b.1 Open / save** | Daemon: read file bytes from **vault blob** (Task 5c.2); include `file_size` / `blob_id` in recv DTO. UI: **Open** / **Save as…** on chat file cards (blob URL or Tauri save dialog). No long-lived plaintext export path under vault. | Two-UI: receive file → open or save → matches bytes sent; outgoing file openable from vault blob or session buffer. |
| **5b.2 Files rail by contact** | Files screen: list contacts (or recent activity) → drill-down **Received** / **Sent** per nick; “Send file” uses same path as chat attach. Aggregate from chat store + daemon inbox index (not a new protocol). | Files tab matches chat history for that contact; tap row jumps to chat or opens file. |
| **5b.3 Vault layout (optional)** | Normalize on-disk layout e.g. `inbox/` → `contacts/<nick>/in/` (migrate or alias); daemon lists files per contact for Files rail without scraping chat-only state. | CLI or API list per contact; existing `instances/*/inbox/*` still reachable after migration. |

**Depends on:** Task 4 (encrypted file transfer), Task 5 (daemon + UI shell). Can land **5b.1** before **5b.2**.

**Verify (end-to-end):** `./scripts/run-two-ui.sh` — mutual invites, send file both ways, open from chat and from Files → contact → Received/Sent; lock clears UI; durable files only under **encrypted** vault storage (see Task 5c).

**Depends on (at-rest):** Task **5c** before relying on `inbox/` plaintext paths — 5b.1 should read via daemon from **vault blobs / encrypted store**, not leave exports on disk in the clear.

---

### Task 5c: Relay metadata privacy + encrypted vault (chat log + files on disk)

**Goal:** Match [`THREAT_MODEL.md`](../THREAT_MODEL.md) and [`PROTOCOL.md`](../PROTOCOL.md): relay sees **ciphertext blobs and queue IDs only** (no cleartext sender names in wire JSON); **everything durable under the vault directory is confidential when locked**, including **chat history** and received files.

**Current gaps (dogfood audit):**

- `WireEnvelope` sends **`from` in the clear** inside the blob the relay stores (`hush-core`).
- Daemon writes **`inbox/*` plaintext** after decrypt (`hush-daemon`).
- **Text messages** are not persisted in the vault after `recv` (UI session memory only).
- `account.enc` + `blobs/*` are already encrypted with the vault master key; layout is inconsistent.

#### Vault on disk: one encrypted *space* vs encrypting every file

| Approach | Idea | v1 recommendation |
|----------|------|-------------------|
| **OS encrypted folder** (e.g. fscrypt, gocryptfs, LUKS file) | Whole directory is ciphertext at rest; unlock = mount | Optional power-user path; **not** the primary cross-platform story; backup/restore and Android later get harder. |
| **Logical vault (recommended)** | Single passphrase → master key; **all** app data under `vault_dir/` is either encrypted blobs or an **encrypted DB**; **no plaintext** chat/file/message paths after lock | Aligns with existing `account.enc` + `blobs/` pattern; extend to **messages + attachments index**. Daemon is the only writer while unlocked. |
| **SQLCipher / full-DB vault** | One encrypted SQLite for state + messages + blob refs | Listed in [`WORKSPACE.md`](../WORKSPACE.md) as an option; evaluate when message schema stabilizes; can be the **implementation** of “logical vault” without a kernel mount. |

**Product rule:** When locked, a disk scan of the vault folder must not reveal message text, filenames, or file bytes (filenames may stay as content hashes or opaque IDs on disk if needed).

**Files:** `crates/hush-core/`, `crates/hush-daemon/`, `apps/hush-tauri/` (read path only); relay unchanged except clients send opaque wire bytes.

**Slices:**

| Slice | Work | Verify |
|-------|------|--------|
| **5c.1 Relay wire opaque** | Replace cleartext `WireEnvelope { from, ct }` with **opaque bytes** (e.g. only `ct`, or padded outer blob); sender identity only inside Signal plaintext / session. Document wire format; **expert review** before calling transport done. Assert relay DB blob does not contain protocol names or JSON `from` keys. | Integration test: put blob on relay; `strings` / hex scan does not contain known `protocol_name` substrings. |
| **5c.2 No plaintext under vault** | Stop writing `inbox/` cleartext; store received files as **`vault.put_blob`** (already encrypted) + **metadata row** (contact, name, time, blob id). Remove or migrate existing `inbox/*`. Sender-side copies already use blobs where applicable. | After recv + lock: no readable file bytes under vault dir; `cargo test` round-trip get_blob. |
| **5c.3 Encrypted chat log** | Encrypted store (SQLite with app-level crypto or SQLCipher) for **1:1 threads**: text + file refs (blob id, display name encrypted or stored inside encrypted record). `recv` appends; UI loads history on unlock. Optional: prune/TTL local retention policy later. | Unlock → see prior messages; lock → DB/blobs not readable without passphrase; two-UI send/recv survives restart. |
| **5c.4 Transport hygiene** | Document/deploy **HTTPS** to relay in production; optional: refuse `http://` except loopback in release builds. Queue rotation / report-burn as in PROTOCOL (tie to 5c.1 review). | Manual checklist for VPS relay; tests unchanged on localhost. |

**Ordering:** **5c.1** and **5c.2** before **5b** file open/save that depends on stable blob IDs; **5c.3** can parallel UI work once daemon exposes `GET` history or paginated thread API.

**Verify (end-to-end):** `./scripts/run-two-ui.sh` — chat history survives app restart; received file openable from vault blob; relay `relay.db` blobs fail a “no identity substring” assertion; locked vault directory has no plaintext message or attachment content.

---

### Task 5d: Internal mail (`@myserver.com`, members only)

**Goal:** Full **email-like** product for users on Hush keys + vault — threads, compose, **attachments**, contact list — **without** Internet SMTP between members. Addresses like `sandy@myserver.com` are **routing labels** bound to vault identity (protocol name / keys), not a public MX mailbox.

**Not in this task:** Receiving mail from arbitrary `@gmail.com` (see **5e** inbound drop); **sending** mail to outsiders (see **5f**, spec TBD).

**Depends on:** Task **5c** (encrypted chat log + blobs on disk), Task **5b** (per-contact files UX). Builds on existing 1:1 Signal transport and relay queues.

**Slices:**

| Slice | Work | Verify |
|-------|------|--------|
| **5d.1 Address book** | Map `local@myserver.com` ↔ internal identity; display in UI; optional user-chosen local part with server suffix fixed. | Two members see each other’s `@` handle in profile/compose. |
| **5d.2 Mail UX** | Compose **To/Cc** (Cc optional v1: To only), subject line optional or omitted per product; body + multiple attachments; thread view distinct from or merged with chat (product choice). | Send text + attachment; thread persists after restart (5c.3). |
| **5d.3 Transport** | Same E2E path as chat: opaque relay blobs (5c.1), payloads in vault; attachment refs as encrypted blobs. | Relay blob scan shows no plaintext subject/body; members decrypt only. |

**Verify:** Two registered `@myserver.com` users exchange multi-attachment “mail” end-to-end; locked vault hides content; relay does not learn member-to-member social graph beyond queue metadata goals in 5c.

---

### Task 5e: External inbound drop (outsiders → member)

**Goal:** People **without** a Hush account can deliver **one encrypted package** (body + attachments) to a member. **No passphrase in email body.** **No plaintext** accepted at the boundary — only a versioned **container file** from a lightweight **packager** (website or small app).

**Threat model note (different from internal mail):** The relay (or upload gateway) **may** learn **routing metadata** — e.g. which **drop token** or recipient slot a blob targets, plus size, time, IP. It must **never** hold or parse **plaintext** body/attachments. Document this split in [`THREAT_MODEL.md`](../THREAT_MODEL.md) when implementing.

**Design (agreed):**

| Rule | Detail |
|------|--------|
| **Routing** | **Per-user drop token** or `POST …/drop/{routing_token}` (share link/QR on member’s “Receive from outside” page). Optional outer header in container: `routing_token` or hashed `sandy@myserver.com` — relay reads **only** this fixed header, not inner ciphertext. |
| **Container** | `hush-drop-v1` (name TBD): outer **routing field** + **ciphertext** (AES-GCM + wrap to recipient **drop public key**). Packager UI lets outsider pick recipient as `sandy@myserver.com` (encoded in header), not typed into email prose. |
| **Public door** | One branded entry: e.g. `drop@myserver.com` (email attachment only) and/or `https://myserver.com/send` — gateway **rejects** non-container uploads and **empty/ plaintext-only** submissions. |
| **Relay** | Store **ciphertext only**; route by token/header; **TTL + size cap** like today; rate limits + abuse (CAPTCHA, per-token quota, disable drops per user). |
| **Member client** | Poll or notify; decrypt locally; show in **Inbox / External** (separate from member-to-member mail if useful). |

**Not in this task:** Outbound member → outsider (**5f**). SMTP full federation. IPFS required (optional blob backend later).

**Files:** `crates/hush-relay/` (drop routes), new `hush-drop` or `apps/hush-drop-web` packager (WASM/static), `hush-core` (drop keys + decrypt), `hush-daemon` + UI inbox.

**Slices:**

| Slice | Work | Verify |
|-------|------|--------|
| **5e.1 Wire format + keys** | Drop keypair or derived key per user; container encode/decode; tests vectors. | Round-trip encrypt/decrypt without network. |
| **5e.2 Relay drop API** | `PUT` ciphertext by routing token; list/drain for member queue; TTL; no user directory. | Outsider upload → member recv decrypts; `strings` on relay DB clean for body. |
| **5e.3 Packager (web)** | Static/WASM page: recipient, message, files → download `.hushdrop` or direct upload. | Browser produces valid container; invalid/plain rejected by API. |
| **5e.4 Optional email ingress** | MTA accepts mail to `drop@…`, extracts attachment if magic type, forwards to relay API; body discarded. | Send from external MUA with attachment only → member receives. |

**Depends on:** **5c.2** (vault blobs for decrypted drops), **5c.1** spirit for internal vs drop metadata policy.

**Verify:** Outsider uses packager → upload → `sandy@` unlocks and reads; plaintext upload fails; token guessing rate-limited.

---

### Task 5f: Outbound to outsiders (`deliver@` + outsider contacts + PIN)

**Goal:** Internal member sends an **encrypted package** (body + attachments) to someone **without** a Hush account. Same **container family** as **5e** (`hush-drop-v1`). Honest bounds: notification email (SMTP) is **not** E2E; payload on relay is **ciphertext only**.

**Bot address:** e.g. `deliver@myserver.com` (or in-app **Send to outsider** that routes through the same backend). Not a human mailbox — a **delivery processor**.

**Outsider contacts (in vault, encrypted):**

| Field | Purpose |
|-------|---------|
| Display name | “Bob (Gmail)” in compose picker |
| Notification address | e.g. `bob@gmail.com` — SMTP **notification only** (link + expiry; no PIN in email body) |
| **Delivery PIN** | Shared secret Sandy sets when adding the outsider; used to **encrypt** the package (KDF → AES-GCM). Sandy shares PIN with Bob **out of band** when establishing the contact. Stored in vault; never sent to relay in clear. |

Member maintains a **list of outsider contacts** (like address book entries). Rotate PIN per contact when needed.

**Send flow:**

1. Compose → pick **outsider contact** (not `@myserver.com` member).
2. Client builds `hush-drop-v1` ciphertext keyed from **that contact’s delivery PIN** (and optional per-message random salt).
3. Upload ciphertext to relay (TTL + size cap); relay sees blob id / delivery id only for routing storage — **no plaintext**.
4. `deliver@` processor sends **notification email** to notification address: short text + **pickup link** (and expiry). PIN is **not** in the email.
5. Member app shows link + reminder to share PIN if Bob does not have it yet.

**Unlock (outsider):**

1. Open **pickup link** → lightweight **Open delivery** web page (pair to **5e.3** packager codebase).
2. Enter **delivery PIN** (the one Sandy gave them when added as outsider contact).
3. Browser decrypts locally (WASM); show message + attachments; optional download `.hushdrop`.

**Rules:**

- No passphrase / PIN in email body or subject.
- Plaintext-only submissions rejected at `deliver@` / upload API.
- Rate-limit pickup attempts per delivery id (PIN guessing).

**Later (not v1):** optional **outsider public key** on contact instead of PIN (repeat recipients); same list UI, different wrap mode.

**Files:** `hush-core` (outsider contact model, encrypt/decrypt helpers), `hush-daemon` + UI (outsider address book, send), relay drop/delivery store, `deliver@` SMTP egress or worker, web **open** + **send** pages.

**Slices:**

| Slice | Work | Verify |
|-------|------|--------|
| **5f.1 Outsider contacts** | Encrypted vault table: name, notification email, PIN (or KDF salt + verifier); CRUD in Settings / Contacts. | Lock vault → contacts not readable; unlock → list persists. |
| **5f.2 Deliver encrypt + upload** | Compose to outsider → PIN-wrap container → relay PUT; in-app copy link. | `strings` on relay blob has no body; wrong PIN fails decrypt on open page. |
| **5f.3 Notification email** | SMTP (or provider) from `deliver@…`: link only, TTL stated. | Inbox shows link; no PIN in headers/body. |
| **5f.4 Open delivery web** | PIN entry + fetch blob by id → decrypt in browser. | End-to-end test with fixture container. |

**Depends on:** **5d** (compose/attachments UX), **5e.1** (container format), **5c.2** (vault storage patterns).

**Verify:** Member adds outsider Bob with PIN → sends package → Bob opens link + PIN in browser → reads message; wrong PIN locked out after N tries.

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
- Expert review pass on queue auth (covered in Task **5c.1** / **5c.4** before calling transport “done”)
- Full SMTP/IMAP product for `@myserver.com` on the public Internet (partial inbound via **5e** only)

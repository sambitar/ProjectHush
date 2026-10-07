# v1 Capsule Implementation Plan

**Goal:** Two people on Linux can add each other by QR/invite, send encrypted text and a small file through a dumb queue relay, and open a local vault after unlock — without the relay knowing their names.

**Architecture:** Rust core owns keys and vault. Local daemon exposes an API. Tauri UI is a local client. Relay stores ciphertext on invite-minted queue IDs. Signal protocol for 1:1; no groups.

**Tech stack:** Rust, libsignal, SQLite, axum (or equivalent), Tauri, signed updater (later milestone).

**Spec:** `docs/DESIGN.md`, `docs/THREAT_MODEL.md`, `docs/PROTOCOL.md`, `docs/WORKSPACE.md`, [`docs/DISTRO_READINESS.md`](../DISTRO_READINESS.md).

**Product stance (2026-10):** Pre-ship **dogfood** — add features and harden the capsule before installers or signed releases. **Task 7** (signed builds / updater) stays **last** until you choose to produce.

## Recommended build order

Task IDs are stable for cross-links; **implement in this sequence** (not the section order below).

| Phase | When | Tasks | Status |
|-------|------|-------|--------|
| **A — Foundation** | First | **1** workspace/vault, **2** relay, **3** two-party CLI, **4** files | ✅ Done |
| **B — Capsule messaging** | Core product | **5** daemon + Tauri, **5c** opaque relay + encrypted log/blobs, **5b.1–5b.2** files UX, **5h** live presence, **6** recovery/backup | ✅ Mostly done |
| **C — Messaging polish** | Next dogfood | **5b.3** (optional), remaining `API_GAPS.md` | 🔜 **In progress** (DR-01–09, 14; 5c.4; remove contact; device link; passphrase change done; multi-device **1–3 of 7** done — per-device keys, signed device list, relay device directory + revocation) |
| **D — Async member mail** | When live chat feels solid | **5d** internal `@myserver.com` mail (replaces “message while offline”) | ✅ Done (threads, compose, multi-attachment, offline delivery; Cc/Bcc + out-of-band attachments deferred) |
| **E — Outsider delivery** | After 5d | **5e** inbound drop, **5f** outbound to outsiders | Planned |
| **F — Local notes** | Before public layer | **16** sticky-notes dashboard | Planned |
| **G — Articles** | After notes or parallel late | **11** Nostr pen names | Planned |
| **H — Knowledge layer** | After 11 dogfood | **12–15** (see [`knowledge-layer-roadmap.md`](knowledge-layer-roadmap.md)) | Backlog |
| **I — Confidential** | Heavy; spec early, code late | **10** TPM + lease rail | Planning only |
| **J — Ship** | When feature-complete | **7** signed builds, CI audit, packages | **Deferred** |
| **K — Member calls** | After **C** stabilizes | **17** P2P audio/video between contacts | Planning only |
| **—** | Do not start | **8** (Stripe, Android, groups, Flathub, OS image, …) | Later |

**Rationale (short):** Finish **private 1:1** (B) before **mail** (D) and **outsiders** (E). **Notes** (F) are vault-local and smaller than **Articles** (G). **Knowledge** (H) needs pen names and encrypted store. **Confidential** (I) is a separate trust domain — after messaging at-rest is boring. **Ship** (J) only when you are ready to produce.

Detailed specs follow **by task ID** (1, 2, …). Dependency order is in the table above.

## Global Constraints

- This file is the plan; implementation proceeds task-by-task on request.
- Do not invent a ratchet; use libsignal.
- UI must not fetch JS/HTML from the network.
- Relay has no user directory.
- No seed phrases on the home screen.
- Unpaid users must still open the vault (payments are after dogfood).
- Tests on core and relay before polishing UI.
- When implementing, follow **Rules for the app now** in [`DISTRO_READINESS.md`](../DISTRO_READINESS.md). If a task conflicts, flag before proceeding.

## Distro-readiness checks (apply while implementing)

Use before closing slices on Tasks **1, 2, 5, 5c, 6, 7, 10**. Full rules: [`DISTRO_READINESS.md`](../DISTRO_READINESS.md). Known violations: [`docs/tasks/distro-readiness-gaps.md`](../tasks/distro-readiness-gaps.md).

- [x] Persistent state only under configurable **data root** (`HUSH_DATA_DIR` / vault dir); runtime/socket/temp only under **runtime dir**.
- [x] Vault path works as a **removable mount**; daemon handles **mount loss** (lock + key wipe + recovery).
- [ ] **No plaintext** secrets or message/file content on disk; no secrets in logs.
- [x] Daemon API: prefer **Unix socket** in runtime dir; **no TCP by default** in release; document sandbox paths/syscalls.
- [x] **systemd** unit with hardening directives shipped in repo (when daemon packaging lands).
- [x] Network: relay client is the **single outbound** abstraction; **HTTPS** for non-loopback in release (5c.4).
- [ ] **`KeyProvider`** for unlock (passphrase first; TPM/FIDO/lease later) — no hardcoded TPM/passphrase in vault core.
- [ ] UI remains a **thin daemon client**; no untrusted file auto-open.
- [ ] Builds: **`--locked`**, reproducibility docs, signing layer reusable beyond Tauri (Task 7).

## Files to create (when implementation starts)

| Path | Responsibility |
|---|---|
| `crates/hush-core/` | Identity, vault, sessions, queue client |
| `crates/hush-relay/` | Queue HTTP API, TTL, rate limits |
| `crates/hush-daemon/` | Unlock, local API |
| `crates/hush-cli/` | Send/recv without GUI |
| `apps/hush-tauri/` | Contacts, conversation, send file, vault status |
| `crates/hush-articles/` | (Task 11) Nostr pen names, feed, cache — **planned** |

---

### Task 1: Workspace and vault identity

**Status:** ✅ Done.

**Files:** `crates/hush-core/` (new)

- Cargo workspace as in `WORKSPACE.md`.
- Generate identity key, wrap with passphrase, open vault.
- SQLite + encrypted blobs directory.
- Tests: wrong passphrase fails; round-trip a blob.

**Verify:** `cargo test -p hush-core`

**Distro-readiness:** Introduce **`HUSH_DATA_DIR`** layout (vault + config); all durable paths under data root; parsing treats vault bytes as untrusted. See DR-01, DR-17 in [`distro-readiness-gaps.md`](../tasks/distro-readiness-gaps.md).

---

### Task 2: Relay queues

**Status:** ✅ Done (presence heartbeats added for live chat — see **5h**).

**Files:** `crates/hush-relay/`

- `PUT/GET/DELETE` for a queue id with a slot secret.
- 7-day TTL, 1 MiB cap, per-queue rate limit.
- No user table.
- Tests: unauthorized put fails; expire drops blob; report burns queue.

**Verify:** `cargo test -p hush-relay` and a local binary on localhost.

**Distro-readiness:** Relay URL config must be consumable from a **single client config** (future Tor/firewall); no implicit cwd `relay.db` as product default — DR-14.

---

### Task 3: Two local clients through the relay

**Status:** ✅ Done.

**Files:** `hush-core` session layer, `hush-cli`

- Two vaults on one machine.
- Invite mints queues; Signal session established using invite data.
- CLI send text; other CLI drain queue and decrypt.
- Fan-out not required yet.

**Verify:** script: start relay, two CLIs, message appears decrypted only on the recipient.

---

### Task 4: Files

**Status:** ✅ Done.

**Files:** `hush-core`

- Encrypt file to recipient, store in sender vault, send over session (or chunk if over 1 MiB — prefer small-file v1).
- Recipient stores in their vault.

**Verify:** test with a small fixture file; plaintext never on relay disk (assert stored body is not the file bytes).

---

### Task 5: Daemon + minimal Tauri UI

**Status:** ✅ Core done — remaining work lives in **Phase C** (distro, socket, CORS, mount loss, systemd).

**Files:** `hush-daemon`, `apps/hush-tauri`

- Unlock, contacts, conversation, send file, vault size.
- Auto-lock.
- No remote UI assets.

**Verify:** manual on Linux: two app instances, QR/invite, send text and file. (Browser tools are not the app.)

**Distro-readiness:** Ship **systemd user unit** + hardening; migrate daemon to **Unix socket** in `HUSH_RUNTIME_DIR`; remove permissive CORS; move UI prefs out of `localStorage` into data root or daemon; mount-loss + suspend lock; core dump disable — DR-02–DR-09, DR-05. Flag any new TCP listener. **Done:** socket-first API (DR-02) and the hardened unit + `mlock`/dumpable secret handling (DR-06, DR-07, `scripts/smoke-daemon-hardening.sh`).

---

### Task 5b: Per-contact files (exchange view)

**Status:** ✅ **5b.1** / **5b.2** done; **5b.3** optional (**Phase C**).

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

**Status:** ✅ **5c.1–5c.4** done in tree (**5c.4** ships the tested HTTPS policy, `packaging/systemd/system/hush-relay.service`, and a deployable [`docs/ops/relay-https-checklist.md`](../ops/relay-https-checklist.md)). Expert transport review still recommended before calling relay “done.”

**Goal:** Match [`THREAT_MODEL.md`](../THREAT_MODEL.md) and [`PROTOCOL.md`](../PROTOCOL.md): relay sees **ciphertext blobs and queue IDs only** (no cleartext sender names in wire JSON); **everything durable under the vault directory is confidential when locked**, including **chat history** and received files.

**Historical gaps (addressed in code — keep for audit trail):**

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

**Distro-readiness:** **5c.2** closes DR-04 (plaintext `inbox/`); **5c.3** closes DR-17; **5c.4** enforces HTTPS policy; encrypted store must live under **data root** only; `zeroize`/`mlock` on DB keys — DR-07, DR-13.

---

### Task 5h: Live chat presence (online-only send)

**Status:** ✅ Done.

**Goal:** Chat is **live** when both sides are online; no async chat while the contact is offline (async member mail = **5d**). Relay **presence** heartbeats (~90s TTL); daemon blocks send when recipient offline; UI shows online indicator (no “Delivered” delivery mock).

**Files:** `hush-relay` (`/q/{id}/presence`), `hush-core` / `hush-daemon`, `apps/hush-tauri`.

**Verify:** `./scripts/run-two-ui.sh` — both unlocked → green dot; one locked → other cannot send; mail hint in composer when offline.

**Depends on:** **5**, **2**. **Before:** **5d**.

---

### Phase C: Messaging polish (next — not a numbered task)

Work through these while dogfooding; split into PR-sized slices as needed.

| Area | Examples | Tracks |
|------|----------|--------|
| **Distro / daemon** | `HUSH_DATA_DIR`, Unix socket default (**done**), tighten CORS, systemd unit, mount-loss lock, suspend lock, central client config | [`distro-readiness-gaps.md`](../tasks/distro-readiness-gaps.md) DR-01–09, 14 |
| **Transport** | **5c.4** done (HTTPS policy tested, relay unit, deploy checklist); sealed-sender expert review remains | **5c.1** / PROTOCOL |
| **Files** | **5b.3** per-contact vault layout (optional) | **5b** |
| **Contacts / settings** | Real mute/block/remove (**done**); device-link QR one-time transfer (**6** slice, **done** — Settings → Security + unlock “Transfer from another device”); passphrase change (**done** — vault format v2 wraps the master key, so rotation rewrites `vault.lock.json` only and leaves blobs, `account.enc`, `chat.enc` and the recovery wrap intact; `POST /api/change-passphrase`, Settings → Security); nick overrides in data root not `localStorage` | `API_GAPS.md`, DR-05 |
| **UI dogfood** | Reactions, command palette polish, error states | `API_GAPS.md` |

**Exit criteria:** Two-peer script is boring for a week; no plaintext surprises under vault; daemon ready for socket-first packaging.

---

### Task 5d: Internal mail (`@myserver.com`, members only)

**Status:** ✅ Done. Plan: [`2026-10-07-internal-mail.md`](2026-10-07-internal-mail.md).

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

**Residual limitations (v1):**

- No **Cc/Bcc** — To only.
- Attachments are **inlined on the wire**, capped at **8 files / 256 KiB total** (same small-file bound as chat); chunked/out-of-band attachment fetch is deferred to **5e**/**5f**.
- The handle is an **invite-carried label**; a contact added before a handle change keeps the old handle until the invite is re-added. There is no handle search or relay directory (Threat Model C).
- Mail reuses the `chat.enc` log via a `ChatRecordPayload::Mail` variant; independent mail retention is deferred until it is needed (Rule of Three).

---

### Task 5e: External inbound drop (outsiders → member)

**Status:** Planned — **Phase E** (after **5d**).

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

**Status:** Planned — **Phase E** (after **5e**).

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

**Status:** ✅ Done. Device-link QR slice implemented as a one-time encrypted transfer (unlocked old device → locked new device); live multi-device sessions remain a later slice.

**Files:** `hush-core`, settings UI

- Buried recovery code at first unlock.
- Export/import encrypted vault to a folder (stand-in for WebDAV/S3).
- Device-link QR if time; else document as next slice.
- **Articles (Task 11):** pen-name keypairs and articles DB/cache in backup manifest (pen name loss = identity loss).

**Verify:** destroy local vault copy, restore with passphrase + backup file; recover keys with recovery code in a test.

**Distro-readiness:** Mandatory **encrypted backup** + first-run prompt; introduce **`KeyProvider`** (passphrase impl first); USB wear / dead stick recovery path — DR-12, DR-16. Align with [`DISTRO_READINESS.md`](../DISTRO_READINESS.md) Confidential/USB notes.

---

### Task 7: Signed builds (ship path)

**Status:** **Deferred** — **Phase J** when producing installers/updater (not required for feature dogfood).

**Files:** CI or a `scripts/` sign+verify helper, Tauri updater config

- minisign/ed25519 (or Tauri’s scheme) key pinned in the app.
- Refuse unsigned update.

**Verify:** verify command on a built artifact; a tampered file fails.

**Distro-readiness:** Extract **`hush-verify`** (or similar) crate/CLI reused by Tauri and future OS updater; add `rust-toolchain.toml`, CI `cargo deny`/`audit`, reproducible release doc — DR-10, DR-11.

---

### Task 16: Notes dashboard (sticky papers — planning only)

**Status:** Planned — **Phase F** (before **11** Articles).

**Goal:** Private **sticky-note** board in the capsule: papers on a dashboard or **grouped** stacks, **drag-and-drop** layout, **checklists** with check-off — encrypted local store, backup via Task **6**. Visual metaphor: real note paper (Soft Daylight), not a collaborative whiteboard.

→ **[`docs/tasks/notes-dashboard.md`](../tasks/notes-dashboard.md)**

**Depends on:** Task **5c**, **6**. **Independent** of Articles (Task **11**). Optional link to **projects** (Task **14.2**) later.

**Not v1:** No multi-user live board on Hush infrastructure; no publishing notes to Nostr by default.

---

### Task 11: Articles (Nostr pen names — planning only)

**Status:** Planned — **Phase G** (after **16** or in parallel once **Phase C** is stable).

**Goal:** Pseudonymous long-form publishing: **pen names** (vault keypairs, isolated from messaging), **Nostr** articles/comments (NIP-23/22/09/50/56 — **verify before build**), **Tor** transport (`arti`), local follows/mutes/drafts, daemon-sanitized UI.

Full design, build order, and acceptance criteria:

→ **[`docs/tasks/hush-articles.md`](../tasks/hush-articles.md)**

**Crate:** `crates/hush-articles/` on **rust-nostr**; daemon **`/api/articles/*`**; UI never talks to relays directly.

**Build order (summary):** pen names + relay pool → publish/read → feed phase 1 → media pipeline → editor → comments/search/mute/report → feed phase 2 (opt-in global) → Tor per-pen-name hardening.

**Depends on:** Task **5** (daemon API), **5c** (encrypted local store), **6** (backup includes pen-name keys). **Independent** of `hush-relay` / libsignal messaging.

**Distro-readiness:** Cache/drafts under data root; document **second egress** (Nostr over Tor); sanitized-only UI — see Task 11 doc.

**Not at launch:** Operating a first-party Nostr relay.

**Future (post-11):** Magazine sections, argument graph, library, briefing, ideas/debates/research, projects, graph UI — phased and policy-filtered in [`knowledge-layer-roadmap.md`](knowledge-layer-roadmap.md) (Tasks **12–15** backlog, not scheduled).

---

### Tasks 12–15: Knowledge layer (backlog — planning only)

**Status:** **Phase H** — not implemented. ChatGPT-style “Magazine / Ideas / Graph” ideas **filtered** against DESIGN and threat model C.

→ **[`docs/plans/knowledge-layer-roadmap.md`](knowledge-layer-roadmap.md)**

| Phase | Tasks | Theme |
|-------|-------|--------|
| **A (12.x)** | Magazine filters, argument links, library, explainable briefing, doc→publish | → [`docs/tasks/knowledge-layer-phase-a.md`](../tasks/knowledge-layer-phase-a.md) |
| **B (13.x)** | Theses, research templates, debates | Discourse without likes |
| **C (14.x)** | Local knowledge graph UI, projects, investigations | Organize work |
| **D (15.x)** | Audio, courses, private messaging rooms, local library AI | Defer; strict privacy |

**Do not start** until Task **11** is dogfooded unless explicitly reprioritized.

**Status:** **Phase H** — backlog.

---

### Task 10: Confidential section (TPM + lease — planning only)

**Status:** **Phase I** — spec/threat model can proceed anytime; **implementation** after messaging + notes/articles unless reprioritized.

**Goal:** Linux-only **Confidential** rail: custom encrypted containers (hardware-bound via TPM 2.0), server-signed **lease** before KEK unseal, in-app editor with no plaintext on disk.

→ **[`docs/tasks/confidential-section.md`](../tasks/confidential-section.md)**

**Phases (summary):** (1) spec + threat model → (2) crypto core `.cnf`/`.hcf` → (3) TPM seal → (4) lease server → (5) daemon lease client → (6) UI rail → (7) stolen-device + recovery → (8) security review.

**Depends on (recommended):** Task **5c.2** (no plaintext messaging vault) before overlapping file workflows.

**Distro-readiness:** **`KeyProvider`**; FIDO2/stick vs machine TPM; lease server separate from relay — [`DISTRO_READINESS.md`](../DISTRO_READINESS.md).

---

### Task 17: Member calls — P2P audio/video (planning only)

**Status:** **Phase K** — planning only. Spec decisions resolved (2026-10-06); no code until a phase is approved.

**Goal:** Two online contacts hold an **end-to-end encrypted, direct peer-to-peer audio** call (video later). Signaling rides the existing relay as an authenticated channel; media goes straight between the daemons, so **no Hush server carries it**. Each contact learns the other's IP (**accepted**); IP-level privacy is deferred to the OS/distro layer or a user-supplied relay.

→ **[`docs/tasks/member-calls.md`](../tasks/member-calls.md)**

**Key constraint:** Tauri on Linux renders through WebKitGTK, which does **not** expose `RTCPeerConnection` by default (experimental build flag, not enabled by any distro). The peer connection and media pipeline therefore live in **`hush-daemon`**, not the webview; the UI is chrome only. **LiveKit** is evaluated and **deferred** to the future group-call path (Task 15.3) — it is an SFU whose default mode sees media and maps the graph, which v1 1:1 does not need.

**Depends on:** Task **5h** (presence), **5c** (opaque transport + encrypted log), **5** (daemon). **Independent of** **5d** / **16** / **11** / **10**.

**Distro-readiness:** new runtime deps (GStreamer + plugins, `xdg-desktop-portal`); second egress (self-hosted STUN; optional user-supplied TURN); daemon capture-device/systemd exceptions — see the task doc.

---

### Task 8: Deliberately later (do not start)

- Stripe
- WebDAV/S3 live providers
- Android
- Groups; **messaging** follow-model (Articles Task **11** uses **local-only** follows, not published lists)
- Flathub
- Expert review pass on queue auth (covered in Task **5c.1** / **5c.4** before calling transport “done”)
- Full SMTP/IMAP product for `@myserver.com` on the public Internet (partial inbound via **5e** only)
- Confidential **implementation** (spec in Task **10** may proceed earlier; ship after v1 dogfood unless reprioritized)
- **Distro / OS image** (stages 2–5 in [`DISTRO_READINESS.md`](../DISTRO_READINESS.md)) — app must meet distro-ready acceptance criteria first
- **Signed releases** (Task **7**) — when producing, not during feature dogfood
- See **Recommended build order** at top of this file for **11 / 16 / 12–15 / 10** sequencing

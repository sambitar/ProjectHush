# Confidential section (planning only)

**Status:** Not implemented. No implementation code until a phase is explicitly approved.

**Index:** Listed as **Task 10** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md).

**Goal:** Add a **Confidential** app area that stores text (and later binary) in a custom encrypted container. Ciphertext copied to another machine stays unreadable without this device’s TPM + valid lease. Target: **Linux + TPM 2.0**.

**Stack fit (current repo):**

| Layer | Today | Confidential (planned) |
|-------|--------|-------------------------|
| Crypto primitives | `hush-core` — Argon2id, **XChaCha20-Poly1305** (`crypto.rs`) | Same family for containers (AEAD); **no Python `cryptography`** — use Rust `chacha20poly1305` / `aes-gcm` |
| Keys / unlock | Passphrase → vault master key; daemon holds keys while unlocked | Separate **KEK** in TPM + **server lease**; orthogonal to chat vault passphrase |
| UI | Tauri rail: Chats, Files, **Vault**, Settings (`app.ts`) | New rail **Confidential** (distinct from Vault bento) |
| Network | Global plan: UI does not fetch assets from network; chat uses local daemon | **Exception required:** lease enrollment/renewal over TLS to **lease server** (new component) |
| Server | `hush-relay` — queues only, no user table | New **`hush-lease`** (name TBD) — device registry, signing, flagging, admin |

---

## Overall acceptance criteria

- [ ] A `.cnf` (or chosen ext) file copied to another machine **cannot** be decrypted without that machine’s enrolled TPM + valid lease material.
- [ ] A **flagged** device **cannot** decrypt after the current lease expires (even offline).
- [ ] Patching or killing the client does **not** reveal the server-held key half without a valid lease verification path.
- [ ] All Confidential files share **one KEK** and **one lease** — unlock/lock together.
- [ ] **No plaintext** of Confidential content written to disk; documented leak paths (temp, sync, thumbnails) called out in UX.
- [ ] **Recovery path** documented and implemented **before** crypto-shred / flag-driven destruction ships.

### Honest limitations (ship in user-facing copy + `THREAT_MODEL.md` addendum)

- Root on a live, unlocked machine can read process memory.
- Displayed content can be screenshotted or exfiltrated by malware.
- TPM / disk failure without recovery key → **permanent loss** of Confidential data.
- Lease TTL = worst-case decryption window after theft if device was unlocked and leased.

---

## Phase 1 — Spec: container, key hierarchy, threat model

**Dependencies:** None (start here).

**Deliverables:**

- [ ] Container format spec (versioned): magic, version, salt/nonce, **key ID**, wrapped DEK, AEAD payload (filename + body inside ciphertext).
- [ ] Extension name decision (see open questions: `.cnf` vs `.hcf`).
- [ ] Key hierarchy diagram: DEK per file → wrapped by KEK → KEK split (TPM half + server half via lease).
- [ ] `docs/THREAT_MODEL.md` addendum: Confidential vs messaging vault; lease server trust; TPM limits; emulator refusal.
- [ ] ADR: where files live on disk (`vault_dir/confidential/` vs separate directory); relationship to Task **5c** logical vault.

**Acceptance criteria:**

- [ ] Two engineers can implement Phase 2 from the spec without guessing wire/layout bytes.
- [ ] Threat doc states what lease server may learn (device ID, renewal times, flag state).

**Checklist:**

- [ ] Byte layout + test vectors (encrypt sample, hex dump).
- [ ] Versioning and migration rule for format v1→v2.
- [ ] Confirm AEAD: **XChaCha20-Poly1305** (align with `hush-core`) or AES-256-GCM (document choice).

---

## Phase 2 — Crypto core (container read/write)

**Dependencies:** Phase 1.

**Planned code location:** `crates/hush-confidential/` (new) or `hush-core::confidential` module — prefer **new crate** to keep messaging vault separate.

**Deliverables:**

- [ ] Create / read / update container; DEK per file; KEK wrap/unwrap in header only.
- [ ] KEK rotation: re-wrap DEKs in headers **without** re-encrypting file bodies.

**Acceptance criteria:**

- [ ] Unit tests: round-trip; wrong KEK; truncated header; flipped ciphertext bit → clean failure.
- [ ] Property: ciphertext on disk ≠ plaintext (grep/strings tests).

**Checklist:**

- [ ] `cargo test -p hush-confidential`
- [ ] Zeroize DEK/KEK material in memory where feasible (`zeroize` crate, match `hush-core`).

---

## Phase 3 — TPM integration (Linux)

**Dependencies:** Phase 2 (KEK sealing interface defined).

**Deliverables:**

- [ ] Seal **KEK half A** (or full KEK envelope per Phase 1 ADR) to TPM 2.0; local sealed blob only.
- [ ] Optional TPM PIN / auth value; encrypted sessions for unseal (parameter encryption).
- [ ] **Minimal PCR policy**; document firmware/kernel update behavior.
- [ ] **Refuse** software TPM / vTPM for enrollment (detect and block or degrade to “Confidential unavailable”).

**Acceptance criteria:**

- [ ] Seal on machine A; unseal on A with valid auth; **unseal fails on machine B** with same blob.
- [ ] No TPM (or untrusted TPM): app does not claim hardware binding; Confidential disabled with clear message.

**Checklist:**

- [ ] Document deps: `tpm2-tools`, `tss-esapi` / `tpm2-pytss` evaluation for daemon-side integration.
- [ ] Pinning: which TPM objects (EK, SRK, signing key for renewal).

---

## Phase 4 — Lease server and protocol

**Dependencies:** Phase 1 (protocol spec); partially parallel with Phase 3.

**Planned code location:** `crates/hush-lease-server/` + `docs/CONFIDENTIAL_LEASE_PROTOCOL.md` (or section in PROTOCOL.md).

**Deliverables:**

- [ ] Enrollment: device TPM signing pubkey + **EK credential activation** proof.
- [ ] Renewal: TLS, device ID, server nonce, TPM signature (nonce ∥ lease serial, optional PCR quote).
- [ ] Lease JWT-like or custom signed blob: device ID, issue/expiry, serial, nonce, **encrypted server key half** to device TPM key.
- [ ] App-embedded **signing pubkey** for lease verification.
- [ ] Admin: flag device, emergency offline unlock (challenge → signed time-limited response).
- [ ] Rate limits, audit log (no file content, no KEK material).

**Acceptance criteria:**

- [ ] Integration test: enroll → renew → verify lease; flagged device denied renewal.
- [ ] Replay of old lease rejected (serial/nonce/expiry).

**Checklist:**

- [ ] Hosting model (same VPS as relay vs separate); TLS certs.
- [ ] Relation to future Stripe / paid tier (Task 8) documented.

---

## Phase 5 — Client lease logic (daemon)

**Dependencies:** Phases 3–4.

**Planned code location:** `hush-daemon` worker + `hush-confidential` library; **not** in Tauri webview (keys stay in daemon).

**Deliverables:**

- [ ] Hold valid lease before unsealing KEK; combine server half with TPM unseal.
- [ ] Background renewal at ~50% of TTL; renew on Confidential open when online; backoff.
- [ ] UI API: lease status (valid until, last renewal, offline warning).
- [ ] Clock rollback: last server time; evaluate TPM `PolicyCounterTimer` / monotonic policy per Phase 1.

**Acceptance criteria:**

- [ ] Expired lease → Confidential list visible but open fails with actionable error.
- [ ] Network block does not bypass lease check (no offline decrypt without emergency path).

**Checklist:**

- [ ] Daemon endpoints: `GET /api/confidential/lease`, `POST /api/confidential/renew`, etc. (names TBD).
- [ ] Does **not** require chat vault unlock, or **does**? (open question — see below).

---

## Phase 6 — Confidential section UI

**Dependencies:** Phases 2, 5 (minimal); Phase 3 for real hardware.

**Planned code location:** `apps/hush-tauri` — new rail `confidential`; daemon API only.

**Deliverables:**

- [ ] Rail + list of containers; create / rename (metadata in encrypted payload).
- [ ] In-app editor/viewer: decrypt in memory, save back to container; **no** external editor handoff.
- [ ] Lease status chip (valid until, renewing, expired, flagged).
- [ ] Optional: `inotify` watched folder → import drop as new container; export drag = container file only.

**Acceptance criteria:**

- [ ] Manual: create note, lock app, reopen after lease valid → content intact.
- [ ] UX warnings: sync folders, backups, indexing (link to help).

**Checklist:**

- [ ] Soft Daylight tokens/styles; icon in `icons.ts`.
- [ ] `API_GAPS.md` updated when APIs exist.

---

## Phase 7 — Stolen-device flow

**Dependencies:** Phases 4–5; **recovery key** from Phase 1/6 must exist first.

**Deliverables:**

- [ ] Server flag + confirmation (admin auth).
- [ ] On flagged + online: optional **crypto-shred** (delete sealed blob; optional delete `.cnf` files).
- [ ] Offline flagged device: lock when lease expires only.
- [ ] Offline **recovery key** procedure (documented; tested once).

**Acceptance criteria:**

- [ ] Flagged device with expired lease cannot open files in integration test.
- [ ] Recovery key restores access in drill (without weakening normal theft model).

**Checklist:**

- [ ] Legal/copy: mistaken flag is data loss.
- [ ] Recovery before shred ships (security rule).

---

## Phase 8 — Security review

**Dependencies:** Phases 1–7 complete.

**Scenarios:**

- [ ] Copied `.cnf` on second machine.
- [ ] Copied full disk image.
- [ ] Blocked network + expired lease.
- [ ] Patched client binary (lease verify still fails without server key half).
- [ ] Replayed lease blob.
- [ ] Clock rollback attempt.
- [ ] Root memory dump (document outcome, not “pass”).

**Acceptance criteria:**

- [ ] Written report in `docs/reviews/` or issue checklist; critical findings fixed or accepted with ADR.

---

## Security rules (all phases)

- No plaintext Confidential content on disk; no secrets in logs.
- Keys not stored next to ciphertext they protect.
- No homemade crypto; use audited Rust AEAD + TPM stack.
- Recovery path exists before destructive flag/shred features.

---

## Open questions and codebase conflicts

| # | Topic | Conflict / decision needed |
|---|--------|----------------------------|
| 1 | **Extension `.cnf`** | On Linux, `.cnf` often means “config file.” Prefer **`.hcf`** (Hush Confidential File) or **`.hush-cnf`** unless branding insists on `.cnf`. |
| 2 | **“No network UI” constraint** ([`milestones`](../plans/2026-10-04-v1-milestones.md) global) | Confidential **requires** TLS to lease server. Amend global constraints: *bundled UI only; lease API calls allowed to configured host.* |
| 3 | **Vault vs Confidential** | Existing **Vault** rail is security/settings bento (backup mock, storage). **Confidential** is a separate rail for TPM-bound files. Avoid user confusion in naming. |
| 4 | **Passphrase unlock vs lease** | Chat vault uses passphrase (`hush-daemon` unlock). Must decide: Confidential requires **both** vault unlock + lease, or **independent** unlock (TPM+lease only). |
| 5 | **Two server roles** | `hush-relay` must stay dumb (no user table). **Lease server** holds device IDs — new trust boundary; document in `THREAT_MODEL.md`. |
| 6 | **Task 5c encrypted vault** | Chat history + blobs use vault master key. Confidential containers should **not** reuse that key for hardware binding; separate KEK hierarchy. |
| 7 | **Plaintext `inbox/`** (current daemon) | Violates “no plaintext on disk” spirit for messaging; fix in **5c.2** before Confidential messaging overlap (e.g. “save to Confidential”). |
| 8 | **Unpaid users keep vault** ([`DESIGN.md`](../DESIGN.md)) | If lease server is paid-only, define free tier lease length or offline-only Confidential without server (likely **no** — conflicts with design). |
| 9 | **Recovery models** | DESIGN: recovery **code** for vault export. Confidential: separate **offline recovery key** for TPM loss — two recovery stories in Settings. |
| 10 | **Implementation language** | Spec mentions `cryptography` (Python); repo is **Rust**. Use `chacha20poly1305` / TPM Rust bindings only. |
| 11 | **Android / Tauri later** | TPM 2.0 story is Linux-first; mobile Confidential deferred. |
| 12 | **Split KEK + server half** | Clarify crypto construction (e.g. HKDF combine vs XOR) in Phase 1 — must survive review; blocking network must not equal “use half from disk.” |
| 13 | **Portable USB / TPM** | [`DISTRO_READINESS.md`](../DISTRO_READINESS.md): vault may move between machines; use **`KeyProvider`** with TPM **or** FIDO2-on-stick binding — do not assume single-machine TPM only. |

---

## Suggested scheduling (relative to v1 milestones)

- Do **not** start before **Task 5c.2** (no plaintext under vault) is underway — avoids two plaintext-on-disk patterns.
- **Phase 1–2** can proceed in parallel with **5c** once spec is approved.
- **Phase 4** (lease server) may align with **Task 8** (payments) if lease is a subscription surface.
- Full **Phase 6 UI** after dogfood **Task 5** chat path is stable.

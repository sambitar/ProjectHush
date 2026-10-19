# Places (desktop) Implementation Plan

**Goal:** Ship Settings → My places on the desktop app so a vault can adopt another vault as a Place (own account, own threads), vouch it, introduce it to contacts, and show a peer-side badge — per the settled D1–D17 in [`2026-10-18-android-linking.md`](2026-10-18-android-linking.md).

**Progress (2026-10-19):** Core vouch + adopt/detach/introduce + daemon routes + Settings → Places UI + contact badges landed on `feature/places-desktop`. PlaceAdopt still needs the adopted vault to add the minter's invite before decrypt (documented). Not committed until you ask.

**Architecture:** Each Place is a normal Hush account. The minting Place signs a vouch hop (XEd25519 over identity key, same primitive as queue grants). Hops chain up to 2. Adoption: existing Place scans the new vault's invite, adds it as a contact, signs the hop, stores a local Place record, and sends `PlaceAdopt` so the new vault auto-adds the minter and records "minted me". Introduce: opt-in `PlaceIntroduce` carries invite + chain to a chosen contact. Peers verify the chain against contacts they already trust and show a flat contact + badge.

**Tech Stack:** Rust (`hush-core`, `hush-daemon`), libsignal XEd25519, existing `MessagePayload` / chat recv path, Tauri UI (`app.ts`).

**Spec:** [`2026-10-18-android-linking.md`](2026-10-18-android-linking.md) D1–D17; this conversation.

## Global Constraints

- Never commit to `master`; work on `feature/places-desktop`.
- No shared vault master key; no multi-device / device-list resurrection.
- Chain cap = 2 hops. Detach is local only (D12). Badges refresh on a newer introduce (D14).
- Vouch signing uses the minting Place's identity key (same as queue-grant signing).
- Invite format: vouch travels as a **sidecar on the wire messages** (`PlaceAdopt` / `PlaceIntroduce`), not inside the `hush1…` pointer body (keeps QR short). Optional: later bake into invite if needed for in-person badge without a message.
- No bare `cargo fmt`. Prefer TDD at listed seams.
- Commit only when the user asks.

## File map

| File | Responsibility |
|---|---|
| `crates/hush-core/src/place.rs` *(new)* | `PlaceHop`, `SignedHop`, `VouchChain` sign/verify; chain continuity; trusted-anchor lookup |
| `crates/hush-core/src/error.rs` | `PlaceNotFound`, `InvalidPlace`, `PlaceChainTooLong`, `PlaceExists` |
| `crates/hush-core/src/payload.rs` | `PlaceAdopt`, `PlaceIntroduce` variants |
| `crates/hush-core/src/account.rs` | `PlaceRecord` in account state; `adopt_place`, `detach_place`, `introduce_place`, `places`; recv handlers |
| `crates/hush-core/src/lib.rs` | Export place types |
| `crates/hush-core/tests/places.rs` *(new)* | End-to-end adopt / introduce / badge / detach |
| `crates/hush-daemon/src/account_worker.rs` | Work variants + methods |
| `crates/hush-daemon/src/main.rs` | `/api/places` routes |
| `apps/hush-tauri/src/lib/api/types.ts` | DTOs |
| `apps/hush-tauri/src/lib/api/daemon.ts` | Client methods |
| `apps/hush-tauri/src/app.ts` | Settings → Places UI; contact badge |

## Seams (TDD)

1. **`VouchChain`** — pure sign/verify/anchor (unit tests in `place.rs`).
2. **`Account::adopt_place` / `detach_place` / `introduce_place`** — integration in `tests/places.rs` with two local accounts (same pattern as `multi_device` / two-party tests).
3. **Daemon routes** — smoke or worker-level if existing pattern is thin HTTP → worker.

---

### Task 1: Vouch chain codec and verify

**Files:**
- Create: `crates/hush-core/src/place.rs`
- Modify: `crates/hush-core/src/lib.rs`, `crates/hush-core/src/error.rs`
- Test: in-crate tests in `place.rs`

**Interfaces:**
- Produces: `MAX_PLACE_CHAIN_HOPS = 2`, `PlaceHop { minter_account_id, place_account_id, label, signed_unix }`, `SignedHop`, `VouchChain`, `VouchChain::sign_hop`, `VouchChain::verify`, `VouchChain::trusted_anchor(trusted_ids) -> Option<String>`
- Signing bytes: `hush-place-vouch-v1\n{minter}\n{place}\n{label}\n{unix}` (label may be empty)

- [ ] **Step 1: Write failing tests** — round-trip sign/verify; reject tamper; reject chain > 2; `trusted_anchor` returns earliest hop whose minter is in the trusted set; reject discontinuous chain.
- [ ] **Step 2: Run — expect FAIL** — `cargo test -p hush-core place::`
- [ ] **Step 3: Implement `place.rs` + error variants + export**
- [ ] **Step 4: Run — expect PASS**

---

### Task 2: Account state + adopt / detach

**Files:**
- Modify: `crates/hush-core/src/account.rs`, `crates/hush-core/src/payload.rs`
- Test: `crates/hush-core/tests/places.rs`

**Interfaces:**
- `PlaceRole::{MintedByMe, MintedMe}`
- `PlaceRecord { account_id, nickname, label, role, chain, added_unix }`
- `AccountState.places: Vec<PlaceRecord>`
- `Account::adopt_place(nickname, invite_str, label) -> Result<()>` — decode invite, `add_contact`, build chain (prepend own prior chain if MintedMe, cap 2), persist, send `PlaceAdopt` with our standing invite + chain
- `Account::detach_place(account_id)` — remove PlaceRecord only (contact stays)
- `Account::places() -> &[PlaceRecord]`
- Recv `PlaceAdopt`: verify chain anchors to sender; `add_contact_from_intro` or full add as appropriate; store MintedMe record

- [ ] **Step 1: Failing test** — two vaults; A adopts B; both have place records; B has A as contact; chains verify.
- [ ] **Step 2: FAIL → implement → PASS**
- [ ] **Step 3: Detach test** — A detaches B; A's places omit B; contact remains; B's record unchanged.

---

### Task 3: Introduce + peer badge verification

**Files:**
- Modify: `account.rs`, `payload.rs`
- Test: `tests/places.rs`

**Interfaces:**
- `MessagePayload::PlaceIntroduce { invite, chain, label }`
- `Account::introduce_place(place_account_id, contact_nickname) -> Result<()>`
- Recv: verify chain; pending or immediate add_contact with suggested nick from label; store peer-side `place_of: Option<String>` on Contact **or** a side map `place_badges: account_id -> anchor_account_id`
- Prefer a field on `Contact`: `#[serde(default)] place_of: Option<String>` (anchor account id). Cleared/replaced when a newer PlaceIntroduce from that introducer arrives (D14: replace that introducer's place-set — track `introduced_by` on the badge).

Simpler v1 for D14: each PlaceIntroduce from contact C about place P sets `Contact.place_of` for P to the trusted anchor, and records `place_introduced_by: C`. A later PlaceIntroduce from C that does **not** include P is out of scope unless we send a full set — v1: only positive introduces update badges; detach does not push. Document that D14 "newer introduce without place" means a follow-up introduce message type or re-introduce list later. **v1 D14 subset:** badge set on introduce; sticky until contact removed. (Full replace-set can be Task 3b if time.)

- [ ] **Step 1: Test** — A adopts B; A introduces B to C; C has B as contact with `place_of == A`.
- [ ] **Step 2: Implement → PASS**

---

### Task 4: Daemon API

**Files:**
- Modify: `account_worker.rs`, `main.rs`
- Routes:
  - `GET /api/places` → list
  - `POST /api/places/adopt` `{ nickname, invite, label }`
  - `POST /api/places/detach` `{ account_id }`
  - `POST /api/places/introduce` `{ place_account_id, contact_nickname }`

- [ ] **Step 1: Wire worker + routes following `/api/notes` pattern**
- [ ] **Step 2: `cargo test -p hush-daemon` / build**

---

### Task 5: Desktop UI

**Files:**
- Modify: `types.ts`, `daemon.ts`, `app.ts`

- Settings nav item **Places**
- List both directions (D17); Adopt (paste invite + label + nickname); Detach; Introduce (pick contact)
- Contact list: if `place_of` set, show muted chip `place of {nick}`

- [ ] **Step 1: API client + types**
- [ ] **Step 2: Settings page**
- [ ] **Step 3: Badge on contact rows**
- [ ] **Step 4: `npx tsc --noEmit` in `apps/hush-tauri`**

---

### Task 6: Docs close-out

- Update `2026-10-18-android-linking.md` status: D approved for desktop slice
- Note in `PROTOCOL.md` under Identity: Places (one paragraph)
- Fix stale multi-device line in milestones if touched

## Acceptance

- [ ] Two desktop vaults: adopt → both see My places; auto-contacts
- [ ] Introduce to a third vault → third sees contact + badge
- [ ] Detach removes from My places only
- [ ] `cargo test -p hush-core --test places` passes
- [ ] UI typechecks

## Self-review

- Vouch not in `hush1` body: intentional for QR size; in-person badge without a message needs a later invite flag — call out in PROTOCOL.
- D14 full "introduce omits place" replace-set deferred to a follow-up; sticky badge after introduce is the v1 behavior unless Task 3 lands the replace-set.

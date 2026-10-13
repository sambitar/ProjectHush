# Conclave Paper — collaborative note-taking (planning only)

**Status:** Spec resolved 2026-10-12 by grilling; not implemented. No code until a phase is
explicitly approved.

**Index:** New **Task 19** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md)
(Phase **M**). Extends the conclave model in [`PROTOCOL.md`](../PROTOCOL.md#conclaves); builds on
Task **16**'s sidecar-store patterns ([`notes-dashboard.md`](notes-dashboard.md)).

**Goal:** A conclave gets **Paper** — one shared, live collaborative document (Etherpad-like) that
the joined trustees edit together during a meeting. Every keystroke is E2E-encrypted, fanned out
over the trustees' existing 1:1 Signal sessions exactly like a conclave message; when a direct
channel is available, edits converge in **sub-second** over a WebRTC DataChannel, otherwise they
converge in **~1–3 s** over the relay. The document persists in **every trustee's vault**, like the
conclave's message log. There is no server, no Etherpad code, and the relay sees only ciphertext.

**Name:** **Paper** (product name, lowercase in prose). It competes in the direction of "the
meeting's shared scratchpad", not Zoom.

**Non-goals (this task):** voice/video (Task **17**, postponed); screen share (Task **18**);
shared reading + discussion (Task **20**, its own task — a sibling that reuses this spine);
multiple papers per conclave; Timeslider-style history scrubbing UI (a later phase — the CRDT log
makes it free data, the UI is not built now); real Etherpad embedding (rejected — Section 1);
guests/non-trustees; a Hush-hosted sync server (never).

---

## 1. Why not embed actual Etherpad

Etherpad (Apache-2.0, since 2009) is a **Node.js server**: pads live **in plaintext in the server's
database** (Postgres in production), clients load the editor UI from that server over HTTP, and all
real-time sync is **server-authoritative OT** ("Easysync" changesets composed against a pad
revision). Studied 2026-10-12 (README + `ui/` layout + easysync docs).

| Concern | Etherpad default | Hush requirement |
|---|---|---|
| Where pads live | Server database, **plaintext** | Only in each trustee's **encrypted vault** |
| Sync authority | The **server** composes changesets | **No authority for text** — a CRDT merges peer-side; the only authority in a conclave is the convener's, and only over membership |
| Editor delivery | HTTP from the server | **Never fetched from the network** — local assets only |
| Runtime | Node.js >= 24 + database | Rust daemon + the existing Tauri webview; no second runtime |

What Etherpad contributes is the **experience and the model**, which this spec adopts:

- **Authorship is the default** — every keystroke attributed, author colours, named live cursors.
- **The document model** — flat text plus character/line attributes (bold on a range, heading on a
  line). A CRDT text with marks expresses this directly.
- **A pad is a persistent shared object**, not a message stream — the paper outlives the meeting
  inside every vault, exactly the way conclave messages already do.

**Rejected outright:** embedding or bundling Etherpad — it would put members' notes in plaintext on
one participant's machine, break the no-network-UI rule, and add a Node.js runtime to a Rust
product.

---

## 2. Decisions locked (2026-10-12)

| Decision | Choice |
|---|---|
| What it is | **Hush-native** collaborative document; Etherpad's feel, none of its architecture |
| Sync engine | **CRDT** (`yrs`, Rust) in the **daemon**; the webview renders and sends edits |
| Liveness | **Sub-second via WebRTC DataChannel**, pulled forward from Task 17/18 as a **data-only** stack (no GStreamer, no media); **relay fan-out fallback** at ~1–3 s convergence |
| Persistence | **Per-vault**, like conclave messages — per-trustee copies, no shared history; a locked vault cannot read it |
| Scope | **One paper per conclave**, creatable by **any joined trustee** (first creator wins) |
| Editor (v1) | **Basic rich text**: bold / italic / underline, lists, headings; **author colours**; **named live cursors** |
| Hub | The **paper's creator** is the DataChannel hub (star, Task 18 pattern); hub is only a rendezvous point — text merges anywhere |
| Hub loss (temporary) | Editing **degrades to relay cadence**; direct liveness resumes when the creator returns; no election |
| Hub loss (permanent) | Creator **left/kicked** → the **convener inherits the hub** by default (convener-authoritative `PaperHub`, like a roster update); creator still a member but gone forever → relay cadence indefinitely, convener may **re-home** the paper explicitly |
| Conclave ends | Papers go **read-only** in every vault (same as conclave messages) |
| Late joiner | The hub sends the **current document snapshot** on join — live state, not history |
| Lock mid-editing | *That* trustee's unsent deltas queue locally and flush on unlock; everyone else unaffected |
| Milestone | Task **19** / Phase **M** |

---

## 3. Architecture

Two channels for the same CRDT, chosen per-peer by availability — never a mode the user picks:

```mermaid
flowchart LR
  subgraph A["Trustee A (paper creator = hub)"]
    DA["hush-daemon A\nyrs doc + fan-out"]
  end
  subgraph B["Trustee B"]
    DB["hush-daemon B\nyrs doc"]
  end
  subgraph C["Trustee C"]
    DC["hush-daemon C\nyrs doc"]
  end
  R["hush-relay\n(ciphertext only)"]
  DA <==|"DataChannel: yrs updates + presence\n(sub-second, DTLS, P2P)"==> DB
  DA <====> DC
  DA -.->|"PaperDelta fan-out\n(1:1 Signal sessions)"| R
  DB -.-> R
  DC -.-> R
  R -.-> DA
```

- **Fast path** — each trustee's daemon opens one **WebRTC DataChannel** (SCTP over DTLS) to the
  hub's daemon. yrs binary updates and presence (cursor, colour, name) ride it. **Data-only**: no
  GStreamer, no PipeWire, no media pipeline — the Task 17 WebKitGTK constraint still applies (the
  peer connection lives in the daemon, never the webview), but none of Task 17's media work is
  needed.
- **Slow path** — the same yrs updates ride `MessagePayload` variants fanned out per joined trustee
  over the existing Signal sessions, store-and-forward through the relay, exactly like conclave
  chat. While a paper is open, the UI polls `/api/recv` faster (1 s instead of 4 s; `RECV_POLL_MS`
  is already the only cadence knob). Deltas are debounced (~300–500 ms) daemon-side before fan-out.
- **Merge** — yrs is CRDT: order and duplication do not matter, both paths can run at once, and the
  doc converges everywhere. The hub has **no merge authority**; it exists so peers have one address
  to point DataChannels at.
- **Storage** — `papers.enc` encrypted sidecar in the vault (the `notes-state.enc` pattern),
  holding per-conclave paper state: descriptor, doc snapshot, and the update log. Written with
  `Vault::write_encrypted_file` / `read_encrypted_file`. The conclave's `ConclaveRecord` gains a
  `paper: Option<PaperRef>` so the UI knows a paper exists without loading the doc.
- **Editor** — ProseMirror + `y-prosemirror` in the webview, bound to a local Yjs doc that is
  wire-compatible with the daemon's `yrs` doc; binary updates are exchanged over the daemon API
  (`POST /api/paper/…/update`, fast poll or SSE for inbound). Toolbar: bold, italic, underline,
  H1–H3, bullet list, numbered list. Author colours from a fixed palette keyed by `account_id`;
  named cursors from presence. All assets local — the UI never fetches from the network.

### Components

| Piece | Where | Responsibility |
|---|---|---|
| Paper wire payloads | `crates/hush-core/src/payload.rs` | `PaperState`, `PaperDelta`, `PaperSnapshot`, `PaperPresence`, `PaperHub` (Section 4) |
| Fan-out + routing | `crates/hush-core/src/account.rs` | `send_conclave_paper_*` beside `send_conclave_message`; route inbound paper payloads to the paper store |
| Paper store (sidecar) | `crates/hush-daemon/src/paper_store.rs` (new) | Per-conclave doc snapshot + update log + descriptor; caps; the `notes_store.rs` pattern |
| CRDT doc + merge | `yrs` dep in `crates/hush-daemon` (or `hush-core`) | Apply local/remote updates; compact log into snapshots; produce snapshots for late joiners |
| DataChannel plane | `crates/hush-daemon/src/paper_data.rs` (new) | Pure-Rust WebRTC (`webrtc-rs`), DataChannel only; SDP/ICE signalling as `MessagePayload` variants over the existing sessions |
| Daemon endpoints | `crates/hush-daemon/src/main.rs`, `account_worker.rs` | `/api/paper/*` (Section 5) |
| Editor UI | `apps/hush-tauri/src` (`app.ts`, `lib/paperStore.ts`, ProseMirror deps) | Paper tab in the conclave view, toolbar, cursors, live/degraded badge |

**Two WebRTC stacks, stated honestly.** Paper uses a pure-Rust stack (`webrtc-rs`) for
DataChannel-only connections so it does not pull GStreamer into a text feature. Task 17/18 will use
GStreamer `webrtcbin` for media. Consolidating on one stack is a Task 18 decision (media needs
`webrtcbin`; Paper could migrate); until then the stacks share the **STUN configuration** via the
single `ClientConfig` (DR-14 — no second config system).

---

## 4. Wire format (proposed)

New `MessagePayload` variants in `crates/hush-core/src/payload.rs`, fanned out per joined trustee
over their 1:1 Signal sessions exactly like `Conclave`. The relay never sees anything but the
existing opaque ciphertext.

```rust
pub enum MessagePayload {
    // ... existing ...
    // Task 19 — Paper:
    /// Creator-authoritative at creation; convener-authoritative for `hub`.
    PaperState { paper: PaperDescriptor },
    /// Opaque yrs update (binary, base64 on the wire). No authority — merge anywhere.
    PaperDelta { paper_id: String, update: Vec<u8> },
    /// Full current doc state. Sent by the hub to a newly joined trustee,
    /// or as relay catch-up when a peer's revision is too far behind.
    /// Carries the descriptor: the trustee that needs one joined after
    /// `PaperState` went out, so it has no record to place the state against.
    PaperSnapshot { paper: PaperDescriptor, state: Vec<u8> },
    /// Presence over the slow path is NOT sent in v1 (DataChannel-only),
    /// reserved here so the slow path can carry coarse presence later.
    PaperPresence { paper_id: String, peer: String, state: Vec<u8> },
    /// Convener-authoritative hub succession / re-home.
    /// Accepted only when sent by the convener named in the descriptor.
    PaperHub { paper: PaperDescriptor },
    /// A tag this build does not know, from one that does. Decoded, never sent.
    Unknown,
}

pub struct PaperDescriptor {
    pub paper_id: String,      // random UUID, never exposed to the relay as a name
    pub conclave_id: String,
    pub creator: String,       // account id
    pub hub: String,           // account id of the current hub
    pub title: String,         // defaults when the sender omits it
    pub created_ms: u64,
}
```

**Implemented 2026-10-12** (`crates/hush-core/src/payload.rs`). Deviations from the sketch above,
all deliberate: the binary fields carry `#[serde(with = "b64_bytes")]` rather than a JSON number
array (the house codec, same as `File`/`MailAttachment`); `Unknown` is a `#[serde(other)]` unit
variant; and `title` was added to the descriptor so the wire and the daemon's at-rest record can be
one type instead of two that drift.

**Amended 2026-10-12** (`PaperSnapshot` now carries the descriptor, not a bare `paper_id`). The
sketch above was wrong and the dogfood proved it: a snapshot is only ever needed by a trustee that
joined *after* the paper existed, so it is exactly the peer that never received the `PaperState`
fan-out. With a bare `paper_id` the payload named a paper the receiving vault had no record of,
and `paper_apply_inbound` dropped it — "no paper &lt;id&gt; in this vault" — so the hub's catch-up
could never land. Carrying the descriptor makes the payload self-contained and order-independent.
A snapshot for a paper the vault already holds merges only the state and keeps the local
descriptor, so a peer cannot retitle or re-home a paper through one; a snapshot that introduces an
unknown paper is accepted only from the creator it names, the same authority rule `PaperState`
enforces.

- **Authority model mirrors the conclave's:** `PaperState`/`PaperHub` are applied only when sent by
  the convener (for `hub`) or the paper's creator (for creation); `PaperDelta` is authority-free
  (CRDT); a `PaperSnapshot` is applied when it advances the local doc.
- **Backward compatibility:** unknown payload tags must be ignored cleanly. This is the same
  tolerant-decode change Task 17 Phase 1 plans (`Error::Corrupt` → `Unknown` fallback today);
  whichever task lands first **brings that one shared change**.
- **No relay change.** `encode_relay_blob`/`decode_relay_blob` are untouched. Deltas are small
  (tens of bytes per debounced batch), far under the 1 MiB cap.

---

## 5. Daemon API (proposed)

Socket-first, thin-UI, following the notes/reads pattern (`account_worker.rs` owns the Vault; routes
in `main.rs`).

| Route | Body | Returns / notes |
|---|---|---|
| `GET /api/paper/{conclave_id}` | — | Descriptor + local revision; 404 before creation; **401 when locked** |
| `POST /api/paper/{conclave_id}/create` | `{title?}` | First creator wins; **409** if the paper exists |
| `POST /api/paper/{conclave_id}/update` | binary yrs update | Daemon applies, persists (debounced sidecar write), fans out, serves peers |
| `GET /api/paper/{conclave_id}/updates?since=rev` | — | Inbound updates since revision (fast-poll while the editor is open; SSE is a later concern) |
| `GET /api/paper/{conclave_id}/state` | — | Full doc state for the editor's initial load |
| `POST /api/paper/{conclave_id}/hub/rehome` | `{new_hub}` | Convener only |

DataChannel management (offer/answer/ICE relay, hub lifecycle) is daemon-internal; the UI sees only
a status flag per conclave: `live` (DataChannel up) / `degraded` (relay cadence) — an honest badge,
not a hidden failure.

---

## 6. Threat model delta

Add to [`THREAT_MODEL.md`](../THREAT_MODEL.md) when implementing:

| New surface | What it may see | Rule |
|---|---|---|
| Relay | Same as today: ciphertext, size, time, queue id, IP. Paper adds **more, smaller, more frequent blobs** while a paper is open | **No change** to what is visible; the *timing* signal is richer — accepted, same as any live feature; padding is out of scope |
| STUN server | Querying IP + port | Self-hosted, same as Task 17. **No TURN** — a peer behind symmetric NAT simply falls back to relay cadence |
| Hub daemon | Each peer's IP (ICE), and doc updates (it merges and re-fans-out) | **Accepted** — trustees are mutually trusted; the hub sees no more plaintext than any trustee already holds in their own vault |
| DataChannel peers | Each other's IP | Same accepted limit as Task 17/18, stated in copy |
| Doc bytes at rest | Nothing in plaintext | `papers.enc` is sealed under the vault master key; locked vault = unreadable; editor runs from local assets only |
| Persistence | The document **persists in every vault by design** (Etherpad's core value, and the same persistence conclave messages already have). Deleting is local, per vault, like an ended conclave | Copy states this plainly — Paper is not "leaves nothing behind" like a call; it is "stays in the trustees' vaults" like conclave chat |

**Honest limits for user-facing copy:** collaboration reveals IPs to trustees; the relay learns
*that* (and roughly when) a conclave is editing, though never what; a kicked trustee keeps the last
copy of the paper their vault already held, exactly as they keep conclave messages.

---

## 7. Phases and acceptance criteria

### Phase 0 — Spec and threat model (this document)
- [x] Decisions in Section 2 resolved and recorded (2026-10-12).
- [x] `THREAT_MODEL.md` addendum (Section 6) when implementation starts — landed 2026-10-12 with Task 8.

### Phase 1 — Paper core, offline (daemon + editor, no network) — landed 2026-10-12
- [x] `paper_store.rs` sidecar: descriptor + doc snapshot + update log, caps (doc ~256 KiB soft cap, snapshot ~512 KiB; refuse with a reason, the notes pattern), round-trip + corruption tests.
- [x] `yrs` doc owned by the daemon; `/api/paper/*` endpoints; lock = 401 everywhere.
- [x] Webview: Paper affordance in the conclave view (tab beside chat), ProseMirror editor bound through a local Yjs bridge; toolbar (bold/italic/underline, lists, headings); author colour is the local user's.

**Verify:** create + edit a paper in one vault; restart the app — the document survives; lock the vault — every paper route 401s and `papers.enc` on disk is ciphertext; `cargo test -p hush-daemon`. Verified 2026-10-12 — see the Task 5 dogfood record in
[`docs/plans/2026-10-12-conclave-paper.md`](../plans/2026-10-12-conclave-paper.md).

### Phase 2 — Wire fan-out (relay cadence end-to-end)
- [x] Payload variants + tolerant unknown-tag decode (shared change with Task 17 Phase 1) — landed 2026-10-12; the shared change is done, so Task 17 Phase 1 no longer brings it.
- [x] `send_conclave_paper_*` fan-out in `account.rs`; inbound routing; UI poll interval 1 s while the editor is open (`PAPER_POLL_MS = 1000`). The delta debounce landed on the **editor** side, not the daemon: `paperStore.ts` batches keystrokes and merges them (`Y.mergeUpdates`) into one `POST` every `PAPER_FLUSH_MS = 400`, which is the 300–500 ms window the plan asked for. The daemon fans out per `POST`, so a non-UI caller has no coalescing.
- [x] Late-joiner snapshot: hub sends `PaperSnapshot` when a `ConclaveAccept` lands for a conclave that has a paper. The payload carries the descriptor — a bare `paper_id` was unplaceable on the newcomer that needed it (amended 2026-10-12, Section 4).

**Verify:** `./scripts/paper-dogfood.sh` — three daemons and a real relay: Alice creates the paper; Bob converges on her edit; Bob's edit reaches Alice with both documents byte-identical; Carol joins late and the hub's snapshot catches her up; `relay.db`, all three vaults, and every other file on disk hold no paper plaintext. Verified 2026-10-12. This is a daemon-level dogfood, not a pass in the webview — see the Task 4 note in the milestone plan for the limits.

### Phase 3 — DataChannel liveness — landed 2026-10-12
- [x] Spike first: prove `webrtc-rs` DataChannel between two daemons over a LAN before the rest commits (offer/answer/ICE as `MessagePayload` variants). Proved over loopback with no STUN server; a real LAN run is still open (the code does not depend on the answer, because the relay fans out underneath in every case).
- [x] Hub = creator; peers open one DataChannel each; updates + presence ride it when up. The hub re-broadcasts what it receives, because the fast path is a star while the relay fan-out is a mesh.
- [x] Automatic per-peer path selection (fast when the channel is up, relay otherwise) with the live/degraded badge. `Sent::UseRelay` is a normal answer rather than an error, so the relay stays the guarantee.

**Verify:** two daemons on a LAN converge sub-second; kill the hub's channel — peers degrade to relay cadence and keep merging; hub returns — liveness resumes with no user action. Sub-second convergence is proven on loopback (957 µs for one update against the relay's 1–3 s); the LAN half and a real symmetric NAT remain open, with degradation reached by simulating unreachability.

### Phase 4 — Presence, succession, hardening — landed 2026-10-12
- [x] Author colours + named live cursors (ProseMirror decorations fed by DataChannel awareness); cursor presence is DataChannel-only in v1 (nothing over the relay). The label is resolved on receipt from the sender's account id against the receiver's own roster, so a caret cannot be labelled after someone else.
- [x] Hub succession: creator leaves/kicked → convener-authoritative `PaperHub` (default: the convener); explicit re-home route for the convener. The authority rule lives in `hush-core`, so there is no second copy of the roster to drift.
- [x] Lock/suspend mid-editing: local delta queue flushes on unlock; a locked **hub** degrades peers cleanly. Caps enforced with reasons. This one was a real gap: `closePaper()` claimed to flush and did not, and `lock()` called the API first, so an edit inside the 400 ms window was lost twice over.

**Verify:** three-way edit with distinct colours/cursors; kick the creator mid-session — the convener becomes hub and liveness resumes; lock the hub — peers degrade; a locked editor's queued edits land on unlock and merge cleanly. Verified 2026-10-12 by `./scripts/paper-dogfood.sh` (23 checks), which drives the daemon API with the editor's own bytes rather than typing in a window.

### Phase 5 — Polish — landed 2026-10-12
- [x] Toolbar completeness check against the v1 scope; empty state ("Start the paper"); read-only rendering after `ConclaveEnd`.
- [x] Error states (NAT-limited, hub gone, caps) with actionable copy.
- [x] `DISTRO_READINESS.md` updates: `yrs` + `webrtc-rs` dependencies, STUN config surface, sandbox notes (no new devices; SCTP egress).

**Verify:** full three-daemon dogfood session, including an ended conclave rendering read-only papers in every vault. Verified 2026-10-12. `cargo deny` / `cargo audit` were **not** run — neither tool is installed in this environment; adding them to CI is separate work.

---

## 8. Scheduling

- **Before Task 17** — the user explicitly postponed voice/video; Paper deliberately does **not**
  wait for the media plane (data-only WebRTC, pure-Rust stack).
- **Phase 1 is network-free** and can start as soon as approved; Phases 2–3 sequence after it.
- **Depends on:** conclave fan-out (done), 5h presence (done), 5c encrypted store (done) — all
  landed. The only shared unlanded piece is the tolerant unknown-tag decode, which Paper brings if
  it lands before Task 17 Phase 1.
- **Independent of:** mail (5d), notes (16 — Paper is collaborative and conclave-scoped; Vault
  notes stay private and local; no shared code beyond the sidecar pattern), articles (11),
  confidential (10).

## 9. What not to build

- Real Etherpad (server, OT, Node runtime) — Section 1.
- Multiple papers per conclave; cross-conclave papers.
- A Timeslider/history UI (the CRDT log keeps the data; the UI is a later phase).
- Presence/cursors over the relay in v1.
- First-party TURN, STUN other than self-hosted, or any sync server.
- Recording, transcription, or export formats beyond the existing vault backup (which carries
  `papers.enc` automatically).

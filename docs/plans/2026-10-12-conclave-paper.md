# Conclave Paper Implementation Plan

**Goal:** One shared, live, E2EE collaborative document ("Paper") per conclave — sub-second over a
DataChannel, ~1–3 s over the relay fallback, persisted per vault.

**Architecture:** `yrs` CRDT owned by `hush-daemon` (sidecar `papers.enc`); webview edits through a
ProseMirror/Yjs bridge exchanging binary updates over the daemon API; new `MessagePayload` paper
variants fanned out per trustee over the existing 1:1 Signal sessions; pure-Rust WebRTC
(`webrtc-rs`) DataChannel to the paper creator's daemon as the fast path.

**Tech Stack:** Rust (`yrs`, `webrtc-rs`), axum daemon API, Tauri webview (ProseMirror +
`y-prosemirror`, local assets only).

**Spec:** [`docs/tasks/conclave-paper.md`](../tasks/conclave-paper.md) — decisions locked
2026-10-12 by grilling.

## Global Constraints

- One paper per conclave; creatable by any joined trustee; convener authority for hub succession.
- Relay must never gain new abilities or see plaintext; no TURN; STUN self-hosted and configured
  via `ClientConfig` only (DR-14 — no second config system).
- UI never fetches editor assets from the network; daemon owns state and wire (thin-UI doctrine).
- Lock/suspend: all paper routes 401; a locked hub degrades peers; unsent local deltas flush on
  unlock.
- Caps refuse with a reason rather than silently dropping (the `notes_store` pattern).
- Unknown payload tags decode to a tolerated `Unknown`, never `Error::Corrupt` (shared with
  Task 17 Phase 1 — whichever lands first brings it).
- Work on a feature branch; tests on core/daemon before UI polish.

---

### Task 1: Paper sidecar store (`paper_store.rs`)

**Files:**
- Create: `crates/hush-daemon/src/paper_store.rs`
- Modify: `crates/hush-daemon/src/main.rs` (module declaration only)
- Test: `crates/hush-daemon/src/paper_store.rs` (`#[cfg(test)]`, the `notes_store.rs` pattern)

**Interfaces:**
- Consumes: `Vault::write_encrypted_file` / `read_encrypted_file` (`crates/hush-core/src/vault.rs`)
- Produces: `PaperStore { papers: Vec<PaperFile> }` with `PaperFile { conclave_id, descriptor,
  snapshot: Vec<u8>, log: Vec<UpdateEntry> }`; `decode(bytes: Option<&[u8]>) -> Result<PaperStore,
  String>`, `create(...)`, `apply_update(...)`, `compact(...)`, each enforcing caps and returning
  reason strings.

- [x] **Step 1: Write failing tests** — round-trip `encode`/`decode`; create conflict (second
  `create` for the same `conclave_id` errors); doc soft-cap 256 KiB refuses with a reason; snapshot
  cap 512 KiB refuses; corrupted bytes decode to a clean `Err`, not a panic.
- [x] **Step 2: Run — expect FAIL**: `cargo test -p hush-daemon paper_store`
- [x] **Step 3: Implement** `paper_store.rs` mirroring `crates/hush-daemon/src/notes_store.rs`
  (state struct + `encode`/`decode`, caps enforced in methods).
- [x] **Step 4: Run — expect PASS**: `cargo test -p hush-daemon paper_store`

### Task 2: `yrs` doc ownership + `/api/paper/*` daemon endpoints

**Files:**
- Modify: `crates/hush-daemon/Cargo.toml` (add `yrs`)
- Create: `crates/hush-daemon/src/paper_doc.rs` (yrs doc + apply/compact/snapshot helpers)
- Modify: `crates/hush-daemon/src/main.rs`, `crates/hush-daemon/src/account_worker.rs` (routes +
  `WorkResult` variants, the reads/notes pattern)
- Test: `crates/hush-daemon/src/paper_doc.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `paper_store.rs` (Task 1)
- Produces: `GET /api/paper/{conclave_id}`, `POST /api/paper/{conclave_id}/create`,
  `POST /api/paper/{conclave_id}/update` (binary), `GET /api/paper/{conclave_id}/updates?since=rev`,
  `GET /api/paper/{conclave_id}/state` — all 401 when locked, 404 before creation, 409 on
  double-create.

- [x] **Step 1: Write failing test** — two independent `yrs` docs apply each other's update bytes
  and converge to identical text; applying a snapshot advances revision; compaction collapses the
  log without changing the doc.
- [x] **Step 2: Run — expect FAIL**: `cargo test -p hush-daemon paper_doc`
- [x] **Step 3: Implement** `paper_doc.rs`; wire routes through `account_worker.rs` so all vault
  access stays behind the worker (`WorkResult` variants like `reads.rs`).
- [x] **Step 4: Run — expect PASS**: `cargo test -p hush-daemon` (full crate — lock/401 paths
  included)

### Task 3: Wire payloads + tolerant decode

**Files:**
- Modify: `crates/hush-core/src/payload.rs` (`PaperDescriptor`, `PaperState`, `PaperDelta`,
  `PaperSnapshot`, `PaperPresence`, `PaperHub` variants; `Unknown` fallback on unknown tags)
- Test: `crates/hush-core/src/payload.rs` (`#[cfg(test)]`, existing payload tests pattern)

**Interfaces:**
- Produces: encodable/decodable paper payloads; unknown-tag bytes decode to `MessagePayload::Unknown`
  instead of `Error::Corrupt`.

- [x] **Step 1: Write failing tests** — round-trip each variant; a payload tagged with an unused
  tag decodes to `Unknown`; `PaperHub` carries the descriptor so receivers can check the sender is
  the convener.
- [x] **Step 2: Run — expect FAIL**: `cargo test -p hush-core payload` — 11 compile errors
  (the variants and `PaperDescriptor` did not exist).
- [x] **Step 3: Implement** variants + tolerant decode (the one shared change Task 17 Phase 1 also
  wants — coordinate, do not duplicate). **Landed 2026-10-12**; `docs/tasks/member-calls.md` now
  points at this instead of planning it again.
- [x] **Step 4: Run — expect PASS**: `cargo test -p hush-core` — 140 passed, 23 of them in
  `payload`; `cargo test --workspace` — 365 passed across 25 binaries, 0 failed.

**Implementation notes (2026-10-12):**
- `PaperDescriptor` is defined **once**, in `payload.rs`, and re-exported by the daemon's
  `paper_store` — it rides the wire (`PaperState`, `PaperHub`) *and* is the at-rest record, so a
  second copy would have been the thing that drifted. The move is wire- and disk-compatible: the
  JSON shape is unchanged, pinned by `a_sidecar_written_before_the_descriptor_moved_still_loads`
  and by loading the real `instances/peer-a/papers.enc` written by the pre-move build.
- Binary fields (`update`, `state`) use the house `b64_bytes` codec, not the JSON number arrays the
  Section 4 sketch showed — same as `File`/`MailAttachment`/`ConclaveFile`.
- Tolerant decode is `#[serde(other)]` on a unit `Unknown` variant. Two boundaries are pinned by
  tests: an unknown *tag* is tolerated and must **not** fall through `decode_legacy_payload` into a
  bogus `Text` chat message, while a known tag with a malformed body is still `Error::Corrupt`.
- Adding the variants surfaced three exhaustive matches (`account.rs::recv`, the daemon's
  `received_dto`, `hush-cli`): each now names the paper variants explicitly, and none routes them
  into the `unreachable!` control-payload arm. In `recv` they are skipped without deleting the blob
  until Task 4 routes them, so the gap between the two tasks loses nothing; `Unknown` is deleted,
  since this build can never interpret it and leaving it would re-deliver it forever.
- Inbound routing into the paper store is **not** in this task (that is Task 4); the arms above
  exist only so the workspace compiles and nothing panics.

### Task 4: Fan-out + inbound routing (relay cadence)

**Files:**
- Modify: `crates/hush-core/src/account.rs` (`send_conclave_paper_delta`, `send_conclave_paper_state`,
  `send_conclave_paper_snapshot` beside `send_conclave_message` at line ~1534; inbound routing in
  `recv` at line ~2453)
- Modify: `crates/hush-core/src/chat_log.rs` (`ConclaveRecord.paper: Option<PaperRef>` with
  `#[serde(default)]` for old records)
- Modify: `crates/hush-daemon/src/account_worker.rs` (debounced delta fan-out while a paper is
  open; apply inbound deltas/snapshots; hub sends `PaperSnapshot` on `ConclaveAccept` for a
  conclave with a paper)
- Test: `crates/hush-core/src/account.rs` tests (two-account fixture, the existing conclave test
  pattern)

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: two daemons converge a shared paper via the relay alone.

- [x] **Step 1: Write failing test** — Alice creates a paper and edits; Bob's `recv` applies the
  deltas; both docs converge; a kicked/late trustee receives `PaperSnapshot` from the hub.
- [x] **Step 2: Run — expect FAIL**: `cargo test -p hush-core account`
- [x] **Step 3: Implement** senders + inbound routing. The debounce is **not** daemon-side, and
  deliberately so: the editor already coalesces keystrokes before they leave the webview
  (`PAPER_FLUSH_MS = 400` in `paperStore.ts`, batching into `Y.mergeUpdates` and one `POST`), which
  is the 300–500 ms window this step asked for. A second timer in the daemon would add shared state
  and concurrency for no fewer relay blobs. A non-UI caller that posts every keystroke still fans
  out per call — nothing coalesces that today.
- [x] **Step 4: Run — expect PASS**: `cargo test -p hush-core -p hush-daemon`
- [x] **Step 5: Verify**: `./scripts/paper-dogfood.sh` — three daemons and a real relay: Alice
  creates the paper, Bob converges on her edit, Bob's edit reaches Alice with both documents
  byte-identical, Carol joins late and the hub's snapshot catches her up, and no file left on disk
  (relay.db, every vault, every log) holds the text in the clear.

**Done 2026-10-12**, with one fix the dogfood forced. `PaperSnapshot` carried a bare `paper_id`, so
the catch-up it exists for was dropped by the late joiner as "no paper &lt;id&gt; in this vault" — the
one peer that needs a snapshot is the one that never saw the `PaperState` fan-out. It now carries
the descriptor, which makes it self-contained; see the amendment in
[`conclave-paper.md`](../tasks/conclave-paper.md#4-wire-format-proposed).

Two limits on that verification, stated plainly:

- The dogfood drives the daemon API with the bytes the editor sends (the UI's own `yjs`, an
  XmlFragment named `prosemirror`, via `scripts/paper-edit.mjs`). It is **not** a pass in the
  webview: nobody has typed in two windows and watched them converge, because the Tauri webview
  cannot be driven headlessly here. The editor itself is Task 5 and is verified separately.
- It covers two trustees plus one late joiner, not the plan's three simultaneous editors, and it
  does not exercise a locked vault mid-edit or a hub that disappears (Phase 4).

### Task 5: Webview editor (ProseMirror + Yjs bridge, Paper tab)

**Files:**
- Modify: `apps/hush-tauri/package.json` (`prosemirror-*`, `y-prosemirror`, `yjs` — local build only)
- Create: `apps/hush-tauri/src/lib/paperStore.ts` (bridge: local Yjs doc ↔ daemon `updates`/`state`;
  follow `lib/notesStore.ts` + `lib/api/daemon.ts` patterns)
- Modify: `apps/hush-tauri/src/app.ts` (Paper tab in the conclave view, toolbar, author colour,
  live/degraded badge stub), `apps/hush-tauri/src/lib/api/types.ts`, `apps/hush-tauri/src/lib/constants.ts`
  (`PAPER_OPEN_POLL_MS = 1000`)

**Interfaces:**
- Consumes: Task 2 endpoints.
- Produces: editable rich-text paper (bold/italic/underline, lists, H1–H3) that survives restart
  and degrades visibly.

- [x] **Step 1:** bridge unit logic (update envelope encode/decode, revision tracking) in a small
  test file runnable with the existing UI test setup; then manual two-window verify per the repo's
  browser-tools rule.
- [x] **Step 2:** implement the tab + editor + bridge; the editor sends debounced updates, applies
  inbound ones; lock clears the pane.
- [x] **Step 3: Verify:** create/edit across two app instances converges; restart persists; locked
  vault shows nothing.

**Dogfood record — 2026-10-12 (Phase 1, single instance):** Run against the live two-peer dev stack
(Alice unlocked, `instances/peer-a`) rather than a unit harness — the app has no test setup to run
bridge logic in, so Step 1 is still open as written. Verified end to end: the paper was created
through the UI ("Start paper"), the `Chat | Paper` pill and the 9-button toolbar render with correct
`aria-pressed` state, typing flushed to the daemon (`/api/paper/{id}` reached revision 31), a
**fresh client rebuilt the exact text from the daemon's state** (`<paragraph>Phase 1 dogfood…`), the
revision stayed flat while idle (no flush loop), and `papers.enc` on disk held no plaintext.
Two bugs surfaced that a green suite had missed and are fixed, with regression tests on both sides —
see `DEEPSEEKPROBLEMS.MD` (2026-10-12, "two bugs the tests could not see"): a zero-byte state broke
`Y.applyUpdate` on a never-edited paper, and a temporal-dead-zone throw in the editor's constructor
blanked the whole window.
Still unverified: two-instance convergence (needs Phase 2 fan-out — Phase 1 is local by design) and
the "don't repaint the editor while typing during background traffic" guard, which needs a second
live event to trigger.

**Steps 1 and 3 closed — 2026-10-12.** Both halves of Step 1 now exist:

- **The unit test setup does exist.** `apps/hush-tauri` runs Node's own runner over
  `src/lib/*.test.ts` via `npm test`, type-stripping the TypeScript, so no framework had to be
  added. `paperWire.ts` was split out of `paperStore.ts` precisely to make this possible — the store
  mounts ProseMirror and talks to the daemon, neither of which belongs in a unit test. It is now 16
  tests: the base64 envelope over every byte value, `mergeBatch` collapsing a keystroke burst into
  one update, the revision/compaction rule, the awareness envelope surviving intact, the two-peer
  relabelling codec, and a leaving peer's frame removing its cursor.
- **The two-window verify was done, by driving two real app windows** (headless Chrome over CDP,
  `Input.dispatchMouseEvent` + `Input.insertText` through the browser's own input pipeline, so
  ProseMirror sees the same events a person produces). Result: Alice started the paper and typed
  `ALICE-BROWSER-7Q4`; Bob's editor showed it after ~2.5 s; Bob typed `BOB-BROWSER-3Z9`; Alice's
  editor showed it after ~1.5 s. Both editors ended byte-identical, and each side rendered the
  other's caret with the right name and colour.

The two-window run is what found the create collision and the cursor label — see
`DEEPSEEKPROBLEMS.MD` (2026-10-12, "verification and the create collision"). Both are fixed and
verified; the timings above are relay cadence, because both windows were on one host and the
DataChannel had not yet come up at the moment the text crossed.

Still open from this task: **restart persistence** was checked in the Phase 1 record (a fresh client
rebuilt the text from the daemon's state) but not re-checked on the current build, and the
"don't repaint while typing during background traffic" guard still has no trigger. Neither is
believed broken; neither has fresh evidence.

### Task 6: DataChannel fast path (`paper_data.rs`)

**Files:**
- Modify: `crates/hush-daemon/Cargo.toml` (add `webrtc-rs`), `crates/hush-core/src/config.rs`
  (`stun_url` in `ClientConfig` — the one place)
- Create: `crates/hush-daemon/src/paper_data.rs` (offer/answer/ICE as `MessagePayload` variants
  over the existing sessions; DataChannel carrying yrs updates + presence; hub lifecycle)
- Test: `crates/hush-daemon/src/paper_data.rs` (loopback pair converges; hub drop degrades)

**Interfaces:**
- Consumes: Tasks 4–5.
- Produces: sub-second convergence when the channel is up; automatic relay fallback.

- [x] **Step 1: Spike** — two daemons on one machine open a DataChannel and move yrs updates;
  time to convergence logged. Abort/pivot criterion: if `webrtc-rs` proves unsuitable, re-plan
  before continuing (log in `DEEPSEEKPROBLEMS.MD`).
- [x] **Step 2:** hub lifecycle — creator is hub; peers connect; hub channel loss flips peers to
  relay cadence and back with no user action; badge reflects it.
- [x] **Step 3: Verify:** LAN two-daemon sub-second typing; symmetric-NAT simulation degrades
  cleanly (no TURN, clear UI state). *Verified on loopback and through the degradation path, not on
  a real LAN or a real symmetric NAT — see the record below.*

**Spike record — 2026-10-12 (Task 6, Step 1):** `webrtc` 0.21.1 is suitable; no pivot needed. A real
pair of peer connections over loopback (`127.0.0.1:0`, **no STUN server**) opens one `paper`
DataChannel and moves a real `yrs` v1 update: handshake 612 ms, then one update end to end in
**957 µs** against the relay's ~1–3 s cadence, received byte-for-byte and merging to the sender's
exact state. The spike is test-only (`#[cfg(test)] mod paper_data;`) until Step 2 promotes it. It
proves loopback only: a real LAN run is Step 3, and the in-daemon driving model is Step 2 — see
`DEEPSEEKPROBLEMS.MD` (2026-10-12, Task 6 spike) for the API and runtime findings that cost time.

**Step 2 record (so far) — 2026-10-12:** the transport half is in and tested; the wiring half is not.
`MessagePayload::PaperOffer` / `PaperAnswer` carry the SDP over the existing 1:1 sessions, with the
gathered ICE candidates inside it — one round trip, and deliberately no third variant for candidates.
`ClientConfig::stun_url` is the one place a STUN server is configured, and it has **no default**: a
public one would learn the address of every peer that opens a paper. `paper_data::DataPlane` is the
connection manager — `offer`/`answer`/`accept_answer` for the handshake, `send` for the fast path,
`drain` for inbound updates, `path` for the badge. `send` answers `Sent::UseRelay` rather than an
error whenever the fast path is unusable, so the relay stays the guarantee and a peer that cannot be
reached quickly is not a failure. Four tests cover it: a real update crossing an open channel
(handshake 428 ms; update 2.2 ms, which includes the test's own 1 ms poll interval), a peer with no
channel, an unanswered offer (the degradation case, reached without needing a NAT), and
renegotiation. The module is still `#[cfg(test)]` — nothing consumes it until outbound deltas try
`send` before the relay fan-out, and inbound `PaperOffer`/`PaperAnswer` reach `answer`/
`accept_answer`.

**Step 2 done — 2026-10-12.** The wiring landed and the open design point below was resolved by
building the thing it asked for.

- **Dial on open.** `App.paper_planes` is a per-conclave registry of `DataPlane`s. Opening a paper
  dials the rendezvous the descriptor names: the hub waits, every other trustee offers. Dialling is
  rate-limited by `DataPlane::should_dial`, so a peer that cannot be reached is retried rather than
  hammered.
- **Signalling inbound.** `PaperOffer` / `PaperAnswer` are consumed by `paper_handle_signal` before
  the document path can see them, so the transport moves without the text being touched.
- **Drain loop.** `spawn_paper_plane_loop` drains every plane on a tick, applies inbound updates to
  the sidecar, and re-broadcasts them to the other channels — the hub re-fan the design point asked
  for. Inbound presence frames are written to the ephemeral store instead (30 s TTL, no relay copy).
- **Outbound fast path.** An outbound delta tries `DataPlane::send` first and falls back to the relay
  fan-out; `Sent::UseRelay` is a normal answer, not an error, so the relay stays the guarantee.
- **Badge.** `paper_path_view` reads `live_peers` off the planes, so `fast_path.path` is `live` when
  at least one trustee is on a channel and `relay` when none are.
- **Lock.** Locking the vault shuts every plane down (`paper_planes_shutdown`), which is what makes
  the dogfood's "locking the hub drops the peer to relay cadence, with no user action" true: the
  trustee's `drain` sees the channel close and the badge flips to `relay` with no UI involvement.

**Step 3 limits, stated plainly.** What is verified is the **degradation path**, and it is verified
several ways: a peer with no channel, an unanswered offer, a renegotiation, and — in the dogfood — a
hub that locks mid-session and a hub that is kicked. What is **not** verified here is a real LAN
(two machines, real NICs) or a real symmetric NAT: every run is loopback, and the NAT case is
reached by *simulating* unreachability (no answer) rather than by sitting behind a NAT. Both remain
open as environment work; nothing in the code depends on which of them is true, because the relay is
still fanning out underneath in every case.

**The hub re-broadcast design point (resolved).** The relay fan-out is a full mesh; the DataChannel
is a **star** through the hub. A trustee's edit reaches only the hub over the fast path, so for the
fast path to be worth anything on a paper with more than two trustees the hub must forward an update
it receives over one channel to its other channels. That is now what the drain loop does. It makes
the hub the star's bandwidth centre and a single point of failure for *latency* — not for
correctness, because the relay keeps fanning out underneath — which is the honest cost of a star.

### Task 7: Presence, succession, hardening

**Files:**
- Modify: `crates/hush-daemon/src/paper_data.rs` (awareness: cursor/colour/name over DataChannel only)
- Modify: `crates/hush-core/src/account.rs` (convener-authoritative `PaperHub` handling — accept
  only from the convener named in the descriptor), `crates/hush-daemon/src/main.rs`
  (`POST /api/paper/{conclave_id}/hub/rehome`, convener-only)
- Modify: `apps/hush-tauri/src/app.ts` (named live cursors via ProseMirror decorations; read-only
  rendering after `ConclaveEnd`; hub-gone / NAT / cap error states)
- Test: `crates/hush-core/src/account.rs` (PaperHub from a non-convener is ignored)

- [x] **Step 1:** failing test for convener-only `PaperHub` acceptance; implement.
- [x] **Step 2:** presence (DataChannel-only) + cursors in the editor.
- [x] **Step 3:** lock/suspend paths — locked hub degrades peers; local unsent deltas flush on
  unlock and merge.
- [x] **Step 4:** read-only paper rendering after `ConclaveEnd`; caps surface their reasons.
- [x] **Step 5: Verify:** full three-daemon dogfood — colours/cursors distinct; creator kicked →
  convener inherits hub, liveness resumes; ended conclave → papers read-only in every vault.

**Step 1 record — 2026-10-12.** The authority rule lives in `hush-core`, not the daemon, so there is
no second copy of the roster to drift. `only_the_convener_may_re_home_a_paper` has a joined trustee
Bob put a `PaperHub` claim on the wire (which a modified client would do) and asserts Alice drops
it — `inbox.papers.is_empty()` — while the convener's identical claim is delivered.
`a_trustee_cannot_re_home_a_paper` pins the sender side (`Error::NotConclaveConvener`). The route
turns that into a 403 rather than a 400, because the caller may ask and simply is not the convener.

**Step 3 record — 2026-10-12.** Two halves, and one of them was a real gap that a green suite had
not seen:

- **Daemon.** Locking the vault shuts every `DataPlane` down (`paper_planes_shutdown`), so a peer's
  channel closes and its badge flips to `relay` with no user action; the relay fan-out is still
  running underneath, so the edit lands anyway. Both are dogfood checks ("locking the hub drops the
  peer to relay cadence, with no user action" and "an edit made after the hub locked still lands
  over the relay"). The daemon's outbox also retries after unlock, which is what "flush on unlock"
  means at the transport.
- **Webview — fixed here.** `closePaper()`'s callers claimed it "flushes its pending edits", but it
  destroyed the Yjs doc and its timers without sending them, and `lock()` called `api.lock()`
  *before* `closePaper()`, so any edit inside the 400 ms flush window was lost twice over: never
  sent, and then dropped with the doc. Now `closePaper()` flushes before it destroys (the bytes are
  merged synchronously, so the request is built before the doc goes), and a new exported
  `flushPaperEdits()` is called by `lock()` **before** `api.lock()`, while every paper route still
  answers. The window in which an edit can be lost is now a crash, not an ordinary lock.

**Step 4 record — 2026-10-12.** Three layers, each tested where it lives:

- **Core:** `paper_payloads_are_refused_once_the_conclave_has_ended` — a delta and a snapshot are
  both `Error::ConclaveEnded` on the convener's side, and Bob cannot send once he has applied the
  end.
- **Daemon:** `paper_write_refusal` refuses a write with `403 FORBIDDEN` when the conclave is ended
  or the writer is not a joined trustee, and the refusal is a reason string rather than a silent
  drop.
- **UI:** `attachEditor` builds no toolbar, sets `aria-readonly="true"` and `contenteditable` off,
  and shows the refusal's reason. Cursors still render, because the document is still shared — it is
  frozen, not gone.
- **Caps** surface their reason: the sidecar's doc and snapshot caps return the reason string the
  routes turn into the API error, the `notes_store` pattern.

**Step 4 correction — 2026-10-12, verification follow-up.** The read-only editor was unreachable for
the case it names. `conclavePane` returned at its `ended` branch *before* the Chat | Paper pill was
built, so an ended conclave rendered the history and the ended note with no tab strip at all, and
`renderPaperSurface` — the only caller of `attachEditor` — never ran. The daemon had it right the
whole time: `GET /api/paper/{id}` stays `200` on an ended conclave, and
`an_ended_conclave_paper_refuses_writes` asserts exactly that ("read-only, not gone"), so the UI was
hiding a document it was built to show. The ended branch now builds the pill and keeps both surfaces:
the paper renders read-only with its note and no toolbar, and only the composer is dropped. Two
smaller things came out of the same pass: `attachEditor` takes the refusal's **reason string** rather
than a boolean, because a kicked trustee was being told "This conclave has ended" when it had not,
and the dead `paper_conclave_ended` helper (superseded by the inline check in `paper_write_refusal`)
is gone.

Verified in the live UI on `peer-a`, ended conclave `9aeb64a4`: `[role=tab]` = `["Chat", "Paper"]`,
the paper tab selected, `.paper-readonly-note` = "This conclave has ended, so the paper is read-only.",
`.ProseMirror` `innerText` = the converged document text, `contenteditable="false"`, 0 toolbar
buttons; the chat tab keeps its ended note and has no composer. A live (unended) conclave still
renders the toolbar's 11 buttons and an editable surface.

**Step 2 record — 2026-10-12.** Presence rides the DataChannel and nothing else: the editor
publishes its awareness (cursor as a Yjs relative position, plus the author's identity and colour)
to the daemon, the daemon forwards it along the star, and the other side polls it back as opaque
frames. There is no relay fallback and no persistence, which is why "no cursors" is the correct
reading of a paper at relay latency rather than a bug: a cursor that arrives a second and a half
late is worse than no cursor, and a stale one is a lie about who is reading.

The **label** is resolved on receipt rather than sent: a peer publishes its account id, which is the
identity the relay authenticated and therefore the one thing a peer cannot lie about, and the
receiver looks that id up in its own conclave roster before drawing the caret. That is what makes
the caret say `bob` instead of 64 hex characters while still making it impossible to label your
caret after someone else. Verified in the live UI, both directions: Alice's editor renders
`<div style="background-color: #b7791f">bob</div>` and Bob's renders `alice`, each with the
author's own colour. Five unit tests cover the frame codec, including two-peer frames and bytes the
build cannot read (passed through, never thrown away here — dropping is the caller's call).

**Step 5 record — 2026-10-12.** `scripts/paper-dogfood.sh`, three daemons and a real relay, all 23
checks green. The succession and read-only claims are each covered twice: the explicit re-home by
the convener (and the 403 for a trustee) *and* the automatic inheritance after a kick, then a
post-end write refusal on the convener's vault, on a kicked vault, and on a surviving trustee's —
with the ended document still readable and still the one the trustees converged on. Presence is
proven with the relay deliberately undrained, so a frame that arrives cannot have come from
anywhere else. Two limits: the dogfood drives the daemon API with the editor's own bytes rather
than typing in a window, and it covers three trustees, not the plan's larger set.

### Task 8: Distro-readiness + docs

**Files:**
- Modify: `docs/DISTRO_READINESS.md` (`yrs`, `webrtc-rs` deps; `stun_url` config; sandbox: SCTP
  egress, no new devices), `docs/THREAT_MODEL.md` (spec Section 6 table), `docs/PROTOCOL.md`
  (Paper subsection under Conclaves), `docs/plans/2026-10-04-v1-milestones.md` (Task 19 status)

- [x] Update each doc with dates and file references; no new config system; `cargo deny`/`audit`
  note for the new deps.
- [x] Verify: `cargo test --workspace`, `./scripts/run-two-ui.sh` three-daemon session, locked-vault
  directory scan shows `papers.enc` ciphertext only.

**Docs — 2026-10-12.**

- `docs/PROTOCOL.md` — a **Paper** subsection under Conclaves: the payload variants, the
  creator/convener authority split, the relay fan-out, the star-shaped DataChannel and the hub
  re-broadcast, `stun_url` with no default and no TURN, presence with no relay copy, succession, and
  the "no plaintext" line. Ends with the relay's view, which is unchanged from any other message.
- `docs/THREAT_MODEL.md` — the spec's Section 6 table, verbatim in shape: relay timing (richer,
  accepted), STUN, the hub daemon, DataChannel peers, doc bytes at rest, and the persistence rule
  that Paper is not "leaves nothing behind" like a call.
- `docs/DISTRO_READINESS.md` — two additions: the DataChannel is a **second, direct egress path**
  (SCTP over UDP, so a sandbox must permit it and must not assume the relay is the only socket; no
  new devices, so `PrivateDevices` stays), and the two new deps (`yrs`, pure computation; `webrtc-rs`,
  pure Rust with userspace sockets only) belong in the `cargo-deny` / `cargo-audit` scope.
- `docs/plans/2026-10-04-v1-milestones.md` — Task 19's status moves from "Phases 1 and 2 landed,
  Tasks 6–8 remain open" to all eight tasks landed.
- **No new config system:** STUN is a field on the existing `ClientConfig`, read from
  `{data_root}/client-config.json` like every other client setting (DR-14).

**`cargo deny` / `cargo audit`: not run — neither is installed here** (`error: no such command:
deny` / `audit`). The note above is what the plan asked for; adding the tools to CI is separate work.

**A bug the two-window run found — 2026-10-12.** Driving two real windows rather than trusting the
suite turned up a genuine correctness bug in the create path, and it is the reason the fast path
looked dead: `PapersState::adopt` looked a paper up by `paper_id` while `create` and `get` key on
the conclave, so two trustees clicking "Start paper" before either heard from the other left **two
papers for one conclave**, each naming its own hub. Neither side dialled, for the life of the paper.
The text still converged — the relay fan-out is by conclave — so the only symptom was a performance
feature quietly not working, which is exactly what a convergence-only test cannot see. `adopt` now
keys on the conclave and settles a collision deterministically (earlier `created_ms` wins, ties to
the smaller id), which is the rule the UI already promised; the loser keeps its document and the
daemon re-announces it as a snapshot, because the deltas it sent under the old id were dropped.
After the fix both daemons report one `paper_id` and `fast_path: {"path":"live","live_peers":1}`.
Full write-up, including the stale-binary red herring that cost the first hour, in
`DEEPSEEKPROBLEMS.MD` (2026-10-12, "verification and the create collision").

**Verification — 2026-10-12.**

- `cargo test --workspace` — **398 passed, 0 failed, 7 ignored** across 27 test binaries, including
  `hush-daemon` (120) and `hush-core` (99) and the 19 `conclaves` integration tests.
- `npm test` in `apps/hush-tauri` — 16/16, plus `tsc --noEmit` clean.
- `./scripts/paper-dogfood.sh` — 23/23, three daemons and a real relay. The no-plaintext check scans
  **every** file the run leaves behind (not a hand-picked list) for all four edit strings, so
  `relay.db`, all three vaults' `papers.enc`, and the daemon logs are all in scope.
- **Two real app windows** (headless Chrome over CDP): convergence both ways, the remote caret
  rendered on both sides with the right name and colour, `fast_path.path` moving to `live`, and an
  ended conclave showing no editor and no `Chat | Paper` pill at all. This is the run that found the
  create collision; the commands were throwaway, so the durable evidence is the checks above plus
  the three collision unit tests.

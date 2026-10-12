# Notes dashboard (sticky papers)

**Status:** Spec resolved 2026-10-11; **16.1 implemented** 2026-10-11 (encrypted sidecar store,
`/api/notes/*`, and the Vault tile opening a full-screen list/editor). **16.5 (search) implemented**
and a **sticky-note view mode** added 2026-10-11. **16.2–16.4 not started.**
**Index:** Task **16** in
[`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md) (Phase **F**).

**Goal:** A **private** notes surface inside the capsule: looks like real sticky papers on a desk —
scattered on a board or gathered into **regions** — with **drag-and-drop**, plain text, and
**checklists** you can tick off. Entirely **local** and **encrypted** under the vault; included in
backup by the existing whole-vault export.

**Not the same as:** Article **library annotations** (Task **12.3**, tied to Nostr addresses) or
**Confidential** containers (Task **10**). Notes are everyday scratch space, reached from the Vault.

**Relationship to other docs:** follows the daemon-only-state and vault-sidecar posture of
[`hush-reads.md`](hush-reads.md) and [`hush-torrents.md`](hush-torrents.md). Like Reads, the state
is user data sealed in the vault; unlike Torrents, nothing leaves the device — there is no network
path at all.

---

## Decisions (resolved 2026-10-11)

| Decision | Choice | Why |
|----------|--------|-----|
| **Surface** | A **tile in the Vault bento**, beside Storage / Auto-lock / Backup / Files / Torrents. Opening it shows the board **full-bleed with a "Back to Vault" button**. No new rail. | The rail is already twelve entries deep; the Vault bento is where vault-backed features are surfaced (Files, Torrents). |
| **Storage** | Encrypted JSON **sidecar `notes-state.enc`** in the vault, written with `Vault::write_encrypted_file`. The sidecar bytes are exactly the API payload. | Matches `reads-state.enc` / `torrents-state.enc`. SQLite is a relay-only dependency and would be a new pattern for user state. |
| **Backup** | Nothing to do — `backup::export_vault` tars the whole vault directory, so the sidecar rides along. | The earlier "backup manifest includes notes DB" step assumed SQLite; it is dropped. |
| **Boards** | **One board** (`board_id = "default"`), but every note and region stores `board_id` so more boards are additive later, not a migration. | Avoids the multi-board UI now without closing the door. |
| **Text** | **Plain text** (title + optional body). | Sticky-paper metaphor; the `RichTextEditor` is mail-only and would be overkill. |
| **Note shape** | **Flat:** title + optional body + optional checklist. A note can be just a checklist, just text, or both. | No block model needed for a scratch pad. |
| **Checklists** | **Structured items** `[{id, text, done}]` on the note, not `[ ]` lines parsed from the body. | A box can be ticked without opening the editor, and ids stay stable for a future export. |
| **Layout** | **Free canvas**, absolute `x`/`y`, `z` raised on focus. **Bounded** (~3000×2000 logical) and scrollable, **no zoom**; notes clamp inside. | No pan/zoom state to persist; bounds keep notes reachable. There is no drag code in the UI today, so this is greenfield. |
| **Groups** | **Regions**: stored rectangles (x/y/w/h, label, colour). A note is "in" a region **by its centre** (derived, not stored). Dragging a region **moves the notes inside it**; a region can **collapse to a stack**. | The doc's "stacks or named regions" resolved to regions; derived membership means moving a region carries its papers. |
| **Search** | Case-insensitive **substring** over the in-memory sidecar (title + body + checklist text). No engine. | There is no FTS in the repo and a personal board is small; revisit only if it grows. |
| **Delete** | **Confirm, then hard delete** — matches the Torrents library delete. | No trash surface to maintain. |
| **Locked vault** | **401** on every state route. | Matches the Reads contract. |

### Defaults baked in

- **Paper colour:** a fixed five-colour palette, optional per note (no free colour picker).
- **Timestamps:** `created_at` / `updated_at` (Unix seconds), used for sort and "edited".
- **Saves:** edits debounced to a single sidecar rewrite; a drag commits its position on pointer-up.
- **Region delete:** the notes stay on the board — membership was only ever positional.
- **Caps:** like Reads, bound the sidecar (proposed: 2 000 notes, 200 regions, 100 checklist items
  per note). A cap refuses with a reason rather than silently dropping.
- **Empty state:** a single "New note" affordance in the middle of an empty board.

### Deferred (explicitly not v1)

Export a note as markdown; send a single note to a contact; link a note to a project (**14.2**);
the multi-board UI; a real search index; rich text; any Android consideration (Android is Task **8**,
do not start).

---

## Product sketch

| Element | Behavior |
|---------|----------|
| **Sticky note** | Title + optional body + optional checklist; optional paper colour; created and edited only on device. |
| **Board layout** | Notes have `x`/`y` (and `z`) on a bounded canvas — "papers around the desk." |
| **Regions** | A labelled rectangle you drag notes into. Notes inside move with it; it collapses to a compact stack. |
| **Checklists** | Structured items on the note; a box toggles without opening the editor. |
| **View mode** | Clicking a paper opens it as a centred **sticky** — portrait, squarish at rest, growing with the text — with Edit / Delete / Close. Edit is the only way into the editor. |
| **Search** | Substring over titles, bodies and checklist text; results jump the board to the note. |

**UX tone:** Calm, tactile, Soft Daylight — physical paper, not a Notion clone or an infinite
whiteboard SaaS.

---

## Policy / threat model

- **Private by default** — nothing is sent to `hush-relay` or Nostr; there is no network path.
- **No collaboration** — no shared board, no live co-editing. (Sending a single note as an E2E
  file/message is a possible later, fan-out feature — not this task.)
- **No engagement metrics** — personal only.
- **Distro:** the sidecar lives under the vault / data root; a locked vault makes notes unreadable.

---

## Technical direction

- **Store:** `notes-state.enc` via `Vault::write_encrypted_file` / `read_encrypted_file`, decoded by
  a `notes_store.rs` module shaped like `reads_store.rs` (state struct + `encode` / `decode`, caps
  enforced in methods, unit tests for round-trip and corruption).
- **Daemon:** routes under `/api/notes/*`; state get/put goes through `account_worker.rs`, which owns
  the Vault, via `WorkResult` variants (the Reads/Torrents pattern).
- **UI:** a new tile in `renderVault` (`apps/hush-tauri/src/app.ts`) that sets a vault sub-view; the
  board renders in the vault pane with a Back button. A `lib/notesStore.ts` module and methods in
  `lib/api/daemon.ts` / `lib/api/types.ts` follow the Reads pane. Drag uses pointer events
  (`pointerdown` / `setPointerCapture` / `pointermove` / `pointerup`).

### State shape (indicative)

```
Note   { id, board_id, title, body, color, x, y, z, created_at, updated_at,
         checklist: [ { id, text, done } ] }
Region { id, board_id, label, color, x, y, w, h, collapsed }
NotesState { board_id, notes: [Note], regions: [Region] }
```

### API (all under `/api/notes`)

Implemented in 16.1:

| Route | Body | Returns |
|-------|------|---------|
| `GET /state` | — | `NotesState` |
| `POST /note` | `{id?, title, body, color?}` | `{ state: NotesState, id }` — create when `id` is absent; the `id` is returned so the UI can open the note it just made |
| `DELETE /note/{id}` | — | `NotesState` |

Planned for later steps (not yet mounted):

| Route | Body | Returns |
|-------|------|---------|
| `POST /note/position` | `{id, x, y, z}` | `NotesState` |
| `POST /note/checklist` | `{note_id, items}` | `NotesState` |
| `POST /region` | `{id?, label, color, x, y, w, h}` | `NotesState` |
| `POST /region/geometry` | `{id, x, y, w, h, collapsed}` | `NotesState` |
| `DELETE /region/{id}` | — | `NotesState` |

---

## Build order (draft)

| Step | Deliverable |
|------|-------------|
| **16.1** | **Done 2026-10-11** — `notes_store.rs` + sidecar; note CRUD; the Vault tile and the full-screen view with Back; a plain list |
| **16.2** | Bounded canvas, drag, `x`/`y`/`z` persisted on pointer-up |
| **16.3** | Regions: create, drag-into, collapse, and moving a region moves its notes |
| **16.4** | Checklists: add / edit / reorder / toggle without opening the editor |
| **16.5** | **Done 2026-10-11** — substring search over title + body + checklist, filtering the list in place |

**Also 2026-10-11 (not in the original order):** the **view mode** — clicking a note opens a
read-only sticky popup rather than jumping straight into the editor. The sticky takes the note's
paper colour as its fill with fixed dark ink, so it reads as a physical sticky in either app theme;
it holds a squarish portrait minimum and grows with the text up to the viewport, then scrolls.

**Depends on:** Task **5c** (encrypted local store). **Independent** of Task **11** Articles.

---

## Acceptance criteria (when done)

- [ ] Notes survive app restart; not visible when the vault is locked.
- [ ] A note can be placed on the canvas and dragged; its position survives restart.
- [ ] A region can be created, notes dragged into it, and collapsed to a stack; dragging the region
      moves the notes whose centres are inside it.
- [ ] A checklist item can be added and toggled without opening the note editor, with no data loss.
- [ ] Restore from backup recovers notes, positions, regions and checklists.
- [ ] No note content in relay logs, and no plaintext on disk at rest.

**Progress (16.1 + 16.5 + view mode):** the store, routes and list/editor are in and tested
(`notes_store::notes_store_tests`, `notes_api_tests`), so a created note is sealed in the vault
and re-reads after a restart. Search and the sticky view mode were exercised in a running app
(2026-10-11): title match, body-only match, no-match and clear all behaved, the search field keeps
focus while the list re-renders in place, clicking a paper opens the sticky, Edit lands in the
editor with the note loaded, and Delete-with-confirm from the sticky removed the note. The canvas,
region and checklist criteria below stay unchecked until 16.2–16.4 land. The canvas, regions and
checklists are still the unbuilt part of this task.

---

## Open questions

None outstanding — all resolved in the Decisions table above on 2026-10-11. Reopen here if a
deferred item is pulled into scope.

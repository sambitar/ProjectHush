# Notes dashboard (sticky papers — planning only)

**Status:** Future backlog — **not** v1. **Index:** Task **16** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md).

**Goal:** A **private** notes surface inside the capsule: looks like real sticky papers on a desk — scattered on a board or **grouped** into clusters — with **drag-and-drop**, plain text, and **checklists** you can tick off. Entirely **local** and **encrypted** under the vault (Task **5c** / data root); included in **Task 6** backup.

**Not the same as:** Article **library annotations** (Task **12.3**, tied to Nostr addresses) or **Confidential** containers (Task **10**). Notes are everyday scratch space on the Vault / Home rail.

---

## Product sketch

| Element | Behavior |
|---------|----------|
| **Sticky note** | Title + body; optional color/paper style; created/edited only on device. |
| **Board layout** | Notes have **x/y** (and z-order) on a canvas — “papers around the dashboard.” |
| **Groups** | User-defined **stacks** or named regions; drag note into group (collapses to stack or grid inside group). |
| **Checklists** | Per-note blocks: lines with `[ ]` / `[x]` or structured checklist items; toggle without opening full editor. |
| **Search** | Local FTS over note titles/bodies (same patterns as articles cache — daemon-side). |

**UX tone:** Calm, tactile, Soft Daylight — physical paper metaphor, not a Notion clone or infinite whiteboard SaaS.

---

## Policy / threat model

- **Private by default** — never sent to `hush-relay` or Nostr unless user explicitly uses **12.5**-style “publish excerpt” later.
- **No collaboration v1** of this feature — no shared board on our servers; optional future: send a **single note** as E2E file/message to a contact (fan-out), not live co-editing.
- **No engagement metrics** — N/A (personal only).
- **Distro:** note DB and any thumbnails under **`HUSH_DATA_DIR`**; lock vault → notes unreadable.

---

## Technical direction (when implemented)

- **Storage:** SQLite table(s) e.g. `notes`, `note_checklist_items`, `note_groups`, layout fields (`board_id`, `pos_x`, `pos_y`, `group_id`, `sort_order`).
- **API:** daemon routes under `/api/notes/*` (CRUD, reorder, toggle checklist item); UI uses drag-and-drop (pointer events + persisted layout).
- **UI rail:** New **Notes** or sub-panel under **Vault / Home** — avoid crowding Chats primary rail until dogfood says otherwise.
- **Links (later):** optional `linked_project_id` when Task **14.2** projects exist; optional pin note to contact (local only).

---

## Build order (draft)

| Step | Deliverable |
|------|-------------|
| **16.1** | CRUD note + encrypted store + simple list view |
| **16.2** | Canvas layout + drag position persist |
| **16.3** | Groups / stacks + drag between group and board |
| **16.4** | Checklist blocks + toggle API |
| **16.5** | FTS search; backup manifest includes notes DB |

**Depends on:** Task **5c** (encrypted local store), **6** (backup). **Independent** of Task **11** Articles.

**Optional later:** export note as markdown file to vault blob; link from project dashboard.

---

## Acceptance criteria (when done)

- [ ] Notes survive app restart; not visible when vault locked.
- [ ] User can place notes on a board and in at least one **group** via drag-and-drop.
- [ ] User can add a checklist and check/uncheck items without data loss.
- [ ] Restore from backup recovers notes and layout.
- [ ] No note content in relay logs or plaintext on disk at rest.

---

## Open questions

- One global board vs multiple boards (tabs).
- Max canvas size / performance on low-end Android.
- Whether checklist state belongs in same note revision or separate table (prefer separate rows for sync-friendly backup).

# Knowledge layer — Phase A (Tasks 12.x)

**Status:** Planning only. **Do not start** until Task **11** is dogfooded (publish, follow, offline cache, Tor per pen name).

**Index:** [`docs/plans/knowledge-layer-roadmap.md`](../plans/knowledge-layer-roadmap.md) · Task **12** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md)

**Goal:** Turn Articles from “read followed authors” into **argument + library** — still no likes, no trending, no Hush social graph.

---

## Product positioning (internal)

Ship copy direction (not marketing hero):

> Hush — communicate privately, publish under pen names, and build knowledge on your device without turning everything into a popularity contest.

Phase A is the first expansion after Task 11’s feed + editor: **sections, links, library, briefing, publish-from-vault**.

---

## UI shell (Tauri)

- New primary rail: **Articles** (Task 11) gains sub-nav:
  - **Reading** — followed feed (11.3)
  - **Sections** — local filters (12.1)
  - **Library** — shelves + private notes (12.3)
  - **Briefing** — on-device digest (12.4)
- **Argument composer** opens from article view (12.2).
- **Publish from vault** flow starts in Files/Vault, ends in Articles editor (12.5).
- Keep **Chats** and **Articles** visually and cryptographically separate (pen name ≠ messaging contact).

---

## Local data model (encrypted under data root)

Extends `hush-articles` SQLite (same DB file or `library.db` — decide at implementation; one backup manifest entry).

| Table / concept | Purpose |
|-----------------|--------|
| `reading_list` | Ordered article addresses + added_at |
| `shelves` | User-defined taxonomy (name, parent_id) |
| `shelf_items` | article_address → shelf_id |
| `annotations` | article_address, offset/quote hash, **private** note ciphertext |
| `edges` | from_address, to_address, `link_type` (supports, refutes, evidence, part_of, cites) |
| `briefing_rules` | JSON rules (followed pubkeys, min independent refs, keywords) |
| `curated_lists` | Named pubkey lists (Editor’s picks — config import OK) |

**No** stored like counts, view counts, or follower totals in UI-facing tables.

FTS5 continues to index cached article bodies (Task 11); library search includes annotations locally only.

---

## Daemon API sketch (`/api/articles/…`)

Build on Task 11 routes; all responses sanitized; no raw relay events to webview.

| Route | Phase A |
|-------|---------|
| `GET /library/shelves` | List shelves + counts |
| `POST /library/shelves` | Create/rename shelf |
| `POST /library/items` | Add article to shelf / reading list |
| `GET /library/annotations?address=` | Private notes for article |
| `POST /library/annotations` | Upsert highlight + note |
| `GET /edges?address=` | Outgoing typed links |
| `POST /edges` | Create link (validates both ends cached or resolvable) |
| `POST /argument/preview` | Ordered list of addresses → render “argument path” for composer |
| `GET /briefing/today` | Run rules on device; return items + **reason** strings |
| `POST /publish/from-blob` | 12.5: vault blob id → draft article (redaction step in UI) |

Exact paths may merge with 11.x naming; keep **one articles subsystem** gate behind vault unlock.

---

## Build order

| Step | Task | Deliverable | Verify |
|------|------|-------------|--------|
| **12.1.1** | Section filters | Tag/topic filters on cached feed; import curated pubkey list from config | No network beyond 11 relay reads |
| **12.2.1** | Edge types + storage | CRUD edges; publish optional `a` tags on new articles when user chooses “public link” | Round-trip: A links B, B visible in graph query |
| **12.2.2** | Argument composer | UI: ordered chain + link types; export as article with embedded link list | Reader sees path, not a comment thread |
| **12.3.1** | Reading list + shelves | Drag to shelf; encrypted annotations | Lock vault → annotations unreadable |
| **12.3.2** | FTS over library | Search titles + local notes | No annotation leakage to relays |
| **12.4.1** | Briefing rules engine | Default rule: “≥2 followed authors cited same address” | Each item shows `reason` field |
| **12.4.2** | Briefing UI | Morning view; no “trending” | Offline works from cache |
| **12.5.1** | Vault → draft | Pick blob, redact UI, create local draft | Draft not on relay until publish |
| **12.5.2** | Evidence excerpt | Quote block + link back to private blob hash (optional publish) | Published article does not leak vault path |

**Depends:** Task **11.5** (editor), **11.6** (search), **5c** (encrypted at rest), **6** (backup includes library tables).

---

## Policy checks (before each PR)

- [ ] No engagement metrics in UI (likes, views, leaderboards).
- [ ] Briefing reasons are **inspectable** (user can see rule that included an item).
- [ ] Annotations and shelves never published by default.
- [ ] Pen name keys still isolated from messaging identity.
- [ ] No new first-party server beyond existing relay + Nostr relays.

---

## Acceptance criteria (Phase A complete)

- [ ] User follows authors, reads offline, organizes **reading list + shelves**, adds **private** highlights.
- [ ] User builds and publishes an **argument chain** linking ≥2 articles with typed edges.
- [ ] User receives a **briefing** generated on device with per-item **why included** text.
- [ ] User can start a public article from a **vault blob** after explicit redaction.
- [ ] Backup/restore recovers library + edges + annotations (Task 6 manifest updated).

---

## Out of scope for Phase A

- Theses/debates (Task **13.x**), graph visualization ( **14.1** ), projects ( **14.2** ), audio/TTS ( **15.1** ), library AI ( **15.4** ).
- Messaging group rooms ( **15.3** ) — separate from Nostr public layer.

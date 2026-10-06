# Daemon API gaps (UI mocks)

**Future rails:** Task **11** `/api/articles/*` and Task **12** library/edges/briefing — `docs/tasks/hush-articles.md`, `docs/tasks/knowledge-layer-phase-a.md`.

The Soft Daylight UI uses real daemon endpoints for unlock, lock, invite, contacts, send, send-file-data, recv, and status.

These features use local mocks in `src/lib/api/mock.ts` and `src/lib/chatStore.ts` until the daemon grows:

- Message history: loaded from **`GET /api/messages`** after unlock (Task **5c.3**); new messages still arrive via **`POST /api/recv`**
- Delivery states and 7-day queue expiry countdown
- Reactions
- Mute, block, report, remove contact (local flags only)
- Nickname edit (local display override)
- Auto-lock timeout configuration
- Backup status and backup action
- Recovery code reveal
- Change passphrase
- Trusted devices
- QR camera scan
- Global files index (aggregated from chat store) — **planned:** Task 5b in `docs/plans/2026-10-04-v1-milestones.md` (per-contact Received/Sent + open/save)
- Inbox file **open/save in UI** — daemon exposes **`GET /api/blob/{id}`**; wire **5b.1** open/save from chat cards

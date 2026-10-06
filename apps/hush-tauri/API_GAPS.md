# Daemon API gaps (UI mocks)

**Future rails:** Task **11** `/api/articles/*` and Task **12** library/edges/briefing — `docs/tasks/hush-articles.md`, `docs/tasks/knowledge-layer-phase-a.md`.

The Soft Daylight UI uses real daemon endpoints for unlock, lock, change-passphrase, invite, contacts, send, send-file-data, recv, and status.

These features use local mocks in `src/lib/api/mock.ts` and `src/lib/chatStore.ts` until the daemon grows:

- Message history: loaded from **`GET /api/messages`** after unlock (Task **5c.3**); new messages still arrive via **`POST /api/recv`**
- Live presence: **`POST /api/presence/heartbeat`** + **`online`** on contacts; chat send blocked when contact offline (async mail = Task **5d**)
- Reactions
- Mute, block, nickname override (persisted in **`ui-prefs.json`** under data root via **`GET/PUT /api/ui-prefs`**)
- Report abusive relay slot (local block list exists; no server report API yet)
- Auto-lock timeout: **`auto_lock_minutes`** in ui-prefs (Settings → Security); daemon enforces idle lock + Linux suspend lock
- Recovery code cannot be re-shown after acknowledge (by design — save offline)
- Trusted devices: one-time device-link transfer is real (**`POST /api/link-device/start`**, **`GET /api/link-device/{id}`**, **`POST /api/link-device/pull`**; Settings → Security shows the QR, unlock screen has "Transfer from another device"). Live multi-device sessions and a device list/revoke UI are still open.
- QR camera scan (mobile): the app renders pairing/invite QRs but cannot scan one yet; paste/URL entry is the v1 path
- Global files index (aggregated from chat store) — **planned:** Task 5b in `docs/plans/2026-10-04-v1-milestones.md` (per-contact Received/Sent + open/save)
- Files rail **5b.2**: per-contact Received/Sent + send file (from chat store / daemon history)

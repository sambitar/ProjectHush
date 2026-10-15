# Daemon API gaps (UI mocks)

**Future rails:** Task **11** `/api/zines/{id}/publish` (public lane, [`docs/tasks/atproto.md`](../../docs/tasks/atproto.md)) and Task **12** library/edges/briefing — [`docs/tasks/knowledge-layer-phase-a.md`](../../docs/tasks/knowledge-layer-phase-a.md).

The Soft Daylight UI uses real daemon endpoints for unlock, lock, change-passphrase, invite, contacts, send, send-file-data, recv, and status.

These features use local mocks in `src/lib/api/mock.ts` and `src/lib/chatStore.ts` until the daemon grows:

- Message history: loaded from **`GET /api/messages`** after unlock (Task **5c.3**); new messages still arrive via **`POST /api/recv`**
- Live presence: **`POST /api/presence/heartbeat`** + **`online`** on contacts; chat send blocked when contact offline. Mail is real (Task **5d**): **`POST /api/mail/send`**, **`GET /api/mail`**, **`POST /api/mail/handle`**, and `kind: "mail"` on **`POST /api/recv`** deliver store-and-forward while the recipient is away.
- Conclaves are real (2026-10-08; membership made consensual the same day): **`GET/POST /api/conclaves`**, **`POST /api/conclaves/message`**, **`POST /api/conclaves/trustees/invite`**, **`POST /api/conclaves/trustees/kick`**, **`POST /api/conclaves/trustees/accept`**, **`POST /api/conclaves/trustees/decline`**, **`POST /api/conclaves/trustees/leave`**, **`GET /api/conclaves/invites`**, **`POST /api/conclaves/end`**, **`DELETE /api/conclaves/{id}`** (2026-10-09: delete an ended conclave and its message log from this vault only; shared-file blobs stay), and `kind: "conclave"` on **`POST /api/recv`**. Group chat is sender-side fan-out over the joined trustees' 1:1 sessions; there is no relay-side room. Membership is consensual: naming someone sends an invite they must accept, and a message never materialises a conclave on a vault that has never seen it. The convener (creator) is the only authority for invite, kick and end; any joined trustee may leave, and the convener leaving ends the conclave. Ending is broadcast, so the conclave is read-only on every peer while the log is kept.
- Reactions
- Mute, block, nickname override (persisted in **`ui-prefs.json`** under data root via **`GET/PUT /api/ui-prefs`**)
- Report abusive relay slot (local block list exists; no server report API yet)
- Auto-lock timeout: **`auto_lock_minutes`** in ui-prefs (Settings → Security); daemon enforces idle lock + Linux suspend lock
- Recovery code cannot be re-shown after acknowledge (by design — save offline)
- QR camera scan (mobile): the app renders invite QRs but cannot scan one yet; paste/URL entry is the v1 path
- Global files index (aggregated from chat store) — **planned:** Task 5b in `docs/plans/2026-10-04-v1-milestones.md` (per-contact Received/Sent + open/save)
- Files rail **5b.2**: per-contact Received/Sent + send file (from chat store / daemon history)

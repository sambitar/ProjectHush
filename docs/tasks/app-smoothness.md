# App smoothness — making Hush feel light

**Status:** Partially implemented 2026-10-17 (items 1, 2 in part, 4 in part — see each
item's status). Diagnosis grounded in code as of 2026-10-17; memory figures measured
the same day against the running `Hush_0.2.0_amd64.AppImage` (app v0.3.1 code).
Verification so far is static (`tsc`, production build, 83/83 unit tests); the running-app
checks in "How we will know it worked" are still pending.

**Goal:** The app should *feel* light: typing and scrolling never stutter, background
traffic never disturbs what the user is doing, the CPU idles when the user idles, and
long panes cost the same to scroll on day 30 as on day 1. Raw RAM is a secondary effect
of reaching this goal, not the target.

---

## Why it feels heavy today (evidence)

The single root cause is that every state change rebuilds the entire UI on the main
thread, and nothing ever fully idles around it.

- `render()` in `apps/hush-tauri/src/app.ts` (line ~1735) does
  `this.root.innerHTML = ""` (line ~1797) and rebuilds rail, lists, active pane, and all
  modals from scratch. It carries scroll position, composer focus/caret, and modal zoom
  across the wipe (lines ~1739–1797) — machinery that exists only to compensate for the
  wipe, and each carry is itself a jank vector: scrollbar thumb jumps, hover states on
  list rows vanish, text selections die, CSS transitions re-trigger.
- ~90 `this.render()` call sites, and **no `requestAnimationFrame` anywhere in the
  file** — nothing coalesces renders. One user action touching two pieces of state
  rebuilds the world twice (e.g. `sendText` renders at send, again on error).
- Background traffic repaints the foreground: `pollRecv` (every `RECV_POLL_MS` = 4s,
  `constants.ts` line 4) full-renders the shell whenever a contact flips presence, mail
  arrives, or the outbox flushes — the gate is at lines ~1589–1609. A presence flip
  while the user scrolls a different pane destroys its hover state and re-parses the
  whole shell for a dot nobody is looking at.
- Three interval timers (`pollRecv` 4s, drops refresh 2.5s, Tor pill 2.5s) plus a
  presence heartbeat; `pollRecv` alone is 3–5 *sequential* HTTP round trips (heartbeat →
  recv → contacts → maybe mailbox → maybe conclaves). The drops poll is correctly scoped
  to its rail (started/stopped at lines ~2436–2439) and the Tor pill already patches in
  place (`updateTorPill`, line ~3122) — but the shell never sleeps: nothing pauses on
  `document.hidden` (the existing `visibilitychange` listener at ~1487 only resets chat
  scroll).
- No `content-visibility`, `contain:`, or `will-change` in `app.css` (5,665 lines):
  every rebuild and every scroll pays full layout for every row of chat history, mail
  list, torrents library, reads, and the zines grid, visible or not.
- Preserved strength: typing does **not** render (the composer draft lives in the
  textarea; `oninput` only auto-resizes, lines ~2900–2903). Keep it that way.
- The Tor pill's repaint-in-place is the proven in-repo pattern for patching one
  element per poll tick instead of re-rendering.

### Memory context (evidence, not the goal)

Measured 2026-10-17 via `smaps_rollup`/`status` (PSS, not RSS — ~142 MB of the app's
pages were swapped out at measurement time). The renderer held ~119 MB, ~70% of the
total footprint was the WebKitGTK platform, ~30% our code. For this goal the numbers
matter as the *symptom* of allocation churn: a whole DOM tree built every few seconds
sets the GC high-water, and GC pauses are the micro-stutters felt while typing and
scrolling. Reducing churn reduces both the jank and the footprint.

The daemon's PSS could not be measured (`/proc/<pid>/smaps_rollup` returned
"Permission denied" — see the 2026-10-17 entry in `DEEPSEEKPROBLEMS.MD`); RSS and swap
came from `/proc/<pid>/status`.

---

## Ranked plan (by felt improvement per effort)

### 1. `scheduleRender()` — coalesce all renders into one per frame

Replace the ~90 direct `this.render()` calls with `scheduleRender()`: set a dirty flag
and schedule a single render on `requestAnimationFrame`. User-initiated actions flush
immediately (still via rAF, same frame); poll-driven changes batch on a trailing delay
(~100 ms) so bursts collapse into one rebuild. No behavior change, mechanical swap —
but it removes double rebuilds per interaction and moves rebuilds off the keystroke/
scroll path.

Expected: typing and scrolling stop competing with rebuilds; double-render wastes gone.

**Status: DONE (2026-10-17).** `scheduleRender()` (user actions, rAF),
`scheduleBackgroundRender()` (polls/store subscriptions, 100 ms trailing batch), and
`flushRender()` live just after `render()` in `app.ts`; the batch length is
`RENDER_BATCH_MS` in `lib/constants.ts`. All call sites swapped, except eight that
stay synchronous on purpose: the two unlock-zoom sites (they measure a DOM rect, then
need the new tree mounted in the same tick for `flyMarkToRail`), the four rail search
fields and the atproto explore search (their caret survival is only guaranteed with
the wipe in the same tick — and their caret loss after one character is a pre-existing
bug, logged in `DEEPSEEKPROBLEMS.MD`), and the scheduler's own internals.

### 2. `content-visibility: auto` on long list rows

One CSS rule per long list (chat history, conclave messages, mail list, torrents
library, reads, zines grid) plus `contain-intrinsic-size` to keep the scrollbar stable.
Rows outside the viewport skip layout and paint. Effectively free virtualization
without JS, supported by WebKitGTK ≥ 2.40.

Expected: long panes stop costing O(n) per rebuild and per scroll; the biggest
"lighter on modest hardware" lever for near-zero risk.

**Status: PARTIAL (2026-10-17).** Applied to `.conv-rows`, `.mail-list`, `.reads-list`,
`.zines-grid`, and `.torrents-items` rows (block at the end of `app.css`). The local
WebKitGTK is 2.52.6, so the feature is live, not a graceful no-op. The message panes
(`.chat-messages`, `.conclave-messages`) are deliberately excluded for now: their
scroll-carry logic reads `scrollHeight` on every rebuild, and since every rebuild also
recreates the rows from scratch, the remembered intrinsic sizes reset each time —
estimate error could push the pinned-to-bottom detection past its 48 px tolerance.
They join when pane-scoped rendering (item 5) stops recreating rows.

### 3. Patch in place for background-only changes

Extend the `updateTorPill` pattern (line ~3122) to the other background deltas:
presence dots, unread badges, and — the important one — incoming messages into the
*visible* conversation. A message arriving in the open chat should append/patch the
message list, not rebuild rail + composer + modals. The `editing` gate at ~1589–1609
then becomes nearly unreachable, because background changes no longer repaint the
surface being edited.

Expected: hover states, selections, and focus survive background traffic entirely.

### 4. Idle awareness — let the CPU sleep

- Pause or back off all three timers when `document.hidden`; resume with one refresh on
  return.
- Exponential backoff for polls that return unchanged N times in a row.
- Parallelize or batch `pollRecv`'s sequential awaits (heartbeat/recv/contacts, plus
  conditional mailbox/conclaves reads) — fewer chained round trips is lower latency
  from "something happened" to "it is on screen", and fewer wakeups.

Expected: idle CPU near zero, quieter fan, lower battery draw — the machine sounds as
light as the UI looks; state changes land sooner.

**Status: PARTIAL (2026-10-17).** The visibility listener now lives in the `App`
constructor (once, instead of re-registered on every unlock cycle as before): while
hidden, the recv, Tor-pill, and drops timers all stop; on visible they restart, the
return is treated as a fresh read, and one coalesced render repaints from live state.
Still open: the unchanged-streak backoff and parallelizing `pollRecv`'s sequential
awaits. (The torrents store already polls adaptively with pane-open gating — no work
needed there.)

### 5. Pane-scoped renders (structural)

Split `render()` into independently patchable regions: rail, list column, active pane,
modal layer. A change to one region patches only that region's DOM. This is the
end-state of items 1 and 3 and the largest effort here — but every region scoped also
deletes wipe-compensation machinery (scroll carry, caret carry) instead of needing it.
Do it after 1–3 have landed and the remaining churn is measurable.

### 6. Voice jitter — daemon allocator hygiene

Same work the RAM analysis proposed, reframed: the daemon allocates request/response
and voice/paper-plane buffers in bursts, and glibc holds freed pages across per-arena
fragmentation — which shows up as *audio latency spikes*, not RSS. `MALLOC_ARENA_MAX=2`
in the launcher, or `mimalloc`/`jemalloc` for the daemon, or `malloc_trim(0)` on a
timer. Also audit that `paper_planes`/`voice_planes` (`crates/hush-daemon/src/main.rs`,
keyed by conclave/session id) are removed on room close/lock — a stale plane holds
sockets, buffers, and tasks that jitter the event loop.

---

## How we will know it worked

Verify against the running app, not just by reading code:

- **Idle render count zero**: instrument `render()` with a counter; with no input and
  no traffic for a minute, it must not tick.
- **Presence flip isolation**: a contact going offline while another rail is open must
  not rebuild the shell (verify via the same counter).
- **No long tasks**: main-thread tasks (Performance panel or `PerformanceObserver` for
  `longtask`) stay under ~50 ms during normal use with a long chat history loaded.
- **Idle CPU**: the WebKitWebProcess at ~0% CPU when the window is visible but idle.
- **Scroll parity**: scrolling a 2,000-message chat history feels the same at the top
  and after 30 minutes of accumulated messages.

---

## Out of scope for this goal (checked, do not re-litigate)

- **RAM-only levers**: tuned `[profile.release]`, AppImage vs deb/Flatpak, binary-size
  reduction — no effect on frame behavior or perceived smoothness. This doc replaces
  `app-shrinking.md` (which tracked raw-RAM options); the measurements that still
  serve as evidence are kept in "Memory context" above.
- **Native UI rewrite** (egui/iced/slint/GTK instead of the webview): parked in the
  previous analysis as unjustified by current numbers; not a smoothness step.
- **Daemon cache tuning** (`reads.rs`, `reads_store.rs`): already disciplined — capped
  memory cache, disk-cache budget with pruning, history caps truncated. No fruit.
- **`WEBKIT_DISABLE_COMPOSITING_MODE`**: trims compositor buffers at the cost of
  scroll smoothness — directly against this goal.

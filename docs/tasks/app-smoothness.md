# App smoothness — making Hush feel light

**Status:** Implemented in full, and verified at runtime only where the measurement did not
need the app. Item 1 done
(2026-10-17); item 2 done (2026-10-18, its `content-visibility` extended to the message
rows by item 5); item 3 done for the deltas it names, with two gaps recorded in it that it
did not name (2026-10-18); item 4's visibility pause and
parallelization done, its backoff declined (2026-10-18); item 5 done — the shell, every
rail, the two message panes, the Mail rail's two scroll boxes, the Chats list and the Trusts
pane, and the modal layer (2026-10-18); item 6 done — its audit half already satisfied, and
its allocator half now capped in the daemon and measured (2026-10-18). The four list search
fields stopped
re-rendering on 2026-10-18 (they narrow in place, which is what fixed their caret loss — see
item 1), and the Mail list and reader became keyed regions the same day, so the reload that
used to replace them now redraws their rows and messages in place — and later the same day the
folder counts joined them, so a mail reload no longer repaints the shell at all, and neither
does an invitation arriving (item 3). Items 7 and 8 were added
by a code audit on 2026-10-18, after the rest of this doc: they are costs found by reading,
not symptoms anyone reported. Item 7 was then measured and fixed the same day — the mail
list's body parsing is now memoized, which took a 200-message folder fill from a measured
~41 ms of main thread to a cache hit — and item 8 was measured and fixed the same day too:
its per-contact count was ~19.5 ms against ~0.8 ms for one pass, and it turned out to have
a second caller in the Files list that the audit had not named. The ranked plan's last three
items landed the same day: the caret carry was deleted (item 12), the Mail reader appends what
arrived instead of re-parsing the thread (item 13), and the daemon's arenas are capped (item
14). IRC was deliberately left alone: that pane is being redesigned, so nothing here was worth
doing against the current one.
Diagnosis grounded in code as of 2026-10-17; memory figures measured the same day against
the running `Hush_0.2.0_amd64.AppImage` (app v0.3.1 code). Verification so far is static
(`tsc`, production build, 280 unit tests) plus the item 7, 8 and 14 measurements and the
browser check behind item 7; the running-app checks in "How we will know it worked" are
still pending, and no run of the *app* has been possible in this environment.

**Goal:** The app should *feel* light: typing and scrolling never stutter, background
traffic never disturbs what the user is doing, the CPU idles when the user idles, and
long panes cost the same to scroll on day 30 as on day 1. Raw RAM is a secondary effect
of reaching this goal, not the target.

---

## Why it feels heavy today (evidence)

*Written 2026-10-17, before any of the plan landed, and kept as the diagnosis the plan was
written against. The status under each item is what is true now; three of the claims below
have since been addressed and are listed here only as the starting point — the root wipe
and the missing `requestAnimationFrame` (item 1), and the missing `content-visibility`
(item 2).*

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
  presence heartbeat; `pollRecv` alone was 3–5 *sequential* HTTP round trips (heartbeat →
  recv → contacts → maybe mailbox → maybe conclaves). Since 2026-10-18 the independent
  reads go out in parallel — see item 4. The drops poll is correctly scoped
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
`RENDER_BATCH_MS` in `lib/constants.ts`. All call sites swapped, except four that stay
synchronous on purpose: the two unlock-zoom sites (they measure a DOM rect, then need the
new tree mounted in the same tick for `flyMarkToRail`), the atproto explore search (its
filter regroups and recounts the directory, so the modal is genuinely rebuilt — it
re-queries the fresh field and puts the caret back itself), and the scheduler's own
internals.

The four rail search fields — Chats, Mail, Drops, and the torrents library — were the
other synchronous sites, and are no longer render sites at all. They narrow their lists in
place (2026-10-18), which is what fixed their caret loss: a keystroke used to rebuild the
shell, replacing the very field it came from.

### 2. `content-visibility: auto` on long list rows

One CSS rule per long list (chat history, conclave messages, mail list, torrents
library, reads, zines grid) plus `contain-intrinsic-size` to keep the scrollbar stable.
Rows outside the viewport skip layout and paint. Effectively free virtualization
without JS, supported by WebKitGTK ≥ 2.40.

Expected: long panes stop costing O(n) per rebuild and per scroll; the biggest
"lighter on modest hardware" lever for near-zero risk.

**Status: DONE (2026-10-18).** Applied to `.conv-rows`, `.mail-list`, `.reads-list`,
`.zines-grid`, and `.torrents-items` rows (block at the end of `app.css`), and now to the
message rows — `.chat-messages > .bubble-group` and `.conclave-messages > .bubble-group`.
The exclusion below was lifted by item 5, which stopped recreating those rows: they persist
across a render now, so the heights the containment rule remembers survive, which is
exactly what the pinned-to-bottom check needs when it reads `scrollHeight`. Day separators
are deliberately left out of the rule — they are short and constant, and an estimate for
them would only skew the sum. `pinToBottom` (`lib/paneScroll.ts`) reads `scrollHeight`
twice for the same reason: the first write is what brings the last estimated rows into
view, and the second read is then the real bottom, so the pane does not settle short of it
and look to the next arrival like a reader who scrolled away (2 unit tests). The local
WebKitGTK is 2.52.6, so the feature is live, not a graceful no-op.

The original exclusion, for the record: *the message panes are deliberately excluded for
now — their scroll-carry logic reads `scrollHeight` on every rebuild, and since every
rebuild also recreates the rows from scratch, the remembered intrinsic sizes reset each
time, so estimate error could push the pinned-to-bottom detection past its 48 px
tolerance.*

Not measured at runtime: this environment cannot run the app (see `DEEPSEEKPROBLEMS.MD`,
2026-10-18). The claim rests on the code, the engine version, and the unit tests.

### 3. Patch in place for background-only changes

Extend the `updateTorPill` pattern (line ~3122) to the other background deltas:
presence dots, unread badges, and — the important one — incoming messages into the
*visible* conversation. A message arriving in the open chat should append/patch the
message list, not rebuild rail + composer + modals. The `editing` gate at ~1589–1609
then becomes nearly unreachable, because background changes no longer repaint the
surface being edited.

Expected: hover states, selections, and focus survive background traffic entirely.

**Status: DONE for the deltas it names (2026-10-18), with two gaps recorded below that the
item did not name.** `patchBackground()` in `app.ts` runs before the poll
decides to rebuild, and covers the deltas the item names:

- **Presence dots** — the Chats list rows, the Trusts pane rows, and the open chat's
  header. The open chat's composer is rebuilt on its own when the contact's presence
  changes (the same `renderComposer` the first paint uses, so the two cannot disagree),
  carrying the draft and the caret into the fresh box.
- **Unread badges** — the rail's count pills, the Chats rows' unread rings, and the
  conclave cards' unread pills, all from the stores. IRC keeps its own owner: its badge
  is a count plus a mention dot, and `updateIrcRailBadge` now draws both from the rail's
  first paint too (a render used to drop a mention dot that was already on screen).
- **Incoming messages into the visible conversation** — appended, not rebuilt. The
  decision lives in `lib/messageAppend.ts` (`planAppend`, 13 unit tests): which messages
  the pane is missing, whether the first continues the run already on screen (the old
  last bubble loses its tail), and whether the day heading repeats. It returns null when
  the pane is out of step with the store — a reload, a prune — and the poll then rebuilds
  rather than patching a stale pane into looking current. Both panes use it: the 1:1
  thread and the conclave chat.
- Builders were extracted so the first paint and the append share them (`convRow`,
  `chatBubble`, `conclaveGroupEl`, `conclaveBubble`, `railBadgeEl`, `conclaveUnreadEl`,
  `daySepEl`, `orderedTrusts`), and rows carry the id/nick they were drawn for, which is
  how the patch finds its place without depending on DOM order. (`convListContacts`, once
  the filtered list this bullet named, is gone: the search hides rows rather than changing
  which ones exist, so the patch walks `chatContacts()` directly.)

The poll now rebuilds the shell for exactly three things: a mail reload, a conclave whose
*structure* moved, and a voice ring (its prompt mounts above the shell). The conclave case
needed a new signal — `loadConclaves` returns `{ changed, grewOnly }`, where `grewOnly`
means nothing changed but arriving messages, so the cards and the open chat can be patched
(7 new unit tests). The `editing` gate at the end of `pollRecv` therefore now guards only
those three, instead of every presence flip.

**The mail reload left this list (2026-10-18).** It was the loosest of the three, and the whole
reason was one region: the Mail rail's left column draws a count per folder, and nothing patched
those. `patchMailReload` now paints them where they are — `folderBadge` is the one read behind a
folder's number, shared by the column's first paint and by the patch, so the two cannot disagree
about whether a number is an unread summons or just a count — and brings the rail's list or
reader forward through that region's own `update`, which is what a keyed region is for. Off the
Mail rail a reload owes nothing beyond the rail's own mail badge, which `patchRailBadges`
already owned.

**And it did not work for a day, silently (2026-10-18, found while doing item 8).**
`patchMailReload` read its region out of `this.root` — `findRegion(this.root, spec.key)` — and the
regions are the *shell's* children, not the root's: the root holds the shell, the ring banner and
the dialogs above it. `findRegion` walks `parent.children`, so it found nothing, every time, and
"nothing found" is the patch's own way of saying "rebuild instead". So every mail reload still
cost a full render, and the paragraph above was wrong when it was written. What hid it is that
the damage is small: the regions are keyed, so the render that happened kept the list, the reader
and the folder column anyway — the caret and the scroll survived, and only the work was wasted.
Item 8 needed the patch to be real (the Drops folder's two bodies are keyed now and their carets
depend on the patch path being taken), so it is fixed: the lookup moved into
`updateRegionInPlace` in `lib/regions.ts`, which takes the parent and is given the shell. The
function is unit-tested against fakes, including the case that caused this — a parent one level
above the regions finds none, and says so — because nothing in this environment can run the
shell.

**An invitation left this list too (2026-10-18).** `patchConclaveCards` used to give up whenever
the number of invitation cards moved, and that was the last thing besides a ring that repainted
the shell on a background event: an invitation arriving is not a conclave appearing, so it is not
a card, and the box above the cards had to appear and disappear. `patchConclaveInvites` draws it
where it is — the whole box at once, which is honest, because it holds no caret, scroll or
selection — and `invitesAreCurrent` is what keeps it from being redrawn on every presence flip
and taking the Accept button out from under the pointer. An *introduction* still forces a render,
and must: the consent modal is mounted from the render, so the render is what shows it.

The conclave members row and header are closed by analysis rather than by patching, and the
analysis is worth recording because it says no. They are rebuilt by `updateConclaveDetail` on
every update of the keyed detail pane, but everything they draw — a trustee joining, leaving or
being kicked, a nick, the roster counts, and the trust facts the member menus are built from — is
a structure change, which `loadConclaves` already reports as `grewOnly: false` and which forces
the render that rebuilds them. So no delta reaches them without one, apart from an unrelated
render (a ring), and keeping them through that would need a signature over everything a chip
draws — including its menu items, which are built from the trust list, so the signature would
have to repeat the builder's own reasoning about who can chat and who can be introduced. That is
the drift hazard this doc keeps naming, paid for a hover state. An equality check cannot stand in
for it either: every `OverflowMenu` carries its own id, so two identical chips are never equal as
nodes.

Found while doing this, and **fixed on 2026-10-18** — a contact's **reach** moving was invisible
to the poll. `presenceChanged` compared `online` and the contact count, so a trust going from full
to mail-only changed nothing the poll noticed, `patchBackground` was never asked, and the menus
built from `this.contacts` — a trust row's, a chat header's, a conclave member chip's — kept
offering actions the boundary would now refuse until some other delta forced a render.

The fix splits the two facts rather than widening one. `pollRecv` now computes `reachChanged`
beside `presenceChanged` (both over the contact set, so a contact arriving or leaving counts as
both), and a reach move goes straight to `scheduleBackgroundRender` without asking the patch —
because a reach move rewrites menus, and menus are drawn once and never patched. What that render
costs is then up to the region keys, which is the part the keying work had already decided:

- The Trusts pane rebuilds, and should: `trustRowShape` carries both reaches and the mail address,
  so its key moves, and `patchTrustRows` still refuses a moved shape.
- The Chats list does **not** rebuild, and should not: its key is the row set plus each row's
  shown name and protocol. A reach that moves off `full` takes the row off the list altogether
  (`chatContacts`), so the row set already says so, and the dot the reach decides is rewritten in
  place by `patchConvRows`. Reading the reach into this key would have rebuilt the column — and
  taken the caret out of its search field — on the very render a reach move causes.
- The open chat rebuilds its header and composer, because `syncChatPane` keys both by the facts
  they draw, `reach` among them.
- A conclave's member row is rebuilt by `updateConclaveDetail` on every update of the keyed detail
  pane, so its chips are redrawn with the rest.

Both row-shape decisions now live in `lib/regionKeys.ts` (`chatRowShapes`, `trustRowShape`) with
tests over the reach, the protocol and the mail address, rather than in `app.ts` where nothing
could pin them. 207 tests, `tsc` and the production build pass. **No runtime check exists** — the
render counter still needs a manual pass, and the reach move is one of the deltas that pass has to
exercise (see `DEEPSEEKPROBLEMS.MD`).

The reader left this list on 2026-10-18 with the list, for the same reason the mail reload did —
nobody had noticed that a reload reset the open thread's scroll too.

The render counter the verification section asks for is in place
(`window.__HUSH_RENDER_COUNT__`), so the two checks that need it can now be run; the
append/tail/caret wiring still needs a manual pass, and so does the mail reload's new path —
it should leave that counter alone where it used to tick it. No run of the app was possible in
this environment (see `DEEPSEEKPROBLEMS.MD`, 2026-10-18).

### 4. Idle awareness — let the CPU sleep

- Pause or back off all three timers when `document.hidden`; resume with one refresh on
  return.
- Exponential backoff for polls that return unchanged N times in a row. **(declined —
  see status)**
- Parallelize or batch `pollRecv`'s sequential awaits (heartbeat/recv/contacts, plus
  conditional mailbox/conclaves reads) — fewer chained round trips is lower latency
  from "something happened" to "it is on screen", and fewer wakeups. **(done)**

Expected: idle CPU near zero, quieter fan, lower battery draw — the machine sounds as
light as the UI looks; state changes land sooner.

**Status: DONE (2026-10-18), with the backoff half declined.** The visibility listener now lives in the `App`
constructor (once, instead of re-registered on every unlock cycle as before): while
hidden, the recv, Tor-pill, drops, and Voice-meter timers stop, the Paper session's two
polls stop, and the one read that must survive — the voice ring — keeps running at the
same cadence in a reduced form. On visible they restart, the return is treated as a fresh
read, and one coalesced render repaints from live state.

**Parallelization: DONE (2026-10-18).** `pollRecv` now issues its four independent reads
in one `Promise.all` — heartbeat, `recv`, `contacts`, voice rooms — where they used to
be four sequential awaits, and the mailbox and conclave re-reads go out together when
both are due. One wrinkle the split has to respect: `recv` is the read that *consumes*
the inbox (the daemon acks the page it returns), so its response must always be used
once issued. The heartbeat therefore settles on its own with a local catch rather than
joining the group — a failed beat must not reject the batch and discard a `recv` that
already landed, or those messages would be drawn from the relay and never reach the UI.

The win is smaller than "four round trips become one" implies, and this status says so
rather than repeating the plan's claim: `presenceHeartbeat`, `recv`, and `contacts` all
dispatch through the daemon's single worker actor (`account_worker.rs`, `call` over a
one-slot channel), so they serialize daemon-side however the UI issues them. What the
change actually buys is (a) the UI↔daemon hop is no longer paid three times between
those calls, and (b) the voice read, which does *not* go through the worker
(`voice_state` reads `state.voices` directly), now overlaps `recv`'s relay fetch instead
of queueing behind it. No latency measurement was possible in this environment, so the
size of the win is reasoned, not measured.

**Backoff: DECLINED (2026-10-18).** The plan assumed backing off unchanged polls was
free idle-CPU. It is not: `/api/recv` is what pulls messages off the relay
(`crates/hush-core/src/account.rs`, an HTTP fetch per poll — there is no push connection
to buffer arrivals), so the poll interval *is* the message-receipt latency. A ceiling of
N seconds means an incoming message can sit on the relay for N seconds before the app
sees it, which contradicts this same item's "state changes land sooner". The two halves
of item 4 pull opposite ways; the 4 s cadence stays. If idle CPU ever needs the backoff
anyway, the honest version is context-aware — full backoff on the non-chat rails, 4 s
while a chat or conclave is open — not a blanket ceiling.

(The torrents store already polls adaptively with pane-open gating — no work needed
there.)

**Still open, and the plan undercounted it: "all three timers" is not all of them.**
Audited 2026-10-18. The visibility listener stops `pollTimer`, `torPillTimer`, and
`dropsPollTimer`, and those are the three it names — but two more repeating timers reach
the daemon and are not in the list:

- **The Paper session's polls** (`lib/paperStore.ts`): `PAPER_POLL_MS` = 1000 and
  `PAPER_PRESENCE_POLL_MS` = 500, started by `openPaper` and stopped by `closePaper`.
  Nothing stops them on `visibilitychange`, so a hidden window with a Paper tab open
  keeps making three daemon requests a second, indefinitely. This is the largest hole in
  the item: it is not a wakeup every four seconds, it is a 3 Hz loop.
- **The Voice section's input meter** (`app.ts:15913`): a 250 ms interval calling
  `voiceDeviceMeter`, torn down by `renderSettings` on any repaint — which is no help
  while hidden, because a hidden window does not repaint. Four requests a second for as
  long as the section is open and the window is away.

Both are scoped to a surface the user opened, so neither is a startup cost; both were
pure waste the moment the window is hidden.

**Both fixed (2026-10-18).** The meter is torn down by `stopVoiceMeter` on the hidden
branch — the same teardown `renderSettings` already did, applied on the one path where no
repaint is coming; the visible branch re-renders, which re-arms it, so the surface's own
contract does the restart. The paper's loops now go through `pausePaperSync` /
`resumePaperSync`, and every `setInterval` in `paperStore` lives in a single `startLoops`,
so there is one arming site to reason about. Resume polls once immediately rather than
waiting an interval, so the document and the cursors are current before the user looks at
them.

Two deliberate exceptions, recorded because they are the parts a later reader would
otherwise "fix":

- **The one-shot flush timers keep running** (`flushTimer`, `presenceFlushTimer`). An edit
  made just before the window went away must still reach the daemon, and that is one
  request rather than a loop.
- **`startLoops` reads `document.visibilityState` instead of remembering a "paused" flag.**
  The shell pauses from a handler that returns early while the vault is locked, so a
  remembered pause could outlive the pause and leave a paper opened later with no loops at
  all.

All seven repeating timers in the app are now accounted for: five stop while hidden, one
(`voiceRingTimer`) exists only while hidden, and the paper's two are covered by
`pausePaperSync`.

Also worth recording, because it is a trap rather than a task: **do not add a timer to
the auto-lock countdown.** `renderVault`'s "Nm until lock" is computed at render time
(`app.ts:14627`) and has no interval behind it. It goes stale between renders, which is a
correctness nit, and the obvious "fix" — a 1 Hz ticker — would put a permanent timer on
an idle app, against this item. If the countdown should move, it should move on the
existing poll, not on a new one.

While checking that, `AUTO_LOCK_MS` in `lib/constants.ts` turned out to be dead: declared
and referenced nowhere, while the pane actually reads the daemon's own policy through
`getAutoLockMinutes` / `getAutoLockMs` (`lib/api/uiPrefs.ts`), and the daemon is what
enforces it (`auto_lock_loop`, `ui_prefs::auto_lock_duration`). A stale UI copy of a
security-relevant policy is a trap in the same way as the countdown: the tempting move is
to wire it up, which would create the very timer this paragraph forbids. Removed
(2026-10-18).

**And the visibility pause already landed has a cost this doc did not record: it stops the
ring.** Found 2026-10-18 while looking at the two timers above. The voice ring is not a
timer and has no separate path — `refreshVoiceRooms()` is called from exactly one place,
inside `pollRecv` (`app.ts:1621`), and `pollRecv` is what the visibility handler stops. So
with the window hidden:

- the daemon still takes the call and stamps the ring, independently of the UI
  (`crates/hush-daemon/src/main.rs:6915`, `VOICE_RING_TTL_MS = 30_000` at line 6160);
- the hidden UI never reads `voiceState`, so `voiceRooms` goes stale, `activeRing()` cannot
  see the new ring, and there is no prompt and no chime;
- after 30 s the stamp expires daemon-side, so on return there is nothing left to answer;
- nothing else surfaces it. There is no notification plugin in `Cargo.toml` or
  `package.json`, no tray icon is ever created, and `request_user_attention` is never
  called — the generated ACL schema lists those core commands, the app does not use them.

What survives is the fallback the daemon's own comment describes: the room stays live, so
on return one poll repaints the conclave card's "voice live" chip and the user can still
join by hand. What is lost is the alert, and the 30 s window to answer. Note that comment
was written against a ~4 s poll and predates this item, so "a missed ring degrades to the
pill" is now only true once the window is visible again.

Whether that was acceptable was a product decision rather than a bug to fix quietly —
keeping the voice read alive costs one small request per 4 s against this item's whole
point, and alerting outside the webview needs a surface the app did not have.

**Fixed (2026-10-18), on the decision that a hidden window must still ring.** Two pieces:

- **The voice read survives the pause.** A `voiceRingTimer` makes one `voice_state` call per
  `RECV_POLL_MS` while the window is hidden — no `recv`, no presence, no store work behind
  it — and hands the read back to `pollRecv` on the visible branch. The chime already rides
  `refreshVoiceRooms`, so a hidden window rings audibly; what the read restores is the
  prompt, and with it the 30 s window to answer.
- **The OS is asked for attention** through `lib/attention.ts`, once per ring rather than at
  the poll cadence — a request every 4 s is a window flashing for 30 s, not an alert. On
  Linux that is GTK's urgency hint (tao 0.37 maps `request_user_attention` to
  `set_urgency_hint`), on macOS a dock bounce, on Windows a taskbar flash. It needs
  `core:window:allow-request-user-attention` in `capabilities/default.json`, which is not
  part of `core:default`; the resolved capability was read back from the build's
  `out/capabilities.json` rather than assumed.

The alternative — a desktop notification naming the caller — was declined as the larger
surface: it needs the notification plugin in both `Cargo.toml` and `package.json` plus a
permission prompt, where this needs no new dependency. The cost is now explicit rather than
silent: one voice state read per 4 s while hidden, which is the price of not missing calls.

One more cleanup in the same area: `lastRingChime` was write-only — assigned in three
places and read in none — while its comment described a gate the code did not have. It is
replaced by `voiceAlerted`, which is what that comment claimed it was for.

**And the ring was a chat chime, which is not a ringtone. Done 2026-10-18.** What the hidden
read above restored was *audible* only in the sense that `playChatSound()` fires while
hidden: the arrival is the same 0.35 s blip as a chat message, repeated once per poll for up
to 30 s. A call did not sound like a call, and the rhythm was the poll's rather than the
sound's. Both call states now have a real clip, and both loop:

- **`ring.mp3`** — 8.65 s, two ring bursts with gaps after each, so repeating it back to
  back is seamless. Heard by the vault being called.
- **`tone.mp3`** — 30.05 s of five identical cycles (2 s tone, 4 s silence), ending in
  silence on a cycle boundary, so it too repeats seamlessly. Heard by the vault that opened
  the room and is still alone in it: the ringback a caller expects while waiting.

A loop has a lifetime to manage, which makes these the app's only sounds that are a *level*
rather than an event. So the shell sends what should be audible and the native side makes
that true, and `set_voice_cue` is idempotent for exactly that reason: its caller is the 4 s
poll, and a non-idempotent command would restart the clip every poll — the same tempo bug in
a new place.

Three decisions, recorded because each is a case a later reader would otherwise flip:

- **The ring reaches a hidden window; the ringback does not.** The ring is the alert this
  whole item is about. The ringback exists to reassure someone *watching* the waiting
  screen, and a tone looping out of a minimised window is noise — indefinitely, since
  nothing ends it but a join or a leave.
- **The ring outranks the ringback.** Two conclaves can put one vault in both states at once
  — rung in one, waiting alone in the other — and the call arriving is the one to attend to.
- **The sound switch silences both.** It is the app's only sounds control and the ring
  already obeyed it. Its label and the Notifications copy said "chats", which was true only
  while the ring was a chat chime; both now say what the switch actually does.

The decision is a pure function (`voiceCueFor` in `lib/voiceCue.ts`) rather than a branch
inside the class, so the precedence above is pinned by 16 tests — including that an unread
status cannot mistake an unknown opener for this vault. `activeRing` moved there with it
(`activeRingIn`), since "which ring is live" feeds both the cue and the banner, and one
definition beats two.

**The native player had to be rebuilt for a loop, and the shape is not free choice.** cpal's
`Stream` is deliberately neither `Send` nor `Sync`, and rodio's `Sink` is not `Clone`, so
neither can live in a `static` or be handed to another thread. A dedicated worker thread
therefore owns the `OutputStream` and the shell talks to it over a channel. The device is
opened for a cue and dropped when the cue changes, which keeps the chat sound's per-play
stream rule (a stream held across a suspend silently swallows the next sound) and means an
app that is not in a call holds no audio device at all.

**Not verified: nobody has heard them.** The clips decode (`bundled_clips_decode`), the
command is permitted end to end (the generated ACL was read back), and the decision is
tested — but the app was not run and this environment has no speaker. See the `[OPEN]` entry
in `DEEPSEEKPROBLEMS.MD` for what to listen for.

### 5. Pane-scoped renders (structural)

Split `render()` into independently patchable regions: rail, list column, active pane,
modal layer. A change to one region patches only that region's DOM. This is the
end-state of items 1 and 3 and the largest effort here — but every region scoped also
deletes wipe-compensation machinery (scroll carry, caret carry) instead of needing it.
Do it after 1–3 have landed and the remaining churn is measurable.

**Status: DONE (2026-10-18) — the shell, every rail, the modal layer, and the machinery it was
written to delete.** Taken in the order this item asks for: the region with the most to gain,
and the machinery it deletes, before the regions that are merely cheap. The narrative below was
written when it was partial — it is kept as the record of what landed first, and the paragraph
after it says where the rest went.

What landed:

- **The root is no longer wiped.** The shell (`.shell`) is created once and kept, and each
  render syncs the regions inside it (`ensureShell` / `syncShell`). The lock screen is the
  one exception: it is painted into a wiped root, and the shell goes with it.
- **`lib/regions.ts`** is the mechanism. `planRegionSync` is the whole decision and is
  pure, so it is unit-tested without a DOM (10 tests): given the keys the shell's children
  carry and the keys the render wants, it says which are kept, which are replaced, and
  which are dropped. `syncRegions` executes that, and `rebuildAround` rebuilds a pane's
  parts around one element that must stay where it is.
- **The key is the whole contract, and the honest default is no key.** A region with a key
  that matches is left exactly where it is and brought up to date in place — never removed
  and re-inserted, because re-inserting is what detaches it. A region with no key is
  rebuilt, which cannot go stale. Each region that carries a key carries it because
  something inside it must not be detached: a search field and its caret, a scroll box and
  the reader's place in it, an open thread's history, a button the user is about to press.
- **The Conclaves column** is keyed by the shape it is in, whether there is a trust to meet
  with, and each card's name with what no patch rewrites about it (whether the meeting
  ended, how many trustees it shows, whether a voice session is live in it). Its invitation
  box and its cards are patched in place — `patchConclaveInvites`, `patchConclaveCards` —
  and the open meeting's mark moved into that patch, so clicking a card repaints the mark
  without rebuilding the filter field.
- **The ring prompt** is synced by key on the root, where it mounts above the shell. It is the
  one surface that answers a knock, so it shows on every rail, and a render caused by something
  else — someone else joining the room, a message arriving, a click on another rail — used to
  replace it, taking the focus off "Answer" mid-tab and re-announcing an `alertdialog` whose text
  had not changed. The key is the ring and the two names on it (`ringPromptKey`).
- **The 1:1 thread** is keyed by its conversation, and so are its parts: the header by the
  facts it draws (nick, display name, protocol, presence, reach, mute), the composer by the
  conversation and the reach — the two facts its structure is drawn from, since a mail-only
  trust has the reason instead of a box — and the message list by the conversation. The
  composer's presence half is patched in place (`updateComposer`), so a peer going offline no
  longer detaches a box the user is typing in. The list is patched, never recreated, so an
  unrelated repaint no longer re-parses the whole history.
- **The conclaves pane** keeps the open meeting's detail pane, which keeps its chat: the
  pane is rebuilt around the live `.conclave-messages` (`updateConclaveDetail`), with the
  fresh pane drawn as a scaffold that skips the bubbles it would throw away.
- **The Mail rail's four scroll boxes** keep the reader's place. The list pane is keyed by the
  folder, not by the rail: the toolbar is only built once per key, so the folder name baked
  into it is what makes a folder switch a rebuild and a mail reload an update (`fillMailList`
  redraws the rows in place). The reader is keyed by the thread, and updates through the same
  scaffold-transplant the conclave pane uses — now a shared helper, `transplantAround` in
  `lib/regions.ts`, which both callers were doing by hand. The Drops folder's list and reader
  joined them (2026-10-18, item 8), keyed by a constant and by the drop, with `fillDropList` as
  their row patch. The other Mail bodies — compose and Drafts — are unkeyed, and which body is
  up is decided in one place (`mailBody`) so the dispatch and the key cannot disagree about it.
  That one place is also what decides that a drop the vault no longer holds is not a body: the
  folder falls back to its list, rather than a key promising a reader while an update draws a
  list under it.
- **The rail, the Chats list and the Trusts pane** joined them (2026-10-18), which is the
  part of this item that the caret in a list's search field was waiting for. The rail's key is
  the current rail and the torrent count: the three unread badges are rewritten where they
  stand by `patchRailBadges`, and the IRC badge has a writer of its own, so a count moving
  must not rebuild the nav and take the hover under the pointer with it. The Chats list and
  the Trusts pane are keyed by the rows they list and what each row was drawn for — a name and
  a reach — and are brought forward by the same patches the poll runs (`patchConvRows`,
  `patchTrustRows`), which is what leaves the field, the caret in it, and the scroll box
  around it where they are. The keys are functions in `lib/regionKeys.ts` rather than template
  strings written at the registration site, because what is *not* in a key is the whole of its
  reasoning and both ways of getting it wrong are silent: a key that gains a field is a region
  that rebuilds on a change it could have patched, and a key that loses one is a pane showing
  something that is no longer true. `convListBody` is read by both the builder and the key, so
  the two cannot disagree about which of the two nothings the Chats list is showing.
- **The scroll carry is deleted.** `chatScrollPos`, `conclaveScrollPos`,
  `pendingChatScroll`, `pendingConclaveScroll`, `paneScrollTarget`, and `applyPaneScroll`
  are gone; `lib/paneScroll.ts` is down to `isAtBottom` and `pinToBottom`. Nothing has to
  be carried because nothing is detached — an element that stays in the document keeps its
  own `scrollTop`. That is measured, not assumed: in the webview engine a detached scroll
  box reports `scrollTop` 0, and reports 0 again after re-attachment in the same task.
- **The caret carry is deleted too** (2026-10-18, plan item 12), once the last regions that
  needed it were keyed. It used to capture the focused composer's value and caret before the
  swap and write both back after, which is wipe compensation in its purest form. It went with
  the wipe: the chat composer is keyed by the conversation and the reach and its presence half
  is patched in place, the conclave composer sits in a detail pane keyed by the meeting, its
  tab and whether it has ended, and the modal layer is synced by shape — so a repaint that is
  not a conversation switch, a tab switch or an end does not touch either box. The two
  `data-composer-key` stamps the carry read are gone with it: nothing else ever used them.
  What a rebuild still costs is stated rather than implied: a *conversation switch* is a
  different box (the draft belongs to the one that left), a *reach change* replaces the box
  with the reason it cannot be used, and the Mail rail's compose body is still unkeyed, so a
  mail reload redraws it — its text survives because those fields live in state, and its caret
  does not. That last one is the only draft-losing rebuild left in the app, and it predates
  this work rather than being caused by it.
- **The modal layer** is no longer wiped either, which it depended on: a dialog the render
  still wants is taken out by hand before its replacement mounts, and a dialog the render
  dropped is handed to `leaveModal`, which moves it over the fresh UI for the reverse zoom.
  Dialogs are rebuilt only when their drawn facts move — see item 11 in the list below.

What is left: nothing on this list. The last three items were the ones that were not about
wipe compensation at all — the caret carry's deletion (item 12), the Mail reader's delta append
(item 13) and the daemon's allocator (item 14) — and all three landed on 2026-10-18. What
remains is the manual pass (item 0 below) and the two measurements that need more than this
environment has: the running-app checks, and the audio-latency half of item 14.
What is left of item 5: nothing that a key can reach. Every column named in the paragraph this
replaces is keyed as of 2026-10-18 (the conclaves column as item 4, the Zines panes as item 7,
the Reads rail as item 6, the torrents library as item 9, the Mail folders as item 5, the Drops
pair as item 8, the Vault and Settings as item 10, and the modal layer as item 11). Two
exceptions, both decisions rather than gaps: IRC, whose pane is being redesigned and is
deliberately left unkeyed, and the Mail rail's compose body, which is still built on every
render — its text lives in state so a reload does not lose it, and its caret goes with the
rebuild.

One consequence worth naming, because it is the piece of this that a reader will otherwise
hit: the list search fields live inside the list columns, so their keystrokes repaint nothing,
and a render that still happens leaves the column they sit in alone wherever the column is
keyed. What is left under it is the fields in the two panes above that are rebuilt on purpose:
IRC's, and the Mail compose body's. Every other search field in the app — Chats, Trusts, Mail's
list and Drops folder, the conclaves filter, the torrents library, the notes board, Settings —
now keeps its caret through a background tick and through a user action on another rail, and
the dialog fields keep theirs through a tick as well (item 11).

A caveat that keying does not fix, since the contents are still redrawn: the Mail list's rows
are redrawn whole on a reload, so a deep position is kept by pixel offset rather than by scroll
anchoring — all the anchors are destroyed at once, so the browser has nothing to anchor to, and
the list draws against `contain-intrinsic-size: auto 72px` estimates for rows that were never
rendered, so that position can land a little off. The reader had the same problem and no longer
does: item 13 appends the messages that arrived, keeps the ones on screen, and holds the
reader's place with an anchor — the topmost visible card and its offset — because a mail thread
is newest first and an arrival is inserted above the viewport. The list's rows are a smaller
prize (a row is one line of text and a memoized preview, against a whole sanitized body) and
are left as they are.

Not verified at runtime: this environment cannot run the *app* (see `DEEPSEEKPROBLEMS.MD`,
2026-10-18). `tsc`, the 280 unit tests, and the production build pass. Nothing exercises
`syncRegions`, `transplantAround`, or the in-place pane paths against a real DOM — the tests
cover `planRegionSync`, which is the part that can be reasoned about without one, and the Mail
rail's key shape is now one of its cases. `mailBody`, `updateMailList`, `fillMailList`,
`updateMailThread`, and `fillMailThread` are not covered by anything: they read `this` and
build DOM, so the key they agree with is asserted only in the shape the tests pin, not against
the app. The reader's split into chrome and messages is covered by nothing at all, and the
anchor that replaced its pixel offset is reasoned about rather than measured — no
`getBoundingClientRect` has been read back in a real engine here.

The rail, the Chats list and the Trusts pane (2026-10-18) are in the same position, one step
further along: their keys are pure functions with 11 tests, and those tests pin the *shape* of
each key — that the rail's is the rail and the torrent count and not the badges, that the
Chats key is the body, the appear-offline flag and the rows and neither the query nor anything
`patchConvRows` rewrites — but nothing asserts that `app.ts` passes those functions what they
mean. `convListBody`, `convListKey`, `trustsKey`, `syncConvList` and `syncTrustsPane` read
`this` and build or patch DOM, and are covered by nothing. The one check that would catch a key
that is too *small* is the manual one above; a key that is too large cannot be caught by
reading the tests at all, only by the render counter staying flat.

### 6. Voice jitter — daemon allocator hygiene

Same work the RAM analysis proposed, reframed: the daemon allocates request/response
and voice/paper-plane buffers in bursts, and glibc holds freed pages across per-arena
fragmentation — which shows up as *audio latency spikes*, not RSS. `MALLOC_ARENA_MAX=2`
in the launcher, or `mimalloc`/`jemalloc` for the daemon, or `malloc_trim(0)` on a
timer. Also audit that `paper_planes`/`voice_planes` (`crates/hush-daemon/src/main.rs`,
keyed by conclave/session id) are removed on room close/lock — a stale plane holds
sockets, buffers, and tasks that jitter the event loop.

**Status: DONE (2026-10-18) for the allocator half, which is now capped in-process; the
audit half was already satisfied in code.**

*The allocator.* The cap is applied by the daemon itself, in
`crates/hush-daemon/src/alloc.rs` (`mallopt(M_ARENA_MAX, 2)`), rather than by a launcher.
The plan said "in the launcher", and that is where it would have been forgotten: the
daemon is started from three places that matter (`scripts/run-one-ui.sh`, the bundled
sidecar the desktop shell spawns in `daemon_supervisor.rs`, and
`packaging/systemd/user/hush-daemon.service`) plus a dozen smoke scripts and the README's
by-hand instructions, and a launcher that misses it regresses silently. In-process it
cannot be missed, and `main` now builds the tokio runtime by hand instead of with
`#[tokio::main]` so the call happens while the process still has one thread — the
attribute would have run it with the workers already spawned. `MALLOC_ARENA_MAX` set in
the environment still wins, because that is the one escape hatch a latency investigation
would reach for. The daemon logs which of the two happened at startup
(`hush-daemon allocator: arenas<=2`).

*Measured, on this host (8 cores), against the real debug binary started the way
`run-one-ui.sh` starts it, both runs at 11 threads with the same 300-request load:* the
cap takes the daemon's address space from **753 MB to 163 MB** — a difference of 576 MB,
which is exactly nine 64 MiB glibc arena reservations (589,856 kB ÷ 65,536 kB = 9.0).
Reproduced twice, and the cap is exactly two arenas rather than approximately: the capped
run's 162,860 kB is its ~32 MB base plus two 64 MiB regions. RSS moved much less (86 MB
capped, 97 MB uncapped), which is the point the item makes — this is an address-space and
reuse fix, not an RSS one.

*Not measured, and this is the honest limit of the above:* the audio latency spikes the
item is actually for. Two reasons, both concrete. The daemon sets `PR_SET_DUMPABLE=0` as
part of its own hardening (`security::harden_process`), so `/proc/<pid>/maps` and `smaps`
are root-only and the arena mappings had to be inferred from `VmSize` rather than counted;
and an audio measurement needs an unlocked vault, a conclave, and a peer on the other end,
none of which this environment has. What is verified is that the tunable is accepted by
glibc, that it is applied before the runtime exists, and that it removes nine arena
reservations from a running daemon. Whether that removes the spikes is the measurement
still outstanding.

*Not done, and deliberately:* `malloc_trim(0)` on a timer, and `mimalloc`/`jemalloc`.
`malloc_trim` takes arena locks, which is a jitter source in the process the fix is for —
the wrong tool for a real-time path. A replacement allocator is the right next step *if*
the audio measurement shows the cap is not enough, and choosing between them is a
decision to make against that measurement rather than in advance of it.

The audit half is already satisfied in code, which this doc
previously did not record: `paper_planes_shutdown`/`voice_planes_shutdown` run on lock
(`crates/hush-daemon/src/main.rs:528,533`), `paper_plane_reset` (line 6126) and
`voice_plane_drop` (line 6192) remove one plane on close, and
`locking_the_vault_closes_the_paper_planes` (line 9335) covers the lock path. Not
exercised at runtime — the claim rests on the code and that one test.

---

### 7. The mail list parses every message body twice per row

Found by auditing the search-field work (2026-10-18), which is what introduced it.

`mailRow` (`app.ts:11316`) calls `htmlToText(m.body)` twice for every row it draws:
once inside `mailSearchText(m)` for the `data-filter-text` haystack, and again inside
`preview(m)` for the snippet. `htmlToText` is not cheap — it runs `sanitizeHtml`, which
is `new DOMParser().parseFromString(html, "text/html")`, a full document parse, then a
recursive node walk, then an `innerHTML` serialize; the result is then parsed *again* into
a `div` and swept twice with `querySelectorAll` (`lib/richText.ts:159,189`). So one
`htmlToText` is roughly two parses and a serialize, and a 200-message folder fill pays for
400 of them. That is the largest single main-thread cost in the mail list, and it is paid
on every fill — including every mail reload, not only on open.

Nothing memoizes it: the body is flattened from scratch each time a row is built, and the
same body is flattened again when the reader draws it.

**Expected:** the mail list's fill cost drops by roughly half immediately (the duplicate
call), and by far more once the plain text is computed once per message rather than once
per draw. Fix candidates, cheapest first: have `mailRow` flatten once and pass the text to
both consumers; memoize `htmlToText` on the body string with a small bounded map; or
compute the haystack lazily, only when a query is non-empty, since `applyRowFilter` is its
only reader.

**Status: DONE (2026-10-18) — memoized, and measured before it was changed.**

*Measured.* The claim above was reasoned from the operation count, so the operation count
was replaced with a number first. The real `sanitizeHtml`/`htmlToText` bodies were
transcribed into a browser page and timed on three body shapes, 200 iterations each after a
warmup:

| body | chars | per call | a 200-row fill at two calls per row |
|---|---|---|---|
| one line | 42 | 0.033 ms | 13 ms |
| a paragraph, a link, a 3-item list | 170 | 0.102 ms | 41 ms |
| 12 paragraphs plus a 20-item list | 1951 | 0.340 ms | 136 ms |

So a folder of ordinary messages was ~41 ms of main thread per fill — the top of the 50 ms
long-task budget, and worse on slower hardware. **Caveat, because it matters:** that
measurement ran in Chromium (Electron 42 / Chrome 148), and the app runs in WebKitGTK, so
the figures are the shape and the order of magnitude, not the app's own numbers. What
transfers is that the cost is linear in the body and paid twice per row.

*Changed.* `htmlToText`'s parse moved into `parseBodyText`, wrapped by a new
`memoizeLru` (`lib/memo.ts`) as `flattenBody` with a 512-entry bound, so a body is
flattened once and then answered from the map. This subsumes the plan's first two
candidates rather than choosing between them: the duplicate call in `mailRow` is now a
cache hit, so no call-site change was needed, and the memo covers the reload case that
flattening once per row would not have touched. The plan's "cheapest first" ordering was
wrong on that point — the cheap fix and the durable fix are the same change.

*Verified.* `node --test` covers the memo's policy (7 tests: one compute per key, LRU
eviction, recency refresh on a hit, the bound, a bound below one, a cached `undefined`, a
non-integer bound). The wiring could not be unit-tested — `parseBodyText` needs a DOM the
runner does not have — so it was checked in a browser with `DOMParser` replaced by a
counting wrapper: a tagged body constructs one parser on the first call and **still one on
the second**, its output is identical to the unmemoized parse, a tagless body skips the
cache entirely and returns itself, and with a two-entry bound three bodies cost three
parses with a fourth when the evicted one is read again. `tsc`, the 153-test suite and the
production build are green.

*What is left of this item:* the reader still re-sanitizes every message body on every
thread fill (`bodyToDisplayHtml` at `app.ts:11587`), because that path calls `sanitizeHtml`
and not `htmlToText`. That is the same shape of waste but a smaller one — a thread is far
shorter than a folder — and it was not measured, so it is deliberately not being fixed on
speculation. It is the reason the `sanitizeHtml` memo was not added now.

### 8. The Vault tile's "Exchanged files" count is quadratic

Found by the same audit.

`renderVault`'s files tile computes its count as
`allFileMessages().length + this.contacts.reduce((n, c) => n + this.mailAttachmentCount(c.nickname), 0)`
(`app.ts:14657`). `mailAttachmentCount` calls `exchangedMailMessages(nick)`
(`app.ts:6964`), which builds three separate `listFolder` calls — each a filter **and a
sort** over the whole message store — per contact. So the tile is O(contacts × messages
log messages): fifty contacts against two thousand messages is a hundred and fifty sorts
of two thousand items, on every render of the Vault pane.

The count is one number and it is per-vault, not per-contact: the fix is to walk the three
folders once, group by contact, and sum. Same output, linear.

**Status: DONE (2026-10-18) — and it was in two places, not one.**

*The second site.* The audit named the Vault tile, because that is where the one-liner
reads as a count. Grepping the helper rather than the site found a second caller:
`renderFilesContactList`'s row loop (`app.ts:7042`) also asked `mailAttachmentCount(nick)`
once per contact, for the same reason and with the same cost. Both callers wanted *every*
contact's count, so the defect was in the helper's shape, not in either caller — which is
why the fix is in `mailStore.ts` rather than at either site.

*Measured, with synthetic data.* Replicating the per-contact helper against a single pass,
2 000 messages and 50 contacts: **19.5 ms** for the per-contact loop against **0.835 ms**
for one pass, about 23×. The synthetic store puts each message in exactly one folder, while
a real Inbox can hold most of the store, so the per-contact figure is if anything
conservative. Engine caveat as in item 7: Chromium, not WebKitGTK.

*Changed.* `mailAttachmentCounts(flags)` in `lib/mailStore.ts` walks inbox, sent and archive
once, deduplicating by id, and returns a `Map<nick, count>`; the Vault tile sums it over
`this.contacts`, and the Files list hoists one call above its loop and reads
`?? 0` per row. The per-contact `mailAttachmentCount` and its `reduce` are gone. It lives in
the store because it is a derivation over the store's own data — and because, unlike
`renderVault` and `renderFilesContactList`, it can be tested.

*Verified.* Six unit tests in `mailStore.test.ts`, the file's first to exercise the store
rather than free functions: the folder arithmetic, the dedup, absent-not-zero, Outbox mail,
trashed mail, and mail from a dropped contact. The dedup test was confirmed discriminating
by mutation — deleting the `seen` guard makes it report `actual: 8, expected: 4` — because a
dedup test written against a single-folder fixture would have passed either way. The two
call sites themselves are static-only (tsc and the build), like the rest of those panes.

*Checked and not the same defect:* the chat half of both counts — `fileCountForContact`,
`lastFileTimeForContact`, `contactsWithFiles` (`lib/chatStore.ts`) — looks like the same
shape but is not. Those read `getThread(nick)`, which is `threads.get(nick)` and not a scan
of the store, so the total across contacts is linear in the chat history rather than
quadratic in contacts. They do re-sort one contact's own thread several times, which is
waste but bounded by that thread. Do not re-audit this.

---

## Remaining work, in the order to take it

The ranked plan above is ordered by felt improvement per effort. This is what is left of it, in
the order to take the leftovers — each numbered step is a slice that stands on its own, and the
order is value per effort rather than item number. Every step ends the same way: `tsc --noEmit`,
the unit tests, and a production build, with the decisions extracted into pure functions and
tests wherever they can be, because that is the only verification available in this
environment.

**Not in this list: IRC.** The IRC pane is being redesigned, so work against the current one is
wasted. Its region, its search field and `renderIrc` stay unkeyed and unpatched on purpose, and
that is a decision rather than a gap. Whatever the redesign needs from this machinery — a keyed
region, a patch path, a `filterText` haystack — belongs to that work.

**Also not here: item 4's backoff half**, which was declined with a reason (see item 4), not
deferred.

0. **The manual pass, on a machine that can run the app.** Nothing below can be verified from
   here: no display, no DOM runner, and `app.ts` has no tests. Every check in "How we will know
   it worked" is still unrun, and the four that read `window.__HUSH_RENDER_COUNT__` are the ones
   that would catch a key that is too *large* — the failure the tests cannot see. This is
   blocked on hardware, not on work, and until it happens every "done" in this doc means "done
   in code, and read twice".
1. **The reach delta** (item 3's gap, recorded in `DEEPSEEKPROBLEMS.MD`) — **done 2026-10-18.**
   `pollRecv` computes `reachChanged` beside `presenceChanged` and sends a reach move straight to
   a render without asking the patch, because a reach move rewrites menus and menus are drawn
   once. The keys decide the cost: the Trusts pane rebuilds (its shape carries both reaches), the
   Chats list does not (the row set already carries a reach that leaves `full`, and the dot is
   patched), the open chat rebuilds its header and composer, and a conclave's member row is
   redrawn by the detail pane's update. Both row-shape decisions moved into `lib/regionKeys.ts`
   with tests. Still needs the manual pass: no runtime check of the move exists.
2. **The composer: patch its presence half instead of keying it** — **done 2026-10-18.** The key
   is now the conversation and the reach, which are the two facts its *structure* is drawn from
   (a mail-only trust has the reason instead of a box), and `updateComposer` writes the hint, the
   placeholder and the three disabled flags in place. Every state-dependent string and flag is
   decided once in `lib/composer.ts` and read by both the builder and the patch, so the two
   cannot drift — which is what the old rebuild-and-carry existed to prevent. The hint element is
   now always rendered and hidden when it has nothing to say, because a hint that comes and goes
   is a structure change, and a structure change is what detaches the box.
   Still needs the manual pass: type a draft, have the peer go offline, and confirm it is the same
   element afterwards with the draft, caret and height intact.
3. **The ring prompt: keep it while the same ring is showing** — **done 2026-10-18**, and the
   premise this was written with was wrong. There is no elapsed state to patch: the prompt draws
   the opener, the conclave and two buttons, nothing else, and the poll already refuses to render
   for a ring that has not changed (`ringSignature` — so "a 30 s ring renders once at each end"
   was already true, and a ring is not the frequent renderer this line claimed). What *was* wrong
   is that `render()` rebuilt the prompt on every render whatever the cause, and during a ring the
   poll renders for other reasons — someone else joining the room, a message arriving, a click on
   another rail. Each of those replaced the prompt, which took the focus off "Answer" mid-tab and
   re-announced an `alertdialog` whose text had not changed. It is now synced by key
   (`syncRingBanner`), the key is the ring plus the two names it shows (`ringPromptKey`, pinned
   exactly so the joined count cannot be added to it silently), and a rename of the opener or the
   conclave is what rebuilds it.
   Still needs the manual pass: tab to "Answer" during a ring, force a render, and confirm the
   button keeps focus.
4. **The conclaves list column** — **done 2026-10-18.** Both of its patches already existed
   (`patchConclaveCards`, `patchConclaveInvites`), so this was the audit the Chats and Trusts
   columns got: every part the card draws, and which of them a patch owns. Four drawn facts had
   no owner and are now in the key — the head's "New conclave" button and the empty state's copy
   both follow whether there is any trust to meet with, and a card's ended flag, member count and
   live-voice chip are drawn once and never rewritten (`conclaveCardShape`, pinned by tests). The
   open meeting's mark joined the patch's side instead, on the Chats rule: clicking a card must
   not rebuild the filter field. It is the last column on a rail whose background deltas are
   patched, so on this rail a background delta now repaints nothing but the numbers it changed.
   Still needs the manual pass: type in the filter, then let an invitation arrive and a voice
   session start, and confirm the field keeps its caret and the cards keep their places.
5. **The Mail rail's folder column** — **done 2026-10-18.** The plan's key was wrong on one half.
   The column draws four things: the address line, eight folder buttons, their counts and the open
   folder's mark. The folder set and its labels are written out in the builder, so "the folder set"
   cannot move and needs no place in the key. The counts were already `patchMailFolderCounts`'s.
   That left the address line and the open folder — and putting the open folder in the key would
   have rebuilt the column on every folder switch, dropping the focus off the button the user had
   just pressed with the keyboard, which is the same mistake item 3 made. So the mark joined the
   patch's side (`syncMailFolders` toggles `.active` beside the counts) and the key is the address
   line alone (`mailFoldersKey`, pinned exactly). All three ways a folder can change under the user
   — `openFolder`, saving a draft, sending one — end in a render, so the mark is never stale.
   Done when switching folders does not rebuild the column of folders.
   Still needs the manual pass: tab to a folder, press Enter, and confirm focus stays on it.
6. **The Reads rail's three regions** — **done 2026-10-18.** The plan's line for this one was about
   the wrong rail: the Reads rail has no search field and no `notesRenderRows` (that is the Vault's
   notes pane, item 10). What the three regions do have is a scroll box each — `.reads-surface-list`
   for the feeds, `.reads-list` for the stories, `.reads-main` for the article — and all three were
   unkeyed, so the poll rebuilt them every tick and the article jumped back to its top while it was
   being read.
   The audit turned up the reason this one needed something the earlier items did not. A coarse key
   is what keeps a scroll box — the mail list's rule — but a coarse key means the update runs on
   every render, and these updates are not cheap: the list redraws every row, and the reader
   re-sanitizes the whole article and rebuilds the comment tree. So the region spec grew a `shape`
   (`regions.ts`, tested): the key decides when the element may be replaced, the shape decides
   whether the update has anything to do, and the executor skips it when the element was already
   drawn from that shape. The reader's shape is deliberately coarse about its body — whether an
   article is there, not its HTML — because the point is to avoid the expensive comparison, not to
   make it.
   The keys are the three identities: the feeds it lists, the view the list is showing, the story
   and view the reader is on. What is drawn and moves is in the shapes, including the *rendered*
   "2 min ago" line rather than the `created_at` behind it — a shape built from the field would be
   identical while the row on screen said the wrong thing. `readsMetaText` is now one definition
   with four readers, which also removed a duplicate the reader and the list had each grown.
   The feeds column keeps its current mark and its Saved count in its update instead: the mark is
   on the button that was clicked, and the count is not known when the column is drawn. That also
   fixed the count never being cleared — unstarring the last story left the number behind.
   Done when a poll tick during an article leaves the article where it was.
   Still needs the manual pass: open an article, scroll it half way, and confirm the chat poll does
   not move it; then scroll the story list, open a story, and come back to the list in place.
7. **The zines panes** — **done 2026-10-18**, except for the part of the plan that was not true. The
   plan said keying the front page would make `pendingZinesScroll` unnecessary and that it is "the
   last scroll carry". Neither holds: opening a story *is* a different body, so the front page's
   element is replaced on the way in and there is nowhere for its scroll to be kept — the carry is
   the only way back, and it stays. (There is also the Settings carry, which item 10 keeps.) What
   the carry's comment claimed about "the thread pane above" was stale: that pane has been keyed for
   a while and its carry is long gone. The comment now says why this one survives.
   What the keying did buy is the rest of it. Both panes were unkeyed, so every poll tick rebuilt
   them: the front page jumped back to its masthead while you were browsing it, the reader jumped
   back to the top of the article while you were reading it, and the draft's title and section
   fields were replaced under the caret. Now the column is keyed on a constant (it has no identity
   of its own — it is the same column whatever the page beside it shows) and the page on the body
   it is showing: the front page with its filters, the story with its lane, the draft with the post
   it revises.
   Both regions carry shapes, and the page's is deliberately body-aware — only the body that is up
   is filled, so a corpus arriving in the background cannot redraw the reader and a detail arriving
   cannot redraw the front page behind it. The draft's shape is its post and nothing else, which is
   what keeps the caret in the field being typed in: its fields live in the DOM while it is open and
   its errors and Publish button are written in place, so there is nothing for a redraw to bring
   forward. `zineCardBits` is now one definition with two readers, like `readsMetaText`, so the
   "3 hours ago" on a card is compared as the text it prints.
   Done when a poll tick during an article, a browse or a draft leaves all three where they are.
   Still needs the manual pass: open a story, scroll it, and confirm a tick does not move it; then
   start a draft, type in the title, and confirm the caret survives a tick.
8. **The Drops pane.** Done (2026-10-18). Its search field was one of the two that still lost
   the caret, and `patchBackground` had no drops branch, so the row patch came first.
   Done when a drop arriving leaves the field, the caret and the reader's place alone.

   **What the audit changed.** The plan named two things and got the split wrong. "The Drops
   pane" is two panes in two different rails: the *Drops rail's* own pane (Tor status, the open
   window, the unverified switch), and the *Mail rail's Drops folder* — a list with a search
   field and a reader, which is where the caret and the reader's place actually live. A drop
   arriving moves the second, never the first; `refreshDrops` reads only Tor and the open
   window. Both halves are done, because the plan's `patchBackground` note was about the first.

   - **The Mail rail's Drops folder.** `mailBody` decides `dropList` or `dropView` (it reads the
     store now: a drop the vault no longer holds is not a body, so the folder falls back to its
     list — which is what keeps the region key honest, since a key promising `mailDropView:x`
     would otherwise be kept while the update drew a list under it). Both bodies are keyed:
     `mailDropList` is a constant (there is exactly one Drops folder, and the body decides which
     list this is) and `mailDropView:<id>` is the drop. The row patch is `fillDropList`, which
     redraws the rows and the note into `.mail-list` while the toolbar, the field and the scroll
     box stay where they are, plus `narrowDropList` — which looks the note up rather than
     carrying it, because a fill is free to have replaced it. `updateDropView` redraws inside
     the reader, which is its own scroll box. The "Generate drop key" button is the one piece
     that had to be handled by hand: the note is kept while its *kind* holds
     (`data-empty-kind`), so a mail reload that lands mid-request cannot swap a disabled button
     for a fresh enabled one and start a second key.
   - **The Drops rail.** Keyed on a constant with a shape over everything the pane draws
     (`dropsPaneShape`), and the deltas that used to cost a full render now paint one pane:
     `refreshDrops`, `connectTor`, `openDropCard` and `closeOpenDrop` call `patchDropsPane`,
     which falls back to a render when the pane is not on screen. The registry is deliberately
     *not* in the shape — it is a constant in the binary, so the only things that change which
     cards are drawn are the two switches, which are. The pane is the rail's scroll box, so
     opening a Drop no longer sends the directory back to its top.
   - **A toggle still redraws the pane**, and that is the one thing left here: the unverified
     switch is in the controls row inside the pane, and the grid below it has to change with it,
     so the fill rebuilds the switch too and a keyboard toggle loses the focus it just had. It
     had the same cost before this item (the whole shell was rebuilt), so it is not a
     regression — but keeping the controls row while the grid is redrawn is the fix, and it
     needs a seam (`transplantAround`) whose only payoff is that one focus. Not done.

   Verified by `tsc`, the production build and 250 unit tests — the shape's ten facts, and the
   patch path that finds a region in the right parent. **Not verified at runtime:** no drop
   arriving, no Tor connect, and no toggle has been exercised on a machine that can run the
   app, so the caret and the reader's place are claims about the code, not observations.
9. **The torrents library.** The same row patch first, with one honest question before it: its
   rows tick on every progress update, so the patch has to be cheap per row or the rebuild it
   replaces was the better deal.
   Done when a running download no longer interrupts typing in the filter.
   **Done (2026-10-18), and the answer to the honest question is that the patch is cheap.**
   What the pane actually had was a workaround, not a patch: the store's subscription returned
   early whenever an `input` inside `.torrents-pane` had focus, so a running download did not
   interrupt typing — it froze the whole pane instead. Progress stopped, a completed torrent did
   not appear, and the queue stayed as it was until the caret left the field. Everything else
   cost a full shell rebuild every three seconds.
   - **The pane is keyed on its body** (`torrentsPaneKey`): no key, the queue, or the library —
     with the library's path, order and layout, because those decide the row order, the
     container's class and which toggle is marked, and all three are drawn by the builder. The
     query is deliberately not in the key, and neither is `torrentsDragOver`: they are the
     elements' own state, and a key that carried them would rebuild the pane — and the field —
     on every keystroke.
   - **The update touches only what moved.** The chrome is rewritten in place where its own text
     moved — the two tab labels (counts the queue moves), the Add button's label and disabled
     state, the inline error, and the Add field's text when a submit cleared it behind the
     field's back — and the rows are reconciled. The pane is the rail's scroll box and holds the
     only two fields in this rail, so nothing in it is replaced by a poll.
   - **The queue's cards are reconciled one by one** (`torrentCardRegion`, keyed by id and
     status). The status is in the key because the card's actions are drawn from it and nothing
     rewrites them; inside a card the update writes the three things that moved — the badge's
     wording, the bar's width, the muted line — so the hover under a pointer resting on Pause
     survives the tick that redraws the numbers under it. `torrentCardMeta` is one definition
     with two readers, because that line ends in `readsTimeAgo` and the age moves on its own.
   - **The Library's rows are reconciled as a set**, keyed by their facts, because nothing patches
     a row: a size that moved or an age that ticked over is a row that has to be drawn again.
     The rows that did not move keep their element, and with it the hover on "Open inside vault".
   - **The narrowing moved off the closure.** The field's `oninput` used to install a `narrow`
     closure that captured the container and the note; a redraw replaces both, so the query
     typed before a redraw would have put the filtered rows back. `applyTorrentsNarrow` looks
     them up instead, and the note's wording is one definition (`torrentsLibraryNote`).
   - **`patchTorrentsPane`** is the store's path now: a poll paints this one pane and falls back
     to a render when it is not on screen. The focus guard is gone, because there is no longer a
     rebuild for it to hold back.

   Verified by `tsc`, the production build and 255 unit tests — the key's bodies, the pane
   shape's facts and a card's shape. **Not verified at runtime:** no display, so no poll has been
   watched against a live download, and "the caret survives" is a claim about the code — the
   field is never replaced, so it can only be right, but it has not been seen.
10. **The Vault rail and Settings.** Both are panes that only a user action changes, with scroll
    worth keeping; the Vault's file pane draws `.conv-row`s of its own, which is why
    `patchConvRows` scopes itself to `.conv-rows`.
    Done when moving between Settings sections keeps the page's scroll.
    **Done (2026-10-18), and the audit changed what "done" means for both halves.**

    - **Settings.** The page is one region on a constant key, because the pane *is* its scroll
      box and the nav's eleven sections are written out in the builder — so a section switch
      cannot move the nav, and the focus stays on the button the user just pressed. The section
      is a drawn fact in the shape, not part of the key, which is why the update resets the
      pane's scroll by hand: a new section opens at its top, and a repaint of the section
      already on screen leaves the reader exactly where they were. That is what the deleted
      `settingsScroll` carry used to do, and it is why the carry is gone rather than kept —
      there is no longer a rebuild for it to compensate. The body is one builder per section, so
      nothing in this page is patched piecewise; the shape is the whole decision.
    - **The Vault rail.** Four interchangeable bodies behind one region: the bento tile, the
      Files browser (contact list, or one contact's exchanged files), the Notes board (list, or
      the editor), and the Wallet. The key is which body is up, with the sub-identity — whose
      files, which note — carried in it (`vaultBodyKey`, pinned by tests), because opening a
      contact's files or a note's editor is arriving somewhere new and should start at the top.
      The pane is the rail's scroll box, so the shape is what keeps the reader's place across a
      repaint, and the update redraws the body inside the pane it already has.
    - **The shape is the whole job here**, because nothing in this rail is patched piecewise.
      That makes it the thing that stops a background tick — a message arriving, a presence
      flip, a TorBox poll — from rebuilding a body whose own facts did not move. It also means
      the *drafts* had to be kept out of it: the wallet's five fields, the notes editor's title
      and body, the notes search. They live in state and the redraw writes them back, so leaving
      them out loses nothing, while putting them in would make the reader's own typing a reason
      to rebuild the field they are typing in on the next tick from anywhere. Two of the wallet's
      drafts patch the button they arm instead — the token field's Add button already did, and
      the wipe field's Remove button now does, which fixes a defect found in this audit: typing
      `remove` never enabled it, because nothing re-read the draft until some unrelated render
      happened to.
    - **The clock is in the shape, in the form it prints.** The tile's auto-lock countdown and
      "last backup 3 days ago", and the board's "3 hours ago", are what the reader sees; a shape
      built from the fields behind them would be identical while the screen said the wrong
      thing. The two tile readings moved into one definition with two readers
      (`autoLockLines`, `backupTileHint`), as did the Files rows (`filesContactRows`), the row
      meta line (`fileRowMeta`) and a note's printed title (`noteRowTitle`, which also removed a
      four-way duplicate).
    - **The wallet's subscription guard stays**, narrowed and explained: the pane is a
      whole-body redraw, so a background store change landing while a field has focus would take
      the caret out of it. The phrase-card clause is gone — the phrase lives in state and the
      redraw reproduces it, and the copy button reads the state, not the DOM.

    Verified by `tsc`, the production build and 259 unit tests — the settings shape, the vault
    key's bodies and sub-identities. **Not verified at runtime:** no display, so no scroll has
    been watched across a section switch or a poll tick, and the caret claims are claims about
    the code (a draft is never in the shape, so the field is never rebuilt by its own typing —
    which can only be right, but it has not been seen).
11. **The modal layer — the hard one.** `mountModal` already keys each dialog; what it cannot do
    is tell an unchanged dialog from a changed one, so every dialog is rebuilt and a caret in
    one dies with it. This needs a signature per dialog, which is the same shape as a region key
    and the reason it is last.
    Done when typing into an open dialog survives a background structure change.
    **Done (2026-10-18), and the audit found one workaround to delete and one bug in the pairing.**

    - **The pairing is keyed, not positional.** `planRegionSync` pairs the shell's regions by
      position, because the shell's slots are the same every render. The dialog list is not: it
      is sparse, and closing the first of three dialogs moves the other two up a place, so
      pairing by position would rebuild a form the reader is typing into — for what is really
      one dialog leaving. `planModalSync` (regions.ts, ten tests) pairs by key, and keeps a
      dialog only when its shape is unchanged *and* it still sits after the previous kept one:
      a want that would have to reorder the stack rebuilds instead, because moving an element is
      what detaches it. `syncModals` walks that plan, inserts a rebuilt or new dialog after the
      previous slot's element (or before the next kept one, when it opened in front of a dialog
      already up), carries the launch point across a rebuild and drops what closed.
    - **Twenty-five shapes.** Each dialog's drawn facts, in `modalShape`: the name a
      confirmation prints, a busy flag on a button, a store's rows, an error line, a relative
      time. `addFriend` has no drawn facts at all — the invite card fills itself from an async
      call and the fields own their drafts — and says so with a constant, so it is never rebuilt
      while it is open.
    - **The drafts stay out, and the fields patch themselves.** Every field the reader types in
      is excluded even where the builder writes it out of state — the confirm-to-delete words,
      the note picker's query, the atproto forms, the conclave pickers' selections — because the
      reader's own typing must never be a reason to rebuild the field they are typing in. The
      controls that depend on a draft patch themselves in place instead: the delete buttons read
      the field and set `disabled` on input, and the zine share picker patches its send button.
    - **One workaround deleted, because the shape made it unnecessary.** The atproto Explore
      dialog's filter called `this.render()` on every keystroke and then re-queried the input to
      put the caret back — with the comment saying it was deferred "until the render itself
      stops rebuilding the modal". It does now: the filter is out of the shape and the list is
      rebuilt under the field (`fillList`), like every other search in the app.
    - **One duplicate removed.** `noteRowText` is now one definition with three readers (the
      board's rows, the picker's rows, and the shapes both are compared against), and
      `pickerItem` is one definition with six (the three pickers, each in its builder and in its
      shape), so a row cannot print one thing while its shape compares another.
    - **The progress bar is patched, not shaped.** `updatePrompt`'s percentage is written in
      place by `patchUpdateProgress`, so the shape carries whether a download is running and not
      how far it has got — otherwise every chunk would rebuild the dialog it is drawn in.
    - **`modalKeys` is gone.** The set existed to tell a closed dialog from an open one after the
      fact; the plan says which slots were dropped, so the bookkeeping went with it.

    Verified by `tsc`, the production build and 269 unit tests — the pairing's ten cases (a
    close, a shape moving, an open in front of and behind a kept dialog, a duplicate key, the
    reorder that must not happen). **Not verified at runtime:** the insertion order and the
    focus claims are claims about the code, and no dialog has been typed into on a machine that
    can run the app.
12. **Delete the caret carry**, once 2–11 are keyed — the composer capture and restore in
    `render()`. It is the last piece of wipe compensation left: the scroll carries are gone
    except the zines one (step 7), and this is the stated goal of this item — keying deletes
    machinery instead of needing it.
    **Done (2026-10-18), and what it was standing in for was already answered by the keys.**
    The capture read the focused composer's value and caret before the swap and wrote both back
    after, so the deletion was gated on showing that no repaint the app still performs can detach
    either box. Both are covered by keys that were already in place: the chat composer's pane key
    is `chatComposer:${nick}:${reach}` and its presence half is patched by `updateComposer`, and
    the conclave composer sits in a detail pane keyed by the meeting, its Chat/Paper tab and
    whether it has ended. So a poll tick — a presence flip, an arrival, a mail reload — reaches
    neither box. The two `data-composer-key` stamps went with the capture; nothing else read
    them. The rebuilds that remain are deliberate and are named rather than implied: a
    conversation switch is a different box, a reach change replaces the box with the reason it
    cannot be used, and a tab switch rebuilds the detail pane. The one unkeyed draft left is the
    Mail rail's compose body, which redraws on a mail reload — its text survives (the fields live
    in state) and its caret does not, and that predates this work.

    Verified by `tsc`, the production build and 269 unit tests. **Not verified at runtime:** no
    draft has been typed into on a machine that can run the app, so the claim that a tick leaves
    both boxes attached is a claim about the keys and the patches, not an observation.
13. **The Mail reader: redraw only what arrived.** Today a reload re-parses every message body in
    the thread and keeps the reader's place by pixel offset, because all its anchors are
    destroyed at once. `planAppend` is the shape of the fix, as it was for the message panes.
    Done when reloading a long thread is near-free and the place is kept by an anchor.
    **Done (2026-10-18), and the audit found three things the plan's wording did not cover.**

    The shape was right, but the direction was not: a mail thread is **newest first**
    (`getThread` sorts by `createdAt` descending), so arrivals go at the *head*, and
    `planAppend`'s prefix rule is the wrong end of the list. Rather than pass a flag into a
    function whose other half (runs, day separators) the reader would ignore, `messageAppend.ts`
    grew `planPrepend` — same contract, opposite end — and both now share one `matchesAt` check.
    Two functions that return different things is the honest shape; one function returning
    `continuesRun` and `daySep` to a caller that ignores them is a trap for the next reader.

    The "pixel offset" half was also not what the code did. `.mail-messages` is already the kept
    element (item 5), so `scrollTop` was never lost — it was *wrong*: prepending above the
    viewport shifts every card down by the new card's height, and a carried number scrolls the
    reader backwards by exactly that. So the place is now held by an **anchor**: the topmost
    visible card and its offset from the box's top edge, re-measured after the insert and applied
    as a delta. The reader on the newest message is the exception that needs no anchor — the top
    is the one position nothing can be inserted above — and is snapped to it, which is what makes
    an arrival visible when the reader is looking at the newest message.

    And the double parse: `updateMailThread` used to call `renderMailThread`, which parsed every
    body in the thread into a pane that `transplantAround` immediately discarded. The chrome
    (head + empty list) is now built by `mailThreadChrome`, so a reload parses no body it does
    not draw. `fillMailThread` remains as the rebuild fallback, and one `mailMessageCard` builds
    a card for both paths.

    Two drawn facts that are not the pane's key are patched rather than rebuilt: the `open`
    highlight (clicking another message in the same conversation keeps `mailThread:<id>`) moves
    by class, and the head is still redrawn every update because it reads unread state. Anything
    other than "what is drawn is the older end of the thread" rebuilds — a message trashed, a
    contact removed so their mail left the thread, or a reply whose clock puts it in the middle.

    Verified by `tsc`, the production build and 280 unit tests — eleven for `planPrepend` (the
    empty pane, the head arrival, a burst in the pane's own order, and four ways of being out of
    step) and four for `isAtTop`. **Not verified at runtime:** no anchor has been re-measured in a
    real engine here, and the two-rect read per arrival is reasoned about rather than measured.
14. **Item 6's allocator half.** `MALLOC_ARENA_MAX` in the launcher, or `mimalloc`/`jemalloc`,
    or `malloc_trim(0)` on a timer — plus the measurement of the audio latency spikes it is for,
    which needs a running daemon. The audit half is already satisfied in code.
    **Done (2026-10-18) for the cap, with the measurement split in two.** The cap went into the
    daemon rather than a launcher, and it is measured — the address-space half of it, on this
    host, against the real binary: 753 MB → 163 MB, nine fewer 64 MiB arena reservations. The
    audio-latency half is still outstanding, and the reason is concrete rather than a shrug: the
    daemon's own hardening makes its memory map root-only, and the audio path needs a vault, a
    conclave and a peer. Both halves, and the two options deliberately declined, are written up
    under item 6 above.

## How we will know it worked

Verify against the running app, not just by reading code:

- **Idle render count zero**: read `window.__HUSH_RENDER_COUNT__` in the webview
  console (incremented by `render()`); with no input and no traffic for a minute, it
  must not tick.
- **Presence flip isolation**: a contact going offline while another rail is open must
  not rebuild the shell (verify via the same counter — the presence patch is what
  answers it, so a rise here means the patch bailed).
- **Pane persistence** (item 5): with a long thread open, scroll up into history, then
  toggle *Appear offline* in the Chats list. The reader's place must not move, hovered
  rows must stay hovered, and a selection in the history must survive — the pane is
  keyed, so the repaint must not have touched it. The same in a conclave: switch to
  another rail and back, and the meeting opens on its newest message (the pane was
  dropped and rebuilt); change anything else and it must not move.
- **The composer is never detached** (item 12): type half a sentence into a chat, then let a
  presence flip, a message arrival and a mail reload land, one after another, without touching
  anything. The draft must still be there with the caret where it was, in the same element —
  this is the check that the deleted caret carry used to stand in for, and it is the one that
  fails if a key grows a field it should not (a presence flag, a display name). Then switch to
  another conversation and back: the box is expected to be empty, because that is a different
  box. Then do the same in a conclave, and switch its Chat/Paper tab away and back: the draft is
  expected to be gone, since the tab switch rebuilds the detail pane on purpose.
- **Mail pane persistence** (item 5): with a long folder open, scroll down into it, put
  the caret in the search box, and wait for a mail reload — an incoming message, or the
  Outbox flushing Queued to Sent. The rows are redrawn, so the *content* moves; the search
  field must keep the caret and what is typed in it, and the scroll box must stay where it
  was rather than jumping to the top. Expect small drift, not a jump (see item 5). Then
  open a long thread, scroll down into it, and wait for another reload: the reader must stay
  where it was. Opening a thread, and going back to the list, are both rebuilds — each
  starts at the top, and the search field keeps its text across a folder switch.
- **A mail arrival holds the reader's place** (item 13): open a long conversation and scroll
  down into the middle of it, so the top of the box is on a message rather than on the newest
  one. Let an arrival land (another device replies, or the Outbox flushes a Queued message into
  the same thread). The message the reader was looking at must not move — not by a pixel, since
  the arrival is inserted above it — and the new message must be there when they scroll up. Then
  scroll back to the very top and let another arrive: the reader must be on the new message, not
  left looking at the one it displaced. Then send a reply from this device while reading: it
  appears at the top the same way. Finally, file a message out of the open thread (trash it from
  the head's actions) — that is the rebuild path, so the reader is expected to land back at the
  top.
- **A mail reload repaints nothing** (item 3): with the Mail rail open, watch
  `window.__HUSH_RENDER_COUNT__` across a reload — a message arriving, or another device
  filing one. The counter must not tick, the folder counts must still be right (the Inbox
  badge, and the rail's own), and the list or the reader must be current. This is the check
  that would have caught the parent bug in `patchMailReload` (item 8): as shipped before that
  fix the counter ticked every time, because the patch never found its region. It must tick, as
  before, when the Mail rail is showing something the patch cannot bring forward: the compose
  form or Drafts. The drop list and the drop reader are keyed now, so they must *not* tick.
  Then do the same from another rail: an
  incoming message must move the Mail rail badge and nothing else.
- **The Drops folder keeps the caret** (item 8): open Mail → Drops with drops filed, put the
  caret in "Search drops" and type a few characters, then have a drop arrive (or another device
  file one). The rows are redrawn, so the *content* moves; the field must keep the caret and the
  query, and the scroll box must stay where it was. Then open a drop, scroll into it, and wait
  for another reload: the reader must stay where it was.
- **The Drops rail patches its own deltas** (item 8): with the Drops rail open, press
  "Connect Tor" and watch the counter — the Tor chips and the button must change without a
  render, the directory must not jump back to its top, and the error must appear inline. Then
  open a Drop and close it: the banner must come and go in place, and the counter must not tick
  for either. The unverified switch is the exception and is known: it redraws the pane, so the
  counter ticks and the switch loses the focus it was just given.
- **A running download repaints one pane** (item 9): start a torrent and leave the Torrents
  rail open. Watch `window.__HUSH_RENDER_COUNT__` across several polls — the counter must not
  tick, while the card's badge, bar and "1.2 GB / 3.0 GB" line keep moving and a card that
  finishes leaves the queue and appears in the Library. Then put the caret in the Library's
  search field and type: the rows must narrow as they go, the field must keep the caret, and the
  poll must keep painting behind it — this is the check that catches the old freeze, which
  protected the caret by stopping the pane entirely. Do the same in the Add field while a
  download runs. Hover a card's Pause button across a tick: it must not move or lose its hover.
  Switch the Library's sort and layout while the search field has focus: those two rebuild the
  pane (they are in its key), so the caret is expected to go — that is the one documented
  rebuild left in this pane.
- **The Vault rail and Settings keep their place** (item 10): in Settings, scroll the Appearance
  section down, switch to Voice and back — the section opens at its top, and the focus is on the
  button that was pressed. Scroll a section and let a background tick land (a message arriving,
  or a presence flip): the page must not move and the counter must not tick. In the Vault, open
  the Wallet, scroll to the bottom of the cards, and wait for a TorBox or presence poll: the
  cards must not jump to the top, and the counter must not tick. Then type a destination address
  in the Send card and let a message arrive: the caret must stay in the field. Open Notes, start
  a new note, type in the body, and let a tick land: the caret must stay in the body rather than
  being pulled back to the title, which is what a full rebuild used to do.
- **An invitation repaints nothing** (item 3): from another vault, invite this one to a
  conclave with the Conclaves rail open. The card must appear above the conclave cards, the
  counter must not tick, and Accept must work. Then hold the pointer over Accept while a
  presence flip lands — the button must not move or lose its hover, because the box is only
  redrawn when the invitations themselves change. Declining the last invitation with no
  conclaves in the list must show the empty state; that one is allowed to tick.
- **No long tasks**: main-thread tasks (Performance panel or `PerformanceObserver` for
  `longtask`) stay under ~50 ms during normal use with a long chat history loaded. Profile
  three fills specifically, because they are the ones with work proportional to the store
  and no test covers any of them: a long Mail folder's first open (item 7 — this one is now
  expected to pass, and the before/after is the check that the memo is working: the first
  fill after launch pays the parse, every reload after it should be near-free), the same
  folder's reload (item 7), and the Vault pane with many contacts (item 8 — now expected to
  pass, with the same before/after shape as item 7).
- **Search fields keep the caret** (items 1, 5 and 9): type a word into the Chats, Mail, Drops,
  and torrents-library search boxes one character at a time, at a normal typing speed, with a
  long list loaded. Every character must land in the field, and the list must narrow as it
  goes. Then do it again while a repaint lands: a presence flip is patched
  (`patchBackground`), so it never rebuilt the column the field sits in. What does rebuild it
  is a conclave whose structure moved, a voice ring, or a user action elsewhere — and in Mail,
  Chats, Trusts and the torrents Library, the columns keyed as of 2026-10-18, the field must
  keep the caret through all three, and through a mail reload and a torrent poll (checked
  above). The notes search and the Settings fields are keyed as of 2026-10-18 too, so they must
  keep the caret through a background tick; in the Wallet and the notes editor it is the same
  claim about a whole-body redraw rather than a patch. In the Drops pane it will not yet; IRC is
  deliberately excluded (its pane is being redesigned).
- **A dialog keeps its caret** (item 11): open Add a trust, type a name and a code, then let a
  presence flip or a message arrive — the fields must keep their text and the caret, and the tab
  you were on must stay the tab. Open Chat history and put a date in: a mail reload must not take
  the caret or reset the radios. Open Explore (Zines → Public lane → Explore) and type in the
  filter: the list must narrow as you go, character by character, with the caret staying — this
  is the one that used to render the whole app on every keystroke. Open the palette over a
  contact card and close the card underneath: the palette must not flash, lose its query or lose
  the caret, and the counter must not tick for the close beyond the one render that drops the
  card. Then check the two dialogs that *must* rebuild: a wallet balance arriving while the
  wallet's card is up, and a torrent finishing while its delete dialog is up — each redraws its
  own dialog only, and the counter ticks once.
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
- **CSS paint and compositing traps** (audited 2026-10-18, nothing there): `app.css` has
  no `backdrop-filter`, no `filter: blur`, no `will-change`, and no `transition: all`. Its
  four `animation: ... infinite` rules (`relay-pulse`, the key card's `spin`,
  `update-indeterminate`, `drops-pulse`) are each gated on a transient state class and stop
  when the state clears, so none runs on an idle shell. Do not re-audit this.
- **Continuous `requestAnimationFrame`**: there is none. The only `requestAnimationFrame`
  in the app is the coalesced render's one-shot frame (`app.ts:2335`); nothing drives a
  per-frame loop.

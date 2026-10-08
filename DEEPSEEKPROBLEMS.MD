# DeepSeek implementation problems

Running log of problems hit while implementing slices. Kept so the same dead ends are not
re-explored and so unverified claims stay visible instead of being quietly dropped.

**Convention:** append an entry when a problem is hit, not retroactively at the end of a
slice. Newest section first. One entry per problem, with what was attempted, the symptom, the
resolution, and the status.

Status tags: `FIXED` (resolved in-slice), `WORKED AROUND` (avoided, cause not fixed),
`OPEN` (unresolved, needs a different environment or a later task), `NO LONGER APPLICABLE`
(the configuration it described cannot occur any more).

---

## 2026-10-08 — Meetings rail (local record, sender-side fan-out)

Built the Meetings feature: a vault-held `MeetingRecord` with its own message log, a
`MessagePayload::Meeting { meeting_id, name, members[], body }` descriptor on every message,
sender-side fan-out over the members' 1:1 sessions, recipient-side materialisation from the
descriptor, daemon routes, and a Meetings rail in the UI. Problems hit:

### [FIXED] `hush-cli` matched on `MessagePayload` without a `Meeting` arm, so the workspace would not build

- **Attempted:** `cargo test --workspace` after wiring the core and daemon.
- **Symptom:** `error[E0004]: non-exhaustive patterns: 'MessagePayload::Meeting { .. }' not covered`
  at `crates/hush-cli/src/main.rs:147`. The core and daemon crates compiled; the CLI did not, and
  the whole workspace build failed, so no test ran.
- **Cause:** adding the enum variant made every existing `match` on `MessagePayload` non-exhaustive.
  The `recv` match in the CLI prints inbox items and is easy to forget because it is not part of the
  feature.
- **Resolution:** added a `Meeting` arm to the CLI that prints the meeting message. `cargo test
  --workspace` then passed. This is the same class of breakage as the existing `[OPEN] 350d2d1`
  entry above, but in a different crate: that entry names `account.rs`; the CLI was a second,
  unlisted site.
- **Lesson:** a new `MessagePayload` variant means grepping every `match` on it, not just the ones in
  the crates you are editing.

### [WORKED AROUND] Two ad-hoc daemons collided on the default socket path (tooling, not the repo)

- **Attempted:** run two daemons for an end-to-end Meetings check with a shared
  `HUSH_RUNTIME_DIR` and only `--addr` set.
- **Symptom:** the second daemon exited at startup:
  `Error: Custom { kind: AddrInUse, error: "another hush-daemon is already listening on
  <run-dir>/hush-daemon.sock (use --socket to pick a different path)" }`.
- **Resolution:** passed an explicit `--socket` per daemon in the throwaway script. The repo's
  `scripts/run-two-ui.sh` already does this; the fault was in my one-off script.
- **Lesson:** the default socket path is derived from the runtime dir, so two daemons sharing one
  runtime dir need explicit `--socket` values.

### [WORKED AROUND] `/api/invite` is `POST`, and the smoke script assumed `GET`

- **Attempted:** read the invite with `GET /api/invite`.
- **Symptom:** empty response, so invite extraction failed. The route is `post(invite)`.
- **Resolution:** switched the script to `POST`. Tooling-only; the route is unchanged and correct.

### [OPEN] The Meetings UI was not exercised in a running app

- **Attempted:** verify the rail, the list/detail panes, the create and add-member modals, and the
  multi-select `ContactPicker` end to end.
- **Symptom / limit:** no display was available, so the UI could not be driven in a real window. The
  feature was verified at the daemon HTTP layer with two live daemons plus a relay (create, send,
  fan-out, materialisation, append, membership add/remove, unknown-member 400), and the frontend was
  checked with `npx tsc --noEmit` (exit 0) and `ReadLints` (no errors) only.
- **Consequence:** the multi-select `ContactPicker` behaviour (chip tokens, keep-open-on-select,
  Backspace removing the last chip) and the layout are **unverified in a browser**. The single-select
  mail composer path is untouched and still typechecks, but it was not re-driven either.
- **Needed to close:** a manual pass in `scripts/run-two-ui.sh --dev` with two tabs.

### [OPEN] Membership changes are local only, and the UI says so

- **Attempted:** add/remove meeting members.
- **Reality:** a membership change edits only the actor's own `MeetingRecord.members`; it is not sent
  to anyone and does not change another client's member list or history. A recipient that receives a
  descriptor for a meeting it already holds ignores the descriptor's member list.
- **Consequence:** after A removes B, B still sees the meeting and A's *earlier* messages; B simply
  stops receiving new fan-out from A. This is the agreed first-cut scope, not a bug, but it is a real
  limitation and is stated in `docs/PROTOCOL.md` and in the add-member dialog copy rather than hidden.

---

## 2026-10-08 — Dev login silently failed against a stale daemon

After the Meetings frontend landed, the dev app would not log in. The cause was a version
mismatch, not a code defect, but the failure mode was silent and misleading.

### [FIXED] A stale daemon turned a 404 into a fake "vault locked" screen

- **Attempted:** log in to the dev app at `http://localhost:5173/?api=http://127.0.0.1:9876`.
- **Symptom:** submitting the correct passphrase returned the user to the unlock screen, with no
  error shown anywhere and no browser console error. The daemon was demonstrably healthy:
  `GET /api/status` returned `200 {"locked":false,...}` and `POST /api/unlock` returned `204`. The
  DOM stayed on `.unlock-screen` with no `.shell`.
- **Cause:** the new `App.refreshStatus()` calls `reloadMeetings()`, which hits
  `GET /api/meetings`. The running daemons had started at 09:12, before
  `target/debug/hush-daemon` was rebuilt at 11:44, so they did not serve that route and answered
  `404`. The pre-existing catch-all in `refreshStatus` (`apps/hush-tauri/src/app.ts`, around line
  340) treats *any* error as "the vault is locked" and sets `this.status = { locked: true, ... }`,
  so the UI snapped back to the unlock form even though the unlock had succeeded. In-page probe:
  `{"status_http":200,"locked":false,"contacts_http":200,"meetings_http":404}`.
- **Resolution:** rebuilt the daemons and restarted the stack with `scripts/run-two-ui.sh --dev`.
  No source change was needed. `GET /api/meetings` now answers `401` when locked and `200` when
  unlocked, and the unlocked shell renders.
- **Lesson:** Vite serves the frontend from source, so the frontend is always current while the
  daemon binary is only as new as its last build. Any frontend change that calls a new endpoint
  will fail against a daemon started before that endpoint existed. Restart the dev stack after
  landing daemon-side changes.

### [OPEN] `refreshStatus` treats any auxiliary-load error as "vault locked"

- **Attempted:** nothing yet; found while diagnosing the above.
- **Symptom:** a failure to load an *optional* resource (the mailbox or the meeting list) forces the
  whole app into the locked state, hides the real error, and makes a healthy, unlocked daemon look
  like a failed login. The user sees wrong-passphrase behaviour for a problem that has nothing to
  do with the passphrase.
- **Resolution:** not fixed. The fix is to scope the `try` so that mailbox/meeting load failures
  cannot flip `status.locked`, and ideally to surface the real error instead of swallowing it.
  Deferred so as not to edit `app.ts` while another agent's commit was in flight.
- **Lesson:** a catch-all that maps every error onto a user-visible state will misattribute
  unrelated failures; scope error handling to the operation that can actually fail.

---

## 2026-10-08 — Committed a half-wired Meetings change to master

Committed the whole working tree to `master` as `350d2d1` while a Meetings worker was
mid-implementation. The commit does not compile.

### [OPEN] `350d2d1` on master fails to build: `MessagePayload::Meeting` has no match arm

- **Attempted:** commit the accumulated work (multi-device removal, outsider mail, Trusts, the
  mail pane rework) to `master` as a single checkpoint, after first trying to confirm that no
  agent had edits in flight.
- **Symptom:** `cargo test --workspace` exits 101 and zero tests run:

  ```
  error[E0004]: non-exhaustive patterns: `&MessagePayload::Meeting { .. }` not covered
     --> crates/hush-core/src/account.rs:1479:62
      |
  1479 |     let (file_blob_id, mail_attachments) = match &payload {
      |                                             ^^^^^^^^ pattern `&MessagePayload::Meeting { .. }` not covered
  ```

  `cargo build -p hush-core` fails identically, and every other crate depends on it, so nothing
  downstream builds either. `npx tsc --noEmit` passes (exit 0).
- **Cause:** the concurrent Meetings worker had already written `crates/hush-core/src/payload.rs`
  (the `Meeting` variant plus a round-trip test) and `crates/hush-core/src/chat_log.rs`
  (`MeetingRecord` / `MeetingMessage`), but had not yet added the match arm in `account.rs`. The
  commit captured that half-wired state. Parent `ff2a8d5` has no `Meeting` anywhere, so the
  breakage is introduced by this commit.
- **Why the pre-commit check missed it:** it relied on file mtimes for `app.ts` / `app.css` and a
  `find . -newermt '-4 minutes'` window. Neither proves a concurrent agent is idle: the worker had
  written `payload.rs` just outside that window, and the probe never looked at `payload.rs` or
  `chat_log.rs` at all. No build was run before committing.
- **Resolution:** not yet. The worker is still editing the same files, so the working tree is left
  alone rather than raced. When it lands, the completed feature will be built and committed, which
  restores a compiling `master`. The tip of `master` is broken until then.
- **Lesson:** a build is the only evidence that a tree is committable. Run `cargo check` before
  committing, not after, and do not treat an mtime probe as proof that a concurrent writer is done.

---

## 2026-10-08 — Internal-mail keyboard contact picker (the composer To field)

Replaced the composer's native `<select>` recipient list with a typeable, keyboard-driven
combobox (`ContactPicker.ts`). Problems hit while wiring and verifying it:

### [FIXED] Escape in the picker re-rendered the whole shell and dropped focus

- **Attempted:** press Escape to close the picker's list, expecting only the list to close.
- **Symptom:** the list closed, but focus fell to `<body>` and the composer was rebuilt from
  scratch. A marker set on the input node (`input.dataset.marker`) was gone afterwards, proving
  the node had been replaced. Cause: `App` installs a document-level `keydown` handler that
  calls `this.render()` on every Escape (app.ts, near line 273). The picker called
  `e.preventDefault()` but the event still bubbled to that handler.
- **Resolution:** added `e.stopPropagation()` in the picker's Escape branch when it actually
  handles the key. Re-verified in the browser: the marker survived, `document.activeElement`
  stayed on the input, and the value was restored.
- **Lesson:** a control that handles a key the app also handles globally must stop propagation,
  not just prevent the default; otherwise a local action triggers a global re-render.

### [WORKED AROUND] `browser_type` inserts text without firing keydown, so it did not exercise the replace-on-type path

- **Attempted:** type `zz` into a picker already showing the selected label `bob`.
- **Symptom:** the value became `bobzz`, not `zz`. `browser_type` uses `Input.insertText`, which
  fires only `input`, while the picker's "the first keystroke replaces the selected label" logic
  lives in `keydown`.
- **Resolution:** verified the replace path with real key events (`browser_press_key`): pressing
  `c` on a field showing `bob` cleared it to `c`. Verified the filter path separately with an
  `input` event.
- **Lesson:** `browser_type` is not a faithful keyboard for keydown-dependent behavior; use
  `browser_press_key` when the logic branches on key events.

### [OPEN] Native click-to-open could not be verified in the automation browser (tooling, not the repo)

- **Attempted:** click the To field to open the list, the way a mouse user would.
- **Symptom:** the input received focus (`document.activeElement` was the input) but
  `aria-expanded` stayed `false` and the popup stayed hidden. `document.hasFocus()` returned
  `false` for the whole session: the Cursor browser tab was never the OS-focused window, and a
  click in that state did not dispatch a `focus` event. Dispatching `new FocusEvent("focus")`
  and pressing ArrowDown both opened the list, so the handler itself is correct.
- **Resolution:** none required in the repo. Closing this needs a click-to-open check in a
  focused window (the Tauri window, or a manually focused browser).
- **Lesson:** in this environment, a missing native focus event after a programmatic click is an
  automation artifact, not an app bug. Do not "fix" the component for it.

### [WORKED AROUND] The no-contacts state could not be reached in the dev vaults

- **Attempted:** verify the "no contacts at all" empty state against a running composer.
- **Symptom:** both dev vaults (`peer-a`, `peer-b`) each hold exactly one contact, so the
  no-contacts branch is unreachable there, and the multi-match/accent filtering could not be
  exercised with one contact either.
- **Resolution:** imported the real `ContactPicker` module into the running page and mounted it
  with synthetic items (including an accented `Álvaro` and an empty list) to exercise the same
  code in the same browser. Verified: `alv` matches `Álvaro` (accent- and substring-insensitive),
  two matches highlight and move with ArrowDown, and the empty list shows its own message.
- **Lesson:** a one-contact dev vault is not enough to verify a picker; mount the real module
  with representative data rather than claiming the edge states work.

---

## 2026-10-08 — Public-key outsider drop (second drop mode, alongside the PIN mode)

Added `hush-drop-v2`, an X25519 sealed box for outsider drops, and a `hush-cli drop-keygen` /
`drop-open` pair so an outsider can make a key and open one. The PIN path (`hush-drop-v1`) is
untouched and verified unchanged. Problems hit while wiring and verifying it:

### [WORKED AROUND] A dev relay I started for a UI check shared port 8787 with a live two-peer session

- **Attempted:** to exercise the composer end to end I started `hush-relay` on the default
  `127.0.0.1:8787` with a throwaway DB (`HUSH_RELAY_DB=.tmp-ui-check/relay.db`) and a private daemon.
- **Symptom:** the two-peer dev environment (`scripts/run-two-ui.sh`, daemons on 9876/9877, Vite on
  5173) was already up and reuses a relay that answers on 8787. It therefore adopted my relay. When
  I then `rm -rf .tmp-ui-check` to tidy up, the running relay's SQLite database file was unlinked
  out from under it; new deliveries would fail because SQLite cannot recreate its journal at a path
  whose directory is gone.
- **Resolution:** killed my relay and restarted one at 8787 with the canonical
  `instances/relay/relay.db`, which is what `run-two-ui.sh` would have used. Any blob stored in the
  deleted DB is unrecoverable, but nothing in that window was under test. Re-ran the UI check against
  a private daemon on a free port (19876) instead of 9876, leaving the two-peer session alone.
- **Lesson:** check for a running dev relay before starting another, and never point a relay at a
  throwaway DB on a port a dev script may reuse. `ss -ltnp` before starting is cheaper than repairing
  a shared session.

### [FIXED] The UI check nearly used a daemon that was not mine (the stale-process trap)

- **Attempted:** `curl http://127.0.0.1:9876/api/status` to see whether my freshly started daemon was
  up.
- **Symptom:** it returned `200`, which looked like success — but `ss -ltnp` showed 9876 was already
  held by the two-peer session's `peer-a` daemon (started earlier, different vault, different
  passphrase), and my daemon had failed to bind.
- **Resolution:** checked `ps -eo pid,lstart,cmd` and `ss -ltnp` before trusting the status code, then
  ran my daemon on 19876. The user's warning about a stale daemon is exactly right: a `200` on the
  default port says nothing about which process answered.
- **Lesson:** never treat "the port answers" as "my process is up". Compare the process start time and
  the port owner first.

### [FIXED] The key smoke script wrote an attachment into the repository root

- **Attempted:** ran `scripts/smoke-drop-key.sh`, which opens the drop a second time with
  `drop-open --delivery <id> --relay <url>`.
- **Symptom:** `git status` showed an untracked `q3.txt` in the repo root. That invocation had no
  `--save-dir`, so the package attachment was written to the process working directory (the repo root,
  since the script was run from there).
- **Resolution:** added `--save-dir "$T/open-relay"` (and an `mkdir -p`) to that call; re-ran the
  script and confirmed `git status` is clean of stray files. This is the intended default of
  `drop-open` — write attachments where the user is — the script just had to contain it.
- **Lesson:** any script that drives a tool which writes to the CWD must pin the output directory.

### [FIXED] `pkill -f` matched the shell running it and killed the cleanup mid-way

- **Attempted:** `pkill -f 'hush-daemon --dir /home/sam/ProjectHush/.tmp-ui-check'` followed by
  `rm -rf .tmp-ui-check` in the same command.
- **Symptom:** the command produced empty output and the temp vault was still present. The `pkill`
  pattern appeared in the shell's own command line, so `pkill` killed the shell before the `rm` ran.
- **Resolution:** re-checked with `ps -eo pid,cmd | grep -F hush-daemon` (which does not match itself
  the same way) and removed the directory in a separate command.
- **Lesson:** `pkill -f` can match the invoking shell; scope it to a PID or a pattern that cannot
  appear in the shell's own argv. Tooling-related, not a repo problem.

### [FIXED] `cargo test` takes only one filter, and the build lock waited 2m34s behind the other agent

- **Attempted:** `cargo test -p hush-core --lib drop:: crypto::`.
- **Symptom:** `error: unexpected argument 'crypto::' found` — `cargo test` accepts a single test-name
  filter. Separately, `cargo test -p hush-core --lib drop::` reported `Blocking waiting for file lock
  on build directory` for 2m34s, because the concurrent multi-device agent was building in the same
  target directory.
- **Resolution:** ran the two filters as separate commands. The lock cleared on its own; no failure.
- **Lesson:** budget for the shared `target/` lock when another agent is building the same tree. This
  is a collision cost, not a defect.

---

## 2026-10-08 — Multi-device removed entirely (one device per account)

The user asked for multi-device support to be removed outright, not hidden behind a disabled entry
point. Removed: the signed device list (`DeviceList`/`DeviceRecord`, `PUT`/`GET /d/{account_id}`),
the relay `device_dirs` table, per-device fan-out, cross-device `Revoked` reconciliation and
`revoke_sweep`, device linking (`POST /api/link-device/*`), `ContactDevice`/`contact.devices`, the
device-list TTL refresh, and the tests that existed only for multi-device. Kept: per-contact queues,
per-contact revocation, the `revoked` blocklist, prekeys, and the on-disk device file (which still
reads `device_id`, now a constant `1`). `DEVICE_ID = 1` everywhere a libsignal address needs one.

**Grant authentication is unchanged in strength.** The device list used to be the authority a
`hush-queue-grant-v1` signature was verified against. With it gone, `apply_queue_grant` verifies the
signature directly against the contact's `identity_key` — the key pinned in the invite that
established the relationship, and the key that hashes to the contact's account id. The signature
check itself is untouched; only the device-list lookup was dropped, and there is no path that accepts
a grant verified against a sender-supplied key. The `revoked` blocklist is kept: it is what drops a
deleted contact's already-queued blobs on drain, a job the per-contact burn cannot do for a blob that
is already in the shared inbox.

### [FIXED] A grant send advanced the session ratchet without persisting it

- **Attempted:** verified the post-removal wire end to end by exchanging invites with the CLI
  (`hush-cli add-contact`) and then sending from the daemon that opened the same vault. The first
  message the recipient drained reported `Could not decrypt a queued message — unknown sender or
  session not ready.` and the blob stayed undecryptable on every later drain; later sends worked.
- **Cause:** `Account::grant_queues_to` calls `deliver_to`, which runs `encrypt_message` and advances
  the session ratchet, but never saves. `add_contact` saves *before* the grant, so a process that
  exits right after adding a contact (the CLI) left the ratchet on disk at its pre-grant position.
  The next process then encrypted with a reused chain key and the peer could not decrypt. The daemon
  masks this because it keeps the store in memory and saves again on its next send.
- **Resolution:** `grant_queues_to` now calls `self.save().await?` after the grant attempt. Re-ran
  the exact CLI-add/daemon-send flow: first message delivered, no warning. Full workspace suite still
  green.
- **Lesson:** any path that mutates the session store must persist it before it can be the last thing
  a process does. "The caller will save" is false when the caller is a one-shot CLI command.

### [FIXED] Mutation check on the revocation burn still fails when the burn is stubbed

- **Attempted:** stubbed `burn_contact_queue` to return `Ok(())` and ran
  `cargo test -p hush-core --test revocation`.
- **Symptom:** `deleting_one_contact_burns_only_its_queue` FAILED with "the deleted contact's queue
  must be burned immediately" — the assertion is real. `a_revoked_contact_cannot_deliver_into_the_shared_inbox`
  still passed, which is expected: it exercises the `revoked` blocklist dropping decrypted blobs, a
  separate mechanism from the burn, so it is not a mutation target for this stub.
- **Resolution:** restored the burn; both tests pass. Live check against the dev relay: a throwaway
  pair delivered into the recipient's per-contact queue, the recipient deleted the contact, and the
  relay's burned-queue count went 2 -> 3 (`select count(*) from queues where burned=1`).
- **Lesson:** a mutation check only proves the test that targets the mutated function. Record which
  assertions a stub is expected to break, or a passing bystander test looks like a hole.

---

## 2026-10-08 — Per-contact queue revocation implemented (option 2)

### [FIXED] A live delete did not burn, because the running daemon predated the wiring it was testing

- **Attempted:** ran the throwaway delete flow end to end against the dev daemon on
  `127.0.0.1:9876`: add a throwaway contact, deliver once, delete it, check the relay's burned count
  and whether the throwaway could still send.
- **Symptom:** `remove:204` and the contact was gone from `/api/contacts`, but `burned queues after
  delete: 0` and `SEND AFTER DELETE: UNEXPECTEDLY SUCCEEDED`. Rebuilding and restarting the daemon
  made the identical script pass: `burned queues before delete: 1` to `after delete: 2`, and
  `SEND AFTER DELETE: failed as expected -> Error: Relay("put failed: 403 Forbidden")`.
- **Cause:** the running process was started on 2026-10-07, before this slice. `target/debug/hush-daemon`
  was rebuilt at 06:36:31, but the daemon-side wiring it needed landed later:
  `crates/hush-daemon/src/account_worker.rs` (`RemoveContact -> revoke_contact`) was last written at
  06:44:02 and `crates/hush-core/src/payload.rs` (`QueueGrant` / `Revoked`) at 06:43:06. So the process
  image was a build in which removing a contact did not revoke. A `strings` check that the *binary on
  disk* contained the burn code was true and irrelevant — the process was running an older image.
- **Resolution:** killed the stale process, restarted `target/debug/hush-daemon`, and re-ran; the burn
  and the 403 then appeared. A temporary `eprintln!` in `burn_contact_queue` / `grant_queues_to`
  confirmed the granted and burned queue ids were identical, then was removed.
- **Lesson:** a long-running daemon is a build artifact with its own start time; verifying the file on
  disk proves nothing about the process image. Compare the process start time against the source and
  binary mtimes before trusting any live result, and restart the process after every rebuild.

### [FIXED] A burned queue reported 403, so a device's own reconciliation broke its next drain

- **Attempted:** after wiring `Account::recv` to drain the inbox plus every per-contact queue, and to
  burn a queue on an own-device revocation notice, ran the new integration test.
- **Symptom:** `Relay("list failed: 403 Forbidden")` from `alice_2.recv()`. The notice is processed
  during the inbox pass and burns device 2's queue for the deleted contact; the same `recv` then lists
  that queue, and `RelayClient::list_optional` only treated 404 as empty.
- **Cause:** the relay maps a burned queue to `403` (`RelayError::Burned -> FORBIDDEN`) and a wrong
  token to `401` (`Unauthorized`). `list_optional` was written for a queue that may not exist (404) and
  did not know a burned queue is also "nothing to read".
- **Resolution:** `list_optional` now returns an empty list on 403 as well as 404. A 401 (wrong token)
  still errors, so the relaxation cannot hide a credential bug. Evidence:
  `crates/hush-core/tests/revocation.rs` passes.
- **Lesson:** "the queue is gone" has two relay encodings — never created (404) and burned (403) — and
  a drain has to treat both the same. Enumerate the status codes a check must absorb, not just the one
  the happy path produces.

### [NO LONGER APPLICABLE] A linked device cannot open a session with a single-device contact that never published prekeys

- **Attempted:** the multi-device revocation test has Alice's second device grant its per-contact queues
  to Bob and Carol, who are single-device accounts.
- **Symptom:** `hush: grant to <bob> device 1 failed: relay error: prekey get failed: 404 Not Found`.
  A linked device holds none of the original device's sessions (the device store is not transferred), so
  `ensure_session` fetches the peer's prekey record; a single-device account only ever publishes prekeys
  inside `announce_linked_device`, so there is no record to fetch.
- **Impact:** in production a linked device's grant to such a contact silently fails, so that contact
  keeps delivering to the linked device's shared inbox. Revocation on the linked device then relies on
  the `revoked` blocklist dropping the messages rather than on burning a per-contact queue. Revocation
  still hides the messages, but the queue-burn guarantee does not hold for that pair until the contact
  publishes prekeys.
- **Worked around in the test** by calling `publish_prekeys(5)` on Bob and Carol, which models a contact
  that has published. Not fixed in the daemon: `ensure_device_list_published` runs on unlock but nothing
  calls `publish_prekeys` outside linking. Publishing prekeys on unlock would close it.
- **Status:** NO LONGER APPLICABLE (2026-10-08). Multi-device and device linking were removed, so the
  configuration this describes — a *linked* second device granting to a single-device contact — cannot
  occur. The entry is kept rather than deleted, per this log's rule. The underlying observation (a
  one-time `ensure_session` fetch against a peer that never published prekeys) is not exercised by any
  surviving code path, because a grant now always rides the session the invite established.

### [FIXED] Existing queue-count assertions in `two_party_relay.rs` assumed one blob per inbox

- **Attempted:** ran the workspace suite after adding queue grants.
- **Symptom:** `relay_blob_has_no_cleartext_sender_json` and `reporting_a_queue_burns_it` failed with
  `left: 2, right: 1`: the inbox now also holds the sender's queue grant alongside the message.
- **Resolution:** the tests assert two blobs (one grant, one message) and check the opacity of both.
  This is the intended new state, not a regression.
- **Lesson:** a test that counts queue blobs encodes the set of payload kinds that can land there.
  Adding a control payload is a change to that set.

---

## 2026-10-07 — Per-contact queue revocation (multi-device)

### [FIXED] Per-contact revocation is blocked on how a contact's *other* devices learn the queue

> **Closed 2026-10-08.** The user chose **option 2** and it is implemented. Each device derives a
> per-contact queue and its read token from its own read token (`crypto::contact_queue_id` /
> `contact_queue_read_token`), so the read token stays per device and out of the vault. The device
> list's queue is now a **grant channel**: each device sends a signed `MessagePayload::QueueGrant`
> (`queue_id`, `delivery_token`, `signature`) into the contact's inbox over the libsignal session, and
> the recipient verifies the signature against the identity key the signed device list vouches for that
> device — the same authority `DeviceList::verify` uses for the account id, so an unauthenticated write
> to the public grant channel cannot name a queue the writer owns. The blocklist (`AccountState.revoked`)
> remains only as defence in depth. Cross-device reconciliation is the `MessagePayload::Revoked` notice
> a device sends to its own siblings' inboxes (authenticated by the own-device session), which makes each
> sibling burn its own queue and drop the contact; `revoke_sweep` retries any burn that failed. Evidence:
> `crates/hush-core/tests/revocation.rs` (`deleting_one_contact_burns_only_its_queues_on_every_device`,
> `a_single_device_delete_burns_its_queue`), which fail when `burn_contact_queue` is disabled (verified
> again 2026-10-08 by stubbing `burn_contact_queue` to `Ok(())`: both tests fail, at the "device 1 must
> burn its queue" and the "must not be able to deliver" assertions).
>
> **Not fully closed.** One multi-device path is still broken and tracked separately as `OPEN`:
> a *linked* device's grant to a *single-device* contact that never published prekeys fails
> (`prekey get failed: 404`), so for that pair revocation falls back to dropping on drain rather than
> burning the queue. See "[OPEN] A linked device cannot open a session with a single-device contact
> that never published prekeys". The design is implemented; that pair's burn guarantee is not.


- **Attempted:** implement the fix the user chose — mint a per-contact queue at add time so deleting
  a contact burns only that contact's queue — by first designing it against the real send/recv path.
- **What is settled by the code and docs:** the relay needs no new endpoint. `Store::authorize`
  (`crates/hush-relay/src/store.rs`) mints a queue on first `PUT`, `Store::report_queue`
  (`store.rs:294`) burns it, and `POST /q/{id}/report` exists (`crates/hush-relay/src/app.rs:244`).
  `docs/DESIGN.md` already chose "invite-minted rotating receive queues"; `docs/PROTOCOL.md` says
  "recipient lists/drains their queues" (plural) and "rotate queue IDs inside the encrypted session".
  So a per-contact queue on the recipient, handed out in the invite, is the intended shape.
- **The blocking finding — multi-device.** A peer fans out one ciphertext per recipient device
  (`Account::send_payload_inner`, `account.rs:1084`), and the recipient learns those devices from the
  signed device list, which carries each device's `queue_id`/`queue_token`
  (`Account::publish_device_list`, `account.rs:1239`; `refresh_contact_devices`, `account.rs:1339`).
  For deleting a contact to cut off **every** device of the account, the contact must be unable to
  deliver to *any* of them, so the queues are necessarily **per (contact, device)**. But a queue's
  delivery token can only reach the contact privately — the device list is public and unauthenticated
  (`GET /d/{account_id}`, `app.rs:159`), which is the exact leak that makes the current rotation
  impossible. So the per-device queues must be granted in-session (a new payload), not published.
- **Why the invite alone cannot carry them:** the invite is generated by one device (whichever calls
  `invite()`), so it can only name that device's queue. The account's other devices' queues have no
  channel to the contact unless the *other* device grants them — and that device may have no session
  with the contact yet, and the contact may not even be a contact of that device (contacts are
  one-sided; a peer can add you without you adding them). A grant channel therefore has to be
  introduced (a per-device "control"/bootstrap queue in the device list that only accepts grants and
  own-copies), plus a blocklist so a revoked account's grants are ignored, plus cross-device
  reconciliation so each of the deleter's devices burns its own per-contact queues.
- **Second blocking finding — where the read token lives.** If per-contact queues are read by all of
  the account's devices (one queue per contact, simplest fan-out), the read token must be shared
  across devices, which today it explicitly is not: `DeviceKeys.read_token` is per device and kept out
  of the vault and out of backups (`device.rs:55-71`), and `docs/THREAT_MODEL.md` lists queue read
  tokens as "per device, never published". Sharing them is a threat-model change, and it weakens
  *device* revocation (a revoked device's vault copy could still read pre-existing per-contact
  queues unless every per-contact queue is also rotated). Keeping them per device avoids that but
  forces per-(contact, device) queues plus the cross-device burn described above.
- **Symptom:** neither branch is a patch. Both change the invite model (per-handout queues, so
  `AccountState.invite_cache` stops being a single blob and becomes a set of issued handout queues),
  the device-list semantics (its queue becomes a grant channel, not a delivery target), and the
  send/recv paths (poll N queues, route grants vs messages, drop a blocklisted sender). Each is
  security-critical, and the wrong pick produces a change that *looks* like revocation while leaving a
  device reachable — which the task says is worse than the current honest state.
- **Resolution needed (a user decision, then a slice):** pick one and I will implement it end to end:
  1. **Per contact, shared read token in the vault.** One queue per contact, all the account's
     devices read it, the sender fans out N ciphertexts into it. Smallest change; requires accepting
     that read tokens live in the vault (and rotating every per-contact queue when a *device* is
     revoked).
  2. **Per (contact, device), read token stays per device.** Sounder for device revocation; needs a
     per-device grant/control queue, a blocklist, and cross-device burn reconciliation. Largest
     change.
  Either way the invite becomes per-handout and the device list's queue becomes a grant channel.
- **Lesson:** "mint a queue per contact" is one sentence, but the recipient having more than one
  device turns it into a credential-distribution problem, and the only private channel is the session.
  Before committing to a revocation design, count the credential holders (every device of both
  parties) and ask how each one learns and loses the credential — not just the inviting device.

---

## 2026-10-07 — Trusts: typed-DELETE confirmation, copy sweep, card ordering

### [FIXED] The delete dialog cannot say "they lose the ability to message you", because they do not

> **Closed 2026-10-08.** The protocol change landed (see the newest section): deleting a contact now
> burns the per-contact queues its devices deliver into. The Trusts delete modal in
> `apps/hush-tauri/src/app.ts` now says the person is removed, history stays in the vault, and they can
> no longer message you. It states the one remaining limit accurately: a contact added before
> per-contact queues existed is still on the shared inbox (not burned, or every old contact would be cut
> off at once) and becomes revocable once it receives its own queue. The typed-`DELETE` gate is
> unchanged. `npx tsc --noEmit` passes.

- **Attempted:** built the requested typed-`DELETE` confirmation and wrote its copy to the spec:
  removed from Trusts, history stays in the vault, they lose the ability to message you, cannot be
  undone, plus a sentence about re-issuing the invite to the remaining contacts.
- **Symptom:** the revocation those two sentences describe does not exist. The entry below
  ("Rotating the invite cannot revoke a contact") is still `OPEN`, and no rotate-and-re-issue flow
  was built, so a deleted contact can still deliver to the relay inbox. The typed gate itself is
  real and was verified in the running app (disabled until the field matches, input focused, Enter
  guarded, Escape and backdrop dismiss, state reset on reopen); only the claim was not shippable.
- **Resolution:** shipped honest copy. The dialog says the person is removed from Trusts, that
  history stays, that they disappear from Chats/Mail/Trusts and the session is dropped, that this
  cannot be undone, and it states the limit plainly: they can still write to the relay inbox,
  because the invite cannot be rotated without cutting off the remaining contacts too. The
  "re-issued invite" sentence is omitted because no invite is re-issued.
- **What is still needed:** the protocol change named in the entry below. When it lands, the copy
  can be upgraded to the wording the requirement asked for. Until then, saying it would be a lie
  in the product.
- **Lesson:** a UI requirement can be downstream of a protocol decision. Write the copy against
  what the system actually does and record the gap, rather than letting a modal assert a security
  property that nothing enforces.

### [OPEN] WebKitGTK still has not run this pane, so the dialog is verified only in Chromium

- **Attempted:** verify the Trusts pane, the typed confirmation, the ordering and the filter in a
  running app rather than by reading code.
- **Symptom:** no failure. Everything above was exercised against the Vite dev server at
  `localhost:5173` in the Cursor IDE browser (Chromium) and against the live daemon on
  `127.0.0.1:9876`: the card order `ámbar, bob, Zed` from an API order of `bob, Zed, ámbar`, the
  filter narrowing with real `input` events, and both delete paths (Enter and the button) end to
  end. But the shipped desktop shell is WebKitGTK, and no Tauri CLI is installed here, so that
  engine was never run.
- **Resolution:** not resolved. Same constraint as the composer's `execCommand` entry: browser
  evidence, not shell evidence.
- **Lesson:** say which engine the evidence came from. "Verified in the app" is ambiguous when the
  dev server and the desktop shell are different engines.

---

## 2026-10-07 — Mail reading pane (header, rules, absolute time)

### [FIXED] A concurrent edit to `app.ts` turned `tsc` red in code this slice does not own

- **Attempted:** re-run `npx tsc --noEmit` as the final check after finishing the mail-pane edits.
- **Symptom:** `src/app.ts(1231,7): error TS2304: Cannot find name 'setKnownContacts'` (exit 2).
  The name is imported at `app.ts:38` from `./lib/mailStore` and called at lines 258, 317, 1234 and
  3225, but at that moment no module exported or defined it — a repo-wide grep found it only at
  those call sites. The failing code is the Trusts/contact-revocation slice, which belongs to the
  other agent and is off-limits here. The first `tsc` run in this session, taken before that edit
  landed, passed with exit 0.
- **Resolution:** not touched in this slice, deliberately — repairing it would have meant editing
  another agent's in-flight code and colliding with it. The other agent then landed the export
  (`setKnownContacts` now defined at `mailStore.ts:86`), and a fresh `npx tsc --noEmit` in this
  session returned exit 0 with no output. That run is the evidence that closed this entry.
- **Lesson:** on a shared working tree a clean check can go red between two commands. Record the
  exact error and which slice owns it; do not repair another agent's half-finished edit. Re-running
  the check later is enough when the owner is still working.

### [OPEN] A concurrent agent edits the same two source files, so the verified UI state is not final

- **Attempted:** implement the mail header/rules/absolute-timestamp slice in
  `apps/hush-tauri/src/app.ts` and `apps/hush-tauri/src/styles/app.css` while another agent was
  building the Trusts rail in the same two files.
- **Symptom:** `git diff` for `app.ts` is 637 lines and mixes several pre-existing uncommitted
  slices (rich-text compose, outsider mail) with the Trusts work, so this slice's diff cannot be
  read in isolation. The other agent also appended its own section to this log
  (`## 2026-10-07 — Trusts rail and contact revocation`) between two of my reads of this file,
  so the race is live, not hypothetical.
- **Resolution:** re-read each region immediately before editing and used small anchored
  replacements; every edit applied cleanly and `npx tsc --noEmit` stayed clean, so no collision
  occurred in this slice. The header block, the rules and the absolute timestamp were then
  verified in the running dev server.
- **What is still needed:** the UI was verified against a tree another agent is still writing, so
  the reading pane must be re-checked once the Trusts work lands. This is a workflow risk, not a
  code defect in the mail pane.
- **Lesson:** on a shared working tree, "it type-checks" certifies the tree only at the instant it
  was read. Anchor edits to exact text and re-read before every write; do not treat a whole-file
  diff as proof that a change is yours.

### [WORKED AROUND] `browser_snapshot` collapses the mail header, so the DOM had to be read over CDP

- **Attempted:** verify the new From/To/Date/Subject block using the browser tool's accessibility
  snapshot of the open thread.
- **Symptom:** the snapshot listed the heading and the action buttons but not the `<dl>` fields,
  so it could not show whether the header rendered at all.
- **Resolution:** read the rendered text and computed styles with `browser_cdp`
  (`Runtime.evaluate`), which confirmed the four fields, the 1px `--border` rules and the aligned
  label column. The accessibility snapshot is not a substitute for reading the DOM for
  layout-only changes.
- **Lesson:** assert on computed style and geometry, not just the accessibility tree.

---

## 2026-10-07 — Trusts rail and contact revocation

### [FIXED] Rotating the invite cannot revoke a contact: the public device list republishes the delivery token

> **Closed 2026-10-08.** Revocation no longer depends on rotating the invite. Contacts deliver into a
> per-contact queue that each device mints and reads with a per-device credential; the device list's
> queue is a grant channel and no longer a delivery target once a grant has landed. Deleting a contact
> burns those queues on every device of the deleter, so the published device list does not resurrect a
> delivery path. The delivery token is still public by design, but it now authorizes only putting a blob
> in a queue that the deleter can burn, and the grant that names a queue is signed by the granting
> device's identity key and verified against the signed device list. Migration limit (old contacts on
> the shared inbox) is documented in `docs/PROTOCOL.md` and in the modal copy. Evidence:
> `crates/hush-core/tests/revocation.rs`.

- **Attempted:** built the Trusts rail and, before wiring Delete to a
  rotate-and-re-issue flow, traced what actually authorizes a delivery into an
  account's inbox.
- **Evidence from the code:**
  - `Account::invite()` (`crates/hush-core/src/account.rs`) mints and caches one
    invite carrying the account's single inbox `queue_id` and its delivery token.
    Every contact of an account receives that same invite, so they all hold the
    same delivery token. `DeviceKeys::queue_token()` (`crates/hush-core/src/device.rs`)
    is `queue_delivery_token(read_token)`, one value for the whole account.
  - `Store::authorize` (`crates/hush-relay/src/store.rs`) binds a queue to
    `sha256(delivery_token)` on first write and rejects a non-matching token.
    There is no path to rebind an existing queue, so a rotation means a new queue
    id and a new token.
  - The decisive leak: `Account::publish_device_list()` writes
    `queue_token: self.device.queue_token()` for device 1 into the signed list,
    and `GET /d/{account_id}` is deliberately unauthenticated
    (`crates/hush-relay/src/app.rs`). `Account::refresh_contact_devices()` reads
    that list and overwrites the contact's `queue_id`/`queue_token`
    (`crates/hush-core/src/account.rs`). A contact learns the owner's
    `account_id` from the invite it was given (`InvitePayload.protocol_name`).
- **Symptom:** any rotate-and-republish revocation is undone by the revoked
  contact's next send. `refresh_contact_devices_if_stale` (5-minute TTL, and it
  also runs from the daemon's `/api/contacts`) fetches the public device list and
  picks up the new delivery token. Burning the old queue does not help, because
  the new queue is the one the list advertises. This contradicts
  `docs/THREAT_MODEL.md`, which names "a contact who was blocked (they should not
  keep delivering to a live slot)" as an attacker, and `docs/PROTOCOL.md`, which
  describes a queue "the other party can send into" per contact
  ("Block = drop local session + tell relay to burn that queue"). The
  implementation collapsed that per-contact slot into one shared account inbox.
- **Resolution needed:** a protocol change, not a patch. Options, none
  slice-sized: (a) per-contact inboxes minted when a contact is added, with
  revoke = burn that contact's inbox (matches PROTOCOL.md; needs a way to hand the
  dedicated inbox to the contact and keep it out of the public device list);
  (b) authenticate `GET /d/{account_id}` behind a directory capability rotated on
  revocation and re-issued to remaining contacts; or (c) rotate the account
  identity, which breaks every session. Until one is chosen, Delete removes the
  contact, keeps history, and drops their sessions, but does **not** stop them
  writing to the inbox.
- **Lesson:** a credential published at a stable, unauthenticated rendezvous
  cannot be rotated away from someone who already knows the rendezvous key. The
  question is not "can I mint a new token" but "who can discover it".

---

## 2026-10-07 — Outsider mail (delivery + drop)

### [FIXED] The drop container's production KDF cost was paid in every test, taking the suite from seconds to 85

- **Attempted:** wrote the `hush-drop-v1` container tests against the public `seal`/`open`, which
  stretch the PIN with PBKDF2 at 600,000 iterations.
- **Symptom:** `cargo test -p hush-core --lib` went from ~13s to **84.9s**, with four tests each
  reporting `has been running for over 60 seconds`. The tests were correct and passed; the cost was
  the production iteration count multiplied by every seal and open in the tests, on an unoptimized
  build.
- **Resolution:** split `seal` into a public entry point that passes `DROP_ITERATIONS` and a private
  `seal_with_iterations`. The count is recorded in the container header either way, so `open` needs
  no hint and the tests exercise the same code path with 1,000 iterations. The drop tests now run in
  **0.07s** and the production count is unchanged and pinned by a compile-time assertion
  (`const _: () = assert!(DROP_ITERATIONS >= 600_000)`).
- **Lesson:** a deliberately expensive primitive has to be injectable, or the cost lands on the test
  suite and people quietly lower it. Pin the production value at compile time so parameterizing it
  for tests cannot become a way to weaken it.
- **Still true after the fix:** the *integration* test in `crates/hush-core/tests/outsider_mail.rs`
  deliberately pays the real 600k cost, because it is the only place the shipped path is exercised
  end to end. It is the slowest test in the suite (~40s in a debug build). That is a cost, not a
  defect, but it should not grow: keep the number of real-iteration seals and opens in integration
  tests to the minimum that still proves the shipped path.

### [FIXED] A wrong KDF test-vector reference was written into the plan before the code existed

- **Attempted:** specified the PBKDF2 test as "RFC 6070, case 2" with a 20-byte output.
- **Symptom:** RFC 6070 is the **PBKDF2-HMAC-SHA1** vector set. Using it to check a
  PBKDF2-HMAC-SHA256 implementation would have asserted the wrong digest, or the test would have
  been "fixed" by weakening the implementation to match a SHA-1 result.
- **Resolution:** the implementation and its test use the standard PBKDF2-HMAC-SHA256 vectors
  (including a 40-byte case that spans two output blocks), and the plan text was corrected to say
  why RFC 6070 does not apply. Verified: `pbkdf2_matches_the_published_sha256_vectors` passes.
- **Lesson:** a test vector quoted from memory is a claim. Name the exact document and hash, and
  prefer a vector whose digest length forces the multi-block path.

---

## 2026-10-07 — Rich-text mail compose

### [WORKED AROUND] The browser-automation text tool does not carry the editor's typing style, so formatted typing looked broken

- **Attempted:** verified the new compose editor by clicking Bold, then using the browser tool's
  "type" action to enter text, then reading the editor DOM.
- **Symptom:** the toolbar reported `Bold: pressed` but the surface's `innerHTML` was the bare string
  `bold text and a bullet` — no `<b>`, no `<ul>`. It looked like `execCommand` was a no-op in the
  WebView, which would have meant the feature did not work.
- **Resolution:** the same `document.execCommand("bold")` run directly through `Runtime.evaluate`
  against selected text produced `<b>hello world</b>`, and `insertUnorderedList` produced
  `<ul><li><b>hello world</b></li></ul>`. The automation's type action inserts text by a path that
  does not apply the browser's pending typing style, so the formatting was never going to appear.
  This is a tooling artifact, not a repo problem. The editor was then verified end to end (send,
  recipient, draft round-trip) by driving the formatting commands through CDP and reading the DOM.
- **Lesson:** `queryCommandState` reporting a toolbar button as active says nothing about the DOM.
  Confirm formatting by reading `innerHTML`, not the pressed state — the same trap the Phase 5 note
  records for toolbar clicks.

### [FIXED] Saving a draft opened from a thread left the reading pane on the old thread

- **Attempted:** tested the rich composer from the Compose button while a conversation was open, then
  saved a draft.
- **Symptom:** the folder switched to Drafts (badge updated, list correct) but the main pane still
  rendered the conversation that had been open, because `openCompose` never cleared `mailThreadId`
  and `renderMailMain` prefers the thread over the list. Pre-existing, but only reachable by opening
  compose while a thread was displayed, which the new test flow did.
- **Resolution:** `openCompose` now clears `mailThreadId`/`mailOpenId` for a brand-new message (no
  draft and no reply thread), so leaving compose returns to the folder list. Replies and draft edits
  keep their thread. Verified: Save draft from an open thread now lands on the Drafts list.
- **Lesson:** "the folder changed" and "the pane changed" are separate assertions. Checking only the
  badge would have missed this.

### [OPEN] The composer formats with `document.execCommand`, which is deprecated and untested on WebKitGTK

- **Attempted:** chose a formatting API for the contenteditable surface without adding an editor
  dependency (ProseMirror/Tiptap/Quill would be a large new dependency for bold/italic/lists).
- **Symptom:** no failure yet. `execCommand` is deprecated and its behaviour is not guaranteed
  identical across engines. It was exercised only in the Cursor IDE browser (Chromium) during this
  slice; the real desktop shell on Linux runs WebKitGTK, which was not run here (no Tauri CLI — see
  the entry above about the unreferenced dev config).
- **Resolution needed:** confirm bold/lists/headings in a packaged `hush-tauri` window on WebKitGTK
  before relying on it. If it misbehaves, the fallback is a small Range/Selection formatter over the
  same allowlisted subset; the sanitizer and renderer would not change.
- **Lesson:** an untested-by-construction choice should be written down as such, not presented as
  done. The rendering and sanitization paths are engine-independent and were verified; the editing
  commands are not.

---

## 2026-10-07 — Live UI development without rebuilding the desktop binary

### [FIXED] `run-two-ui.sh --dev` left the Vite dev server running after Ctrl+C

- **Attempted:** added a `--dev` mode that starts the relay, both daemons with a TCP listener, and
  `npm run dev`, then checked that stopping the script releases everything.
- **Symptom:** the relay and both daemons died, and `node .../node_modules/.bin/vite` stayed alive with
  port 5173 still bound, so the next `--dev` run failed its port check. The dev server is started as
  `npm run dev`, which runs the script through `sh -c`; `npm` did not forward the `SIGTERM` that
  `cleanup()` sent to its pid, and the `sh` wrapper died while its child did not.
- **Resolution:** the dev server is now started with `setsid` and stopped as a process group
  (`kill -TERM -- "-$VITE_PGID"`), which takes `npm`, `sh`, and `vite` together. Verified by starting a
  run, sending `SIGINT` to the script only, and confirming no leftover process and no listener on 5173,
  8787, 9876, or 9877.
- **Lesson:** `npm run <script>` is two processes, not one. Anything that has to be killed later must
  either be exec'd directly or started in its own process group.

### [OPEN] The repo carries a Tauri dev config that nothing references, and no Tauri CLI is installed

- **Attempted:** looked for a way to run the desktop window against the Vite dev server (hot reload
  inside the real shell instead of a browser tab).
- **Symptom:** `apps/hush-tauri/src-tauri/tauri.dev.conf.json` sets `devUrl` and `beforeDevCommand` as
  Tauri's dev mode requires, but nothing in the repo passes `--config` to it: no script, no doc, no
  mention outside the file itself. `cargo-tauri` is not installed and `@tauri-apps/cli` is not in
  `node_modules`, so `tauri dev` cannot run here at all.
- **Resolution needed:** either wire the config up (add `@tauri-apps/cli` and a `dev:app` script that
  runs `tauri dev --config src-tauri/tauri.dev.conf.json`) or delete the file. Until then, live
  development is browser-only, which is what `--dev` provides. Not verified either way in this
  environment: no Tauri CLI to try it with.

---

## 2026-10-07 — Mail client UI: folders, drafts, Outbox

### [FIXED] "Drafts are deletable" was implemented in the store and the daemon, and reachable from nowhere

- **Attempted:** verified the new Drafts folder in the browser by saving a draft, reopening it, and
  editing it. Then went looking for the delete control, because the requirement said drafts are
  deletable.
- **Symptom:** there was none. `deleteDraft` existed in `mailStore`/`api`, `DELETE /api/mail/draft/{id}`
  existed on the daemon, and a core test (`a_draft_is_saved_edited_and_deleted`) passed — but the
  rendered compose pane held only `Attach files`, `Save draft`, `Send`, and the draft row in the list
  offered nothing. The capability was verified at every layer except the one the user touches.
- **Resolution:** added a `Discard draft` button to the composer, shown only while editing a saved
  draft (a brand-new message is abandoned by closing compose, and nothing was written for it). Verified
  in the browser: the record is removed, `chat.enc` returns to its pre-draft size, and the Drafts badge
  clears.
- **Lesson:** a green core test proves the function, not the feature. For anything with a UI, the
  acceptance check has to name the control that triggers it, and "deletable" is not satisfied by a
  function nobody can call.

### [FIXED] Dev-mode CORS rejected `PUT` and `DELETE`, so draft save and delete worked from curl and failed in the browser

- **Attempted:** exercised the new draft endpoints from the running dev UI.
- **Symptom:** the preflight was refused. `local_ui_cors` allowed `GET`, `POST`, `OPTIONS` only, so
  `POST /api/mail/draft` (fine) worked while `DELETE /api/mail/draft/{id}` and any `PUT` were blocked by
  the browser before the request left it. The same calls returned 204 from `curl`, which is exactly why
  this was not caught earlier: the smoke script and the tests do not run through a browser.
- **Resolution:** added `Method::PUT` and `Method::DELETE` to the CORS layer.
- **Lesson:** a curl-only smoke suite cannot see CORS. When a UI slice adds a new HTTP method, the
  allowed-methods list is part of the change.

### [FIXED] Changing `send_mail`'s return type stopped `internal_mail.rs` compiling

- **Attempted:** made `send_mail` return `Result<MailOutcome, Error>` so a queued send could be reported
  to the caller instead of surfacing as an error.
- **Symptom:** the crate no longer compiled — the existing tests in `crates/hush-core/tests/internal_mail.rs`
  assert on the old `Result<(), Error>` shape, so every call site in that file was a type mismatch. The
  change was correct and the tests were simply written against the previous signature.
- **Resolution:** updated the existing assertions and added the new queue/retry, draft round-trip, and
  draft-not-plaintext tests in the same pass.
- **Lesson:** a return-type change in `hush-core` is a workspace-wide edit. Grep the tests in other
  crates before running them, or the first signal is a compile wall.

### [FIXED] New `decode_attachments` tripped `clippy::type_complexity`

- **Attempted:** ran `cargo clippy --workspace --all-targets` after the slice, per the completion
  standard, to check nothing new had been added to the pre-existing warning baseline.
- **Symptom:** `warning: very complex type used` at `crates/hush-daemon/src/main.rs:779` — the
  `Result<Vec<(String, Vec<u8>)>, (StatusCode, Json<ApiError>)>` return of the new helper. The rest of
  the warnings (7 `redundant_closure` in `backup.rs`, 4 `ptr_arg`, 1 `items_after_test_module`) are
  pre-existing and were confirmed against `git show HEAD`.
- **Resolution:** named the error half `type ApiErrorResponse = (StatusCode, Json<ApiError>)` and used it
  in the signature.
- **Lesson:** a warning count is only meaningful against a baseline. Check the offending file against
  `HEAD` before deciding whether a warning is yours.

### [FIXED] `run-two-ui.sh` could not be re-run while a harness was live, and tore itself down instead of saying so

- **Attempted:** re-ran `scripts/run-two-ui.sh` to pick up a rebuilt UI while the previous run's relay and daemons were still up.
- **Symptom:** the script printed its usual "Opening two UI windows" block and then immediately exited with "A process exited; shutting down…", taking the windows with it. Two daemons had been started on sockets that were already live; `bind_socket` refuses to unlink a live socket (correctly), so both new daemons exited at once, and `wait -n` returned on the first of them. The readiness loop had not noticed because the *old* daemon answered `/api/status` on that path, so every check looked healthy.
- **Resolution:** the script now probes each socket first and reuses a daemon that answers, the same way it already reused a running relay. Starting a daemon is followed by a readiness check that also watches for the process dying, and a failure prints the daemon's log and exits non-zero instead of continuing. Per-process logs moved into `$RUN_DIR/logs/`. Reuse is refused when a TCP listener was requested and is not open, since a running daemon cannot grow one.
- **Lesson:** "already running" and "just started" have to be told apart before starting anything, or the second run silently fights the first. A readiness check that only asks "does something answer here?" cannot see its own process die.

### [FIXED] A bad `HUSH_RUNTIME_DIR` surfaced as a raw `mkdir` error and skipped the script's own diagnostics

- **Attempted:** tested the new daemon-startup failure path by pointing `HUSH_RUNTIME_DIR` at an unwritable path.
- **Symptom:** the script exited 1 before reaching any of it, with `mkdir: cannot create directory '/proc/nope': No such file or directory` and no mention of which variable to fix.
- **Resolution:** the `mkdir` is now guarded and names `HUSH_RUNTIME_DIR` in its message. The daemon-death path was then verified separately by pointing `HUSH_DAEMON_A_SOCKET` at an unwritable path, which produced `alice daemon did not come up on /usr/peer-a.sock:` followed by the daemon's `PermissionDenied`.
- **Lesson:** `set -e` plus an unguarded setup command means the first failure the user sees is the least informative one.

### [WORKED AROUND] A toolbar click that reports `[active, focused]` has not necessarily done anything

- **Attempted:** clicked `Move to Trash`, `Restore to Inbox`, and `Delete forever` and read the DOM
  immediately after each.
- **Symptom:** the snapshot came back with the button `active, focused` and the pane completely
  unchanged, which reads as a dead handler. The actions are async — each one writes `ui-prefs.json` over
  HTTP and re-renders after the response — so the read raced the write. Re-checking the DOM a second
  later showed the correct result every time; no code was at fault.
- **Resolution:** after each action, wait for the settle (a short sleep, or poll the folder badge) before
  reading the DOM. Do not click a second time: the duplicate click is what turns a slow write into an
  apparent bug.
- **Lesson:** an `active`/`focused` button is evidence of a click, not of an effect. Tooling-related,
  not a repo problem.

---

## 2026-10-07 — Two-peer presence: Alice and Bob could not see each other

Symptom: in the `scripts/run-two-ui.sh` harness both peers listed each other with
`inbound_ready: true` but `online: false`, so the composer stayed disabled and a message sat
undelivered in the relay.

### [FIXED] A rotated relay inbox was republished only once, so a lost publish left the peer unreachable

- **Attempted:** traced presence end to end. `POST /api/presence/heartbeat` on both daemons returned
  204 and advanced `presence_at` for both live queues (`61c4082e` = Alice, `ec716fd5` = Bob), yet
  `GET /api/contacts` still reported `online: false`.
- **Symptom:** both contact records named inboxes no daemon reads. Alice's contact `bob` held
  `917eae3e` (a blob was sitting in it, delivered minutes earlier and never received); Bob's contact
  `alice` held `53050bc1`, last heartbeated the previous day. Both peers' real inboxes had changed —
  the read/delivery credential split rotates every pre-split vault's queue once — but the contacts
  still pointed at the abandoned ones.
- **Cause:** the republish that tells peers about the new inbox ran only on the unlock that rotated
  (`if a.queue_rotated()`), and it was best effort. `Account::open` rewrites `device.enc` as v2 on
  that same call, so `queue_rotated()` is false on every later open — the comment claiming "the next
  unlock retries" was wrong, and a publish that failed (relay not up yet) was lost for good.
  `device_dirs` on the relay held Alice's list and no row at all for Bob, so nothing could ever point
  Alice at Bob's real inbox.
- **Resolution:** `Account::ensure_device_list_published()` compares the *published* list against this
  device's current inbox and republishes when they disagree (including "no list at all"), so the
  repair is durable and also heals a vault whose publish was lost before the check existed. The daemon
  unlock path calls it unconditionally instead of gating on `queue_rotated()`.
- **Lesson:** a one-shot flag is not a retry policy. When the condition is cleared by the same
  operation that consumes it, "best effort" silently becomes "never". Compare against durable state
  (what is actually published) instead of trusting a one-shot marker.

### [FIXED] Presence read a cached inbox, and the UI refuses to send to an offline contact, so a moved inbox stayed offline forever

- **Attempted:** looked for anything that would refresh a contact's cached queue. Only
  `send_payload_inner` called `refresh_contact_devices_if_stale`.
- **Symptom:** `contact_is_online` checks the cached `queue_id`, so a contact whose inbox moved read as
  offline; and the UI disables the composer and send button for an offline contact (`renderComposer`,
  `ta.disabled = !contactOnline`), so the one action that would refresh the cache could never run.
  Alice could not learn Bob's new inbox even after he republished, because she had no way to trigger
  the refresh.
- **Resolution:** the daemon's `/api/contacts` handler now refreshes any stale contact device list
  before reporting presence (`refresh_contact_devices_if_stale`, made `pub`), bounded by the same
  5-minute TTL the send path already used. Presence polling is the natural healing point.
- **Lesson:** when a UI gate depends on a cached value, something other than the gated action has to
  be able to refresh it. "We only refresh on send" plus "we never send while stale" is a closed loop.

### [OPEN] Desktop screenshot under Wayland is a black root, so the UI windows could not be verified visually

- **Attempted:** captured the screen with `ffmpeg -f x11grab -i :0` to confirm the two Tauri windows
  show "Online".
- **Symptom:** the PNG is 1920x1080 and 100% black (68 distinct colours, all antialiasing noise). The
  session runs Wayland (`WAYLAND_DISPLAY=wayland-0`); x11grab only sees the empty XWayland root, and no
  Wayland-native tool (`grim`, `spectacle`) is installed.
- **Resolution:** not resolved. Verified through the daemon instead: both `/api/contacts` responses
  report `online: true`, and both queues' `presence_at` advance on their own every few seconds, which
  only happens if the two UI windows are up, unlocked and polling.
- **Lesson:** tooling-related, not a repo problem. On this host the UI has to be verified through the
  daemon API; a desktop screenshot is not evidence here.

---

## 2026-10-07 — Repo consolidation (two worktrees merged into master)

### [FIXED] `pkill -f '<pattern>'` killed its own shell before the build could start

- **Attempted:** stop a stale `cargo check` by pattern and launch a fresh one in the same command:
  `pkill -f 'cargo check -p hush-core'; … ; nohup cargo check … > /tmp/check3.log &`.
- **Symptom:** `/tmp/check3.log` was never created and none of the follow-up commands ran. `pkill -f`
  matches the full command line, and the invoking shell's own command line contained the literal
  pattern, so it signalled itself before the redirect was set up.
- **Resolution:** killed by pid (`pgrep -x cargo` / `pgrep -x rustc`) instead of by a pattern that
  appears in the caller's own command line.
- **Lesson:** `pkill -f` / `pgrep -f` match the caller's own command line. Never pattern-kill on a
  string the command itself contains; use exact process names or the `[c]argo` bracket trick.

### [FIXED] `AwaitShell` returned instantly because the launcher shell had already exited

- **Attempted:** polled a backgrounded build with `AwaitShell` using the shell id of the command that
  started it (`nohup cargo check … & echo started`).
- **Symptom:** the poll reported the job "completed" in ~5s with exit 0 and "pattern did not match"
  while cargo was still compiling. The launcher shell only ran `echo` and exited; `nohup` had
  detached cargo, so the shell job's completion said nothing about the build.
- **Resolution:** polled the process (`pgrep -x cargo`) and the log file instead of the launcher's
  shell id.
- **Lesson:** a shell id tracks the shell, not the process it backgrounded. Detached jobs need
  process or log polling.

---

## 2026-10-07 — Tooling: backgrounded build, and a wrong "it died" call

### [FIXED] A running background build was reported dead, then started a second time

- **Attempted:** warmed the new master worktree's cold Rust build in the background so the two-UI
  script would start fast, then checked on it.
- **Symptom:** a `task finished: success` notification arrived for the launcher command (which ends
  in `&` and returns immediately). I then looked only at the log tail (stalled at
  `Compiling cairo-sys-rs`) and at the absence of binaries in `target/debug`, and reported that the
  build had been killed. It had not been: `pgrep -af bin/cargo` showed the original process still
  running with eight `rustc` children, and the log had grown to 209 lines. My "restart" added a
  second cargo that blocked on the target-dir lock, which I then had to kill. The build finished on
  its own in 5m 02s and produced all three binaries.
- **Resolution:** no repo change needed; the build was healthy throughout. This is a tooling and
  inference problem, not a code problem.
- **Lesson:** before calling a background job dead, check the process (`pgrep`), not its side
  effects. "No output yet" plus "no artifacts yet" is not evidence of death. Also, a `task finished`
  notification for a command ending in `&` refers to the launcher, not the job it spawned.

---

## 2026-10-07 — Internal member mail (Task 5d)

### [FIXED] A stale second browser tab drained the relay queue and looked like UI staleness

- **Attempted:** verifying the new unread badge by reading the rail badge and thread list from the
  live page while a hand-rolled two-peer harness delivered mail.
- **Symptom:** intermittently the daemon's `/api/mail` held records while the UI store did not, and
  the badge stayed off. Instrumenting `window.fetch` in the page showed every `/api/recv` returning
  `{"messages":[],"warnings":[]}` even though new mail had just been sent — so the queue was being
  drained by someone else before the observed tab's poll.
- **Root cause:** `browser_tabs` listed two Hush tabs: the current one on `:4174` and a leftover tab
  on `:4173` from an earlier session. The `:4173` preview server was long dead, but the already
  loaded page kept running and polling the same daemon on `:9876`. It won the drain race and kept
  the mail in its own JS context; the tab under test then legitimately saw an empty queue. This is a
  harness problem, not an app defect.
- **Resolution:** closed the stale tab, then re-verified with a single polling client: the badge
  appears on incoming mail, the thread shows one unread dot, opening the thread clears both, and a
  thread whose first message had no subject displays the later subject.
- **Lesson:** before trusting a UI-observation mismatch, list the open tabs. A dead preview server
  does not stop an already-loaded page from continuing to talk to a shared daemon.
- **Related fix:** `loadMail` used to call `clearMailStore`, which also wiped unread. Any refresh
  that ran after an ingest (backup download, saving the mail handle, re-unlock) would have dropped
  the badge. `loadMail` now rebuilds thread content only and leaves unread alone.

### [WORKED AROUND] Re-seeding the manual UI harness produced a spurious relay `401`

- **Attempted:** built a two-peer harness (relay + two daemons) to check the Mail polish in a real
  browser, ran it, then re-ran it to pick up a port change. The second run began with
  `rm -rf /tmp/hush-mail-polish` while the first run's relay was still running and holding
  `relay.db`.
- **Symptom:** the recipient's `/api/recv` returned `{"error":"relay error: list failed: 401
  Unauthorized"}` and `/api/mail` stayed `[]`, which looks like a queue-credential regression. It
  is not: `scripts/smoke-mail.sh` (clean single run, fresh dir) passes the same store-and-forward
  path end to end, including the offline send and the reply.
- **Resolution:** stopped reusing a directory while processes hold it. The harness is disposable:
  kill relay + daemons first, then wipe, then seed once. This is a tooling/harness problem, not a
  repo problem.
- **Lesson:** a `401` from a hand-rolled harness is not evidence of a code regression until the
  hermetic smoke script reproduces it. Run the script first.

### [WORKED AROUND] `CODE` from a shell helper is lost inside `$(...)`

- **Attempted:** in `scripts/smoke-mail.sh`, captured the response of `api_at` (a helper that sets
  the global `CODE` and writes `$T/out.json`) with `THREAD=$(api_at ... | sed -n ...)`.
- **Symptom:** the smoke failed with `mail send returned 400: {"thread_id":"d862..."}` — a 400 that
  was actually the *previous* request's status. A command substitution runs in a subshell, so the
  `CODE=...` assignment inside `api_at` never reached the parent shell; the `[[ "$CODE" == "200" ]]`
  check read a stale value while the body was a genuine 200.
- **Resolution:** call `api_at` directly (not in `$(...)`) so `CODE` and the new `RESP` global
  propagate, then parse `$RESP`. Only wrap the pure text transform (`printf | sed`) in `$(...)`.
- **Lesson:** a shell helper that communicates through globals cannot be called in a command
  substitution. Either return via stdout and never globals, or call it as a statement.

### [FIXED] New `MessagePayload::Mail` variant broke an exhaustive match in `hush-cli`

- **Attempted:** added `MessagePayload::Mail { .. }` to `hush-core` and built the workspace.
- **Symptom:** `error[E0004]: non-exhaustive patterns: 'MessagePayload::Mail { .. }' not covered` at
  `crates/hush-cli/src/main.rs:118`. The daemon's `to_dto` was updated in the same pass, but the
  CLI's `recv` printer was missed because it lives in a different crate.
- **Resolution:** added a `Mail` arm to the CLI that prints the thread/subject/body and writes each
  attachment out via `account.read_blob`.
- **Lesson:** adding an enum variant to `hush-core` is a workspace-wide change; grep every
  `match ... payload` across all crates, not just the one being edited.

### [OPEN] UI screenshots are blank in this environment

- **Attempted:** Task 9 browser verification of the Mail rail. Captured the page with both
  `browser_take_screenshot` and CDP `Page.captureScreenshot` (decoded the base64 to PNG).
- **Symptom:** both produce a solid white image, while the same page's DOM is fully rendered
  (`browser_snapshot` shows the rail, thread list, thread view, composer; `Runtime.evaluate` returns
  the real text). The browser tab runs in the background and is not painting, so no pixels are
  produced. This is a tooling/environment limitation, not an app defect.
- **Resolution:** not resolved; visual evidence is unavailable here. Verification was done at the DOM
  level instead — the Mail rail, thread view, attachment chip, reply composer, Profile section, and
  New-mail composer were read from the live page, and a reply sent through the UI landed in the other
  peer's queue under the same thread id. A human should still eyeball the rail once on a machine where
  the tab paints.
- **Lesson:** when a screenshot comes back blank but the DOM is populated, suspect a non-painting
  background tab before suspecting the app.

### [FIXED] Integration test asserted one mail record where two are correct

- **Attempted:** in `a_reply_lands_in_the_same_thread`, asserted each side's `mail_records()` had
  exactly the one thread id.
- **Symptom:** the test failed with `left: [<id>, <id>]`, `right: [<id>]`. Each side keeps its own
  sent copy *and* the received reply, so two records sharing one thread id is the correct outcome.
- **Resolution:** asserted two records with the same id, which is a stronger statement about thread
  continuity anyway.
- **Lesson:** the first green assertion about message counts should be derived from the actual
  send/receive pairing, not assumed.

---

## 2026-10-07 — Multi-device Phase 4 landed (queue credential split, fan-out verified)

### [FIXED] `DeviceKeys::load` did not compile: a `u32` const matched against a `u64`

- **Attempted:** ran `cargo test --workspace` to confirm the queue credential split and the v1 device
  file migration before committing.
- **Symptom:** `error[E0308]: mismatched types` at `crates/hush-core/src/device.rs:152` —
  `expected u64, found u32`, with the note `` `DEVICE_FORMAT_VERSION` is interpreted as a constant,
  not a new binding ``. The match was `match version { 1 => .., DEVICE_FORMAT_VERSION => .. }` where
  `version` is `u64` and the const is `u32`.
- **Resolution:** the arm became `v if v == u64::from(DEVICE_FORMAT_VERSION)`. A guard is required
  because a path pattern cannot compare against a `u64`.
- **Lesson:** this is the second time a `match` arm meant as a comparison was silently read as a
  binding. Rust catches it only because the types differ here; with matching types it would have
  compiled and quietly shadowed. Use a guard when the arm is a comparison.

### [FIXED] Two relay suites presented the published token for read, delete and burn

- **Attempted:** after switching `list_blobs`, `get_blob`, `delete_blob` and `report_queue` to
  `authorize_read`, ran the workspace suite.
- **Symptom:** 5 failures across two suites, all `Relay("list failed: 401 Unauthorized")`:
  `crates/hush-core/tests/two_party_relay.rs` (`relay_blob_has_no_cleartext_sender_json`,
  `reporting_a_queue_burns_it`) and `crates/hush-relay/tests/relay_queues.rs`
  (`put_get_delete_roundtrip`, `expire_drops_blob`, `report_burns_queue`). Each test registered a
  queue with one token and then used that same token to read it back.
- **Resolution:** the tests now model the two halves of the credential pair. `hush-relay`'s tests
  derive the delivery token with `hush_core::queue_delivery_token(read)` and use it for `PUT` while
  presenting the read token for `GET`/`DELETE`/`POST .../report`. `hush-core`'s tests present the
  device's own read token, exposed as `Account::own_read_token()`. This is a genuine behaviour change
  and the tests were wrong, not the relay.
- **Lesson:** the failing tests were the *correct* outcome of the change, not collateral damage. A
  test that uses one credential for both a write and a read is asserting that those are the same
  credential, which is exactly the hole being closed.

### [FIXED] `own_read_token()` had to be added, because nothing could read its own inbox

- **Attempted:** fixed the two `two_party_relay.rs` tests to present a read token.
- **Symptom:** there was no way to get one. `Account` exposes `invite()`, which carries only the
  delivery token, and `device` is a private field, so an integration test had no access to the read
  credential at all.
- **Resolution:** added `Account::own_read_token()`, documented as the private credential for local
  callers that talk to the relay directly instead of going through `Account::recv`.
- **Lesson:** splitting a credential in two means every existing caller has to be classified as a
  reader or a deliverer. Two of them were tests that only worked because the two were the same value.

### [FIXED] The fan-out test could not refresh the contact's device list in time

- **Attempted:** wrote Task 8's test — link a second device of Alice's account, then have Bob send to
  Alice and assert both of Alice's devices receive it.
- **Symptom:** `Relay("list failed: 404 Not Found")` from `alice_2.recv()`, and Alice's device 1
  received the message. Bob had only ever delivered to device 1.
- **Cause:** `Account::add_contact` already calls `refresh_contact_devices` as a best-effort step, and
  that call sets `devices_fetched_unix` to now. Bob added Alice *before* she linked her second device,
  so his cache said "one device, checked just now" and the send inside the 5 minute TTL skipped the
  refresh. The linked device was invisible for up to the TTL.
- **Resolution:** the test calls `bob.refresh_contact_devices("alice")` explicitly and asserts it
  returns 2, standing in for the TTL expiring. This is Q5 working as designed, not a defect — the
  documented cost of caching the list. A newly linked device is unreachable for up to 5 minutes.
- **Lesson:** a cache seeded by a successful-but-empty check is indistinguishable from a fresh one.
  The first test run looked like a fan-out bug and was a caching consequence; the failing assertion
  named the wrong subsystem.

### [FIXED] `ContactDevice` had no `added_unix`, so a republish redated every device

- **Attempted:** implemented `record_linked_device`, which republishes the signed list via
  `publish_device_list`.
- **Symptom:** not a failure but a latent data-loss path. `publish_device_list` mapped carried-over
  devices with `added_unix: 0` and set its own entry to `now_unix()`, so every republish reset the
  date on every device. Adding a third device would have redated the second, and adding any device
  would redate device 1. Nothing read the value yet, so no test caught it.
- **Resolution:** `ContactDevice` gained `added_unix` with `#[serde(default)]`, projected through both
  `refresh_own_devices` and `refresh_contact_devices`, and `publish_device_list` now carries the
  stored value forward. This is also what Task 14's "added date" column needs.
- **Lesson:** the field was being written to the wire by one function and dropped by the other. A
  round-trip test over the list would have caught it; the shape tests only covered `Contact`.

### [FIXED] Task 9's core had to land early, because Task 8's test needs it

- **Attempted:** the plan says Task 8's test "cannot be written yet" until Phase 5's linking exists,
  and the Phase 4 note in `docs/plans/2026-10-06-multi-device.md` said the same.
- **Symptom:** confirmed by construction. A second device of an account can only be reached if it is
  in the account's signed list, and only device 1 can sign that, and device 1 can only sign a record
  it has been given. There is no path around it: the test needs `adopt_linked_vault` and
  `record_linked_device`.
- **Resolution:** implemented the two core functions, without the daemon endpoints or CLI wiring that
  Task 9 also lists. `Account::adopt_linked_vault(dir, passphrase, device_id)` opens a transferred
  vault, mints fresh device keys, clears the invite cache (which names the *sending* device's inbox)
  and clears `own_devices`. `Account::record_linked_device(&ContactDevice)` validates the id, stores
  the record and republishes. `Account::link_request()` is what the new device sends over the link.
- **Note:** `record_linked_device` takes the new device's own statement of its identity key, inbox and
  delivery token, and cannot verify it against the list — writing the list is the point. Device 1's
  signature is the only authority, so it must never be called with a request that did not arrive over
  an authenticated link channel. The relay enforces the same statement afterwards by refusing prekey
  records whose identity key is not what the list says.
- **Still missing for Task 9:** the daemon `link-device/start` and `link-device/complete` endpoints,
  the CLI, and any authentication on the link channel itself.
- **Lesson:** "this test cannot be written yet" is worth re-checking once the blocker is named. The
  blocker here was one function pair, not the whole phase.

### [FIXED] Overclaimed "clippy clean" by grepping away the warnings

- **Attempted:** reported the tree as clippy-clean after
  `cargo clippy --workspace --all-targets 2>&1 | rg "^(error|warning: unused|warning: this)"`.
- **Symptom:** the filter matched no warnings, so the report said clean. A later unfiltered run showed
  7 `redundant_closure` warnings in `crates/hush-core/src/backup.rs`, 4 `ptr_arg` warnings in
  `hush-cli`/`hush-daemon`, and 1 `unnecessary_to_owned` in the new
  `crates/hush-relay/tests/prekey_records.rs`.
- **Resolution:** the new warning is fixed. The other 11 predate this slice — verified with
  `git show HEAD:...` for the `ptr_arg` ones and by `backup.rs` being unmodified in `git status` — and
  were deliberately left alone so the Phase 4 commit does not carry an unrelated refactor.
- **Lesson:** a grep filter that can match nothing is not evidence. Grep for the positive
  (`warning`) and count, rather than enumerating the warning kinds you expect.

### [OPEN] 11 pre-existing clippy warnings, so "clippy clean" is still not a gate

- 7 `redundant_closure` in `crates/hush-core/src/backup.rs` (`map_err(|e| Error::Io(e))` where
  `Error::Io` would do), 4 `ptr_arg` in `crates/hush-cli/src/main.rs:208`,
  `crates/hush-daemon/src/account_worker.rs:402,451,494`.
- **Needed to close:** a mechanical fix plus a decision on whether to add `-D warnings` to CI. Same
  shape as the `cargo fmt` entry below: the linter cannot gate while it is not clean.
- **Tooling note, not a repo problem.**

### [WORKED AROUND] `perl -0pi -e` for a fixture edit silently did nothing

- **Attempted:** added `added_unix: 0` to 5 `ContactDevice` literals in
  `crates/hush-core/tests/multi_device.rs` with a multi-line `perl -0pi -e` regex.
- **Symptom:** exit code 1 and empty output. The regex did not match, so no edit was made and the
  following build failed with the same 5 `missing field` errors as before.
- **Resolution:** used the edit tool with `replace_all` per literal instead, which is also what the
  workspace rules require. Cost one build cycle.
- **Lesson:** bulk regex edits over Rust literals are a false economy; the compiler reports the same
  errors either way, but only the structured edit is atomic and verifiable.

---

## 2026-10-07 — Multi-device Phase 1 (per-device key material)

### [FIXED] Plan's Task 1 test could not compile as written

- **Attempted:** followed `docs/plans/2026-10-06-multi-device.md` Task 1 literally, which puts the
  `DeviceKeys` tests in an integration test at `crates/hush-core/tests/device_identity.rs` and calls
  `vault.master_key()`.
- **Symptom:** `Vault::master_key()` is `pub(crate)`, so an integration test cannot call it —
  the file would not have compiled. The plan had also written those tests as `#[test]` while
  `DeviceKeys::load`/`create` have to be `async`.
- **Resolution:** the `DeviceKeys` tests live in `#[cfg(test)] mod tests` inside
  `crates/hush-core/src/device.rs`, where `pub(crate)` is reachable. The plan's self-review notes
  flagged the `async` part but not the visibility part.
- **Lesson:** a plan that calls a crate-internal accessor from an integration test is wrong before
  it is written. Check visibility when choosing where a test lives.

### [FIXED] `DeviceKeys` had no `Debug`, breaking `unwrap_err` in its own tests

- **Attempted:** asserted a missing device file with
  `DeviceKeys::load(..).await.unwrap_err()`.
- **Symptom:** `error[E0277]: device::DeviceKeys doesn't implement Debug`, because `unwrap_err`
  requires the `Ok` type to be `Debug`. This is the second time this exact trap has been hit — the
  earlier `Account::open(..).unwrap_err()` in `backup_recovery.rs` failed the same way.
- **Resolution:** added a manual `impl Debug for DeviceKeys` that prints only `device_id` and calls
  `finish_non_exhaustive()`, mirroring `Vault`. That is the better fix anyway: a derived `Debug`
  would happily dump key material into a log line.
- **Lesson:** types holding secrets should get a hand-written `Debug` up front, not after a test
  failure. `Account` still lacks one and will hit this again.

### [FIXED] Corrupted grep output from misusing `rg -r`

- **Attempted:** `rg -rn "signal\.enc|receive_queue_id"` to find call sites.
- **Symptom:** the output came back with every match replaced by the letter `n`, e.g.
  `let signal_bytes = fs::read(dir.join("n"))`. `-r` is ripgrep's *replace* flag, so `-rn` means
  "replace matches with n", not "recursive with line numbers".
- **Resolution:** re-ran as `rg -n`. No code was changed on the basis of the bad output, but the
  corrupted result was briefly read as "references are in other files".
- **Lesson:** `rg` is already recursive; `-r` is never what is wanted for a search.

### [FIXED] Ambiguous `as_ref()` in `backup.rs` after the exclusion change

- **Attempted:** added an exclusion check to the tar loop, which required owning the relative path
  as a `String` (`into_owned()`) to test it.
- **Symptom:** `error[E0283]: type annotations needed` on
  `append_path_with_name(&entry, rel.as_ref())` — `String` implements `AsRef` for `str`, `Path`,
  `OsStr` and `[u8]`, so the call became ambiguous. The previous `Cow<str>` had resolved it.
- **Resolution:** pass `rel.as_str()` explicitly.
- **Lesson:** changing a `Cow<str>` to `String` can break an `as_ref()` that was previously
  unambiguous.

### [FIXED] Test asserted a file that a fresh vault does not have

- **Attempted:** asserted the backup still contained `chat.enc` to prove history was preserved.
- **Symptom:** `history must still be included: ["hush-backup-manifest.json", "account.enc",
  "identity.pub", "vault.lock.json", "recovery.lock.json", "recovery.enc", "identity.secret"]` —
  `chat.enc` is only written once there is a message, so a fresh vault has none.
- **Resolution:** assert `account.enc` instead, which is the shared vault state that always exists.
- **Lesson:** the assertion was testing an incidental file, not the property (shared data survives,
  device data does not). Asserting the property would have avoided the failure.

### [FIXED] Plan specified the wrong signature primitive for the device list

- **Attempted:** plan Task 3 signs the device list with `ed25519_dalek::SigningKey` and verifies with
  `VerifyingKey`, matching how `Vault` signs.
- **Symptom:** the device identity is a **libsignal Curve25519** key, not an Ed25519 key, so it
  cannot be passed to `ed25519_dalek` at all. The plan would not have compiled at Phase 2.
- **Resolution:** not yet applied — Task 3 is Phase 2. The device list must be signed with libsignal's
  `PrivateKey::calculate_signature` (XEd25519, as `mint_prekey_bundle` already does for signed
  prekeys) and verified with `IdentityKey::verify_signature`. `docs/plans/2026-10-06-multi-device.md`
  still shows the Ed25519 version and needs correcting before Phase 2 starts.
- **Lesson:** "sign it" is not specific enough. Identify which key type holds the signing key before
  choosing a signature library.

### [OPEN] `identity.secret` is unused and still rides along in backups

- **Attempted:** excluding this device's key material from backups surfaced that the archive
  contains `identity.secret` and `identity.pub`.
- **Symptom:** neither is used anywhere in the codebase outside `Vault`'s own tests —
  not relay auth, not invites, not the Signal identity. The account's `protocol_name` comes from the
  **signal** identity key, not this one. So it is a per-vault Ed25519 keypair that nothing reads.
- **Why it matters:** Q9 of the multi-device design requires signing the device list with an
  account-level key, and this is the obvious candidate. **Correction (same day):** the first version
  of this entry claimed the key must be excluded from backups like `device.enc`. That is backwards. A
  linked device has to be able to *publish* the device list, so the account signing key belongs in the
  **shared** vault and must be transferred at link time; keeping it out of backups would leave a
  restored vault permanently unable to publish a list at all. Security comes from rotating that key
  on revocation, not from keeping it out of the transfer.
- **Resolution:** needs a decision, not a patch. See the Phase 2 trust-chaining problem below.

### [OPEN] A restored backup now gets a fresh identity, and contacts are not told

- **Attempted:** excluding `device.enc` from backups means `import_vault` produces a vault with no
  device keys, so restore has to mint a new device identity or the vault is unusable.
- **Symptom:** implemented and tested (`a_restored_backup_gets_a_fresh_device_identity`), and the
  existing round-trip test still passes because it only compares `protocol_name`. But the restored
  device now has a **different identity key** from the one contacts have pinned for it, so sessions
  with existing contacts will fail to establish until they re-verify.
- **Resolution:** correct behaviour for this model, but not yet surfaced anywhere in the UI or the
  daemon. Phase 4/5 has to handle identity-change on the receiving side and the UI has to say
  something other than "restored". Recorded rather than hidden.
- **Lesson:** removing a key from a backup changes the restore path's semantics. The old test passing
  was not evidence the behaviour was still correct.

### [FIXED] Migration had to move from Phase 5 into Phase 1

- **Attempted:** the plan schedules the legacy migration (`signal.enc` -> `device.enc`) as Task 10
  in Phase 5.
- **Symptom:** Phase 1 stops writing `signal.enc`, so an existing vault would have neither file and
  every `Account::open` on a real vault would fail immediately. Migration is not optional work that
  can wait for a later phase.
- **Resolution:** migration landed in Phase 1 (`migrate_legacy_device` in `account.rs`, plus
  `legacy_vault_migrates_keeping_identity_name_and_inbox`). The legacy file is deleted only after
  `device.enc` is written, so a failed save cannot destroy the identity.
- **Lesson:** when a change removes a file, the read path for that file is part of the same task,
  not a follow-up.

### [OPEN] The tree is not `cargo fmt`-clean, so "run the linters" had no baseline

- **Attempted:** ran `cargo fmt --all -- --check` and `cargo clippy` before calling Phase 1 done,
  per the completion standard.
- **Symptom:** `cargo fmt` reports diffs in files this slice never touched —
  `apps/hush-tauri/src-tauri/src/daemon_client.rs`, `crates/hush-core/src/signal.rs`,
  `crates/hush-core/src/vault.rs`, `crates/hush-daemon/src/main.rs`. Checking the committed version
  (`git show HEAD:crates/hush-core/src/backup.rs | rustfmt --check`) also fails, so `backup.rs` was
  already unformatted before this slice edited it. There is no `rustfmt.toml`.
- **Resolution:** deliberately not fixed here. Running `cargo fmt` across the tree would bury the
  slice in unrelated diff; only the new file `crates/hush-core/src/device.rs` was formatted.
  Reformatting the tree wants its own commit.
- **Also:** 8 clippy `redundant closure` warnings, all pre-existing, in `backup.rs`
  (`.map_err(|e| Error::Io(e))`) and `signal.rs`. Not introduced by this slice and not fixed
  mid-feature, per the feature protocol.
- **Lesson:** establish the linter baseline before reading a diff as caused by the current change.
  Otherwise pre-existing noise looks like a regression.

### [FIXED] The device list had no workable trust root — resolved as Q11 (device 1 signs)

- **Attempted:** correct Task 3 of the plan so the device list is signed with a libsignal key
  (`PrivateKey::calculate_signature`, XEd25519) instead of `ed25519_dalek`.
- **Symptom:** fixing the primitive exposed that the design never said *which* key signs, and no
  answer survived contact with the threat model on the first pass. The relay is untrusted
  (`THREAT_MODEL.md` treats us, the operators, as an attacker), so peers cannot take device
  membership on the relay's word. The signature must chain to something the peer already pinned, and
  the only thing pinned is the account id.
- **Resolution:** settled as **Q11 — device 1 is the signing root.** The list is signed with device 1's
  libsignal identity private key and verified against the account id, which *is* device 1's identity
  key hash (Q7). No new pinned trust material is needed: the peer verifies with the one thing it
  already has. Costs accepted: device 1 must be online and unlocked to add or revoke, and device 1
  cannot itself be revoked (losing it means rotating the account and re-inviting everyone). A linked
  device cannot sign, because device 1's private key is never transferred — which is precisely what
  Phase 1 guarantees by keeping `device.enc` out of backups.
- **Also settled by this (superseded — see the Phase 2/3 entry below):** the Q9 account token and the
  signature were initially held to answer *different* questions, with the signature proving
  authenticity to peers and the token gating who may write. That split did not survive implementation:
  the token was deleted, and the signature now does both jobs. Recorded here because the original
  reasoning is what made the wrong design look coherent.
- **Consequence:** `identity.secret` is not the account signing key after all, so it stays unused dead
  code. Still an open cleanup decision.
- **Lesson:** "authorize the write" and "sign the payload" are separate questions. Settling one and
  assuming it settles the other leaves a design that cannot be implemented.

### [FIXED] The plan's tamper-detection test proved nothing about the signature

- **Attempted:** implemented Task 3 including the plan's `signed_list_verifies_and_detects_tampering`,
  which flips a byte at `len / 2` and asserts that verification fails.
- **Symptom:** mutation testing — commenting out the `verify_signature` call and re-running — left that
  test **passing**. Flipping an arbitrary byte in the middle of the blob corrupts the JSON, so
  `serde_json::from_slice` fails first and the test never reaches the signature check. It was asserting
  on the parser, not the signature. The wrong-key test *did* catch the mutation, which is what exposed
  the difference between the two.
- **Resolution:** the tampering test now rewrites a byte inside a string value (`queue-1` -> `pueue-1`),
  which keeps the payload valid JSON, and asserts the payload still parses *before* asserting that
  verification fails. Re-ran the mutation: with `verify_signature` disabled, both tests now fail.
- **Lesson:** a test named after a security property is not evidence of that property. Disable the check
  and confirm the test fails, or the assertion may be riding on a completely different code path.

---

## 2026-10-07 — Multi-device Phase 3 (relay device directory, revocation)

### [FIXED] Trust-on-first-use token auth made revocation impossible

- **Attempted:** implemented `PUT /d/{account_id}` with the Q9 account token as the write credential,
  mirroring the queue rule: the first writer mints the token and every later write must present it.
  Revocation was then supposed to rotate the token, so the revoked device's copy stopped working.
- **Symptom:** wrote `rotating_the_token_currently_breaks_publishing` to exercise the revocation path
  end to end, and it failed. Device 1 rotates the token, publishes the updated list with the new token,
  and the relay answers `401` — because the stored `token_hash` is still the *old* token's hash. The
  account cannot perform the rotation that revokes anyone. Under first-write-wins there is no way for a
  new token to authenticate itself before the relay has already accepted it, so rotation is not a
  recoverable operation at all.
- **Also wrong with the model, independent of the bug:** even patched, the gap between "token rotated
  locally" and "relay has the new hash" is a window in which the revoked device still holds a working
  credential. And a first-write-wins rule makes any two-writer race a coin flip on arrival order, which
  is not an acceptable property for the operation that revokes a device.
- **Resolution:** user decision — `PUT /d/{account_id}` is authorized by **device 1's signature on the
  body**, verified by the relay against the account id (`hush_core::DeviceList::verify(body,
  account_id)`), and the account token is deleted outright. There is no credential to rotate, so there
  is no rotation race and no window: a revoked device does not hold device 1's private key and
  therefore cannot produce a body the relay will accept. `GET` stays unauthenticated, because a peer
  fetching a contact's list holds no credential for that account and does not need one — the signature
  is what makes the contents trustworthy.
- **Consequence:** `AccountToken`, `DeviceList::token`, `DeviceList::token_epoch` and the
  `device_dirs.token_hash` column are gone. `epoch` survives as a peer-visible freshness counter only,
  so a peer holding a cached list can tell that revocation happened. `identity.secret` remains unused
  dead code, as noted in the Q11 entry above.
- **Lesson:** trust-on-first-use fits a queue — a resource with one owner and no membership changes.
  It does not fit a resource whose whole purpose is to change membership, because the operation that
  changes membership is precisely the one that needs the new credential to already be trusted. If the
  write is signed by a key the reader already pins, authorization comes free and the token is
  redundant.

### [FIXED] The relay test could not construct a signed list

- **Attempted:** wrote `crates/hush-relay/tests/device_directory.rs` so a valid `PUT` carries a real
  device-1 signature, which is the only way to exercise the new authorization rule.
- **Symptom:** `error[E0432]: unresolved import libsignal_protocol` — `hush-relay`'s
  `[dev-dependencies]` contained no libsignal and no `rand`, so the test could not build an
  `IdentityKeyPair` to sign with.
- **Resolution:** added `libsignal-protocol` and `rand` to `crates/hush-relay/[dev-dependencies]`,
  pinned to match `hush-core`. The relay's own code still has no dependency on libsignal; only its
  tests do.
- **Lesson:** moving an authorization decision into a shared library makes the enforcing service's
  tests depend on that library's types. Budget the dev-dependency when relocating the check, or the
  test that proves the rule cannot be written.

### Checked and clear: dropping `token_hash` from an uncommitted table is not a migration risk

- **Concern:** `device_dirs` was created in this same uncommitted slice with `token_hash BLOB NOT NULL`.
  Changing the DDL to drop the column is safe only if no database exists with the old shape, because
  `Store::open` uses `CREATE TABLE IF NOT EXISTS` plus a best-effort `ALTER TABLE ... ADD COLUMN` and
  has no way to drop a column or migrate a schema version. A stale table would make every publish fail
  with `NOT NULL constraint failed: device_dirs.token_hash`, surfacing as a `500`.
- **Evidence:** `git show HEAD:crates/hush-relay/src/store.rs` has no `device_dirs` — the table has
  never been committed. Both dev databases (`./relay.db`, `./instances/relay/relay.db`) contain only
  `queues` and `blobs`, so they predate the table entirely and will get the new shape on next open.
- **Resolution:** no migration needed. Recorded so the check is not repeated.
- **Lesson:** with an append-only `CREATE TABLE IF NOT EXISTS` schema and no version table, a column
  can only be removed safely while the table is still unreleased. Confirm that before editing DDL.

---

## 2026-10-07 — Multi-device Phase 4 (contacts as device sets, fan-out)

### [FIXED] Nothing publishes a device's prekey bundle, so fan-out has no bundle to build a session from

- **Attempted:** started Task 7/8 and went to fill in `ContactDevice`, which the plan specifies as
  `{ device_id, identity_key, registration_id, queue_id, queue_token, pre_key_* }`.
- **Symptom:** there is nothing to populate `pre_key_*` from. The only structure published to the relay
  is `DeviceRecord` (`crates/hush-core/src/device_list.rs`), and it carries `device_id`, `identity_key`,
  `registration_id`, `queue_id`, `queue_token`, `added_unix` — **no prekey material at all**. The plan
  is internally inconsistent: Task 3 defines a `DeviceRecord` with no prekeys, and Task 7 expects a
  `ContactDevice` with them.
- **Why it matters:** the invite (`InvitePayload`) carries a full `PreKeyBundle` out of band, and that
  is the *only* way a bundle has ever been delivered. It works because you exchange an invite with the
  one device you are adding. It does not scale to a device you have never exchanged an invite with,
  which is exactly what Task 8's fan-out does — sending to every device of a contact requires a
  `PreKeyBundle` for each of them, and there is currently no source for it.
- **Blocked on:** a decision among three models (per-device prekey endpoint; prekeys inside the device
  list; bundles handed over the established encrypted session). Raised with the user rather than
  guessed, because the choice has consequences beyond Phase 4 — see the note on one-time prekeys below.
- **Resolution (same day):** the user chose the **per-device prekey endpoint**. Each device publishes
  its own record at `PUT /k/{account_id}/{device_id}`, signed by its own identity key, and the relay
  verifies the record's `identity_key` against that device's entry in the device-1-signed list before
  storing it. `GET /k/{account_id}/{device_id}` pops exactly one one-time prekey out of the stored
  pool and returns it, so a peer that fetches a bundle consumes a prekey and nobody else can get the
  same one. An exhausted pool serves the record with no one-time prekey and libsignal falls back to
  the signed prekey. Implemented in `crates/hush-core/src/prekey.rs`, `crates/hush-relay/src/store.rs`
  (`take_prekey_record`) and `crates/hush-relay/src/app.rs`; 9 tests in
  `crates/hush-relay/tests/prekey_records.rs`.
- **Note:** the plan's `ContactDevice` still lists `pre_key_*` fields and should not. They are fetched
  per session, not cached in the contact, or they go stale the moment the device republishes.
- **Consequence for forward secrecy that drove the decision:** a bundle published in a *static list* can
  never carry a one-time prekey, because a one-time prekey has to be consumed by exactly one fetch and
  a list is read by everyone. libsignal accepts a bundle with `pre_key: None`, so this works, but the
  first message then gets forward secrecy from the signed prekey alone. Only a per-device prekey
  endpoint that hands out and deletes one prekey per request preserves the full Signal property. That
  is a real security difference, not a style preference, so it should drive the decision.
- **Lesson:** "the device list tells peers where to deliver" and "the device list tells peers how to
  start a session" are different jobs, and the plan conflated them. Membership is signed by device 1;
  prekey material is per-device and rotates per-device, so signing them with the same key couples
  every device's session setup to device 1 being online.

---

### [FIXED] Fan-out is written but unreachable, so it has no end-to-end test

> **Closed 2026-10-07.** `Account::adopt_linked_vault` and `Account::record_linked_device` landed as
> part of the Phase 4 slice (see the newest section), which is the linking pair this entry was blocked
> on. The test now exists as `a_message_reaches_every_device_of_the_recipient_and_our_own` in
> `crates/hush-core/tests/multi_device.rs` and passes, asserting both directions: Bob's message lands
> in both of Alice's inboxes, and Alice's message reaches Bob *and* is filed as outgoing on her second
> device. It was mutation-checked — disabling the own-device fan-out makes it fail on the
> "linked device must file what device 1 sent" assertion. The `#[ignore]`d queue-leak test named below
> was replaced by `crates/hush-relay/tests/queue_credentials.rs`, which asserts the opposite
> behaviour and passes.

- **Attempted:** finished Task 8 (fan out to every `ContactDevice`, plus an `OwnCopy` to each of our
  own other devices) and went to write the plan's test,
  `a_message_reaches_every_device_of_the_recipient_and_our_own`.
- **Symptom:** no test can build the scenario. A contact's `devices` set is only ever filled from an
  `InvitePayload`, and an invite describes exactly one device, so every contact in a test has one
  device and the fan-out loop runs once. `own_devices` is only filled by `refresh_own_devices`, which
  reads a device list from the relay — and the only way to publish a list containing a second device
  is to already have one in `own_devices`. There is no `Account` API to add a device, and `state` is
  private, so a test cannot inject one. The linking flow that would break the cycle is Phase 5
  (Tasks 9-11).
- **What is verified instead:** `cargo test --workspace` passes (including the 6 tests in
  `crates/hush-core/tests/multi_device.rs` for the `Contact` shape and legacy migration, and the 9
  prekey-endpoint tests), and the fan-out code path compiles and is exercised for the single-device
  case by the existing `two_party_relay.rs` suite. The multi-device branch itself is **not** covered.
- **Resolution needed:** Phase 5's device-list fetch must populate `contact.devices` from the relay
  before Task 8 can be tested as written. Do not mark Task 8 done on the strength of a green suite —
  the suite passing here means the new branch is untested, not correct.
- **Lesson:** "it compiles and everything passes" and "the new code ran" are different claims. When a
  new branch has no reachable entry point, the test suite is silent about it by construction.

---

### [FIXED] The public device list publishes the queue token, which is a full read/write/burn credential

> **Closed 2026-10-07 by the credential split, before Phase 5 continued.** `read_token` is 32 random
> bytes generated on the device and stored only in `device.enc`; `delivery_token` is
> `HMAC-SHA256(read_token, "hush-queue-delivery-v1")` and is what goes in the invite and the
> `DeviceRecord`. The relay stores `sha256(delivery_token)`. `put_blob` and `presence_last_at` check
> the presented token against that hash; `list_blobs`, `get_blob`, `delete_blob` and `report_queue`
> instead recompute `HMAC(presented)` and check *that*, so a published delivery token cannot read,
> drain or burn the inbox it names. No relay schema change was needed — the stored hash simply
> changed meaning — and there is no second credential to register or rotate. Existing vaults rotate
> once on load (`device.enc` v1 to v2): the identity and every session are kept, the queue is
> replaced, and the caller republishes the list. Evidence: `crates/hush-relay/tests/queue_credentials.rs`
> (3 tests) plus the workspace suite.
>
> **What this does not fix:** the delivery token still authorizes `PUT` into that inbox, so a stranger
> who knows the account id can still flood a device's queue. Burning — the durable denial of service
> this entry was raised for — is closed. Rate limiting is the existing mitigation for flooding.

- **Attempted:** started Task 9 (device linking) and went to have device 1 record the new device's
  queue in the signed list, which meant deciding what a `DeviceRecord` may carry.
- **Symptom:** `DeviceRecord` carries `queue_token`, and `GET /d/{account_id}` is deliberately
  unauthenticated. But the same token authorizes `PUT /q/{id}` (deliver), `GET /q/{id}` (list),
  `GET /q/{id}/{blob}` (read), `DELETE /q/{id}/{blob}` (delete) and `POST /q/{id}/report` (burn).
  So the moment an account publishes a device list, anyone who knows the account id can read,
  delete and burn that device's inbox. Demonstrated live in
  `crates/hush-relay/tests/queue_token_leak.rs` (currently `#[ignore]`d, asserting the *required*
  behaviour): a request with the published token returns `200` on `GET /q/{queue_id}` and lists the
  owner's queued blobs.
- **Why it matters:** the account id is not a secret — it is the `protocol_name` in every invite the
  account has ever handed out, and it is inside the list itself. This is not plaintext disclosure
  (the relay only holds ciphertext), but it breaks three things the project claims:
  - `docs/THREAT_MODEL.md` protects "who talks to whom, as seen by our relay, so it is not a
    social-graph database", and states the relay stores blobs "on random queue IDs, not Alice's
    inbox". A public `account_id -> queue_id` map plus a readable inbox defeats that.
  - `docs/THREAT_MODEL.md` lists "Invite/queue tokens" as an asset. This publishes them.
  - Burning is the worst case, not reading. A stranger can burn a device's queue permanently, and a
    **device 2 cannot replace a burned queue**: only device 1 can sign a new list, so recovery
    depends on device 1 being online and unlocked. That is a durable denial of service.
- **Pre-existing, not introduced here:** the invite already carries the same token
  (`InvitePayload.queue_token`) and the same relay routes authorize with it, so any contact the
  account has ever invited can already read, delete and burn its inbox. Multi-device widens the
  audience from "contacts" to "anyone who knows the account id" and makes it permanent by putting
  the queue in a list that must be republished.
- **Fix shape (not applied):** split the credential. The relay keeps a private **read token** that
  never leaves the device and authorizes list/get/delete/report; a separate **delivery token** is
  what goes in the invite and the device list and authorizes only `PUT` (deliver) and presence.
  That is a relay schema change (`queues` gains a second token hash) plus an invite and
  `DeviceRecord` change, which is why it is raised rather than patched.
- **Lesson:** a token that gates both reading and writing cannot be handed to anyone who only needs
  to write. Publishing an address and publishing a credential are different acts; the plan's
  `DeviceRecord` conflated them.

---

## 2026-10-06 — Member calls spec (Task 17, P2P WebRTC)

### [FIXED] Assumed the Tauri webview could host WebRTC on Linux

- **Attempted:** first pass at the audio/video design put the WebRTC peer connection in the
  Tauri webview (browser `RTCPeerConnection` + `getUserMedia`), with the daemon relaying only
  signaling. The webview is a browser, so this looked like the low-effort path.
- **Symptom:** research contradicted it. WebKitGTK (Tauri's Linux renderer) does **not**
  expose `RTCPeerConnection` by default — it is behind an experimental build-time flag that
  no distro enables. Tauri maintainers: *"With Tauri/webkit no, with Electron/CEF yes"*
  (`tauri-apps/wry#85`); users see *"ReferenceError: Can't find variable: RTCPeerConnection"*
  (`tauri-apps/tauri#13143`). The earlier chat message to the user said WebRTC in WebKitGTK
  is *"historically the flakiest part"*, which understated it: it is absent, not flaky.
- **Resolution:** the spec
  ([`docs/tasks/member-calls.md`](docs/tasks/member-calls.md)) moves the peer connection and
  media pipeline into `hush-daemon`; the webview is UI chrome only. Media engine recommendation
  is GStreamer `webrtcbin`.
- **Lesson:** verify platform WebRTC support (not just `getUserMedia`) against the actual
  renderer before designing a webview media path. A webview being "a browser" is not enough.

### [FIXED] Set the call-transport default against the project's own threat model

- **Attempted:** the first spec defaulted to **relay-only TURN** so a contact would never learn
  your IP, on the reading that "hide the IP" was the priority.
- **Symptom:** it contradicted `THREAT_MODEL.md`, which treats **us (operators)** as an attacker
  and makes "our relay must not map who talks to whom" an explicit v1 constraint. A first-party
  TURN puts us in a position to see the call graph — a *stronger* graph signal than chat,
  because a real-time relay session links both IPs unambiguously. The stated goal ("P2P without
  exposing the IP") is itself only achievable by relaying, so the default traded a small leak
  for a bigger one.
- **Resolution:** after discussion the default was flipped to **direct P2P**. Contact IP
  disclosure is accepted (contacts are mutually trusted); there is **no first-party relay**;
  IP-level privacy is deferred to the OS/distro layer or a user-supplied relay/overlay. Recorded
  in [`docs/tasks/member-calls.md`](docs/tasks/member-calls.md).
- **Lesson:** check a proposed default against the project's own threat model before writing it
  down. "Protect the user from X" is not automatically aligned if it strengthens a different
  adversary the model already names.

---

## 2026-10-06 — Passphrase change (vault format v2, daemon endpoint, Settings UI)

### [FIXED] `pkill -f "vite"` killed the shell that ran it

- **Attempted:** one cleanup call that killed the test daemon and Vite, then `rm -rf`'d the
  temp dirs.
- **Symptom:** the call returned completely empty output — not even the `echo "cleaned"` at
  the end. `pkill -f` matches against full command lines, and the shell's own command line
  contained the literal string `vite`, so it signalled itself. Everything after the `pkill`
  never ran, leaving the daemon, Vite and both temp dirs alive while looking like a success.
- **Resolution:** killed by explicit PID.
- **Lesson:** `pkill -f <pattern>` can match the command line of the shell issuing it. This is
  the same family as the earlier `pgrep -f` entry — pattern matching over command lines
  includes the matcher.

### [FIXED] `unwrap_err()` does not compile on a type without `Debug`

- **Attempted:** `Account::open(dir, "old-pass").await.unwrap_err()` in the new recovery test.
- **Symptom:** `error[E0277]: 'Account' doesn't implement 'Debug'` — `unwrap_err` needs the
  `Ok` type to be `Debug` so it can print it in the panic message.
- **Resolution:** matched on the `Result` and panicked on `Ok` explicitly. `Account` holds key
  material, so adding a `Debug` impl just to satisfy a test helper would be the wrong fix.
- **Lesson:** for a `Result` whose `Ok` type is deliberately opaque, match instead of
  `unwrap_err()`.

### [FIXED] `input.autocomplete` is `AutoFill`, not `string`

- **Symptom:** `error TS2322: Type 'string' is not assignable to type 'AutoFill'` from a
  helper taking `autocomplete: string` and assigning it to the input.
- **Resolution:** typed the parameter as `AutoFill`. Caught by `tsc --noEmit` before any
  runtime check.

### [WORKED AROUND] Browser `Runtime.evaluate` fails in this environment

- **Attempted:** read page text with `browser_cdp` + `Runtime.evaluate` to confirm state after
  clicking Unlock.
- **Symptom:** both attempts returned
  `TypeError: Cannot read properties of null (reading 'split')` from the CDP bridge, for a
  trivial expression. Cost two round trips.
- **Resolution:** used `browser_snapshot` instead, which returns the accessibility tree and
  was sufficient for every check (validation message, error text, disabled state, cleared
  fields).
- **Note:** tooling limitation, not a repo problem.

### [OPEN] Existing vaults are not migrated to format v2 automatically

- **Detail:** `Vault::create` writes v2 (random master key, wrapped by the passphrase) and
  `Vault::change_passphrase` upgrades a v1 vault in place, but `Vault::open` deliberately does
  **not** rewrite the header. So any vault created before this slice — including the dogfood
  peers under `instances/` — stays on v1 until someone changes its passphrase.
- **Impact:** the claim "vaults use format v2" is only true for new vaults. v1 vaults keep
  working, and a passphrase change silently upgrades them, but nothing forces it.
- **Why not auto-migrate:** `open` is also used on read-only and removable mounts, and
  rewriting key material as a side effect of unlocking is a surprising thing to do. Verified
  the v1 path explicitly (`v1_vault_opens_and_upgrades_without_re_encrypting`).
- **Next:** if v1 should be retired, add an explicit upgrade on unlock with a logged
  migration, or a `hush-cli` migration command.

---

## 2026-10-06 — 5c.4 (relay HTTPS policy, relay unit, deploy checklist)

### [FIXED] IPv6 loopback was dead code in the HTTPS policy

- **Symptom:** the loopback check was `host == "[::1]" || host == "::1"`, but the
  host was extracted with `authority.split(':').next()`. For `http://[::1]:8787`
  that yields `"["`, so **neither** IPv6 arm could ever match and release builds
  wrongly refused a legitimate loopback relay.
- **Why it survived:** the guard was wrapped in `cfg!(debug_assertions)`, so no
  debug test could reach the branch. Nothing exercised it.
- **Resolution:** parse with `reqwest::Url` and strip the brackets `Url::host_str`
  keeps around IPv6 literals, then check `IpAddr::is_loopback()`. This also
  correctly allows `127.0.0.2`, which the old literal list rejected.
- **Lesson:** hand-rolled URL parsing produced a bug that a real parser makes
  impossible. Reach for the parser that is already a dependency.

### [FIXED] Nearly reported a doc/code mismatch that did not exist

- **Attempted:** grepped `crates/hush-core/src/config.rs` for the HTTPS policy,
  found nothing, and started writing up "the checklist documents a policy the code
  does not implement".
- **Symptom:** the policy is in `relay_client.rs`; `config.rs` merely calls it. A
  single-file grep was treated as a repo-wide search, and the wrong conclusion was
  one step from being reported as a finding.
- **Resolution:** searched the whole workspace before claiming absence, which is
  what surfaced both the real code and the actual `[::1]` bug.
- **Lesson:** grep one file, conclude nothing. This is the failure mode the search
  rules exist to prevent, and it happened anyway.

### [WORKED AROUND] TLS termination could not be verified end-to-end locally

- **Attempted:** stand up a TLS terminator in front of the relay, point a client at
  `https://localhost:8443` with a self-signed certificate, and run a real exchange.
- **Symptom:** the client is built with `rustls-tls` and bundles Mozilla's roots
  (`webpki-roots`), so it ignores `SSL_CERT_FILE` and the system trust store. A
  self-signed certificate cannot be trusted without changing client code.
- **Resolution:** did **not** add a `danger_accept_invalid_certs` escape hatch just
  to enable a test — that would be a real footgun in a privacy tool. Instead
  verified the proxy hop over plain HTTP (all relay routes are path-transparent
  through a proxy) and documented the constraint: a publicly trusted CA is
  mandatory, and there is no client-side override.
- **Note:** the constraint turned out to be a useful documentation finding rather
  than a blocker — it rules out internal CAs for VPS deployments.

---

## 2026-10-06 — DR-06 / DR-07 (systemd hardening + daemon secret handling)

### [FIXED] `mlockall` policy assumed an unlimited `RLIMIT_MEMLOCK`

- **Attempted:** gate `MCL_FUTURE` on `setrlimit(RLIMIT_MEMLOCK, INFINITY)` succeeding and
  reading back as exactly `INFINITY`.
- **Symptom:** on this machine the default is 2 GB soft/hard, and raising the hard limit
  needs `CAP_SYS_RESOURCE`. So `setrlimit` fails, the check returns false, and the daemon
  would have silently degraded to locking only current pages. This is not a corner case — it
  is the default on a normal desktop, and it means key material allocated *after* startup
  stays swappable while the log still says memory locking is on.
- **Resolution:** treat headroom as sufficient when the effective soft limit is
  `>= 256 MiB`, which covers both `LimitMEMLOCK=infinity` and a typical desktop default.
- **Lesson:** "did the privileged call succeed" is the wrong gate for a capability that has a
  usable non-privileged path.

### [FIXED] `Hardening::summary()` reported the inverse of the memory-lock state

- **Attempted:** one `on_off(bool)` helper shared by both fields, where `true` printed `"off"`
  (correct for core dumps, since off is the good state there).
- **Symptom:** `memory_locked=off` would have been logged while memory was in fact locked,
  because the same inverted helper was reused for a field where "on" is the good state. The
  unit test expectation is what exposed it, not the runtime log.
- **Resolution:** separate literals per field; the helper is gone.
- **Lesson:** a shared formatter that encodes a per-field *meaning* is a trap.

### [OPEN] Five systemd hardening directives cannot be verified in this environment

- **Directives:** `CapabilityBoundingSet=`, `PrivateDevices`, `ProtectKernelModules`,
  `ProtectKernelLogs`, `ProtectClock`.
- **Symptom:** `systemd-run` fails with `Failed to drop capabilities: Operation not
  permitted` and `status=218/CAPABILITIES` before the daemon is even exec'd. Each of these
  directives drops a capability internally, and the sandbox here forbids that.
- **Impact:** they ship unproven. If the daemon needed one of those capabilities, this
  environment would not catch it — the failure would look identical.
- **Next:** verify on the dogfood machine outside the sandbox. Everything else in the unit was
  exercised individually and passes.

### [OPEN] `ProtectHome=read-only` did not demonstrably apply

- **Symptom:** under `ProtectHome=read-only` with `ReadWritePaths=<data root inside $HOME>`,
  writing to `$HOME/hush-should-fail` (outside the data root) **succeeded**, while `/etc` was
  correctly denied by `ProtectSystem=strict`. `ProtectHome` alone, without `ReadWritePaths`,
  also allowed the write, and the journal showed no warning.
- **Impact:** cannot claim the rest of `$HOME` is read-only for the daemon on this machine.
  The daemon does work under the directive and can write its data root, which is the part that
  actually matters for function.
- **Next:** re-check on a normal host. `systemd-detect-virt` reports `none`, but an earlier
  journal line blamed a "container manager" for skipping `ProtectHostname` namespace setup.

### [FIXED] A DNS test that proved nothing

- **Attempted:** start the daemon with `HUSH_RELAY_URL=http://<bogus-host>:8787` and compare
  relay errors with and without `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`.
- **Symptom:** both runs errored against `http://127.0.0.1:8787`, not the bogus host. A vault
  keeps the relay it was created with, so the env var at daemon start was ignored and the test
  never touched DNS at all. Two runs agreeing looked like a clean result.
- **Resolution:** create the vault with the env var set, then compare. Separately confirmed
  `getent hosts example.com` resolves with rc=0 under the restriction, so there is no relay-DNS
  regression.
- **Lesson:** confirm the test reached the intended code path before trusting a "no
  difference" result.

### [FIXED] `systemctl show` on a stopped transient unit returns manager defaults

- **Attempted:** verify `MemorySwapMax=0` and `LimitMEMLOCK=infinity` after the probe unit had
  been stopped and collected.
- **Symptom:** `MemorySwapMax=infinity`, `LimitMEMLOCK=2083516416` — the defaults, which read
  as "the properties were never applied".
- **Resolution:** query while the unit is running.

### [WORKED AROUND] `journalctl` read before flush looked like a real regression

- **Symptom:** two back-to-back transient units showed empty stdout, which read as "DNS
  resolution is broken under the address family restriction". Re-running the identical
  configuration with explicit `echo START` / `echo END` markers resolved the hostname fine.
- **Resolution:** re-run with markers before drawing a conclusion. Cause not pinned down.

### [FIXED] `pgrep -f <pattern>` matched the wrapping shell too

- **Symptom:** `P=$(pgrep -f "hush-daemon --dir X")` returned two PIDs, so `/proc/$P/status`
  expanded to `/proc/1234\n5678/status` and `awk` failed with `Is a directory`.
- **Resolution:** use `pgrep -x hush-daemon`, or capture `$!` when the script starts the
  process itself.

### [WORKED AROUND] Backgrounding a process inside a single shell call produced empty output

- **Attempted:** `cmd >log 2>&1 & sleep 3; cat log` in one shell invocation, plus `{ ... & }`
  and `setsid ... &` variants.
- **Symptom:** the call returned exit 0 with no output at all, repeatedly, while the background
  process did start. Cost several round trips before the pattern was recognised.
- **Resolution:** move start-and-probe sequences into a script file and run the script.
- **Note:** tooling quirk, not a repo problem.

### [FIXED] Doc comment and `#[derive]` joined onto one line

- **Symptom:** a string replace that stripped a trailing newline left
  `/// ... at startup.#[derive(Debug, Clone)]`. It compiled and was clippy-clean, so only
  reading the file back caught it.
- **Lesson:** after a replace that touches line boundaries, read the region back.

### [FIXED] rustfmt split one `if`/`else` but not its sibling

- **Symptom:** `let core = if ... { "off" } else { "ON" };` is 51 characters, and rustfmt's
  `single_line_if_else_max_width` defaults to 50, so it expanded while the 45-character
  sibling stayed inline. The result looks inconsistent until you know the rule.
- **Resolution:** applied rustfmt's form.

### [OPEN] Repo is not rustfmt-clean, so `cargo fmt --check` cannot gate

- **Pre-existing, not introduced here.** `crates/hush-daemon/src/main.rs`,
  `account_worker.rs` and `suspend_lock.rs` all report rustfmt diffs, and
  `account_worker.rs` has 3 `clippy::ptr_arg` warnings (`&PathBuf` params).
- **Impact:** a `--check` gate fails on untouched code, so formatting regressions in new code
  are easy to miss.
- **Next:** a dedicated formatting cleanup slice, then wire `--check` into CI.

---

## 2026-10-05 — DR-02 (Unix socket default + Tauri native proxy)

Backfilled from the slice summary; less detail than the entries above.

### [FIXED] axum `FileTypeExt::is_socket` did not compile

- **Symptom:** the socket-liveness check needed `std::os::unix::fs::FileTypeExt` in scope.
- **Resolution:** imported it and isolated the check behind an `is_socket_file` helper.

### [FIXED] `hyper-util` legacy `Client` was unavailable

- **Symptom:** the Tauri proxy needs `hyper` 1.x's legacy client over a `UnixStream`, but the
  type was gated behind Cargo features that were not enabled.
- **Resolution:** added `client-legacy`, `http1` and `tokio` features to
  `apps/hush-tauri/src-tauri/Cargo.toml`.

### [FIXED] Smoke-script cleanup trap killed the test shell

- **Symptom:** `trap 'kill "${DAEMON:-0}" ...' EXIT` ran `kill 0` when a second PID was unset,
  signalling the whole process group — including the shell running the test.
- **Resolution:** iterate over the non-empty PIDs instead of relying on a default.

### [FIXED] Smoke-script reset variable ordered after use

- **Symptom:** `DAEMON=""` was placed after `DAEMON=$!`, so the PID was clobbered.
- **Resolution:** declare variables above the process spawns.

### [FIXED] Hermetic smoke script depended on a live relay

- **Symptom:** the presence-heartbeat check failed with no relay running, making the "hermetic"
  suite environment-dependent.
- **Resolution:** removed relay-dependent calls; that path is covered by the E2E run instead.

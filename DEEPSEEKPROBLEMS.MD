# DeepSeek implementation problems

Running log of problems hit while implementing slices. Kept so the same dead ends are not
re-explored and so unverified claims stay visible instead of being quietly dropped.

**Convention:** append an entry when a problem is hit, not retroactively at the end of a
slice. Newest section first. One entry per problem, with what was attempted, the symptom, the
resolution, and the status.

Status tags: `FIXED` (resolved in-slice), `WORKED AROUND` (avoided, cause not fixed),
`OPEN` (unresolved, needs a different environment or a later task).

---

## 2026-10-07 — Internal member mail (Task 5d)

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

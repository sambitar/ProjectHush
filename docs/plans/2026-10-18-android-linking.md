# Android and vault storage — link the phone, and what it carries

> **Status:** **D (Places) settled** (grill Round 3, 2026-10-19). Desktop slice in progress on
> `feature/places-desktop` — see [`2026-10-19-places-desktop.md`](2026-10-19-places-desktop.md).
> Raised by the question: "Android doesn't have enough space for a vault and stuff — can we
> just link it?"
> Standing context that constrains any Android work: [`hush-postal-bridge.md`](hush-postal-bridge.md)
> §5 (Android scanner), [`../WORKSPACE.md`](../WORKSPACE.md) ("Android: later, same Tauri project").

## 1. The problem, restated

A phone cannot be required to carry a full vault. The vault has two very different parts:

| Part | Contents | Growth |
|---|---|---|
| Vault core | `device.enc`, `identity.secret`, `account.enc`, `chat.enc` (encrypted SQLite of text history) | Small — KBs to tens of MB, ever |
| Blob store | `blobs/` — content-addressed encrypted blobs: attachments, Drops (up to 8 MiB each), zine media, mail attachments, torrent downloads (chunked via `put_blob_stream`, `crates/hush-core/src/vault.rs`) | Unbounded; this is the only part that gets big |

So "not enough space for the vault" is really "not enough space for the blob store."
Any design is free to put the core on the phone and only has to answer for `blobs/`.

## 2. Where the code is today (trust this over any plan doc)

Multi-device was built 2026-10-07 (per-device keys, signed device lists at `PUT /d/{account_id}`,
per-device queues, fan-out — verified end-to-end) and **deliberately removed 2026-10-08**.
Current truth, per [`../PROTOCOL.md`](../PROTOCOL.md) and the tree:

- **One device per account.** `DEVICE_ID` is the constant `1`; `device.enc` remains on disk with a
  `device_id` field for format compatibility only.
- No `device_list.rs`, no `/d/{account_id}` route (the relay only carries a
  `DROP TABLE device_dirs` migration), no `link-device` endpoints.
- The 2026-10-06 multi-device plan doc is retained as a record, including recorded deviations and
  the phases that never landed (daemon/CLI link endpoints, revocation UI, docs close-out).

Known-removed-design limitations, if any of it is re-landed: device 1 is the signing root and must
be online and unlocked to link or revoke; a revoked device keeps the history it already has; a
peer may encrypt to a revoked device for up to the ~5 min list TTL.

> **Doc debt found while writing this:** `docs/plans/2026-10-04-v1-milestones.md` Phase C still
> says "multi-device 1–4 of 7 done" — stale text that survived the removal. Fix when this decision
> lands (or sooner).

## 3. The options

### A. Full link — re-land multi-device, carry everything

The phone is a real second device: own identity keys, own relay queue, sessions per device; the
link transfers history **and** the full blob store (the removed design's Q4, minus `device.enc`).

- **Space:** fails the original requirement. Torrent downloads and a lifetime of 8 MiB drops make
  the blob store arbitrarily large. Pruning it on the phone reintroduces B's machinery anyway.
- **Crypto:** nothing new; the removed design already proved the link path end-to-end.
- **Trust boundary:** unchanged — the relay keeps seeing only opaque blobs and timing.
- **Desktop dependence:** none.
- **Verdict:** right model for identity and delivery, wrong answer to the space question by itself.

### B. Link minus blobs — phone is a real device with a partial vault

Same as A for keys and message delivery, but the link transfer carries **history without
`blobs/`**, and blobs are fetched on demand with a hard cache cap on the phone (LRU, order of
hundreds of MB — TBD).

- **Crypto-consistent:** blobs are wrapped under the vault master key, which the removed design's
  link already transferred (wrapped to the new device's passphrase). Fetching ciphertext on demand
  changes storage, not trust. No re-encryption proxy, no content-addressing break.
- **Delivery:** fan-out already lands a copy on every device's own queue, so live messages arrive
  locally on the phone. The relay queue has a TTL, so a phone offline longer than the TTL catches
  up from the **desktop** (the de facto history replica) rather than by relay replay — the catch-up
  source for old messages and old blobs is the desktop, or the optional ciphertext-only cloud
  replica when the desktop is off.
- **Trust boundary:** one new decision (see §4): how the phone fetches old ciphertext blobs. Either
  path serves ciphertext only.
- **Offline:** chat text is fully local-first; only attachments need network.
- **Desktop lost:** the phone holds the full history and keys and can succeed it (re-link a new
  desktop). Same recovery story as A.
- **Phone lost:** revoke the device — the removed design's Phase 6 machinery.
- **Verdict:** answers the space question without giving up local-first or changing the trust
  boundary. Recommended.

### C. Thin client — vault stays on the desktop, phone is a remote view

The phone holds no vault and no keys; it drives the desktop daemon's API over the network.

- **Space:** minimal on the phone — but that is its only win.
- **Trust boundary:** the worst of the three. The daemon is loopback-socket-only by design
  (`0600` Unix socket is *the* access control; the Windows release plan keeps even loopback TCP
  token-gated and disables the wallet on TCP precisely because a network-reachable API is
  second-class). Serving the daemon beyond the machine is a trust-boundary change the project
  gates explicitly.
- **Crypto:** decrypted-by-the-desktop content crosses the phone link unless a new E2E channel is
  built on top — strictly worse than A/B, where plaintext never leaves a device.
- **Local-first:** none. Desktop must be on and unlocked whenever the phone is used.
- **Desktop lost:** the phone is a brick — the worst possible failure mode for a tool whose threat
  model includes device loss.
- **Postal bridge conflict:** §5 of the postal bridge plan requires the Android scanner to decrypt
  QR payloads in memory with keys in the Android Keystore. A thin client has no local keys and
  would round-trip the desktop per scan.
- **Verdict:** rejected for the vault question. A narrow "remote viewer" mode could exist one day
  as a separate, explicitly-scoped feature; it is not the storage answer.

### D. Child accounts — each device is its own place (proposed 2026-10-18)

Not a second *device* on Sam's account — a second *place*. The phone is its own account: own
identity keys, own queue, own vault. Peers see **Sam Desktop and Sam Phone as two contacts**.
Settings gains "add a child place": the phone shows its invite, the desktop scans it, the child
relationship is recorded locally. No shared master key, no device list, no fan-out — the existing
contact/invite/queue machinery carries all of it.

- **Build size.** The relay sees a normal account; the "child" is a Settings flow plus a local
  label. Nothing the 2026-10-08 removal took out comes back, and no §4 machinery is needed.
- **Space.** The phone vault holds only what the phone account received — small by construction.
  No blob-fetch transport, no cache cap, no desktop catch-up source.
- **Security.** Blast radius shrinks. B shares the vault master key with the phone; D gives each
  place its own, so a compromised phone reads only the phone's mail. Revocation is "remove
  contact", which already exists, and no signing root needs to be online to do it.
- **Relay traffic.** No fan-out copies (B sends every message to every device of the recipient);
  one queue per place.

The three real costs:

1. **Peer adoption.** Every contact must learn the child to message it. In person: scan the
   phone's QR — the existing flow, zero new machinery. Remotely: the parent introduces the child —
   the one genuinely new mechanism, a signed "this is my child, here is its invite" message over
   the existing contact channel; the peer accepts and the child becomes a contact. Without the
   signature, "I'm Sam's child" is unverifiable.
2. **Conversation continuity.** A thread with Bob lives on one place. The desktop thread and the
   phone thread never merge; senders pick a place, and Sam reads a thread where it lives.
   Compartmentalization is the point — but it is a visible product change.
3. **Group spaces.** Conclave membership is per identity; the phone is not in a conclave the
   desktop joined. The admin re-invites the child, or the introduction message covers membership
   too (open question).

Secondary consequences: each place backs up itself (a lost phone loses phone-only history; the
recovery code story already exists per device); peers see two "Sam" contacts and nicknames
distinguish them; §5's feature table re-reads under D as "the phone is a normal account —
everything text-shaped it *receives* is fully local, there is no fetching the desktop's blobs
(no shared key), torrents stay desktop-only by policy."

### D — Settled (grill Round 1, 2026-10-19)

| # | Decision |
|---|---|
| D1 | Vouch shape: **parent-signed claim carried beside the child's invite** — anyone who already trusts the parent can verify the place. Amended 2026-10-20: the vouch is **two signatures**, the parent's and the child's counter-signature, and only a chain where every hop has both earns a badge. Carried as a sidecar on `PlaceAdopt` / `PlaceAdoptAck` / `PlaceIntroduce`, not baked into the invite body. Parent must be unlocked once to mint. |
| D2 | Continuity: **two places = two contacts = two threads.** No merge, no mirror. |
| D3 | Conclaves: **admin re-invites**; double membership accepted as rare (user is on one place at a time). |
| D4 | Backup/loss: **each place stands alone.** No parent backup of the child's vault. |
| D5 | UI name: **Places** (Settings → My places). "Child" stays implementation jargon only. |
| D6 | On adoption, parent and place **auto-add each other as contacts**, and the child **counter-signs the hop naming it** and returns it; until that lands the parent's record reads "waiting for them to confirm" and cannot be introduced. |
| D7 | **Any Place can adopt another** — no special root; places are equals. |
| D8 | Existing contacts learn a new Place via **opt-in introduce** (pick who gets the signed vouch/invite). |
| D9 | Peer contact list: **flat + badge** — two ordinary contacts; a "place of …" chip when the vouch verifies. |
| D10 | Vouch may carry a **short place label** ("phone", "laptop") as a nickname *suggestion* only. |
| D11 | **Chain walking** — because Places are equals (D7), a Place-of-a-Place still badges as belonging to someone Bob already trusts if the vouch chain reaches that person. One-hop-only would contradict equals. |
| D12 | Detach = **remove from My places** only. Histories stay; peers keep contacts until they remove them. No live vouch revocation in v1. |
| D13 | Vouch chain **cap = 2** hops (e.g. desktop → phone → tablet). Longer chains verify only as ordinary contacts. |
| D14 | Badges **re-verify on a newer introduce** from a Place Bob already trusts: the introduce replaces that introducer's place-set for Bob. Sticky until then. Re-scanning a place's own QR still shows whatever vouch that invite currently carries (detach on the parent does not rewrite the child's invite). No live revocation channel. |
| D15 | Adoption direction: **existing Place scans the new vault's invite** (Settings → Add place). The scanner is the minting Place and signs the vouch. |
| D16 | Introduce payload: **child invite + full vouch chain** in one accept. |
| D17 | My places lists **both directions** — places I minted and the Place that minted me. |

Signing key for the vouch: the minting Place's existing identity key (the same key peers already
pin from its invite). No new key material. A vouch chain is a list of such signatures back to a
Place the verifier already trusts, capped at D13.

## 4. Open questions if B is chosen

1. **Blob fetch transport.** (a) Relay rendezvous: desktop PUTs a missed blob to the relay (or a
   short-lived per-blob grant), phone GETs it — relay stays a dumb ciphertext store, existing
   routes, nothing new listening on the desktop. (b) Direct phone→desktop over Tor with a
   device-scoped token — fewer moving parts, but a new listener on the desktop to document in
   `THREAT_MODEL.md`.
2. **Which blob sets a phone syncs on demand vs never.** Mail attachments, drop attachments and
   zine media are plausibly on-demand; torrent downloads should stay desktop-only. Each store that
   references blobs (chat, mail, drops, zines, library) needs its policy named.
3. **Cache cap and eviction order** (LRU vs keep-pinned), and where the cap is surfaced honestly in
   the UI.
4. **Catch-up path for text history** after a long offline period — pull from the desktop's
   `chat.enc` scope vs relay replay window.
5. **Re-landing scope.** Re-land the removed Phases 1–4 as they were (deviations are recorded in
   the 2026-10-06 plan), then daemon/CLI link endpoints (the never-finished Phase 5 tail), before
   any Android work. The Android shell itself stays "same Tauri project" per `WORKSPACE.md`.

## 5. Feature availability on a linked phone under B

What the Android app can do the moment it is a linked device with a blob-free vault.
Storage note: torrent *metadata* is sidecar state (it syncs), but torrent *files* are vault
blobs (`crates/hush-daemon/src/torrents.rs` — the pipeline writes files into the vault).

**Full, local, offline-capable** (vault core only):

- 1:1 chat — full text history, send/receive, ratchets, live presence (5h)
- Conclaves — text, membership, fan-out, room management
- Internal mail (5d) — threads, compose, flags; text local
- Trustee reach (24) — the mail/chat/conclave text paths
- Notes dashboard (16.1/16.5) — text sticky papers
- Invites/trust setup — QR scan; the camera stack postal bridge §5 specs for the scanner serves here too
- Drops — **sending** is fully local (encrypt + upload); receiving gets the body locally, attachment on tap
- Articles / public lane (11/22) — publish text posts, read followed authors from the offline cache (egress is already lazy)
- Reads — RSS is network-bound anyway; its cache is already capped
- Settings/security — passphrase change, device list/revoke, auto-lock

**Works, with the content fetched on tap** (blob-backed; needs desktop or cloud replica reachable):

- File exchange (5b) — refs and metadata local; contents on open/save
- Mail and drop attachments; any inline media in chat
- Recorded audio / voice media (Opus), zine media when zines land

**Desktop-only under B:**

- Torrent Library files — the listing can sync; the files are desktop blobs. Watching a single
  video on the phone would mean pulling it through the cache cap on tap — a policy question, not
  free
- Wallet (Task 8) — unbuilt, and unix-socket-gated today; undecided for any phone

**Android platform realities, independent of A/B/C:**

- No push service (privacy posture) — background delivery is pull; notifications need a
  foreground service or are best-effort while the app runs
- Keys wrapped by Android Keystore (StrongBox where available), biometric/PIN gate, `FLAG_SECURE`
  on plaintext screens — already specified in postal bridge §5; the linked-device model extends it
  from scanner keys to the whole vault
- Tauri 2 Android shell — same UI codebase per `WORKSPACE.md`

**B-specific UX consequences to state honestly in the UI:**

- An attachment may be "not on this device" with no desktop/replica reachable — show that state,
  never a spinner forever
- Linking a new phone needs the desktop online **and unlocked** (device 1 is the signing root)
- A backup made on the phone has no blobs under the default exclusion — decide whether a
  "light backup" is a feature or a warning

## 6. Recommendation

**Commit to D (Places).** Grill Rounds 1–3 settled the tree (D1–D17). A and C remain out. B is
shelved: the storage problem is answered by construction under D, without re-landing multi-device
or a blob-fetch path.

**Next (only after user confirms shared understanding):** a junior-proof slice plan —
invite/vouch format version bump + tests → Settings → My places (adopt by scan, both-directions
list, detach) → opt-in introduce message → peer flat+badge UI. Android shell stays a later slice;
Places is useful on desktop↔desktop too.

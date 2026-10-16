# Protocol sketch (v1)

Not a crypto paper. We use **Signal protocol (`libsignal`)** for 1:1 sessions. This document is the *transport and identity* around that. The blind-queue details need expert review before we freeze wire bytes.

## Roles

- **Vault:** one person’s encrypted data on their device. Source of truth.
- **Daemon:** owns keys and vault; UI is a local client.
- **Relay:** dumb store-and-forward of ciphertext blobs, keyed by **queue IDs**, not people — plus one
  generic broadcast lane, the **bulletin** (Zines, Task **21**), which is keyed by nobody.
- **Cloud replica (optional):** WebDAV or S3-compatible; ciphertext only.

## Identity

- Each person has a long-term identity key pair (Signal identity).
- Friends do not look you up by name on a server.
- An **invite** (QR or link) carries, at least:
  - identity public key
  - a receive **queue id** + **queue authorization secret** (so only the invited person can put blobs on that queue)
  - an optional **mail handle** (local part only, e.g. `sandy`) and **vault name** (e.g. `quiet-harbor`) — the label and suffix of this member's mail address, carried inside the invite
  - app/version so old clients fail clearly
- **Invite encoding (v3 pointer, 2026-10-16).** The shared invite is a short
  address-shaped code — `hush1…`, ~175 characters — that **names** the account
  instead of carrying the bundle inline. bech32 (BIP-173 charset and checksum,
  hand-rolled because the standard 90-character address cap does not fit a
  ~100-byte payload) encodes: a version byte, a presence-flags byte, the
  33-byte identity key (the code's "address" is its hex — the account id),
  the 16-byte inbox queue id, the 32-byte delivery token, and optionally the
  relay (omitted when it is the build's default relay), mail handle and vault
  name. The peer accepting the code fetches the signed prekey record from the
  relay's directory (`GET /k/{account_id}`) and verifies it against the
  identity key the code pinned, so the relay can refuse to serve but cannot
  forge or swap keys; the out-of-band channel that carried the code still
  pins the identity. The delivery token travels in the code because it must
  not be learnable from the name alone, or anyone who guessed a name could
  write to that inbox. Publishing happens at vault creation and on every
  unlock (64 one-time prekeys per publish, which also tops the pool back up);
  an invite is therefore reusable — every holder can start a session, and the
  directory hands each fetcher a different one-time prekey. Legacy invites
  are still accepted by `add_contact`: a v2 string is plain JSON with
  URL-safe base64 byte fields (~2,950 characters; v1 wrapped byte-array JSON
  in base64 at ~9,200), and intro cards still carry a full v2 payload inline
  because they ride an already-encrypted session and must not depend on the
  directory at accept time. A string starting with `hush1` decodes as a
  pointer first and falls back to the legacy readers.
- Display names are local only.
- The mail address is **not** a relay directory entry. `handle@vault-name` (`sandy@quiet-harbor`) is a human label exchanged out-of-band inside the invite; the relay never learns or resolves it, and there is no "look up a person by address" API. The suffix is the vault's own name, chosen when the vault is created (default `hush`); it is deliberately not a domain, because mail is members-only and nothing resolves it.

**One device per account.** An account is a single identity with a single device. There is no device
list, no per-device fan-out, and no way to add a second device: `DEVICE_ID` is the constant `1`
everywhere a libsignal address needs one, and the on-disk device file keeps a `device_id` field only
so files written under the old model still read back. This was removed deliberately on 2026-10-08 —
the multi-device machinery (signed device lists published at `PUT /d/{account_id}`, per-device
queues, cross-device `Revoked` reconciliation, and the `POST /api/link-device/*` transfer) no longer
exists, and neither do the endpoints.

An account's identity key is its **account id**: `account_id = hex(identity_public_key)`. That is what
a peer addresses it by and what the relay checks a prekey record against.

## Queues

Each **account** owns one **inbox** and mints one **per-contact queue** for every contact it
receives from. A contact's messages are delivered into the queue minted for them, so a queue is the
unit of revocation: deleting a contact burns that contact's queue and touches no one else's.

- **Inbox:** the account's own queue, named in its invite. It carries the account's queue grants and
  — for a contact that has not yet received a per-contact queue — that contact's messages.
- **Per-contact queue:** derived, not stored. The id is `HMAC(read_token,
  "hush-contact-queue-id-v1" || contact_account_id)` and the read token is `HMAC(read_token,
  "hush-contact-queue-read-v1" || contact_account_id)`. The account recomputes both from its own read
  token, so it stores no per-contact secret and neither value ever leaves the device.
- **Grant channel:** the inbox is where a queue **grant** is sent. A grant is
  `MessagePayload::QueueGrant { queue_id, delivery_token, signature }`, carried inside the same
  libsignal session as any other payload. The signature is over the canonical grant bytes
  (`hush-queue-grant-v1\n{queue_id}\n{delivery_token}`) by the granting account's identity key, and
  the recipient verifies it against the identity key the sender **pinned in their invite** — the same
  key that hashes to the sender's account id, and the key that established the relationship in the
  first place. The delivery token is public by design (it authorizes putting a blob in the queue and
  nothing else), so the signature — not a blocklist, and not a device list — is what makes a grant
  trustworthy on a channel anyone can write to.
- **Delivery:** a send is one ciphertext to one queue: the recipient's per-contact queue for this
  account when a grant has arrived, and its inbox until then.
- **Readiness is derived, never stored (2026-10-15).** A contact is *inbound-ready* when it holds a
  delivery queue **or** something has been heard from it — a filed chat, file or mail — which is the
  fallback for a grant that was dropped in transit. The UI says "waiting for them to add you" on that
  derivation, so nothing has to keep a flag in sync with the grant: the vault's own `heard_from` bit
  and the presence of `delivery_queue_id` are the whole truth, and a vault that predates the rename
  reads back through a serde alias.
- **Revocation:** deleting a contact burns the per-contact queue for them and records the account id
  in a local blocklist. The blocklist is not what makes the revocation happen — the burn is — but it
  still drops a revoked contact's stragglers from the shared inbox (the only shape it can attribute
  is the legacy JSON envelope, which names its sender) and re-drives a burn that failed while the
  relay was down. Re-adding the contact clears the entry.
- **Read vs delivery:** the **read** token (never published) authorizes listing, reading, deleting and
  burning this account's queues. The **delivery** token (published in the invite, derived from the
  read token) authorizes only putting a blob in the queue.
- Sender uploads: `PUT /q/{queue_id}` with **opaque ciphertext bytes** (Signal message only; no
  cleartext sender id in the relay blob) + proof they know the slot secret (exact auth: spec during
  implementation; must not require a global user id).
- Recipient lists/drains their queues; then deletes blobs.
- TTL: **7 days**, then the relay drops the blob.
- Max blob: **1 MiB** for v1 messages; larger files: encrypt in the vault, send a pointer + chunks as multiple blobs or a follow-up (keep v1 files small if needed rather than inventing a CDN).
- Rotate queue IDs inside the encrypted session so the relay cannot staple one ID to a person forever.
- Block = burn the contact's per-contact queue + drop local session + stop fetching them.

Relay **must not** offer “list all queues for user X.” There is no user X on the relay.

### Prekeys

`PUT /k/{account_id}` publishes this account's prekey record so a peer can open a session without an
invite; `GET /k/{account_id}` takes it, consuming one one-time prekey. There is no bearer credential:
the record carries the account's identity key and signs its signed prekey and kyber prekey with the
matching private key, and the relay checks both against the identity key that `account_id` decodes
to (`account_id` *is* the hex of that key). Knowing a public account id therefore does not let anyone
replace an account's prekeys. The route is per account rather than per `(account, device)` because
there is only one device.

This is not a multi-device feature: it is how a **pointer invite** (v3) works —
the code names the account, and the directory record is the bundle. Vault
creation and every unlock publish a fresh record (the publish also tops the
one-time prekey pool up), so a peer that accepts a `hush1…` code always finds
keys to fetch. A peer holding only an account id — no invite — can still fetch
the same record, which is why the route is public.

### Migration limit (2026-10-08)

Contacts created before per-contact queues existed keep delivering to the account's shared-inbox
queue, and that queue is **not** burned — burning it would cut off every pre-existing contact at
once. Such a contact therefore keeps working and is **not revocable** until it receives a per-contact
queue, which it acquires the next time this account sends to it and it drains its inbox and applies
the grant. Until then, deleting the contact removes it locally and drops its messages on drain, but
its device still has a working queue to write to. New contacts are revocable from the moment the
grant lands.

Removing the device list also removed the only way a peer learned a *changed* inbox, so a rotated
inbox (a vault migrating off the pre-delivery/read-split or pre-multi-device layout) now requires a
fresh invite: the cached one is dropped on open and the next `invite()` mints one naming the new
queue.

## Messages

- Payload inside Signal: text, or a file reference (content hash + key + size) for an attachment already in the sender’s notion of “encrypt to this recipient.”
- One session, one queue put. No group object.

### Mail

Mail is a payload **kind** on the same Signal session and the same queue — not a second transport. It is the internal member-mail feature (`handle@vault-name`, members only): an email-like UX (threads, subjects, multiple attachments, address book) without SMTP or federation.

- **Wire payload:** `MessagePayload::Mail { thread_id, subject, body, attachments[] }`. Attachments are inlined on the wire (capped at 8 files / 256 KiB total for v1) rather than referenced out-of-band.
- **Async by design:** chat and files require the recipient to be **online** (live presence); mail deliberately skips that check, so it is queued on the recipient's relay slot while they are away and delivered on their next drain. This is the one behavioural difference from chat, and the reason mail exists.
- **Local storage:** mail records reuse the encrypted `chat.enc` log via a `ChatRecordPayload::Mail` variant; attachment bytes live as encrypted vault blobs (`put_blob`) referenced by id. One log, one encryption path, one backup path.
- **Threads:** `thread_id` groups a conversation; a reply reuses the thread id and keeps its own copy, so both sides hold the full thread.
- **No directory:** the handle is an invite-carried label (see Identity). There is no handle search, no relay-side address book, and no receiving from non-members in v1.

Relay view of a mail message is unchanged from any other message: an opaque ciphertext blob. Subject, body, and attachment names are inside the Signal envelope and never appear in relay storage.

### Conclaves

A conclave is **not** a relay-side room and there is no group object in the protocol. Group chat is
sender-side fan-out: the sender encrypts the message once per joined trustee over that trustee's
existing 1:1 session and puts N blobs (N puts for N joined trustees) — exactly the same primitive as
two-party chat, just repeated. Nothing on the relay changes.

- **Roles.** The **convener** is the creator: the sole authority for inviting a trustee, kicking one,
  and ending the conclave. A **trustee** is anyone else in it. Any joined trustee may leave; the
  convener leaving ends the conclave instead.
- **Membership is consensual.** Naming someone sends an **invite**; they join only by accepting. A
  message never materialises a conclave on a vault that has never seen it — only an accepted invite
  does. An invitee who has not accepted is not a fan-out target.
- **Trustee states:** `invited` (asked, not answered), `joined` (receives the fan-out), and the
  terminal `left` / `kicked`. State transitions are monotonic: a terminal state is never revived, and a
  joined trustee is never downgraded back to invited by a stale peer snapshot.
- **Single authority.** The convener broadcasts the authoritative roster as a descriptor
  (`conclave_id`, `name`, `convener`, `trustees[]`, `ended`); peers apply it only when the sender is the
  convener named in it. This replaces the older per-message member list, which drifted.
- **Wire payloads:** `MessagePayload::Conclave { conclave_id, name, trustees[], body, file? }` for a
  message, plus the control variants `ConclaveInvite`, `ConclaveAccept`, `ConclaveDecline`, `ConclaveLeave`,
  `ConclaveUpdate`, and `ConclaveEnd`, each carrying the descriptor. Control payloads change local state
  and are never surfaced as chat messages. The message variant and the id field were renamed from
  `Meeting` / `meeting_id`; `#[serde(alias = "meeting")]` and `#[serde(alias = "meeting_id")]` keep a
  message written under the old names decodable.
- **Delivery:** store-and-forward, like mail (the fan-out uses the same `require_online: false` path), so
  an away trustee still receives the message on their next drain. There is no delivery receipt and no
  server-side membership to read.
- **File sharing:** a file uses the same sender-side fan-out as text — one encrypted copy per joined
  trustee, N blobs for N trustees. There is no shared blob to point everyone at, so N copies is the
  honest implementation. The bytes ride as an optional `file { name, data }` on the same
  `MessagePayload::Conclave` (inlined, bounded by `MAX_INLINE_FILE_BYTES`); each recipient stores them
  as its own local encrypted blob and records a `ConclaveFileRecord` (`name`, `blob_id_hex`, `size`) in
  its conclave log. The fan-out targets the joined set only, matching text, and a trustee who joins
  after the file was sent does not receive it: there is no shared history to backfill from, so the log
  is not complete for a late joiner.
- **Local storage:** conclaves reuse the encrypted `chat.enc` log via a `ConclaveRecord` (`id`, `name`,
  `convener`, `trustees[]`, `created_ms`, `messages: ConclaveMessage[]`, `ended`). A conclave's messages
  are kept in the conclave's own log, not in the 1:1 `ChatRecord` projection. One log, one encryption
  path, one backup path. A pre-rename record stored `members` as bare nickname strings and no convener;
  `#[serde(alias = "members")]` plus a load-time hydration step (resolve the nicknames to account ids,
  resolve the local account's own entry — or add it when the legacy list omitted it, since a vault is
  never its own contact — and make the local account the convener) keep an existing vault working. The on-disk key for the list
  stays `meetings` (`#[serde(rename = "meetings", alias = "conclaves")]`).
- **Ending is shared.** The convener broadcasts a `ConclaveEnd` descriptor; every peer marks the record
  `ended` and renders it read-only. The record and its message log stay in every vault.
- **Deleting is local.** An ended conclave can be deleted — record and message log together — from one
  vault only, and nothing is broadcast: the other trustees keep their own copies. The vault blobs behind
  the files shared in the conclave are left in place, the same no-collection-pass trade
  `delete_mail` makes; there is no shared history, only per-vault ones to delete.
- **Not a directory:** `trustees[].nick` is the sender's local nickname; `trustees[].account_id` is the
  stable identity. The relay never sees either.

Relay view of a conclave message is unchanged from any other message: an opaque ciphertext blob. The
conclave name, trustee list, and body are inside the Signal envelope and never appear in relay storage.

### Trustee reach — mail only or full (2026-10-15)

Which surfaces a relationship may use is a property of the **circle relationship**, not of any one
message: a trustee is either reachable by **mail alone** or by **everything** (mail, 1:1 chat, and
conclaves). Mail is the floor and is never gated.

- **Wire payload:** `MessagePayload::Reach { scope }` where `scope` is `"mail_only"` or `"full"`.
  Sent when a contact is added with a narrowed reach and whenever the choice changes; store-and-forward,
  so an away peer learns it on their next drain. It is a **control** payload — never rendered as an
  inbox item — and is applied only from a known contact, because the 1:1 session is the authentication.
  Forged, it can only *narrow*, so the worst case is hiding a surface the sender was not going to use.
- **Both sides hold a choice.** Each contact record carries `reach` (this vault's own choice) and
  `peer_reach` (the peer's announced one, `full` until they say otherwise). The **effective** reach is
  `reach.narrow(peer_reach)`: a surface works only when both sides allow it, so `mail_only` on either
  side makes the relationship mail-only. Neither side can force a surface the other excluded.
- **Enforced at the boundary, not just hidden in the UI.** A `mail_only` relationship refuses
  `send_text` / `send_file` (`Error::ReachMailOnly`), refuses `create_conclave` and
  `invite_conclave_trustee` for that trustee, drops an inbound chat or file at `recv` with a warning,
  and refuses an inbound conclave invite before it becomes a pending invite.
- **No transport change.** Mail, chat and conclaves already ride the same 1:1 Signal session, so this is
  a **policy** gate on deliverable bytes: the app chooses not to offer or accept the surface. The relay
  sees nothing new.
- **Existing conclave membership is grandfathered.** Narrowing to mail-only stops *new* chats and new
  conclave joins; it does not retroactively remove a trustee from a conclave they are already in — a
  kick is the removal path. Documented rather than silent, because a fan-out that silently dropped a
  member would be worse.

Design and decisions: [`tasks/trustee-reach.md`](tasks/trustee-reach.md) (Task **24**).

### Paper

One shared, live, encrypted collaborative document per conclave — the "Paper" — owned by the daemon
and edited through the UI's ProseMirror/Yjs bridge. It is **not** a relay-side object and adds no
relay ability: the document is a `yrs` CRDT whose updates ride the same per-trustee fan-out as
conclave messages, and nothing on the relay learns the paper exists.

- **State.** The merged CRDT is the daemon's (`papers.enc` in the vault, the `notes_store` pattern:
  a snapshot plus a log of opaque `yrs` updates, compacted when the log grows). The webview holds a
  working copy and exchanges binary updates over `/api/paper/{conclave_id}/*`; the daemon is the
  source of truth, so a restart rebuilds the editor from the daemon's state.
- **Wire payloads.** `MessagePayload::PaperState { descriptor }` (creator-authoritative creation),
  `PaperDelta { paper_id, update }`, `PaperSnapshot { descriptor, state }` (late-joiner catch-up),
  `PaperHub { descriptor }` (convener-authoritative succession), `PaperPresence { .. }`, and the
  DataChannel signalling pair `PaperOffer` / `PaperAnswer`. Binary fields use the house `b64_bytes`
  codec. An unknown payload tag decodes to a tolerated `Unknown`, never `Error::Corrupt`.
- **Authority mirrors the conclave's.** A `PaperDelta` carries no authority — a CRDT merges an update
  from anyone, in any order. A paper this vault has never seen is materialised only from a
  `PaperState` whose sender is the creator the descriptor names. `PaperHub` is accepted only from the
  conclave's **convener**, so a trustee cannot claim the rendezvous. Writing is refused (`403`) when
  the conclave has **ended** or the writer is not a joined trustee.
- **Delivery.** The default path is the relay fan-out: one encrypted copy per joined trustee over
  that trustee's existing 1:1 session, store-and-forward, exactly like a conclave message. Deltas are
  never replayed, so a trustee that joins after edits is caught up by a `PaperSnapshot` the **hub**
  sends; only the hub sends one, so N trustees cannot each send their own.
- **Fast path (DataChannel).** The paper's **hub** (its creator, until succession moves it) is a
  rendezvous for a pure-Rust WebRTC `DataChannel` carrying `yrs` updates and ephemeral presence.
  Signalling rides the relay as `PaperOffer`/`PaperAnswer` (the gathered ICE candidates are inside
  them — one round trip, no third variant). The channel is a **star** through the hub, so the hub
  re-broadcasts an update it receives over one channel to its other channels; the relay is still
  fanning out underneath, so the channel is a latency optimization, never the guarantee. When it is
  unusable the daemon sends over the relay and the UI badge reads `relay` instead of `live`.
- **STUN, no TURN.** `ClientConfig::stun_url` is the one place a STUN server is configured and it has
  **no default** — a public one would learn the address of every peer that opens a paper. There is no
  TURN, so symmetric NAT degrades to the relay rather than being traversed.
- **Presence.** Cursors, colour, and display name are `y-protocols` awareness frames carried over the
  DataChannel **only** — there is no relay copy and a 30-second TTL in daemon memory, so presence is
  ephemeral and a peer that drops off simply disappears.
- **Succession.** The descriptor names the hub. The convener can move it explicitly
  (`POST /api/paper/{conclave_id}/hub/rehome`); when a hub **leaves or is kicked**, the convener
  inherits it automatically, with no user action, and the surviving trustees re-dial the new
  rendezvous. A kicked trustee keeps its readable copy and its history but is refused writes once it
  is told it was kicked.
- **No plaintext.** The document, its title, and the presence frames never appear in relay storage,
  and `papers.enc` is ciphertext at rest; the dogfood scans every file the run leaves behind for the
  edit strings and finds none.

Relay view of a paper delta is unchanged from any other message: an opaque ciphertext blob, stored on
a queue and dropped after its TTL. The relay cannot tell a paper update from a chat message, and
there is no paper object, membership, or history on it.

### Zines (2026-10-12 design, built 2026-10-13)

Members-only long-form publishing. A **pen name** holds any number of **posts** — each its own
article with its own read key and revision chain — readable by **all members**, not only contacts. It
is the **one place the relay gains a new ability**: a **bulletin**, a generic opaque append-only
broadcast lane. Everything else reuses existing primitives. Full design:
[`docs/tasks/zines.md`](tasks/zines.md) (Task **21**).

- **Bulletin.** `PUT /b/{queue_id}` appends an opaque blob to a flat log; `GET /b/{queue_id}?since={seq}`
  drains it. Both are authorized with the **read** token of a queue the member owns — the same
  credential a drain needs — so the publish path is the one every member already uses and no
  *delivery* slot is spent on it (and the delivery token, which travels in invites, cannot publish at
  all). The queue id in the path is **derived** from the vault's identity rather than being the inbox
  id: the relay self-binds an unclaimed queue to whichever credential reaches it first, and an inbox
  id plus its delivery token both travel in an invite, so publishing under the inbox id would let a
  contact claim the member's bulletin credential on their behalf. The stored row is
  `(seq, hash, body, inserted_at)` and carries **no
  queue id**, so the log is not an author index; every member drains the same lane, so a drain
  reveals no readership. Per-blob cap 256 KiB, TTL 30 days, and a per-member rate limit on publish
  (drains are not rate-limited: a read is not what fills the lane). Content-addressed: a **re-post
  keeps its original `seq`** and only moves `inserted_at`, so refreshing the window does not hand
  every drainer a second copy. It is **per relay** — relays never talk to each other (see *Many
  relays* below), so cross-relay reach is holders re-posting to their own relay's bulletin, with no
  federation and no relay-to-relay path.
- **Delivery is broadcast; readability is epidemic.** There is no all-members key and no member list
  to encrypt to, so the bulletin carries **ciphertext**. An article is sealed under the post's
  symmetric **read key**; the author hands that key to a contact as `MessagePayload::ZineGrant` over
  their existing 1:1 session, and **any holder** may re-share the same key to *their* contacts, so
  reach grows one session at a time from whoever can already read. A member may therefore hold an
  **unreadable** blob until a key reaches them; those blobs are held, never dropped, and a retry
  sweep runs on every poll so a key that arrives after its blob still opens it (and a blob that
  arrives after its key is opened on the drain). This is the price of members-only E2EE on a
  broadcast lane.
- **Wire payload.** One new variant, `ZineGrant { zine_id, key, latest_hash }` — read keys are the
  only zine thing that rides the sessions; articles and comments ride the bulletin. Receiving it is
  authority-free (any holder may re-share); it grants read, never write. `latest_hash` is the address
  of the newest blob the sender held, which lets a recipient match it by hashing bytes rather than
  opening everything. `Unknown` (shared with Paper) keeps a zine-unaware build interoperable.
- **Article blob.** `ZineBlob { v, sign_pk, signature, descriptor }`, sealed under the read key, so
  the relay sees only ciphertext. Authorship is the pen name's **signature** over the descriptor's
  canonical bytes (verifiable *without* the read key, which is what lets a holder check provenance);
  integrity is **AEAD**. The descriptor carries `zine_id` (minted per post, so it is an address and
  not an identity), `pen_id` (the pen name's seal public key, stable across everything that name
  writes), `pen_name` (the byline), title, **sections (tags)**, author-declared `created_ms` (clamped
  on ingest), `revision`, `prev_hash` (an edit chain), body, and the merged comment thread. Sections
  live inside the encrypted descriptor, so the relay never sees a tag and there is **no relay-side
  category registry**.
- **Comments.** `ZineCommentBlob { v, zine_id, author, body, created_ms }`, sealed to the pen name's
  **public** key (an anonymous sealed box) and published to the bulletin, so no session to the author
  is needed to write one. The author drains, trial-decrypts, keeps what opens as a **pending** pile,
  and merges the accepted ones. A merge is a **revision**, because the thread lives in the descriptor,
  so accepting a comment republishes the post and that republish is what makes the comment visible to
  everyone else. A refusal is not a retraction: the comment was never published, and the blob stays on
  the lane (sealed to the author alone) until it ages out. Moderation is entirely author-side.
- **Availability is the holders.** Every holder re-posts what it holds once the relay has not seen it
  for a week (a quarter of the 30-day window), a couple of blobs per poll to stay inside the publish
  rate limit. That is what keeps a zine in the window with its author offline — and there is no other
  path, since relays do not talk.
- **At rest.** `zines.enc` in the vault holds my posts, held posts, the **read keys I hold**, the held
  blob bytes, and the local section index; it is ciphertext when locked. Held bytes are bounded
  (1024 blobs, 32 MiB); going over evicts superseded revisions first and reports the drop in the
  daemon's log rather than letting the corpus shrink silently. Browsing is a local facet — no follows,
  no ranking, no relay knowledge.

Relay view of a bulletin blob is an opaque ciphertext blob with a size and a time. The relay learns
that **some** member published at a time (it must authorize the publish) and nothing else; it cannot
tell an article from a comment, and the pen name, title, sections, and body never appear in its
storage.

### What the UI is allowed to call "end-to-end encrypted" (2026-10-15)

The chat header has said **End-to-end encrypted** since v1, and that mark is now shared
(`apps/hush-tauri/src/components/ui/E2eMark.ts`) by every surface where the same claim holds. It is a
claim about the transport, so the list is derived from the sections above rather than chosen per
screen — a surface may only show it when the relay holds ciphertext it has no key for:

| Surface | Why the claim holds |
|---|---|
| 1:1 chat and its files | The Signal session itself. |
| Mail, with attachments | A payload kind on that same session; subject, body and file names are inside the envelope. |
| Conclaves, with files | Sender-side fan-out: one encrypted copy per joined trustee over that trustee's own 1:1 session. |
| Paper | Relay fan-out over the same sessions; the DataChannel fast path is a direct peer connection. |
| Voice | DTLS-SRTP between the two daemons, with the SDP fingerprints authenticated by the Signal session. No Hush server carries media. |
| Drops | Sealed to the recipient's published drop key before they are parked, so the relay cannot open them or name the sender. |
| Zines | The body is encrypted to a per-post read key, and that key is handed over inside a 1:1 session — never through the bulletin. |

**Surfaces that must not use it.** Reads, Torrents and IRC talk to services outside the members-only
envelope, so their content is not ours to promise anything about. Notes never leave the vault, which
is a stronger property than end-to-end, not the same one. And on the Zines reader the mark belongs to
the member zine branch only: the ATProto document branch is a **public** post, which is precisely the
confusion `renderAtprotoDocument` exists to prevent.

## What the relay HTTP API looks like (shape)

- `PUT /q/{id}` — store blob (authz for that queue)
- `GET /q/{id}` — list or fetch pending blobs (authz for the owner of the queue)
- `DELETE /q/{id}/…` — ack
- `POST /q/{id}/report` — mark slot abusive (rate-limit / burn)
- `PUT /k/{account_id}` — publish this account's prekey record (authz: the record's own signature)
- `GET /k/{account_id}` — take the record, consuming one one-time prekey
- `PUT /drop/{id}`, `GET /drop/{id}` — outsider delivery containers, keyed by a random capability id
- `PUT /b/{queue_id}`, `GET /b/{queue_id}?since={seq}` — the **bulletin** broadcast lane (opaque blobs; publish and drain both authorized by the read token of a queue the member owns; the stored row carries no author, and a re-post keeps its seq)
- No search, no profiles, no public keys directory, and no device directory (`/d/…` is gone)

A drop container is `HSHD`-magic and one of two versions (2026-10-08): **v1** is sealed under a PIN
(PBKDF2-HMAC-SHA256 → XChaCha20-Poly1305), **v2** is sealed to the recipient's X25519 public key (an
ephemeral-key ECDH → HKDF-SHA256 → XChaCha20-Poly1305 sealed box). The relay only ever stores the
opaque container and cannot tell which version it holds or open either.

TLS for transport privacy in transit to the VPS; that does **not** replace E2E.

### Client transport: clearnet or Tor onion (2026-10-09)

The relay is reachable both ways: clearnet (`https`/`http`) and as a v3 onion service
(see `docs/ops/relay-lab-vps.md`). The client's relay switch chooses which path its
traffic takes; the relay sees whichever path a given client used and cannot tell
clients apart beyond that.

- **State:** the vault carries `onion_relay` (the same relay's onion address) and
  `use_tor_relay` (which base is active); the daemon's `ui-prefs.json` carries the
  switch pre-unlock so the login screen can read and flip it while locked, applied
  into the vault at unlock. The desktop shell owns a Tor process (the same one the
  Drops feature uses) and posts its SOCKS port (`socks5h://127.0.0.1:port`) to the
  daemon when the switch turns on.
- **Fail-closed:** with the switch on, every relay request goes to the onion base
  through the SOCKS proxy, and a missing half — no onion address on file, no live
  SOCKS port — is an error, never a silent fall back to clearnet. Dead Tor means
  sends queue in the outbox until it returns or the user flips the switch off.
- **CLI:** `hush-cli init --onion <url> [--tor]` creates a vault born on the onion;
  `hush-cli relay on|off|show` moves the switch without the UI. The CLI carries the
  SOCKS port through `HUSH_RELAY_SOCKS_PROXY`.
- **API:** `GET /api/relay` (readable while locked), `POST /api/relay/mode`
  `{onion, socks}`, `POST /api/relay/onion` `{onion}` (validated v3, staged while
  locked, applied live otherwise). `GET /api/status` adds `relay_tor`.

### Many relays: clients route, relays never talk (2026-10-09, design not built)

Today every vault points at one relay, so a queue id alone is a complete address.
Scaling past one relay does not turn the fleet into a federation. The rule:
**relays never talk to each other; the client is the routing layer.** More
capacity means more home relays, each vault homing on one at creation, and a
cross-relay send is simply the sender's client doing the `PUT` it already knows
how to do, against the recipient's relay.

**Queue references are addresses.** Every reference to a queue — the inbox named
in an invite, a per-contact queue in a grant, a drop container link — is fully
qualified: the relay's clearnet base, its v3 onion base, the queue id, and the
delivery token. Whoever hands out a queue reference has handed over everything
needed to write to it, wherever it lives. Discovery does not change: references
still travel out-of-band or inside sessions, and there is still no directory.
Missing bases on an old invite or grant mean "the reader's own vault relay",
which is exactly today's behavior (`#[serde(default)]` keeps old payloads
decodable).

**The back invite (accept receipt).** Contact setup stays one out-of-band
artifact:

1. Alice's invite carries her identity, prekey bundle, inbox queue reference and
   version. Bob accepts; the session opens from the inline prekeys, so no relay
   is involved and `GET /k/` is still never fetched.
2. Bob's accept receipt is his first message into Alice's inbox: a
   `MessagePayload::QueueGrant` naming the per-contact queue he minted for her
   (derivable at accept time, since her account id is in her invite) plus his
   relay bases. Prose calls it the **back invite**; on the wire it is the grant,
   now fully self-locating.
3. From then on a send is one `PUT /q/{id}` to the recipient's relay and a drain
   is always from the home relay. Blocking burns the per-contact queue on the
   burner's own home relay, exactly as today.

Minting the per-contact queue at accept, instead of after a first grant cycle,
makes a contact revocable from the first message. The inbox remains the grant
channel it always was (pre-acceptance stragglers, contacts grandfathered under
the migration limit above). Identity pinning is unchanged: Alice pins Bob's
identity key from his first message — the X3DH initiator, as in Signal — so the
back invite adds no trust step and carries no identity field of its own.

**What each relay sees.** The recipient's relay sees opaque puts to its user's
queues from a sender it knows nothing else about — not her home relay, not
whether she has a vault anywhere. The sender's relay sees her drains and who
writes to her queues. The social graph is split across relays; no single relay
sees both ends of a conversation. One operator running every relay can still
attempt timing correlation between them, as one relay can within itself today;
Tor narrows it, and splitting relays fragments the graph for anyone who does not
run the whole fleet.

**Rejected: relay-to-relay forwarding.** Forwarding a blob to a foreign queue
needs a routing table — which queue lives on which relay — and that table is a
new server-side graph, valuable at seizure. It also adds server-to-server auth,
dwell time on a second relay, and a protocol to maintain, all to save the client
a `PUT` it already knows how to make. Forwarding would hide a sender's home
relay from the recipient's relay and could later carry mixnet-style padding;
both are purchasable later if community-run relays ever exist. Until then,
relays stay dumb terminators that never egress to each other.

**Ops.** Scale-out is sharding by home relay: one relay per batch of users, and
a relay's load is only its own users' inbound queues. An onion address survives
a hardware move by backing up the service key (see `docs/ops/relay-lab-vps.md`);
re-invites are needed only when a relay is decommissioned, which is the existing
rotated-inbox migration rule. Drop links carry their relay base under the same
rule. The vault's `use_tor_relay` switch stays the user-facing Tor on/off and
applies to every base, since each queue reference carries both.

## Notifications

- Desktop: daemon polls or holds a connection.
- Android later: a push token that is not the identity key and not a stable person id. Details with Android work, not v1 Linux dogfood.

## Recovery (not wire)

- Passphrase wraps keys locally.
- Optional encrypted vault upload to BYO cloud.
- Recovery code unwraps keys if the device is gone.
- These are vault format, not relay protocol.

## The public lane (not relay protocol)

Long-form publishing has two surfaces, and only one of them is ours:

- **Zines** are the members-only commons: a long-form post, its body sealed and broadcast on the relay **bulletin** (Task **21**). The relay still sees only opaque blobs keyed by nobody.
- **The public lane** (Tasks **11** and **22**, [`tasks/atproto.md`](tasks/atproto.md)) is **AT Protocol / Standard.site**, and it does **not** go through the relay. A member connects **their own** DID and PDS with an app password (exchanged for an access/refresh session), and publishing a zine writes a `site.standard.document` record into **their own repo**; the reading list pulls `site.standard.document` records from the publications they follow. Nothing is minted, no PDS is operated by Hush, and there is no relay-side change.

The two are separate objects: the private zine and its public record are never cross-linked, and the pen name that signs a bulletin post is not the publisher. Reads are **pull-only and per-repo**, so no index or AppView accumulates a graph. Egress goes through the **same proxy setting the relay uses** and is **lazy** — only Connect, Refresh and Publish reach the network; opening a cached post makes no request.

## Out of scope for this sketch

- Groups, MLS (zines — Task **21** — is a members-only **broadcast commons**, not a follow-feed: no follows, no ranking, no relay-visible graph)
- The public lane's wire format — that is the Standard.site lexicons, not this sketch; see [`tasks/atproto.md`](tasks/atproto.md)
- Sealed-sender copy of Signal’s exact scheme (we use queues instead; review whether we still need sealed certificates)
- P2P bypass of the relay (nice later; not required for v1)

# Protocol sketch (v1)

Not a crypto paper. We use **Signal protocol (`libsignal`)** for 1:1 sessions. This document is the *transport and identity* around that. The blind-queue details need expert review before we freeze wire bytes.

## Roles

- **Vault:** one person’s encrypted data on their device. Source of truth.
- **Daemon:** owns keys and vault; UI is a local client.
- **Relay:** dumb store-and-forward of ciphertext blobs, keyed by **queue IDs**, not people.
- **Cloud replica (optional):** WebDAV or S3-compatible; ciphertext only.

## Identity

- Each person has a long-term identity key pair (Signal identity).
- Friends do not look you up by name on a server.
- An **invite** (QR or link) carries, at least:
  - identity public key
  - a receive **queue id** + **queue authorization secret** (so only the invited person can put blobs on that queue)
  - an optional **mail handle** (local part only, e.g. `sandy`) and **vault name** (e.g. `quiet-harbor`) — the label and suffix of this member's mail address, carried inside the invite
  - app/version so old clients fail clearly
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

This is not a multi-device feature: an invite already carries a prekey bundle inline, and the relay
record is the same thing for a peer that has an account id but no invite. In the normal flow a
session is established from the invite and the relay record is never fetched.

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
- **Not a directory:** `trustees[].nick` is the sender's local nickname; `trustees[].account_id` is the
  stable identity. The relay never sees either.

Relay view of a conclave message is unchanged from any other message: an opaque ciphertext blob. The
conclave name, trustee list, and body are inside the Signal envelope and never appear in relay storage.

## What the relay HTTP API looks like (shape)

- `PUT /q/{id}` — store blob (authz for that queue)
- `GET /q/{id}` — list or fetch pending blobs (authz for the owner of the queue)
- `DELETE /q/{id}/…` — ack
- `POST /q/{id}/report` — mark slot abusive (rate-limit / burn)
- `PUT /k/{account_id}` — publish this account's prekey record (authz: the record's own signature)
- `GET /k/{account_id}` — take the record, consuming one one-time prekey
- `PUT /drop/{id}`, `GET /drop/{id}` — outsider delivery containers, keyed by a random capability id
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

## Out of scope for this sketch

- Groups, follow-feeds, MLS
- Sealed-sender copy of Signal’s exact scheme (we use queues instead; review whether we still need sealed certificates)
- P2P bypass of the relay (nice later; not required for v1)

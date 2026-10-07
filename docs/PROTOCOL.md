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
  - an optional **mail handle** (local part only, e.g. `sandy`) — a label bound to this identity, carried inside the invite
  - app/version so old clients fail clearly
- Display names are local only.
- The mail handle is **not** a relay directory entry. `handle@MAIL_DOMAIN` (`sandy@myserver.com`) is a human label exchanged out-of-band inside the invite; the relay never learns or resolves it, and there is no "look up a person by address" API.

Device link (old device alive): show a QR from the unlocked old device; new device becomes a second Signal device in that identity. Exact linking follows libsignal’s device model; we do not invent a second identity system.

**v1 implementation (one-time transfer, not yet a live second device):** the unlocked old device exports its encrypted backup once and publishes it under a short-lived, single-use **link id** (`POST /api/link-device/start`). The new device, while **locked**, fetches and imports it (`POST /api/link-device/pull` with the pairing payload's `source_api` + `link_id`) and then unlocks with the same passphrase. The pairing payload is JSON (`kind: "hush-link-device"`, `source_api`, `link_id`, `expires_unix`); QR and clipboard carry the same bytes. TTL is ~10 minutes and the link id is consumed on first read. Treat the link id as a bearer secret: the payload it returns is the passphrase-encrypted vault, and anyone who can reach the source daemon and guess the id could download it (so keep the source on a trusted network, and prefer a LAN address only for the brief window). This is an identity/vault copy, **not** concurrent multi-device: both devices hold the same keys and do not ratchet with each other. Real multi-device (per-device sessions, libsignal device model) is a later slice.

## Queues

- Creating a contact mints (or rotates) a queue the *other* party can send into.
- Sender uploads: `PUT /q/{queue_id}` with **opaque ciphertext bytes** (Signal message only; no cleartext sender id in the relay blob) + proof they know the slot secret (exact auth: spec during implementation; must not require a global user id).
- Recipient lists/drains their queues; then deletes blobs.
- TTL: **7 days**, then the relay drops the blob.
- Max blob: **1 MiB** for v1 messages; larger files: encrypt in the vault, send a pointer + chunks as multiple blobs or a follow-up (keep v1 files small if needed rather than inventing a CDN).
- Rotate queue IDs inside the encrypted session so the relay cannot staple one ID to a person forever.
- Block = drop local session + tell relay to burn that queue + stop fetching it.

Relay **must not** offer “list all queues for user X.” There is no user X on the relay.

## Messages

- Payload inside Signal: text, or a file reference (content hash + key + size) for an attachment already in the sender’s notion of “encrypt to this recipient.”
- Fan-out: N independent sessions, N queue puts. No group object.

### Mail

Mail is a payload **kind** on the same Signal session and the same queue — not a second transport. It is the internal member-mail feature (`handle@MAIL_DOMAIN`, members only): an email-like UX (threads, subjects, multiple attachments, address book) without SMTP or federation.

- **Wire payload:** `MessagePayload::Mail { thread_id, subject, body, attachments[] }`. Attachments are inlined on the wire (capped at 8 files / 256 KiB total for v1) rather than referenced out-of-band.
- **Async by design:** chat and files require the recipient to be **online** (live presence); mail deliberately skips that check, so it is queued on the recipient's relay slot while they are away and delivered on their next drain. This is the one behavioural difference from chat, and the reason mail exists.
- **Local storage:** mail records reuse the encrypted `chat.enc` log via a `ChatRecordPayload::Mail` variant; attachment bytes live as encrypted vault blobs (`put_blob`) referenced by id. One log, one encryption path, one backup path.
- **Threads:** `thread_id` groups a conversation; a reply reuses the thread id and keeps its own copy, so both sides hold the full thread.
- **No directory:** the handle is an invite-carried label (see Identity). There is no handle search, no relay-side address book, and no receiving from non-members in v1.

Relay view of a mail message is unchanged from any other message: an opaque ciphertext blob. Subject, body, and attachment names are inside the Signal envelope and never appear in relay storage.

## What the relay HTTP API looks like (shape)

- `PUT /q/{id}` — store blob (authz for that queue)
- `GET /q/{id}` — list or fetch pending blobs (authz for the owner of the queue)
- `DELETE /q/{id}/…` — ack
- `POST /q/{id}/report` — mark slot abusive (rate-limit / burn)
- No search, no profiles, no public keys directory

TLS for transport privacy in transit to the VPS; that does **not** replace E2E.

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

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
  - app/version so old clients fail clearly
- Display names are local only.

Device link (old device alive): show a QR from the unlocked old device; new device becomes a second Signal device in that identity. Exact linking follows libsignal’s device model; we do not invent a second identity system.

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

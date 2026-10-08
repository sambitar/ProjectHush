# Threat model (v1)

Plain language. If we cannot say it here, we do not say it on the website.

## What we are protecting

- **Message and file contents** from the company, the relay host, the cloud replica host, and other users who were not invited.
- **The vault at rest** on a locked device (passphrase/PIN; auto-lock).
- **Who talks to whom**, as seen by *our relay*, so it is not a social-graph database. The relay stores blobs on random queue IDs, not “Alice’s inbox.”

## Who we treat as an attacker

- Us (operators), VPS host, BYO cloud.
- A roommate or thief who finds a locked laptop/phone.
- A random person on the internet who does not have an invite slot.
- A contact who was blocked (they should not keep delivering to a live slot).

## Who we do not claim to beat

- Someone who already has the unlocked app or the passphrase.
- Malware on the device.
- A government that seizes the device and compels keys, or watches all network traffic (ISP-level correlation of sizes and times).
- A thief of the **update signing key** (that person can ship a malicious app). Protect that key.

## Assets

- Identity keys and Signal session state
- Vault (messages, files, local nicknames)
- Recovery code
- Queue **read** tokens — never published, and the only credential that lists, reads, deletes from or
  burns this account's queues (its inbox and the per-contact queue it mints for each contact).
- Update signing key (operator)

Deliberately *not* assets, because they are published by design: the invite string and the queue
**delivery** token. A delivery token authorizes putting a message in that queue and nothing else, so
handing it to a contact does not let them read the queue or destroy it. Flooding it is still
possible, which is what the relay's rate limit is for.

An account's inbox is a **grant channel**, so it is writable by anyone holding that account's invite.
A queue grant is therefore authenticated: the granting account signs the queue id and delivery token
with its identity key, and the recipient verifies that signature against the identity key the sender
**pinned in their invite** — the key that hashes to the sender's account id and that established the
relationship. An unauthenticated write to the grant channel cannot make a recipient deliver into a
queue the writer controls, so the channel being public does not weaken the model. This check is the
security boundary for per-contact queues; a blocklist alone would not be.

There is **no device revocation**, because there is no second device. Before 2026-10-08 an account
could hold several devices, and a signed device list was both the directory peers read and the
authority a grant was verified against; removing a device from that list was the revocation
mechanism. Multi-device was removed deliberately, so that mechanism is gone with it. The remaining
revocation — deleting a **contact** — is unchanged and still burns that contact's per-contact queue.
The prekey endpoint is likewise per account (`/k/{account_id}`) and its authorization is the
publisher's signature against the account id, not a device-list lookup.

## Trust

- **Device (when unlocked):** full access. That is the product.
- **Relay:** ciphertext, size, time, slot ID, IP. Must not learn names or a person directory.
- **Cloud replica:** ciphertext of the vault. Unlock is the user’s passphrase.
- **Payment processor:** name/email/money. The messenger identity is not the Stripe customer ID.
- **Update server:** can offer bytes; the app installs only if they verify the baked-in public key.

## Implications already decided

- No phone/email directory, no global username search.
- No DHT or other public discovery layer. A Tox-style DHT publishes each identity so peers can find it, which makes the social graph queryable by anyone; discovery stays invite-only.
- Add-by QR/invite.
- Report-a-slot, not content scanning.
- Unpaid users keep the local vault.

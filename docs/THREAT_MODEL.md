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

## Paper (a conclave's shared document)

One encrypted collaborative document per conclave, added 2026-10-12. The surface it adds:

| New surface | What it may see | Rule |
|---|---|---|
| Relay | Same as today: ciphertext, size, time, queue id, IP. Paper adds **more, smaller, more frequent blobs** while a paper is open | **No change** to what is visible; the *timing* signal is richer — accepted, same as any live feature; padding is out of scope |
| STUN server | Querying IP + port | Self-hosted default (`stun:hushrelay.space:3478`), overridable only via `ClientConfig::stun_url`. **No TURN** — a peer behind symmetric NAT simply falls back to relay cadence |
| Hub daemon | Each peer's IP (ICE), and doc updates (it merges and re-fans-out) | **Accepted** — trustees are mutually trusted; the hub sees no more plaintext than any trustee already holds in their own vault |
| DataChannel peers | Each other's IP | Same accepted limit as calls, stated in copy |
| Doc bytes at rest | Nothing in plaintext | `papers.enc` is sealed under the vault master key; locked vault = unreadable; the editor runs from local assets only |
| Persistence | The document **persists in every vault by design**, like conclave messages. Deleting is local, per vault | Copy states this plainly — Paper is not "leaves nothing behind" like a call; it is "stays in the trustees' vaults" |

**Honest limits for user-facing copy:** collaboration reveals IPs to trustees; the relay learns *that*
(and roughly when) a conclave is editing, though never what; a kicked trustee keeps the last copy of
the paper their vault already held, exactly as they keep conclave messages.

## Zines (members-only long-form, added 2026-10-12)

Every member's blog of articles with a comments section, readable by **all members** (not only
contacts), added 2026-10-12. It is the **first new relay ability since the founding constraint**, so
it is stated in full. Full design: [`docs/tasks/zines.md`](tasks/zines.md) (Task **21**).

| New surface | What it may see | Rule |
|---|---|---|
| Relay bulletin | Opaque blob sizes, count, times; **not** which member published which blob in storage | Accepted — the lane is a flat log with **no author column** |
| Publish linkability | At publish the relay **must** authorize, so it transiently learns **which member published a zine blob** | **Accepted and documented** — narrower than what the operator already knows (payment identity). Drains reveal no readership (every member drains the whole lane). An optional Tor-path publish is the later fix |
| Re-post linkability | A re-post tells the relay a member **holds** that zine | **Accepted** — in a converging commons (everyone holds everything) this is near-zero signal; it is the price of author-independent availability |
| Read keys | Held in `zines.enc` at rest; a grant gives **permanent** read of that zine | **No revocation**, like a kicked trustee keeping conclave messages; rotation is later |
| Comments | An opaque blob; only the pen name can open it | Accepted; a commenter's slot is linkable to the publish, same as an article |
| Corpus size | Every member downloads every blob | Bandwidth, not privacy; text-only keeps totals in low MBs; the per-member rate limit is the throttle |
| At rest | Nothing in plaintext | `zines.enc` sealed under the vault master key; locked vault = unreadable; the editor runs from local assets only |

**As built (2026-10-13), and what the built form adds to the table above:**

| Surface | What it may see | Rule |
|---|---|---|
| Publish credential | A publish and a drain use the **same** credential (the read token of a queue the member owns), so a publish does not spend or reveal a delivery slot | Narrower than the design note: the operator learns *some* member published, not which slot was written to |
| Re-post | The sweep tells the relay a member **holds** a blob it re-sends, about once a week per blob | Accepted, as above — and it is the only availability path, because relays never talk |
| Grant | A `ZineGrant` rides the existing 1:1 session, so it is sealed by the session the two members already have | Nothing new; the relay sees a normal message |
| Comment blob | Sealed to the pen name's **public** key, so it is anonymous to everyone including the relay — and **any holder can keep re-posting it** | A refused comment is not a retraction: it is never merged, and the blob stays on the lane sealed to the author alone until it ages out |
| Acceptance | A merge is a revision, so accepting a comment **republishes** the post | The thread is public to every reader once accepted; a pending comment is visible only to the author |
| Held bytes | Bounded (1024 blobs, 32 MiB), evicting superseded revisions first, with the drop reported in the daemon log | A holder under pressure loses bytes; the alternative (unbounded growth) is worse, and the cap is stated rather than silent |

**Rejected surfaces, recorded so they are not re-proposed:** a **public IPFS/DHT** layer (announces
your IP *and* your interest in that exact content to the swarm — a direct break of "the relay cannot
learn who reads what"); a **private swarm in v1** (a second network stack for a text-only feature);
a **relay-side category registry** (a relay directory, which the founding constraint forbids —
sections live inside the encrypted article instead).

**Honest limits for user-facing copy:** publishing reveals your member slot to the relay operator
(not to other members); a zine you hold can be seen to be held if someone re-posts it; read access,
once granted, is not revocable and spreads transitively, because any holder may hand the key on;
a comment is sealed to a pen name rather than sent to an inbox, so the author can refuse to merge it
but cannot take it off the lane; and every member holds a copy of what they have received — zines are
not "leaves nothing behind."

## The public lane (AT Protocol / Standard.site, added 2026-10-14)

Publishing a zine to the **open web** and reading other people's public documents back, added
2026-10-14. This **replaced** the Nostr lane (relays, BIP340 signatures, `npub`/`naddr`/`nevent`,
NIP-23 export, NIP-22 comments, and a per-vault public identity); nothing of that shape remains in
the code. Full design: [`docs/tasks/atproto.md`](tasks/atproto.md) (Tasks **11** and **22**).

| Surface | What it may see | Rule |
|---|---|---|
| Identity | The member's **own DID and PDS**, connected with an app password | Hush mints nothing and operates no PDS; the member revokes the session at their own account without Hush's help |
| Session | An app password (once) and an access/refresh JWT pair | Its own encrypted sidecar (`atproto-connection.enc`), in the Task **6** backup, rotated on write, **never** in a response or a log; the credential-discipline test asserts the connect answer carries no password or token |
| Publish | A **public, permanent record in the member's own repo** | Explicit per-post confirmation that lists what leaves (title, tags, body, first-publish date) and what does not (thread, history, pen name, read key); the record shape has **no field** for anything private, so stripping is a property of the shape rather than a step that can be forgotten |
| Read egress | Each followed PDS learns **which publication was read and by whom** (its own access log); the member's own PDS learns nothing from a read | **Pull-only and per-repo**, so no index and no AppView accumulates a graph; routed through the relay's own proxy setting |
| Opening a post | Nothing | Served from the cache; opening makes no request, so no PDS learns which document was opened |
| Public→private linkage | A published zine links **this vault to that ATProto account** | Stated plainly in the dialog. The pen name still covers the bulletin; the private zine and its public record are **separate objects** and are never cross-linked or deduped |
| At rest | `atproto-connection.enc` and `atproto-state.enc`, sealed under the vault master key | Cached public content is encrypted for **hygiene, not secrecy** — it is public either way |

**Honest limits for user-facing copy:** a public post is **public, permanent and cannot be
un-published from here**; the record lives in the member's own account, which is what can edit or
remove it. Reading the lane reaches the network only when **Refresh** is pressed, and publishing only
when **Publish** is pressed.

## Conclave voice (group audio in a conclave, added 2026-10-13)

A live, in-memory voice room per conclave among trustees, added 2026-10-13 (spec:
[`docs/tasks/conclave-voice.md`](tasks/conclave-voice.md), Task **23**). Full mesh of direct
peer connections; Phase 1 (signaling) landed, real audio behind the engine spike.

| New surface | What it may see | Rule |
|---|---|---|
| Relay (control plane) | Same as today: ciphertext, size, time, queue id, IP. Voice signaling is ordinary fanned-out Signal ciphertext; the relay carries **no media** | **No change.** The `Voice*` payloads are indistinguishable from any other conclave fan-out |
| Joined trustees | Every other joined trustee's IP and ICE candidates | **Accepted** — the mesh exposes the set, not one peer. A mic is louder than a message, which is why joining a room is an explicit consent act; the exposure is stated in copy |
| STUN server | Querying IP + port, no party linkage | Self-hosted default (`stun:hushrelay.space:3478`), overridable via the existing `ClientConfig::stun_url`. Runbook: [`docs/ops/voice-stun-checklist.md`](ops/voice-stun-checklist.md) |
| First-party relay (TURN) | — | **None** in v1; never first-party. A peer behind symmetric NAT gets a clear failure, not a relay |
| Media on disk | Nothing | Never written, no temp files, no recording or transcription — ever ([`DESIGN.md`](DESIGN.md)); voice state is memory-only, so a restart or a lock ends the session |
| Vault | Nothing new in v1 | No call-history rows, no chat notices for voice events; the session leaves no trace |

**Honest limits for user-facing copy:** everyone in the room hears everyone (a room, not a dial
tone); every joined trustee learns every other joined trustee's IP; the network can correlate media
timing and size though never content; a locked or offline trustee is not in the session.

## Expiring chat lines (added 2026-10-21)

A 1:1 chat line sent with `/expire`, deleted from both sides at a chosen deadline.

| New surface | What it may see | Rule |
|---|---|---|
| Relay | The same ciphertext blob as any chat line, until the recipient's next drain consumes it | **No change.** The send is refused when the recipient is offline, so no copy is parked to be delivered after the deadline |
| Vault | **Nothing.** No `chat.enc` row is written on either side; there is no export, history or backup copy | The line lives only in the daemon's poll response and in each UI's memory |
| UI memory | The plaintext until `expires_ms` | A reload, a lock or a crash loses it early; a missed poll loses it entirely |
| Screen capture | Whatever is on screen | **Not prevented.** A photograph or recording keeps the line; the feature promises deletion from Hush, not from the world |

**Honest limits for user-facing copy:** deletion is timed by the peers' clocks, so a large clock
skew can shorten or lengthen the visible window; a line that cannot be delivered because the peer
went away is not queued — the sender is told the chat needs both sides online. The dialog says the
message is never saved in either vault, which is true; it does not claim the relay never held the
ciphertext.

## Chat links (added 2026-10-21)

A `/link` message is ordinary text; the relay sees the same opaque ciphertext as any chat line.
The shell makes only absolute `http(s)` URLs clickable, refuses credentials and non-`http(s)`
schemes, and routes every click through a confirmation that names the host before handing the URL
to the system browser.

| New surface | What it may see | Rule |
|---|---|---|
| Relay | The same ciphertext blob as any text message | **No change.** No preview fetch, no link metadata, no new payload |
| Link destination | The user's normal browser identity and IP, once the user confirms | **Accepted and stated** — the confirmation says the site will not see Hush's identity |
| Vault | The URL as part of the ordinary encrypted chat row | Same retention as any text message |
| Peer-supplied URL | Whatever the URL points at | **Never auto-opened.** No preview, no fetcher, no automatic navigation; the webview never navigates to it, and a click is confirmed first |

**Honest limits for user-facing copy:** the link text is still user-visible content and can be
misleading; the confirmation names the host, not the destination's intent. A link is not a
malware scanner.

## Torrent sharing (added 2026-10-21)

A torrent offer is a pointer between trustees: name, info hash and size. The content is fetched by
the recipient's own TorBox account, never carried by Hush.

| New surface | What it may see | Rule |
|---|---|---|
| Relay | The same ciphertext blob as any signal or text message | **No change.** No file bytes, no tracker list, no new relay ability |
| Sender | Which torrents it already holds; the recipient's accepted/declined response | The sender chooses the item from its own Torrents state; the daemon resolves the hash |
| Recipient's TorBox | The torrent hash/name, the recipient's TorBox account, and the swarm traffic | **Accepted and stated** — the offer dialog says the file is fetched by TorBox, not Hush |
| Recipient's network | Swarm peers and traffic to TorBox | Outside Hush's path; the recipient's TorBox settings and network are their own |
| Vault | The offer body in `torrents-state.enc`; the downloaded files as ordinary vault blobs | Same encryption as the rest of the torrents sidecar |

**Honest limits for user-facing copy:** a hash-only magnet cannot recover a private tracker list
that was never retained; a torrent name is not a guarantee of its contents; accepting a share uses
the recipient's TorBox quota and storage, and the recipient can decline. No torrent starts without
both sides' permission and the recipient's explicit confirmation.

## Implications already decided

- No phone/email directory, no global username search.
- No DHT or other public discovery layer. A Tox-style DHT publishes each identity so peers can find it, which makes the social graph queryable by anyone; discovery stays invite-only. (This is also why **zines** use a relay bulletin and not a public IPFS/DHT — see above.)
- Add-by QR/invite.
- Report-a-slot, not content scanning.
- Unpaid users keep the local vault.

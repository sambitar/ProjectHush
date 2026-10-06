# Design decisions

Living record. Reasons and rejected alternatives. Not a marketing doc.

## Settled

### Development vs shipping

- **Develop on Linux.** The author’s machine is Linux.
- **Ship multi-platform where the first audience actually is.** Linux-only is not a product constraint. Desktop + Android are in scope for early users. iOS is later (store rules).

### First audience

- **Decision:** Privacy-already people (A). The author, friends like them, small teams who already want private chat and files and will install an app that is not WhatsApp.
- **Surfaces:** Desktop + Android is enough for day one.
- **Not v1 customers:** Phone-only WhatsApp families (C), journalists/activists as the design center (D), a named existing community (B) unless one appears later.
- **UX constraint:** Write so a non-geek partner can join later (no seed phrases in the normal path). Do not scope v1 as if that partner is the first user.
- **Why:** Matches a solo build, real dogfood, paid-subscription honesty. C would force phone polish and notifications before the vault is real.

### Threat model

- **In scope (C):**
  - Operators and hosts cannot read messages or files (E2E; local vault is source of truth).
  - Casual local snooping: passphrase/PIN unlock, auto-lock, vault encrypted at rest.
  - The relay should **not** be able to map who talks to whom (sealed sender / blind inboxes / similar). This is a v1 design constraint, not a later wish.
- **Out of scope:** State-level targeted surveillance (seized devices, compelled keys, malware on the device). Do not market as if we beat that.
- **Honest limit:** We minimize what the relay learns. We do not claim “no metadata” against a global passive network observer (ISPs, nation-state traffic analysis).
- **Why C despite cost:** Audience A cares about this; it will drive identity, discovery, and relay (no phone-number directory, no simple “lookup Alice” inbox).

### Identity

- **Network identity:** Key-based. Friends add you by QR or invite link sent on another channel. No phone/email ID. No global username search in v1.
- **Display names:** Local nicknames only (C+D). “Sam” lives on your device, not on a directory.
- **Optional later:** A username someone *chooses* to publish, knowing publication is visible.
- **Why:** Matches threat model C. A directory of lookups would map interest and often the social graph.
- **Rejected:** Phone/email as ID (A). Public username directory as the default (B).

### Key recovery

- **Happy path (not “recovery”):** Old device still works → link new device with QR.
- **Lost device (D):** Passphrase-encrypted vault replica on optional BYO cloud (WebDAV/S3), plus an emergency recovery code (shown once / buried in settings, not the normal home screen).
- **Not v1:** Trusted-contact / social recovery.
- **Failure they must accept:** Forget passphrase **and** lose device **and** lose recovery code → identity and vault are gone.
- **Why:** Matches local-first + optional cloud. No stranger key-shards on friends’ disks in v1.

### Messaging protocol

- **Decision:** Signal protocol via `libsignal` (Rust) for one-to-one ratchets. Encrypted files ride the same sessions.
- **Not the protocol:** The invite/QR identity and blind/rotating inboxes are *transport*. Do not invent a new ratchet.
- **Needs expert review later:** Sealed-sender / blind-inbox layer (easy to get wrong).
- **Rejected:** vodozemac as the v1 1:1 stack; Noise-only (too much invention); adopting SimpleX wholesale.

### Relay

- **Decision:** Invite-minted rotating receive queues (B). One cheap first-party VPS at the start. Ciphertext only. Per-queue rate limits; unused/blocked slots are burned. Blobs expire after a few days.
- **Relay may see:** size, time, slot ID, connecting IP.
- **Relay must not see:** names, a first-class social graph, plaintext.
- **Not v1:** User-hosted relays as the only option; no store-and-forward.
- **Later:** Optional self-hosted relay as a checkbox. Push tokens must not be the messaging identity (spec).
- **Rejected:** One inbox per person (A) — leaks the graph.

### Multiple recipients

- **v1:** Fan-out only. Same file/note is encrypted and sent per contact. No group object. Recipients do not see the other recipients.
- **Later (not scheduled):** Lightweight groups possible. No shared vault/folder.
- **Keep unblocked:** Conversation-scoped message/attachment IDs so a group thread can exist later without a shared storage pool.
- **Rejected:** Real groups in v1 (C).

### Social layer

- **v1:** None. DMs and files only.
- **Later:** Keep both a follow-model and small private rooms *possible*. Build neither now.
- **Data-model constraint:** Vault stays per-person. No global feed as a first-class store.
- **Note:** If one is built first later, private rooms fit threat model C better than a public square.
- **Rejected:** Committing to public follow-only, rooms-only, or never-any-social.

### Calls (later — Task 17)

- **Decision:** Audio (video later) is **direct peer-to-peer** WebRTC between contacts. No Hush server carries the media; the existing relay carries only the authenticated handshake. A contact learns your IP, which is **accepted** — contacts are mutually trusted and WebRTC always reveals it.
- **No first-party media relay.** We do not run TURN, so we are never in a position to see a call graph. The relay must not map who talks to whom; a media relay would map it unambiguously.
- **IP-level privacy** belongs to the OS/distro layer (a future distro can ship network protections) or an optional user-supplied relay/overlay for IP-hiding — not the app's default.
- **Live-only, unlocked only.** A locked vault holds no keys, so it cannot even read an incoming call invite. No calls while locked or closed, and no third-party push (a push token is a metadata leak).
- **No recording, no transcription, ever.** A call leaves nothing behind.
- **Detail:** Task **17** ([`docs/tasks/member-calls.md`](tasks/member-calls.md)).

### Payments

- **Decision:** Subscription via a normal card processor (Stripe or similar) as the default.
- **What they pay for:** First-party relay, easy updates, optional first-party storage. Not the right to open their own files.
- **Unpaid / lapsed:** Vault, history, and local encryption still work. QR add and direct/online paths still work. You may cut *your* relay, *your* storage tier, and official auto-update. Do not brick the capsule.
- **Anonymous payment:** Later optional extra, not v1. Default payment **will** link money to a name/email; do not market the product as anonymous.
- **Rejected:** Lifetime license as the business (B); free-core as the plan (C); crypto as the default (D).

### Safety and moderation

- **Decision:** Local block/mute/hide, plus report-a-slot to the first-party relay (B).
- **Cannot do:** Read or scan DM/file plaintext on the relay. No hash-scanning of ciphertext. No on-device CSAM scanner in v1.
- **Spam:** Invite/slot required. Per-queue rate limits. Block burns the slot.
- **Hard line:** Plaintext we actually host (website, support, logs) — remove and report illegal content as required. Ciphertext: act on **relay abuse** (flood, spam slots), not speech inside chats.
- **Rejected:** Content filters on payloads (C); on-device scanning as a product feature (D).

### Public knowledge (later — after private capsule is real)

- **Decision:** Optional **pseudonymous publishing** (Nostr-shaped **Articles**), not a Hush-hosted social graph or viral feed. Private chat stays on libsignal + blind queues; public work uses **pen names** isolated from messaging keys.
- **Product arc (one pipeline, many surfaces):** Conversation (private) → publish idea → link evidence → local library → optional **project** index — without likes, trending, or our relay learning who follows whom.
- **Feeds:** Follow-based and **explainable** (“included because…”), not engagement ML. “Editor’s picks” = curated pubkey lists in config, not ranking.
- **Reputation (if any):** Structured links (supports / refutes / evidence), optional local “I changed my view” notes — **not** follower counts or upvotes as the primary signal.
- **Storage:** Reading list, shelves, annotations, and graph edges live in the **encrypted local vault** (per person). Nostr cache is public-by-design; UI must say so.
- **Rejected for this layer:** Global “For You” algorithm; Hush-operated follow graph; in-app crowdfunding; activist/“uncensored” positioning; first-party Nostr relay at launch.
- **Detail:** Task **11** ([`docs/tasks/hush-articles.md`](tasks/hush-articles.md)); Tasks **12–15** backlog in [`docs/plans/knowledge-layer-roadmap.md`](plans/knowledge-layer-roadmap.md).

### Public positioning

- **Decision:** Quiet ownership (A). Landing: your data on your machine; we cannot read it. Calm capsule, not a cause.
- **Allowed:** A short “How this is designed” page with the technical facts (E2E, local vault, dumb relay).
- **Rejected:** Activist/uncensored brand (C); angry split voice (D). “Uncensored” is not the product name or hero line.

### Frontend

- **Decision:** Tauri, local web-tech UI in an app window (A). Rust core + local daemon; UI never fetched from the network.
- **Ship:** Desktop first in practice; Android in the same project when the core works. iOS later.
- **Rejected:** Flutter (two languages); GTK/Qt plus a second mobile UI; egui/iced as the product UI.

### Updates and signing

- **Decision:** Signed auto-update (B). Key pinned in the binary. Website hosts artifacts; HTTPS alone is not trust.
- **Fallback:** Signed manual downloads (AppImage/APK).
- **Later extra:** Flathub / F-Droid, not a v1 gate.
- **Crown jewel:** The update signing key. Treat like production secrets.
- **Unpaid:** Last installed version still runs; you may stop *your* updater. Vault still opens.
- **Rejected:** Unsigned / “just HTTPS” updates (D). Manual-only as the only path (A).
- **Later, not v1:** Reproducible builds.

## Rejected (so far)

- Linux-only v1 as the *product* (kept only as the *dev* environment).
- High-risk users as first audience.
- Threat model D (state targeted) as a promise.
- Threat model A-only or B-only (too weak given C).

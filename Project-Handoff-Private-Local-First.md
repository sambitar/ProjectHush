# Project Handoff: Private, Local-First, Censorship-Resistant Communication App

You are my design and engineering partner. This document is a handoff from a long design discussion. **Do not start writing code yet.** Your first job is to help me finish clarifying the design, one decision at a time, and only then produce a plan and begin implementation.

---

## 1. Vision

A private communication and storage app for **ordinary, non-technical people** (not just geeks) who want to feel that their digital space is theirs and cannot be taken away, deleted, or sold. The user should feel they are inside a **"private capsule"**: sealed, calm, theirs.

Marketing idea: *"Your data. On your desk."* / *"A place online that's yours."*

Censorship resistance is a benefit of the architecture, not the headline. The goal is free speech at the protocol level and safety at the experience level (user-controlled filtering, blocking, muting), because ordinary people experience a totally unmoderated space as unsafe.

## 2. Decisions already made

1. **Local-first (Design A).** Each user's data lives on their own device. Servers never hold user content.
2. **NOT a shared storage pool.** Users do not store other people's encrypted fragments. No anonymous stranger blobs on anyone's disk. (Rejected due to CSAM/illegal-content risk and user trust.)
3. **Personal vault only, person-to-person sharing only.** No shared group storage spaces. Each storage space belongs to exactly one person. Sharing = one person encrypts something to one specific recipient.
4. **Optional cloud replica, bring-your-own-storage.** Users may choose an approved cloud provider as a backup replica of their encrypted vault. Local copy is the source of truth; cloud is a replica.
   - Build on open protocols (**WebDAV and S3-compatible**), not per-provider custom integrations.
   - Approve providers by published criteria (privacy-respecting jurisdiction, no-logging stance, stable API, track record). Candidates: Filen, Internxt, Storj, Backblaze B2, Hetzner, self-hosted Nextcloud, possibly pCloud/Proton if their APIs allow.
   - **Encrypt on the device before upload.** The provider only ever sees ciphertext (plus size/timing/IP metadata).
   - Optional first-party paid storage tier as the easy default.
5. **Not a web app.** It must be an installed native app with its own window and identity. Reasons: a web server controls the code it ships (can be compromised or subpoenaed to steal keys), browsers are poor at owning data, and a tab does not feel like a sealed capsule. A simple website is allowed **only** for landing/download pages, never for handling user data.
6. **Linux desktop first**, then expand to other desktops, Android, and later iOS.
7. **Server is minimal and "dumb":** discovery, notification, and store-and-forward of encrypted messages for offline recipients. It should hold as little metadata as possible and ideally be absent where p2p works.
8. **Everything encrypted end to end.** Do not invent cryptography; use well-tested libraries.
9. **Business model:** paid subscription (no ads, no data selling, no analytics/trackers). Possibly a choice between paying and contributing resources to one's own circle. Anonymous payment is an optional extra for those who want it.

## 3. Original ideas, status

- **Usenet-style modern discussions:** *Deferred.* Direction settled 2026-10-14: the public layer is the **AT Protocol / Standard.site** lane — a member's own account, posts in their own repo, pulled per-repo. Small private rooms (many one-to-one encrypted links) remain the alternative for group talk.
- **Discord-like chatrooms:** *Deferred.* Must be calmer and lighter than Discord. Readable conversations are a design goal.
- **File pool via Syncthing with moderator deletion:** *Dropped* in its original form (conflicts with the local-first, person-to-person model).
- **Files shown by hash, unreadable without the producer key:** Compatible in spirit: files are encrypted, content-addressed in the vault, and shared by encrypting to the recipient. Keep for the vault design.
- **Strong political/uncensored stance:** Treat "uncensored" as a technical/governance property (hard to shut down or surveil) rather than a branded ideology. Hard line only for clearly illegal content such as child abuse material, on any infrastructure we operate. To be revisited later.

## 4. Proposed architecture (to be validated with me, not assumed)

1. **Core library (Rust):** encryption, key management, local vault, sync, protocol. **No UI code.**
2. **Local daemon:** background process that runs the core, owns the vault, talks to the network. UI is just a client of its local API.
3. **Frontend:** Tauri (Rust backend + locally rendered web-tech UI in its own window). Chosen for being light and reusable on Android later. Open to alternatives if you can justify them.
4. **Relay server:** small, separate, one cheap VPS to start (not full AWS). Discovery, notifications, store-and-forward ciphertext.
5. **Storage:** SQLite with SQLCipher or app-level encryption for local metadata and messages. Content-addressed encrypted blobs for files.
6. **Crypto:** one key pair per person. Use existing vetted work (Signal protocol implementations, Matrix's vodozemac, or Noise-based libraries such as `snow`) plus libsodium or RustCrypto crates. Do **not** design a custom messaging protocol from scratch.
7. **Packaging:** AppImage or Flatpak for Linux.

## 5. Constraints and risks to keep in mind

- **Onboarding and key recovery** is the biggest UX risk. Losing a device must not mean losing everything, and keys must be hidden from normal users (no seed phrases in the normal flow). Needs real design work (trusted contacts or optional recovery code).
- **Metadata is the real surveillance risk,** more than content. Minimize what the relay learns (look at Signal sealed sender, SimpleX, Tor).
- **Spam and abuse:** unsolicited DMs must be controlled by default (approval required, new account limits, one-tap block/report).
- **Mobile limitations later:** background sync, storage, and notifications are restricted, especially on iOS. Relay servers matter more on mobile.
- **App store censorship risk:** Android can sideload (APK, F-Droid); iOS is locked down. Not a concern for the Linux phase.
- **Cloud provider risk:** account termination or API changes. Keep the storage layer swappable and warn users that local is the source of truth.
- **Legal:** payment records can deanonymize subscribers; relay operator exposure should be kept minimal by never holding readable content.
- **Network effect:** target one specific first audience instead of "everyone" (to be decided).

## 6. Capsule feel (UX principles)

- Own window and identity, no browser chrome.
- Unlock with passphrase/biometrics/PIN, auto-lock.
- A visible "your vault" view: what is on this device, how much space it uses, what (if anything) is replicated to the cloud.
- Fully usable offline, with a clear "sealed" indicator that nothing leaves without the user's action.
- No analytics, trackers, or ads, stated in plain language.
- Calm, distinctive, readable design. Explicitly **not** a Discord clone.
- Plain-language trust messaging ("nobody at our company can read your messages"), not crypto jargon.
- Familiar shapes (chats, contacts, files) so a WhatsApp user understands it within a minute.

## 7. Suggested build order (only after the design is settled and I approve)

1. Vault and identity: keypair generation, passphrase-encrypted key storage, local encrypted storage.
2. Two daemon instances on one machine exchanging encrypted messages through a relay.
3. Minimal relay: register identity, look up public key, store-and-forward ciphertext.
4. Basic UI: contacts, conversation view, send-file button.
5. File sharing: encrypt to recipient, store in vault, send.
6. Cloud replica via WebDAV/S3 once the rest is stable.

Discussions, follow model, and chatrooms come only after the one-to-one core feels good.

## 8. How I want you to work

1. **Design before code.** Walk me through the open questions below **one at a time**. For each: explain the options and trade-offs briefly, give a recommendation, and wait for my decision.
2. **Record every decision** in a living `docs/DESIGN.md` (decisions, reasons, rejected alternatives). Keep a separate `docs/OPEN_QUESTIONS.md`.
3. **Challenge me honestly.** If an idea of mine is risky, legally dangerous, or technically unrealistic, say so directly and propose alternatives. I do not want yes-saying.
4. **Do not invent cryptography.** Prefer proven libraries and protocols; flag anything that needs expert review.
5. **Keep it simple.** Solo developer, Linux first. Prefer boring, well-supported technology. Avoid premature scaling (no AWS until needed).
6. **No code until I say so.** After the design is clear, produce: (a) a written protocol spec, (b) a threat model, (c) a Rust workspace layout, (d) a phased implementation plan with milestones. Only then implement step by step, with tests.
7. Keep answers concise and use plain language; I will be explaining this to non-technical people too.

## 9. Open questions to resolve (in this order)

1. **First audience:** which specific group of ordinary users is the launch target (community, region, interest)? This shapes features and tone.
2. **Threat model:** who are we protecting against (platform shutdown, advertisers, local snooping, governments)? What is explicitly out of scope?
3. **Identity:** username/password, phone number, or just a keypair-based identity? How are contacts discovered without leaking social graphs?
4. **Key recovery:** which mechanism (trusted contacts, recovery code, cloud-encrypted backup of key)?
5. **Protocol choice:** build on Signal protocol libraries, vodozemac, Noise, or something else? One-to-one only for now.
6. **Relay design:** exactly what the relay can and cannot see; how it handles offline delivery, rate limiting, spam, and abuse.
7. **Sharing multiple recipients:** is "send to several people individually" acceptable, or do we need lightweight groups eventually?
8. **Social layer (later):** follow model vs. small private rooms vs. both.
9. **Payments:** subscription provider, anonymous payment option, and what happens when someone stops paying (their local data must remain theirs).
10. **Moderation and safety:** default filters, blocklists, reporting, and the narrow hard line for illegal content on infrastructure we run.
11. **Moderation of positioning:** how "uncensored" and "political" the product is publicly, given hosting and payment-provider risk.
12. **Frontend stack:** confirm Tauri vs. alternatives (Flutter, native GTK/Qt) given the capsule feel and future Android support.
13. **Updates and signing:** how releases are signed and verified; how updates are delivered without an app store.

## 10. Your first response should be

1. A short confirmation that you understood the vision and the decisions in section 2.
2. Any contradictions, gaps, or risks you spot in this document.
3. Question #1 from section 9, with 2 to 4 concrete options and your recommendation.

Do not write code and do not scaffold a project yet.
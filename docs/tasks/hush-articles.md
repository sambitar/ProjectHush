# Articles (Nostr pen names) — planning only

> **Reshaped 2026-10-12 — this task is now the Nostr _export_ bridge, not the native shape.**
> The native members-only long-form surface is **zines** ([`zines.md`](zines.md), Task **21** /
> Phase **O**): a broadcast relay bulletin, pen-name identity, no public network, no follows.
> Nostr keeps only the part it is uniquely good at — **reach beyond the membership** — as an
> optional export of a zine as a NIP-23 event over Tor.
> **Superseded below:** the feed engine, follows/mutes as a discovery mechanism, the relay pool as a
> product surface, the phase-2 global stream, and the `crates/hush-articles/` product role (it is now
> **export-only**). **Still relevant:** pen names, the NIP-23 event shape, media hardening, the Tor
> egress, and the sanitizer/render rules.
>
> **Amended 2026-10-13 (neighborhoods):** the export now signs under a **separate public identity**,
> not the pen-name keys, and the crate gains a **read** lane — see [`neighborhoods.md`](neighborhoods.md)
> (Task **22** / Phase **P**). Export is explicit, per-post, and re-encodes the body so the public
> event does not correlate with the private zine.

**Status:** Both halves are implemented in `crates/hush-articles/` as of 2026-10-13: the **read** lane (neighborhoods, Task **22**) and the **export** bridge described here (Task **11**), plus the **comment** lane (NIP-22, neighborhoods Phase **4**). The export is reachable at `POST /api/zines/{id}/export`; the transport is the hand-rolled NIP-01 client, not `arti`. **Index:** Task **11** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md) — **reshaped 2026-10-12** to an export bridge for **zines**.

**Goal:** Long-form **pseudonymous publishing** inside Hush: pen names, Nostr-compatible articles, comments, and local social graph — **isolated** from 1:1 messaging identity and from `hush-relay`.

---

## Opinion / fit (summary)

| Strength | Risk / mitigation |
|----------|-------------------|
| Pen names **independent** of Signal/messaging keys — good for journalists | Must be explicit in UI: “This is public pseudonymous publish, not private chat.” |
| **Local follows/mutes/drafts** — no published follow lists | Phase 2 **global firehose** is opt-in; default stays followed-authors-only |
| **Daemon-only** network + sanitization — matches distro direction | Second network stack (**Nostr over Tor** via `arti`) — document beside `hush-relay` client |
| **No first-party Nostr relay** at launch — avoids becoming the host | Curated default relay list is still a trust/availability dependency |
| Media hardening (no raw SVG, EXIF strip) — necessary | Blossom/hash media is new trust surface; verify hash + Tor fetch in daemon |
| NIP-23/22/09/50/56 mapping is standard | **Verify NIP numbers and kind constants** before coding; rust-nostr version lock |

**Not a rewrite of messaging:** `hush-core` / libsignal / blind queues stay for private mail. `hush-articles` is a separate crate; only shares vault encryption, backup (Task 6), and daemon API patterns.

---

## Architecture

```
apps/hush-tauri  ──HTTP/UDS──►  hush-daemon  (/articles/*)
                                    │
                                    ▼
                            crates/hush-articles
                            (read lane: gate, cache, index, NIP-01 client)
                                    │
                                    ▼
                            Tor SOCKS proxy ──► public Nostr relays
                            (the shell's Tor; optional Blossom-style media later)
```

- **`crates/hush-articles/`:** the read lane (NIP-23 mapping, the quality gate, the `(pubkey, d)` dedupe, the encrypted cache and index, and a minimal NIP-01 client over the daemon's Tor SOCKS proxy). The pen-name keys, event build/verify, media pipeline, feed engine and drafts described below are the **export** half and are still unbuilt.
- **Daemon routes** (all behind vault unlock + articles subsystem gate):  
  `feed`, `article`, `search`, `publish`, `comment`, `follow`, `mute`, `drafts`, `pen_names` (exact paths TBD under `/api/articles/…`).
- **UI:** renders **only sanitized HTML/markdown** from daemon; no raw relay JSON in webview; no remote image URLs (daemon local media URLs only).

---

## Pen names (isolated identities)

- Each pen name: **random keypair** in vault (not derived from messaging identity).
- **Separate Tor circuit** (and relay connections) per pen name so relays cannot correlate pen names by IP.
- **Key loss = identity loss** → pen-name material must be included in **Task 6** backup/recovery export (document in backup manifest).

---

## Protocol mapping (verify NIPs before implementation)

| Need | Mechanism | Notes |
|------|-----------|--------|
| Article | Long-form markdown, **NIP-23** (kind **30023**, addressable) | Edit = replace prior version by address |
| Comments | **NIP-22** comment events → article address | |
| Retraction | **NIP-09** deletion request | UX: honest “request”, not guaranteed removal |
| Search | **NIP-50** where supported + **local FTS5** (seen events only) | |
| Cited by | `a` reference tag on citing article; count via relay queries | **Approximate** |
| Reports | **NIP-56** report events + **local mute** rules | |

**Local-only by default:** follows, bookmarks, mutes, drafts — **do not publish follow lists** (metadata risk for journalists).

**Footnotes:** GFM footnotes for other clients; **structured source tags** for rich Hush rendering.

---

## Media pipeline (daemon)

1. Strip EXIF/location; re-encode; resize.
2. Rasterize infographics (**SVG → PNG** on publish; no raw SVG to readers).
3. Hash image; upload via **Tor** to hash-addressed media (Blossom-style); user-configurable media server.
4. Reader: daemon fetches via Tor, **verifies hash**, caches **encrypted**, serves **local URL** to UI.

---

## Feed engine

| Phase | Behavior |
|-------|----------|
| **1** | Followed authors only, newest first, timestamp cursor |
| **2** | Optional global newest-first stream (opt-in) |

- Dedupe by **article address**; keep latest version only.
- Sort by **locally received time**; clamp future `created_at` (forgeable).
- **SQLite** cache for offline read.

---

## Rendering safety

- No raw HTML from network.
- No remote images in UI — rewrite to daemon-local URLs.
- Sanitizer on all markdown output.
- **Confirm** before opening external links.

---

## Relays

- Publish to **≥ 3** relays; read from many; track health.
- Ship small **curated default list**; user-editable.
- **Do not** run a first-party Nostr relay at launch.

---

## Build order (core tests first)

| Step | Deliverable | Verify |
|------|-------------|--------|
| **11.1** | Pen names + relay pool (rust-nostr); local test relay | Sign/verify event; publish to test relay |
| **11.2** | Publish + read round trip | Integration test |
| **11.3** | Feed engine phase 1 (followed authors) | Cursor pagination; dedupe; offline cache |
| **11.4** | Media pipeline | EXIF strip; SVG rasterize; hash verify |
| **11.5** | Editor UI (drafts local) | Draft never on relay until publish |
| **11.6** | Comments, search (NIP-50 + FTS), mute, report | |
| **11.7** | Feed phase 2 (global stream, opt-in) | |
| **11.8** | Tor isolation hardening (per-pen-name circuit) | Manual: two pen names, distinct paths |

---

## Dependencies

- **Task 5c** — encrypted SQLite/cache patterns under data root; no plaintext article bodies on disk at rest when locked.
- **Task 6** — backup/recovery **includes pen-name secret keys** and articles DB.
- **Task 5** — daemon route pattern; Unix socket API; distro rules for outbound network.
- **Not** dependent on `hush-relay` (different protocol).

---

## Distro-readiness

- Article cache, drafts, FTS DB under **`HUSH_DATA_DIR`**.
- Tor traffic through the **shell's Tor SOCKS proxy** (the same one the relay client and wallet use) — document in the syscall/network inventory (second egress beside the messaging relay client).
- UI: no auto-open external links; sanitized output only — aligns with [`DISTRO_READINESS.md`](../DISTRO_READINESS.md).

---

## Conflicts / open questions

| # | Topic |
|---|--------|
| 1 | Milestone Task 8 defers **“follow-model”** for **messaging** — Articles uses **local** follows only; clarify in product copy. |
| 2 | **Global “no remote assets”** — articles media is fetched by **daemon**, not webview; OK if UI never holds remote URLs. |
| 3 | **Transport dependency weight and audit surface** — settled 2026-10-13: the read lane uses a hand-rolled minimal NIP-01 client (`tokio-tungstenite`) over the existing Tor SOCKS proxy rather than `rust-nostr` + `arti`, to avoid a second Tor stack beside the one the relay and wallet already use. Re-run `cargo-deny` before ship. |
| 4 | NIP/kind drift — pin spec version in crate; test vectors. |
| 5 | Journalists on **persistent USB** — Tor state and cache size on limited partition. |
| 6 | Relationship to **5d internal mail** vs public articles — separate rails in UI. |

---

## After Task 11 (Phase A — Task 12)

Task 11 delivers **read, publish, follow, comment, search, bookmarks**. Phase A adds **argument links, library shelves, private annotations, explainable briefing, vault→article** without new protocols on the critical path:

→ [`docs/tasks/knowledge-layer-phase-a.md`](knowledge-layer-phase-a.md)

Ship **bookmarks/reading list** in 11.6 as a thin list; **shelves + annotations + edges** land in 12.x.

---

## Acceptance criteria (overall)

- [ ] Pen name A and B cannot be linked by relay IP when using separate circuits (best-effort test).
- [ ] Messaging identity pubkey never appears in article events.
- [ ] UI cannot load unsanitized markdown/HTML or remote images.
- [ ] Drafts and local follow list never published to relays by default.
- [ ] Backup restore recovers pen names and readable cache.
- [ ] Publish to 3 relays; read offline from cache after fetch.

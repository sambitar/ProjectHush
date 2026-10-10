# Reads (news reader) — rail pane

**Status:** Phase 2 complete and verified (2026-10-09, same day as phase 1). Owner call:
strip the HN surfaces and search — Reads is now a **source list**. One built-in source,
**HK News** (Hacker News best-of-the-week, shipped with the app, cannot be removed), plus
**user-added RSS feeds** (RSS 2 / Atom / JSON Feed via `feed-rs`). Reading behavior
(reader mode, save/hide/read markers) is identical for every source.

**Relationship to other docs:** Shares the daemon-only-network and sanitized-rendering
posture of [`hush-articles.md`](hush-articles.md) and
[`knowledge-layer-roadmap.md`](../plans/knowledge-layer-roadmap.md), but reads **public**
data — no Nostr, no pen names, no publishing.

---

## Decisions (owner-confirmed 2026-10-09)

| Decision | Choice |
|----------|--------|
| Sources | One built-in (**HK News** = HN best-of-the-week) + user-added RSS feeds; the built-in cannot be removed |
| Article reading | **In-app reader mode**: daemon fetches the page, extracts main content, sanitizes, caches; "open original" as fallback behind a confirm |
| Comments | **In-app, read-only**, HN stories only — daemon fetches the comment tree, sanitizes HTML, collapsible in the UI; RSS items have no discussion and open straight into the reader |
| Network path | **Direct HTTPS** from the daemon (no Tor) — HN and RSS feeds are public and unauthenticated; documented as a deliberate exception to the Tor egress default |
| Personal data (sources / saved / read / hidden / history) | **Daemon-side, vault-backed** — encrypted sidecar file beside `drop-keys.enc`, never the webview |
| Removed in phase 2 | Front/Newest/Ask/Show surfaces, the search box, and the domain filter (owner: "only show the best… that's it") |

---

## Architecture

```
apps/hush-tauri (Reads rail pane)
    │  /api/reads/*  (same transport as every other pane: UDS or loopback HTTP)
    ▼
hush-daemon
    ├─ reads.rs        HK News (Algolia) + RSS client (feed-rs), TTL cache
    │                  reader-mode fetch + extract + sanitize (ammonia)
    ├─ reads_store.rs  sources/saved/read/hidden/history (vault-encrypted sidecar)
    └─ reads-cache/    public content cache under the data dir (plaintext ok — public data)
                         │
                         ▼
        hn.algolia.com + feed hosts (direct HTTPS, no auth)
```

- The webview never talks to hn.algolia.com, feed hosts, or article sites; every byte it
  renders comes from the daemon, already sanitized.
- `hush-core` gained one small, generic vault method pair —
  `read_encrypted_file`/`write_encrypted_file` — generalizing the existing `drop-keys.enc`
  pattern; the JSON schema stays owned by the daemon.

### Sources

| Source | Fetch |
|--------|-------|
| `hk-news` (built-in) | `search?tags=story&numericFilters=points>50,created_at_i>{now-7d}` (best ≈ top recent; HN's /best has no direct API equivalent) |
| RSS feed (user) | daemon `GET` of the feed URL, parsed by `feed-rs`; entries without a link are skipped; rows sorted newest first |

- RSS entry ids are `rss-` + 16 hex of the link's SHA-256 (stable across fetches, never
  colliding with HN's all-digit ids — saved/hidden/read state keys on them).
- Adding a source validates it **by fetching once**: the SSRF guard runs first, the feed
  must parse, and its own title is adopted. Re-adding a URL refreshes the title instead of
  duplicating. At most 50 feeds.
- HN story detail: `items/{id}` (story + recursive comment tree). Comments arrive as small
  HTML fragments; allowed tags after sanitize: `p, a, i, em, b, strong, code, pre,
  blockquote, ul, ol, li, br`. Everything else stripped. `a` gets no auto-open: the UI
  routes every link through a confirm step.

### Reader mode

- Fetch limits: 10 s timeout, 2 MiB body cap, ≤ 5 redirects, `http(s)` only, loopback /
  link-local / private-IP literals refused.
- Extraction: readability-lite scoring over parsed DOM (text density, class/id hints);
  output sanitized with the same allow-list as comments.
- **Images are stripped in this phase** (the webview must not load remote assets; an image
  proxy through the daemon is a later option).
- **Bot challenges fail honestly.** A site that answers with a Cloudflare-style challenge
  (403/429/503, `cf-mitigated: challenge`) becomes `ReadsError::Blocked`, reported as
  `this site refused an automated reader (HTTP 403)` instead of a raw status line. No header
  spoofing is attempted — a Chrome User-Agent, full `sec-ch-ua` client hints and HTTP/1.1 were
  all still refused, because the challenge fingerprints the TLS/JS stack — and no retry loop is
  used: the block is intermittent per-IP and six immediate retries did not converge, so
  retrying would only add load and burn reputation. The pane shows a centered
  "This site blocks automated readers" state with **Open the original** as the one obvious
  action, so a blocked page never reads as a bug (2026-10-10).
- Extracted text cached per URL under `reads-cache/`, size-capped; stale entries pruned.
  Failures are never cached, so a block is retried on the next open rather than pinned.

### Personal state (vault-backed)

`reads-state.enc` next to `drop-keys.enc`, encrypted with the vault master key:
user RSS sources, saved stories (metadata + when saved), hidden story ids, read markers
(capped), reading history (capped). Written on mutation; readable only while unlocked —
while locked, `/api/reads/sources` still answers with just the built-in so the pane renders.
Backups that include the vault directory automatically include it.

### API (daemon routes)

| Route | Purpose |
|-------|---------|
| `GET /api/reads/feed?source={id}&page=N` | story list; `hk-news` pages (30/page), RSS feeds arrive whole (`done: true`) |
| `GET /api/reads/sources` | built-in + user sources |
| `POST /api/reads/sources {url}` | validate + add an RSS feed; returns the new source list |
| `DELETE /api/reads/sources/{id}` | remove a user feed |
| `GET /api/reads/story/{id}` | HN story + sanitized comment tree |
| `GET /api/reads/reader?url=…` | sanitized reader-mode HTML (cached) |
| `GET /api/reads/state` | sources/saved/read/hidden/history |
| `POST /api/reads/saved` / `DELETE /api/reads/saved/{id}` | toggle save |
| `POST /api/reads/hidden` / `DELETE /api/reads/hidden/{id}` | hide / unhide |
| `POST /api/reads/read` | mark read (history + read marker) |

### UI

- Rail button `Reads` (`BookOpen` icon) between Conclaves and Mail — reading sits beside
  conversation, matching the knowledge-layer positioning.
- Three columns, mirroring Mail's shape: nav (**source list** with the built-in first,
  user feeds with a remove ×, an **"Add a feed" button that opens a dialog**, and Saved) ·
  story list · main pane (reader text, HN discussion behind a toggle for HN stories, "open
  original" behind confirm, save/hide on rows).
- The add dialog owns the URL, the in-flight state, and any refusal reason together, so a
  rejected feed is explained beside the field it belongs to. A URL typed without a scheme is
  assumed to be `https://` (the daemon refuses scheme-less URLs verbatim, which is the common
  input). Enter submits; Escape or a backdrop click cancels. This replaced the cramped inline
  nav field on 2026-10-10, whose errors rendered over in the middle column — which is why a
  refused feed read as "nothing happens when I click Add".
- External links (article original, links inside reader/comment bodies) open through a
  `hush_open_url` Tauri command after an explicit confirm dialog; never auto-opened.
- A reader fetch that fails shows a centered pane state instead of a muted line under the
  article head: an icon, a heading ("This site blocks automated readers" for a bot block,
  otherwise "This page could not be loaded"), the reason, and a prominent **Open the original**
  button wired to the same confirm. Both variants were checked live, including the button
  opening the confirm modal (2026-10-10).

---

## Verification (2026-10-09)

Live browser session against a throwaway vault (`/tmp/hush-reads-e2e`, removed after the
run), daemon rebuilt with each change:

- Phase 1 (superseded surfaces/search): all five surfaces rendered live HN data; reader
  mode extracted article text; a 500+ comment thread rendered collapsed by depth;
  save/hide/read state round-tripped through the vault across a daemon restart; the
  domain chip filtered to a single host and cleared back; external links opened only
  through the confirm dialog.
- Phase 2 (sources): nav shows HK News + Saved only (no surfaces/search chrome); the
  built-in feed paginates (30/page, "More"); `https://news.ycombinator.com/rss` added
  through the UI — adopted its own title ("Hacker News"), opened as the active source,
  rows carry RSS meta (author = feed title fallback, time, domain; no points/comments);
  an item opened straight into the reader with the full article extracted; Discussion is
  disabled for RSS items and enabled for HN (179 comments rendered); saving an RSS item
  worked and appeared under Saved; removing the feed returned to HK News; a re-added
  source survived a daemon restart (locked: only the built-in listed; unlock: user
  source back). `POST /api/reads/sources` while locked → 401; loopback feed URL → 400
  (SSRF guard); unknown source id in the feed route → 404.
- `cargo test -p hush-daemon` 20 passed (HK News URL shape, RSS/Atom fixtures, stable
  link ids, comment sanitizer, SSRF guard, reader extractor, store caps/dedupe/caps),
  `hush-core` suite green, `npm run build` clean.

### Add-feed dialog (2026-10-10)

Replaced the inline nav field with a dialog after the owner reported the add "does not work".
Root cause of the report: a scheme-less URL (`news.ycombinator.com/rss`) is refused by the
daemon, and the reason rendered in the middle column, so the nav showed no change at all.
Verified live against the `peer-a` vault at `localhost:5199/?api=http://127.0.0.1:9876`:

- `127.0.0.1/feed.xml` → the dialog stays open, `refused URL "https://127.0.0.1/feed.xml"` is
  shown beside the field, the value is preserved, and the button returns to "Add feed".
- `lobste.rs/rss` (no scheme) → `https://` is assumed, the dialog closes, the source opens,
  and no duplicate is added.

`tsc --noEmit` and `npm run build` both clean.

## Backlog (not this slice)

- Image proxy through the daemon for reader-mode media.
- Per-source unread counts; "newest" ordering toggle for RSS.
- Offline-first: pre-fetch feeds on launch; read cached content while locked.
- Reading progress and "continue where you left off".
- OPML import for feed lists.

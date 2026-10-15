# Knowledge layer roadmap (future tasks)

**Status:** Vision and backlog only — **not** v1 scope. Filters ChatGPT-style “Magazine / Ideas / Graph” proposals against [`DESIGN.md`](../DESIGN.md), [`THREAT_MODEL.md`](../THREAT_MODEL.md), and existing tasks (**11** Articles, **10** Confidential, **5** messaging).

> **Reshaped 2026-10-12, transport changed 2026-10-14:** long-form publishing changed shape — the
> native members-only surface is **zines** ([`zines.md`](../tasks/zines.md), Task **21**), and the
> public surface is the **public lane** over **AT Protocol / Standard.site**
> ([`atproto.md`](../tasks/atproto.md), Tasks **11** and **22**). The Nostr lane (relays,
> `npub`/`naddr`, a separate public identity, NIP-23 export, NIP-22 comments) is gone. The
> follow-based feed ideas below belong to the **public layer**, not to zines, which is pull-only with
> no follows. Read every “Task 11” reference below as “the publish path”.

**North star (Hush-aligned):**  
*Private capsule for people who already care about privacy → optional **public pseudonymous** publishing → **argument and evidence** over popularity → **local** library and graph on the device → optional **projects** that link talk to work — without Hush becoming a host that reads DMs, a viral algorithm, or a Facebook-style group graph on our infrastructure.*

**ChatGPT arc we keep (reframed):**  
Conversation (private) → Idea → Argument → Evidence → Knowledge → Project → Action — but **stages ship separately**; each must respect threat model **C** and distro rules.

**Positioning (when we ship public layer):**  
*Communicate privately, publish on the open web, and build knowledge on your device — without turning everything into a popularity contest.*

**ChatGPT “Phase 1” → Hush schedule:** Chats + Articles + reading list + pen names + citations + search = **Task 11** (+ bookmarks in 11.6). Magazine sections, argument links, full library, briefing, vault→publish = **Task 12 (Phase A)** — see [`docs/tasks/knowledge-layer-phase-a.md`](../tasks/knowledge-layer-phase-a.md).

**Reputation without likes:** Prefer **typed links** (supports / refutes / evidence), **local** reading history, and optional **private** “I changed my view” journal (Task **13.3**). Do not show global agree/disagree counts or follower leaderboards.

---

## Policy filter (use before adding any task)

| Principle | Implies for knowledge layer |
|-----------|------------------------------|
| **Threat C** | No global social graph on **hush-relay**; the public lane is **public by design** — a member's own ATProto account, reads pull-only and per-repo; the pen name is isolated from the messaging identity. |
| **DESIGN social** | v1 messaging: no social layer. Publishing to the public lane is **opt-in**, Standard.site-shaped, not “Hush feed on our relay.” |
| **No likes / no viral algo** | Feeds: **follow-based + explainable inclusion** (“why you see this”), not engagement ranking. Editor’s picks = **curated list**, not ML. |
| **Moderation B** | Local mute/block/report; **no** scanning DM/file ciphertext on our relay. Public content: user reports + the PDS/ecosystem’s own policy, honest limits. |
| **Vault per person** | Library, annotations, reading list, private documents → **encrypted local store** (Task 5c / data root), not a shared cloud folder. |
| **Quiet positioning** | Product voice: calm capsule + “how it’s designed,” not “uncensored” hero marketing. |
| **Scope** | One serious expansion at a time; **Task 16** notes (optional) → **Task 11** Articles → **12.x** (see milestones **Recommended build order**). |

---

## ChatGPT ideas → Hush stance

| # | Idea | Verdict | Hush-shaped version |
|---|------|---------|---------------------|
| 1 | **Articles “Magazine”** (sections, Editor’s Picks) | **Adopt (partial)** | Task **11** + **12.1**: local **tags/sections** (Essays, Research, …) as **filters on followed + cached** content; “Editor’s picks” = **static curated pubkey list** in app config, not algorithm. |
| 1b | **“Build an argument”** (A→evidence→B→counter) | **Adopt (core differentiator)** | Task **12.2**: **Argument graph** as local links between addressable documents (AT-URI + Hush link types); no new protocol required initially. |
| 2 | **Ideas feed** (AGREE/DISAGREE, no likes) | **Adapt** | Task **13.1**: lightweight **kind** or document subtype “thesis”; responses as **linked documents**; signals = **structured tags** (“evidence”, “challenge”) — **no** upvote counts in UI. |
| 2b | Argument map visualization | **Defer** | Task **14.1** graph UI after links exist in SQLite. |
| 3 | **Research** (papers, datasets, Scholar-like) | **Adapt** | Task **13.2**: extend article template (abstract, methods, sources tags); **datasets** as **hashed attachments** + Blossom; **follow a subject** = **local topic subscription** (keywords/pubkeys), not global “47 researchers” directory on Hush servers. |
| 4 | **Projects** (goals, people, funding) | **Adapt carefully** | Task **14.2**: **project** = local + publishable **index event** linking articles/docs; **no** Hush-hosted funding ledger v1; funding = external link with confirm. Fits “organize work,” not crowdfunding platform. |
| 5 | **Library** (taxonomy, private highlights) | **Strong adopt** | Task **12.3**: encrypted **reading list + shelves + private annotations** in vault; overlaps **5c** storage; **not** “saved posts” only. |
| 6 | **Knowledge graph** | **Defer** | Task **14.1**: SQLite edges (cite, refute, supports, part-of) derived from tags + user links; **no** server-side global graph DB. |
| 7 | **Audio / podcasts** | **Defer** | Task **15.1**: TTS “Listen” local-first; publish audio as media hash; transcript = article body; **no** auto TikTok clips. |
| 8 | **Debates** (structured, “changed my mind”) | **Adapt** | Task **13.3**: debate = **two (or more) linked article trees** + local-only “I updated my view” note (optional anonymous aggregate **only if** publish model defined — default **local**). |
| 9 | **Learning / courses** | **Defer** | Task **15.2**: ordered **reading list** + local progress; no MOOC platform. |
| 10 | **Hush Briefing** | **Adapt** | Task **12.4**: **explainable digest** from **followed authors + local rules** (“3 independent refs”); **not** trending; generated **on device**. |
| 11 | **Investigations** | **Adapt** | Task **14.3**: article **series kind** + timeline/source tags (labels? verify); same as Task 11 media pipeline. |
| 12 | **Circles** | **Conflict / defer** | DESIGN: **groups/rooms later**, private rooms fit C better than public circles. Task **15.3**: **small private rooms** on **messaging** first; a public “circle” only as an **encrypted group key** spec — **not** Facebook groups on the Hush relay. |
| 13 | **Documents vault → publish** | **Align** | Task **5c** blobs + **10** Confidential + **12.5**: private doc → excerpt → public article; **Confidential** stays TPM/lease; library docs stay vault AEAD. |
| 14 | **“Ask the Library” AI** | **Defer / strict** | Task **15.4**: **local retrieval only** over **user’s** library + cache (FTS/graph); optional user-supplied API key; **no** “send my graph to OpenAI” default; no training on user data. |

**Reject outright (for Hush):**

- Global **“For You”** engagement ML feed.
- **Like counts / follower leaderboards** as primary reputation.
- **Hush-operated** public social graph DB (who follows whom) on first-party servers.
- **Funding/wallets** inside v1/v2 knowledge layer.
- **“Uncensored”** or activist-first branding for these features.
- **Running our own public PDS** at launch (Task 11 already says no).

---

## Phased future tasks (after Task 11)

### Phase A — Articles+ (extends Task 11)

| ID | Task | Deliverable |
|----|------|-------------|
| **12.1** | Magazine surfaces | UI sections = filters; curated picks list (config file). |
| **12.2** | Argument links | Typed links between articles; “build an argument” composer. |
| **12.3** | Library | Encrypted shelves, reading list, private highlights/notes. |
| **12.4** | Explainable briefing | On-device morning digest from follows + rules; show **why** each item. |
| **12.5** | Private doc → publish | Pipeline from vault blob to public article (redaction step). |

**Depends:** Task **11**, **5c**, **6** (backup includes library DB).

---

### Phase B — Discourse primitives

| ID | Task | Deliverable |
|----|------|-------------|
| **13.1** | Theses / ideas | Short-form publish + structured response tags (no votes). |
| **13.2** | Research templates | Metadata tags, literature-review article type. |
| **13.3** | Debates | Linked position articles + local “changed my mind” journal. |

**UI verbs (Hush-shaped, not Reddit):** contributors tag responses as **evidence**, **challenge**, or **expand** (published as tags on linked documents) — **no** agree/disagree counters.

**Depends:** Phase A **12.2** links.

---

### Phase C — Organization

| ID | Task | Deliverable |
|----|------|-------------|
| **14.1** | Knowledge graph UI | Local graph from citations + argument links. |
| **14.2** | Projects | Project index + links to articles/docs/tasks (tasks = local only). |
| **14.3** | Investigations | Timeline + claim/source tagging on article series. |

**Depends:** Phase B; **no** Hush-hosted project funding.

---

### Phase D — Media & augmentation

| ID | Task | Deliverable |
|----|------|-------------|
| **15.1** | Audio | Listen (local TTS); publish audio + transcript article. |
| **15.2** | Courses | Ordered lists + progress (local). |
| **15.3** | Private rooms | **Messaging** small groups (DESIGN “later rooms”) — separate from the public lane. |
| **15.4** | Library AI | Optional retrieval assistant over **local** index only. |

---

## Architecture sketch (target state)

```
                    HUSH CAPSULE
                          │
     ┌────────────────────┼────────────────────┐
     │                    │                    │
  PRIVATE              PUBLIC              BRIDGE
  (libsignal)          (ATProto)           (publish)
     │                    │                    │
  Chats/Mail         Documents             Zine→Post
  Confidential       Pen names             Evidence extract
  Library (local)    (via the member's      Redaction UI
  Notes board        own account)          (optional excerpt)
  (Task 16)
     │                    │                    │
     └────────────────────┼────────────────────┘
                          │
              Local SQLite: cache, FTS, edges, library
              (encrypted at rest under data root)
```

**AI:** assistant reads **local** library/cache/graph only unless user explicitly attaches an external provider.

---

## Milestone index

| Task | Doc |
|------|-----|
| **11** | [`docs/tasks/atproto.md`](../tasks/atproto.md) — the public lane (publish + read) |
| **12 (Phase A)** | [`docs/tasks/knowledge-layer-phase-a.md`](../tasks/knowledge-layer-phase-a.md) |
| **13–15** | This file (Phase B–D); add `docs/tasks/` slices when Phase A is done |
| **16** | [`docs/tasks/notes-dashboard.md`](../tasks/notes-dashboard.md) — private sticky notes (local only) |

---

## Open questions (record, don’t decide now)

- Document subtype for “thesis” vs reusing `site.standard.document` with tags.
- Whether “changed my mind” can ever be published without becoming a popularity game.
- Project objects published to the public lane vs purely local with an optional index record.
- Circles: messaging MLS/group later vs public-only — **DESIGN** favors private rooms on the capsule first.
- AI: on-device model vs user API key vs no AI for v1 of Phase D.

---

## Acceptance criteria (knowledge layer “done” for a phase)

- **Phase A:** User can follow authors, read offline, build argument links, keep private library notes — **no** engagement metrics shown.
- **Phase B:** User can publish a thesis and attach evidence articles without vote counts.
- **Phase C:** User can browse local graph and attach work to a **project** without Hush holding money or a global user directory.
- **Phase D:** Optional audio and local retrieval AI; still no default cloud training.

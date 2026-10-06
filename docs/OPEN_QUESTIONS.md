# Open questions

Handoff list (section 9) is settled. See `DESIGN.md`.

1. ~~First audience~~ → A
2. ~~Threat model~~ → C
3. ~~Identity~~ → C+D
4. ~~Key recovery~~ → D
5. ~~Protocol choice~~ → A libsignal
6. ~~Relay design~~ → B
7. ~~Sharing multiple recipients~~ → B
8. ~~Social layer (later)~~ → C
9. ~~Payments~~ → A
10. ~~Moderation and safety~~ → B
11. ~~Moderation of positioning~~ → A
12. ~~Frontend stack~~ → A Tauri
13. ~~Updates and signing~~ → B

## Knowledge layer (not blocking v1 — decide before Task 12)

Tracked in [`docs/plans/knowledge-layer-roadmap.md`](plans/knowledge-layer-roadmap.md) and [`docs/tasks/knowledge-layer-phase-a.md`](tasks/knowledge-layer-phase-a.md):

- NIP/kind for “thesis” vs tagged NIP-23 articles
- Argument link tags (Hush-specific vs pure `a`/`e` tags only)
- Whether any aggregate “changed my mind” signal may be published without becoming a popularity game
- Project index on Nostr (kind?) vs local-only with optional publish
- Private messaging rooms vs Nostr “circles” — **DESIGN** favors capsule rooms first (Task **15.3**)
- Library AI: on-device model vs user API key vs defer

## Not blocking v1 spec

These can wait until implementation or a later pass:

- Product name and visual identity
- Exact blob size / queue TTL (spec draft uses 1 MiB / 7 days)
- Stripe product details
- Expert review of the blind-queue / sealed-sender transport
- Reproducible builds

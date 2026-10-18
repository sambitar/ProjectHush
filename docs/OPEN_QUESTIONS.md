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

- Document subtype for “thesis” vs tagged `site.standard.document` records
- Argument link tags (Hush-specific vs AT-URI references only)
- Whether any aggregate “changed my mind” signal may be published without becoming a popularity game
- Project index published to the public lane vs local-only with an optional record
- Private messaging rooms vs a public “circle” — **DESIGN** favors capsule rooms first (Task **15.3**)
- Library AI: on-device model vs user API key vs defer

## Postal bridge (not blocking v1)

Tracked in [`docs/plans/hush-postal-bridge.md`](plans/hush-postal-bridge.md):

- Final QR payload layout and ECC level
- Which print/mail provider is primary (compare Lob, PostGrid, Click2Mail on price and certified mail)
- Service return address type (PO box vs registered agent)
- Whether Mode 2 sealed letters use key-in-QR, recipient public key, or passphrase split by default
- Wallet payment flow for per-letter orders and how the gateway verifies payment without learning identity

## Not blocking v1 spec

These can wait until implementation or a later pass:

- Product name and visual identity
- Exact blob size / queue TTL (spec draft uses 1 MiB / 7 days)
- Stripe product details
- Expert review of the blind-queue / sealed-sender transport
- Reproducible builds

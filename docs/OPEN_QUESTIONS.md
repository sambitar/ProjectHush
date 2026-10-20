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

## Postal bridge (not blocking v1 — deferred past production, 2026-10-19)

Tracked in [`docs/plans/hush-postal-bridge.md`](plans/hush-postal-bridge.md). The whole bridge,
Mode 1 included, waits until after **Phase J — Ship** in
[`docs/plans/2026-10-04-v1-milestones.md`](plans/2026-10-04-v1-milestones.md). It needs a gateway,
a print provider, a service return address, payments and abuse controls, so it is production
infrastructure rather than a dogfood feature.

- Final QR payload layout and ECC level
- ~~Which print/mail provider is primary~~ → **Lob** (researched 2026-10-19; figures in the plan's
  Section 6). PostGrid stays as the adapter's swap target; Click2Mail is the certified fallback.
- Service return address type (PO box vs registered agent) — must be an address Hush controls,
  because undeliverable mail returns to the `from` address
- Whether Mode 2 sealed letters use key-in-QR, recipient public key, or passphrase split by default
- Wallet payment flow for per-letter orders and how the gateway verifies payment without learning identity

## Not blocking v1 spec

These can wait until implementation or a later pass:

- Product name and visual identity — **resolved 2026-10-19**: the product is **Hush**; the
  `hushline.app` collision noted 2026-10-15 is accepted. See the launch section below.
- Exact blob size / queue TTL (spec draft uses 1 MiB / 7 days)
- Expert review of the blind-queue / sealed-sender transport
- Reproducible builds

## Launch (blocking v1 — opened 2026-10-19)

Tracked in [`docs/plans/2026-10-19-launch.md`](plans/2026-10-19-launch.md). Launch is a **paid**
public v1 for Linux and macOS, so these are no longer deferrable:

- ~~Stripe product details~~ → product and a **flat $1.99/month** price (WS-4)
- ~~Payments~~ → a signed, offline-verifiable license gating the relay and official updates; the
  vault always opens. **No first-party storage tier** (removed 2026-10-19)
- ~~License binding~~ → bound to `account_id`. The license service holds the Stripe customer next
  to the `account_id`; the relay holds neither
- ~~Founding-price tier~~ → **flat $1.99**, no founding tier
- ~~Product name~~ → **Hush**, at `hushrelay.space`. No rename; the `hushline.app` collision noted
  2026-10-15 is accepted
- Privacy Policy and Terms — required to take money
- macOS notarization — Apple Developer Program enrollment has lead time

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

## Not blocking v1 spec

These can wait until implementation or a later pass:

- Product name and visual identity
- Exact blob size / queue TTL (spec draft uses 1 MiB / 7 days)
- Stripe product details
- Expert review of the blind-queue / sealed-sender transport
- Reproducible builds

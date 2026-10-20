# Hush Postal Bridge — Project Context for Cursor

> **Status:** Future plan / standing context — **not** current scope. Added 2026-10-18.
> **Deferred past production (2026-10-19):** the whole bridge, Mode 1 included, waits until after
> **Phase J — Ship** in [`2026-10-04-v1-milestones.md`](2026-10-04-v1-milestones.md). Mode 1 is the
> only mode with no phone in it, but it still needs a gateway, a print provider, a service return
> address, payments and abuse controls, so it is production infrastructure rather than a dogfood
> feature. The provider research done on 2026-10-19 is recorded in Section 6 and Section 11 so it
> does not have to be redone when the work starts.
> Standing context for every task that touches postal mail, QR payloads, or the Android
> scanner; surfaced to Cursor via [`.cursor/rules/hush-postal-bridge.mdc`](../../.cursor/rules/hush-postal-bridge.mdc).
> If a task conflicts with this file, stop and ask before proceeding.

## 1. What this is

Hush (hushrelay.space) is a local-first, end-to-end encrypted messenger: encrypted local vault, Signal-style ratchets, ciphertext-only relay with rotating queues, optional Tor lane, QR/invite-based trust, anonymous Drops (sealed boxes, up to 8 MiB, no sender key, no reply path), Mail pane between trustees, wallet pane (BTC/ETH). No phone number, no email, no directory.

The **Postal Bridge** adds physical mail: a Hush user can send a real paper letter to a street address, including to people who are not on Hush, while keeping Hush's privacy ethos and its "honest limits" tone (never market anything as anonymous or untraceable).

## 2. The trust boundary (read first)

The relay is deliberately dumb and learns only blob size, timing, and a slot ID. **Postal mail cannot work that way**: something must see a street address, and the print provider and USPS see it too.

Rules that follow from this:

- The postal gateway is a **separate component** from the relay, with its own threat-model entry. Never merge its data model or logs into the relay.
- Gateway data minimization: store only what fulfillment needs, delete content/address/PDF artifacts after the print provider confirms acceptance, retry deletes on failure, keep only minimal transaction identifiers for reconciliation and abuse handling.
- No analytics, no cookies, no telemetry anywhere in the postal flow.
- Return address on every piece is **Hush's service address** (a business PO box or registered-agent address), never the user's. Providers such as Lob require a `from` address, so it cannot be blank.
- Recipient name is optional. Providers generally accept either `name` or `company` (Lob: one of the two is required, free text, max 40 chars). Use a generic addressee like "Current Resident" when no name is given.
- Docs and UI must state plainly: omitting sender details is **not** anonymity. Payment processor, hosting, print provider, and USPS keep their own records and these can be legally compelled.

## 3. The three modes (all three are wanted; build them as distinct features)

### Mode 1 — Plain letter
Gateway sees text/PDF and address, forwards to the print provider, then deletes. Hush service return address. This is the MVP baseline and the simplest path. Support: typed letter or PDF upload, address entry, optional certified mail later.

### Mode 2 — Sealed letter
The printed page carries mostly ciphertext, so the gateway and print provider see only an address plus an opaque blob.
- Short messages: ciphertext is encoded directly in the QR.
- Longer messages: QR holds a relay blob ID plus (optionally) the key; ciphertext lives in the relay and is fetched and decrypted on scan.
- **Key handling decides the protection.** If the key is printed in the QR, the print provider can read it. Real protection against the provider requires either (a) the recipient is already a trustee and the message is encrypted to their public key, or (b) a split secret, with a passphrase shared over another channel. Never claim provider-proof privacy for the key-in-QR variant.

### Mode 3 — Invite / Drop letter
The QR is a Hush trust invite or the sender's Drop key, so a non-user can reply over Tor with no account, no phone number, no reply path tied to a name. Paper becomes an onboarding and first-contact channel (journalist/source use case). This mode reuses existing invite and Drop machinery, so build it first if a first slice is needed.

## 4. QR payload (design requirements, not final)

Design a versioned binary format. Requirements:

- Version byte first; reject unknown versions.
- Type field distinguishing: inline ciphertext, relay blob pointer, trust invite, drop key.
- Authenticated encryption only (AEAD, e.g. XChaCha20-Poly1305); sealed boxes for recipient-public-key cases. Never hand-roll crypto; use libsodium or Tink.
- Explicit length fields and hard size caps. QR binary capacity tops out around 3 KB, so budget for ECC level and keep payloads smaller than the maximum so they scan reliably when printed.
- The decoder treats the QR as **untrusted input**: validate, then decrypt, and fail closed on any error.
- Document where the key lives for each type (in QR, public-key-encrypted, or out-of-band passphrase).

## 5. Android scanner and decrypt

Yes, this is feasible and routine. Requirements:

- Scanning: CameraX with ZXing (fully local, open source). If using ML Kit, use the bundled on-device model, not the Play-services-delivered one. Prefer ZXing for the privacy audience.
- Crypto: libsodium via a binding (e.g. Lazysodium) or Google Tink.
- Keys: private key wrapped by an Android Keystore key (StrongBox where available), gated by biometrics or the vault PIN, mirroring the desktop vault.
- Decrypt in memory only; zero buffers afterward; never write plaintext to disk, logs, or clipboard.
- `FLAG_SECURE` on any screen showing decrypted content.
- A scan must never auto-open links or make network calls. Show the user what the QR is first and require confirmation before any relay fetch.
- Close the camera immediately after a successful decode.
- Distribution: signed builds with a published signature (same philosophy as desktop). F-Droid or direct signed downloads preferred alongside any store listing.
- Out of scope in the threat model: rooted or malware-compromised devices.

## 6. Providers and map/address layer

- Print/mail fulfillment: Lob, PostGrid, or Click2Mail (all offer APIs; compare per-letter price, certified mail support, minimums). Lob's docs confirm `from` is required and `name` or `company` is required on addresses.
- **Primary candidate: Lob** (researched 2026-10-19, from Lob's own table at
  `help.lob.com/print-and-mail/ready-to-get-started/pricing-details`). Rates are all-in per piece —
  print, postage and envelope. First Class B&W letter: **$1.060** on Developer ($0/month, capped at
  500 mailings/month), $0.890 on Startup ($260/month), $0.860 on Growth ($550/month). Each extra
  page $0.10. Certified $6.950 all-in, or $9.860 with Electronic Return Receipt. Return envelope
  $0.04, custom return envelope $0.05. Over 6 sheets adds $2.495. Rates rise slightly on
  2026-11-01 ($1.060 → $1.066 on Developer); certified is unchanged. Developer's break-even against
  Startup is ~1,529 letters/month, so the $0 tier holds for a long time.
- **PostGrid is the credible second source**: Starter $0/month, B&W First Class letter $1.059, extra
  page $0.10, Certified $6.94, Certified + eRR $9.85. Near-identical to Lob on price, which is what
  makes the swappable adapter below worth having rather than aspirational.
- **Click2Mail is the certified-mail fallback only**: no subscription and no minimum, First Class
  letter $1.45 for one sheet (2 pages) plus **$0.75 per additional sheet**, Certified from $6.66.
  Its multi-page pricing is punitive (a 4-page letter is $2.20 against Lob's $1.36) and its catalogue
  is bulk/marketing mail — lists, mail merge, EDDM, presorted rates — which Section 8 rules out.
- **Do not trust aggregator comparison sites for Lob's rate.** frontdeskreview.com and
  customerexperience.io both publish Lob at $0.628–$0.828 labelled "first class". Those are Lob's
  **Standard Class** rows, and Standard Class is marketing mail only — PostGrid's own page says it
  "Must be marketing in nature. Business mails are not allowed." The First Class rate is $1.060.
  Using the wrong number overstates Lob's margin story by roughly 40%.
- **Prior art: MappyMail (mappymail.com) already ships this feature.** Built on **Lob** for print
  and mail production and **Stripe** for payment, from **$1.65** retail per letter — so ~$0.59 gross
  margin over Lob's Developer rate before card fees. It prints its own service return address and
  states the honest-limits line almost verbatim. Its deletion pipeline is a working reference for
  Section 2: content, mailing addresses and PDF artifacts queued for deletion once the print
  provider confirms acceptance; immediate when no confirmation email was requested; 14 days
  otherwise; a 7-day manual-review window when the provider response is uncertain; retries on failed
  cleanup; orphaned uploads deleted at ~24h. It uses a windowed envelope, so a PDF upload gets a
  generated first page carrying the addresses. Its marketing says "anonymous letter", which
  Section 9 forbids; its own terms concede the point.
- Address validation, autocomplete, and reverse geocoding: Lob's Address Verification API includes Reverse Geocode Lookup and autocomplete; alternatives are Smarty, Google, Mapbox, or the USPS Addresses API. Lob's verification rate was **not** verified on 2026-10-19 — it is a separate metered product from the per-piece table.
- Keep provider calls behind a thin adapter interface so the vendor can be swapped.
- Secret provider keys live on the gateway server only. Browser/app code may use only publishable verification keys.

## 7. Payments

The existing subscription links a payment to a name. Prefer **per-letter payment from the vault wallet (BTC/ETH)** for postal features so the postal piece does not deepen that link. Fiat/Stripe checkout can exist as an option, with the linkage disclosed.

## 8. Abuse controls (required, not optional)

Anonymous physical mail to a street address is the most abusable feature in the product. Build from day one:

- Rate limits per IP, per payment method, and per vault/installation identifier.
- One recipient per order; no bulk, no mail merge, no lists.
- A published content policy and the ability to refuse or refund an order.
- Internal order logging sufficient to respond to valid legal process.
- A recipient-side opt-out / block list by address.
- Apartment/unit handling: do not guess missing unit numbers.

## 9. Non-goals

- No claim of anonymity or untraceability, anywhere in copy.
- No identifying residents or property owners from a map click or address.
- No telemetry, ad tech, or third-party analytics.
- No plaintext storage of letters beyond fulfillment and the deletion window.
- No hand-rolled cryptography.

## 10. How Cursor should work on this

- Ask before starting any task that would change the trust boundary, add stored data to the gateway, or add a third-party service.
- When a task touches the QR format, update Section 4 and note a format version bump.
- Keep Mode 1, 2, and 3 as separate modules sharing a common provider adapter and QR codec.
- Prefer small, reviewable steps. Suggested order, **once Phase J has shipped**: QR codec + tests → Mode 3 (invite/drop letter) → Android scan/decrypt prototype → Mode 1 gateway with deletion pipeline → Mode 2 sealed letters. Two caveats on that order, found 2026-10-19: Modes 2 and 3 both end with the recipient scanning on the Android app, which does not exist yet (`apps/` holds only `hush-tauri`; `WORKSPACE.md` says Android is later), so they cannot be demoed end-to-end until it does. Mode 1 is the only mode with no phone in it, and it does not use the QR codec at all.
- Write tests for: payload parsing (malformed, oversized, wrong version), tamper detection, buffer zeroing, and the deletion pipeline.
- Add every new data field or third party to a running threat-model note in the same PR.

## 11. Open decisions

- Final QR payload layout and ECC level.
- ~~Which provider is primary~~ — **Lob**, researched 2026-10-19 (numbers in Section 6). PostGrid
  stays behind the adapter as the swap target; Click2Mail is the certified-mail fallback.
- Service return address type (PO box vs registered agent). Note the constraint found in research:
  undeliverable mail returns to the `from` address, so this must be an address Hush controls and can
  receive returns at — not a borrowed one.
- Whether Mode 2 uses key-in-QR, recipient public key, or passphrase split by default.
- Wallet payment flow for per-letter orders and how the gateway verifies payment without learning identity.

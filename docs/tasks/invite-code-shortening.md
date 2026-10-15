# Invite code shortening — binary serialization

**Status:** Idea captured 2026-10-15; **planning only** — no code, no phase approved yet.

**Index:** Task **25** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md).
Wire: [`docs/PROTOCOL.md`](../PROTOCOL.md) → "Identity" (the invite is an out-of-band artifact:
pasted string or QR, carried once at contact setup).

---

## 1. Problem

The invite string is huge. `InvitePayload::encode`
([`crates/hush-core/src/invite.rs`](../../crates/hush-core/src/invite.rs)) is
`serde_json::to_vec` → URL-safe base64, so the string a member copies, pastes, or encodes as a
QR is **base64 of a JSON document**, and the JSON document is mostly **byte arrays written as
comma-separated decimal integers**:

```json
"identity_key":[8,62,22,186,...],"signed_pre_key_signature":[47,9,201,...],"kyber_pre_key_signature":[...2415 numbers...]
```

Every byte of key material costs ~3.6 characters (`1–3` digits + a comma). The three signature
and key arrays alone are ~3.7 KB of raw bytes → **~13–14 KB of JSON digits**, and ~19–20 KB
after base64. Estimates only — **the first acceptance item below is to measure and record the
real numbers** — but the order of magnitude is the problem: a pasted invite is an unwieldy wall
of characters, and a QR of 19 KB is far past what most phone cameras will reliably scan.

## 2. Proposal (owner's idea, 2026-10-15)

Replace the text-in-text encoding with a **binary serialization** of the same payload:

- **Binary format** — MessagePack, CBOR, or Protocol Buffers. Keys vanish into field tags,
  and `Vec<u8>` serializes as a raw byte string instead of a decimal array. Estimated result:
  ~4 KB payload → ~5.3 KB as base64 text — roughly **a quarter** of today's string, and
  plausibly inside QR range.
- **Raw bytes end to end.** The payload is already binary data (keys, signatures, ids); JSON is
  a text wrapper around it. Base64 stays only as the outer paste-safe skin (the UI transports
  the invite as a string; QR/link need text), but wraps compact binary instead of JSON.
- **Compression, honestly assessed.** zlib/Gzip/Brotli over the *current* payload would win
  little: the bulk is key material and signatures, which are high-entropy and do not compress.
  The win comes from killing the JSON/base64-of-JSON overhead, not from compression. Any
  compression step must justify itself against measured bytes, not vibes.

## 3. Constraints

- **Old clients fail clearly, never wrongly.** `INVITE_VERSION` guards the shape today
  (`Error::UnsupportedInvite`). A new encoding must be *recognisable* — a magic prefix byte or
  version-tagged envelope — so an old build rejects the new string with the same clear error,
  and a new build can tell an old string from a corrupt one. This is the same
  unknown-tag-tolerance discipline the other tasks carry.
- **Short flight window.** An invite is minted on demand and consumed once at contact setup
  (and re-minted on inbox rotation), so there is no long-lived stored artifact to migrate —
  only strings already in flight when someone upgrades mid-handshake.
- **No relay involvement.** The relay never sees the invite; this is purely the out-of-band
  artifact and the two `encode`/`decode` functions.

## 4. Open decisions

- **Format:** `rmp-serde` (MessagePack) and `serde_cbor` are drop-in serde swaps; protobuf
  buys explicit field numbering and cross-language stability at the cost of codegen and a
  second schema language. Lean MessagePack/CBOR for the serde round-trip; decide after
  measuring both.
- **Field-level shrinking** (optional, same slice or a follow-up): `protocol_name` as an enum
  tag; `relay` as a short alias of a known-relay table with the full URL only for unknown
  relays; drop-or-default fields that are constant today (`device_id` is always 1).
- **Outer skin:** URL-safe base64 (no change) vs base45 (QR-alphabet-optimised) — depends on
  whether QR is rendered from the string.
- **Target size:** a number to aim at, set after the measurement step.

## 5. Verification (planned)

1. **Measure first:** add a round-trip test that mints a real invite (all prekeys populated)
   and asserts the encoded length, recording the JSON and binary numbers in this doc.
2. Round-trip tests for the new codec, including: old-format string decodes, new-format string
   decodes, truncated/corrupt input → `Error::Corrupt`, wrong version → `Error::UnsupportedInvite`.
3. Existing suites green: `cargo test -p hush-core`, `cargo test -p hush-daemon` (invite routes),
   `npx tsc --noEmit` (no UI change expected).
4. Manual: copy an invite from the app, confirm the paste-and-add flow still completes against
   a fresh vault.

## 6. Depends on / independent of

**Depends on:** nothing new — Task **5** (daemon), **5d** (mail, handle/vault fields), Task **6**
(device transfer, also paste-based). **Independent of** the media and publishing tasks.

**Distro-readiness:** one new runtime dependency (a small serializer crate) is the thing to
weigh; no new egress, no relay change, no new durable state.

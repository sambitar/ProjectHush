# Trustee reach — mail-only or full (mail + chat + conclave)

**Status:** Built 2026-10-15. Design captured 2026-10-14; the open questions in §5 were decided during
implementation and are recorded below rather than left dangling.

**Index:** Task **24** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md)
(Phase **R**). Wire summary: [`docs/PROTOCOL.md`](../PROTOCOL.md) → "Trustee reach".

**Vocabulary:** in Hush a **trustee** is a person in your circle (the Trusts pane); a conclave's
trustees are participants drawn from that circle. One word, one idea. This task is about the
**circle** relationship, not about a conclave's internal roster.

---

## 1. Goal

When you add a trustee, you choose **which surfaces that trustee may use with you**. Two options:

| Option | Mail (async) | 1:1 chat (live) | Conclaves |
|---|---|---|---|
| **Mail only** | yes | no | no |
| **Full** (default) | yes | yes | yes |

Mail is the floor: every trustee can always reach you by mail. **Mail only** means exactly that —
no live 1:1 chat and no conclave together. **Full** is today's behaviour and stays the default, so
nothing changes for anyone who does not touch the setting.

**Why:** some members want to add people they only want to correspond with by mail — a professional
contact, a one-off introduction — without opening a live chat thread or pulling them into a
conclave. The app previously offered all three surfaces to every trustee the moment they were added.

**Non-goals:** outsiders / inbound drops (Task **5e** / **5f**) have their own address book and are
out of scope; the scope is between two members. No third surface beyond the two options above; no
per-conclave scope (a conclave trustee is either reachable for conclaves or not, as a property of the
circle relationship).

---

## 2. Where the setting lives

- **On the contact record**, beside the nickname and account id — the same place "Change nickname"
  already edits. Set in the add-trustee flow, changeable later from the trustee's menu.
- Two fields, one per side: `Contact.reach` is **this vault's own choice**; `Contact.peer_reach` is
  **the peer's announced choice** (`full` until they announce otherwise, so an old peer or one that
  has never announced is never mistaken for mail-only).
- **Announced to the peer** as `MessagePayload::Reach { scope }`, so both ends agree and a UI can
  render the same way.

**Effective reach is the narrower of the two sides.** If either party chose **mail only**, the
relationship is mail-only for both. Neither side can force a surface the other excluded.

---

## 3. Enforcement (both ends, not just a hidden button)

The rule holds at the core boundary, the way the convener-only invite rule does — a hidden button is
not a rule. Every row below is built and covered by a test.

| Surface | Mail-only behaviour | Where |
|---|---|---|
| Trusts pane menu | "Chat" is absent; "Email" stays. The reach toggle is the control that would re-open it. | `apps/hush-tauri/src/app.ts` `trustRow` |
| Chats rail | Not listed at all: there is no chat to open, so a row for it could only reach a composer notice. It keeps its place in Trusts and in Mail, and widening the reach puts the row — and its history — back. An open chat that narrows is closed rather than left without a row to return to. | `renderConvList`, `chatContacts` |
| Command palette | The entry reads "Email X" and opens the composer, instead of "Open X", which would open a chat the boundary refuses. | command palette |
| File cards | "View in chat" is absent for a chat file whose trust has since narrowed; "View in mail" is unaffected. | files pane card |
| Chat pane empty state | Names the reason when every trust is mail only, rather than inviting a first chat. | `renderMainChat` |
| Conclave create / invite pickers | A mail-only trustee is not offered as a candidate. | `renderConclaveCreateModal`, `renderConclaveInviteModal` |
| Conclave member chip | "Chat directly" is absent; "Send mail" stays. | conclave trustee menu |
| Trusts pane row | A badge says why the actions are gone, and the wording follows the *effective* reach. Ours reads "Mail only" — the same words as the picker and the toggle, so the reader is never told their own setting is the one holding the trust back. The peer's narrowing reads "Offline": being restricted is not ours to be told, and the row already shows them with no presence dot, because a mail-only trust has no chat to be online for. | `trustRow`, `.trust-reach-badge` |
| Chat pane composer | Replaced by a hint naming the trust as mail only and pointing at the trust menu; a dropped file is refused with the same copy. | `renderComposer`, `sendText`, `sendFile` |
| Presence | No live-chat presence dot, because there is no chat surface to be present for. | `trustRow` |
| Outbound chat | `send_text` / `send_file` / `send_file_bytes` refuse with `Error::ReachMailOnly`. | `Account::send_payload` |
| Outbound conclave | `create_conclave` and `invite_conclave_trustee` refuse that trustee with `Error::ReachMailOnly`. | `account.rs` |
| Inbound chat / file | Refused at `recv` with a warning, not filed as if it were mail. | `Account::recv` |
| Inbound conclave invite | Refused before it becomes a pending invite, with a warning. | `Account::recv` |
| Daemon API | The refusals surface as `400` with the core's message, so CLI and UI get the same answer. | `map_err` in `hush-daemon` |

Mail rides the same 1:1 Signal session as chat, so this is a **policy** gate, not a transport one:
the bytes are deliverable either way, and the app chooses not to offer or accept the surface.

---

## 4. Wire

`MessagePayload::Reach { scope: TrusteeReach }`, with `scope` serialized as `"mail_only"` / `"full"`.

- **A control payload**, handled above the message dispatch in `recv` and listed in every
  `unreachable!`/ignore arm alongside `QueueGrant` and `ZineGrant`, so it never renders as an inbox item.
- **Applied only from a known contact** — the 1:1 session is the authentication. A stranger's
  announcement is dropped.
- **Store-and-forward**, so an away peer learns the choice on their next drain; the local choice stands
  even if the announcement cannot be delivered (the next change re-sends). The announcement is
  **queued in the vault, not delivered inline**: `set_contact_reach` records it in `pending_reach` and
  returns, and the daemon's outbox timer (or `POST /api/outbox/retry`) drains it like queued mail —
  a slow relay must not hold the user's toggle for three 15-second delivery attempts.
- **No signature.** It can only ever narrow, so a forged announcement can at worst hide a surface the
  sender was not going to use.
- Unknown-tag tolerance is the shared discipline the other tasks already carry: a build that does not
  know `reach` decodes it to `Unknown` and ignores it, and a contact's `reach`/`peer_reach` default to
  `full` when the field is absent.

---

## 5. Decisions (the questions §5 of the original note left open)

- **Intersection, not one-way.** The narrower reach wins. The alternative — the setting limits only
  what *you* offer while the peer keeps their own view — was rejected because it lets a peer open a
  surface the other side refused.
- **Mail-only does not gate mail attachments.** Mail is mail, attachments included; the live surfaces
  are what the setting removes.
- **Changing the scope mid-relationship is allowed both ways and announced.** Narrowing takes effect
  immediately (including closing a chat, which is just the next send being refused); widening is
  announced but may still be capped by the peer's own choice.
- **Existing conclave membership is grandfathered.** Narrowing stops new chats and new conclave joins;
  it does not retroactively remove a trustee from a conclave they are already in. A kick is the removal
  path. A silent drop from the fan-out would be worse than the inconsistency.
- **Copy.** The two *states* read "Mail only" and "Mail, chat and conclaves"; the modal asks "How can
  they reach you?"; the badge is "Mail only"; the refusal is "this is a mail-only relationship — chat
  and conclaves are off"; the empty chat rail says "Everyone you know is mail only."
- **Copy, the toggle.** A menu row is a command, so both directions of the reach toggle are imperatives
  naming what the click does: "Restrict to mail only" (lock icon) narrows, "Allow chat and conclaves"
  (chat icon) widens. The narrowing row deliberately does not say "Mail only", which is the badge's
  wording — a state name in a command slot reads as a status report, and two meanings for one phrase in
  one row is what made it look wrong. It takes the lock rather than the mail icon, which the Email entry
  directly above it already owns.
- **Naming.** "Reach" is the internal noun; the UI avoids it in favour of the plain question and the
  option labels, because "reach" is our word, not the user's.

---

## 6. What landed

- `crates/hush-core/src/payload.rs` — `TrusteeReach` (`narrow`, `allows_live`) and
  `MessagePayload::Reach`.
- `crates/hush-core/src/account.rs` — `Contact.reach` / `Contact.peer_reach` (with the legacy and
  devices deserialize arms defaulting to `full`), `Account::set_contact_reach`, the private
  `effective_reach` / `effective_reach_for_account`, the gates in `send_payload`, `create_conclave`,
  `invite_conclave_trustee`, and the receive-path handling.
- `crates/hush-core/src/error.rs` — `Error::ReachMailOnly`, `Error::InvalidReach`.
- `crates/hush-daemon` — `ContactDto.reach` (effective) and `.my_reach` (own choice),
  `AddContactRequest.reach`, `POST /api/contacts/reach`, `Work::SetContactReach`, and the
  `to_dto` ignore arm.
- `crates/hush-cli` — `add-contact --mail-only` and a `set-reach --reach full|mail-only` subcommand.
- `apps/hush-tauri` — `TrusteeReach` type, `api.addContact(..., reach)` and `api.setContactReach`,
  the reach picker in the add-trust modal, the trust-row badge and menu toggle, the chat composer
  replacement for a mail-only trust, and the chat/conclave gating in the trust row, the conclave
  member chip, and both conclave pickers. `chatContacts()` is the single answer to "who can be
  chatted with": the rail, the command palette, the file cards and the default selection all read it
  rather than filtering contacts for themselves.

**Verification:** `crates/hush-core/tests/reach.rs` (7 tests), `reach_api_tests` in
`crates/hush-daemon/src/main.rs` (3 tests), plus the `payload.rs` unit tests for `narrow` and the
round-trip. Full suites green: `cargo test -p hush-core` (all), `cargo test -p hush-daemon` (216),
`npm test` in `apps/hush-tauri` (78), `npx tsc --noEmit` clean.

**Verified by hand** on the two-vault dev stack (Vite on `:5173`, daemons on `:9886`/`:9887`): with
Alice at mail only from Bob's side, her row leaves the Chats rail, the rail and the pane both name
the reason, and the palette entry becomes "Email alice"; widening back to full restores the row and
the "Open alice" entry.

---

## 7. Depends on / independent of

**Depends on:** Task **5** (daemon), **5d** (mail), **5h** (presence), conclave fan-out (done),
**5c** encrypted store (done). **Independent of** **17** / **18** / **19** / **23** media work —
it is a contact-policy change with no new transport.

**Distro-readiness:** no new runtime deps and no new egress; the reach is a field in the existing
encrypted store, and the announcement rides the existing session.

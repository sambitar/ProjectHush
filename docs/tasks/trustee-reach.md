# Trustee reach — mail-only or full (mail + chat + conclave) — planning only

**Status:** Idea captured 2026-10-14. **Planning only**, not grilled to decision level. No code until
a phase is approved.

**Index:** New **Task 24** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md)
(Phase **R**).

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
conclave. The app currently offers all three surfaces to every trustee the moment they are added.

**Non-goals:** outsiders / inbound drops (Task **5e** / **5f**) have their own address book and are
out of scope; the scope is between two members. No third surface beyond the two options above; no
per-conclave scope (a conclave trustee is either reachable for conclaves or not, as a property of the
circle relationship).

---

## 2. Where the setting lives

- **On the trustee record**, beside the nickname and account id — the same place "Change nickname"
  already edits. Set in the add-trustee flow, changeable later from the trustee's menu.
- **Announced to the peer**, so both ends agree on the reach and a UI can render it the same way.
  The scope is a property of the *relationship*, not a private local preference: a mail-only
  trustee must also not have their chat and conclave attempts land silently.

**Effective reach is the narrower of the two sides.** If either party chose **mail only**, the
relationship is mail-only for both. Neither side can force a surface the other excluded. This is the
consent-shaped default and the one to build unless grilling says otherwise (Section 5).

---

## 3. Enforcement (both ends, not just a hidden button)

The rule must hold at the daemon boundary, the way the convener-only invite rule does — a hidden
button is not a rule.

| Surface | Mail-only behaviour |
|---|---|
| Trusts pane menu | "Chat directly" and any "Add to conclave" action are absent (or present but disabled with a reason). Mail actions stay. |
| Conclave create / invite pickers | A mail-only trustee is not offered as a candidate; if listed, disabled with a reason ("mail only"). |
| Chat pane | Opening a thread for a mail-only trustee is refused with copy; an existing thread does not silently start. |
| Inbound chat / conclave payloads | A well-behaved peer never sends one, because it knows the scope. A peer that does is refused at `recv` with a notice, not stored as if it were mail. |
| Presence | A mail-only trustee shows no live-chat presence dot, since there is no chat surface to be present for. |
| Daemon API | `send_conclave_message` / chat send / `create_conclave` / `invite_conclave_trustee` refuse a mail-only trustee with a distinct error, so CLI and UI both get the same answer. |

Mail (5d) rides the same 1:1 Signal session as chat, so this is a **policy** gate, not a transport
one: the bytes are deliverable either way, and the app chooses not to offer or accept the surface.

---

## 4. Wire

The scope has to travel so both ends agree. Two candidate shapes, decided at grilling:

- a field on the existing contact/introduce/accept payload, or
- a small `MessagePayload::Reach { scope }` control payload, applied only from the named peer, and
  re-sent when the scope changes.

Whichever lands, unknown-tag tolerance is the same shared change the other tasks already carry.

---

## 5. Open questions (record, do not decide now)

- **Intersection vs one-way.** The default above is intersection (narrower wins). Alternative: the
  setting only limits what *you* offer, and the peer keeps their own view — rejected unless the user
  asks for it, because it lets a peer open a surface the other side refused.
- **Does mail-only gate mail *attachments* and file sharing**, or only the live surfaces? Current
  stance: mail is mail, attachments included.
- **Changing the scope mid-relationship.** Is a move from Full to Mail-only allowed to close an open
  chat, and does it need to be announced (a `Reach` re-send) rather than silently local?
- **Copy** for the two options and the refusal messages (the house voice, not "blocked").
- **Naming** of the setting in the UI ("Reach", "How they can reach you", "Mail only").

---

## 6. Depends on / independent of

**Depends on:** Task **5** (daemon), **5d** (mail), **5h** (presence), conclave fan-out (done),
**5c** encrypted store (done). **Independent of** **17** / **18** / **19** / **23** media work —
it is a contact-policy change with no new transport.

**Distro-readiness:** no new runtime deps and no new egress; the scope is a field in the existing
encrypted store, and the announcement rides the existing session.

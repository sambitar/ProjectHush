# Trustee signals — the non-chat sideband

**Status:** Built **2026-10-20**. Infrastructure only: the envelope, policy, expiry, acks and audit
are live; `Ping` is the reference kind. Feature kinds are added as `SignalBody` variants.

**Index:** Task **26** in [`docs/plans/2026-10-04-v1-milestones.md`](../plans/2026-10-04-v1-milestones.md).
Wire summary: [`docs/PROTOCOL.md`](../PROTOCOL.md) → "Trustee signals".

**Vocabulary:** a **trustee** is a person in your circle. A **signal** is a typed payload that rides
the same 1:1 session as chat and mail but is never rendered as either. "Signaling" here is the
product's sideband, not WebRTC call signalling (which still uses its own payload variants).

---

## 1. Goal

Chat and mail are for people. Some traffic between trustees is not a conversation at all: a wallet
address, a torrent handoff, a "look at this", a future payment request. Embedding those in invite
codes, chat lines, or ad-hoc payload variants makes every feature invent its own transport.

This task builds **one transport** for all of them:

- a typed envelope with a stable id, optional expiry and an optional acknowledgement;
- a per-contact on/off policy, on by default;
- duplicate suppression and a bounded audit log;
- a daemon API and a trust-menu toggle;
- unknown-kind tolerance, so an older build ignores what it cannot read instead of failing.

**Non-goals:** no new relay, no new queue, no new encryption, no background scheduler. A signal is
one more encrypted blob in the existing store-and-forward queue. The layer carries **data**, never a
command: a kind that changes state (a download, a payment) must ask the user by its own rules.

---

## 2. Wire

```text
MessagePayload::Signal {
    id: String,              // UUID chosen by the sender: dedupe + ack correlation
    sent_ms: Option<u64>,    // advisory, Unix ms
    expires_ms: Option<u64>, // absolute deadline; absent = durable
    ack: bool,               // ask for SignalAck
    body: SignalBody,
}

MessagePayload::SignalAck { id: String, status: SignalStatus }

SignalBody = { "kind": "ping", "note"?: "…" } | <unknown kind>
SignalStatus = delivered | unsupported | expired | denied | failed | unknown
```

- **Internally tagged body** (`kind`). A build that does not know a kind decodes it as
  `SignalBody::Unknown`, acks it `unsupported` and applies nothing. The existing unknown-top-level-tag
  discipline is unchanged.
- **Control payload.** Both `Signal` and `SignalAck` are consumed above the message dispatch in
  `recv` and listed in the `to_dto` ignore arms. Neither ever becomes a chat or mail row.
- **Store-and-forward.** Signals use the mail path (`require_online = false`), so an offline trustee
  gets them on their next drain. They are **not** gated by trustee reach: a mail-only relationship has
  no chat, but it still has a sideband.
- **Size caps.** `MAX_SIGNAL_BYTES = 16 KiB` for the encoded payload, well below the relay's 1 MiB
  blob cap. A `Ping` note is capped at `MAX_PING_NOTE_BYTES = 4 KiB`. Large bytes belong on the file
  or torrent path.
- **Ordering is not promised.** The relay queue gives at-least-once delivery; the envelope gives
  dedupe and expiry, not sequencing. A kind that needs order carries its own sequence.

---

## 3. Policy and enforcement

| Rule | Where |
|---|---|
| Signals are allowed for every trustee by default. | `SignalPolicy::default()` → `Allow` |
| A contact may be switched to `Deny`; the choice is local and unannounced. | `Contact.signals`, `POST /api/contacts/signals`, Trusts row menu |
| `Deny` refuses outbound sends. | `Account::send_signal` / `send_signal_ack` → `Error::SignalsDisabled` |
| `Deny` drops inbound signals **silently**, with no ack, so the peer cannot probe it. | `Account::recv` |
| A signal from a sender with no contact edge is dropped. | `Account::recv` |
| An expired signal is dropped; a requested ack says `expired`. | `Account::recv` |
| A repeated `(sender, id)` is not applied twice; a requested ack is re-sent. | `Account::recv` + bounded `signal_log` |
| A known kind is handed to the caller; an unknown kind is audited and dropped. | `InboxResult::signals` |
| The policy survives a restart. | `AccountState` serialization + `signal_policy_survives_reopen` |
| Removing or revoking a contact drops its audit records. | `remove_contact` / `revoke_contact` |

The session is the authentication. There is deliberately no second signature: a signal comes from a
contact the vault already pinned, and the envelope never confers authority (unlike `QueueGrant`,
whose signature names a queue the recipient will deliver into).

---

## 4. Audit and visibility

"Not in the chat stream" must not mean "not inspectable". Every accepted signal adds a
`SignalRecord` to `AccountState::signal_log`:

```text
{ id, from_account, from_nick, kind, received_ms, expires_ms?, supported }
```

- **Metadata only.** The body is not stored: a wallet address or a future secret should not become a
  second archive. The log answers *who sent what kind, when*.
- **Bounded.** `MAX_SIGNAL_LOG = 256`; the oldest records are pruned. The log is also the dedupe
  window, not a permanent ledger.
- **Per contact.** `Account::signals_for(nickname)` returns newest first; `GET
  /api/contacts/{nickname}/signals` exposes it to the shell.
- **Acks are not logged.** They are handed to the caller as `InboxResult::signal_acks` and are
  correlated by the sender's own pending state.

---

## 5. What landed

- `crates/hush-core/src/signals.rs` — `SignalPolicy`, `SignalStatus`, `SignalBody`, `SignalRecord`,
  `ReceivedSignal`, `ReceivedSignalAck`, caps and `prune_signal_log`.
- `crates/hush-core/src/payload.rs` — `MessagePayload::Signal` and `SignalAck`.
- `crates/hush-core/src/account.rs` — `Contact.signals`, `AccountState.signal_log`,
  `send_signal`, `send_signal_ack`, `set_contact_signals`, `signals_for`, the receive branches,
  dedupe/expiry/policy handling, and audit pruning on removal.
- `crates/hush-core/src/message.rs` — `InboxResult.signals` and `.signal_acks`.
- `crates/hush-core/src/error.rs` — `SignalsDisabled`, `InvalidSignal`, `SignalTooLarge`.
- `crates/hush-core/src/signal.rs` → `signal_protocol.rs` — renamed so "signal" names the feature,
  not the libsignal wrapper.
- `crates/hush-daemon` — `ContactDto.signals`, `Work::{SendSignal, SetContactSignals,
  ContactSignals}`, `POST /api/signals/send`, `POST /api/contacts/signals`,
  `GET /api/contacts/{nickname}/signals`, and signals/acks on `RecvResponse`.
- `apps/hush-tauri` — `SignalPolicy`, `SignalBodyDto`, `SignalRecordDto`, `ReceivedSignalDto`,
  `ReceivedSignalAckDto`, the API methods, the trust-menu toggle, and `signals` in
  `trustRowShape`.

**Verification:** `crates/hush-core/tests/signals.rs` (7 end-to-end tests: out-of-band delivery +
ack, optional ack, expiry, deny, mail-only, reopen, unknown refuses locally), the `signals.rs` unit
tests (round-trip, unknown kind, policy/status fallbacks, caps, pruning), `cargo test -p hush-core
--lib` (164), `cargo check -p hush-daemon`, `npx tsc --noEmit`, and `npm test` in
`apps/hush-tauri` (281).

---

## 6. Decisions

- **On by default.** Signals are part of the trustee relationship, not a separate opt-in. The menu
  narrows rather than grants.
- **Deny is silent.** An ack would reveal that the recipient read the signal. Silent drop is the
  least information.
- **Unknown kinds are acked `unsupported`.** The sender learns the peer's build is older without the
  receiver pretending to have applied it.
- **Metadata-only audit.** Bodies can be sensitive; the log is for inspection, not replay.
- **Absolute expiry, Unix ms.** A relative TTL would be re-interpreted every time a queued signal is
  retried.
- **No auto-execution.** The envelope has no `execute` concept. A kind that changes state must ask
  the user; the layer only delivers.
- **The bounded log is the dedupe window.** The session already rejects replayed ciphertext, so a
  replay older than 256 signals is not a new exposure.

---

## 7. Kinds to build next

These are deliberately **not** in this task:

1. **Wallet address** — durable, change-warning before the next payment.
2. **Torrent handoff** — durable offer, ask by default, ack accepted/declined.
3. **Payment request / receipt** — chain, fresh address, amount, txid notification.
4. **Contact card refresh** — mail handle, relay, vault name in place of a new invite.
5. **Ping UI** — the reference kind is wire-complete but has no user surface yet.

Each is a `SignalBody` variant plus the feature that applies it. The transport does not change.

---

## 8. Depends on / independent of

**Depends on:** Task **5** (daemon), **5c** (encrypted store), **5h** (presence, only for future
ephemeral kinds). **Independent of** reach, conclaves and the media plane: a signal is one more
control payload on the existing session, and the relay learns nothing new.

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

SignalBody = { "kind": "ping", "note"?: "…" }
           | { "kind": "wallet_address", "bitcoin"?: "…", "ethereum"?: "…" }
           | <unknown kind>
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

## 5. Wallet addresses — the first durable kind

`SignalBody::WalletAddress { bitcoin, ethereum }` carries the sender's receive addresses. It is the
first **core-applied** kind: `recv` writes it onto the contact record rather than handing it to the
daemon, because the contact record is the destination.

- **Stored on the contact:** `wallet_bitcoin`, `wallet_ethereum`, and `wallet_updated_ms`. A later
  announcement replaces the previous one; an absent chain clears that chain rather than keeping a
  stale address.
- **Announced:** automatically after wallet create/restore (in the background, so a slow relay does
  not hold the response), and on demand from the wallet's "Share with trustees" action
  (`POST /api/wallet/announce`). Contacts that denied signals are skipped.
- **Change warning:** the send card shows who shared the address and when, and tells the user to
  confirm it. The transfer is irreversible, so the UI must never make a changed address look like
  the old one.
- **USDC is already supported.** It is an ERC-20 token the wallet tracks and sends through the
  existing token path; this signal supplies the destination address. No Circle integration, no
  custodian, no third-party key.

**Payment consent.** Wallet addresses are useful only for money, so they are gated by a second
per-contact choice: `Contact.payments` (`PaymentPolicy`, **deny by default**). Enabling it is the
only way the wallet's "Share with trustees" announcement includes that contact, the only way an
inbound `WalletAddress` is stored, and the only way `/api/wallet/pay-trustee` will run. Turning it
off clears the stored address. Both sides must enable it for an address to cross.

**The chat action.** When payments are on, the composer's `+` offers **Send USDC** beside **Attach
file**, and `/pay` or `/pay <amount>` in the composer opens the same dialog. The payment uses the
contact's announced Ethereum address and the network's canonical USDC contract (mainnet or Sepolia);
other assets stay in the wallet pane. The daemon resolves the address and contract, so the UI never
invents a destination. The dialog is **two steps**: amount, then a confirmation that restates the
amount, recipient address and network.

**The chat line.** A completed payment is a `ChatRecordPayload::Payment` record, drawn as a centered
system line with its transaction hash ("You sent 1 USDC to Alice" / "You received 1 USDC from
Alice"). The sender records its own line after broadcast. The recipient's line is written only
after its daemon verifies the chain (`SignalBody::Payment` carries the txid and amount): mined and
successful, the network's USDC contract, the recipient's own address, and at least the claimed
amount. Pending transactions retry with backoff for 15 minutes; a refusal is logged and dropped.
`RecvResponse.chat_appended` tells the UI to re-read its history, because the line is not one of
`recv`'s messages.

## 5a. Torrent sharing — the second durable kind

`SignalBody::TorrentOffer { offer_id, name, hash, size }` offers one torrent from the sender's
Torrents library. The content is never sent: the recipient's own TorBox account fetches it. The
recipient's daemon stores the body in `torrents-state.enc` (the signal audit log is metadata-only),
shows a confirmation, and only on accept builds `magnet:?xt=urn:btih:<hash>&dn=<name>` and queues an
ordinary TorBox item. `TorrentResponse { offer_id, accepted }` tells the sender.

- **Two-sided, deny by default.** `Contact.torrents` is the local switch and `peer_torrents` is the
  announced one; `TorrentConsent { allow }` carries a change. Both must allow before
  `POST /api/torrents/share` or an accept route will run. Unlike payments there is no optimistic
  default: a peer that never sent consent has not opted in.
- **No auto-download.** The signal is data, not a command. The recipient sees the name and size and
  chooses; a missing TorBox key leaves the offer pending and the Download button disabled.
- **No trackers from a peer.** The daemon builds a hash-only magnet, so a peer cannot smuggle a
  tracker list or a different source into the offer. A private-tracker torrent may not resolve,
  because the library retained only the hash; the UI states that limit.
- **The UI.** `/torrent` and the trust menu's **Share a torrent** open the picker; the `+` menu
  offers it when both sides allow. Incoming offers open a modal and are also stored, so one that
  arrived while the app was away reappears on the next unlock.

## 6. What landed

- `crates/hush-core/src/signals.rs` — `SignalPolicy`, `SignalStatus`, `SignalBody` (`Ping`,
  `WalletAddress`, `Unknown`), `SignalRecord`, `ReceivedSignal`, `ReceivedSignalAck`, caps and
  `prune_signal_log`.
- `crates/hush-core/src/payload.rs` — `MessagePayload::Signal` and `SignalAck`.
- `crates/hush-core/src/signals.rs` also carries `PaymentPolicy` (allow/deny, deny by default).
- `crates/hush-core/src/account.rs` — `Contact.signals` and `Contact.payments`,
  `Contact.wallet_bitcoin` / `.wallet_ethereum` / `.wallet_updated_ms`, `AccountState.signal_log`,
  `send_signal`, `send_signal_ack`, `set_contact_signals`, `set_contact_payments`, `signals_for`,
  the receive branches, dedupe/expiry/policy handling, the core application of `WalletAddress`,
  clearing addresses when payments are turned off, and audit pruning on removal.
- `crates/hush-core/src/message.rs` — `InboxResult.signals` and `.signal_acks`.
- `crates/hush-core/src/error.rs` — `SignalsDisabled`, `InvalidSignal`, `SignalTooLarge`.
- `crates/hush-core/src/signal.rs` → `signal_protocol.rs` — renamed so "signal" names the feature,
  not the libsignal wrapper.
- `crates/hush-daemon` — `ContactDto.signals`, `.payments` and the wallet-address fields,
  `Work::{SendSignal, SetContactSignals, ContactSignals, SetContactPayments}`,
  `POST /api/signals/send`, `POST /api/contacts/signals`, `POST /api/contacts/payments`,
  `GET /api/contacts/{nickname}/signals`, `POST /api/wallet/announce`,
  `POST /api/wallet/pay-trustee` (records the sender's line and sends the notice),
  `Work::RecordPayment`, `RecvResponse.chat_appended`, the pending-notice verifier with backoff,
  automatic announcement after create/restore (payment-enabled trustees only),
  `PublicState.ethereum_network`, and signals/acks on `RecvResponse`.
- `apps/hush-tauri` — `SignalPolicy` / `PaymentPolicy`, the signal DTOs, the API methods, the trust
  menu's signals and payments toggles plus the "Payments allowed" badge, the chat composer's `+`
  menu (Send USDC / Attach file), the `/pay` and `/pay <amount>` composer command, the two-step
  payment dialog, the payment system line and its styles, the trustee picker in the wallet send
  card, and the "Share with trustees" action.
- **Torrent sharing (2026-10-21).** `SignalBody::TorrentOffer`, `TorrentResponse` and
  `TorrentConsent`; `TorrentPolicy` on contacts (deny by default both ways); `set_contact_torrents`;
  the torrents sidecar's offer list; `POST /api/torrents/share`, `/offers/{id}/accept` and
  `/decline`; the `/torrent` command, picker, offer modal, trust-menu toggle and badge.

**Verification:** `crates/hush-core/tests/signals.rs` (9 end-to-end tests: out-of-band delivery +
ack, optional ack, expiry, deny, mail-only, reopen, unknown refuses locally, wallet-address
replacement, and an address dropped when payments are off), the `signals.rs` unit tests
(round-trip, unknown kind, policy/status fallbacks, caps, pruning, wallet-address cap, payment
validation), `chat_log.rs` payment dedupe, `cargo test -p hush-core` (167 lib), `cargo test -p
hush-daemon` (227), `cargo check --workspace`, `npx tsc --noEmit`, and `npm test` in
`apps/hush-tauri` (282).

---

## 7. Decisions

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

## 8. Kinds to build next

These are deliberately **not** in this task:

1. ~~**Wallet address** — durable, change-warning before the next payment.~~ **Built 2026-10-20.**
2. ~~**Torrent handoff** — durable offer, ask by default, ack accepted/declined.~~ **Built 2026-10-21.**
3. **Payment request / receipt** — chain, fresh address, amount, txid notification.
4. **Contact card refresh** — mail handle, relay, vault name in place of a new invite.
5. **Ping UI** — the reference kind is wire-complete but has no user surface yet.

Each is a `SignalBody` variant plus the feature that applies it. The transport does not change.

---

## 9. Depends on / independent of

**Depends on:** Task **5** (daemon), **5c** (encrypted store), **5h** (presence, only for future
ephemeral kinds). **Independent of** reach, conclaves and the media plane: a signal is one more
control payload on the existing session, and the relay learns nothing new.

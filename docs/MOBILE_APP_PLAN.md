# Mobile app plan

**Status:** draft, 2026-10-21. The top section is the product intent. The addendum below records the
code-level constraints, the consequences of the settled Places model, and the decisions still open.
Where the two disagree, the addendum wins until the decision is taken.

## Scope

- Determine exactly what moves into the mobile app.
- We can create other types of experiences beyond a straight port.
- Chats: yes, of course.
- Payments: included within the chats.
- Torrents: no.
- Attachments: yes.
- Conclave: instead of moving the whole Conclave, allow users to go to Contacts / Trustees and press "Call".



## Ethereum / USDC

- Ethereum is only involved to enable USDC payments.
- On first launch of the new Android app, prompt the user: "Do you have the desktop app?"
  - If yes: the user provides their invite, so the Android app uses the same ETH address as the desktop.
  - The Android device is then added as a place (linked to the same identity).

---

# Addendum — 2026-10-21

Governing docs: [`2026-10-18-android-linking.md`](plans/2026-10-18-android-linking.md) (Places,
D1–D17), [`PROTOCOL.md`](PROTOCOL.md) § "Trustee signals",
[`2026-10-16-windows-release.md`](plans/2026-10-16-windows-release.md) (the wallet is
Unix-socket-only), and [`tasks/member-calls.md`](tasks/member-calls.md) (1:1 calls).

**The three corrections that matter most:**

1. An invite carries no wallet, and a Place does not share a vault key. "Same ETH address" requires
   restoring the same phrase (or accepting a separate address).
2. The phone is a separate place: it does not inherit the desktop's chats, contacts, or history.
3. USDC still needs ETH for gas, so the phone is not "USDC only". Mainnet and Sepolia are the only
   supported networks today.

## 1. Three things the draft currently conflates

The draft says an invite gives the phone "the same ETH address" and makes it "a place linked to
the same identity". Those are three different mechanisms in the code, and none of them implies the
others.

| Thing | What it actually is | What it does **not** give the phone |
|---|---|---|
| **Invite** | A one-time code/QR that adds one identity as a contact | No keys, no vault, no history, no wallet |
| **Place / vouch** | A **separate identity and vault**, plus a two-signature claim that it belongs to the same person (`crates/hush-core/src/place.rs`, D1/D6) | No shared master key, no shared identity, no shared chats, no shared wallet (D2/D4) |
| **Wallet** | One BIP39 phrase sealed in a vault, deriving BTC and ETH (`crates/hush-wallet/src/seed.rs`) | It is not carried by an invite or a vouch |

So the flow as written cannot work: **the invite carries no wallet material, and Places deliberately
do not share a vault key.** Two things must be chosen separately:

1. Is the phone **its own place** (separate identity, separate chats, separate wallet) or the
   **same device/identity** as the desktop (the multi-device model that D shelved)?
2. Does the phone have **its own wallet**, or the **same wallet/address** as the desktop?

They are independent. The recommended answers are **own place** and **own wallet**, with an
explicit advanced path for "use the same wallet".

## 2. What "same ETH address" actually requires

The desktop wallet is a phrase, not an account synced through Places. The only existing way to make
the phone use the same address is to **restore the same BIP39 phrase on the phone**. There is no
wallet-sync transport, and the vouch has no wallet field.

| Option | How it works | What it costs |
|---|---|---|
| **A. Phone has its own wallet** (recommended) | New phrase on the phone; its own ETH address; its own BTC wallet too, because one phrase backs both. | Desktop and phone are two wallets. Peers see two places with two addresses. That is a feature of Places, not a bug. |
| **B. Restore the desktop phrase on the phone** | Same address and same balance on both. | Two copies of the seed; a compromised phone drains the desktop wallet. Plus a real **nonce race**: two independently online signers can fetch and use the same nonce. The code has no coordination. |
| **C. Watch-only on the phone** | Shows the desktop address/balance; signing stays on the desktop. | Does not exist today; needs a new import flow and a trust story. It cannot satisfy "payments in chats". |
| **D. Phone asks the desktop to sign** | A new remote-signing protocol over the contact channel. | Does not exist; the desktop must be online and unlocked; breaks local-first and adds a key-use surface. Not recommended. |

**A separate address is fine for payments.** The peer pays the phone's contact, and the phone
announces its own address. Nothing about the payment protocol requires one address everywhere.

**What "same address" does not give you:** shared payment history. Payment lines and notices are
written to the **receiving vault's** chat log. If a peer pays the shared address through the
phone's contact, the desktop sees nothing in its own chat, and vice versa. The money is in one
on-chain account; the conversation record is not shared.

### If B is chosen anyway

- Show a one-screen warning that this duplicates the seed and breaks the "one place, one blast
  radius" property.
- State that only one device should send at a time, or accept occasional nonce failures. There is no
  lock or lease in the code.
- Never put the phrase in a QR code. Manual entry, or a future encrypted one-time transfer over the
  contact channel. A phrase QR is a shoulder-surfing and screenshot hazard.
- Do not combine the place invite and the wallet phrase in one flow or one code.

## 3. Places consequences that must be stated in the product

These follow from D2/D4/D6 and change what "the mobile app" means:

- **The phone does not inherit the desktop's chats.** A place is a separate contact with its own
  thread. The desktop's existing conversations are not on the phone.
- **The phone does not inherit the desktop's contacts.** Adoption auto-adds the desktop and phone to
  each other (D6), but other peers must add the phone: in person by scanning its QR, or via the
  opt-in introduce message (D8).
- **Peers see two contacts.** The "place of …" chip appears only when the vouch chain verifies
  (D9), capped at two hops (D13).
- **Conclaves are per identity.** The phone is not in a conclave the desktop joined; the admin must
  re-invite it (D3). The draft says there is no conclave UI on mobile, so the phone should not be
  invited at all.
- **Backups are per place.** The phone backs up itself; there is no parent backup of the phone vault
  (D4). Android auto-backup must not capture the vault (see §8).
- **The relay keeps a message for seven days** (`RelayConfig::default` in
  `crates/hush-relay/src/config.rs`, `blob_ttl`). Under Places there is no desktop catch-up, because
  the desktop cannot read the phone's queue. A phone offline longer than that loses the messages and
  attachments sent to it, and the sender still sees a successful send. Decide whether to accept the
  window or give the phone its own ciphertext replica.
- **Loss is handled by removal, not revocation.** Remove the place; the peer keeps the badge until
  they remove the contact, because there is no live vouch revocation in v1 (D12).

## 4. Proposed scope

The draft's list is a good start; this fills in the rails it does not mention. "V1" means the first
shipping Android build.

| Area | V1 | Notes |
|---|---|---|
| 1:1 chat (text) | **Yes** | The core; fully local-first once the vault is open. |
| Mail | **Yes (read + reply)** | This is the offline path. Without it the phone can only talk to peers who are online at the same time. Recommend at least reading and replying to text mail. |
| Attachments | **Yes** | Stored in the phone's own vault. No cross-place sync. Needs a phone-local size cap and a clear "storage full" failure. |
| USDC payments in chat | **Yes** | The `+` → Send USDC action and `/pay` parsing already exist in `payCommand.ts`; reuse the two-step confirmation and the daemon's `pay-trustee` rules. |
| Receive USDC / share address | **Yes** | The phone must announce its address after wallet create/restore and on demand, like `POST /api/wallet/announce`. |
| Contacts / trustees / payments consent | **Yes** | Deny-by-default payments and signals; the phone needs the toggles, or it cannot be paid. |
| Places: adopt, introduce, detach | **Yes (minimum)** | Needed for the "Do you have the desktop app?" flow and for peers to learn the phone. |
| Invites / QR scanning | **Yes** | Also the camera stack the postal bridge scanner will reuse later. |
| 1:1 audio call | **Yes** | From Contacts/Trustees, as the draft says. Only while both apps are reachable; see §5.4. |
| 1:1 video call | **Later** | Same signaling; media and UI are extra work. |
| Group chat / Conclaves | **No** | The phone is not a conclave member. Peers cannot send to it there. |
| Conclave Paper | **No** | Desktop workspace. |
| Group calls | **No** | A mesh does not scale; LiveKit is the named future path, not this slice. |
| Drops | **No** | Blob-heavy and desktop-oriented. |
| Torrents / TorBox | **No** | Explicitly excluded. Hide the controls; do not show a dead policy toggle. |
| Notes | **No** | Desktop workspace. |
| Reads | **Later** | Network-bound and self-contained; a reasonable second slice. |
| Zines / public lane / AT Protocol | **No** | Not needed for the mobile job. |
| Full Wallet pane (BTC, ETH, tokens) | **No** | Hide it. Mobile exposes only the USDC/payments surface. The keys still exist in the vault. |
| Files browser | **No** | Attachments are opened from their message. |
| Postal bridge scanner | **Later** | The QR/camera work lands here first; the scanner is a separate plan. |
| Settings / security / places | **Yes** | Lock, biometric, relay, contacts, places, remove place. |

**Non-goals for v1:** desktop parity; chat history sync from the desktop; group chat or group calls;
torrents; the full wallet pane; iOS. The phone is a companion place, not a second desktop.

## 5. Architecture

### 5.1 No daemon sidecar on Android

The desktop is two processes: the Tauri shell and `hush-daemon`, joined by a `0600` Unix socket. The
wallet is mounted only when that socket exists (`wallet_routes = state.socket.is_some()` in
`crates/hush-daemon/src/main.rs`). Android has no daemon process to hold that socket, and the
webview cannot speak to one anyway.

**Recommended:** compile the daemon logic into the Tauri Rust backend and expose the same API as
Tauri commands (the existing `hush_api` proxy is already the seam). This gives the wallet **no
network listener at all**, which is stronger than the desktop's socket and avoids the Windows-plan
tradeoff where the wallet is deliberately absent over TCP.

**Alternative:** loopback TCP plus a bearer token, as the Windows plan does. If chosen, the wallet
gate must be changed from "has a Unix socket" to an explicit mobile capability, and the plan must
acknowledge that any app on the device can reach the port if it learns the token. This is the
weaker option and I would not choose it.

What this requires:

- `hush-daemon` is a binary today, not a library. The mobile API needs either a lib refactor or a
  thin mobile module that calls `AccountWorker`/`WalletManager` directly. This is shared work for
  every feature, not just the wallet.
- The mobile shell must stop falling back to `default_transport() → unix://…` (see `lib.rs`).
- `isTauriShell()` is true on Android, so the current "wallet lives in the desktop app" fallback in
  `renderWallet` will not fire. Add a real availability gate; otherwise the pane tries to reach a
transport that does not exist.

### 5.2 Wallet reuse

- `crates/hush-wallet` is portable: alloy, EIP-55, BIP44 `m/44'/60'/0'/0/{index}`, ERC-20
  `balanceOf`/`transfer`/`symbol`/`decimals`, JSON-RPC over HTTP. The vouch/invite model does not
  touch it.
- Bitcoin dependencies are currently mandatory (`bdk_wallet`, `bdk_electrum`). If the mobile build
  ships USDC-only, add a `bitcoin` feature gate or accept the extra size. Both are expected to
  compile for Android, but this was **not verified** (see the OPEN entry in `DEEPSEEKPROBLEMS.MD`).
- The Ethereum side has no chain database (`store.rs`): network, account index, tracked tokens.
  Balances are read on demand, so mobile lifecycle restrictions do not create a sync problem. The
  Bitcoin side does keep a BDK change set, which is another reason to feature-gate it out of a
  USDC-only build.

### 5.3 Tor

`NetworkMode::default()` is Tor, and the wallet is locked to Tor whenever the relay is on Tor. The
desktop satisfies this by spawning a `tor` child process (`src-tauri/src/tor.rs`). Android has no
bundled Tor binary.

| Option | Cost |
|---|---|
| **Orbot** (SOCKS 9050) | Fastest; an external app the user must install. Acceptable if documented as a dependency. |
| **Embedded Arti** (`arti-client`) | Self-contained; binary size, startup and battery cost, plus lifecycle work. |
| **Clearnet only** | Works immediately, but exposes the user's IP and queried addresses to the RPC provider. Inconsistent with the app's posture unless explicit and warned. |

Recommendation: **Orbot first**, with a clear "install Orbot for private mode" state, then evaluate
Arti if the external dependency is unacceptable. Do not silently fall back to clearnet.

### 5.4 Calls

- The desktop's call stack is daemon-owned WebRTC (`webrtc`/`rtc` crates, native media), because
  WebKitGTK has no `RTCPeerConnection`. The Android System WebView **does** support WebRTC, so the
  phone can use a different media path.
- That is a **second code path**: signaling stays in Rust (inside the existing Signal session),
  while the WebView owns the peer connection and capture/playout. Decide this explicitly; do not
  assume the desktop media stack ports.
- Calls are direct P2P; a contact learns the other's IP (accepted in the existing threat model). A
  self-hosted STUN server is the intended infrastructure; there is no first-party TURN. NAT failures
  are a real state to show, not a spinner.
- **No push means a call cannot wake a closed app.** The phone rings only while the app is open or
  while a foreground service is running. This is a product constraint, not a bug; state it in the
  call UI.

### 5.5 Storage and attachments

- The phone's vault is app-private, encrypted, and small; attachments are the only unbounded part.
- Under Places there is no blob sync, so there is no "not on this device" state across places. A
  file sent to the phone is fetched to the phone; if the phone is out of space, say so plainly and
  let the relay hold the ciphertext until its TTL.
- Set a size cap and an eviction policy before shipping. Do not leave the phone to fill up.

## 6. First-launch flows

The draft's single question is the right idea; it needs three branches.

### A. "No, this is my only device"

1. Create the phone vault with its own passphrase and recovery code.
2. Optionally create the wallet now, or defer until the user needs payments.
3. Show the recovery code once; make the user confirm it.

### B. "Yes, I have the desktop app"

1. The phone creates its own vault first. It needs an identity before it can be adopted.
2. The desktop is opened and **unlocked**.
3. Desktop → Settings → My places → Add place → scans the **phone's** invite QR (D15). The desktop
   is the minting place; the phone counter-signs (D6).
4. Both sides auto-add each other as contacts (D6).
5. The phone now has its own wallet (or none yet). It does **not** have the desktop's chats or
   contacts.
6. Optional advanced step: "Use the same wallet as my desktop" → restore the desktop phrase on the
   phone, with the warnings in §2. This is deliberately a separate step, not part of adoption.

Note the direction: the **existing place scans the new vault's invite**. The draft's "the user
provides their invite" is ambiguous; the code expects the desktop to scan the phone.

### C. "Restore from a backup / recovery code"

Import the phone's own vault backup if one exists. A desktop backup restored on the phone makes the
phone a copy of the desktop's place, which is the shelved multi-device model, not Places. Do not
offer it without saying so.

## 7. Payments and USDC — the details that matter

### 7.1 The desktop rules to reuse

`wallet_pay_trustee` in `crates/hush-daemon/src/main.rs` is the contract:

- payments and signals must both be allowed on the sender's contact record, and the peer must allow
  payments;
- the destination is the contact's **announced** Ethereum address, never a UI-supplied one;
- the USDC contract is resolved from the wallet's `ethereum_network` (`usdc_contract`), never from
  user input;
- the sender's line is written to chat even if the peer is unreachable, because the money has moved;
- the notice is best effort; the recipient verifies the transfer on chain before showing
  "received".

### 7.2 Ethereum is not optional, even for USDC

USDC is an ERC-20. A transfer costs **ETH for gas**. The phone cannot be "USDC only": it needs a
small ETH balance, and the UI must show the fee or at least warn that one is paid. Gasless/relayer
flows are out of scope; they add a third-party service and a trust-boundary decision.

### 7.3 Network match

Today the wallet supports **mainnet** and **Sepolia** only, with fixed USDC contracts:

| Network | USDC contract |
|---|---|
| mainnet | `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` |
| Sepolia | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` |

Both peers must be on the same network. There is no negotiation; a phone on Sepolia and a desktop
on mainnet will not complete a payment. The confirmation already names the network; keep that.

**Decision needed:** mainnet-only, or add an L2 (Base/Arbitrum) for cheap USDC? The crate can support
another network, but both ends need the new chain and USDC contract, and the desktop needs the same
change. Mainnet fees are a real onboarding cost for a phone wallet.

### 7.4 Onboarding a phone wallet

- Auto-track USDC when the phone creates or restores a wallet, so the balance is visible without
  asking the user to paste a contract address. The desktop's manual "Add token" flow is a desktop
  affordance; the mobile equivalent is a fixed USDC row.
- Show the receive address and a QR, plus "share with trustees".
- Do not force wallet creation at first launch. Offer it when the user opens a payment, receives a
  payment request, or enables payments with a contact.

### 7.5 Same-address consequences, restated

If the phone restores the desktop phrase:

- both devices can spend the same funds, including paying gas;
- one payment appears in only one device's chat log;
- concurrent sends can collide on the nonce;
- a phone compromise is a desktop compromise.

If the phone has its own wallet:

- a peer pays whichever place it has as a contact;
- the desktop can pay the phone, and the phone can pay the desktop, because adoption made them
  contacts — but payments are deny-by-default, so both sides must enable them;
- the two balances are separate and must be funded separately.

### 7.6 Two secrets, not one

The vault recovery code and the wallet phrase are different secrets. Restoring the vault does not
restore the wallet's funds unless the phrase was written down. The desktop shows the phrase exactly
once, at creation (`walletPhraseCard` in `app.ts`); on mobile, make the wallet step explicit and do
not let it be dismissed silently.

- If the phone creates its own wallet, the user must back up **both** the vault recovery code and
the wallet phrase. Losing the phone loses the wallet otherwise.
- If the phone restores the desktop phrase, the user already has that phrase (or should); the vault
recovery code is still separate.
- Never derive one from the other, and never display them together.

## 8. Security and Android platform work

- **Vault key:** wrap the master key with the Android Keystore (StrongBox where available), gated by
  biometric/PIN. The phrase is sealed inside the vault as it is on the desktop.
- **Screens:** `FLAG_SECURE` on plaintext screens (chat, seed, address QR), and no sensitive content
  in the recents snapshot.
- **Clipboard:** do not offer "copy phrase" as the desktop does. If the address is copied, clear it
  after a short interval. Never put the seed on the clipboard.
- **Backup:** `android:allowBackup="false"` and `dataExtractionRules` that exclude the vault; no
  cloud auto-backup of keys.
- **Notifications:** no message text or sender in the notification by default; "New message" only.
- **Network:** cleartext disabled; TLS to the relay and RPC. If Tor is selected, no direct route as a
  fallback.
- **Permissions:** camera (QR), microphone (calls), notifications, foreground service, biometric.
  Nothing else; no contacts, location, or storage-wide access.
- **No push service:** delivery is pull. A foreground service can keep chat/calls alive while the
  user expects it; otherwise messages arrive when the app opens. Do not promise background
  notifications that the architecture cannot deliver.
- **No in-app updater:** the updater plugins are desktop-only. Distribution is APK/F-Droid with its
  own signing story; say how updates are delivered.
- **Phone loss:** remove the place on the desktop. There is no live vouch revocation, so peers keep
  the badge until they remove the contact (D12).
- **Rooted devices:** warn, do not pretend to block.
- **QR input is untrusted:** validate invite/vouch payloads and fail closed, as the postal bridge
  plan already requires for its scanner. An invite QR and a seed QR must never be the same shape.

## 9. What must exist before the wallet slice

1. Android shell that builds and runs on a device (Tauri 2 Android, NDK, target installed).
2. In-process backend/transport, with the vault unlock/lock lifecycle.
3. Contacts and invites working on the phone, because payments need a contact with consent.
4. Places adoption, because that is the "I have a desktop" path.
5. A decision on wallet identity (§2) and USDC network (§7.3).

Until 1–4 exist, a mobile wallet is a crate cross-build and a UI mock, not a feature.

## 10. Milestones

| # | Slice | Done when |
|---|---|---|
| M0 | Cross-compile spike | `hush-wallet --features ethereum` links for `aarch64-linux-android`; app size measured. |
| M1 | Android shell + in-process backend | App opens, creates/unlocks a vault, reports status, no daemon process. |
| M2 | Contacts, invites, chat text, attachments | Two phones and a desktop can talk; attachments land in the phone vault. |
| M3 | Places | Desktop adopts phone by QR; both sign; peer badge verifies. |
| M4 | Payments | Phone wallet (or restored phrase), USDC send/receive, consent gates, on-chain verification. |
| M5 | 1:1 calls | Audio call from a contact, with the no-push/foreground limitation stated. |
| M6 | Hardening and release | Keystore/biometric/FLAG_SECURE, backup exclusions, signing, F-Droid/APK. |

## 11. Open decisions

1. **Wallet identity:** own wallet (recommended) or the desktop phrase? If the latter, accept the
   nonce race and seed duplication.
2. **USDC network:** mainnet today, or add an L2 before mobile ships?
3. **Tor:** Orbot, embedded Arti, or clearnet with a warning?
4. **Mail:** in v1 or later? Without it the phone is online-only for messaging.
5. **Calls:** WebView media path (recommended) or port the native stack? WebView is the second code
   path, but the desktop stack does not run there.
6. **Places adoption direction:** confirm the desktop scans the phone's invite (D15). The draft's
   wording suggests the opposite.
7. **Conclaves:** confirm the phone is never a member (no UI, no messages).
8. **Attachment cap and eviction:** a number, and where the "storage full" state appears.
9. **Minimum SDK / ABIs / distribution:** decide before the first release build.
10. **Backup story:** recovery code only, or an encrypted export? Per place, never the parent's.
11. **Offline window:** accept the seven-day relay TTL, or give the phone its own ciphertext
    replica? Under Places the desktop cannot serve as the catch-up source.
12. **Two-secret onboarding:** how hard is the wallet-phrase step, and how is "I wrote it down"
    verified on a phone?

## 12. Doc conflicts to fix

- [`2026-10-18-android-linking.md`](plans/2026-10-18-android-linking.md) §5 still says "Wallet
  (Task 8) — unbuilt". The wallet is built; that line is stale.
- This plan's "invite → same ETH address" and "linked to the same identity" conflict with D1/D4/D6.
  The addendum's §2 is the corrected model.
- The Windows plan's "wallet is Unix-socket-only" is still true for desktop; the Android in-process
  backend is a different transport, so it needs its own explicit gate rather than reusing the
  socket condition.

## 13. Mobile-only experiences worth considering

These are optional and should not delay the core. They follow from the constraints above rather than
from a desire to clone the desktop.

- **Camera-first home.** Launch to a scanner: a place invite, a contact invite, or a payment address.
  Scanning is the natural mobile gesture; the desktop's rail-plus-pane layout is not.
- **People-first, not rail-first.** A list of trustees with Chat / Call / Pay, and the last message
  underneath. The desktop's ten rails do not fit a phone's width or attention span.
- **A payments mini-sheet, not a wallet pane.** USDC balance, receive address and QR, and a send
  form. The full BTC/ETH/token surface stays on the desktop.
- **Mail as the fallback composer.** Chat needs both peers online; when a chat send would fail
  because the peer is offline, offer "send as Mail" in the same flow. This is the mobile-first answer
  to the asynchronous reality.
- **A place banner.** "This phone is Sam's phone" with the vouch badge, so the user always knows
  which identity and which address a peer is seeing.
- **In-person adoption.** One screen on the desktop (Settings -> My places -> Add place) and one on
  the phone (My QR). The scan is the whole flow; no codes to copy.

Do not build a literal port of the desktop's layout. The components can be reused; the navigation
and information density should not.

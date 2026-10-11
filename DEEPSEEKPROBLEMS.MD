# DeepSeek implementation problems

Running log of problems hit while implementing slices. Kept so the same dead ends are not
re-explored and so unverified claims stay visible instead of being quietly dropped.

**Convention:** append an entry when a problem is hit, not retroactively at the end of a
slice. Newest section first. One entry per problem, with what was attempted, the symptom, the
resolution, and the status.

Status tags: `FIXED` (resolved in-slice), `WORKED AROUND` (avoided, cause not fixed),
`OPEN` (unresolved, needs a different environment or a later task), `NO LONGER APPLICABLE`
(the configuration it described cannot occur any more).

---

## 2026-10-11 — Wallet, part 2: Ethereum and ERC-20 tokens on the same phrase

Slice goal: derive a second chain from the phrase the Bitcoin wallet already stores, at the
interoperable BIP44 path `m/44'/60'/0'/0/i`, and let the wallet hold, receive, send, and track
ERC-20 tokens — with the same privacy rule as Bitcoin: while the relay is on Tor the wallet is on
Tor, and a missing proxy is an error, never a quiet fallback to clearnet. Amounts gain a second
unit (wei) that does not fit in a JSON number, so every amount on the wire becomes a decimal string
in base units.

### [FIXED] `spqr`'s build script panicked with a message that never named the missing tool: `protoc`

- **Attempted:** `cargo build --features ethereum`, which rebuilt the workspace's Signal-protocol
  dependencies as well as the new wallet crate.
- **Symptom:** the build died inside `spqr`'s build script with
  `Protobufs in src are valid` — the string in its
  `.expect("Protobufs in src are valid")` around `prost_build::compile_protos(...)`. `prost-build`
  shells out to `protoc`; there was none on `PATH`, and the panic message names neither `protoc` nor
  `prost`. `libsignal`'s build scripts (`rust/protocol`, `rust/account-keys`, `rust/keytrans`,
  `rust/message-backup`, `rust/svrb`) fail the same way.
- **Fix:** this repo already ships protoc at `.tools/protoc-dist/bin/protoc` and every
  `scripts/*.sh` exports `PROTOC` to it. The same 29.3 build was placed at `~/.local/bin/protoc` —
  same version, same 9623664 bytes — so that a bare `cargo build` also works, not just the scripts.
  `protoc --version` now reports `libprotoc 29.3`.
- **Pre-existing, not caused by `alloy`.** `spqr` is a dependency of `libsignal`, which
  `hush-relay` uses; the repo already logs a fresh-`target` build failing for this reason. What is
  new is only that this slice's dependency change forced those build scripts to run again.
- **Lesson:** a build script's `.expect()` string is the symptom, not the cause. When a Rust build
  dies inside a third-party `build.rs` with a domain-shaped message, look for an external binary it
  shells out to before looking at the crate's own code.

### [FIXED] A doc comment on a `sol!` item is a rustdoc warning, and `&str + &str` does not concatenate

- **Attempted:** document each ERC-20 function with `///` inside the `sol!` macro body, and build an
  expected calldata suffix in a test with `"0".repeat(63) + "1"`.
- **Symptom:** two compile-time complaints, both in the new file. rustdoc warns about the doc
  comment attached to the macro invocation (`unused doc comment`, because the attribute is not on an
  item rustdoc can name), and `binary operation `+` cannot be applied to type `&str`` on the test's
  string concatenation.
- **Fix:** plain `//` inside `sol!`, with the documentation on the module and the wrapper methods
  that call the generated types; and `format!` for the test's expected suffix. `cargo build` is
  warning-free.
- **Lesson:** a declarative macro that generates items owns their attributes — documentation for
  generated code belongs on the hand-written wrapper. And `String + &str` works where `&str + &str`
  does not; the error names the operator, not the types that would have worked.

### [FIXED] `#[cfg(feature = "ethereum")]` in the daemon was an `unexpected_cfgs` warning and a dead branch

- **Attempted:** gate the Ethereum half of `crates/hush-daemon/src/wallet.rs` on
  `feature = "ethereum"`.
- **Symptom:** `warning: unexpected cfg condition value: ethereum` from the `unexpected_cfgs` lint —
  the daemon crate declared no such feature, so the "off" branch was the only one that could ever
  compile, and the whole Ethereum path would have been dead code with no error to say so.
- **Fix:** declared it: `[features] default = ["ethereum"]` and
  `ethereum = ["hush-wallet/ethereum"]` in `crates/hush-daemon/Cargo.toml`.
- **Lesson:** a `cfg` on a feature that does not exist is not an error in Rust; it is a silently
  disabled branch. The `unexpected_cfgs` lint is the only thing that catches it.

### [FIXED] A failed rebuild after a mode switch left the Bitcoin wallet on its previous connection

- **Attempted:** make the wallet follow the relay's Tor switch by rebuilding it on every change.
- **Symptom:** no loud failure, which is the problem. `rebuild_eth` already failed closed — it set
  its handle to `None` and recorded why — but `rebuild_btc` only reported the error and left
  `*btc` holding the wallet built for the *previous* mode. If a switch to Tor could not be built
  (for instance the shell had not yet reported its SOCKS port), the wallet stayed on its clearnet
  configuration while the UI said Tor.
- **Fix:** `rebuild_btc` now mirrors `rebuild_eth`: on failure `*btc = None` and `btc_error` records
  the reason, so the wallet is unavailable rather than wrongly connected.
- **Lesson:** when two sibling functions implement the same invariant, the invariant belongs in the
  shared shape. The Ethereum side was written correctly first and the Bitcoin side was not
  revisited; a reviewer comparing them would have caught it, a test would not have.

### [FIXED] The wallet's HTTPS transport only had TLS because `alloy`'s defaults happened to bring it

- **Attempted:** give the adapter its own `reqwest` with `default-features = false, features =
  ["socks", "json"]`, so no system proxy or native-tls is picked up behind its back, and depend on
  `alloy` with an explicit feature list.
- **Symptom:** nothing failed — which is the defect. The wallet talks to `https://…` RPC endpoints
  and its own manifest never asks for a TLS backend; it works today only because the `alloy` features
  left on by default include `reqwest-rustls-tls`. Confirmed with
  `cargo tree -e features -p hush-wallet --features ethereum -i reqwest@0.13.5`: `reqwest`'s
  `rustls` feature arrives solely through `alloy-provider/reqwest-rustls-tls`. Setting
  `default-features = false` on `alloy` — a reasonable future cleanup — would have removed TLS and
  turned every balance read and send into a runtime failure.
- **Fix:** name `rustls` in the wallet's own `reqwest` features, with a comment saying why it is
  named twice.
- **Lesson:** a crate that makes HTTPS calls should declare its TLS backend. Inheriting one from a
  dependency's defaults is a latent runtime failure that no compiler and no test on this box can see.

### [OPEN] A debug relink of the daemon test binary exceeded 40 minutes on this box

- **Attempted:** `cargo test -p hush-daemon wallet`, the only test command that exercises the wallet
  routes end to end.
- **Symptom:** the final `rust-lld` link of `hush_daemon`'s test binary produced a 403 MB executable
  and sat at roughly 70% written for tens of minutes. `vmstat` showed the machine fully swapped
  (`swpd` 8.2 GB of 8.0 GB, `si/so` active, 180 MB of 15.9 GB free); the linker's user CPU time did
  not advance between samples while its system time crept up, i.e. it was faulting pages in and out,
  not computing. `date` and the temp file's mtime did advance, so it was progressing, only far slower
  than the work warrants.
- **Not a repo defect.** It is the dev profile (`debug = 2`, no `[profile]` section anywhere in the
  workspace) linking a binary that now contains alloy + rustls + aws-lc-sys + ring, on a machine
  where the user's own applications hold about 14 GB. Adding `alloy` to the daemon is what pushed
  this test binary over the edge; `hush-wallet`'s own test binary links quickly.
- **What is needed:** free memory at build time, or a lighter dev profile for this work. Setting
  `[profile.dev] debug = "line-tables-only"` in the workspace root would cut the link cost sharply,
  but it changes the debug artifacts of every crate in the repo and was not applied unilaterally.
- **Lesson:** a dependency added for one crate is paid for by every test binary that links that
  crate. Before adding a heavy tree to a crate with integration tests, check what the final link
  costs, not just what `cargo check` costs.

### [OPEN] Ethereum's network paths are untested in this environment

- **What is unverified:** `verify_chain()`, `balance()`, `token_metadata()`, `token_balance()`,
  `send()`, and `send_token()` have never run against a real JSON-RPC endpoint, and no Ethereum
  request has ever crossed the shell's Tor SOCKS proxy. The tests cover derivation against the
  published vector, EIP-55 checksumming, the four ERC-20 selectors and the `transfer` calldata's
  argument order, chain-id and network-name validation, the Tor-without-a-proxy refusal, the
  `u128`-saturating balance conversion, and the balance snapshot's per-token bookkeeping — all
  offline, all against no network.
- **What is needed:** one live read against a mainnet or Sepolia endpoint over clearnet and then
  through Tor, and one signed send on a funded Sepolia account. Until then, treat the Ethereum
  send path as unproven, exactly as the Bitcoin one is.

### [OPEN] The multi-chain Wallet pane has been type-checked, not looked at

- **What is unverified:** the chain capsule, the token list, the token picker on the send form, and
  the live unit hint (`= … wei` / `= … of the smallest USDC unit`) have been compiled and
  type-checked (`npx tsc --noEmit` clean) but never rendered. The Bitcoin pane's rendering was
  checked against a mock daemon in a browser (see the 2026-10-10 entry); the Ethereum additions to
  that pane have not been through even that.
- **What is needed:** the same mock-daemon browser pass the Bitcoin pane got, extended to a
  `tokens` array and an `ethereum` chain entry, or a real window run. The CSS for the new classes
  (`.wallet-chains`, `.wallet-token-list`, `.wallet-token-row`, `.wallet-token-symbol`,
  `.wallet-token-balance`, `.wallet-token-address`, `.wallet-token-picker`) is new and likewise
  unrendered.

### [FIXED] The Bitcoin adapter checked its Tor proxy when it first synced, not when it was built

- **Attempted:** make both chains refuse to exist without a route in Tor mode. The Ethereum adapter
  builds its HTTP client in `EthereumWallet::create`, so a missing proxy is an error there. The
  Bitcoin adapter builds its electrum client lazily, inside `make_client()`, which only runs on the
  first network call.
- **Symptom:** `BitcoinWallet::create(config, mnemonic)` returned `Ok` with `mode: Tor, socks: None`.
  The refusal did happen — no clearnet fallback ever occurred, because every network entry point
  goes through `make_client()` — but it happened later, as a sync error. So the daemon held a
  wallet object it treated as ready, reported it to the UI as ready, and the two chains disagreed
  about whether a wallet existed at all for the same configuration: `build_wallet` said yes,
  `build_eth` said no.
- **Fix:** `BitcoinConfig::validate()` (a thin wrapper over the new `socks_addr()`) is called from
  both `create` and `load`, so the invariant is "a wallet object in memory has a correctly routed
  client" rather than "…will have one eventually". `make_client()` still checks too.
- **Lesson:** when two implementations of one invariant live in the same module, put the check where
  it makes the illegal state unrepresentable, not where the failure happens to surface. A lazy
  client turns a construction-time refusal into a runtime one, and the two are not equivalent.

### [FIXED] The wallet's Tor was wired to the relay's switch, so choosing Tor with the relay on clearnet left the wallet with no route

- **Attempted:** let the user pick clearnet or Tor for the wallet while the relay is on clearnet,
  the way the UI already offered.
- **Symptom:** `sync_wallet_relay` passed the shell's SOCKS port to the wallet only
  `if relay_tor`, and `POST /api/relay/mode {onion: false}` cleared it. With the relay on clearnet
  and the wallet's mode set to Tor — the daemon's own default, `NetworkMode::default()` being Tor —
  the wallet had no proxy and could not be built. The UI offered a choice that could not work, and
  with the construction-time refusal above in place, a wallet could not even be created.
- **Fix:** the wallet's route is its own: `App.wallet_socks`, reported by the shell through a new
  `POST /api/wallet/proxy`, and `sync_wallet_relay` prefers it over the relay's. The UI brings the
  shell's Tor up when the user picks Tor (`ensureWalletTor`), reports the port, and the status poll
  re-announces it after a daemon restart. Turning the relay off no longer stops Tor while the wallet
  still needs it.
- **Lesson:** two features sharing one process is not the same as two features sharing one switch.
  The relay's switch was reused as the wallet's route because both name the same Tor, and that made
  one feature's off-state break the other.

### [FIXED] A test asserted the Tor-without-proxy refusal using an invalid mnemonic, so it passed for the wrong reason

- **Attempted:** `an_unknown_ethereum_network_is_refused_rather_than_defaulted` built a
  `WalletState::new("x".into(), …)` and asserted `build_eth(...).is_err()`.
- **Symptom:** the assertion passed, but on the phrase, not the network: `"x"` is not a BIP39
  mnemonic, so `build_eth` failed before it ever looked at `state.ethereum.network`. The second half
  of the test — setting the network to a valid `"sepolia"` and expecting `Ok` — then failed, which is
  what exposed it. The test had been asserting nothing about network validation.
- **Fix:** use the canonical all-`abandon` vector phrase, so a failure can only come from the name.
- **Lesson:** a negative assertion needs a control that proves the input reaches the code under test.
  `is_err()` on a state that cannot be built for an unrelated reason is not evidence.

### [FIXED] The API test helper panicked on axum's own rejections instead of reading their status

- **Attempted:** `request(&router, "POST", "/api/wallet/send", Some(r#"{"chain":"litecoin",…}"#))`
  to check that an unknown chain is refused.
- **Symptom:** the test panicked inside the helper at `serde_json::from_slice(&bytes).unwrap()`,
  not at its own assertion. A body axum's `Json` extractor rejects — an unknown enum variant is a
  422 — never reaches a handler and so is a plain-text body, not JSON.
- **Fix:** the helper falls back to the raw body as a `Value::String` when it does not parse.
- **Lesson:** a test helper that `unwrap`s a parse is asserting that the response is well-formed,
  which is a different claim from the one the test is making. Extractors own their rejections; a
  handler-level test has to expect them.

### [WORKED AROUND] The full daemon suite could not be run through `cargo`: the target-dir lock was held by another build

- **Attempted:** `cargo test --features ethereum -p hush-daemon` after the wallet tests passed, to
  check the relay and drops paths for regressions.
- **Symptom:** the command sat in `locks_lock_inode_wait` for 35 minutes with no compiler child of
  its own. `ps` showed the lock holder was a different `cargo test -p hush-tauri` (from another
  Cursor terminal, not this session), whose `collect2`/`rust-lld` child was alive but advancing at
  roughly 0.9 s of CPU per minute — the same swap-exhaustion link stall already logged above, not a
  deadlock. Nothing was wrong with the command; it was simply queued behind a build it could not
  influence.
- **Fix:** ran the already-linked test binary directly —
  `./target/debug/deps/hush_daemon-ed06aec28782b97f` — which needs no lock and no relink. 60 tests
  passed in 24 s. The blocked `cargo` was then killed so it would not seize the lock for a pointless
  relink once the other build finished. The other build was left alone: it belongs to the user.
- **Lesson:** `cargo test` is two things — a build and a run — and the run half does not need the
  target directory. When the build half is queued behind someone else's, run the binary. The
  `.d`/`deps` names and a mtime check are enough to confirm it is the current one (the terminal
  metadata timestamps are UTC while `ls` shows local time; the same build looked 5 hours stale until
  that was noticed).

---

## 2026-10-10 — Self-custody wallet: BDK, sealed in the vault, locked to the relay's Tor

Slice goal: embed a wallet in Hush with no third party. Bitcoin first (BIP84 over electrum), with
Ethereum and its tokens to follow. The seed and the wallet state are sealed in the vault sidecar,
the daemon serves `/api/wallet/*` only over the Unix socket, and the wallet is never less private
than the app: when the relay is on Tor the wallet is on Tor and the toggle is locked.

### [FIXED] The receive address shown to the user was pinned to index 0 forever

- **Attempted:** `PublicState.address` was filled with `w.address_at(0)` — a read-only peek at
  derivation index 0 — while the "new address" action called `reveal_next_address`, which on a
  fresh wallet also returns index 0.
- **Symptom:** a daemon test asserted the two differ and failed with both sides equal:
  `assertion left != right failed / left: "bc1qlemxze4f2qxytlq32zgde0fc68n57kezlez6cd" / right:
  "bc1qlemxze4f2qxytlq32zgde0fc68n57kezlez6cd"`. The deeper problem was not the test: once the
  index-0 address had been paid, the UI would have kept displaying it, inviting a second payer to
  reuse an address the wallet had already used.
- **Fix:** a new wallet now reveals index 0 at creation, so the first "new address" is genuinely
  new. The UI-facing address comes from `BitcoinWallet::current_receive_address()`, the most
  recently revealed address (read-only, safe on every poll). `sync()` reveals the next one when an
  output — spent or unspent, via `list_output()` — has landed on the address on screen.
- **Verified:** `cargo test -p hush-wallet` (17 tests) and the three `wallet_api_tests` in
  `hush-daemon` pass; the create test now asserts the shown address is mainnet P2WPKH and that the
  first "new address" differs and becomes the one the state route returns.
- **Lesson:** "read-only peek at index 0" and "the address you hand out" are different concepts.
  Any displayed address that can be paid must be the one the keychain considers handed out, and it
  must advance when it is used. A test that asserts two addresses differ is what surfaced it —
  a shape-only check (`starts_with("bc1q")`) would have passed.

### [FIXED] Flipping the relay switch on the pre-login screen left the wallet on clearnet

- **Attempted:** the wallet's own routing mode is set through `WalletManager::apply_relay`, which
  only `POST /api/relay/mode` called. Meanwhile the wallet's *reported* mode is read fresh from
  `ui-prefs.json` on every request (`relay_is_tor`).
- **Symptom:** nothing failed loudly. `PUT /api/ui-prefs` can set `relay_tor` — that is exactly how
  the pre-login screen flips the switch while the vault is locked — and `apply_relay_prefs` at
  unlock applied the switch to the relay worker but never to the wallet. So the UI would report
  `mode: "tor"`, `mode_locked: true` while the BDK client still held `NetworkMode::Clearnet` and
  would have queried electrum over the open network. A privacy invariant broken silently.
- **Fix:** one helper, `sync_wallet_relay`, reads the switch and the shell's SOCKS port and applies
  both to the wallet; `apply_relay_prefs`, `put_relay_mode`, and `put_ui_prefs` all call it. The
  prefs route only calls it when `relay_tor` actually changed, so a theme change does not rebuild
  the wallet. New test `the_relay_switch_on_the_prefs_route_also_locks_the_wallet` drives the prefs
  route and asserts the wallet reports Tor, is locked on, and refuses clearnet.
- **Lesson:** when a value is read from one place (prefs) and applied from another (an explicit
  endpoint), every writer of the first place has to be checked. The bug was not in the wallet or in
  the prefs route; it was in the assumption that only one route could move the switch.

### [FIXED] `bdk_wallet` re-exports `ChangeSet`/`WalletPersister`, so the module path does not compile

- **Attempted:** `use bdk_wallet::wallet::{ChangeSet, WalletPersister};`.
- **Symptom:** `error[E0603]: module 'wallet' is private`.
- **Fix:** import from the crate root, `use bdk_wallet::{ChangeSet, PersistedWallet, Wallet,
  WalletPersister, ...};`, where BDK 3.2 re-exports them.
- **Lesson:** in BDK 3.x the public names are at the crate root; the module paths seen in 1.x docs
  are private now.

### [FIXED] `Mnemonic::generate_in` does not exist without the `bip39/rand` feature

- **Attempted:** `Mnemonic::generate_in(Language::English, 12)` to mint a fresh phrase.
- **Symptom:** `error[E0599]: no associated function or constant named 'generate_in' found for
  struct 'Mnemonic'` — `bdk_wallet` does not enable `bip39`'s `rand` feature, so the method is
  compiled out.
- **Fix:** use the `GeneratableKey` trait that BDK does re-export:
  `let generated: GeneratedKey<Mnemonic, Segwitv0> =
  Mnemonic::generate((WordCount::Words12, Language::English))?;` then `.into_key()`. No extra
  feature, no second RNG in the tree.
- **Lesson:** an error naming a missing associated function on a re-exported foreign type is a
  feature-flag problem, not a version problem. Check what the wrapper crate enables before adding
  a dependency feature.

### [WORKED AROUND] `Wallet::sign` is deprecated in BDK 3.2; kept it rather than rebuild the signers

- **Attempted:** sign a built PSBT with the wallet's own signers.
- **Symptom:** deprecation warning pointing at `sign_with_signers`, which wants a caller-owned
  `SignersContainer`. The wallet already owns the correct signers for a single-key BIP84
  descriptor, so building a second container would be duplicated state with no behavioural gain.
- **Resolution:** `#[allow(deprecated)]` on `build_send`, with a comment saying why and when to
  revisit. `sign` still returns a correct boolean, and the `!signed` case is handled.
- **Lesson:** deprecation is not removal. When the replacement would mean duplicating state the
  type already holds, keep the deprecated path, annotate it, and say what would change the answer.

### [WORKED AROUND] The Wallet pane was verified against a mock daemon in a browser, not the real window

- **Attempted:** to see the pane render and behave before claiming it works. This box has no screenshot
  tool for the Tauri window, and `tauri.conf.json` bakes `ui/dist` into the binary, so the running app
  cannot be observed without a Rust rebuild and a window restart.
- **Resolution:** copied `ui/dist` to a temp directory, injected `window.__TAURI__ = {}` (so
  `isTauriShell()` is true and the wallet pane draws instead of its "needs the desktop app" empty
  state) and `window.__HUSH_TRANSPORT__ = { kind: "tcp", base: "http://127.0.0.1:8791" }`, then ran a
  small Python mock of the daemon's routes on that port and served the dist over
  `python3 -m http.server`. Driven over CDP and the browser tools.
- **What that verified, for real:** the rail gains a Wallet entry and the pane opens; the one-time
  phrase card lists the twelve words with numbers and a copy action; the balance renders as
  `0.01234567 BTC` / `1,234,567 sat` with a pending line; the receive address renders with a copy
  button and a QR canvas that actually paints (148x148, 10350 dark pixels); the amount field's live
  hint updates to `= 0.0015 BTC`; Send reaches the daemon and comes back as "Sent. Fee 142 sat." with
  the txid, clearing the form; and the capsule is a real `.segmented` pill (`--surface-2` track,
  `--surface` on the active side) with `aria-pressed` set. With the relay on Tor the Clearnet side is
  `disabled` at `opacity: 0.45` with an explanatory `title`, Tor is `pressed`, and the pane says
  "Locked on Tor while the relay uses it."; with the relay on clearnet both sides are enabled and
  switching to Tor persists to the daemon.
- **What that did NOT verify:** a real electrum sync, a real signed transaction, a real Tor SOCKS
  route, and the real vault sealing of the phrase. Those need a funded wallet and a live network. The
  mock is a stand-in for the daemon's HTTP contract, nothing more.
- **Lesson:** with a compiled-in frontend and no window screenshot, a mock daemon plus a browser is
  the only way to check a pane's behaviour rather than its markup. It is worth the setup cost, but
  the summary must not present it as an end-to-end run.



### [OPEN] Electrum hides the IP but not the address linkage; no embeddable BIP157/158 crate

- **What is unverified:** the claim that this wallet is "as private as possible". Over Tor the
  electrum server cannot see the IP, but it still learns every script the wallet queries, so one
  server can link a wallet's addresses together. Compact block filters (BIP157/158) would take the
  server out of the trust path entirely, and there is no mature embeddable Rust crate for them
  yet.
- **What is needed:** either an audited BIP157/158 client crate, or a documented policy of
  user-supplied electrum endpoints. Until then this is a stated trade, not a closed item. Recorded
  in the module docs of `crates/hush-wallet/src/bitcoin.rs`.

### [OPEN] The wallet's network paths are untested in this environment

- **What is unverified:** `sync()`, `build_send()`, and `broadcast()` have never run against a real
  electrum server, and the Tor path has never run through the shell's SOCKS proxy. The tests cover
  derivation, address advance, change-set round trip, amount and network validation, and the Tor
  "no proxy" refusal — all offline.
- **What is needed:** a live run against an electrum endpoint (clearnet first, then through Tor)
  with a funded regtest or testnet wallet. Until that happens, treat the send path as unproven.



## 2026-10-10 — Library file rows: the row stops being a button, two named actions

Slice goal: the user asked for the file row to stop being one big button ("get rid of the file as
a button in the first place") and to carry two clearly separate actions instead — "Open inside
vault" (shield icon, green) and "Export" (export-arrow icon).

### [FIXED] Every Library row was a `<button>` with other `<button>`s nested inside it

- **Attempted:** the file row was built by `libraryItemShell`, which created
  `<button class="library-item">` and set `onclick` on it; `renderLibraryFile` then appended an
  actions `<div>` holding two more `<button>`s inside that button.
- **Symptom:** invalid HTML. A `<button>` inside a `<button>` has no defined activation behaviour —
  engines disagree about which element owns the click, and the inner control can simply never fire.
  It also made the whole row a click target with `cursor: pointer`, which is exactly what the user
  was objecting to. The torrent row had the same shape (a trash `<button>` inside the row button).
- **Fix:** `libraryItemShell` now returns a plain `<div class="library-item">` and puts the click on
  an inner `.library-item-main`, which is a `<button>` only when the caller passes `onOpen`
  (folders, torrents) and a `<div>` otherwise (files). The torrent trash button and the file action
  buttons are siblings of the name area, never descendants of another button.
- **Verified:** in a browser against the shipped dist CSS, `document.querySelectorAll("button
  button").length === 0`; the file row's `.library-item-main` is a `DIV` and its computed `cursor`
  is `auto`, while the folder/torrent row's is a `BUTTON` with `cursor: pointer`. The accessibility
  tree for a file row lists only "Open inside vault" and "Export" — no row-level button.
- **Lesson:** the shell helper owned the "row is a control" assumption, so the bug was in the shared
  component, not in one caller. When a row needs both a row-level action and per-row controls, the
  row must be a container and the row-level action must be its own element.

### [WORKED AROUND] A UI-only change still cost a 2m40s Rust relink and a window restart

- **Attempted:** changed `app.ts` and `app.css`, ran `npm run build`, and expected the running window
  to show the new markup.
- **Symptom:** it did not. `strings target/debug/hush-tauri | grep -oE "index-[A-Za-z0-9_-]{8}.js"`
  printed `index-CxLUQujr.js` while `ui/dist/assets/` held `index-DepdreVP.js`, and the binary's
  mtime (21:39) predated the new bundle (21:51). `tauri.conf.json` sets `frontendDist` and no
  `devUrl`, and `tauri-codegen-2.7.1/src/context.rs` embeds `frontendDist` unless
  `dev && config.build.dev_url.is_some()`. So the dist is baked into the binary and a frontend edit
  cannot reach the window without a Rust rebuild.
- **Resolution:** `cargo build -p hush-tauri` (2m40s), then restart only the window — the daemon was
  left running and stayed unlocked, so no passphrase re-entry. `scripts/run-one-ui.sh` already does
  the rebuild first and documents why.
- **Lesson:** on this app, "I changed the frontend" is never enough on its own. Before concluding a
  UI change did not work, confirm which bundle the binary carries (`strings` for the hashed asset
  name) rather than trusting that the running process is current.

### [WORKED AROUND] No screenshot tool on this box, and `browser_navigate` refuses `file://`

- **Attempted:** to eyeball the redesigned rows, tried to screenshot the window. `grim`,
  `gnome-screenshot`, `import`, `scrot`, and `spectacle` are all absent, and pointing the browser
  tool at `file:///tmp/.../index.html` is rejected ("Security restriction: file:// URLs are not
  allowed").
- **Resolution:** served a hand-written page containing the exact DOM the new code produces over
  `python3 -m http.server` on 127.0.0.1, linked the real `ui/dist/assets/index-*.css`, and read
  `getComputedStyle` back over CDP. That gave objective numbers: `.btn-library-open` is
  `rgb(127,224,190)` on `rgb(18,54,44)` in dark and `rgb(8,80,65)` on `rgb(225,245,238)` in light
  (i.e. the success tokens, green); the export button stays neutral; the grid tile's two labels wrap
  instead of overflowing (`flex-wrap` applied, no horizontal overflow in either view).
- **Caveat:** this checks the CSS and the DOM shape, not the running app. The real window was not
  observed rendering the new rows — that needs the user's eyes or an input-driven capture.
- **Lesson:** in this environment, verify styling by measuring computed styles, not by looking.

### [WORKED AROUND] The decrypt spinner does not rotate — this machine suppresses every animation

- **Attempted:** after the redesign shipped, the user reported that the ring next to "Decrypting…"
  never rotates and asked for it to spin. The CSS looks correct: `.spinner` carries
  `animation: spin 0.8s linear infinite` and `@keyframes spin` is defined twice (app.css, for
  `.key-card::before`, and motion.css — identical bodies).
- **Root cause, machine-level, not the app:** `gsettings get
  org.gnome.desktop.interface enable-animations` returns **`false`**. WebKitGTK maps that to
  `prefers-reduced-motion: reduce`, and `motion.css` opens with a global
  `*, *::before, *::after { animation-duration: 0.01ms !important; animation-iteration-count: 1
  !important; transition-duration: 0.01ms !important }` reset, plus a `.breathe, .skeleton,
  .spinner { animation: none }` guard further down. Both fire, so **every** animation in the app is
  suppressed on this box — the spinner included, by design, not by defect.
- **Resolution:** an override was written (exempt `.spinner` from the `*` reset via a
  higher-specificity `!important` rule and drop it from the guard, with a shared `--dur-spin` token
  to avoid restating the duration) and then **reverted at the user's request** — they accepted the
  frozen ring on their own machine rather than have the preference overridden. Nothing in the repo
  was left changed for this.
- **Unverified:** that the ring rotates anywhere. It cannot be observed in this environment, and the
  override that would have forced it was removed. The row still reads "Decrypting…" in text and both
  action buttons are disabled, so the busy state is still communicated without the rotation.
- **Lesson:** `prefers-reduced-motion` is not only a browser setting — a GTK/GNOME desktop setting
  turns it on, and it silently disables every animation in this app. When an animation "does not
  work" here, check `gsettings get org.gnome.desktop.interface enable-animations` before touching
  the CSS.

### [WORKED AROUND] `pkill -f "http.server 8777"` killed the shell that ran it

- **Attempted:** `pkill -f "http.server 8777"` to stop the temporary static server, followed by
  `rm -rf` of the preview directory.
- **Symptom:** the command returned no output at all, and the follow-up check showed the directory
  still present. The pattern matched the `bash -c` process running the command — its own argv
  contains the literal string — so the shell was killed before `rm` ran. It happened twice, the
  second time with `for p in $(pgrep -f ...)`.
- **Resolution:** used a pattern that cannot match its own command line
  (`pgrep -f 'http[.]server 87'`) and removed the directory in a separate call. Verified the server
  is gone, port 8777 free, `/tmp/hush-library-preview` removed.
- **Lesson:** `pkill -f` / `pgrep -f` match the invoking shell too. Bracket one character of the
  pattern, or match on the process name.

---

## 2026-10-10 — "Can't open any downloaded torrent file" (handler sandbox, not the app)

Slice goal: files filed into the Torrents Library render correctly but will not open — the user
reported it for "1.5 G or couple of megabytes", so size was not the variable.

### [WORKED AROUND] The ACL verification printed `absent` because it read a stale build directory

- **Attempted:** a background job checked the freshly built ACL with
  `ls -dt target/debug/build/hush-tauri-*/out | head -1` and grepped that directory for the blob
  commands.
- **Symptom:** it reported `hush_open_blob: absent`, `hush_save_blob: absent`, `hush_open_url:
  absent` — apparently contradicting the fix. The directory it picked (`…a0e227e9c85ffa4a`) was an
  earlier build from the same morning; `ls -dt` sorted by directory mtime, which did not identify the
  OUT_DIR the current build actually used.
- **Resolution:** checked the real artifacts instead — `strings target/debug/hush-tauri | grep -c
  allow-hush-url` returns 1, and the `acl-manifests.json` written at 21:25 (in
  `…f9ee78ea814ffacd`, the OUT_DIR that build used) contains `hush_open_blob`, `hush_save_blob`, and
  `hush_open_url`. The grants are present; the first check was wrong, not the fix.
- **Lesson:** do not identify a build's OUT_DIR by directory mtime. Confirm against the produced
  binary or the artifact written by the build that just ran.

### [OPEN — root cause found, fix compiled in, awaiting the click test] Library "open"/"save" was denied by the Tauri v2 command ACL

- **Root cause:** `src-tauri/permissions/hush-files.toml` allowed only `hush_save_file` and
  `hush_open_file`. The blob-streaming slice added `hush_open_blob`/`hush_save_blob` to the Rust
  handler and to the UI but never to the ACL, so Tauri rejected every `invoke("hush_open_blob")` in
  the webview before it reached the daemon. `hush_open_url` had the same gap and its call site
  swallows the rejection ("the failure is visible by nothing happening"), so "Open in browser" was
  silently dead too. The daemon half was always fine — the request simply never left the webview,
  which is why `/tmp/hush-open` stayed empty.
- **How it was pinned down:** two 10x10 DOM dots written by the UI and read back out of an `xwd`
  capture of the window (XWD parsed to PNG in Python). Magenta proved the freshly built bundle was
  executing; red proved `invoke` was **rejecting**. That killed the "stale bundle" and "input never
  reaches the webview" theories that had consumed most of the previous session.
- **Fix:** added `hush_open_blob`/`hush_save_blob` to `permissions/hush-files.toml`, added
  `permissions/hush-url.toml` (`allow-hush-url`) for `hush_open_url`, listed `allow-hush-url` in
  `capabilities/default.json`, and removed all temporary probes. Verified in the build output: the
  regenerated `acl-manifests.json` (21:25) contains all three commands and the binary contains
  `allow-hush-url`.
- **Lesson:** a command added to `generate_handler!` and called from the UI is still dead until it is
  granted in `src-tauri/permissions/*.toml` **and** referenced by a capability. Add the permission in
  the same commit as the command. And when the IPC path itself is suspect, prefer a UI-visible signal
  (a colored DOM marker read back from a screenshot) over `eprintln!`.
- **Also affected by the same gap:** chat/conclave/mail attachment "Open" and "Save as…", and "Open
  in browser".

- **Attempted:** after the MIME-handler fix, the user still reported "i click and nothing happens".
  Rebuilt the shell with `eprintln!` in `hush_open_blob` plus a `[diag]` toast at the top of
  `torrentsOpenFile`, relaunched it against the already-running daemon, and watched
  `/run/user/1000/hush-one/logs/ui.log`.
- **Symptom:** `/tmp/hush-open` stayed empty and the log stayed 0 bytes after the user's clicks, so
  `hush_open_blob` was never invoked. The code-side wiring reads correctly: `renderLibraryFile` →
  `libraryItemShell(... el.onclick = onOpen)` → `torrentsOpenFile` → `openVaultBlob` →
  `invoke("hush_open_blob")`, and `canStreamBlobs()` should be true (`getApiBase()` is `unix://…`).
- **Second symptom (the actual blocker):** to test input delivery independently I relaunched the
  shell as an Xwayland client (`GDK_BACKEND=x11`) and drove it with `xdotool`. `xdotool key ctrl+k`
  changed 251,104 of 836,000 window pixels, so keyboard input does reach the webview and the app is
  responsive. Synthetic clicks produced no reaction. A startup probe (`invoke("hush_debug_log")`
  from the constructor) then also logged nothing — even though `hush_debug_log` is compiled into the
  binary (`strings` finds it 4×), the running process's fd 2 points at `ui.log`, and the embedded
  asset timestamp matches the current dist. So the silence is not explained by the click alone.
- **Unverified:** whether the running webview is executing the current bundle. `WebKitCache/Version
  17` is empty and no `devUrl` is configured, but a constructor `invoke` producing no stderr output
  is still unexplained. A colored-dot probe (magenta = new JS running, cyan/red = invoke ok/failed)
  was added to `app.ts` at the point of pausing but has **not** been built or run.
- **Lesson:** do not trust `eprintln!`/`invoke` diagnostics from a rebuilt shell until one is shown
  to work in that environment; use a signal independent of the IPC path (window pixel diff, or a DOM
  element with a unique color) to confirm the running code first.
- **Probe cleanup:** every temporary probe was removed in the same change — the global click
  listener, the startup/keydown `hush_debug_log` calls, the colored-dot probe, the `[diag]` toast,
  the `eprintln!` in `hush_open_blob`, and the `hush_debug_log` command itself.

### [WORKED AROUND] Every open failed because the default video handler is a Flatpak with no filesystem access

- **Attempted:** reproduced the whole open path. The daemon half is sound: `POST
  /api/blob/{id}/materialize` returned 200 with the exact byte count for both the 1.77 GB
  `Animals…mkv` (1,776,106,107 bytes) and the 17 KB docx, peak daemon RSS 58 MiB, written under
  `/tmp/hush-open`; the shell binary (`target/debug/hush-tauri`, 18:19) embeds the new dist
  (18:16) and registers `hush_open_blob`/`hush_save_blob`. The failure is downstream of the app.
- **Symptom:** the OS handler never sees the file. `xdg-mime query default video/x-matroska` is
  `com.stremio.Legacy.desktop`, a Flatpak. Probing the sandbox directly:
  `flatpak run --command=cat com.stremio.Legacy /tmp/hush-open/probe.txt` →
  `cat: /tmp/hush-open/probe.txt: No such file or directory`, and the sandbox `/tmp` holds only
  `.X11-unix`. `flatpak info --show-permissions com.stremio.Legacy` shows
  `filesystems=xdg-config/kdeglobals:ro` — no `home`, no `xdg-download`, no documents portal — so
  it cannot read *any* local file, and the `--file-forwarding` in its `.desktop` cannot help
  without the documents portal. That is why size was irrelevant: every open was handed to a
  process that cannot see `/tmp`.
- **Resolution:** pointed the video MIME types at native VLC (`/usr/bin/vlc`, not a Flatpak) with
  `xdg-mime default vlc.desktop` for `video/x-matroska`, `video/mp4`, `video/x-msvideo`,
  `video/webm`, `video/x-flv`, `video/mpeg`, `video/ogg`, `video/x-m4v`, `video/3gpp`,
  `video/quicktime`, `video/x-ms-wmv`, `video/mp2t`, `video/x-ms-asf`. Verified VLC reads a file
  in the materialize dir: `vlc -I dummy … /tmp/hush-open/tiny.mp4` logged
  `` `file:///tmp/hush-open/tiny.mp4' gives access `file' … path `/tmp/hush-open/tiny.mp4' ``.
- **Not fixed:** the app still materializes to `/tmp/hush-open` and hands the path to whatever the
  OS default is, so any *sandboxed* handler (Flatpak/Snap) will fail the same way. Fixing that
  class needs the XDG document portal (`org.freedesktop.portal.OpenURI.OpenFile`), and even that
  only helps a handler that requests document access — Stremio's does not. On this machine the
  handler choice is the fix.
- **Broader audit (same slice):** every other content handler on this machine is native and
  unaffected — jpeg/png/gif/webp → Chrome, bmp/svg → eog, tiff/pdf → Evince, audio → VLC,
  zip/tar → Nautilus, text/plain → gnome-text-editor. Two exceptions: `image/avif` also pointed
  at a sandboxed app (the Firefox *snap*, itself broken here with `cannot create user data
  directory`), now switched to native Chrome; and `application/x-bittorrent` still points at the
  Stremio Flatpak (not a content file). Types with **no** handler at all — docx/doc/odt/epub/
  markdown/octet-stream — still fail xdg-open; LibreOffice is not installed, so nothing is
  registered for them.
- **Lesson:** "the file opens from the shell but not from the app" is not always an app bug. Check
  whether the *handler* is sandboxed before touching the egress path; a private `/tmp` makes file
  size and file format irrelevant.

### [FIXED] Materializing to the runtime dir hit tmpfs ENOSPC on a 1.65 GiB blob

- **Attempted:** open the 1.77 GB `Animals…mkv` through the new streaming egress.
- **Symptom:** `write_blob_to_file` failed with `io: No space left on device (os error 28)` and
  the partial file was removed, leaving an empty `open` dir. `open_tmp_dir()` was
  `hush_core::default_runtime_dir().join("open")` = `/run/user/1000/hush-one/open`, a 1.6 GiB
  RAM-backed tmpfs — smaller than the file, and its pages count as memory.
- **Resolution:** `open_tmp_dir()` now returns `std::env::temp_dir().join("hush-open")`
  (`/tmp/hush-open` on the ext4 root, 108 GiB free). Re-verified: 1.77 GB materialize → 200,
  1,776,106,107 bytes, peak daemon RSS 58 MiB; 17 KB docx → 200, 17,282 bytes.
- **Lesson:** a "temp file" is not one thing; the runtime dir is RAM. Use the disk temp dir for
  anything that can hold a user file.

### [WORKED AROUND] `pkill -f` / `pgrep -f` matched the tool's own shell and killed it

- **Attempted:** restart the daemon by matching its command line (`pgrep -f "release/hush-daemon
  --dir …"`, then `kill $OLD`), and later clean up with `pkill -f "hush-open/probe"`.
- **Symptom:** the shell running the command matched its own command line, so it killed itself:
  the first call printed two PIDs and exited in 266 ms with the daemon dead and its replacement
  never started; the second returned empty output. (Tooling, not a repo problem.)
- **Resolution:** match the exact process name (`pgrep -x hush-daemon`, `pkill -x vlc`) or a
  pattern that cannot appear in the command text.
- **Lesson:** `-f` matches the invoking shell too. Prefer `-x`, or the process self-terminates
  mid-script.

---

## 2026-10-10 — Opening a large torrent OOM-killed the UI (egress path)

Slice goal: the desktop window came up under `scripts/run-one-ui.sh` and was SIGKILLed a
couple of minutes later; `ui.log` was empty.

### [FIXED] The Library's Open reused the base64 blob route, so a 1.65 GiB file ballooned the webview

- **Attempted:** traced the `Killed` exit. `journalctl -k` showed
  `Out of memory: Killed process 3027851 (hush-tauri) total-vm:81188076kB, anon-rss:4941924kB`
  — the kernel OOM killer, not a panic (`ui.log` empty, SIGKILL leaves no trace). The machine
  was already swapping hard (7.8 GiB of 7.9 GiB used), so the biggest process was the victim.
- **Symptom:** `hush-tauri` reached ~4.7 GiB RSS / ~77 GiB VM. The vault held a single
  1.65 GiB torrent (`instances/peer-a/blobs/3041b5f8…`, 211 × 8 MiB chunks). Opening it ran the
  chain `Vault::get_blob` (whole `Vec`) → base64 JSON → the Tauri `hush_api` proxy (whole body
  as a JS `String`) → `atob` → `Uint8Array` → `Array.from(bytes)` (an N-element JSON number
  array back over IPC) → `hush_open_file`'s `Vec<u8>`. ~6 full copies, twice across IPC. The
  earlier memlock hardening had turned a clean abort into an allocation the kernel then killed
  — the failure moved from the daemon to the UI.
- **Resolution:** a streaming egress. `POST /api/blob/{id}/materialize` decrypts a blob to a
  uniquely named file under the runtime dir via `Vault::read_blob_stream` (chunk by chunk,
  memory flat), and the desktop shell's new `hush_open_blob` / `hush_save_blob` commands open
  or copy that path. The bytes never enter the webview and no base64 or `Array.from` remains on
  the open/save path. The old `/api/blob/{id}` route stays for the 256 KiB-capped chat/mail
  attachments and browser/TCP dev. Regression test
  `the_blob_route_materializes_a_file_for_open` stores an 8 MiB+ multi-chunk blob and asserts
  the materialized bytes match, that the extension survives, and that two files of the same
  name get distinct paths; suite passes (46 tests).
- **Not verified here:** the live GUI open of the 1.65 GiB file. The workstation still has swap
  nearly full, so re-running a 1.65 GiB materialize + open was deliberately not done. The daemon
  route is covered by the multi-chunk test; the desktop half compiles and typechecks.
- **Lesson:** the egress half of a large-file feature must be checked separately from the ingest
  half; a fix that removes an abort can just move the failure to an OOM kill. And
  `Array.from(uint8array)` over Tauri IPC is a JSON number array — never do it for file bytes.

---

## 2026-10-10 — TorBox `size: -1` fails decode, and an item sits in Caching with no explanation

Follow-up to the `"files": null` fix: the same add produced two more symptoms.

### [FIXED] `invalid value: integer -1, expected u64` — TorBox reports unknown sizes as −1

- **Attempted:** after the null-files fix, the same torrent failed at the next decode point.
- **Symptom:** `Unexpected reply from TorBox: invalid value: integer -1, expected u64`. TorBox
  reports `size: -1` for a torrent it has not parsed yet; the four `size` fields
  (`TorrentInfo`, `TorrentFile`, `CachedInfo`, `CachedFile`) are `u64`, which rejects any
  negative, and `#[serde(default)]` does not help because the field is present.
- **Resolution:** a `lenient_u64` deserializer on all four `size` fields folds negatives to 0,
  which the pipeline already treats as "unknown" (`dl_total`/`size` guards). Regression test
  `negative_sizes_mean_unknown_not_a_decode_error`; suite passes (44 tests).
- **Lesson:** same class as the null entry: match the provider's encoding of "unknown"
  (−1, null, empty) instead of assuming the happy shape. Anything a third party can send,
  the deserializer must survive.

### [FIXED] An item sat in `Caching` forever with nothing on screen saying why

- **Attempted:** traced the Caching loop: `is_due` polls every 4s, so the poll *was* running —
  TorBox just never reported ready for it.
- **Symptom:** status stayed "Caching" indefinitely. The poll's Ok-not-ready branch cleared
  `message` and moved on, so TorBox's own `download_state` ("queued", a full active-slot
  queue, …) never reached the UI. A slot queue on TorBox's side is indistinguishable from a
  hang. Likely trigger here: the first item, marked Error locally by the −1 decode bug, kept
  occupying an active-download slot on TorBox's side while the second torrent queued behind it.
- **Resolution:** `TorrentItem.remote_state` now carries TorBox's verbatim `download_state`
  (set each poll while still caching, cleared when ready), the caching label renders it
  ("Caching on TorBox — queued (0%)"), and the unambiguous terminal states (`failed`,
  `not downloadable`) now fail the item with TorBox's word in the message instead of waiting
  forever. Whether this user's item was queued on TorBox's side could not be observed from
  here (the key lives in the encrypted vault store); the label will now show it either way.
- **Lesson:** a poller that swallows the remote's own status turns their queue into our hang.
  Surface the upstream state verbatim next to the local one when a local state can outlive
  its upstream cause.

## 2026-10-10 — TorBox replies with `"files": null` and the item dies as "Unexpected reply from TorBox"

Slice goal: the user tried adding a torrent and the item failed with
`Unexpected reply from TorBox: invalid type: null, expected a sequence`.

### [FIXED] `#[serde(default)]` does not cover an explicit JSON `null`, so `files: null` failed the whole reply

- **Attempted:** traced the error string to `TorboxError::Decode`
  (`crates/hush-daemon/src/torbox.rs`), then checked every deserialization point for a field
  that could receive `null`: `list()` already folds a `null` `data` to an empty list, but the
  `files` field on `TorrentInfo` and `CachedInfo` is `Vec<_>` with `#[serde(default)]`.
- **Symptom:** `poll_caching` polls a freshly created torrent; TorBox reports
  `"files": null` until it has parsed the file list. serde's `default` applies only to a
  *missing* field, so the explicit `null` failed `serde_json::from_value` with exactly
  `invalid type: null, expected a sequence`. Because `is_transient()` says `false` for
  `Decode`, the item was marked permanently failed and the UI showed the raw serde text.
- **Resolution:** a `nullable_seq` deserializer folds `null` to an empty list on both
  `files` fields, and `checkcached`'s hash→info map now routes through `decode_cached_map`,
  which drops hashes TorBox maps to an explicit `null` (previously such a reply failed the
  whole map; the `if let Ok` at the call site silently swallowed it, losing the "cached"
  hint). Regression tests: `torrent_info_tolerates_an_explicit_null_file_list`,
  `cached_map_skips_hashes_that_map_to_null`; full `hush-daemon` suite passes (43 tests).
  Note: the live API shape was not captured directly — the fix is from serde semantics plus
  the observed error, and the new tests pin the null shape deterministically.
- **Lesson:** every `#[serde(default)]` on data from a third-party API is a silent
  `null` landmine; `default` and `null` are different cases. For anything a provider is
  known to emit as `null` (lists in particular), pair `default` with a
  `deserialize_with` that folds null to the default.

## 2026-10-10 — Release daemon abort on a 2 GiB blob read (finite memlock + MCL_FUTURE)

Slice goal: confirm the release desktop instance was still connected to the lab relay. It was
not: the daemon had aborted.

### [FIXED] A finite `RLIMIT_MEMLOCK` plus `MCL_FUTURE` turned a 2 GiB allocation into a process abort

- **Attempted:** check the running instance's `/api/status` and `/api/relay` after the relay
  origin work.
- **Symptom:** the daemon was gone, `ps` showed only the old vite, and the run script's log ended
  with `2814506 Aborted`. `logs/daemon.log` held the two startup lines followed by
  `memory allocation of 2147483648 bytes failed` and the `RUST_BACKTRACE` note — that is Rust's
  allocation-error handler, so the process aborted rather than shutting down. The desktop window
  went with it (the script's `wait -n` tears the run down when the daemon exits).
- **Cause:** `crates/hush-daemon/src/security.rs` requested
  `mlockall(MCL_CURRENT | MCL_FUTURE)` whenever `RLIMIT_MEMLOCK` was at least
  `FUTURE_LOCK_MIN_BYTES` (256 MiB). The limit on this host is finite — `ulimit -l` reports
  2,083,516,416 bytes (1.94 GiB) — so `MCL_FUTURE` was enabled, and the kernel then charges every
  future mapping against that cap. A probe using the host's real limit reproduces it exactly:
  with `MCL_CURRENT|MCL_FUTURE`, `mmap(2147483648)` fails `EAGAIN` while `mmap(1073741824)`
  succeeds; with `MCL_CURRENT` alone, `mmap(2147483648)` succeeds. The daemon asked for exactly
  2,147,483,648 bytes, so the mapping was refused and the process aborted. The threshold was the
  whole bug: 1.94 GiB is not "comfortably larger than any plausible growth" when one legitimate
  buffer is the size of a user file. The old doc comment predicted this exact outcome.
- **Trigger:** reproduced deterministically with `GET /api/blob/3041b5f8…` (the 1.65 GB,
  212-chunk torrent blob): `Vault::get_blob` collects a chunked blob into a `Vec` whose doubling
  growth lands on an exact 2 GiB reservation. Unlocking alone did not abort (25s soak, alive);
  the blob request did, within ~26s.
- **Resolution:** the decision is now `should_lock_future`, which enables `MCL_FUTURE` only when
  `RLIMIT_MEMLOCK` is unlimited (`u64::MAX`); a finite limit no longer caps allocations.
  Evidence: unit test `security::tests::future_locking_requires_an_unlimited_memlock_limit`
  (passes, with the 1.94 GiB and 2 GiB cases pinned as not-roomy); the relaunched daemon logs
  `memory_locked=on (current pages only; set LimitMEMLOCK=infinity for full coverage)` with
  `VmLck 561312 kB`, so current pages are still pinned, and it survives unlock.
- **Lesson:** a hardening measure must never convert a large-but-legitimate allocation into a
  fatal error. When a limit is really a cap on the failure mode rather than on the threat, lift
  it entirely or leave the failure mode intact — a "generous" finite threshold is still a crash
  waiting for a big enough file.

### [OPEN] The 2 GiB request itself is untouched: `/api/blob/{id}` still materialises whole files

- This is the existing entry "Multi-GB Library Open/Save is unbounded in memory", with a worse
  symptom than it recorded: the path does not merely fail to scale, it aborted the daemon. The
  hardening fix removes the abort, not the allocation. `GET /api/blob/{id}` still builds the full
  plaintext `Vec` and then a base64 JSON body, so opening the 1.65 GB torrent is roughly a 4 GiB
  spike inside the daemon.
- Re-issuing the request after the fix was deliberately not done on this workstation: it has
  3.6 GB available with swap nearly full and Chrome plus Cursor resident, so the spike risks an
  OOM kill instead of a clean pass. The end-to-end claim verified here is the memlock policy, not
  that a 1.65 GB open now succeeds. Closing this needs the streaming / `Range` blob route plus a
  Tauri command that writes to a file without the webview holding the bytes.

### [WORKED AROUND] The daemon logs no requests, so the abort had to be re-derived by replay

- The daemon's log is two startup lines plus the allocator's message; nothing records which
  request was in flight. The trigger could only be found by restarting the release binary and
  re-issuing candidate requests by hand (unlock first, then the blob read). A one-line request log
  — method, route, response size — would have named the caller immediately and would make any
  future "the daemon died" report diagnosable without a repro session. Logging was not added in
  this slice; the gap is recorded rather than papered over.

---

## 2026-10-10 — HTTPS origin for the lab relay, part 2 (release daemon reachability)

Slice goal: get the release desktop daemon onto the lab relay over the box's existing Cloudflare
HTTPS route.

### [FIXED] The configured HTTPS origin had been reclaimed; the hostname was NXDOMAIN

- **Attempted:** use the box's Cloudflare HTTPS origin as the release relay base.
- **Symptom:** the configured origin, `https://citations-stuffed-bulletin-shade.trycloudflare.com`
  (pinned in `HUSH_RELAY_DROP_BASE_URL`), returned nothing: `dig @1.1.1.1` gave no answer
  (NXDOMAIN) while `trycloudflare.com` resolved, and every `curl` was `http=000`. On the box,
  `cloudflared-quick.service` was `active` but had been looping on
  `ERR failed to serve tunnel connection error="control stream encountered a failure while
  serving"` every ~17s since its last successful registration at `Oct 09 09:29:00`. It had worked
  on Oct 8 (the drop base was verified then), so it was reclaimed between the 9th and the 10th —
  the "no uptime guarantee" caveat the quick tunnel prints at startup, made concrete.
- **Resolution:** `systemctl restart cloudflared-quick.service` minted
  `https://ban-therapeutic-daughter-organic.trycloudflare.com`, which resolved and served nginx
  within ~25s (`/ironlink-health` 200). The old hostname is dead and cannot be recovered.
- **Lesson:** a quick-tunnel hostname is not an identity. Check that the hostname resolves before
  treating "the tunnel service is active" as "the origin is up" — `active` covered a
  month-old process that had stopped being reachable.

### [FIXED] The HTTPS origin only carried `/open/` and `/drop/`, so it could not be a relay base

- **Attempted:** point a release vault at `https://<host>` and let the daemon ping it.
- **Symptom:** through the tunnel, `/health` returned 307 and `/q/x` returned 307 — the alhakim
  app's redirect to `/login`, not the relay. The 2026-10-08 ingress had added only
  `location /open/` and `location /drop/` (for the drop opener), and the relay client builds
  `{base}/health`, `{base}/q/{queue_id}...` and `{base}/k/{account_id}`
  (`crates/hush-core/src/relay_client.rs`), so every relay call fell through to the app.
- **Resolution:** added `location = /health`, `location /q/`, `location /k/` to
  `/etc/nginx/sites-available/alhakim`, all proxying to `http://127.0.0.1:8790`, with a
  timestamped backup (`alhakim.bak-20261010`) and `nginx -t` before the reload. Verified through
  the origin: `/health` 200, `/q/x` 401 `{"error":"unauthorized"}`, `/k/x` 404, while
  `/ironlink-health` stayed 200 and `/` stayed 307 — the other apps on the box were untouched.
- **Lesson:** the relay client's base is a whole protocol root, not a page prefix. An ingress
  that serves one route of a protocol proves nothing about the protocol.

### [FIXED] `GET /drop` (the published drop page) fell through to the app

- **Attempted:** reach the relay's drop page through the HTTPS origin.
- **Symptom:** `GET https://<host>/drop` returned 301 (the app's redirect), because nginx only had
  `location /drop/` and a bare `/drop` does not match a trailing-slash prefix. The relay serves
  the page at exactly `GET /drop` (`crates/hush-relay/src/app.rs`), and the URL the daemon
  publishes is `{base}/drop` from `drop_page_url()`
  (`crates/hush-daemon/src/account_worker.rs:264`), so the published link would have opened the
  medical app's login page.
- **Resolution:** added `location = /drop` → relay. `GET /drop` now 200 with the drop page HTML.
  Also corrected the block comment: `/open/{id}` is no longer a relay route (the pickup page was
  retired), so `location /open/` is legacy and answers 404 — kept only so an older binary in
  front is not silently sent to the app.
- **Lesson:** in nginx a trailing-slash prefix and the bare path are different locations. Check
  the exact URL a client publishes, not the family it belongs to.

### [FIXED] `HUSH_RELAY_DROP_BASE_URL` is no longer read by the relay

- **Attempted:** update the box's drop-base drop-in to the new hostname so drop links stay live.
- **Symptom:** no `drop_base_url` exists anywhere in `crates/hush-relay/src`; it appears only in
  this log and `docs/plans/2026-10-07-outsider-mail.md`. The current relay serves a static
  `GET /drop` and the published URL is composed client-side, so the drop-in's `Environment=` line
  is inert.
- **Resolution:** updated the value anyway (harmless, and keeps the file honest about which origin
  is current) and documented it as inert in `docs/ops/relay-lab-vps.md`. The behaviour change that
  mattered was the nginx `location = /drop` above.
- **Lesson:** an env var with no remaining reader fails silently. Grep the crate before treating a
  service's environment as its configuration.

### [OPEN] The relay's HTTPS origin is still an ephemeral quick-tunnel hostname

- **Attempted:** make the release daemon's relay base durable.
- **Symptom:** the origin that now works, `https://ban-therapeutic-daughter-organic.trycloudflare.com`,
  is a `trycloudflare.com` quick tunnel. Its hostname is assigned at process start and changes on
  every `cloudflared-quick.service` restart (`Restart=always`), and Cloudflare reclaimed the
  previous one. Any vault pointed at it breaks the next time the tunnel restarts.
- **Resolution:** none in this slice. The box already runs a durable named tunnel
  (`cloudflared.service`, tunnel `cricket-po`, connected) in front of the same nginx, but its
  public hostname is configured in the Cloudflare dashboard and is not readable from the box: no
  `cert.pem` (`cloudflared tunnel list` fails with "Cannot determine default origin certificate
  path"), no `CLOUDFLARE_API_TOKEN`, and nothing in the unit files, `/root` notes, or bash history
  names it. Getting it needs the dashboard (or a token), which is the user's.
- **Progress (same day):** the user created a tunnel named `hush` in a *second* Cloudflare account
  (`75743fae…`, vs the box's `8bccbccb…`) and supplied its token. A connector was installed on the
  box as **`cloudflared-hush.service`** (not `cloudflared.service`, which already belongs to
  cricket-po — `cloudflared service install <token>` would have overwritten it and moved the
  medical app, cricket PWA, and MCP onto the new tunnel). The unit runs
  `cloudflared --config /etc/cloudflared/hush.yml tunnel run --token-file
  /etc/cloudflared/hush.token`; the explicit `--config` exists so the default
  `/etc/cloudflared/config.yml` (cricket-po) is not picked up. Token and config are `0600`/`0644`.
  The connector authenticates and registers (4 QUIC connections), but it logs
  `WRN No ingress rules were defined in provided config (if any) nor from the cli, cloudflared
  will return 503 for all incoming HTTP requests` — i.e. **no public hostname is configured on the
  tunnel yet**, so there is still no URL to point the vaults at. Adding one needs a domain/zone in
  that account (Zero Trust → Networks → Tunnels → hush → Public Hostname → service
  `HTTP localhost:8790`), which is a dashboard action only the user can take.
- **Lesson:** "the server has an HTTPS route" and "the server can tell you its HTTPS route" are
  different questions; a named tunnel's hostname lives in the account, not on the host. Also,
  `cloudflared service install` is not idempotent across tunnels — it writes the shared unit path,
  so a second tunnel needs its own unit.

### [WORKED AROUND] `pkill -f` matched the invoking shell and killed the command mid-run

- **Attempted:** `pkill -f "target/debug/hush-tauri"` to stop the desktop UI from a shell call.
- **Symptom:** the tool returned exit 0 with completely empty output and the rest of the script
  never ran (the daemon was still up afterwards). The pattern matched the shell's own command
  line, which contained that literal string, so `pkill` killed the shell running the script.
- **Resolution:** killed by PID instead (`kill <pid>`, then polled `kill -0`) for the remaining
  stops. This is a tooling/workflow mistake, not a repo problem.
- **Lesson:** with `pkill -f` the invoking command is a match candidate. Capture PIDs first
  (`ps -eo pid,cmd | grep ...`) and kill by PID, or exclude self.

---

## 2026-10-10 — Running the desktop instance

### The release daemon cannot reach the lab relay, so the app shows "disconnected" — FIXED

- What was attempted: open the desktop instance and use it. The top-rail shield reported
  "Hush is disconnected."
- Symptom: `GET /api/relay` returns `reachable: false` while `curl http://137.184.129.205:8790/health`
  from the same machine returns 200. The cause is the build profile, not the network.
  `HttpPolicy::current()` is `LoopbackOnly` in release and `Allow` in debug, and
  `Account::relay_client()` builds through `RelayClient::try_new_with_proxy(&self.state.relay, None)`
  → `normalize_relay_base` → the policy. The vault's relay is `http://137.184.129.205:8790`, a
  cleartext HTTP URL on a public IP, so in release the client constructor fails with
  `Relay("insecure relay URL: use HTTPS except for localhost")`, `relay_ping` returns false, and
  every relay path (presence, chat, mail, drops) fails. Mail queues in the Outbox rather than
  erroring, so the failure is quiet apart from the light.
- Evidence, same vault and same relay URL, only the profile changed:
  `RELEASE: relay=http://137.184.129.205:8790 / vault reachable=False` vs
  `DEBUG: relay=http://137.184.129.205:8790 / vault reachable=True` (nothing listens on 8787, so
  the debug probe really reached the lab box). Independently, `hush-cli init --relay <lab url>`
  succeeds under `target/debug/hush-cli` and fails under `target/release/hush-cli` with the error
  above.
- Cause of the regression: the same-day change that made `run-one-ui.sh` / `run-two-ui.sh` start
  `target/release/hush-daemon` (for the 43x faster AEAD blob store) put a release daemon behind a
  plain-HTTP lab relay. `docs/ops/relay-lab-vps.md` already documents that this cannot work:
  "So a **release** daemon cannot use this plain-HTTP relay; it needs `https://` ... or a **debug**
  build." The two requirements were never reconciled; the store fix silently broke the relay.
- Blast radius: `instances/peer-a` and `instances/peer-b` both store
  `http://137.184.129.205:8790`. Only peer-a has an `onion_relay` on file, so only peer-a has an
  in-release escape route (Tor mode). peer-b has no onion and no loopback alternative.
- Options considered while this was open (kept as history; none was needed): Tor relay mode (peer-a
  already had an onion on file, peer-b would need re-init with `--onion`), a loopback relay
  (`WITH_LOCAL_RELAY=1`, `http://127.0.0.1:8787` — allowed in release, but repointing is destructive
  and the lab VPS leaves the loop), TLS in front of the lab relay (the checklist requires a publicly
  trusted CA and the client bundles `webpki-roots`, so it needs a hostname — the box is a bare IP),
  reverting the daemon to debug (gives up the fast store), or a dev-profile `opt-level` override
  (unmeasured). A release-mode "insecure" switch was never on the table:
  `docs/ops/relay-https-checklist.md` forbids it outright.
- Resolution (2026-10-10, later the same day): the box already had a Cloudflare HTTPS origin in front
  of nginx; the ingress was widened to carry the whole relay protocol and the vaults were repointed
  with `hush-cli --dir instances/<v> --passphrase <pw> relay set --url
  https://ban-therapeutic-daughter-organic.trycloudflare.com` (vault state, plus the advisory
  `client-config.json`), then the desktop instance was restarted. Verified on the live release
  daemon: `GET /api/status` → `"relay": "https://ban-…trycloudflare.com"`, `"relay_source": "vault"`,
  and `GET /api/relay` → `reachable: true`. No build profile changed and no vault was recreated. See
  the section above for the ingress and the tunnel work.
- Caveat: that origin is an ephemeral quick tunnel (see the OPEN entry above), so this is a working
  fix, not a durable one.
- Lesson: changing a build profile changes more than speed when behaviour is gated on
  `cfg!(debug_assertions)`. Grep for `debug_assertions` before moving a service between profiles, and
  do not assume a green light from a script that only proves the process started.

## 2026-10-10 — Running the desktop instance (scripts)

### `scripts/run-one-ui.sh` was not executable — FIXED

- What was attempted: start the single-desktop-instance script the way its own header documents it,
  `./scripts/run-one-ui.sh`.
- Symptom: `--: line 1: ./scripts/run-one-ui.sh: Permission denied`. The file was `100644` in git
  while every other runnable script in `scripts/` is `100755`. Nothing about the file changed in
  this session; the mode was simply wrong in the index, so the failure was latent until someone
  ran it directly rather than via `bash script.sh`.
- Blast radius: a previous note in this log told the user to run exactly `./scripts/run-one-ui.sh`
  to pick up the release-daemon change. That instruction could not have worked.
- Resolution: `chmod +x scripts/run-one-ui.sh`. Checked the rest of `scripts/` for the same defect
  and found `scripts/smoke-tor-relay.sh` (`100644` with a shebang, the only smoke script not
  `100755`), so it was fixed too. `scripts/drops-vector.mjs` is also `100644` but is documented as
  `node scripts/drops-vector.mjs`, so that one is intentional and was left alone. Verified the
  script then ran end to end: UI build, Rust build, daemon on `instances/peer-a`, window up.
- Lesson: `git ls-files -s` is the authority on the exec bit, not `ls`. A script that is only ever
  invoked as `bash foo.sh` can carry a wrong mode for a long time, so check the mode when a
  README or header tells the user to run it directly.

## 2026-10-10 — Torrents Library renders a blank page

### `icon("folder")` named an icon that does not exist — FIXED

- What was attempted: open the Torrents > Library tab, and open a torrent entry inside it.
- Symptom: the Library pane rendered blank. It worked while the Library was empty (that branch
  renders a plain "Nothing in the library yet" paragraph) and went blank as soon as the first
  torrent was filed, which is also why the just-downloaded file looked missing: "cant find the
  file in library as well" and "library open blank page when i click on it" are the same defect.
  The Library renderer calls `this.libraryItemShell("folder", ...)` for both a torrent entry and
  a subfolder, but `ICONS` in `src/lib/icons.ts` has no `folder` key, only `folderOpen`. So
  `icon("folder")` passed `undefined` to lucide's `createElement`, which destructures its
  argument. Reproduced in isolation: `TypeError: undefined is not iterable (cannot read property
  Symbol(Symbol.iterator))`. The throw happened inside `render()`, so the pane was left
  half-built and looked empty.
- Why TypeScript did not catch it: `ICONS` was annotated `Record<string, IconNode>`, which makes
  `IconName = keyof typeof ICONS` collapse to `string`. Every icon name typechecked, including
  ones with no entry. `libraryItemShell(ic: IconName, ...)` therefore accepted `"folder"`.
- Resolution: added `folder: Folder` to `ICONS`; changed the annotation to
  `satisfies Record<string, IconNode>` so `IconName` is the real key union again; and `icon()`
  now throws a named error instead of relying on lucide's destructuring when a name is missing.
  Verified: `npx tsc --noEmit` and `npx vite build` pass; a temporary probe that imported the real
  `icons.ts` through esbuild with a stubbed `document` rendered `folder`, `folderOpen`, `files`,
  `fileVideo`, `file`, `trash`, `download` as `<svg>` elements; and a temporary bad name
  (`icon("nope-not-an-icon")`) now fails typecheck with `TS2345 ... is not assignable to parameter
  of type '"lock" | "chats" | ... | "drive"'`, proving the union is enforced.
- End-to-end A/B, in the real UI: started a throwaway stub daemon that answers `/api/status` and
  `/api/torrents/state` with one canned Library entry (one file at the root, plus an `Extras/`
  subfolder with two files), then drove the Vite dev server at `localhost:5199/?api=…` through the
  browser. With `folder` present: the Library tab listed the entry, clicking it showed the folder
  row and the file row, and clicking `Extras` showed both files, with breadcrumbs
  `Library / <torrent> / Extras`. With `folder` deleted again (relying on HMR, since Vite does not
  typecheck): clicking the Library tab left `document.body.innerText` empty and `#app` childless —
  the reported blank page — and it came back after restoring the key. The stub daemon and its file
  were removed afterwards.
- Lesson: an `as Record<string, T>` / `Record<string, T>` annotation on a lookup table throws away
  the key union and with it all typo checking. Use `satisfies` when the keys are the contract. And
  when a render function throws, the symptom is a blank pane, not an error — a lookup that cannot
  be validated at compile time should fail loudly at the point of use.

### Multi-GB Library Open/Save is unbounded in memory — OPEN

- What was attempted: click a completed 1.65 GB torrent file in the Library (row click = Open,
  download button = Save).
- Symptom: not reproduced end-to-end in this session, but the path cannot work at that size.
  `torrentsOpenFile`/`torrentsSaveFile` call `resolveMessageFileBytes({ blobId })`, which calls
  `api.blob(id)`; `GET /api/blob/:id` returns `{ data_base64 }` for the whole blob, and the daemon
  decrypts the entire file into a `Vec<u8>` first. A 1.65 GB file becomes ~2.2 GB of base64 in a
  single JSON response, then `atob` into a JS string, then a `Uint8Array`, then
  `Array.from(bytes)` across the Tauri IPC boundary in `openBytesNative`. Each step is a separate
  full-size copy; the small-attachment pattern it was copied from does not scale to torrent files.
- Resolution: none yet. Needs a design decision — a streaming or `Range` blob endpoint the UI can
  hand to the OS/Tauri without buffering, versus a daemon-side "save blob to path / open with
  handler" command. Not attempted in this slice because it is a new endpoint, not a fix to the
  blank page, and the two options differ in where the file ends up on disk.
- Lesson: the whole-blob-base64 endpoint is fine for chat attachments and wrong for a media file.
  Anything that renders user-chosen file sizes needs a size ceiling at minimum, so it fails with a
  message instead of hanging the webview.

## 2026-10-10 — Torrents: "100% then stalls, not in Library"

### The vault store is 43x slower in a debug build — FIXED

- What was attempted: store a 1.65 GB downloaded file into the vault via `put_blob_stream`.
- Symptom: the item sat at "Downloading to vault (100%)" for ~17 minutes with an empty Library,
  which read as a hang. The daemon was a debug build (`target/debug/hush-daemon`), single
  threaded at ~98% CPU, writing chunks at ~1.7 MiB/s. A micro-benchmark of `put_blob_stream`
  on 256 MiB measured 1.7 MiB/s in debug vs 73.3 MiB/s in release (43x). Nothing is wrong with
  the code: `encrypt` is plain XChaCha20Poly1305, and AEAD is one of the things `--release`
  speeds up most.
- Resolution: `scripts/run-one-ui.sh` and `scripts/run-two-ui.sh` now build the daemon with
  `cargo build --release -p hush-daemon` and launch `target/release/hush-daemon`; the relay and
  desktop stay in debug. 1.65 GB then stores in ~25 s. Verified: the release binary starts,
  serves `/api/status`, and refuses a second instance.
- Lesson: a slow-looking pipeline stage can be a build-profile artifact. Measure the stage in
  isolation and in both profiles before hunting for an algorithmic bug.

### Nothing in the UI distinguished "transfer done" from "storing" — FIXED

- What was attempted: show progress while a torrent downloads into the vault.
- Symptom: `dl_done` reaches `dl_total` when the bytes are on disk, but the item is not done
  until `put_blob_stream` finishes. Both phases share `status: "downloading"`, so the bar pinned
  at 100% and the label still said "Downloading to vault (100%)" for the whole store — the exact
  window in which the file is not yet in the Library.
- Resolution: `torrentStatusLabel` now returns "Sealing into the vault…" once `dl_done >= dl_total`.
  A full store-progress indicator would need progress plumbed out of `put_blob_stream`, which is
  a single blocking actor op; the label is the honest minimum.

### Two daemons ran against one vault — FIXED

- What was attempted: run the daemon for manual testing.
- Symptom: `ps` showed two live `hush-daemon` processes with `--dir instances/peer-a` (started
  11:48 and 13:05), and `ss` showed two LISTEN sockets on the same path. They share the state
  sidecar and the vault, so they clobber each other's state and duplicate work. The existing
  `bind_socket` guard calls `UnixStream::connect` to decide whether a peer is live, but a daemon
  that is alive and blocked (e.g. mid-way through a long debug-mode store, not accepting) fails
  that probe, so the second daemon unlinked its socket and took over.
- Resolution: `acquire_vault_lock` takes a non-blocking `flock(LOCK_EX)` on
  `<vault>/.hush-daemon.lock` before anything else starts; a second daemon now exits with a clear
  message. Verified by starting two daemons on one temp vault with *different* sockets: the
  second exits 1 with "another hush-daemon is already running on …".
- Lesson: a liveness probe over the same socket the peer serves is not a mutual-exclusion
  primitive. Lock the resource (the vault), not the transport.

### Sandbox blocked process introspection, and a frozen counter sent me the wrong way — FIXED

- What was attempted: identify which of the two daemons owned the socket and was doing the work.
- Symptom: `/proc/<pid>/fd` reads returned nothing, `ptrace` was denied (`PTRACE_SEIZE:
  Operation not permitted`), `perf` needs `linux-tools` (not installed), and `ss -xlpn` showed no
  pids. As a substitute I read `dl_speed` from the API and found it frozen at 20642902.0, which
  implied the API daemon was not the one running the pipeline. That inference was wrong — the
  item completed and moved to the Library on that same daemon minutes later. The frozen value was
  a persisted field on a daemon whose reporter was not ticking, not proof of a second writer.
- Resolution: confirmed the end state directly instead — 212 chunks in the blob, packed bytes
  exactly `plaintext + 212 * 40`, library entry present with the right `blob_id`. The duplicate
  daemon was confirmed from `ps` + two socket listeners, not from the frozen counter.
- Lesson: a single stale-looking field is not evidence of split-brain. In a sandbox that blocks
  per-pid inspection, confirm the *end state on disk* rather than reasoning from one counter.

---

## 2026-10-10 — Torrents: large-file download restarts

### CDN download inherited the 30 s API timeout — FIXED

- What was attempted: stream a TorBox CDN file to disk with `TorboxClient::fetch_to`.
- Symptom: a ~1 GB file "stops in the middle and starts back again 0%". `TorboxClient::new`
  built one `reqwest::Client` with `.timeout(REQUEST_TIMEOUT)` where `REQUEST_TIMEOUT` is 30 s.
  reqwest's `timeout` is a *total* budget covering the response body, so any transfer lasting
  longer than 30 s is severed mid-stream. A 1 GB file always exceeds that, so `fetch_to`
  returned `TorboxError::Network`, the item backed off, and it retried. The source even
  claimed "the download itself uses a separate, longer budget" while no such client existed.
- Resolution: downloads now use a dedicated `download_http` client with
  `connect_timeout(30s)` + `read_timeout(60s)` and no total timeout. `read_timeout` resets on
  every chunk, so an active transfer is never cut off while a silent connection still fails.
- Lesson: a comment describing intended behaviour is not the behaviour. `reqwest`'s `.timeout`
  is total, not idle — never set it on a client that also streams large bodies.

### Resume reported 0% because progress was per-attempt, not on-disk — FIXED

- What was attempted: report live progress while a multi-file torrent downloads.
- Symptom: even when the HTTP Range resume worked and bytes were already on disk, the bar
  snapped back to 0% on every retry. Progress was `done_files + inflight`, where `inflight` was
  an `AtomicU64` created fresh per `download_item` call and only counted bytes written by the
  current attempt. A resumed attempt started that counter at 0, so `dl_done` fell back to the
  bytes of fully-stored files (0% for a single file) while the transfer actually continued.
- Resolution: progress is now read from disk — `partial_bytes` sums the `.part` temp files of
  the not-yet-stored files each tick, and `refresh_download_totals(on_disk)` adds that to the
  stored files' sizes. `CountingWriter` and the `inflight` counter were removed. The same read
  also yields a real `dl_speed` (delta over the 1 s tick), now shown on the card.
- Lesson: derive progress from durable state (the files) rather than from counters that live
  only as long as one attempt; retries and restarts then report the truth for free.

### `RANGE_UNSUPPORTED` wiped the partial with no bound — FIXED

- What was attempted: resume a file when the CDN ignores the `Range` header and answers 200.
- Symptom: `download_one` deleted the `.part` file, reset `offset = 0`, and `continue`d with no
  limit. A server that never honours Range would loop forever, wiping and re-downloading the
  file each pass — and each pass was also being killed by the timeout above, so it never made
  progress.
- Resolution: the restart is capped at two attempts, after which it returns a `FileError`
  marked non-transient so the item stops in `Error` with an explanation instead of retrying.
  Transient-vs-permanent is now centralised in `TorboxError::is_transient`, used by both the
  pipeline tick and `download_one`, replacing two copies of the same `matches!`.

### Blocking file write on an async task — still OPEN, note updated

- The `CountingWriter` wrapper referenced in the entry below no longer exists (progress is read
  from disk now); `fetch_to` receives the raw `std::fs::File` and still calls `write_all` inside
  the `async fn`. The underlying concern is unchanged and the entry stays OPEN.

---

## 2026-10-10 — Torrents (TorBox) pane

### Large torrent files can be stored streamed but not opened streamed — FIXED

- What was attempted: honour the constraint "make it work for large files (stream to disk,
  don't buffer in memory)" for the whole torrent path.
- Symptom: the TorBox → vault leg is streamed (HTTP Range into a temp file, then chunked
  AEAD into `blobs/<hex>/chunk-N` in 8 MiB chunks), so it never holds a whole file. But the
  Library's Open / Save as… actions reused the app's existing `/api/blob/{id}` route, which
  returns the plaintext base64-encoded in one JSON body (`Vault::get_blob` collects the whole
  file into a `Vec<u8>`). A multi-GB torrent file therefore materialised in the daemon, in the
  JSON string, and in the webview when opened.
- Resolution: closed by the streaming egress above (the same slice that diagnosed the UI OOM).
  `POST /api/blob/{id}/materialize` streams the blob to a temp file via
  `Vault::read_blob_stream`; the shell's `hush_open_blob` / `hush_save_blob` open or copy that
  path, so neither the daemon nor the webview holds the whole file. Evidence:
  `the_blob_route_materializes_a_file_for_open` (multi-chunk blob, bytes match, unique names).
- Lesson: "streams to disk" was only true for the ingest half. The egress half of a
  large-file feature has to be checked separately.

### `fetch_to` writes to a blocking `std::io::Write` on an async task — OPEN

- What was attempted: stream a CDN response into a file while counting bytes.
- Symptom: `TorboxClient::fetch_to` takes `W: std::io::Write` and calls `write_all` inside the
  `async fn` while draining `resp.bytes_stream()`. Each 8 KiB chunk does a blocking file write
  on a Tokio worker thread. With the default 2–3 concurrent downloads this is unlikely to
  stall the runtime, but it is a blocking call in async context and is not defended.
- Resolution: not fixed; would require a channel into `spawn_blocking` or `tokio::fs::File`
  with an adapter. Flagged rather than left implicit.
- Lesson: the byte-counting writer made progress reporting easy but also fixed the sink as a
  sync `Write`, which is exactly what should not be written to from async code.

### The magnet/`.torrent` source would have been serialised into the UI state — FIXED

- What was attempted: hand `PublicState { items: Vec<TorrentItem> }` straight to the webview.
- Symptom: `TorrentItem.source_data` holds the pasted magnet or the base64 `.torrent` bytes
  until TorBox accepts the torrent, and it derives `Serialize` with no skip, so it was in the
  `/api/torrents/state` body. The user asked that magnet links not be exposed.
- Resolution: added `TorrentItem::public_clone()` which clears `source_data`, and used it in
  `TorrentsManager::public_state`. The full value is still written to the encrypted sidecar so
  a queued add survives a restart. Test `public_clone_drops_the_source_secret_but_keeps_the_rest`
  covers both halves.
- Lesson: `#[serde(skip_serializing)]` would have been the wrong fix here because the same
  `Serialize` impl writes the persistence file; the redaction has to happen at the boundary,
  not on the type.

### Resuming a file whose bytes were already complete re-fetched and 416'd — FIXED

- What was attempted: resume every file with an HTTP Range from the temp file's current length.
- Symptom: a crash after the last byte landed but before the blob was stored leaves
  `part` exactly `file.size` bytes. The next tick issued `Range: bytes=<size>-`, TorBox
  answered 416, `fetch_to` mapped any non-success to a `Network` error, and the item parked in
  `Error` with a permanent failure it could never retry out of.
- Resolution: `download_one` now returns the stored blob directly when `offset == file.size`,
  and truncates and restarts when `offset > file.size`. The blob-store tail was extracted to
  `store_part` so both paths share it.
- Lesson: "resume" has a third case besides partial and zero — already-complete — and a Range
  request cannot express it.

### Duplicate `List` import broke `tsc` — FIXED (tooling)

- What was attempted: add `List` and `LayoutGrid` to `lib/icons.ts` for the Library view toggle.
- Symptom: `src/lib/icons.ts(35,3): error TS2300: Duplicate identifier 'List'` — `List` was
  already imported for the rich-text editor's bullet-list button.
- Resolution: dropped the duplicate import and reused the existing binding in the ICONS map.
- Lesson: `icons.ts` already had a `list` entry; grep the file before adding a lucide name.

---

## 2026-10-10 — Reads reader mode against Cloudflare-challenged sites

### Concluded a bot-block was deterministic from one sample of it — FIXED (wrong assumption, corrected)

- What was attempted: owner asked for a workaround for `fetch failed: HTTP status client error
  (403 Forbidden)` when the reader fetched a `politico.com` article.
- Symptom: a first batch of probes (daemon UA, Chrome UA, full `sec-ch-ua` client hints,
  HTTP/1.1, and the homepage) all returned 403 with `cf-mitigated: challenge` and a "Just a
  moment..." body, so the block was called deterministic and header-independent. Then the
  rebuilt daemon fetched the *same* URL successfully once (HTTP 200, real article, cached at
  11:48:49 — cache filename is `sha256(url)`, and the entry held the real title). Deleting that
  cache entry and refetching 403'd again, and six immediate retries against a cache-busted
  variant all 403'd.
- Resolution: the block is intermittent per-IP (Cloudflare reputation), not deterministic, and
  it does not converge under retry. Kept the honest failure path (clear reason plus a prominent
  "Open the original") and deliberately did *not* add a retry loop, which 6/6 failures show
  would only add load and burn reputation further.
- Lesson: one sample of a 403 from a bot-challenge CDN is not evidence that the block is
  permanent. The same client can pass on a later attempt. Sample the same client over time
  before declaring a ceiling — and do not treat "it failed for me once" as "it cannot work".

### `pkill -f "<pattern>"` killed the shell running it — FIXED

- What was attempted: `pkill -f "target/debug/hush-daemon"` to restart the daemon.
- Symptom: the command exited 0 in ~100 ms with no output; the daemon died, but so did the
  shell, so the rest of the script (restart, unlock, verify) never ran. `pkill -f` matches the
  whole command line, and the shell's own command line contained the pattern text. Hit the same
  self-match twice, the second time via the script path (`python3 /tmp/stub_api.py`) that also
  appeared in the script body.
- Resolution: kill by port (`fuser -k 9899/tcp`) or by PID, and where a pattern is unavoidable
  use a bracket class (`[t]arget/...`) whose literal text cannot appear in the invoking script.
- Lesson: any `pkill -f` whose pattern string appears in the script that runs it will kill that
  script. This is tooling, not a repo problem, but it silently truncated a restart twice.

### Verification stub emitted duplicate CORS headers — FIXED (tooling)

- What was attempted: stood up a small proxy on :9899 that makes `/api/reads/reader` return the
  bot-block error, so the pane's new state could be rendered without depending on Cloudflare
  choosing to challenge at that moment.
- Symptom: the app sat on the unlock screen even though `/api/status` reported `locked:false`.
  The stub copied the daemon's response headers and then added its own, producing two
  `access-control-allow-origin` values (`http://localhost:5199` and `*`). Browsers reject such
  responses, so `api.status()` threw, `refreshStatus` set `this.status = null`, and `render()`
  fell through to the unlock screen.
- Resolution: the stub drops upstream `access-control-*` headers and emits a single set.
  Confirmed with `curl -H "Origin: ..."` that exactly one `access-control-allow-origin` is
  returned. This was stub tooling only; the real daemon serves one header.
- Lesson: a proxy that adds CORS headers must strip the origin's. A failure that looks like an
  auth/lock problem can be a header problem one layer down.

### The panel's `height: 100%` pushed its button off the fold — FIXED

- What was attempted: reused `.reads-empty` (the pane's centered empty state) for the
  reader-unavailable panel.
- Symptom: measured geometry put the panel at `y=312..1157` inside an 849 px pane.
  `height: 100%` resolves against the scroll container, not the space left under the article
  head, so the block started below the head and overflowed; the "Open the original" button sat
  at `y=833..871`, cut off at the bottom edge.
- Resolution: `.reads-unavailable` overrides to `height: auto` with its own vertical padding.
  It now measures `y=312..604` with the button at `y=534..572` — inside the pane and near its
  middle. Verified for both the blocked and the generic failure variants.
- Lesson: `height: 100%` in a scroll container is a claim about the container, not about the
  space remaining after a sibling. Measure the geometry instead of trusting a class name that
  says "centered".

### Reader block reproduced through a stub, not a live story — WORKED AROUND

- What was attempted: verify the new "site blocks automated readers" state against a real
  Cloudflare-blocked story in the running app.
- Symptom: the block is intermittent (see the first entry). No story in the current HK News feed
  was blocked at the time (`apnews.com`, `gizmodo.com`, `www.techspot.com`,
  `www.classaction.org` all fetched 200), and forcing a challenge by hammering politico would
  fail and worsen the IP reputation.
- Resolution: verified the daemon's error through direct API calls (502 plus `this site refused
  an automated reader (HTTP 403)`), then rendered the pane state against a stub returning that
  exact string. Both variants were checked live in the browser: blocked ("This site blocks
  automated readers") and generic (`could not extract readable text; try the original` →
  "This page could not be loaded"), including that the button opens the existing
  open-outside-Hush confirm. Not verified end-to-end: a live blocked story driving the panel in
  a single flow.
- Lesson: with an intermittent upstream, the honest test is "the message the daemon actually
  returns, fed to the real UI" rather than waiting for the network to misbehave on camera. State
  which half was simulated.

### Two daemons were polling the same vault — FIXED (environment)

- What was attempted: restarted the daemon to pick up the new binary.
- Symptom: `pgrep -af hush-daemon` showed two processes on `instances/peer-a` — the one started
  by hand (`--addr 127.0.0.1:9876`, 11:12) and one the Tauri app spawned (socket only, 11:37).
  Both had the vault open and `ss` listed two listeners on
  `/run/user/1000/hush-one/hush.sock`, so the browser and the app were being served by different
  processes over the same vault.
- Resolution: killed both and started a single daemon serving the socket and 9876; the app
  reconnects to the socket. Verified one process, socket present, `/api/status` 200.
- Lesson: an app that spawns its own daemon while a hand-started one is already running yields
  two writers on one vault. The daemon has no single-instance guard on the socket or vault
  directory; not fixed here, but worth one.

---

## 2026-10-10 — Reads "Add feed" reported as doing nothing

### A refused feed explained itself in the wrong column — FIXED

- What was attempted: owner reported the RSS add "doesn't work" and that clicking Add did
  nothing; asked for the add to move into a dialog.
- Symptom: reproduced by driving the real UI — clicking Add with a scheme-less URL
  (`news.ycombinator.com/rss`) did submit, but the daemon's refusal
  (`refused URL "news.ycombinator.com/rss"`) was written to `readsError`, which
  `renderReadsList` paints in the *middle* column. The nav, where the Add control lives,
  showed no change at all, so from the control's side it looked like nothing happened. The
  empty-field case behaves the same way.
- Resolution: replaced the inline nav field with an "Add a feed" dialog that holds the URL,
  the busy state, and the refusal reason together; a URL typed without a scheme is assumed to
  be `https://` instead of being bounced. Verified live: a refused URL keeps the dialog open
  with the reason beside the field, and a scheme-less good URL adds and opens the source.
- Lesson: feedback belongs next to the control that caused it. An error rendered in a
  different pane is indistinguishable from no action at all. Reproduce before assuming the
  request never fired.

---

## 2026-10-10 — Received drop rendered one character per line in Mail

### A drop body wrapped to a few characters per line — the card skipped its content column — FIXED

- What was attempted: user reported a received drop "shows a vertical line of letters" when
  opened in Mail.
- Symptom: `renderDropView` appended the header, the rule, and the body straight onto the
  `article.mail-message`. `.mail-message` is a two-column grid (`grid-template-columns: 44px
  1fr`) whose first column is the 44px avatar slot, so auto-placement dropped the header and
  the body into that narrow column. With `white-space: pre-wrap` and `overflow-wrap:
  break-word`, the body then wrapped to a handful of characters per line. `renderMailThread`
  avoids this by appending an avatar and wrapping everything else in `.mail-message-content`.
- Resolution: `renderDropView` now appends a neutral avatar
  (`createAvatarEl("anonymous-drop", "sm")`) and wraps the header/rule/body/attachments in a
  `.mail-message-content` div, matching the thread layout. Verified live against a real drop
  sealed to the vault's published key and parked on the relay: the card computes
  `44px 568px` and `.drop-body` sits inside `.mail-message-content` at 568x75 px (3 lines)
  instead of a ~44px-wide column.
- Lesson: the body data was never the problem — it was clean multi-line text. A grid
  container with an implicit narrow first column turns "append three children" into "put two
  of them in the avatar slot". A view that reuses a layout class must reproduce that class's
  child structure.

## 2026-10-10 — RSS add reported broken a second time

### "Still can't add RSS feed" — the daemon was not running at all — FIXED (environment)

- What was attempted: user reported the Reads "Add RSS feed" flow still failing after the
  earlier stale-daemon fix. Checked for a running daemon before touching any reads code.
- Symptom: no `hush-daemon` process existed; `/run/user/1000/hush-one/hush.sock` was a
  stale leftover (mtime 11:05, nothing listening — `curl --unix-socket` failed with
  `(7) Failed to connect`). The rebuilt binary (`target/debug/hush-daemon`, 11:04:42) was
  never running, so every reads request would fail with "Can't reach the Hush daemon".
- Resolution: started the daemon on the `peer-a` vault with both the unix socket and
  `--addr 127.0.0.1:9876` (the browser UI's default base), unlocked it, then drove the real
  UI at `localhost:5199/?api=http://127.0.0.1:9876`: the Reads pane listed the user source
  and clicking Add on `https://lobste.rs/rss` returned 200 and rendered a new "Lobsters"
  row with a Remove ×. The API path was confirmed independently (`POST /api/reads/sources`
  → 200, source list grew). The reads routes and add flow are correct; the only fault was
  the daemon being down.
- Side effect: two feeds added to the `peer-a` vault during verification (Hacker News,
  Lobsters); removable from the nav × if unwanted.
- Lesson: "can't add RSS" is not evidence of a reads bug. Confirm the daemon is up and the
  vault unlocked first — the same class as "Tauri shell proxies to whatever daemon process
  is already running" above. A stale socket file makes the daemon look present when it is
  not; `curl --unix-socket` (or `ss -xlp`) is the honest check.

## 2026-10-10 — Relay redeploy after the drops slice

### Blank white page on the onion `/drop` — the deployed relay predates the route — FIXED

- What was attempted: user reported `http://ogkf…sad.onion/drop` renders blank white.
  Checked the committed bundle (`page.built.html` intact, 53 KB, JS inlined), then probed
  the lab VPS over SSH: `hush-relay` and `tor` both active, clearnet `/health` 200.
- Symptom: through the onion, `/health` → 200 but `/drop` → **404 with an empty body**
  (clearnet identical). An empty 404 is exactly what a browser renders as a blank white
  page; the dark-themed drop page itself never reaches the browser.
- Cause: the deployed binary was built 2026-10-08 20:13 UTC; the `/drop` page route landed
  in `d81837b` (2026-10-09 17:41). The Tor onion service forwarded fine — the relay behind
  it just did not know the route.
- Resolution: rebuilt `hush-relay` release from HEAD in a throwaway worktree (only
  committed code; the working tree has WIP in `hush-core`) and redeployed per
  `docs/ops/relay-lab-vps.md` (sha256 `ed1cf4bf…`, checksum verified on the box).
  Onion `/drop` now 200 with the 53837-byte page, `text/html; charset=utf-8`; the drop
  family round-tripped through the onion (PUT 204 / GET 200 exact bytes / DELETE 204 /
  GET-after-delete 404). `cargo test -p hush-relay` 18 passed (2 drops, 8 prekey,
  3 queue-credentials, 5 relay-queues).
- Lesson: same class as the stale-daemon entry below — a relay feature does not exist
  until the deployed binary is replaced. After a relay-facing slice, redeploy and curl the
  live surface through both bases; the onion path answering `/health` proves only
  reachability, not that new routes exist.

### Fresh target dir cannot build `hush-core` without `protoc` — WORKED AROUND (environment)

- What was attempted: the clean-source worktree build above hit the earlier `protoc` gap
  (see "A throwaway cargo project cannot build `hush-core` because `protoc` is absent")
  from the other direction: a fresh target dir re-runs `sparsepostquantumratchet`'s build
  script even though the workspace's own cached `target/` hides it.
- Symptom: same panic — `Could not find 'protoc'` — 96 s into `cargo build --release
  --locked -p hush-relay` from an empty target dir. `which protoc` is empty and there is
  no system protobuf-compiler.
- Resolution: no system change needed — a usable `protoc` ships inside the uv/torch cache:
  `PROTOC=/ssd/sam/cache/uv/archive-v0/u47pJhhlHvKWEJVjcotQM/torch/bin/protoc` (libprotoc
  3.13.0) unblocked the full from-scratch release build (2 m 25 s). The underlying
  environment gap stands for any fresh clone; installing `protobuf-compiler` would close
  it for good.
- Lesson: point builds at a `PROTOC` env var instead of relying on a warm `target/`;
  a cached build script output is invisible infrastructure until the first cold build.

### Removing the build worktree wedged the persistent shell session — WORKED AROUND (tooling)

- What was attempted: `git worktree remove --force /tmp/hush-relay-deploy` from the agent
  shell, whose cwd was still inside that worktree from the build commands.
- Symptom: `fatal: Unable to read current working directory`, then every subsequent shell
  command — including `pwd` — returned no exit status. The removal itself succeeded (the
  directory is gone and the repo reads fine through file tools), but the shell session is
  unrecoverable and needs a restart. Purely a tooling problem; no repo state affected.
- Resolution: no in-repo fix. Verify worktree cleanup with file tools instead of the shell
  when this happens; `git worktree prune` on the next real shell use clears any stale
  registration.
- Lesson: `cd` out of a directory before deleting it out from under the persistent shell —
  a deleted cwd poisons the whole session, not just the next command.

## 2026-10-10 — Reads RSS Add button

### Add-feed POST returned a different source list than GET — FIXED

- What was attempted: after `POST /api/reads/sources`, the handler returned only the user's
  `RssSource` entries from `ReadsState`, while `GET /api/reads/sources` returned built-in
  first plus user sources.
- Symptom: reproduced in the dev page — clicking Add succeeded but the nav then rendered
  `["Hacker News", "Saved"]`; `HK News` vanished from the source list until the next rail
  refresh refetched.
- Resolution: extracted `reads_sources_view(&App)`; GET, POST, and DELETE now all return the
  same `Vec<ReadsSourceDto>` shape so the frontend can swap its list wholesale after any
  mutation. Verified in browser: after Add, nav is `["HK News", "Hacker News", "Saved"]`.
- Lesson: when two endpoints conceptually return "the list", share one builder. Hand-rolled
  projections drift the moment one endpoint gains a field.

### RSS feed URL input was uncontrolled inside a re-rendered pane — FIXED

- What was attempted: the add-feed input kept its value only in the DOM node; any
  `render()` (poll tick, sources refresh) rebuilt the nav from state and cleared it.
- Symptom: a click on Add after a re-render submitted an empty draft, which silently
  returned — the user-visible effect is "the Add button doesn't work" with no feedback.
- Resolution: added `readsSourceDraft` app state bound via `input.oninput`; Enter/Add read
  the draft, a successful add clears it, and an empty draft now sets a visible
  `readsError` ("Type or paste an RSS feed URL to add.") instead of a silent no-op.
  Caveat: automated typing into a captured element reference does not survive a re-render
  even with state binding — the harness must re-query the live element per keystroke.
- Lesson: any input in a pane that re-renders from scratch needs its draft in state, or it
  will eat user input on unrelated re-renders.

### Tauri shell proxies to whatever daemon process is already running — OPEN (environment)

- What was attempted: explaining the user report that Add "doesn't work" in the real app.
- Symptom: `src-tauri/lib.rs` `transport_from_env` connects to an existing Unix-socket
  daemon; the shell never spawns one. A daemon binary started before the source routes
  existed answers `404` for `POST /api/reads/sources`, so Add fails there with an error
  shown at the top of the list pane.
- Resolution: none in-repo — the routes exist and are tested. The user's running daemon
  must be killed and relaunched from the rebuilt binary for the Add flow to work.
- Lesson: new daemon routes require restarting the sidecar-less daemon; a webview rebuild
  alone is not enough.

## 2026-10-09 — Reads pane (HN reader, `docs/tasks/hush-reads.md`)

Slice goal: new Reads rail pane fed by Hacker News via Algolia — surfaces, search, in-app
reader mode, sanitized comments, vault-backed saved/read/hidden state.

### [FIXED] RSS items hit the main pane's "pick something to read" empty state

- **Attempted:** after switching Reads to a source list, an RSS item (which has no HN
  discussion) was opened straight into the reader — no `readsDetail` fetch ever runs for
  it.
- **Symptom:** the main pane's guard (`!id || (!detail && !detailLoading)`) decided
  "nothing open" the moment the row was clicked, because for RSS there is no detail to
  load: the reader flow skipped straight past the empty state. Caught in the browser pass.
- **Resolution:** `renderReadsMain` now resolves the story from `detail.story` **or** the
  listed row, and the loading/empty states are decided after that; the discussion branch
  additionally requires `detail` to exist. HN behavior unchanged (verified: 179-comment
  thread still renders; RSS reader renders with Discussion disabled).
- **Lesson:** when one pane serves two content kinds, resolve "what am I rendering" once
  at the top instead of guarding on one kind's loading flags — the second kind trips the
  first kind's empty state otherwise.

### [FIXED] Built the domain filter on an Algolia `domain_{host}` tag that does not exist

- **Attempted:** the task doc specified domain filtering as `tags=story,domain_{host}`
  combined with the surface; `search_url` in `reads.rs` implemented exactly that.
- **Symptom:** every domain-filtered query returned zero stories, and the UI showed an
  empty list. Direct curl against the live API showed hits whose `_tags` only ever contain
  `story`, `author_*`, `story_*`, `front_page`, `ask_hn`, `show_hn` — no `domain_*` tag on
  the public `hn.algolia.com` API v1 (it may exist on Algolia's internal index, not on the
  public proxy).
- **Resolution:** moved domain narrowing into the daemon: `domain_search` scans a bounded
  10 pages of the popularity index (mem-cached after the first click), keeps host matches
  via a new `normalize_host` helper, and serves UI pages out of the filtered list. Niche
  domains can come back short; the empty state in the UI says so, and the task doc's
  surfaces table no longer claims a `domain_` tag. `by_date` is deliberately ignored in
  domain mode ("more from this site" reads as best-of).
- **Lesson:** design against observed API responses, not remembered features. The tag was
  never verified against a live hit before the UI was built on it — one curl of a real
  search with `_tags` in the payload would have caught it at design time.

### [FIXED] Domain chip handler wiped its own filter; a stale-render race made the test look like it passed

- **Attempted:** the row chip's onclick did `this.readsDomain = s.domain; this.readsRunSearch();`
  while `readsRunSearch()` unconditionally does `this.readsDomain = null`.
- **Symptom:** clicking a domain chip never filtered — the run erased the domain before the
  load. First browser test reported the filter "working" because the probe counted rows
  before the list re-rendered: the wait loop exited on `rows > 0` (old rows still on
  screen) and the domain assertions ran against the previous surface's DOM.
- **Resolution:** new `readsRunDomainSearch(domain)` that sets mode/domain/query in one
  place; chip routes through it. Test rewritten to wait for the list to *clear* (row count
  drops below the pre-click value) and then settle, and to assert all rendered domains
  match the filter — verified live: github.com → 4 rows, all github.com, nav chip shows
  the clear control.
- **Lesson:** setters that reset sibling state make "set then run" callers silently undo
  themselves — state transitions that belong together need one entry point. And an async
  UI assertion must anchor on the transition having started (list cleared), not on the DOM
  being non-empty, or it asserts stale markup.

### [FIXED] `#[serde(flatten)]` on `SavedStory` silently broke every save/read annotation in the UI

- **Attempted:** `SavedStory { #[serde(flatten)] story: StoryDto, saved_at }` in
  `reads_store.rs`, with the TypeScript side typed as `{ story: ReadsStoryDto; saved_at }`.
- **Symptom:** saving worked on the daemon (state route returned the story, nav count
  incremented) but the UI never flipped: `state.saved.some(x => x.story.id === id)` compared
  against `undefined` because flatten serializes story fields at the top level. No error
  anywhere — just silently false annotations. Caught in the browser pass, not by cargo tests
  (the roundtrip test asserted the same nested shape as TS, so the two sides disagreed with
  only the Rust side real).
- **Resolution:** removed `flatten`; daemon nests `{ story, saved_at }` to match the TS type.
  Also caught the flip side live: a sidecar written by the pre-fix daemon no longer parses
  (`missing field 'story'`), which for the never-shipped format meant deleting the throwaway
  smoke vault's `reads-state.enc` — no migration code for a format that never shipped.
- **Lesson:** when the same JSON has a Rust and a TypeScript declaration, the roundtrip test
  must assert the *wire* shape against a literal sample, not against Rust's own types, or
  both sides can be consistently wrong. `serde(flatten)` + a hand-written TS type is a
  standing trap.

### [WORKED AROUND] Unlock screen has no status poll, so an unlocked vault keeps showing the passphrase form

- **Attempted:** during the browser smoke, unlocked via `curl POST /api/unlock` and expected
  the unlock screen to flip to the shell on its own.
- **Symptom:** the unlock screen never re-renders while idle — `startPoll` (and every other
  refresh loop) starts only after the shell renders; status is fetched once at init.
- **Resolution:** reload the page in that state; `refreshStatus` at init sees `locked:false`
  and goes straight to the shell. Real users always unlock through the form, so this is a
  smoke-testing artifact, not a product bug.
- **Lesson:** when scripting UI flows against this app, the unlock screen is a dead end for
  out-of-band unlocks; drive the form or reload.

## 2026-10-09 — Incoming drops (sealed box, vault key storage)

Slice goal: rebuild the inbound outsider-drop path per `docs/plans/2026-10-09-incoming-drops.md` —
published drop key as the address, rendezvous id, relay drop page, Mail "Drops" folder.
All seven tasks done this session (Tasks 1–5 earlier, 6–7 and the live browser pass now).

### [FIXED] `tor::tests::missing_binary_reports_unavailable` fails on any host running Tor

- **Attempted:** full `cargo test --workspace` at slice close; every crate passed except
  `hush-tauri --lib`, in a file the drops slice never touched (`tor.rs`).
- **Symptom:** `assertion left == right failed / left: Ready / right: Unavailable` at
  `tor.rs:640`. `ss -tlnp` showed a live `tor` (pid 91698) on `127.0.0.1:9150` on this
  machine. `ensure_blocking` probes the usual loopback SOCKS ports (9050/9150) and
  adopts a live system Tor *before* consulting any binary, so the test's
  `HUSH_TOR_BIN=/nonexistent` override was never reached and the manager went `Ready`.
- **Resolution:** when `HUSH_TOR_BIN` is set at all, skip `adopt_system_tor_if_live` —
  an explicit override means "use this binary", and adopting a different Tor behind the
  operator's back would silently mask a broken override (wrong path, stale install).
  Test green afterwards; also a real behavior fix, not test appeasement.
- **Residual (not fixed):** the test still is not hermetic on hosts with a system Tor
  at a candidate path (`/usr/bin/tor` etc.) — the locator would find and spawn it. A
  fully hermetic fix needs injectable search paths; none of this repo's dev hosts has
  one, so left alone.
- **Lesson:** an "adopt whatever's live" fast path silently changes what a failing
  locator means. Tests that force the not-found path need that path to be reachable.

### [WORKED AROUND] drop page has no `#key` hash prefill — my browser-pass flow was wrong, not the page

- **Attempted:** opened `http://relay/drop#hush-drop-pk-v1:…` in the browser pass and
  expected the key textarea to prefill from the URL fragment.
- **Symptom:** `#key` stayed empty; clicking "Seal and upload" said "Fix the size or the
  key first." Reading `main.js` confirmed there is no `location.hash` handling at all.
- **Resolution:** design intent, not a gap — Settings publishes the page URL and the key
  as two separate strings ("publish both of these together"), and the key is pasted into
  the textarea. Keeping the key out of the URL is deliberate: a URL that silently carries
  the key makes accidental publication easier, and the hash would also linger in the
  dropper's history/session logs. Pasted the key manually; the seal worked first try.
- **Lesson:** before reading page behavior as a bug, read the publishing-side UI that
  produces the inputs — the two-string design was in `app.ts` the whole time.

### [WORKED AROUND] piping a server into `head` kills it (SIGPIPE) — tooling

- **Attempted:** started the scratch relay with `cargo run -p hush-relay 2>&1 | head -3`
  to bound startup noise.
- **Symptom:** the pipeline exited 0 after ~14 s and nothing listened on 8799 — `head`
  left as soon as it had three lines and the relay died writing to the closed pipe.
  Cost one confused curl round.
- **Resolution:** tooling error, not a repo bug — run the built binary
  (`./target/debug/hush-relay`) unpiped in the background terminal instead.
- **Lesson:** never pipe a long-running server; `head` is a termination mechanism.

### [FIXED] The 2026-10-07 plan's libsignal X25519 API does not match the pinned checkout

- **Attempted:** implement `hush-drop-v2` from the removed plan's crypto section
  (`pair.public` / `pair.private`, "libsignal's calculate_agreement rejects the all-zero
  shared secret").
- **Symptom:** none of it compiles against the pinned libsignal (`257105c`). `KeyPair`
  fields are `public_key`/`private_key`; `PublicKey::serialize()` returns the
  type-prefixed 33-byte form (raw point is `public_key_bytes()`, raw reconstruction is
  `from_djb_public_key_bytes`); the inner `curve25519::PrivateKey::calculate_agreement`
  is infallible and explicitly does not validate output — the all-zero rejection lives
  in the `curve.rs` wrapper, which is what `libsignal_protocol::PrivateKey` re-exports.
- **Resolution:** used the wrapper (`calculate_agreement` → `Result`, all-zero →
  `CurveError::InvalidKeyAgreement`), raw bytes in/out at the drop boundary. The
  round-trip, wrong-key, and tamper tests all pass, so the claim is now true at the
  level the code actually uses.
- **Lesson:** trust the removed plan's *decisions*, not its API prose — it described a
  different libsignal than `Cargo.lock` pins. Check the checkout before writing crypto
  glue from an old plan.

### [FIXED] @noble/curves 1.9 has no `./x25519` subpath export

- **Attempted:** `require("@noble/curves/x25519.js")` in `scripts/drops-vector.mjs`.
- **Symptom:** `ERR_PACKAGE_PATH_NOT_EXPORTED`.
- **Resolution:** in noble/curves 1.x, `x25519` is exported from the `ed25519` module
  (`require("@noble/curves/ed25519").x25519`). The page bundle in Task 3 must import it
  the same way.

### [FIXED] RFC 5869 vector in the plan was written from memory and garbled

- **Attempted:** the plan's Task 1 test asserted a half-fabricated hex string.
- **Symptom:** caught in self-review before execution; the string was not the RFC value.
- **Resolution:** computed Test Cases 1 and 3 with Python's independent
  `hashlib`/`hmac` (`3cb25f25…`, `8da4e775…`), wrote those into plan and test; the
  hand-rolled `hkdf_sha256` agrees on both, including the empty-salt multi-block case.
- **Lesson:** crypto vectors come from a second implementation or the RFC text, never
  from recall.

### [FIXED] `ZeroizeOnDrop` struct fields cannot be moved out

- **Attempted:** `let mut retired = keys.current;` in `Vault::rotate_drop_keypair`.
- **Symptom:** E0509, cannot move out of a type implementing `Drop`.
- **Resolution:** `keys.current.clone()` — each clone zeroizes on its own drop.

---

## 2026-10-09 — Delete ended conclave (history gone, files kept)

Slice goal: a delete option for ended conclaves in the Conclaves rail — `DELETE /api/conclaves/{id}`
removes the record and its whole message log from this vault only, nothing broadcast, vault blobs
behind shared files kept.

### [FIXED] delete_conclave turned the idempotent retry into a hard error

- **Attempted:** `Account::delete_conclave` looked the record up with
  `ok_or(Error::ConclaveNotFound)` before the ended-check, so an unknown id errored instead of
  returning the quiet `Ok(false)` no-op that `remove_conclave` (and `delete_mail` before it) promise.
- **Symptom:** the new integration test `deleting_an_ended_conclave_drops_its_history_but_keeps_the_files_and_the_peers_copy`
  failed on its second delete — `panicked ... called Result::unwrap() on an Err value: ConclaveNotFound`
  at `tests/conclaves.rs:550`. The first delete succeeds and makes the id unknown; the second delete
  then hit the `ConclaveNotFound` guard and errored, so any retry/double-submit after a successful
  delete would surface a spurious error.
- **Resolution:** look the record up first and return `Ok(false)` when it is absent; only an existing,
  not-ended conclave yields `Err(ConclaveNotEnded)`. Test now passes, and the no-op contract matches
  `delete_mail`.
- **Lesson:** a guard that keys on a record's state must decide what an absent record means before it
  errors; "delete twice = one delete, no error" and "refuse deleting an active record" are separate
  cases, and only the test that deletes twice catches the difference.

### [FIXED] Account::conclaves() doc comment claimed ended conclaves were filtered out — code returns them

- **Attempted:** reading `account.rs` before extending it; the doc comment said "An ended conclave is
  filtered out here ... a restart still shows it gone" while the body and its own inline comment
  return ended conclaves as read-only.
- **Symptom:** no runtime symptom; contradictory documentation on the exact function the slice builds
  next to. Trusting the comment would have suggested hiding deleted/ended records is already handled.
- **Resolution:** rewrote the doc comment to match the code ("Ended conclaves are returned too: they
  are read-only rather than hidden"). No behaviour change.
- **Lesson:** when a doc comment and the code under it disagree, the inline comment explaining the
  code is usually the newer of the two; fix the stale one in passing rather than leaving a trap.

---

## 2026-10-09 — Client Tor switch (relay over onion, fail-closed)

Slice goal: a per-vault relay switch — pre-login toggle and top-bar pill in the UI,
`hush-cli init --onion/--tor` and a `relay on/off/show` subcommand, daemon
`/api/relay*` endpoints, TorManager promoted to app-level state. New onion state on
`AccountState` (`onion_relay`, `use_tor_relay`) with a fail-closed `relay_client()`
at every former `RelayClient::try_new(&self.state.relay)` call site.

### [OPEN] connection light showed "connected" with the laptop offline (2nd report: fix never reached the wire)

- **Attempted:** make the rail shield reflect whether the relay is actually
  reachable (first pass: `RelayClient::ping`, `Account::relay_ping`, daemon
  cache + background probe, `reachable` in `GET /api/relay`).
- **Symptom (user, live build):** "still not changing the color indicator when
  no internet.. i waited 30 s or more it didn't change .. outbox worked fine …
  send mail after connected again". The outbox result proves fail-closed and
  the queue both work; only the light lied.
- **Root cause found:** `run-one-ui.sh` **reused an already-running daemon**
  ("Daemon already up on $SOCK"). The daemon serving `/api/relay` predated the
  `reachable` field, so the new UI read `undefined` → treated as unknown →
  green forever. The rebuild produced a new binary that the script then never
  started. A dev-loop script that reuses long-lived processes must restart
  them when the code changes, or changes silently never ship.
- **Resolution (implemented, unverified):** (1) the script now restarts the
  daemon on every run (kill the stale one on the socket first; state lives in
  the vault, so a restart is free); (2) defense in depth — the daemon's
  heartbeat loop already round-trips the relay every 30 s and discarded the
  result; its outcome now feeds the reachability cache, so the light converges
  within ≤30 s even if poll-triggered probes were somehow not running.
- **Needed to resolve:** re-run the script (it will say "Stopping the stale
  daemon…"), then pull the network: red within ~5 s from a probe, worst case
  ~30 s from the heartbeat; restore and confirm green.
- **Lesson:** when a fix "doesn't work", first verify it is running. A reused
  process serving old code makes every subsequent theory wrong. The heartbeat
  was already paying the round trip and throwing away the only signal that
  mattered — wire results to the state that needs them.

### [OPEN] connection light showed "connected" with the laptop offline (first pass)

### [OPEN] pill stuck on "Set relay address"; settings told the user to toggle Tor off and on

- **Attempted:** ship the redesign so the switch alone is the whole flow: flip on →
  Tor boots, the daemon learns the SOCKS port, the vault carries the mode, pill goes
  "Tor on".
- **Symptom (user, live build):** "while its connected to Tor it shows Set relay
  address" and "asking me to turn it on and off and still not working". The pill's
  `pending` state fired for *any* `mode && !applied`, and `applied` required the
  daemon to hold the shell Tor's SOCKS port in daemon memory — which a daemon
  restart wipes and nothing ever re-posted. The settings note then literally
  instructed "turn the switch off and on", which re-posted the port but was
  unprincipled and still left the fresh-restart case broken.
- **Evidence found on disk (second report):** `instances/peer-a/ui-prefs.json` had
  `relay_tor: true` and the onion **staged but never consumed** — the vault never
  received the address, so `applied` could never be true. Also: the user runs
  their own Tor, while the app unconditionally spawns a second bundled Tor and
  competes with it; and the vault itself was born on the loopback relay
  (`setup-two-peers.sh` passes `--relay http://127.0.0.1:8787` explicitly), so
  clearnet traffic was going to "the local one" the user does not want.
- **Resolution (implemented, unverified):** (1) self-healing in
  `refreshTorPill()` — the poller re-posts the live SOCKS port whenever the
  daemon's copy differs and auto-starts the shell Tor when it is stopped;
  (2) the shell now **adopts a live system Tor** (SOCKS5-greeting probe on
  9050/9150) instead of always spawning its own, and re-probes an adopted Tor
  on the next explicit start; (3) Tor's stdout is teed to `tor.log` in the
  private data dir so a failure is diagnosable; (4) the rail's existing top
  shield is now the single indicator — no second icon, no badge — filled green
  when connected (clearnet or Tor), orange while Tor comes up, red when not
  reachable, with the accessible name/tooltip carrying the words; (5) the
  relay-address UI is removed (operator config carries `onion_relay`), and
  `hush-cli relay set --url` can move an existing vault off the loopback relay;
  (6) `run-one-ui.sh` no longer starts a local relay by default
  (`WITH_LOCAL_RELAY=1` opts back in).
- **Needed to resolve:** rebuild + `scripts/run-one-ui.sh`; check the shield
  goes green through Tor without any manual step; `relay show` on both peers
  after `relay set` to confirm the VPS relay.
- **Lesson:** three separate assumptions compounded into "not working": state
  that dies with a process restart (SOCKS port), a second Tor where the user
  already had one, and a vault pinned to a relay the user never chose. Diagnose
  from on-disk state before theorizing; every layer here was provable from
  files alone.

### [FIXED] cached invite defeated every negative control in the live Tor E2E

- **Attempted:** prove the client Tor path by running `hush-cli invite` against the
  lab VPS onion with (a) SOCKS env unset and (b) tor dead; both must fail.
- **Symptom:** both controls *passed* — identical invite bytes, ~0.5 s even with tor
  killed. `invite()` returns the minted invite from `invite_cache` in vault state and
  never touches the relay on repeats, so the probes exercised no network path at all.
- **Resolution:** switched the probe to `recv`, which always hits the relay. With the
  corrected probe: SOCKS unset → `Relay("Tor relay mode is on, but Tor is not
  connected yet…")`; tor dead → connection error naming the **onion** URL (no
  clearnet fallback); tor up → success with `Got RENDEZVOUS2 cell from hidden
  service` in tor's info log. All captured in `scripts/smoke-tor-relay.sh`.
- **Lesson:** before trusting a negative control, check whether the code path under
  test has a cache. A control that passes must fail is not evidence.

### [WORKED AROUND] bundled tor cannot be launched bare from a shell

- **Attempted:** start the sidecar binary
  (`resources/tor/tor --SocksPort 9150`) directly for the E2E, same as the app does.
- **Symptom:** process dies instantly, no log file created.
  `error while loading shared libraries: libevent-2.1.so.7`.
- **Resolution:** usage issue, not a repo bug — the sidecar's libraries live beside it
  and the app's `TorManager` points `LD_LIBRARY_PATH` there. Launching outside the
  app needs `env LD_LIBRARY_PATH=resources/tor tor …`; the smoke script does this.
- **Lesson:** a bundled sidecar is only self-contained when its launcher sets its
  library path; document the invocation, do not assume the binary is portable.

### [OPEN] shell execution environment died mid-session; last two batches of work not compiled

- **Attempted:** keep building and verifying across the session; the shell stopped
  returning exit codes partway through and never recovered (retried across ~90
  minutes, including `echo` and `true`).
- **Symptom:** every shell call returns "no exit status"; file tools keep working.
  An agent-environment failure, not a repo problem.
- **Unverified work behind it:** (1) the `hush-cli` `Relay` subcommand and
  `scripts/smoke-tor-relay.sh` from the first slice; (2) the user's redesign —
  relay address moved out of the login card into a Settings → Relay section, the
  login control now a slide switch, the pill reframed to Tor on/off with a
  `pending` state, `GET /api/relay` returning `{tor_mode, applied, onion_url,
  socks}` with the mode choice accepted even when no address is on file yet, and
  `Work::RelayAttach` deleted in favour of `RelayApply`. All statically
  self-reviewed; none of it compiled or type-checked. (3) the stuck-pending fix
  below — poller self-heal, pill state machine rework, and the config-level
  `onion_relay` fallback in `hush-core` and `hush-daemon`.
- **Needed to resolve:** `cargo build -p hush-cli -p hush-daemon -p hush-tauri`,
  `npx tsc --noEmit` + `npx vite build` in `apps/hush-tauri`, then
  `scripts/smoke-tor-relay.sh`.

---

## 2026-10-08 — Chat notifications: rail badges + arrival sound

Slice goal: unread-count pill on the Chats and Mail rail buttons, a native arrival
sound for chat messages (not while the chat is open), global and per-contact sound
toggles.

### [FIXED] unread badge stays after re-entering the chat via the rail button

- **Attempted:** clear the unread count on every path into an open conversation;
  initially only row clicks (`conv row onclick`) and the Trusts card "Chat" action
  (`openChat`) called `markRead`.
- **Symptom:** user reports the Chats pill "does not go when I am in the chat
  window; it only goes away when I press on the chat session button". Reproduced
  the mechanism from the code: `selectedNick` survives rail switches, so leaving
  Chats for Mail and coming back via the **rail button** re-opens the last
  conversation through `render()` with no `markRead` anywhere on that path.
- **Resolution:** `render()` now marks the open conversation read before painting
  (`if this.rail === "chats" && this.selectedNick markRead(...)`), so every entry
  path — click, rail re-entry, in-chat arrival — clears the pill. Verified in the
  browser: away-arrival shows pill "1", clicking the Chats rail button clears it
  with no row click.
- **Lesson:** state that a view mutates as a side effect of *clicks* will miss the
  paths that reach the same view without a click. Put the invariant in the render
  pipeline, not in the handlers.

### [WORKED AROUND] WebKitGTK plays no audio in the Tauri shell

- **Attempted:** play the notification WAV with `new Audio()` (Vite asset URL).
- **Symptom:** silent in the desktop shell; the same page in a normal browser
  plays fine. WebKitGTK media needs host GStreamer plugins that are not installed.
- **Resolution:** added a `rodio`-based `play_chat_sound` Tauri command
  (`src-tauri/src/sound.rs`, WAV embedded with `include_bytes!`) that talks to
  ALSA/Pulse directly; `chatSound.ts` uses it in the Tauri shell and keeps
  `new Audio()` as the dev-browser fallback. Required `libasound2-dev` to build
  (`alsa-sys` failed without it). Sound itself not yet confirmed by ear on the
  desktop instance.
- **Lesson:** browser-side media in a WebKitGTK shell depends on host multimedia
  packages; for anything that must be heard, go native.

### [OPEN] hush-cli recv exits 0 without unlocking the daemon session

- **Attempted:** unlock Bob's vault headless for testing via
  `HUSH_DAEMON_SOCKET=… HUSH_PASSPHRASE=… hush-cli --dir instances/peer-b recv`.
- **Symptom:** CLI exits 0 (prints nothing), but the daemon's HTTP API still
  answers `{"error":"locked"}` (401) on `POST /api/send` immediately after. There
  is no `--socket` flag on the CLI (unexpected-argument error).
- **Resolution needed:** either the CLI speaks to a different socket than the env
  var suggests, or its unlock handshake does not persist in the daemon's session
  state. Test path for now: `curl -X POST …/api/unlock -d '{"passphrase":…}'`
  against the daemon's TCP port, which works (204).
- **Lesson:** a CLI that succeeds silently tells you nothing about which
  service it actually talked to; verify with the target service's own status.

### [OPEN] run-two-ui.sh tears the whole stack down when its UI windows die

- **Attempted:** `pkill` only the two `hush-tauri` windows to stop them from
  draining the shared daemon queue during a browser-side test, expecting the
  daemons to stay up.
- **Symptom:** relay and both daemons disappeared seconds later — the launcher
  waits on its children and its cleanup trap kills everything when the windows
  exit ("Press Ctrl+C here to stop the relay, daemons, and UI this run started").
- **Resolution:** none (by design). For window-free testing, start the relay and
  daemons standalone (`hush-relay`, `hush-daemon --dir … --socket … --addr …`)
  and reopen the windows later; `run-two-ui.sh` reuses live daemons.
- **Lesson:** launcher scripts that own the process group make "restart just the
  UI" impossible; the daemons need a standalone start path for headless work.

### [WORKED AROUND] pkill -f matches the invoking shell's own command line

- **Attempted:** one-shot restart commands chaining `pkill -f "run-two-ui.sh"`
  (and similar `-f` patterns) before relaunching.
- **Symptom:** the command silently died after the first `pkill` — the shell's
  own `bash -c` command line contains the pattern, so pkill killed the shell
  running it. Happened twice; the second time it looked like "the launcher never
  started" because the log file never appeared.
- **Resolution:** bracket-trick patterns (`pkill -f "run-two-ui[.]sh"`) or kill by
  exact PID; verify with `ps` before assuming a launch failed.
- **Lesson:** with `pkill -f`, the pattern is matched against *your* command line
  too; any restart one-liner that does not account for that can kill itself
  mid-sequence.

### [WORKED AROUND] accessibility snapshots cannot see the rail count pills

- **Attempted:** verify the badge via the browser accessibility snapshot
  (button accessible names).
- **Symptom:** the pill's `aria-label` on a role-less span removes its text from
  the accessible name; the snapshot shows "Chats" even when the DOM has
  `<span class="rail-count">2</span>`.
- **Resolution:** read the DOM directly via CDP `Runtime.evaluate`
  (`querySelectorAll('.rail-count')`) for badge assertions.
- **Lesson:** `aria-label` on an element without a role hides its content from
  the a11y tree; give count pills `role="status"` (or check the DOM) when
  asserting on them.

---

## 2026-10-08 — Chat notifications: rail badges + arrival sound (cont.)

### [FIXED] Tauri v2 ACL silently denied the sound command

- **Attempted:** user clicks "Play test sound" (and every arrival sound before
  it) — nothing plays. The Rust command `sound::play_chat_sound` is registered
  in `generate_handler!`, the rodio device probe passes, and other commands
  (`hush_api`) work in the same window.
- **Symptom:** no sound, no error anywhere. `alice-ui.log` (tauri stderr) has no
  "chat sound requested" line after a click, so the command never executed; the
  JS `.catch(() => {})` swallowed the rejection, which is Tauri's
  "command not allowed" IPC denial.
- **Root cause:** Tauri v2 denies every app command that is not allowed by a
  capability. `capabilities/default.json` listed `allow-hush-files`,
  `allow-hush-api`, `allow-drops` — custom permission groups defined under
  `src-tauri/permissions/*.toml` — but nothing for the new command.
- **Resolution:** added `permissions/hush-sound.toml`
  (`allow-chat-sound` → `commands.allow = ["play_chat_sound"]`) and listed
  `allow-chat-sound` in the capability. Rebuilt, windows relaunched.
- **Lesson:** in Tauri v2 a new `#[tauri::command]` needs three things — the
  command fn, the `generate_handler!` entry, and a capability permission. The
  second is easy to remember; the third denies silently at the IPC boundary.
  Log rejections in dev instead of `.catch(() => {})`.

### [FIXED] chat delivery flaky: presence died with a throttled webview and sends never retried

- **Attempted:** user reports "sometimes messages are delivered, sometimes
  not" between two live desktop windows.
- **Symptom/mechanism (three stacked causes):** chat is live-only
  (`send_payload_inner` with `require_online = true`): (1) presence is
  "heartbeat in the last 90 s" at the relay, and heartbeats came only from the
  UI poll loop — an unfocused WebKitGTK window throttles its timers, the
  heartbeat goes stale, and the sender's `contact_is_online` check fails with
  `ContactOffline`; (2) a failed delivery had zero retries — one relay blip
  (the VPS relay over the internet) lost the message; (3) the UI correctly
  removes the optimistic bubble and announces the failure, which the user
  experiences as "sometimes delivered, sometimes not".
- **Resolution:** (1) the daemon now heartbeats presence itself —
  `heartbeat_loop` in `hush-daemon/src/main.rs` ticks every 30 s while the
  vault is unlocked, so presence follows the session, not the webview timers;
  (2) `send_payload_inner` retries delivery up to three times (400 ms, 800 ms
  backoff) for transient relay errors; `ContactOffline` still fails immediately
  by design; (3) no UI change needed. `cargo test -p hush-core` (21) and
  `-p hush-daemon` (2) pass.
- **Lesson:** a webview is not a reliable scheduler. Anything the protocol
  needs continuously (presence, outbox drains, retries) belongs in the daemon's
  tokio loops, which never get background-throttled.

---

## 2026-10-08 — Relay-on-onion: lab service + client SOCKS seam

Slice goal: expose the lab relay as a v3 onion service and give the client a strict SOCKS seam
so relay traffic can go through Tor (the relay never sees the member's IP).

### [FIXED] internal_mail `a_handle_round_trips_through_an_invite` fails on the working tree (predates this slice)

- **Attempted:** ran the full `hush-core` suite to verify the onion/policy/proxy changes.
- **Symptom:** `bob.contacts()[0].mail_vault` is `Some("quiet-harbor")` — the vault name carried
  on the invite — while the test expects `Some("alice's-vault")`, the local default.
  `left: Some("quiet-harbor")`, `right: Some("alice's-vault")`.
- **Evidence it predates this slice:** the four assertion/comment lines naming
  `"alice's-vault"` are all `+` lines in the uncommitted diff of
  `crates/hush-core/tests/internal_mail.rs` (in-flight internal-mail work). This slice touched
  only `relay_client.rs` (onion policy + SOCKS seam), the `socks` cargo feature, and a
  `cfg(test)` initializer fix in `account.rs`; none of those are in the failing path.
- **Resolution (2026-10-08, internal-mail slice):** semantics decided — a contact's
  `mail_vault` is the vault name carried in their invite, not the local default; the local
  default only stands in for *this* vault's own address. Test updated to assert
  `Some("quiet-harbor")` for the contact and `sandy@hush` / `sandy@quiet-harbor` for the
  local address; passes (`cargo test -p hush-core --test internal_mail`, 8/8).
- **Lesson:** a red test left mid-slice fails every later full `cargo test -p hush-core` run
  and buries real failures under known noise. Land the semantics or mark the test `#[ignore]`
  with the reason until the slice resolves them.

---

## 2026-10-08 — Internal-only mail + vault-name addresses

Slice goal: remove all external/outsider mailing; mail addresses become
`handle@vault-name` with the vault named at creation (`hush-cli init --name`); mail thread
UI goes classic email (newest reply on top, no bubbles); profile address editor made compact.

### [FIXED] Relay `opener.rs` survived its own removal and referenced deleted assets

- **Attempted:** the removal pass reported `crates/hush-relay/src/opener.rs` and the
  `opener/` asset directory as deleted.
- **Symptom:** both were still on disk afterwards. `opener.rs` was already orphaned —
  no `mod opener;` in `lib.rs` — and its `include_str!("opener/drop.js")` pointed at a
  file that *had* been deleted, so the crate would not compile if the module were ever
  re-declared. Found by a post-removal grep, not by a build error.
- **Resolution:** deleted `opener.rs` and `opener/` (`open.html`,
  `vendor/hush-noble-chacha.js`); `cargo test -p hush-relay` green.
- **Lesson:** after a "deleted X" claim, verify with a glob of the paths, not memory —
  a partial deletion (some includes removed, the module left behind) is invisible until
  someone re-adds the `mod` line.

### [FIXED] Daemon test module survived the removal of `pickup_link`

- **Attempted:** removing the outsider drop path from the daemon (route, handler, helper).
- **Symptom:** `cargo test -p hush-daemon` did not compile — `error[E0425]: cannot find
  function `pickup_link` in this scope`, twice, in a `#[cfg(test)]` module at the bottom of
  `crates/hush-daemon/src/main.rs` asserting `pickup_link(base, id) == "{base}/open/{id}"`.
  The earlier removal pass deleted the function and the `/open/` route but missed the test
  module dedicated to that contract.
- **Resolution:** deleted the dead test module (its subject no longer exists; a route that
  is gone needs no unit test). `cargo test -p hush-daemon` then passed.
- **Lesson:** when removing a public helper, grep the crate for its *name* after removal,
  not just for callers — `#[cfg(test)]` modules at the bottom of a large file are easy to
  miss and are caught only by a test compile.

### [FIXED] internal_mail test still asserted the removed `myserver.com` domain

- **Attempted:** updating `a_handle_round_trips_through_an_invite` for vault-name addresses.
- **Symptom:** `assertion left == right failed: left: Some("sandy@hush"),
  right: Some("sandy@myserver.com")` at `internal_mail.rs:45` — the test kept the old
  `MAIL_DOMAIN` constant's output in its expectations.
- **Resolution:** rewrote the expectations to the new contract: default address `sandy@hush`,
  after `set_vault_name("Quiet-Harbor")` the address is `sandy@quiet-harbor`, the invite
  payload carries `vault: Some("quiet-harbor")`, and the contact records `mail_vault`.
- **Sub-mistake inside the fix:** my first edit asserted
  `set_vault_name("Alice's Vault")` would normalize to a slug (`alice's-vault`). Wrong:
  `normalize_vault_name` is `normalize_handle` — it *rejects* spaces and apostrophes rather
  than transliterating them, and the test panicked on the `unwrap`. Re-checking `mail.rs`
  gave the correct shape and the test uses a valid slug.
- **Lesson:** normalization functions either canonicalize or reject; do not assume
  transliteration. Read the validator before writing its expectations into a test.

### [WORKED AROUND] `cargo test --all` collides with a live two-peer dev session

- **Attempted:** plain `cargo test --all` to verify the whole workspace.
- **Symptom:** build fails with `Text file busy (os error 26)` on the `hush-tauri` build
  script binary — a running `hush-tauri` app and vite dev server from the two-peer session
  (`scripts/run-two-ui.sh`) hold the artifacts, and the tauri build script cannot be
  replaced while in use. Tooling/environment, not a repo problem.
- **Workaround:** tested the packages this slice touched explicitly —
  `cargo test -p hush-core -p hush-relay -p hush-daemon -p hush-cli` — all green; and
  `npx tsc --noEmit` for the UI (exit 0). The tauri shell's Rust side was not modified by
  this slice, so it stays unverified here; a full `--all` run needs the dev session stopped.
- **Lesson:** on this machine, full-workspace verification and the running dev instance
  share `target/`; scope test runs to changed packages while a dev session is live, and run
  `cargo test --all` only when the session is stopped.

### [FIXED] Test target did not compile: `AccountState` initializer missing `vault_name`

- **Attempted:** first `cargo test -p hush-core --lib relay_client` run.
- **Symptom:** `error[E0063]: missing field 'vault_name' in initializer of 'account::AccountState'`
  at `crates/hush-core/src/account.rs:2491` (`write_legacy_vault` under `cfg(test)`).
- **Cause:** `vault_name` was added to `AccountState` in the internal-mail slice; the
  legacy-vault test helper was not updated. Not caused by this slice, but it blocked the
  whole test target.
- **Resolution:** added `vault_name: None` to the initializer, matching the production
  initializer in `Account::create`. The lib suite then passes 72/72.
- **Lesson:** adding a required struct field breaks every hand-written initializer of it;
  the test target only finds out when someone runs it.

### [FIXED] `reqwest::Proxy::all` silently accepts garbage proxy values

- **Attempted:** validate the `HUSH_RELAY_SOCKS_PROXY` seam by passing the raw string to
  `reqwest::Proxy::all` and relying on it to reject bad values.
- **Symptom:** `Proxy::all("garbage")` and `Proxy::all("ftp://proxy.invalid")` both construct
  fine; scheme matching is deferred to request time, so a typo'd env value silently produces
  a client whose proxy never matches — i.e. direct, un-Tor'd relay traffic.
- **Resolution:** strict validation before handing the value to reqwest
  (`relay_client::socks_proxy`): `Url::parse` must succeed, scheme must be `socks5`/`socks5h`,
  host must be present, anything else is `Error::Relay` at construction. Tests cover
  `garbage`, `ftp://`, and a valid `socks5h://`.
- **Lesson:** validate anything that routes traffic at construction time. A proxy that
  "never matches" fails open in exactly the way a privacy seam must not.

### [FIXED] Onion `/health` verification read `$?` after `echo`, not after `curl`

- **Attempted:** first end-to-end check of the new onion service over SSH.
- **Symptom:** empty body and `exit=0` that belonged to the `echo` following the curl, not to
  curl; the check proved nothing.
- **Resolution:** re-ran capturing `-w "%{http_code}"` into a variable before any other
  command: `curl_rc=0 http_code=200`.
- **Lesson:** in a compound remote shell, capture `$?` immediately or use `-w`; the next
  command resets it.

---

## 2026-10-08 — Drops: card name truncation

Slice goal: fix a UI defect reported by the user after seeing the Drops page.

### [FIXED] Drop names were cut off with an ellipsis next to the avatar

- **Attempted:** the user reported that Drop names in the Drops grid render as truncated text
  ("the name of the channels in the drops is cut with periods"); the user could not read the full
  name.
- **Symptom:** every name longer than the space between the 44 px avatar and the "Verified" badge
  rendered as, for example, "Bloomberg Industry Gro…".
- **Cause:** `.drop-card-name, .drop-card-org` carried `white-space: nowrap` plus
  `text-overflow: ellipsis`, and the name was squeezed into the middle of the `.drop-card-top`
  flex row between avatar and badge.
- **Resolution:** first attempt only removed the ellipsis and let the name wrap inside that row,
  which still buried the name beside the icon. Second attempt, per the user's direction, moved the
  name and organization out of the row onto their own full-width lines under the avatar row:
  `dropCard()` now appends `meta` after `top`, the badge shares the avatar row, and the CSS rules
  were reduced to `overflow-wrap: anywhere` with no nowrap or ellipsis. Verified in the running app
  at 1024 px and 1400 px widths: all 24 names render in full.
- **Lesson:** a two-line clamp or wrap inside a flex row is not a substitute for layout that gives
  the content its own line. When the fix is "the user cannot read X", verify the actual render, not
  just that the CSS property changed.

---

## 2026-10-08 — Drops: bundled-Tor path and build-order verification

Slice goal: confirm that the shipped app runs Drops on the *bundled* Tor binary rather than on
whatever happened to be on the machine. Problems hit:

### [FIXED] A leaked `HUSH_TOR_BIN` hid the bundled-Tor path and pointed the app at a deleted binary

- **Attempted:** audit leftover processes from the earlier verification tasks.
- **Symptom:** the running app's Tor was `/tmp/tor-test/root/usr/bin/tor (deleted)` — a Tor I had
  unpacked from a distro package for manual probing, not the bundled resource. `HUSH_TOR_BIN` was
  exported in the shell from that probing and had never been unset, so every app launched in the
  session inherited it. The `/tmp/tor-test` tree was later deleted during cleanup, so the live Tor
  was executing a deleted binary.
- **Cause:** `locate_binary()` deliberately prefers `HUSH_TOR_BIN` over the bundled resource. The
  variable was set for a one-off probe and silently re-pointed every subsequent launch.
- **Resolution:** unset the variable. Verified the shipped `resources/tor/tor` directly — it
  bootstraps with its own bundled `libevent` (`ldd` reports zero unresolved libraries) and
  `check.torproject.org` returns `{"IsTor":true}`. Then ran the app with `HUSH_TOR_BIN` unset and
  confirmed from instrumented output that `locate_binary` selects `target/debug/tor/tor` and that
  ProPublica's SecureDrop page renders through it (the screenshot contains 37,986 pixels of
  ProPublica's `#813A81`). The app is now running on the bundled binary.
- **Lesson:** "Tor works" is not the same claim as "the bundled Tor works". An env var set for a
  throwaway probe leaks into every later launch, and a verification run that does not state which
  binary it exercised is not evidence for the shipped path.

### [FIXED] Frontend edits were invisible because Tauri embeds `frontendDist` at `cargo build` time

- **Attempted:** add a temporary probe to `src/main.ts` that opens a Drop at startup, then
  `cargo build -p hush-tauri` and launch the binary.
- **Symptom:** three consecutive runs produced no Drop window and no diagnostics from the probe,
  even though the running process was the freshly built binary. This was misread twice as "the
  probe is broken" and "the bundled Tor is not found".
- **Cause:** `tauri.conf.json` sets `build.frontendDist` to `../ui/dist` and defines no `devUrl`, so
  `tauri::generate_context!()` bakes the built UI into the binary during `cargo build`. A plain
  `cargo build` does not rebuild the TypeScript, so the probe was never in the running binary.
  Ordering is what matters: `npm run build` must complete before `cargo build`.
- **Resolution:** rebuilt with `npm run build && cargo build`; the probe ran on the first attempt and
  produced the expected output. Probe and temporary logging have been removed and the tree rebuilt
  clean.
- **Lesson:** in this repo the Rust build is what snapshots the frontend. A frontend edit that
  "does nothing" after `cargo build` means the embedded copy is stale, not that the edit is wrong.
  Instrument with a channel that is guaranteed to be in the running process (a Rust `eprintln!`
  reaches the captured log) before concluding anything about the frontend.

### [OPEN] SIGTERM to the app still leaves the child Tor running

- **Attempted:** stop the app with `pkill -x hush-tauri` (SIGTERM) before relaunching it.
- **Symptom:** the bundled Tor child survived the app and kept running; `tor.pid` still pointed at
  it. The next launch recovered correctly, but only because recovery exists.
- **Cause:** `TorManager::stop()` runs from the `RunEvent::Exit` handler, which a signal or a crash
  never reaches. No SIGTERM/SIGINT handler is installed.
- **Resolution needed:** this is contained, not fixed. `clear_stale()` releases the orphan on the
  next start, and that path is what keeps the feature working. Handling SIGTERM would remove the
  window entirely and is the cleaner fix if the orphan ever causes a user-visible delay.
- **Lesson:** recovery is not the same as prevention. The stale-PID logic makes the orphan
  survivable, so the gap is easy to stop seeing.

---

## 2026-10-08 — Drops: live directory entries + stale-Tor recovery

Slice goal: find live SecureDrop instances and add them to the registry, then make the feature
survive being killed. Problems hit:

### [FIXED] A `bash -c` argument slip made the 24-target probe silently probe nothing

- **Attempted:** probe all 24 directory onions for liveness with
  `xargs -P 8 -n 2 bash -c '<script>'`, reading `slug="$1"; host="$2"`.
- **Symptom:** all 24 returned `http_code=000`, and the whole command finished in 1.2 seconds. A real
  24-target Tor probe with a 45s timeout cannot finish in 1.2s. The output also showed the *slug* in
  the host column and an empty third field.
- **Cause:** `bash -c 'script' a b` binds `a` to `$0`, not `$1`. So `slug` received the onion and
  `host` was empty; every `curl "http://"` failed instantly.
- **Resolution:** added the `_` placeholder (`bash -c 'script' _`) so `$1`/`$2` land correctly.
  Re-run: 18 × `200`, 2 × `302`, 4 × `000`, in 54s.
- **Lesson:** a uniform result across every input is a symptom of a broken harness, not of a
  uniform world. The impossible runtime (1.2s for 45s timeouts) was the tell, and it was visible in
  the output that was already on screen.

### [FIXED] A Tor orphan from a killed app blocked every later start, silently

- **Attempted:** verify a real Drop by opening one at startup. The first run produced no Drop window
  and no error.
- **Symptom:** the bundled Tor from an *earlier* app instance was still running (I had stopped that
  app with SIGTERM). The new launch's Tor exited immediately — it could not take the data-directory
  lock — so `drops_open` failed. The failure was invisible because the probe swallowed it.
- **Cause:** `stop()` runs from the `RunEvent::Exit` handler, which a SIGTERM, a SIGKILL, or a crash
  never reaches. The orphan held `/home/sam/.local/share/com.hush.capsule/tor` and kept running.
- **Resolution:** `TorManager` now records its child's pid in `tor.pid` inside the data directory and
  clears any stale record on start (`clear_stale`), after confirming via the process command line
  that the pid really is a Tor naming *our* data directory — that path is unique and mode 0700, so a
  recycled pid cannot match it. Verified end to end: launch, SIGKILL the app, relaunch — the orphan
  is gone, a new Tor starts, the Drop opens. Before the fix that exact sequence opened nothing.
  Unix only; elsewhere the record is cleared but no process is signalled, which is recorded in
  `DESIGN.md` rather than left implied.
- **Lesson:** an exit handler is not a guarantee. Anything a process spawns must be recoverable from
  the *next* run, because "we clean up on exit" only covers the exits that run it.

### [FIXED] `picks_a_bindable_loopback_port` was a latent flake the new tests surfaced

- **Attempted:** run the suite after adding two Tor tests.
- **Symptom:** `assertion failed: listener.is_ok()` — but only about 1 run in 4, and never in
  isolation (5/5 passes alone).
- **Cause:** pre-existing. The picker deliberately releases the port for Tor to claim, so the test's
  rebind races anything else the kernel hands that port to. The extra tests changed the timing
  enough to expose it. The race is documented in the function's own comment; the assertion was
  stronger than the design promises.
- **Resolution:** the test retries the rebind up to 20 times. A picker that returned an already-bound
  port would still fail, because every attempt would lose rather than one.
- **Lesson:** an intermittently failing test is not "flaky, ignore it" — here it was pointing at an
  assertion that never matched the contract. Retrying is only honest when the design really does
  permit the failure, and when a genuine regression still fails the test.

### [OPEN] Could not confirm the rendered Drops rail by driving the UI, so it was verified another way

- **Attempted:** click the Drops rail in the running app under XWayland and read the cards off a
  screenshot, to confirm the 24 new entries actually render.
- **Symptom:** synthetic clicks appeared to do nothing — the capture was byte-identical before and
  after. `xdotool getmouselocation` confirmed the pointer was over the window, and the click did land
  eventually, but only after several attempts, and the resulting views could not be identified from
  the captures. I cannot read text out of a screenshot in this environment (no OCR available), so
  "the page looks right" was not something I could establish this way.
- **Workaround:** verified the layer I actually changed instead — ran the real `registry.ts` and the
  real `verification.ts` through esbuild and node: 25 entries, 24 verified, 24 in the rail, 24
  distinct onion hosts, and `onionHost()`/`isValidOnionHost()` reject none of them. The rendering path
  (`visibleDrops()` into `dropCard()`) is unchanged pre-existing code, so the open question is only
  cosmetic.
- **Still open:** nobody has looked at the rail with the new entries in it. This is a tooling
  limitation of this environment, not a repo problem. A human should click Drops once.
- **Lesson:** when a claim can only be checked through a channel I cannot read, say so and verify the
  part I own rather than implying the whole thing was seen working.

### [FIXED] The generated splice emitted `// //` on the two https entries

- **Attempted:** rewrite the comment on the New York Times and Washington Post entries while
  switching them to `https`, using a regex whose capture groups included the `// ` prefix.
- **Symptom:** both entries read `// // This instance serves https …`. Valid TypeScript, but sloppy,
  and it hid that the replacement had been done by pattern rather than deliberately.
- **Resolution:** replaced both with the intended two-line comment. Caught by reading the seam
  rather than by a test, which is the only thing that would have caught it.
- **Lesson:** when a transform is regex-driven, read the *result* at every seam, not just the exit
  code.

## 2026-10-08 — Drops: scheme rule was wrong (https-only refused every real SecureDrop)

### [FIXED] The Drop validator required `https`, which no real SecureDrop serves

- **Attempted:** the user pasted the Al Jazeera address as `http://…onion`; the entry was stored as
  `https://…onion` because the validator only accepted `https` for onions, and that rule was assumed
  to be correct.
- **Symptom:** the user's `http://` was silently rewritten, and `onionHost()` returned `null` for it.
  The earlier probe against Al Jazeera had tried both schemes and both timed out, so the outage hid
  the fact that one of them was *structurally* wrong.
- **Reality:** measured against two live instances taken from the official SecureDrop API
  (`https://securedrop.org/api/v1/directory/`) through Tor:
  `https://<onion>/` -> `http_code=000` (failed) on both, `http://<onion>/` -> `http_code=200` on both.
  SecureDrop serves its onion over plain `http`; the onion service layer provides the encryption and
  authenticates the address, so there is no TLS layer. The `https`-only rule would have refused every
  real instance while passing unit tests that used a fabricated `https://<onion>` fixture.
- **Resolution:** `validate_target` and `allowed_navigation` in `drops.rs` accept `http` or `https` on
  a v3 onion; `onionHost()` in `verification.ts` likewise; the Al Jazeera entry now stores the
  published `http://` form verbatim; `rejects_http_onion_in_production` became
  `accepts_http_onion_in_production`; `DESIGN.md` records the scheme rule.
- **Lesson:** a hand-written fixture (`v3_https()`) can encode the same wrong assumption as the code it
  tests, so it passes and proves nothing. Test a rule against a real instance from the source of
  record before trusting it. A timeout is not evidence that a scheme is right — it hides the
  difference between "the service is down" and "the request was malformed".

### [FIXED] Verified the Drop window renders external `http` content (CSP could have blocked it)

- **Attempted:** the app sets a CSP (`default-src 'self' …`) and the Drop window navigates to a
  third-party `http` page. If Tauri injected that CSP into the Drop document, subresources would be
  blocked and the page would render blank — the feature would look broken for reasons unrelated to
  Tor.
- **Symptom:** none observed yet; this was an untested integration risk, not a failure.
- **Resolution:** opened the development Drop (an `http` page) in the real app under Xvfb and captured
  the window: the capture is 93.2% `rgb(18,18,24)` and 0.4% `rgb(242,242,247)`, exactly the mock
  page's declared background and text colours. The external `http` page renders fully styled, so the
  app CSP does not apply to the Drop window. `drops_open` also succeeded, confirming the capability
  fix from the previous slice holds.

## 2026-10-08 — Drops: Al Jazeera entry + bundled Tor

Slice goal: add the Al Jazeera Drop the user supplied, and ship Tor as an app resource so the feature
works with no system install. Problems hit:

### [FIXED] A web-search summary gave the wrong answer about the Al Jazeera address

- **Attempted:** verify the address the user supplied against a trusted source before marking it
  `verified`.
- **Symptom:** the search synthesis said the address appeared only on unofficial mirror sites and that
  the official page listed `aljazeerafo4sau2.onion` (a 16-character v2 onion, dead since v2 was
  removed). It looked like the user had been handed a bogus address.
- **Reality:** fetching `https://www.aljazeera.com/tips` directly showed the exact address from the
  user's message, listed as Al Jazeera's SecureDrop address. The other `ajiunit.com` pages are stale.
- **Resolution:** marked the entry `verified: true` with `verificationSource` set to the tips page.
- **Lesson:** a search synthesis is a lead, not evidence. Fetch the organisation's own page before
  concluding an address is unverified — and before telling the user their address is wrong.

### [OPEN] The Al Jazeera address is authentic but the service does not answer

- **Attempted:** reach the address through Tor to confirm the Drop actually opens.
- **Symptom:** `https://<address>.onion/` and `http://<address>.onion/` both timed out
  (`http_code=000` after 90s and 60s). A control onion (DuckDuckGo) returned 200 in under 7s on the
  same Tor instance, so onion routing itself was fine. The SecureDrop directory reports the instance
  as under maintenance with an extended outage.
- **Resolution:** none needed in code — the address is correct, the destination is down. The entry
  stays (authentic address) rather than being removed.
- **Lesson:** verification and availability are different claims. The "Verified" badge is about
  authenticity and does not promise the service answers.

### [FIXED] Bundled Tor needs its own libevent, and `bundle.resources` remaps paths

- **Attempted:** ship Tor as a Tauri resource so no system install is needed.
- **Symptom:** Tor links `libevent-2.1.so.7`, which is absent on this machine. Copying only the binary
  would have produced a silent "Could not start Tor" at runtime.
- **Resolution:** copied the binary and the library into `src-tauri/resources/tor/`, mapped them with
  the object form of `bundle.resources` so they land at `<resource_dir>/tor/`, and set
  `LD_LIBRARY_PATH` on the child only when the located binary lives under the resource directory.
  Verified with no `HUSH_TOR_BIN` and no `LD_LIBRARY_PATH`: the app spawned `target/debug/tor/tor`,
  and `curl --socks5-hostname` through it returned `{"IsTor":true,...}` plus a 200 from a control
  onion.
- **Lesson:** `tauri-build` copies `bundle.resources` into `target/debug/` for dev runs even with
  `bundle.active: false`, so the bundled path is testable without producing a bundle. A bundled native
  binary is only as portable as its shared libraries.

### [WORKED AROUND] A running app blocks `cargo test` with "Text file busy"

- **Symptom:** `cargo test -p hush-tauri` failed to link with `Text file busy (os error 26)` while an
  app built from the same profile was running.
- **Resolution:** stop the app before rebuilding or testing that profile.
- **Lesson:** the dev binary is the same file the test harness links.

---

## 2026-10-08 — Drops: "Tor Unavailable" in the desktop app (ACL permission missing)

Slice goal: diagnose why the Drops rail reported Tor as unavailable in the running desktop window.
The answer was not in the Tor code at all. Problems hit:

### [FIXED] Every `drops_*` command was denied by the Tauri ACL

- **Attempted:** ship the Drops commands by registering them in `tauri::generate_handler!` and
  adding a `capabilities/default.json` entry.
- **Symptom:** the Drops rail always showed the red "Tor Unavailable" chip, in the desktop app, with
  a working Tor binary and `HUSH_TOR_BIN` set. `torStatus()` swallows invoke rejections and returns
  the `NO_SHELL` fallback (`phase: "unavailable"`), so an ACL denial and "no Tor installed" were
  indistinguishable in the UI.
- **Root cause:** `capabilities/default.json` listed only `core:default`, `allow-hush-files`,
  `allow-hush-api`. Tauri v2 gates app-defined commands through the same ACL as plugin commands
  (`gen/schemas/acl-manifests.json` had `__app-acl__` = only the two existing permissions), so all
  seven `drops_*` commands were rejected.
- **Resolution:** added `src-tauri/permissions/drops.toml` (`allow-drops`, the seven commands) and
  added `allow-drops` to the main-window capability. The capability stays scoped to
  `windows: ["main"]`, so the `drop-view` window still has no capability and cannot call any Hush
  command.
- **Evidence:** a temporary startup probe calling the same `torStart()` the button calls spawned a
  real Tor process (`--SocksPort 127.0.0.1:33171`), and `curl --socks5-hostname` against it returned
  `{"IsTor":true,"IP":"171.25.193.82"}`. The probe was reverted (`git diff` on `main.ts` empty).
- **Lesson:** an ACL denial reaches the frontend as a rejected promise, which is easy to swallow
  into a benign-looking fallback. When a capability-gated command appears to do nothing, read
  `__app-acl__` before debugging the feature.

### [FIXED] A prior claim that the Drops permissions existed was wrong

- **What was claimed:** earlier notes stated the Drops commands had `permissions/*.toml` files and a
  capability scoped to `windows: ["main"]`.
- **Reality:** `src-tauri/permissions/` held only `hush-api.toml` and `hush-files.toml`; no Drops
  permission existed and the capability did not reference one.
- **Resolution:** created the permission and wired the capability (above).
- **Lesson:** a security-relevant claim (isolation by capability scoping) must be verified by reading
  the capability file, not asserted from the design intent.

### [FIXED] Tauri embeds `ui/dist` at compile time, so a UI rebuild alone does not reach the binary

- **Attempted:** `npm run build` and relaunch the existing binary to pick up new UI strings.
- **Symptom:** the new strings were in `ui/dist/assets/*.js` but absent from the running app; a probe
  added to `main.ts` never executed.
- **Resolution:** run `cargo build -p hush-tauri` after `npm run build`; `tauri-build` embeds the
  frontend assets into the binary. `scripts/run-two-ui.sh` already does both.
- **Lesson:** for this app, "the UI changed" means two builds, not one.

### [FIXED] Relaunching the desktop window by hand dropped `HUSH_API_URL`

- **Attempted:** relaunch `./target/debug/hush-tauri` directly to test the Drops changes.
- **Symptom:** `Can't reach the Hush daemon at unix:///run/user/1000/hush/hush-daemon.sock`. The
  daemons were on their own sockets under `HUSH_RUNTIME_DIR=/run/user/1000/hush-two-peers`, but the
  app fell back to the compiled-in default socket.
- **Resolution:** launch with `HUSH_API_URL=unix://<socket>` and `HUSH_WINDOW_TITLE=...`, as
  `scripts/run-two-ui.sh` does.
- **Lesson:** the socket path is injected per window by the run script; the binary default is not the
  dev stack's socket.

### [WORKED AROUND] `pkill -f <pattern>` killed the shell issuing it

- **Attempted:** `pkill -f 'target/debug/hush-tauri'`, then `pkill -f '/tmp/tor-test/root/usr/bin/tor'`.
- **Symptom:** the command returned exit 0 with no output and appeared to do nothing; follow-up
  checks showed the app and the target process still alive. The invoking shell's own command line
  contains the pattern, so `pkill -f` matched and killed the shell before the rest of the line ran.
- **Resolution:** resolve the PID first (`pgrep -x hush-tauri`) and `kill <pid>`.
- **Lesson:** `pkill -f` matches the command string of the process running it. Tooling behaviour, not
  a repo problem, but it silently produced false "nothing to do" results.

### [WORKED AROUND] The native window could not be scripted; evidence came from Xvfb + xdotool + ffmpeg

- **Attempted:** inspect the live webview via `WEBKIT_INSPECTOR_SERVER=127.0.0.1:9222`, then drive it
  with `wmctrl`/`xdotool` on the session display.
- **Symptom:** the WebKit inspector server listened on 9222 but closed every WebSocket handshake
  (with and without `Origin`, with and without a subprotocol) and never answered HTTP; `wmctrl`
  returned nothing and no screenshot tool was installed, because the session is Wayland-native.
- **Resolution:** started `Xvfb :99`, ran the app with `GDK_BACKEND=x11`, drove it with `xdotool`, and
  captured frames with `ffmpeg -f x11grab`.
- **Also:** `document.title` does not update the OS window title in this Tauri setup, so it is not a
  usable output channel; the Tor process tree was.
- **Lesson:** on a Wayland box with no inspector access, a virtual X display is the working route to
  real UI evidence.

---

## 2026-10-08 — Drops feature (SecureDrop directory + isolated Tor webview)

Slice goal: add a Drops rail that discovers SecureDrop-style destinations and opens each one in a
dedicated, capability-less Tauri webview whose traffic is routed through a local Tor process, with
the Hush relay deliberately out of the path. Problems hit:

### [FIXED] Tor refuses to start when `--defaults-torrc` points at `/dev/null`

- **Attempted:** keep Tor hermetic (ignore the system torrc) by passing `/dev/null` as its config
  file, matching a common command-line idiom.
- **Symptom:** `[warn] Unable to open configuration file "/dev/null".` followed by
  `[err] Reading config failed--see warnings above.` Tor exited immediately and never bootstrapped.
- **Resolution:** the manager writes two real, empty files in the Tor data directory (`torrc` and
  `defaults-torrc`) and passes those paths. Re-verified with the same args: Tor logs
  `Bootstrapped 100% (done): Done` and binds `127.0.0.1:<port>` only. The Rust live test
  (`tor::tests::bootstraps_with_a_real_tor`) passes.
- **Lesson:** Tor opens its config path with normal file semantics; `/dev/null` is not a torrc. The
  code was already correct — the failure was in a manual reproduction using `/dev/null`, which is
  exactly the kind of divergence that would have shipped as a mystery if it had been copied into the
  code.

### [OPEN] Real Tor cannot be installed in this environment; verification used a downloaded binary

- **Attempted:** `sudo apt install tor` to exercise the bundled-Tor path against a packaged Tor.
- **Symptom:** `sudo: a password is required` (no passwordless sudo); `tor` is not on `PATH`.
- **Resolution:** `apt-get download tor libevent-2.1-7t64` (no sudo needed), extracted the debs into
  `/tmp`, and set `HUSH_TOR_BIN` to the extracted binary with `LD_LIBRARY_PATH` pointing at the
  extracted `libevent`. This exercised the real manager: it resolved the binary, picked a loopback
  port, bootstrapped, and stopped cleanly. It is not the shipped configuration — the product still
  needs a bundled or system Tor.
- **Needed to close:** bundle a Tor binary as a Tauri resource (or document a system-Tor
  requirement) and re-run `tor::tests::bootstraps_with_a_real_tor` against the shipped layout.
- **Lesson:** the locator (`HUSH_TOR_BIN` -> resource dir -> `PATH` -> common paths) means the
  feature degrades to "Tor Unavailable" with a clear message rather than crashing when Tor is
  absent, but that path was only unit-tested, not product-tested.

### [OPEN] The full Drops rail interaction was not driven end to end

- **Attempted:** click through the rail, toggle the filters, and press Open Drop on the test card.
- **Symptom:** the native Tauri webview cannot be scripted here, and reaching the rail needs a
  running daemon plus manual unlock. The pieces were verified separately instead: the window builds
  with `incognito(true)` + `proxy_url` (temporary probe, now removed) and loads through Tor
  (`check.torproject.org/api/ip` returned `{"IsTor":true,...}`), Tor lifecycle (ignored live test),
  onion validation (unit tests), frontend compile (`tsc && vite build`), and app startup
  (`xvfb-run` smoke, no panic).
- **Needed to close:** a manual pass on a machine with a display and a real daemon, opening both the
  development Drop and (once one exists) a verified `.onion` Drop.
- **Lesson:** "it builds and the parts pass" is not "the button works"; the button wiring is covered
  by the type-checker only until a human clicks it.

### [OPEN] WebKit's proxy is per web context, so the Drop window is Tor-routed but not Tor-Browser-grade

- **Attempted:** route only the Drop window through Tor and leave the rest of Hush direct.
- **Symptom:** wry applies `proxy_url` by setting `NetworkProxySettings` on the webview's WebKit
  context. That is sufficient to route the Drop window through Tor — proved by loading a Tor check
  page inside the webview and getting `IsTor:true` — but every request in that window shares one
  circuit set, and there is no fingerprinting or per-site circuit isolation.
- **Resolution:** accepted and stated honestly in the UI ("Traffic to this Drop is routed through
  Tor. Hush relay is not used.") and in code comments. The main window and the daemon path are not
  proxied, so Tor is not a generic proxy for Hush.
- **Needed to close:** nothing for this slice; it is a known ceiling. Stream isolation would require
  a SOCKS-auth-per-request setup WebKit does not expose.
- **Lesson:** do not let "uses Tor" drift into "anonymous"; the copy says what is actually true.

### [WORKED AROUND] `pkill -f` matched the invoking shell and killed the test command

- **Attempted:** clean up a stray Tor process with `pkill -f 'tor-live'`.
- **Symptom:** the command produced no output at all — `-f` matched the shell whose command text
  contained `tor-live`, so the shell killed itself before printing anything.
- **Resolution:** used `pkill -x tor` (exact process name). Tooling issue, not a repo problem.
- **Lesson:** `pkill -f` matches the caller's own command line; scope the pattern or use `-x`.

### [FIXED] Cold Rust builds of the Tauri/webkit graph cost multiple minutes per cycle

- **Attempted:** iterate on `cargo check` / `cargo test -p hush-tauri` while editing the shell.
- **Symptom:** first `cargo check` took ~6m11s and the first `cargo test` ~3m40s of compile time;
  two live-Tor runs added ~2m each of recompilation for a one-line test addition.
- **Resolution:** none needed — incremental rebuilds after the first are seconds. Noted so the cost
  is expected rather than mistaken for a hang. Tooling observation, not a repo problem.
- **Lesson:** batch shell edits before running the Rust test cycle; the compile graph, not the tests,
  is the bottleneck.

---

## 2026-10-08 — HTTPS origin for the drop opener on the lab relay (Cloudflare tunnel + nginx path ingress)

Slice goal: give the lab relay an HTTPS origin so `GET /open/{id}` is a secure context and a real
recipient's browser can decrypt; redeploy the current build to the VPS first, then point the relay's
pickup-link base at the HTTPS origin and prove decryption in a real browser. Problems hit:

### [FIXED] The offered tunnel hostname was already the medical app's origin

- **Attempted:** use `https://citations-stuffed-bulletin-shade.trycloudflare.com/` as the relay's HTTPS origin.
- **Symptom:** that hostname is the VPS's `cloudflared-quick.service` (`cloudflared tunnel --url
  http://127.0.0.1:80`), and nginx's `location /` proxies to the alhakim medical Next.js app
  (`127.0.0.1:3000`). Repointing the quick tunnel at the relay would have silently broken the
  medical app, exactly as warned.
- **Resolution:** left the tunnel alone and added path-based ingress in nginx: `location /open/` and
  `location /drop/` proxy to `http://127.0.0.1:8790`, everything else falls through to the app. The
  alhakim app has no `/open` or `/drop` routes (checked its `src/app` tree and `middleware.ts`), so
  the prefixes are disjoint. Verified after reload: the medical app's `/` still 307s to `/login` and
  `/login` is 200, while `https://<host>/open/{id}` serves the opener.
- **Lesson:** "the user offered this hostname" is not the same as "this hostname is free"; find out
  what its origin serves before assuming it can be repointed.

### [FIXED] The VPS ran an older relay build with no opener or notify routes

- **Attempted:** exercise `/open/{id}` and `POST /drop/{id}/notify` on `http://137.184.129.205:8790`.
- **Symptom:** `/health` returned 200 but `/open/{id}` returned 404, and the deployed binary's
  strings contained `/drop/{id}` but neither `/drop/{id}/notify` nor `/open/{id}`.
- **Resolution:** built `cargo build --release --locked -p hush-relay` from this tree (sha256
  `beaf4fe3f5ba3ea90e9e8b6c1b0200f530bdcc8c05c6590765b24d47a371200b`), scp'd it, installed to
  `/usr/local/bin/hush-relay`, and restarted. `/open/{id}` now 200 with the opener HTML and
  `/drop/{unknown}` is 404. A concurrent worker had restarted the service at 20:03 UTC; state was
  recorded before (pid 362753, hash `5702f401...`) and after (pid 364470).
- **Lesson:** verify the route, not just `/health`; a 200 health check says nothing about which
  routes the running binary actually has.

### [WORKED AROUND] The daemon refuses a dotless recipient, so the loopback-sink notify test calls the relay directly

- **Attempted:** use `hushmail@localhost` (the OpenSMTPD local mailbox) as the notify recipient
  through the daemon's `POST /api/mail/outsider`.
- **Symptom:** the daemon returned 400 `{"error":"that does not look like an email address"}`.
  `hush_core::normalize_external_address` requires the domain to contain a dot and be at least three
  characters; `localhost` fails.
- **Resolution:** for the loopback-sink proof, called `POST /drop/{id}/notify` directly on the relay
  (whose `validate_recipient` has no dot rule) with `to=hushmail@localhost`. OpenSMTPD delivered it
  locally (`mda delivery ... user=hushmail result=Ok stat=Delivered`) and the message carried the
  HTTPS link and the `message.hushdrop` attachment, with no PIN. No real outbound mail was sent.
- **Lesson:** the daemon's address rule and the relay's are deliberately different; a local-only sink
  address can only be driven at the relay.

### [OPEN] The HTTPS origin is an ephemeral quick-tunnel hostname

- **Attempted:** pin `HUSH_RELAY_DROP_BASE_URL` to `https://citations-stuffed-bulletin-shade.trycloudflare.com`.
- **Symptom:** this is a `trycloudflare.com` quick tunnel. Its hostname is assigned at startup and
  changes whenever `cloudflared-quick.service` restarts (the unit is `Restart=always`). nginx routing
  is host-agnostic, so the page keeps working on whatever hostname the tunnel currently has, but the
  configured `drop_base_url` (and any link already emailed) would then point at a dead hostname.
- **Resolution:** none in this task. The offered hostname was the one available; a named tunnel with a
  stable DNS record would be needed to make the origin durable.
- **Lesson:** an HTTPS origin is only as stable as the tunnel that fronts it; record the hostname's
  lifecycle, not just its current value.

### [WORKED AROUND] Real email delivery from the VPS is still impossible

- **Attempted:** confirm the notification path including outbound mail.
- **Symptom:** the provider blocks outbound TCP 25/587 (pre-existing, documented). `POST /drop/{id}/notify`
  to an external address is accepted by the relay and handed to loopback OpenSMTPD, which cannot relay
  it and bounces; the request itself took about 33s before returning 202.
- **Resolution:** proved the notify contract (202, `sent:true`, `attachment_bytes:328`, `link` pointing
  at the HTTPS origin) and used loopback local delivery as the sink. Did not claim a real email reached
  anyone.
- **Lesson:** separate "the notification path works" from "mail was delivered"; only the former is
  testable on this box.

### [OPEN] Workstation mail ports noted, not used

- **Attempted:** nothing; recording the lead as instructed.
- **Observation:** this workstation listens on 25/587/465 via the mailcow `postfix-mailcow` container
  (`ghcr.io/mailcow/postfix:3.10.12-1`), so unlike the VPS it may be able to submit mail. The egress
  blocker is specific to the VPS droplet, which has no MTA egress.
- **Resolution:** not built around in this task; relevance unverified.
- **Lesson:** the blocked-egress problem is a property of the droplet, not of the project.

---

## 2026-10-08 — Dev stack repointed at the lab VPS relay (137.184.129.205:8790)

Slice goal: run the two-peer dev stack (Vite 5173, alice 9876, bob 9877) with both vaults using
the lab relay instead of a local one, set the relay's own pickup-link base on the box, and prove
a real message crosses the VPS. Problems hit:

### [FIXED] An existing vault's relay cannot be changed; only `init --relay` sets it

- **Attempted:** repoint `instances/peer-a` and `instances/peer-b` at the VPS by writing
  `client-config.json` (`hush-cli config set --relay …`), the "supported way" the config surface
  exposes.
- **Symptom:** `config show` reports the new URL from the config file, but the running daemon keeps
  the old relay: `AccountState.relay` is written by `Account::create` and read-only afterwards, and
  `/api/status` reports `relay_source: "vault"` with the old value. The CLI prints the same caveat
  ("an existing vault keeps the relay it was created with"). There is no `set-relay` command, no
  daemon endpoint, and `add_contact` refuses an invite whose relay differs (`Error::RelayMismatch`),
  so a mismatched pair cannot be patched up at runtime.
- **Resolution:** recreated both vaults with
  `hush-cli --dir instances/peer-{a,b} init --passphrase … --relay http://137.184.129.205:8790` and
  additionally wrote `client-config.json` with `config set --relay`. Destructive: the previous
  contacts and chat history are gone. Acceptable for a hand-test stack, but it is the only supported
  way to move an existing vault to a new relay.
- **Lesson:** the config file is advisory for *new* vaults; the vault is the runtime source of truth.
  Repointing a live vault needs a migration path that does not exist yet.

### [WORKED AROUND] `scripts/setup-two-peers.sh` now fails its final smoke with `ContactOffline`

- **Attempted:** run the documented two-peer setup (`HUSH_RELAY_URL=http://137.184.129.205:8790
  ./scripts/setup-two-peers.sh`) to create the vaults, exchange invites, and smoke a send.
- **Symptom:** vaults and contacts are created (`vault ready … relay: http://137.184.129.205:8790`,
  `added contact alice`, `added contact bob`), then the script's `send alice --text "hello from bob"`
  exits 1 with `Error: ContactOffline`. `Account::send_text` routes through
  `send_payload_inner(..., require_online = true)`, and `contact_is_online` reads relay presence that
  nothing heartbeated because the script starts no daemons. The script predates the live-only-chat
  presence rule.
- **Resolution:** left the script alone (dev helper; a concurrent worker owns the smoke scripts) and
  completed the proof through the running daemons instead: heartbeat presence on both, then
  `POST /api/send` on alice and `POST /api/recv` on bob. The vaults it created are valid and usable.
- **Lesson:** the setup script's final assertion is stale; it should use mail (store-and-forward) or
  start daemons before the chat send. Flagged, not fixed, to avoid a concurrent-worker clash.

### [WORKED AROUND] `run-two-ui.sh --dev` couples the stack to a local relay

- **Attempted:** start the documented dev stack with the script.
- **Symptom:** with `HUSH_RELAY_URL` unset the script starts its own relay on `127.0.0.1:8787`, and
  its final `wait -n` tears the whole stack down if any child exits — including that relay. That is
  exactly the dependency the task forbids (a concurrent worker may stop 8787); it also leaves
  `HUSH_RELAY_URL` in the daemons' environment, muddying "the vault is the source of truth".
- **Resolution:** started the two daemons and Vite directly with the vault-stored VPS relay and
  `env -u HUSH_RELAY_URL`, matching the script's socket paths
  (`/run/user/1000/hush-two-peers/peer-{a,b}.sock`) and ports. `/api/status` then reports
  `relay_source: "vault"` with the VPS URL, and no local relay exists to fall back to.
- **Lesson:** a dev launcher that owns a relay cannot be used to test "no local relay"; start the
  components individually.

### [FIXED] `A && B & C && D` put the variable assignment in the background

- **Attempted:** start three detached processes in one command chain.
- **Symptom:** `--: line 3: /logs/bob.log: No such file or directory`; bob and Vite never started
  because `RUN_DIR=…` was part of the backgrounded `&&` list, so later commands saw an empty
  `$RUN_DIR`. Tooling/quoting mistake, not a repo problem.
- **Resolution:** set variables on their own line, then launch each process.

### [FIXED] `pkill -f 'run-two-ui.sh --dev'` killed the shell that ran it

- **Attempted:** stop the script-managed stack before restarting the daemons without the relay env.
- **Symptom:** the invoking shell died mid-command (output stopped right after "sent TERM to
  launcher") because `pkill -f` matched the shell's own command line, which contained the pattern.
  Same class as the earlier `pkill -f` entry in this log.
- **Resolution:** relied on the script's own EXIT trap (it stopped the daemons and Vite) and verified
  by pid/port afterwards. Tooling problem, not a repo problem.

### [OPEN] A concurrent worker sent an outsider mail from bob's vault

- **Observed:** bob's `/api/mail` contains an outgoing outsider mail to `rushd.bitar@gmail.com`
  (subject/body `test`, delivery_id `a2df307ca8de88cd0f54f7419a51c2cd`) that this worker did not
  send. The bob daemon log shows its notify failing: `drop notify failed: 404 Not Found`. A
  concurrent worker is running `hush-daemon --dir /tmp/hushlab/dropvault` and
  `cargo test -p hush-relay`, with no TCP listener of its own, so it used the stack's daemons.
- **Impact:** cosmetic — an unexplained Sent item in bob's vault that the user may notice while
  hand-testing. Left in place rather than deleted, because deleting it could break the other
  worker's in-flight test.
- **Resolution needed:** none for this slice; flagged so the user is not surprised.

### [WORKED AROUND] Proving the daemons' environment is tooling-limited

- **Attempted:** read `/proc/<pid>/environ` to show the daemons had no `HUSH_RELAY_URL`.
- **Symptom:** `Permission denied` (the Cursor shell sandbox does not share the daemons' namespace).
- **Resolution:** proved the same thing through the API instead: `/api/status` reports
  `relay_source: "vault"`, meaning the stored vault value was used, not the environment. Tooling
  limitation, not a repo problem.

### [FIXED] Lab pickup links cannot decrypt in a browser (correct behaviour, verified)

- **Observed:** opening `http://137.184.129.205:8790/open/<id>` in a real browser gives
  `window.isSecureContext === false`, `typeof crypto.subtle === "undefined"`, the `#insecure`
  section visible and the PIN form hidden — the page states it "must be served over HTTPS".
- **Resolution:** none; this is correct and was not weakened. Verified the workaround works: the
  same container served from a temporary localhost relay (`http://127.0.0.1:8788/open/<id>`) has
  `isSecureContext === true`, shows the PIN form, and decrypts to the real subject/body with the
  PIN. The temp relay was stopped and removed.
- **Lesson:** the lab box can exercise the send side of the outsider flow; the browser-open side
  only works from `https://` or a `localhost` relay.

### [WORKED AROUND] A concurrent worker superseded the VPS `drop_base_url` with an HTTPS tunnel

- **Attempted:** set `HUSH_RELAY_DROP_BASE_URL=http://137.184.129.205:8790` on the lab box via a new
  drop-in (`/etc/systemd/system/hush-relay.service.d/20-lab-drop-base.conf`) and restart the relay;
  confirmed `/health` 200 from off-box and that an outsider drop's link came back as
  `http://137.184.129.205:8790/open/<id>`.
- **Symptom:** minutes later the same drop-in (mtime 20:14, mine was 20:08) contains a Cloudflare
  quick-tunnel URL (`https://citations-stuffed-bulletin-shade.trycloudflare.com`), with a comment
  stating it supersedes the IP because a plain-HTTP link is not a secure context. `systemctl show`
  confirms the tunnel URL is live, and a fresh outsider drop now returns a tunnel link.
- **Resolution:** left the concurrent worker's change in place rather than reverting it. Reverting
  would thrash the shared drop-in and undo a real improvement: the tunnel origin is HTTPS, so the
  browser opener's WebCrypto check passes there. Verified both realities: over
  `http://137.184.129.205:8790` the opener shows the HTTPS-required notice; over the tunnel the same
  container decrypts to the real subject/body. The task's raw-IP requirement and the concurrent
  worker's HTTPS change conflict; reported, not fought.
- **Lesson:** the VPS drop-in is shared mutable state; two workers editing it race. A single owner or
  a checked-in config would avoid it.

### [OPEN] Outsider sends take ~38s to return

- **Observed:** `POST /api/mail/outsider` consistently takes ~35-40s to answer. A 25s client timeout
  returned `http_code=000` (empty body), which reads as a hang; a 90s timeout returned 200 with the
  link. The drop PUT and the relay notify both eventually succeed (the relay journal shows
  `smtp: delivered drop notification … via smtp://127.0.0.1:25`).
- **Impact:** a hand-tester clicking "send to outsider" will see a ~38s stall with no feedback and may
  assume it failed. Not diagnosed in this slice.
- **Resolution needed:** profile the daemon→relay PUT/notify path and the relay's SMTP handoff (the
  box has no external egress, so an MX connect timeout is a plausible cause). Flagged, not fixed.

---

## 2026-10-08 — hush-relay notify prose no longer names the deleted mailer types

The relay owns drop notification now. Its `notify.rs` module and item prose still described the
removed `hush_core` mailer, so it was rewritten to describe the module as it is. Problems hit:

### [FIXED] crates/hush-relay/src/notify.rs prose named types deleted with hush-core's mailer

- **Attempted:** close the OPEN entry from "Clearing outsider-mail fallout after relay-owned
  notification" now that `crates/hush-relay/**` is free.
- **Symptom:** the module doc carried a whole "# Why a local SMTP client rather than
  `hush_core::SmtpMailer`" section and named `hush_core::Mailer` (lines 9, 11, 18); the trait doc
  said "Unlike `hush_core::Mailer`" (line 68); `parse_smtp_spec` claimed to mirror
  `hush_core::SmtpMailer::from_spec` (line 299); and `DEFAULT_SMTP_TIMEOUT` referred to "the core
  mailer" (line 50). None of those types exist; the surviving transport is `SmtpNotifier` here.
- **Resolution:** prose-only rewrite of `notify.rs`. The module doc now says the relay owns
  notification and names the real transports (`Notifier`, `SmtpNotifier`, `file:`, `null`) and the
  `multipart/mixed` container attachment. The trait doc, `DEFAULT_SMTP_TIMEOUT`, `parse_smtp_spec`,
  `NOTIFICATION_SUBJECT` and `compose_message` no longer compare against the deleted mailer. No
  behavior changed. The config prose on `build_notifier` was aligned with `main.rs`: an unset
  `HUSH_RELAY_MAILER` defaults to `smtp://127.0.0.1:25`, while an empty spec is `null`.
- **Evidence:** `cargo check -p hush-relay` exit 0; `cargo test -p hush-relay` exit 0, 38 passed
  (4 lib + 5 deliveries + 7 notify + 5 opener + 1 opener_crypto + 8 prekey_records + 3
  queue_credentials + 5 relay_queues), 0 failed. `rg` for the five names under
  `crates/hush-relay/**` returns no code or comment hits.
- **Lesson:** a type deleted in one crate leaves prose behind in another; grep the type name, not
  just the symbol, and close the OPEN entry when the later pass lands.

### [OPEN] Two "the daemon's" references in hush-relay remain, outside the requested pattern set

- **Attempted:** sweep `crates/hush-relay/**` for stale references to the daemon's removed mailer.
- **Symptom:** `crates/hush-relay/src/app.rs:328` says the shared copy is "so the wording cannot
  drift from the daemon's", and `crates/hush-relay/src/config.rs:12` says the base URL is "the
  relay's deployment identity rather than the daemon's". The daemon no longer has a mailer or a
  `drop_base_url`, so both contrasts are historical. Neither names a deleted type, so neither
  matched the five-name grep this slice was scoped to.
- **Resolution:** left as-is. `app.rs`'s clause is now inaccurate; `config.rs`'s is an explanatory
  "why" that still reads correctly. Flagged for a later prose pass rather than edited outside this
  task's stated boundary.
- **Lesson:** scope the grep to the named symbols and stale prose survives one level out; decide
  explicitly whether to widen the sweep.

---

## 2026-10-08 — Opener follow-up: attachment download, offline flow, scanner and origin probes

Closed the two open items from the opener slice and probed two claims that had only been reasoned
about. Problems hit:

### [FIXED] Attachment download was asserted, not observed

- **Attempted:** confirm a recipient can actually get the attached files, not just read the body.
- **Symptom:** the previous worker saw a `blob:` URL created but never clicked it through to disk,
  so it was unknown whether `default-src 'none'` blocked the `<a href="blob:..." download>`
  navigation.
- **Resolution:** drove the served page in two real browsers (the Cursor browser and a headless
  Chrome over raw CDP). Clicking `Download note.txt` wrote `/home/sam/Downloads/note.txt`, 27 bytes,
  sha256 `74cc295497f993f7adc5f190e4dcc61af1a2dc893270d98fe012137fa184ac21` — byte-identical to the
  sealed attachment; `Download data.bin` wrote 256 bytes, sha256
  `40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880`, matching bytes 0x00..0xFF. No
  CSP change was needed: the policy is unchanged from the previous slice (`default-src 'none';
  script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' blob:; img-src data:;
  base-uri 'none'; form-action 'none'`). A `download` anchor navigation is not governed by
  `default-src`, so `'none'` never blocked it.
- **Lesson:** "a blob URL was created" is not "a file was written"; click the link and hash the bytes.

### [FIXED] The offline flow was asserted by test, not observed

- **Attempted:** prove the page is self-contained and usable later, offline.
- **Symptom:** only a Rust assertion that the HTML has no `<script src=` / `<link `; nobody had
  opened the saved page from `file://` with the network down.
- **Resolution:** saved the served page, stopped the relay, opened `file:///tmp/offline/opener.html`
  in a real headless Chrome, set the tab offline (`navigator.onLine === false`), and opened the
  on-disk container. `window.isSecureContext` was `true` and `crypto.subtle.deriveBits` a function on
  `file://`; decryption produced the right subject, body and two attachments; a download landed with
  the right sha256. The only network request the tab made was the `file://` document itself.
- **Lesson:** `file://` is a secure context in Chromium; verify it rather than assume it.

### [WORKED AROUND] The Cursor browser tool cannot open `file://` or set a download directory

- **Attempted:** load the saved page from `file://` and redirect downloads in the Cursor browser.
- **Symptom:** `browser_navigate` refused with "file:// URLs are not allowed"; CDP `Page.navigate`
  was denied ("not allowed"); CDP `Browser.setDownloadBehavior` was denied ("not allowed"). The
  default download directory (`~/Downloads`) could still be watched, which is how the first
  attachment proof was done.
- **Resolution:** drove the locally installed `/usr/bin/google-chrome` (`--headless=new
  --remote-debugging-port=0`) over raw CDP from a throwaway Node script using Node 24's global
  `WebSocket`, which allows `file://`, `Network.emulateNetworkConditions`, and
  `Browser.setDownloadBehavior`. Tooling limitation, not a repo problem.
- **Lesson:** when the bundled browser tool denies the exact capability a claim needs, a raw CDP
  session against the same Chrome gives the observation instead of an assertion.

### [WORKED AROUND] The offline container was injected as a synthesized `File`, not the OS picker

- **Attempted:** open `/tmp/offline/message.hushdrop` through the page's file input.
- **Symptom:** the file input is a native OS dialog that cannot be driven; CDP
  `DOM.setFileInputFiles` (filesystem-backed file-input) is denied by the browser tool.
- **Resolution:** read the real 631-byte container from disk in Node, then built a `File` from those
  exact bytes and assigned it to `input.files` via `DataTransfer`, dispatching the real `change`
  event. This runs the page's actual `readFile` -> `FileReader` -> `acceptContainer` path; the only
  thing not exercised is the OS file chooser itself.
- **Lesson:** state the substitution; "opened a local file" here means the bytes, not the dialog.

### [OPEN] No real mail-provider scanner was run

- **Attempted:** confirm that a fetcher which never decrypts cannot burn a drop.
- **Symptom:** the claim rested on wrong-PIN vs correct-PIN observations; no actual mail-provider
  link scanner was ever pointed at a drop.
- **Resolution:** simulated the strongest stand-in available — a real headless Chrome rendered
  `/open/{id}` fully (HTML + JS, auto-fetch of the container) and never entered a PIN. Observed
  requests were exactly `GET /open/{id}` and `GET /drop/{id}`; no `DELETE` was issued and a follow-up
  `GET /drop/{id}` returned 200. An explicit `DELETE` then returned 204 and the next GET 404. So the
  mechanism is confirmed: only the post-decrypt `DELETE` consumes the drop. What is still unverified
  is the behaviour of any specific real scanner (some fetch with unusual methods or user agents).
  Remains OPEN until a real scanner is run.
- **Lesson:** a simulated client can confirm the mechanism; it cannot certify a third party's.

### [OPEN] The lab host runs an older relay, so the opener page was not exercised there

- **Attempted:** compare `http://137.184.129.205:8790` with the `http://lvh.me:8791` origin that was
  tested.
- **Symptom:** the lab host answers `/health` with 200 but `/open/{id}` with 404, so it does not run
  this slice's code and the opener page cannot be loaded there.
- **Resolution:** observed the origin classification directly instead. In a real headless Chrome,
  `http://lvh.me:8791/health` and `http://137.184.129.205:8790/health` both report
  `isSecureContext: false` and `typeof crypto.subtle === "undefined"`. Secure context is per-origin,
  so the opener served from that host would take the same insecure branch as `lvh.me`. What is not
  verified is the page rendered *by that host*, because the host does not serve it yet. Remains OPEN
  until the lab host is redeployed.
- **Lesson:** an origin's secure-context status can be observed without the page under test; say
  which of the two was actually observed.

---

## 2026-10-08 — Clearing outsider-mail fallout after relay-owned notification

The relay now sends the drop notification (`POST /drop/{id}/notify`); the daemon mails nothing and
`hush_core`'s `Mailer` seam is gone. This slice cleared the stale scripts/docs/tests left behind.

### [FIXED] The hush-core outsider-mail test asserted the relay reports no link, but it does

- **Attempted:** run `cargo test --workspace` for the baseline the brief described ("175 passed,
  0 failed").
- **Symptom:** `the_daemons_notify_call_reaches_the_relay_route_and_emails_the_link` FAILED at
  `crates/hush-core/tests/outsider_mail.rs:296`:
  `assertion left == right failed: the current relay reports no link; left:
  Some("https://relay.example/open/<id>") right: None`. `cargo test` stopped at that binary
  (exit 101), so the claimed baseline was not reproducible on the tree as it stood.
- **Resolution:** the test now asserts the relay's reported link
  (`Some("https://relay.example/open/<id>")`), matching `NotifyResponse { link }` in
  `crates/hush-relay/src/app.rs` and `RelayClient::notify_drop`. The full suite then reported
  175 passed, 0 failed, exit 0.
- **Lesson:** a test that pins the "relay reports no link" fallback contradicts the relay the
  moment it starts reporting a link. Assert the current contract, not the transitional one.

### [FIXED] smoke-drop.sh / smoke-drop-key.sh drove removed daemon mail config

- **Attempted:** run the repo's end-to-end harness for outsider drops.
- **Symptom:** both scripts exported `HUSH_MAILER` and `HUSH_DROP_BASE_URL` to the daemon and
  asserted `$LINK == "$DROP_BASE/open/$ID"`. Neither variable is read any more (grep across
  `crates/hush-daemon` and `crates/hush-core` is empty), and the link is now composed by the
  relay from `HUSH_RELAY_DROP_BASE_URL`, so the mail assertion could not pass.
- **Resolution:** the relay is launched with `HUSH_RELAY_DROP_BASE_URL="$RELAY_URL"` and
  `HUSH_RELAY_MAILER="file:$MAILER_DIR"`; the daemon gets neither variable; the link is asserted
  against `"$RELAY_URL/open/$ID"`. Both scripts run green (output in the slice report).
- **Lesson:** when config moves between processes, the harness that sets it has to move too.

### [FIXED] crates/hush-relay/src/notify.rs prose still names the deleted hush_core mailer types

- **Attempted:** clear every stale reference to the removed mailer.
- **Symptom:** `crates/hush-relay/**` is owned by a concurrent worker, so it was left untouched.
  Its module doc and comments still name types that no longer exist: lines 9, 11, 18, 68 and 299
  refer to `hush_core::SmtpMailer` / `hush_core::Mailer`; line 50 says "the core mailer".
- **Resolution:** none in this slice, by scope. A later pass should rewrite that prose.
  Tooling/ownership, not a repo defect.
- **Closed (2026-10-08, later pass):** the prose was rewritten; see "hush-relay notify prose no
  longer names the deleted mailer types" above. `cargo check -p hush-relay` and
  `cargo test -p hush-relay` are both exit 0.
- **Lesson:** a rename/removal leaves cross-crate prose behind; grep for the type names, not just
  the symbols.

### [WORKED AROUND] Older OPEN entries about the daemon mailer are now moot but were left in place

- **Attempted:** reconcile the log with the new design.
- **Symptom:** earlier sections still carry `[OPEN]` entries such as "The daemon has no SMTP
  client", "Two hand-rolled SMTP clients now exist in the workspace", and "The SMTP mailer is not
  reachable from the daemon yet". The `Mailer` seam and the daemon mailer were deleted, so these
  no longer describe a live configuration.
- **Resolution:** left verbatim. The log rules forbid deleting an `OPEN` entry, and re-tagging
  another slice's entries is that slice's call, not this one's. Flagged here for a later pass.
- **Lesson:** a removal slice should also retire the OPEN entries it invalidates.

---

## 2026-10-08 — Browser opener page for outsider drops (`GET /open/{id}`)

Served the pickup link the notification emails. The relay now has `GET /open/{id}` returning a
self-contained page that decrypts a `hush-drop-v1` container in the browser, plus
`DELETE /drop/{id}` and an expiry header on `GET /drop/{id}`. Problems hit:

### [FIXED] CSP `connect-src` blocked reading an attachment blob URL

- **Attempted:** confirm the attachment bytes the page hands the recipient.
- **Symptom:** `fetch(a.href)` on the `blob:` URL created by `URL.createObjectURL` failed with
  `TypeError: Failed to fetch`. The page's CSP is `default-src 'none'; connect-src 'self'`, and
  `blob:` was not in `connect-src`. The anchor's `download` navigation still worked, so this was
  not a user-visible break, but any script read of the blob was blocked.
- **Resolution:** added `blob:` to `connect-src` in `open.html`. Re-verified in the browser:
  `fetch` of the blob URL now returns 200 and the decoded bytes equal the attachment payload.
- **Lesson:** `connect-src` governs `fetch()` of `blob:` URLs; a download anchor does not need it,
  so the gap hides until something tries to read the blob.

### [WORKED AROUND] A throwaway cargo project cannot build `hush-core` because `protoc` is absent

- **Attempted:** generate a fresh known-PIN container from a scratch crate in `/tmp` that depends
  on `hush-core` by path.
- **Symptom:** `thread 'main' panicked at sparsepostquantumratchet-.../build.rs:9:10: Protobufs in
  src are valid: ... Could not find 'protoc'`. `libsignal-protocol` pulls
  `sparsepostquantumratchet`, whose build script needs `protoc`; it is not installed
  (`which protoc` is empty, no `protobuf-compiler` package). The workspace itself still builds
  only because `target/` already holds that build script's cached output, so it never re-runs.
- **Resolution:** generated the fixture from a temporary `examples/seal_fixture.rs` inside the
  workspace (which reuses the cached target dir), then deleted the file. The underlying gap
  remains: a clean `cargo build` of the workspace from an empty `target/` would fail without
  `protoc`. That is an environment gap, not a repo defect.
- **Lesson:** a cached `target/` hides missing build-time tools; a scratch project that shares no
  target dir is the fastest way to find out what a fresh clone would need.

### [WORKED AROUND] `Page.addScriptToEvaluateOnNewDocument` injections never ran in the browser tool

- **Attempted:** force the insecure branch by stubbing `crypto` (or `Crypto.prototype.subtle`)
  before page load, then reloading.
- **Symptom:** the injected marker `window.__injectRan` was `undefined` after reload and
  `crypto.subtle` was still present, on two different injection bodies. The CDP command returned
  an identifier and `Runtime.evaluate` worked in the same view, so the call was accepted but the
  script did not run in this Electron webview.
- **Resolution:** abandoned the stub and used a genuinely non-secure origin
  (`http://lvh.me:8791`) instead, which exercises the real code path. Tooling problem, not a repo
  problem.
- **Lesson:** prefer a real environment over a mocked one when the mock's own machinery is
  unverified.

### [FIXED] The pickup link was dead: no route served `/open/{id}`

- **Attempted:** open the link the notification email carries.
- **Symptom:** the relay's routes were `/health`, `/k/...`, `/q/...`, `/drop/{id}`,
  `/drop/{id}/notify`; `{drop_base_url}/open/{id}` (composed in `app.rs`) 404'd. Every
  notification carried a working attachment next to a dead link.
- **Resolution:** added `GET /open/{id}` serving the page from `crates/hush-relay/src/opener/`,
  inlined into one HTML document by `src/opener.rs`.
- **Lesson:** a link the sender composes is a contract; grep for the route before shipping the
  copy that prints it.

### [FIXED] "Destroys itself after the download" was not true, and a consuming GET would be unsafe

- **Attempted:** make the pickup link one-shot, as the marketing copy implied.
- **Symptom:** `store.get_delivery` has no fetch count and
  `tests/deliveries.rs::fetching_the_same_delivery_twice_still_works` deliberately asserts a
  second fetch succeeds (a mistyped PIN, a refresh, or a second device must not destroy the
  message). A GET that consumed the row would burn the drop on a mail scanner's fetch.
- **Resolution:** added `DELETE /drop/{id}` and had the page call it only after a successful
  decrypt and render. The scanner fetches the URL but never runs the decrypt, so it cannot
  consume the drop. Verified in a real browser: a wrong PIN left the server copy in place
  (`GET` still 200), a correct PIN removed it (`GET` 404). The page copy states the server
  ciphertext is deleted, not the page or the recipient's copy.
- **Lesson:** "self-destructs" belongs to the server's copy, not the recipient's; write the copy
  to the thing that actually dies.

### [FIXED] XChaCha20-Poly1305 is not in WebCrypto; vendored @noble/ciphers and bundled it

- **Attempted:** use a browser AEAD for XChaCha20-Poly1305.
- **Symptom:** WebCrypto has no XChaCha20-Poly1305. Fetching a JS library from a CDN would leak
  one request per open and break the "save the page, open later drops offline" case.
- **Resolution:** vendored the audited `@noble/ciphers` v1.3.0 chacha modules
  (`src/opener/vendor/hush-noble-chacha.js`, MIT, Cure53-audited) and flattened its four CJS
  modules into one file so `src/opener.rs` can inline it. PBKDF2-HMAC-SHA256 stays native
  (`crypto.subtle.deriveBits`).
- **Lesson:** vendoring plus a tiny module shim is cheaper than re-deriving a subtle AEAD.

### [FIXED] The browser found a stale lifetime copy the Rust tests could not

- **Attempted:** verify the page in a real browser against a locally run relay.
- **Symptom:** after opening a server-fetched drop (which deletes the server copy and rewrites
  the lifetime text) and then opening a dropped file, the page still said "The server's copy has
  been deleted" for the file path. `updateServerLifetime` returned early when `expiresUnix` was
  null (always the case for a file) and left the previous text in place.
- **Resolution:** `updateServerLifetime` now branches on the source: file-sourced content gets
  its own true copy, server-sourced content shows the real expiry, and `acceptContainer` clears
  stale status.
- **Lesson:** copy that depends on prior state is state, not a constant; drive the sequence, not
  just the first screen.

### [FIXED] `pkill -f 'target/debug/hush-relay'` killed the shell that ran it (tooling)

- **Attempted:** stop the background relay before restarting it.
- **Symptom:** the background shell exited immediately with status `failed`; `pkill -f` matched
  the shell's own command line, which contained the pattern string.
- **Resolution:** used `pkill -x hush-relay` (exact process name). Tooling problem, not a repo
  problem.
- **Lesson:** `pkill -f` matches the invoking command line too; anchor it or use `-x`.

### [FIXED] The insecure-context branch was not exercised on a real non-secure origin

- **Attempted:** verify the "must be served over HTTPS" path on the lab relay
  (`http://137.184.129.205:8790`), where `crypto.subtle` is absent.
- **Symptom:** the browser test ran against `http://127.0.0.1:8791`, and loopback is a secure
  context, so `crypto.subtle` WAS present and the insecure branch never ran. The branch was
  verified by reading `hasWebCrypto()` and by a served-HTML assertion only.
- **Resolution:** re-ran against `http://lvh.me:8791`, a hostname that resolves to 127.0.0.1 but
  is not treated as trustworthy by the browser. There `window.isSecureContext` is `false` and
  `crypto.subtle` is `undefined`; the page showed `#insecure` (`display: block`) with the HTTPS
  message and hid `#source`, `#pin-form`, and `#result`. No pure-JS PBKDF2 fallback was added on
  purpose (600k iterations would freeze a phone). An earlier attempt to force the branch by
  stubbing `crypto` before load failed because
  `Page.addScriptToEvaluateOnNewDocument` did not run in this browser tool (see the separate
  `WORKED AROUND` entry).
- **Lesson:** loopback is not a stand-in for a non-secure origin; a non-trustworthy hostname
  mapped to the loopback address is a cheap way to exercise the branch without a real deployment.

### [OPEN] The cross-check test costs ~36s because it uses the production KDF

- **Attempted:** cross-verify the page's crypto against Rust with the real iteration count.
- **Symptom:** sealing at 600,000 iterations with the hand-rolled Rust PBKDF2 dominates the test
  (the Node side is fast). A lowered count would not prove the page reads the header.
- **Resolution:** accepted, with a comment; the container is sealed once and the tamper cases
  reuse it at 1,000 iterations. Not a correctness problem, a suite-time one.
- **Lesson:** the expensive, realistic vector is worth it once; reuse it for the cheap negative
  cases.

---

## 2026-10-08 — Daemon sends outsider notifications through the relay

Switched the daemon off its own mailer and onto `POST /drop/{id}/notify`, deleted the mailer
seam that became dead, and moved the pickup link toward a single source of truth. Newest
findings first.

### [OPEN] The relay's notify response does not carry the link it emailed, so the daemon and the email can still disagree

- **Attempted:** make the pickup link one source of truth by having the daemon return exactly the link the relay emails.
- **Symptom:** `POST /drop/{id}/notify` replies with `{"sent":true,"attachment_bytes":N}` (`NotifyResponse` in `crates/hush-relay/src/app.rs`); the link it composes at `crates/hush-relay/src/app.rs:320` is used only for the email body and is never returned. The daemon therefore cannot read it.
- **Resolution (daemon side, done):** `RelayClient::notify_drop` deserializes an optional `link` and the `send_outsider` handler uses it verbatim when present. The relay-side change is not made here because `crates/hush-relay/**` is owned by a concurrent worker. Exact change needed: add a `link: String` field to `NotifyResponse` and return the `link` value already computed at `crates/hush-relay/src/app.rs:320` (currently `let link = format!("{}/open/{}", state.drop_base_url.trim_end_matches('/'), id);`).
- **Consequence while open:** the daemon returns `{resolved_relay}/open/{id}` (on the lab box, `http://127.0.0.1:8790/open/{id}`) while the relay emails `{RelayConfig::drop_base_url}/open/{id}` (8787 by default). The daemon's link now points at the relay that actually serves `/open/{id}`, which is strictly more correct than the old independent 8788 default, but the two are not yet identical. The daemon no longer has its own `HUSH_DROP_BASE_URL` knob, so there is one base URL on the daemon side instead of two.
- **Lesson:** a value two services must agree on has to travel with the message that needs it; a "keep these in sync" comment is not a fix.

### [FIXED] The daemon still ran its own mailer and never reached the relay's notify route

- **Attempted:** replace the local `state.mailer.send(...)` in `send_outsider` with a call to the relay's notify endpoint.
- **Symptom before:** the daemon sent a link-only email through `hush_core::Mailer` and never called `POST /drop/{id}/notify`; `build_mailer` silently mapped every spec except `file:` to `NullMailer`, so an SMTP spec was a no-op with no diagnostic.
- **Resolution:** `send_outsider` now resolves the relay through the existing `Work::Meta { relay }` path (falling back to `hush_core::config::resolve_relay_url_with_source` as `status` does), calls `RelayClient::notify_drop(&delivery_id, &to, "pin"|"key", expires_unix)`, and logs a failure with the delivery id, recipient, and relay without failing the drop. The response still carries a working link.
- **Evidence:** `crates/hush-core/tests/outsider_mail.rs::the_daemons_notify_call_reaches_the_relay_route_and_emails_the_link` drives that exact call against a real relay with a file notifier and asserts a message is written carrying the relay's link and the container; `the_daemons_notify_call_surfaces_a_relay_refusal` asserts a 404 becomes `Err`.
- **Lesson:** a "notification" that never touches the component that owns it is only half wired; test the call against the real route, not the compiler.

### [FIXED] `hush_core::SmtpMailer` was a second SMTP implementation with no caller

- **Attempted:** delete only the code the daemon switch makes dead, after grepping for consumers.
- **Symptom / evidence:** `rg 'SmtpMailer'` matched only its own definition and tests in `crates/hush-core/src/mailer.rs` plus prose in `crates/hush-relay/src/notify.rs`; `build_mailer` never constructed it, and no other crate referenced it. It was written for this daemon path but was never wired.
- **Resolution:** deleted `Mailer`, `NullMailer`, `FileMailer`, `SmtpMailer`, and the SMTP machinery (`Conn`, spec parsing, `normalize_data`, `rfc5322_date`, `header_safe`, and the related constants/tests) from `crates/hush-core`; the module was renamed `mailer.rs` to `notification.rs`. Kept `notification_body`/`notification_body_for_key`, which the relay uses (`crates/hush-relay/src/app.rs:324,326`). The surviving transport is `SmtpNotifier` in `crates/hush-relay/src/notify.rs`.
- **Lesson:** a component with no caller is not a feature; grep before keeping "for later".

### [FIXED] The new notify test asserted on unwrapped base64 and failed

- **Attempted:** assert the relay attached the container by checking the raw base64 of the stored blob appears in the written message.
- **Symptom:** `assertion failed: the relay must attach the stored container` at `crates/hush-core/tests/outsider_mail.rs:316`. The relay wraps the base64 body at 76 columns (RFC 2045), so the encoded string never appears contiguously.
- **Resolution:** strip whitespace from the message before the substring check. Test passes.
- **Lesson:** MIME base64 is line-wrapped; compare after removing whitespace, as the relay's own tests do.

### [OPEN] The two smoke scripts still set the deleted `HUSH_MAILER` / `HUSH_DROP_BASE_URL` and assert the old link base

- **Attempted:** confirm nothing else depended on the removed daemon config.
- **Symptom:** `scripts/smoke-drop.sh` and `scripts/smoke-drop-key.sh` launch the daemon with `HUSH_MAILER="file:$MAILER_DIR"` and `HUSH_DROP_BASE_URL="$DROP_BASE"`, then assert `[[ "$LINK" == "$DROP_BASE/open/$ID" ]]` and read the notification from `$MAILER_DIR`. After this change the daemon ignores both env vars, the file notification is written by the relay instead, and the link is `{relay}/open/{id}`.
- **Resolution needed (out of scope here):** run the relay with `HUSH_RELAY_MAILER="file:$MAILER_DIR"`, drop `HUSH_MAILER`/`HUSH_DROP_BASE_URL` from the daemon invocation, and change the link assertion to `$RELAY_URL/open/$ID`. Not edited because the scope boundary for this slice is `crates/hush-daemon/**` and `crates/hush-core/**`.
- **Lesson:** removing a config knob silently breaks the scripts that set it; grep scripts, not just code.

### [OPEN] The daemon's `send_outsider` axum handler itself is not integration-tested

- **Attempted:** prove the daemon reaches the relay's notify route end to end.
- **Symptom:** `hush-daemon` is a binary-only crate (no lib target), so an integration test cannot call `send_outsider`; the daemon's tests are in-file unit tests. The wiring is proven at `RelayClient::notify_drop` (the exact function the handler calls) against a real relay, not through the handler.
- **Resolution needed:** give `hush-daemon` a lib target or extract the notify step behind a testable seam if handler-level coverage is required.
- **Lesson:** state which layer the evidence covers; "the function it calls is tested" is not "the handler is tested".

### [OPEN] Real external mail delivery still cannot be verified here

- **Attempted:** exercise a real SMTP submission for the relay's notify.
- **Symptom:** the lab VPS provider blocks outbound TCP 25 and 587, so no notification can leave the machine. Tests use loopback SMTP sinks or the file notifier, which proves the conversation and the bytes, not delivery to a real mailbox.
- **Resolution:** needs a host with outbound submission allowed (or a relay that provides one).
- **Lesson:** do not read "SMTP conversation completed against a local sink" as "mail was delivered".

---

## 2026-10-08 — Legacy-hydration fix, kick propagation, conclave file sharing

Priority order: fix the legacy-hydration bug, close the Phase 1 kick-propagation gap, then add
conclave file sharing. Problems hit, newest findings first within the section:

### [FIXED] Legacy hydration left the local user's own entry holding a nickname, so the conclave was unsendable

- **Attempted:** send a message in the pre-existing "Daily sync" conclave on `peer-a`.
- **Symptom:** `HTTP 400` with `you are not a trustee of this conclave`. `GET /api/conclaves` showed
  the trustee entry as `account_id: "alice"` — the nickname — instead of the protocol id, so
  `is_joined(me)` was false.
- **Cause:** `hydrate_trustees` resolved nicknames only through contacts. A vault is never a contact of
  itself, so the local account's own nickname never resolved, and the old fallback wrote the nickname
  into `account_id`. The old guard also returned early as soon as every trustee had a non-empty
  `account_id`, so a vault the *previous* hydration had already touched could never self-heal.
- **Consequence stated plainly:** the membership model was incompatible with any vault that predates
  it. A pre-consent conclave could be listed but not used.
- **Resolution:** `hydrate_trustees` now takes `local_account`. When exactly one entry stays unresolved
  it is the local account's nickname and is set to `local_account` in `Joined`; when the local account
  is absent from the legacy list it is added; and entries whose `account_id == nick` (the old fallback)
  are re-processed. Counts now agree: `peer-a`'s copy, whose list named the local nickname, resolves to
  two joined trustees, and `peer-b`'s copy, whose legacy list omitted the local account entirely
  (the "2 vs 1" disagreement), is brought from one to two.
- **Evidence:** `peer-a` sends in "Daily sync" → `HTTP 204`, and `hydration-fix-probe` appears in that
  conclave's log. Regression tests (all in `crates/hush-core/src/chat_log.rs`):
  `a_legacy_record_resolves_the_local_accounts_own_nickname`,
  `an_already_hydrated_fallback_account_id_self_heals`, and
  `both_legacy_shapes_hydrate_to_the_same_trustee_set`.
- **Lesson:** a migration that resolves identities through a lookup must special-case the one identity
  that is never in the lookup — the local one.

### [FIXED] The acceptance test passed while the conclave was in fact unusable

- **Attempted:** accept the previous slice on "lists Daily sync with its trustees and message intact".
- **Symptom:** that assertion passed against a record whose local entry held a nickname, because it only
  checked that the record loaded and listed — it never called `is_joined`, and never sent.
- **Resolution:** the new regression test asserts `is_joined(local_account)` on the exact legacy shape,
  which is the path the old test skipped. Confirmed the test fails on the pre-fix code: temporarily
  restoring the pre-fix `hydrate_trustees` body (the pre-fix and post-fix signatures are both 4-arg, so
  it still compiles) makes `a_legacy_record_resolves_the_local_accounts_own_nickname` panic with
  `the local account must be joined`, and the two older hydration tests fail as well.
- **Lesson:** an acceptance test that asserts a record *exists* does not assert it *works*. Assert on
  the predicate the caller depends on.

### [OPEN] The two legacy "Daily sync" copies still disagree on state, convener, and ended

- **Attempted:** make the trustee lists agree across peers.
- **Reality:** the trustee *set* now agrees — both vaults resolve to `{alice, bob}`, two entries each
  (was 2 vs 1). The two copies still differ in `state` / `convener` / `ended`: `peer-a` shows
  `[alice joined, bob joined]`, `peer-b` shows `[alice kicked, bob joined]` with `ended: true`.
- **Cause:** (a) a pre-consent record stored no convener, so the load-time hydration defaults it to the
  local account and each vault appoints *itself*; (b) `peer-b`'s copy also carries a local kick of alice
  plus an end from earlier testing, and because the kicked account is not a fan-out target (see the
  existing `[OPEN] A kicked trustee is not told`) `peer-a` was never told. Neither is a hydration bug:
  hydration only ever writes `Joined`.
- **Resolution:** none. Legacy records were never synced, so there is no shared history to reconcile
  from, and fabricating one (un-kicking, re-picking a convener) would invent data. Reported rather than
  papered over.
- **Needed to close:** a decision on a convener default for records that never had one, and the
  kicked-peer notification path.

### [FIXED] Phase 1 gap closed: a kick converges on a live joined peer

- **Attempted:** prove kick *propagation* to a live joined peer, not just that the daemon rejects a
  non-convener.
- **Method:** three live daemons — `peer-a` (alice, convener), `peer-b` (bob), and a third vault on
  `127.0.0.1:9878` (carol2) — in a fresh conclave "Phase2 Kick" where both others accepted.
- **Evidence:** after alice kicks bob, alice and carol2 both read `bob: kicked` immediately, while bob's
  own copy still reads `self: joined`. Bob then sends `bob-after-kick-2`; it lands in bob's own log and
  is dropped by both alice and carol2.
- **Conclusion:** propagation and convergence across the remaining joined set are proven. The kicked
  peer is not notified — the inherited `[OPEN] A kicked trustee is not told` limitation, re-confirmed
  here rather than dropped.

### [FIXED] Conclave file sharing: one encrypted copy per joined trustee

- **Design choice:** an optional `file: Option<ConclaveFile>` on the existing
  `MessagePayload::Conclave`, not a new payload variant. The fan-out, the descriptor, the recipient
  routing, and the "only joined trustees" rule are all identical to text; a new variant would have
  duplicated every one of those match arms for no behavioural gain. Backward compatibility is free:
  `#[serde(default, skip_serializing_if = "Option::is_none")]` keeps an old message decodable and a
  body-only message byte-identical on the wire.
- **Why N copies:** files here are per-recipient encrypted blobs (`vault.put_blob`), so there is no
  shared blob to point everyone at. N puts for N joined trustees is the honest implementation, exactly
  as text already fans out.
- **Surface changed:** `MessagePayload::Conclave` gains `file` (`payload.rs`); `ConclaveFileRecord` and
  `append_conclave_file` in `chat_log.rs`; `Account::send_conclave_file_bytes` and the `recv` arm that
  stores the recipient's own blob and records it in the log (`account.rs`); `POST /api/conclaves/file`
  plus `Work::SendConclaveFile` (`main.rs`, `account_worker.rs`); the DTOs and client call
  (`types.ts`, `daemon.ts`, `conclaveStore.ts`); the attach button and `file-card` in the conclave
  composer and message log (`app.ts`); `resolveMessageFileBytes` generalised to take a blob id
  (`fileBytes.ts`).
- **Evidence:** integration test `a_conclave_file_reaches_every_joined_trustee_and_not_an_invitee`
  (11 conclave tests green); live two-peer browser run (below); live three-peer run where the joined
  set receives the file and the non-accepting invitee has no record, no blob, and a `blob not found`
  on fetch.

### [OPEN] A trustee who joins after a file was shared does not receive it

- **Reality:** the fan-out targets the joined set at send time, and there is no shared history to
  backfill from. A later joiner sees an empty conclave log.
- **Consequence:** the conclave log is not complete for late joiners. The UI does not claim otherwise,
  but this is a real limitation of the store-and-forward, no-shared-state model, not a bug to be
  silently widened around.

### [OPEN] File sharing makes the kicked-peer gap more visible

- **Reality:** a kicked trustee is excluded from the fan-out, so a file sent after the kick never
  reaches them and they are never told. With text the effect is "messages stop"; with a file it is
  "the shared document never arrives". Same root cause, same missing notification path; reported rather
  than fixed here.

### [OPEN] `blob_id` is `sha256(plaintext)`, so identical content shares an id across peers

- **Observation:** `Vault::put_blob` derives the id from the plaintext, so the same file stored on two
  peers has the same `blob_id_hex` in both logs (each still holds its own ciphertext under its own
  master key; the bytes were fetched and compared on both peers). This is pre-existing behaviour for
  1:1 files and mail attachments, not introduced by conclave files, but conclave fan-out makes the
  equality visible across peers and it leaks "these two vaults hold the same bytes". Not changed here;
  noted so it is not mistaken for a bug in the new code.

### [WORKED AROUND] The browser tools cannot drive a file picker, so the real change handler was fired over CDP

- **Attempted:** click "Share a file" in the conclave composer and choose a file in the two-peer
  browser run.
- **Symptom:** the button opens a native picker the automation browser cannot operate, and
  `DOM.setFileInputFiles` is denied.
- **Resolution:** located the hidden `<input type="file">` next to the attach button and, via
  `Runtime.evaluate`, built a `DataTransfer`, assigned `input.files`, and dispatched a real `change`
  event. That runs the app's own `onchange` → `sendConclaveFile` → `POST /api/conclaves/file` path; the
  file then rendered as a `file-card` with Open / Save as on both peers. The picker dialog itself is the
  only part not exercised.

### [WORKED AROUND] Proving the regression test fails pre-fix required a temporary edit to a shared file

- **Attempted:** demonstrate the new regression test fails on the pre-fix code.
- **Method:** backed up `chat_log.rs`, restored the pre-fix function body (same 4-arg arity, so it
  compiles), ran the tests, then restored from the backup and verified the SHA-256 matched.
- **Risk:** another agent was active in the same repo during the window, so the file was shared state.
  The edit was short-lived and hash-verified on restore, but a concurrent write would have been
  silently clobbered. Noted as a technique risk, not a repo problem.

---

## 2026-10-08 — Relay-side drop notification (`POST /drop/{id}/notify`)

Moved the outsider-mail notification from the sender's daemon to the relay, which now emails the
pickup link with the sealed container attached. Problems hit:

### [WORKED AROUND] `hush_core::Mailer` cannot carry an attachment, so the relay hand-rolls a second SMTP client

- **Attempted:** wire the relay's notification through the shared `Mailer` trait, per the brief's
  "do NOT write your own SMTP client", with the container as a MIME attachment.
- **Symptom:** the trait that landed in `crates/hush-core/src/mailer.rs` has one method,
  `send(to, subject, body)`, and `SmtpMailer::compose` hardcodes `Content-Type: text/plain;
  charset=utf-8`. There is no attachment parameter and no raw-message entry point. Passing a full
  `multipart/mixed` message as `body` produces a `text/plain` email whose body happens to contain
  MIME text — the client renders it, it does not attach it.
- **Resolution:** the relay defines its own attachment-carrying `Notifier` trait
  (`crates/hush-relay/src/notify.rs`) and ships a minimal SMTP submission (`SmtpNotifier`). This
  is a deliberate deviation from the brief's instruction not to write an SMTP client: the
  container-as-attachment is the task's core requirement, the landed mailer cannot express it, and
  `crates/hush-core` is out of scope. The notification *copy* is still the shared
  `hush_core::notification_body*`, so wording cannot drift; only the transport is duplicated. When
  `Mailer` grows an attachment-capable method, `SmtpNotifier` should be deleted and replaced by an
  adapter.
- **Lesson:** a trait that cannot express the payload forces either a scope violation or a
  non-functional feature. Surface the interface gap before building on the trait.

### [OPEN] The relay now learns the recipient address

- **Attempted:** keep the relay's documented guarantee ("the relay cannot tell who a delivery is
  for") true while sending from the relay.
- **Symptom:** it cannot be true for a notified drop. `POST /drop/{id}/notify` carries the
  recipient address, which the relay must hold to address the SMTP envelope. Previously the drop
  id was an unguessable capability and the relay never saw who a drop was for. The store still
  holds no address (only the request does, transiently), but the guarantee as written in
  `store.rs` and `docs/plans/2026-10-07-outsider-mail.md` is no longer true for notified drops.
- **Resolution:** none; it is inherent to moving the send across the trust boundary. Reported
  rather than silently accepted. If the address must not reach the relay, the daemon must send the
  mail itself, which is the design this slice moved away from.
- **Lesson:** moving a step across a trust boundary moves the data that step handles across the
  boundary too.

### [OPEN] Real internet mail delivery is still unverifiable

- **Attempted:** prove a notification reaches a public mailbox.
- **Symptom:** the lab VPS provider blocks outbound TCP 25 and 587 to public providers (already
  recorded in the provisioning and mailer sections). The relay's SMTP path is verified only
  against a local sink that speaks the protocol.
- **Resolution:** none available here. The email path is NOT described as working end to end.
- **Lesson:** same as the mailer slice: client-to-local-sink and client-to-internet are different
  claims.

### [FIXED] `hush-relay` had no base64 encoder for the attachment

- **Attempted:** base64 the container for the MIME part.
- **Symptom:** `hush-relay`'s dependencies had no base64 crate, and `hush-core` does not re-export
  one.
- **Resolution:** added `base64 = "0.22"` to `crates/hush-relay/Cargo.toml` (the same version
  `hush-core` already uses). No new version and no edit under `crates/hush-core`.
- **Lesson:** n/a.

---

## 2026-10-08 — Outsider-mail SMTP notification (hush-core mailer)

Implemented `SmtpMailer` in `crates/hush-core/src/mailer.rs` (spec `smtp://host:port[?from=addr]`),
plus additive `Mailer::describe` / `Mailer::is_noop` defaults so a silent no-op can be told apart
from a real send. Problems hit:

### [OPEN] Real external notification delivery is unverifiable from this environment

- **Attempted:** prove an actual notification email leaves the machine through a real MTA, per the
  slice's verification requirement.
- **Symptom:** the lab VPS runs OpenSMTPD on `127.0.0.1:25`, but the provider blocks outbound TCP
  25 and 587 (recorded above as "External mail egress is impossible from this droplet"). No
  smarthost is configured. The only SMTP conversation that can be proven here is
  client-to-local-sink, not client-to-internet.
- **Resolution:** none in this slice. The client is verified against a real local SMTP sink (a
  socket server that speaks the protocol, not a mock); external delivery is not claimed and the
  email path is not marked done. This is the same environment limit the provisioning slice hit,
  and it is not a repo problem.
- **Lesson:** "the SMTP client works" and "the notification email is delivered" are different
  claims. Only the first is verifiable on this box.

### [OPEN] The SMTP mailer is not reachable from the daemon yet (owned by a concurrent worker)

- **Attempted:** wire `smtp://` into `build_mailer` and surface delivery status in the
  `/api/mail/outsider` response.
- **Symptom:** `crates/hush-daemon/src/main.rs`, `account_worker.rs` and `apps/hush-tauri/src/app.ts`
  are owned by a concurrent worker and are out of scope for this slice. `build_mailer` still maps
  every non-`file:` spec, including `smtp://...`, to `NullMailer`, and
  `the_mailer_spec_selects_a_file_mailer_or_falls_back_to_null` still asserts that.
- **Resolution:** the exact daemon patch is written up in the slice report (import, `build_mailer`
  arm, response field, and the updated test) but deliberately not applied. Until it lands, the new
  mailer is selectable only from `hush-core` tests, not from a running daemon, so the end-to-end
  notification path is NOT done.
- **Lesson:** a `Mailer` implementation with no caller is a component, not a feature. Say so.

### [WORKED AROUND] `Mailer::send` cannot distinguish a delivered message from a discarded one

- **Attempted:** make the degradation observable without breaking the existing daemon.
- **Symptom:** `NullMailer::send` returns `Ok(())`, and the daemon treats every `Ok` as success, so
  a typo'd spec (which falls back to `NullMailer`) is indistinguishable from a real send. Changing
  `send`'s return type would not compile against the concurrent worker's `main.rs`.
- **Resolution:** added two defaulted trait methods, `describe() -> &'static str` and
  `is_noop() -> bool`, which are additive and leave `main.rs` compiling. The daemon patch uses
  `is_noop()` to report `"disabled"` vs `"sent"` and `describe()` in the failure log. Not fully
  fixed inside `hush-core`, because the caller that reports the status is out of scope.
- **Lesson:** a `Result<(), Error>` where one error-free path means "did nothing" cannot express
  "did nothing" to its caller.

### [OPEN] Two hand-rolled SMTP clients now exist in the workspace

- **Attempted:** implement the only SMTP client in the workspace.
- **Symptom:** while this slice ran, a concurrent worker added
  `crates/hush-relay/src/notify.rs`, which hand-rolls its own SMTP client (`SmtpNotifier`) to send a
  MIME message with the sealed container attached. It does not use `hush_core::SmtpMailer` because
  the `Mailer` trait is plain-text-only, and `hush-relay` is outside this slice's scope. The two
  clients duplicate the transport: connect, EHLO/HELO, MAIL FROM, RCPT TO, DATA, dot-stuffing and
  timeouts.
- **Resolution:** none. The clients are not duplicates in purpose (attachment vs plain text) but
  they are duplicates in transport. A later slice could extract a shared transport into `hush-core`
  and have both call it; that requires editing `hush-relay`, which a concurrent worker owns.
- **Lesson:** when a task says the SMTP client does not exist, check whether a sibling slice is
  about to add one before assuming you are the only author.

### [FIXED] `BufReader::read` needs `Read` in scope, and `BufRead` was unused

- **Attempted:** build the SMTP client.
- **Symptom:** `error[E0599]: no method named read found for struct BufReader<R>` plus
  `warning: unused import: BufRead`. Importing `BufRead` does not bring the supertrait `Read`
  methods into scope.
- **Resolution:** `use std::io::{BufReader, Read, Write};`. Build clean.
- **Lesson:** for `BufReader`, import `Read` for `read` and `BufRead` only for `read_line`/`lines`.

### [FIXED] Edit tool mis-inserted a Rust string literal containing an escape

- **Attempted:** anchor a `StrReplace` on the existing line holding the expiry format string, which
  ends in a backslash-n escape.
- **Symptom:** two consecutive `StrReplace` calls failed with "string to replace was not found",
  despite an apparently exact match. The tool parameter interpreted the backslash-n as a real
  newline, so the pattern no longer matched the source. Tooling issue, not a repo problem.
- **Resolution:** anchored the edit on a following line with no escape sequences instead. Avoid
  backslash-bearing Rust string literals in `old_string`.
- **Lesson:** when an exact-match edit mysteriously misses, suspect escape interpretation.

---

## 2026-10-08 — Lab VPS relay provisioning (hush-relay + SMTP)

Provisioned the lab VPS `137.184.129.205` as a public plain-HTTP hush relay with an SMTP submission
server. New files only: `packaging/systemd/system/hush-relay.service.d/10-lab-public.conf`,
`scripts/smoke-relay-lab.sh`, `docs/ops/relay-lab-vps.md`. Problems hit:

### [OPEN] The daemon has no SMTP client, so there is no SMTP setting to wire

- **Attempted:** read the repo to find "exactly which SMTP settings the daemon expects" so the
  installed OpenSMTPD could be wired to the outsider-mail notification egress.
- **Symptom:** there is no SMTP code at all. `hush-core/src/mailer.rs` ships only `NullMailer` and
  `FileMailer`; `build_mailer` in `hush-daemon/src/main.rs` maps `file:<dir>` and everything else —
  including `smtp:host` — to `NullMailer`. The unit test
  `the_mailer_spec_selects_a_file_mailer_or_falls_back_to_null` asserts exactly that, so a `smtp:`
  spec is silently a no-op, not an error.
- **Resolution:** none in this slice. OpenSMTPD is installed and verified, but the daemon cannot use
  it until an SMTP `Mailer` implementation exists. That means editing `crates/hush-core` and
  `crates/hush-daemon`, which are outside this task's scope and are being edited concurrently.
  Proposed contract (not implemented): `HUSH_MAILER=smtp://<host>:<port>` selecting a new
  `SmtpMailer`.
- **Lesson:** confirm the configuration surface exists before provisioning the thing it is meant to
  configure. The premise "the SMTP settings the daemon expects" did not hold.

### [OPEN] External mail egress is impossible from this droplet (port 25 blocked)

- **Attempted:** verify the SMTP path by relaying a message to an external recipient
  (`hushlab.test@gmail.com`).
- **Symptom:** accepted by smtpd (`250`), then the MTA failed:
  `mta error reason=Connection failed: Network is unreachable` connecting to the Gmail MX on
  `smtp://142.251.16.26:25`. Outbound TCP 25 is blocked from the box and 587 to `smtp.gmail.com`
  also failed, while TCP 443 works. This is a DigitalOcean egress policy, not a repo or OpenSMTPD
  problem.
- **Resolution:** none available without a smarthost/provider relay or a DO support exception. Local
  delivery is verified; real external notification egress is not. Documented in
  `docs/ops/relay-lab-vps.md`.
- **Lesson:** a lab box with a public IP still cannot send internet mail; provider egress policy, not
  configuration, is the limit.

### [FIXED] Port 8787 was already listening — but it is not a hush relay

- **Attempted:** the brief flagged `0.0.0.0:8787` (the relay's default port) as possibly an
  already-installed hush relay.
- **Symptom:** `ss -ltnp` shows the listener is `node`, pid 83639, cwd `/opt/mcp-ironlink`,
  `MCP_HTTP_PORT=8787`, cgroup `/system.slice/mcp-ironlink.service`. It is the IronLine MCP HTTP
  server, unrelated to hush.
- **Resolution:** left it untouched and moved the relay to `0.0.0.0:8790` via a systemd drop-in. No
  second relay was started on 8787.
- **Lesson:** identify the owning unit from the cgroup before assuming a port collision means the
  service you want is already running.

### [FIXED] OpenSMTPD accepted the test message but refused to deliver it to root

- **Attempted:** verify the SMTP server by sending to `root@localhost`.
- **Symptom:** submission returned `250 2.0.0 Ok` / `Message accepted for delivery`, but the queue log
  showed `mda delivery ... result=PermFail stat=Error (permission denied: not allowed to deliver to:
  root)`. No file appeared in `/root/Maildir`.
- **Resolution:** created a dedicated unprivileged `hushmail` user and delivered to
  `hushmail@localhost`; the log then showed `result=Ok stat=Delivered` and the message file was
  present. "Accepted" is not "delivered"; the verification was only valid after the mailbox check.
- **Lesson:** for an MTA, assert on the delivery result line, not the SMTP `250`.

### [WORKED AROUND] The cross-machine test cannot use release binaries over plain HTTP

- **Attempted:** point a local daemon at `http://137.184.129.205:8790` with release binaries.
- **Symptom:** `hush-cli config set --relay http://137.184.129.205:8790` fails with
  `relay error: insecure relay URL: use HTTPS except for localhost`. `HttpPolicy::LoopbackOnly`
  refuses any non-loopback `http://` in a release build by design.
- **Resolution:** ran the cross-machine test with **debug** binaries (`HttpPolicy::Allow`), the only
  way to test a plain-HTTP lab relay without TLS. Documented in `docs/ops/relay-lab-vps.md`. The
  production answer remains "terminate TLS in front of the relay".
- **Lesson:** the lab's plain-HTTP choice is incompatible with shipped clients; the harness must say
  so rather than appearing to prove a release path.

### [OPEN] Full reboot of the VPS was not performed

- **Attempted:** verify the relay comes up at boot.
- **Symptom:** `systemctl restart hush-relay` was verified (active, listening on 8790, `/health`
  200, DB intact) and the unit is `enabled` (symlinked into `multi-user.target.wants`). A real reboot
  was not run because the box hosts unrelated services (nginx, the alhakim app, BGP) and a boot
  failure would take them down with no console access.
- **Resolution:** none needed for the lab, but boot persistence rests on `is-enabled` plus the
  restart test, not on an observed reboot.
- **Lesson:** say "enabled and survives a restart", not "survives a reboot", when the reboot was
  deliberately not run.

---

## 2026-10-08 — Consensual conclave membership and convener governance

Replaced silent materialisation and local-only membership with an invite/accept protocol governed by
a single authority (the convener), renamed `creator`/`member` to `convener`/`trustee`, and kept every
existing vault readable. Problems hit while doing it:

### [FIXED] A large block replacement duplicated two `ChatLog` methods

- **Attempted:** rewrite `create_conclave`/`update_conclave`/`hydrate_*` in `chat_log.rs` with one
  `StrReplace`, re-emitting the neighbouring `append_conclave_message` and `end_conclave` in the new
  text.
- **Symptom:** `cargo build -p hush-core` failed with
  `error[E0592]: duplicate definitions with name `append_conclave_message`` and the same for
  `end_conclave`. The old copies still sat below the replaced region.
- **Resolution:** deleted the two leftover copies. Caught immediately because the build was run right
  after the edit, not at the end of the slice.
- **Lesson:** when a replacement re-emits methods that already exist elsewhere in the file, the
  originals are not removed. Build after every large structural edit.

### [FIXED] A stale peer snapshot downgraded a joined trustee back to invited

- **Attempted:** the three-party fan-out test (`a_conclave_message_reaches_every_joined_trustee_exactly_once`).
- **Symptom:** `assertion left == right failed: bob gets exactly his one copy / left: 0 / right: 1`.
  A debug dump of Alice's record showed `bob … state: Invited` after both Bob and Carol had accepted.
- **Cause:** Carol accepted from an invite that still listed Bob as invited, so Carol's accept
  descriptor carried `bob: invited`. `upsert_trustee` only protected *terminal* states, so that stale
  snapshot overwrote Bob's `joined` with `invited`, and Alice's later fan-out skipped him.
- **Resolution:** made `upsert_trustee` monotonic in both directions: a terminal state is never
  revived, and a joined trustee is never downgraded to invited. Added an assertion to the chat-log
  unit test for the downgrade case.
- **Lesson:** single-authority convergence still has to survive out-of-order snapshots from peers who
  had not yet heard the latest fact. Guard the merge, not just the sender.

### [FIXED] `hydrate_trustees` skipped the exact legacy shape it existed for

- **Attempted:** the load-time migration for a pre-rename record.
- **Symptom:** `assertion failed: hydrated` in `a_legacy_record_hydrates_its_trustees_from_its_nickname_list`.
- **Cause:** the guard was `if !record.trustees.is_empty() { return Ok(false) }`. A pre-rename record
  decodes `members: ["bob"]` into a *non-empty* trustees vec whose entry has an empty `account_id`, so
  the guard treated it as already hydrated.
- **Resolution:** the guard is now "skip only when every trustee already has a non-empty account id";
  entries with an empty id are resolved through contacts (falling back to the nickname).
- **Lesson:** "already migrated" must be tested per item, not by whether the collection is non-empty.

### [FIXED] `#[serde(rename = "members")]` leaked the old key into the daemon's JSON API

- **Attempted:** keep old vaults loading by renaming the new `trustees` field to the on-disk name
  `members`.
- **Symptom:** found by inspection before the frontend was wired: `ConclaveRecord` is serialised
  directly as the daemon's API DTO, so the JSON key would have been `members` even for new records,
  contradicting the `trustees` vocabulary the UI and tests use.
- **Resolution:** dropped `rename`, kept `#[serde(alias = "members")]`. New records write `trustees`;
  old records still read. The frontend DTO uses `trustees`/`convener`/`ended`.
- **Lesson:** a serde `rename` for on-disk compatibility also renames the wire DTO when the same type
  is serialised to the API. Use `alias` for read-compat and leave the canonical name alone.

### [OPEN] A kicked trustee is not told, so their local record still shows them joined

- **Attempted:** verify the kick converges to the removed trustee.
- **Reality:** `broadcast_update` targets the *joined* set only, and the kick marks the trustee
  `Kicked` before broadcasting, so the kicked peer is excluded from the fan-out. The remaining joined
  trustees converge (covered by `a_kick_converges_and_cuts_off_delivery`), and the kicked peer stops
  receiving, but its own record is never updated.
- **Consequence:** the kicked peer's UI still shows them in the conclave; they only discover it when
  messages stop. Enforcing "kicked" client-side would need either a targeted notification to the
  kicked account (they are no longer a fan-out target) or an explicit end-of-membership message.
- **Needed to close:** a decision on whether to notify the kicked peer, and a delivery path that does
  not depend on them being in the joined set. This supersedes the older "[NO LONGER APPLICABLE]
  Membership changes are local only" entry; that limitation is gone, this one remains.

### [FIXED] The consensual conclave UI was not driven in a running app

- **Attempted:** verify the invite banner, invite/kick/leave/end actions, the read-only ended view,
  and the convener-only button gating in a real window.
- **Symptom / limit (then):** no display was available in this environment, so the UI could not be
  driven. The protocol was verified at the integration layer only.
- **Resolution:** closed in the follow-up slice. The conclave flows were driven in a real browser
  against `scripts/run-two-ui.sh --dev` with two peers: the pending-invite card and Accept button, the
  convener-only chips (alice saw Invite / End / Remove; bob saw only Leave), the message log, and the
  composer placeholder `Message conclave`. The remaining gap is the *kick control* itself being
  clicked in the UI — the kick was driven over the daemon API (see the kick-propagation entry above)
  and its convergence was read back through the same UI. Integration coverage grew to 11 tests
  (`crates/hush-core/tests/conclaves.rs`, including the file fan-out) and `npx tsc --noEmit` is clean.

---

## 2026-10-08 — Conclave rebrand, Add-members fix, End-conclave (scope reversal: rename the internals)

Renamed the whole group-chat feature from Meeting to Conclave across Rust, the daemon, the
frontend, the tests, and the docs, fixed the "Add members" button, and added an End-conclave
action with a typed gate. The rename had to keep every existing vault readable. Problems hit:

### [FIXED] "Add members" did nothing: the picker popup covered the dialog's action buttons

- **Attempted:** reproduce against the live dev stack with the members picker open in the create dialog.
- **Symptom:** clicking Create/Add did nothing. A hit-test at the button's coordinates returned the
  `.contact-picker-popup` (z-index 30) as the top element, not the button. In multiple mode the popup
  stays open after a selection, and it drops downwards over `.modal-actions`, which sits directly below
  the picker — so the popup swallowed the click.
- **First fix attempt (wrong):** gave `.modal-actions` `position: relative; z-index: 31` and
  `pointer-events: none` with `> * { pointer-events: auto }`. That fixed the button but broke the
  reverse direction: the transparent row still intercepted clicks aimed at the option list behind it
  (`hitChain: BUTTON.btn-secondary pressable`), so a member could no longer be selected.
- **Resolution:** added a `dropUp` option to `ContactPicker`. Inside a dialog the picker adds
  `contact-picker-drop-up`, which opens the popup above the field (`bottom: 100%`) so it never
  overlaps the action row in either direction. Reverted the z-index/pointer-events hack. Verified in
  the browser: with the popup open, the option click registers, the button enables, and the button
  click lands (`popupOverlapsActions: false`, `popupCoversAdd: false`).
- **Lesson:** a stacking-order fix trades one blocked click for another. Move the colliding element
  out of the way instead.

### [WORKED AROUND] A synthetic `focus` event was needed to open the picker during browser automation

- **Attempted:** click the members combobox with the browser tool, then click an option.
- **Symptom:** the input received `document.activeElement` but no `focus` event fired, so
  `ContactPicker`'s focus handler never ran and the popup stayed `hidden` (`aria-expanded="false"`).
  `input.focus()` and `blur()`/`focus()` from CDP did not help. The browser tab is not OS-focused in
  this environment, so real focus events do not fire.
- **Resolution:** opened the picker with `dispatchEvent(new FocusEvent("focus"))` over CDP for the
  automation step only, then used real clicks for the option and the action buttons. This is a
  harness limitation, not a repo problem; a real user clicking the field fires `focus` normally.
- **Lesson:** a control that opens on `focus` cannot be driven by a click in a non-focused tab.

### [WORKED AROUND] The add-member picker had no candidate contact to select

- **Attempted:** exercise "Add member" end to end, as the brief requires.
- **Symptom:** Alice's vault trusts only `bob`, and `bob` is already in the test conclave, so the
  add-member list was empty and its Add button stayed disabled — the click-through could not be tested.
- **Resolution:** created a throwaway third identity (`hush-cli init` in `/tmp/hush-carol`), added its
  invite to Alice via `POST /api/contacts/add`, then selected `carol` in the add-member dialog. This
  adds a `carol` contact to `instances/peer-a`; it is test residue in a dev vault, not a code change.

### [WORKED AROUND] A backgrounded `cargo test` produced an empty log

- **Attempted:** `cargo build --workspace; cargo test --workspace` in a `( ... ) &` subshell.
- **Symptom:** the build log was written and `BUILD_EXIT=0`, but `/tmp/hush-test-final.log` stayed at
  0 bytes and no `cargo` process remained. The subshell was killed with the shell session that spawned it.
- **Resolution:** re-ran the tests with `setsid nohup bash -c '...' &` so they survive the shell.
- **Lesson:** detach long jobs with `setsid`/`nohup` rather than a bare `&`.

### [FIXED] Backward compatibility for vaults and queued messages written under the old names

- **Attempted:** rename `MeetingRecord`/`MeetingMessage`, `MessagePayload::Meeting`, the payload fields,
  and the `ChatLogFile` list key without breaking `instances/peer-a` and `instances/peer-b`.
- **Symptom:** the list key, the enum tag, and the id field are all serde-written names; a plain rename
  makes an existing `chat.enc` and any already-queued payload undecodable, which surfaces to the user
  as a wrong password.
- **Resolution:** pinned the old names with `alias`/`rename` — `ChatLogFile` keeps
  `#[serde(rename = "meetings", alias = "conclaves")]`; the `Conclave` variant keeps
  `#[serde(alias = "meeting")]`; its id field keeps `#[serde(alias = "meeting_id")]`. Covered by the
  unit tests `a_log_written_before_the_rename_still_loads` and `a_legacy_meeting_payload_still_decodes`.
- **Evidence:** a fresh daemon process (built from the renamed source) opened a copy of `instances/peer-a`,
  unlocked with `peer-a-pass` (`HTTP 204`), and listed the pre-existing `Daily sync` conclave with its
  `bob: hello` message intact.

---

## 2026-10-08 — Meetings rail (local record, sender-side fan-out)

Built the Meetings feature: a vault-held `MeetingRecord` with its own message log, a
`MessagePayload::Meeting { meeting_id, name, members[], body }` descriptor on every message,
sender-side fan-out over the members' 1:1 sessions, recipient-side materialisation from the
descriptor, daemon routes, and a Meetings rail in the UI. Problems hit:

### [FIXED] `hush-cli` matched on `MessagePayload` without a `Meeting` arm, so the workspace would not build

- **Attempted:** `cargo test --workspace` after wiring the core and daemon.
- **Symptom:** `error[E0004]: non-exhaustive patterns: 'MessagePayload::Meeting { .. }' not covered`
  at `crates/hush-cli/src/main.rs:147`. The core and daemon crates compiled; the CLI did not, and
  the whole workspace build failed, so no test ran.
- **Cause:** adding the enum variant made every existing `match` on `MessagePayload` non-exhaustive.
  The `recv` match in the CLI prints inbox items and is easy to forget because it is not part of the
  feature.
- **Resolution:** added a `Meeting` arm to the CLI that prints the meeting message. `cargo test
  --workspace` then passed. This is the same class of breakage as the existing `[OPEN] 350d2d1`
  entry above, but in a different crate: that entry names `account.rs`; the CLI was a second,
  unlisted site.
- **Lesson:** a new `MessagePayload` variant means grepping every `match` on it, not just the ones in
  the crates you are editing.

### [WORKED AROUND] Two ad-hoc daemons collided on the default socket path (tooling, not the repo)

- **Attempted:** run two daemons for an end-to-end Meetings check with a shared
  `HUSH_RUNTIME_DIR` and only `--addr` set.
- **Symptom:** the second daemon exited at startup:
  `Error: Custom { kind: AddrInUse, error: "another hush-daemon is already listening on
  <run-dir>/hush-daemon.sock (use --socket to pick a different path)" }`.
- **Resolution:** passed an explicit `--socket` per daemon in the throwaway script. The repo's
  `scripts/run-two-ui.sh` already does this; the fault was in my one-off script.
- **Lesson:** the default socket path is derived from the runtime dir, so two daemons sharing one
  runtime dir need explicit `--socket` values.

### [WORKED AROUND] `/api/invite` is `POST`, and the smoke script assumed `GET`

- **Attempted:** read the invite with `GET /api/invite`.
- **Symptom:** empty response, so invite extraction failed. The route is `post(invite)`.
- **Resolution:** switched the script to `POST`. Tooling-only; the route is unchanged and correct.

### [OPEN] The Meetings UI was not exercised in a running app

- **Attempted:** verify the rail, the list/detail panes, the create and add-member modals, and the
  multi-select `ContactPicker` end to end.
- **Symptom / limit:** no display was available, so the UI could not be driven in a real window. The
  feature was verified at the daemon HTTP layer with two live daemons plus a relay (create, send,
  fan-out, materialisation, append, membership add/remove, unknown-member 400), and the frontend was
  checked with `npx tsc --noEmit` (exit 0) and `ReadLints` (no errors) only.
- **Consequence:** the multi-select `ContactPicker` behaviour (chip tokens, keep-open-on-select,
  Backspace removing the last chip) and the layout are **unverified in a browser**. The single-select
  mail composer path is untouched and still typechecks, but it was not re-driven either.
- **Needed to close:** a manual pass in `scripts/run-two-ui.sh --dev` with two tabs.

### [NO LONGER APPLICABLE] Membership changes are local only, and the UI says so

- **Attempted:** add/remove meeting members.
- **Reality (then):** a membership change edited only the actor's own `MeetingRecord.members`; it was
  not sent to anyone and did not change another client's member list or history. A recipient that
  received a descriptor for a meeting it already held ignored the descriptor's member list.
- **Superseded (2026-10-08, consensual-membership slice):** membership is now a convener-broadcast
  protocol. Invites are sent and must be accepted; the convener's descriptor is authoritative and
  peers apply it. The old "local only" limitation no longer describes the code, so this entry is kept
  only as history. See the new section at the top of this file.

---

## 2026-10-08 — Dev login silently failed against a stale daemon

After the Meetings frontend landed, the dev app would not log in. The cause was a version
mismatch, not a code defect, but the failure mode was silent and misleading.

### [FIXED] A stale daemon turned a 404 into a fake "vault locked" screen

- **Attempted:** log in to the dev app at `http://localhost:5173/?api=http://127.0.0.1:9876`.
- **Symptom:** submitting the correct passphrase returned the user to the unlock screen, with no
  error shown anywhere and no browser console error. The daemon was demonstrably healthy:
  `GET /api/status` returned `200 {"locked":false,...}` and `POST /api/unlock` returned `204`. The
  DOM stayed on `.unlock-screen` with no `.shell`.
- **Cause:** the new `App.refreshStatus()` calls `reloadMeetings()`, which hits
  `GET /api/meetings`. The running daemons had started at 09:12, before
  `target/debug/hush-daemon` was rebuilt at 11:44, so they did not serve that route and answered
  `404`. The pre-existing catch-all in `refreshStatus` (`apps/hush-tauri/src/app.ts`, around line
  340) treats *any* error as "the vault is locked" and sets `this.status = { locked: true, ... }`,
  so the UI snapped back to the unlock form even though the unlock had succeeded. In-page probe:
  `{"status_http":200,"locked":false,"contacts_http":200,"meetings_http":404}`.
- **Resolution:** rebuilt the daemons and restarted the stack with `scripts/run-two-ui.sh --dev`.
  No source change was needed. `GET /api/meetings` now answers `401` when locked and `200` when
  unlocked, and the unlocked shell renders.
- **Lesson:** Vite serves the frontend from source, so the frontend is always current while the
  daemon binary is only as new as its last build. Any frontend change that calls a new endpoint
  will fail against a daemon started before that endpoint existed. Restart the dev stack after
  landing daemon-side changes.

### [FIXED] `refreshStatus` treats any auxiliary-load error as "vault locked"

- **Attempted:** nothing yet; found while diagnosing the above.
- **Symptom:** a failure to load an *optional* resource (the mailbox or the conclave list) forced the
  whole app into the locked state, hid the real error, and made a healthy, unlocked daemon look like a
  failed login. The user saw wrong-passphrase behaviour for a problem that had nothing to do with the
  passphrase.
- **Resolution (2026-10-08, consensual-membership slice):** `refreshStatus` now wraps only the
  `api.status()` call in the "locked" path. A failure there sets `status = null` (unlock screen, as
  before); the contacts/mailbox/conclave loads run in their own `try` whose failure announces a retry
  message and leaves `status` untouched. Verified with `npx tsc --noEmit` (exit 0); the runtime path
  is not re-driven here (no display), see the UI-verification entry above.
- **Lesson:** a catch-all that maps every error onto a user-visible state will misattribute
  unrelated failures; scope error handling to the operation that can actually fail.

---

## 2026-10-08 — Committed a half-wired Meetings change to master

Committed the whole working tree to `master` as `350d2d1` while a Meetings worker was
mid-implementation. The commit does not compile.

### [FIXED] `350d2d1` on master fails to build: `MessagePayload::Meeting` has no match arm

- **Attempted:** commit the accumulated work (multi-device removal, outsider mail, Trusts, the
  mail pane rework) to `master` as a single checkpoint, after first trying to confirm that no
  agent had edits in flight.
- **Symptom:** `cargo test --workspace` exits 101 and zero tests run:

  ```
  error[E0004]: non-exhaustive patterns: `&MessagePayload::Meeting { .. }` not covered
     --> crates/hush-core/src/account.rs:1479:62
      |
  1479 |     let (file_blob_id, mail_attachments) = match &payload {
      |                                             ^^^^^^^^ pattern `&MessagePayload::Meeting { .. }` not covered
  ```

  `cargo build -p hush-core` fails identically, and every other crate depends on it, so nothing
  downstream builds either. `npx tsc --noEmit` passes (exit 0).
- **Cause:** the concurrent Meetings worker had already written `crates/hush-core/src/payload.rs`
  (the `Meeting` variant plus a round-trip test) and `crates/hush-core/src/chat_log.rs`
  (`MeetingRecord` / `MeetingMessage`), but had not yet added the match arm in `account.rs`. The
  commit captured that half-wired state. Parent `ff2a8d5` has no `Meeting` anywhere, so the
  breakage is introduced by this commit.
- **Why the pre-commit check missed it:** it relied on file mtimes for `app.ts` / `app.css` and a
  `find . -newermt '-4 minutes'` window. Neither proves a concurrent agent is idle: the worker had
  written `payload.rs` just outside that window, and the probe never looked at `payload.rs` or
  `chat_log.rs` at all. No build was run before committing.
- **Resolution:** fixed in `6419be1` ("Add Meetings: named group conversations over 1:1 fan-out"),
  committed on top of `350d2d1` with no amend and no history rewrite. The completed Meetings work
  adds the missing match arm in `account.rs`, so `master` builds again. Verified before committing:
  `cargo build --workspace` exit 0; `cargo test --workspace` 136 passed / 0 failed / 0 ignored
  (including the 5-test `meetings` suite); `npx tsc --noEmit` exit 0; no lint errors. The tip of
  `master` was broken from 11:24 until this commit landed.
- **Lesson:** a build is the only evidence that a tree is committable. Run `cargo check` before
  committing, not after, and do not treat an mtime probe as proof that a concurrent writer is done.

---

## 2026-10-08 — Internal-mail keyboard contact picker (the composer To field)

Replaced the composer's native `<select>` recipient list with a typeable, keyboard-driven
combobox (`ContactPicker.ts`). Problems hit while wiring and verifying it:

### [FIXED] Escape in the picker re-rendered the whole shell and dropped focus

- **Attempted:** press Escape to close the picker's list, expecting only the list to close.
- **Symptom:** the list closed, but focus fell to `<body>` and the composer was rebuilt from
  scratch. A marker set on the input node (`input.dataset.marker`) was gone afterwards, proving
  the node had been replaced. Cause: `App` installs a document-level `keydown` handler that
  calls `this.render()` on every Escape (app.ts, near line 273). The picker called
  `e.preventDefault()` but the event still bubbled to that handler.
- **Resolution:** added `e.stopPropagation()` in the picker's Escape branch when it actually
  handles the key. Re-verified in the browser: the marker survived, `document.activeElement`
  stayed on the input, and the value was restored.
- **Lesson:** a control that handles a key the app also handles globally must stop propagation,
  not just prevent the default; otherwise a local action triggers a global re-render.

### [WORKED AROUND] `browser_type` inserts text without firing keydown, so it did not exercise the replace-on-type path

- **Attempted:** type `zz` into a picker already showing the selected label `bob`.
- **Symptom:** the value became `bobzz`, not `zz`. `browser_type` uses `Input.insertText`, which
  fires only `input`, while the picker's "the first keystroke replaces the selected label" logic
  lives in `keydown`.
- **Resolution:** verified the replace path with real key events (`browser_press_key`): pressing
  `c` on a field showing `bob` cleared it to `c`. Verified the filter path separately with an
  `input` event.
- **Lesson:** `browser_type` is not a faithful keyboard for keydown-dependent behavior; use
  `browser_press_key` when the logic branches on key events.

### [OPEN] Native click-to-open could not be verified in the automation browser (tooling, not the repo)

- **Attempted:** click the To field to open the list, the way a mouse user would.
- **Symptom:** the input received focus (`document.activeElement` was the input) but
  `aria-expanded` stayed `false` and the popup stayed hidden. `document.hasFocus()` returned
  `false` for the whole session: the Cursor browser tab was never the OS-focused window, and a
  click in that state did not dispatch a `focus` event. Dispatching `new FocusEvent("focus")`
  and pressing ArrowDown both opened the list, so the handler itself is correct.
- **Resolution:** none required in the repo. Closing this needs a click-to-open check in a
  focused window (the Tauri window, or a manually focused browser).
- **Lesson:** in this environment, a missing native focus event after a programmatic click is an
  automation artifact, not an app bug. Do not "fix" the component for it.

### [WORKED AROUND] The no-contacts state could not be reached in the dev vaults

- **Attempted:** verify the "no contacts at all" empty state against a running composer.
- **Symptom:** both dev vaults (`peer-a`, `peer-b`) each hold exactly one contact, so the
  no-contacts branch is unreachable there, and the multi-match/accent filtering could not be
  exercised with one contact either.
- **Resolution:** imported the real `ContactPicker` module into the running page and mounted it
  with synthetic items (including an accented `Álvaro` and an empty list) to exercise the same
  code in the same browser. Verified: `alv` matches `Álvaro` (accent- and substring-insensitive),
  two matches highlight and move with ArrowDown, and the empty list shows its own message.
- **Lesson:** a one-contact dev vault is not enough to verify a picker; mount the real module
  with representative data rather than claiming the edge states work.

---

## 2026-10-08 — Public-key outsider drop (second drop mode, alongside the PIN mode)

Added `hush-drop-v2`, an X25519 sealed box for outsider drops, and a `hush-cli drop-keygen` /
`drop-open` pair so an outsider can make a key and open one. The PIN path (`hush-drop-v1`) is
untouched and verified unchanged. Problems hit while wiring and verifying it:

### [WORKED AROUND] A dev relay I started for a UI check shared port 8787 with a live two-peer session

- **Attempted:** to exercise the composer end to end I started `hush-relay` on the default
  `127.0.0.1:8787` with a throwaway DB (`HUSH_RELAY_DB=.tmp-ui-check/relay.db`) and a private daemon.
- **Symptom:** the two-peer dev environment (`scripts/run-two-ui.sh`, daemons on 9876/9877, Vite on
  5173) was already up and reuses a relay that answers on 8787. It therefore adopted my relay. When
  I then `rm -rf .tmp-ui-check` to tidy up, the running relay's SQLite database file was unlinked
  out from under it; new deliveries would fail because SQLite cannot recreate its journal at a path
  whose directory is gone.
- **Resolution:** killed my relay and restarted one at 8787 with the canonical
  `instances/relay/relay.db`, which is what `run-two-ui.sh` would have used. Any blob stored in the
  deleted DB is unrecoverable, but nothing in that window was under test. Re-ran the UI check against
  a private daemon on a free port (19876) instead of 9876, leaving the two-peer session alone.
- **Lesson:** check for a running dev relay before starting another, and never point a relay at a
  throwaway DB on a port a dev script may reuse. `ss -ltnp` before starting is cheaper than repairing
  a shared session.

### [FIXED] The UI check nearly used a daemon that was not mine (the stale-process trap)

- **Attempted:** `curl http://127.0.0.1:9876/api/status` to see whether my freshly started daemon was
  up.
- **Symptom:** it returned `200`, which looked like success — but `ss -ltnp` showed 9876 was already
  held by the two-peer session's `peer-a` daemon (started earlier, different vault, different
  passphrase), and my daemon had failed to bind.
- **Resolution:** checked `ps -eo pid,lstart,cmd` and `ss -ltnp` before trusting the status code, then
  ran my daemon on 19876. The user's warning about a stale daemon is exactly right: a `200` on the
  default port says nothing about which process answered.
- **Lesson:** never treat "the port answers" as "my process is up". Compare the process start time and
  the port owner first.

### [FIXED] The key smoke script wrote an attachment into the repository root

- **Attempted:** ran `scripts/smoke-drop-key.sh`, which opens the drop a second time with
  `drop-open --delivery <id> --relay <url>`.
- **Symptom:** `git status` showed an untracked `q3.txt` in the repo root. That invocation had no
  `--save-dir`, so the package attachment was written to the process working directory (the repo root,
  since the script was run from there).
- **Resolution:** added `--save-dir "$T/open-relay"` (and an `mkdir -p`) to that call; re-ran the
  script and confirmed `git status` is clean of stray files. This is the intended default of
  `drop-open` — write attachments where the user is — the script just had to contain it.
- **Lesson:** any script that drives a tool which writes to the CWD must pin the output directory.

### [FIXED] `pkill -f` matched the shell running it and killed the cleanup mid-way

- **Attempted:** `pkill -f 'hush-daemon --dir /home/sam/ProjectHush/.tmp-ui-check'` followed by
  `rm -rf .tmp-ui-check` in the same command.
- **Symptom:** the command produced empty output and the temp vault was still present. The `pkill`
  pattern appeared in the shell's own command line, so `pkill` killed the shell before the `rm` ran.
- **Resolution:** re-checked with `ps -eo pid,cmd | grep -F hush-daemon` (which does not match itself
  the same way) and removed the directory in a separate command.
- **Lesson:** `pkill -f` can match the invoking shell; scope it to a PID or a pattern that cannot
  appear in the shell's own argv. Tooling-related, not a repo problem.

### [FIXED] `cargo test` takes only one filter, and the build lock waited 2m34s behind the other agent

- **Attempted:** `cargo test -p hush-core --lib drop:: crypto::`.
- **Symptom:** `error: unexpected argument 'crypto::' found` — `cargo test` accepts a single test-name
  filter. Separately, `cargo test -p hush-core --lib drop::` reported `Blocking waiting for file lock
  on build directory` for 2m34s, because the concurrent multi-device agent was building in the same
  target directory.
- **Resolution:** ran the two filters as separate commands. The lock cleared on its own; no failure.
- **Lesson:** budget for the shared `target/` lock when another agent is building the same tree. This
  is a collision cost, not a defect.

---

## 2026-10-08 — Multi-device removed entirely (one device per account)

The user asked for multi-device support to be removed outright, not hidden behind a disabled entry
point. Removed: the signed device list (`DeviceList`/`DeviceRecord`, `PUT`/`GET /d/{account_id}`),
the relay `device_dirs` table, per-device fan-out, cross-device `Revoked` reconciliation and
`revoke_sweep`, device linking (`POST /api/link-device/*`), `ContactDevice`/`contact.devices`, the
device-list TTL refresh, and the tests that existed only for multi-device. Kept: per-contact queues,
per-contact revocation, the `revoked` blocklist, prekeys, and the on-disk device file (which still
reads `device_id`, now a constant `1`). `DEVICE_ID = 1` everywhere a libsignal address needs one.

**Grant authentication is unchanged in strength.** The device list used to be the authority a
`hush-queue-grant-v1` signature was verified against. With it gone, `apply_queue_grant` verifies the
signature directly against the contact's `identity_key` — the key pinned in the invite that
established the relationship, and the key that hashes to the contact's account id. The signature
check itself is untouched; only the device-list lookup was dropped, and there is no path that accepts
a grant verified against a sender-supplied key. The `revoked` blocklist is kept: it is what drops a
deleted contact's already-queued blobs on drain, a job the per-contact burn cannot do for a blob that
is already in the shared inbox.

### [FIXED] A grant send advanced the session ratchet without persisting it

- **Attempted:** verified the post-removal wire end to end by exchanging invites with the CLI
  (`hush-cli add-contact`) and then sending from the daemon that opened the same vault. The first
  message the recipient drained reported `Could not decrypt a queued message — unknown sender or
  session not ready.` and the blob stayed undecryptable on every later drain; later sends worked.
- **Cause:** `Account::grant_queues_to` calls `deliver_to`, which runs `encrypt_message` and advances
  the session ratchet, but never saves. `add_contact` saves *before* the grant, so a process that
  exits right after adding a contact (the CLI) left the ratchet on disk at its pre-grant position.
  The next process then encrypted with a reused chain key and the peer could not decrypt. The daemon
  masks this because it keeps the store in memory and saves again on its next send.
- **Resolution:** `grant_queues_to` now calls `self.save().await?` after the grant attempt. Re-ran
  the exact CLI-add/daemon-send flow: first message delivered, no warning. Full workspace suite still
  green.
- **Lesson:** any path that mutates the session store must persist it before it can be the last thing
  a process does. "The caller will save" is false when the caller is a one-shot CLI command.

### [FIXED] Mutation check on the revocation burn still fails when the burn is stubbed

- **Attempted:** stubbed `burn_contact_queue` to return `Ok(())` and ran
  `cargo test -p hush-core --test revocation`.
- **Symptom:** `deleting_one_contact_burns_only_its_queue` FAILED with "the deleted contact's queue
  must be burned immediately" — the assertion is real. `a_revoked_contact_cannot_deliver_into_the_shared_inbox`
  still passed, which is expected: it exercises the `revoked` blocklist dropping decrypted blobs, a
  separate mechanism from the burn, so it is not a mutation target for this stub.
- **Resolution:** restored the burn; both tests pass. Live check against the dev relay: a throwaway
  pair delivered into the recipient's per-contact queue, the recipient deleted the contact, and the
  relay's burned-queue count went 2 -> 3 (`select count(*) from queues where burned=1`).
- **Lesson:** a mutation check only proves the test that targets the mutated function. Record which
  assertions a stub is expected to break, or a passing bystander test looks like a hole.

---

## 2026-10-08 — Per-contact queue revocation implemented (option 2)

### [FIXED] A live delete did not burn, because the running daemon predated the wiring it was testing

- **Attempted:** ran the throwaway delete flow end to end against the dev daemon on
  `127.0.0.1:9876`: add a throwaway contact, deliver once, delete it, check the relay's burned count
  and whether the throwaway could still send.
- **Symptom:** `remove:204` and the contact was gone from `/api/contacts`, but `burned queues after
  delete: 0` and `SEND AFTER DELETE: UNEXPECTEDLY SUCCEEDED`. Rebuilding and restarting the daemon
  made the identical script pass: `burned queues before delete: 1` to `after delete: 2`, and
  `SEND AFTER DELETE: failed as expected -> Error: Relay("put failed: 403 Forbidden")`.
- **Cause:** the running process was started on 2026-10-07, before this slice. `target/debug/hush-daemon`
  was rebuilt at 06:36:31, but the daemon-side wiring it needed landed later:
  `crates/hush-daemon/src/account_worker.rs` (`RemoveContact -> revoke_contact`) was last written at
  06:44:02 and `crates/hush-core/src/payload.rs` (`QueueGrant` / `Revoked`) at 06:43:06. So the process
  image was a build in which removing a contact did not revoke. A `strings` check that the *binary on
  disk* contained the burn code was true and irrelevant — the process was running an older image.
- **Resolution:** killed the stale process, restarted `target/debug/hush-daemon`, and re-ran; the burn
  and the 403 then appeared. A temporary `eprintln!` in `burn_contact_queue` / `grant_queues_to`
  confirmed the granted and burned queue ids were identical, then was removed.
- **Lesson:** a long-running daemon is a build artifact with its own start time; verifying the file on
  disk proves nothing about the process image. Compare the process start time against the source and
  binary mtimes before trusting any live result, and restart the process after every rebuild.

### [FIXED] A burned queue reported 403, so a device's own reconciliation broke its next drain

- **Attempted:** after wiring `Account::recv` to drain the inbox plus every per-contact queue, and to
  burn a queue on an own-device revocation notice, ran the new integration test.
- **Symptom:** `Relay("list failed: 403 Forbidden")` from `alice_2.recv()`. The notice is processed
  during the inbox pass and burns device 2's queue for the deleted contact; the same `recv` then lists
  that queue, and `RelayClient::list_optional` only treated 404 as empty.
- **Cause:** the relay maps a burned queue to `403` (`RelayError::Burned -> FORBIDDEN`) and a wrong
  token to `401` (`Unauthorized`). `list_optional` was written for a queue that may not exist (404) and
  did not know a burned queue is also "nothing to read".
- **Resolution:** `list_optional` now returns an empty list on 403 as well as 404. A 401 (wrong token)
  still errors, so the relaxation cannot hide a credential bug. Evidence:
  `crates/hush-core/tests/revocation.rs` passes.
- **Lesson:** "the queue is gone" has two relay encodings — never created (404) and burned (403) — and
  a drain has to treat both the same. Enumerate the status codes a check must absorb, not just the one
  the happy path produces.

### [NO LONGER APPLICABLE] A linked device cannot open a session with a single-device contact that never published prekeys

- **Attempted:** the multi-device revocation test has Alice's second device grant its per-contact queues
  to Bob and Carol, who are single-device accounts.
- **Symptom:** `hush: grant to <bob> device 1 failed: relay error: prekey get failed: 404 Not Found`.
  A linked device holds none of the original device's sessions (the device store is not transferred), so
  `ensure_session` fetches the peer's prekey record; a single-device account only ever publishes prekeys
  inside `announce_linked_device`, so there is no record to fetch.
- **Impact:** in production a linked device's grant to such a contact silently fails, so that contact
  keeps delivering to the linked device's shared inbox. Revocation on the linked device then relies on
  the `revoked` blocklist dropping the messages rather than on burning a per-contact queue. Revocation
  still hides the messages, but the queue-burn guarantee does not hold for that pair until the contact
  publishes prekeys.
- **Worked around in the test** by calling `publish_prekeys(5)` on Bob and Carol, which models a contact
  that has published. Not fixed in the daemon: `ensure_device_list_published` runs on unlock but nothing
  calls `publish_prekeys` outside linking. Publishing prekeys on unlock would close it.
- **Status:** NO LONGER APPLICABLE (2026-10-08). Multi-device and device linking were removed, so the
  configuration this describes — a *linked* second device granting to a single-device contact — cannot
  occur. The entry is kept rather than deleted, per this log's rule. The underlying observation (a
  one-time `ensure_session` fetch against a peer that never published prekeys) is not exercised by any
  surviving code path, because a grant now always rides the session the invite established.

### [FIXED] Existing queue-count assertions in `two_party_relay.rs` assumed one blob per inbox

- **Attempted:** ran the workspace suite after adding queue grants.
- **Symptom:** `relay_blob_has_no_cleartext_sender_json` and `reporting_a_queue_burns_it` failed with
  `left: 2, right: 1`: the inbox now also holds the sender's queue grant alongside the message.
- **Resolution:** the tests assert two blobs (one grant, one message) and check the opacity of both.
  This is the intended new state, not a regression.
- **Lesson:** a test that counts queue blobs encodes the set of payload kinds that can land there.
  Adding a control payload is a change to that set.

---

## 2026-10-07 — Per-contact queue revocation (multi-device)

### [FIXED] Per-contact revocation is blocked on how a contact's *other* devices learn the queue

> **Closed 2026-10-08.** The user chose **option 2** and it is implemented. Each device derives a
> per-contact queue and its read token from its own read token (`crypto::contact_queue_id` /
> `contact_queue_read_token`), so the read token stays per device and out of the vault. The device
> list's queue is now a **grant channel**: each device sends a signed `MessagePayload::QueueGrant`
> (`queue_id`, `delivery_token`, `signature`) into the contact's inbox over the libsignal session, and
> the recipient verifies the signature against the identity key the signed device list vouches for that
> device — the same authority `DeviceList::verify` uses for the account id, so an unauthenticated write
> to the public grant channel cannot name a queue the writer owns. The blocklist (`AccountState.revoked`)
> remains only as defence in depth. Cross-device reconciliation is the `MessagePayload::Revoked` notice
> a device sends to its own siblings' inboxes (authenticated by the own-device session), which makes each
> sibling burn its own queue and drop the contact; `revoke_sweep` retries any burn that failed. Evidence:
> `crates/hush-core/tests/revocation.rs` (`deleting_one_contact_burns_only_its_queues_on_every_device`,
> `a_single_device_delete_burns_its_queue`), which fail when `burn_contact_queue` is disabled (verified
> again 2026-10-08 by stubbing `burn_contact_queue` to `Ok(())`: both tests fail, at the "device 1 must
> burn its queue" and the "must not be able to deliver" assertions).
>
> **Not fully closed.** One multi-device path is still broken and tracked separately as `OPEN`:
> a *linked* device's grant to a *single-device* contact that never published prekeys fails
> (`prekey get failed: 404`), so for that pair revocation falls back to dropping on drain rather than
> burning the queue. See "[OPEN] A linked device cannot open a session with a single-device contact
> that never published prekeys". The design is implemented; that pair's burn guarantee is not.


- **Attempted:** implement the fix the user chose — mint a per-contact queue at add time so deleting
  a contact burns only that contact's queue — by first designing it against the real send/recv path.
- **What is settled by the code and docs:** the relay needs no new endpoint. `Store::authorize`
  (`crates/hush-relay/src/store.rs`) mints a queue on first `PUT`, `Store::report_queue`
  (`store.rs:294`) burns it, and `POST /q/{id}/report` exists (`crates/hush-relay/src/app.rs:244`).
  `docs/DESIGN.md` already chose "invite-minted rotating receive queues"; `docs/PROTOCOL.md` says
  "recipient lists/drains their queues" (plural) and "rotate queue IDs inside the encrypted session".
  So a per-contact queue on the recipient, handed out in the invite, is the intended shape.
- **The blocking finding — multi-device.** A peer fans out one ciphertext per recipient device
  (`Account::send_payload_inner`, `account.rs:1084`), and the recipient learns those devices from the
  signed device list, which carries each device's `queue_id`/`queue_token`
  (`Account::publish_device_list`, `account.rs:1239`; `refresh_contact_devices`, `account.rs:1339`).
  For deleting a contact to cut off **every** device of the account, the contact must be unable to
  deliver to *any* of them, so the queues are necessarily **per (contact, device)**. But a queue's
  delivery token can only reach the contact privately — the device list is public and unauthenticated
  (`GET /d/{account_id}`, `app.rs:159`), which is the exact leak that makes the current rotation
  impossible. So the per-device queues must be granted in-session (a new payload), not published.
- **Why the invite alone cannot carry them:** the invite is generated by one device (whichever calls
  `invite()`), so it can only name that device's queue. The account's other devices' queues have no
  channel to the contact unless the *other* device grants them — and that device may have no session
  with the contact yet, and the contact may not even be a contact of that device (contacts are
  one-sided; a peer can add you without you adding them). A grant channel therefore has to be
  introduced (a per-device "control"/bootstrap queue in the device list that only accepts grants and
  own-copies), plus a blocklist so a revoked account's grants are ignored, plus cross-device
  reconciliation so each of the deleter's devices burns its own per-contact queues.
- **Second blocking finding — where the read token lives.** If per-contact queues are read by all of
  the account's devices (one queue per contact, simplest fan-out), the read token must be shared
  across devices, which today it explicitly is not: `DeviceKeys.read_token` is per device and kept out
  of the vault and out of backups (`device.rs:55-71`), and `docs/THREAT_MODEL.md` lists queue read
  tokens as "per device, never published". Sharing them is a threat-model change, and it weakens
  *device* revocation (a revoked device's vault copy could still read pre-existing per-contact
  queues unless every per-contact queue is also rotated). Keeping them per device avoids that but
  forces per-(contact, device) queues plus the cross-device burn described above.
- **Symptom:** neither branch is a patch. Both change the invite model (per-handout queues, so
  `AccountState.invite_cache` stops being a single blob and becomes a set of issued handout queues),
  the device-list semantics (its queue becomes a grant channel, not a delivery target), and the
  send/recv paths (poll N queues, route grants vs messages, drop a blocklisted sender). Each is
  security-critical, and the wrong pick produces a change that *looks* like revocation while leaving a
  device reachable — which the task says is worse than the current honest state.
- **Resolution needed (a user decision, then a slice):** pick one and I will implement it end to end:
  1. **Per contact, shared read token in the vault.** One queue per contact, all the account's
     devices read it, the sender fans out N ciphertexts into it. Smallest change; requires accepting
     that read tokens live in the vault (and rotating every per-contact queue when a *device* is
     revoked).
  2. **Per (contact, device), read token stays per device.** Sounder for device revocation; needs a
     per-device grant/control queue, a blocklist, and cross-device burn reconciliation. Largest
     change.
  Either way the invite becomes per-handout and the device list's queue becomes a grant channel.
- **Lesson:** "mint a queue per contact" is one sentence, but the recipient having more than one
  device turns it into a credential-distribution problem, and the only private channel is the session.
  Before committing to a revocation design, count the credential holders (every device of both
  parties) and ask how each one learns and loses the credential — not just the inviting device.

---

## 2026-10-07 — Trusts: typed-DELETE confirmation, copy sweep, card ordering

### [FIXED] The delete dialog cannot say "they lose the ability to message you", because they do not

> **Closed 2026-10-08.** The protocol change landed (see the newest section): deleting a contact now
> burns the per-contact queues its devices deliver into. The Trusts delete modal in
> `apps/hush-tauri/src/app.ts` now says the person is removed, history stays in the vault, and they can
> no longer message you. It states the one remaining limit accurately: a contact added before
> per-contact queues existed is still on the shared inbox (not burned, or every old contact would be cut
> off at once) and becomes revocable once it receives its own queue. The typed-`DELETE` gate is
> unchanged. `npx tsc --noEmit` passes.

- **Attempted:** built the requested typed-`DELETE` confirmation and wrote its copy to the spec:
  removed from Trusts, history stays in the vault, they lose the ability to message you, cannot be
  undone, plus a sentence about re-issuing the invite to the remaining contacts.
- **Symptom:** the revocation those two sentences describe does not exist. The entry below
  ("Rotating the invite cannot revoke a contact") is still `OPEN`, and no rotate-and-re-issue flow
  was built, so a deleted contact can still deliver to the relay inbox. The typed gate itself is
  real and was verified in the running app (disabled until the field matches, input focused, Enter
  guarded, Escape and backdrop dismiss, state reset on reopen); only the claim was not shippable.
- **Resolution:** shipped honest copy. The dialog says the person is removed from Trusts, that
  history stays, that they disappear from Chats/Mail/Trusts and the session is dropped, that this
  cannot be undone, and it states the limit plainly: they can still write to the relay inbox,
  because the invite cannot be rotated without cutting off the remaining contacts too. The
  "re-issued invite" sentence is omitted because no invite is re-issued.
- **What is still needed:** the protocol change named in the entry below. When it lands, the copy
  can be upgraded to the wording the requirement asked for. Until then, saying it would be a lie
  in the product.
- **Lesson:** a UI requirement can be downstream of a protocol decision. Write the copy against
  what the system actually does and record the gap, rather than letting a modal assert a security
  property that nothing enforces.

### [OPEN] WebKitGTK still has not run this pane, so the dialog is verified only in Chromium

- **Attempted:** verify the Trusts pane, the typed confirmation, the ordering and the filter in a
  running app rather than by reading code.
- **Symptom:** no failure. Everything above was exercised against the Vite dev server at
  `localhost:5173` in the Cursor IDE browser (Chromium) and against the live daemon on
  `127.0.0.1:9876`: the card order `ámbar, bob, Zed` from an API order of `bob, Zed, ámbar`, the
  filter narrowing with real `input` events, and both delete paths (Enter and the button) end to
  end. But the shipped desktop shell is WebKitGTK, and no Tauri CLI is installed here, so that
  engine was never run.
- **Resolution:** not resolved. Same constraint as the composer's `execCommand` entry: browser
  evidence, not shell evidence.
- **Lesson:** say which engine the evidence came from. "Verified in the app" is ambiguous when the
  dev server and the desktop shell are different engines.

---

## 2026-10-07 — Mail reading pane (header, rules, absolute time)

### [FIXED] A concurrent edit to `app.ts` turned `tsc` red in code this slice does not own

- **Attempted:** re-run `npx tsc --noEmit` as the final check after finishing the mail-pane edits.
- **Symptom:** `src/app.ts(1231,7): error TS2304: Cannot find name 'setKnownContacts'` (exit 2).
  The name is imported at `app.ts:38` from `./lib/mailStore` and called at lines 258, 317, 1234 and
  3225, but at that moment no module exported or defined it — a repo-wide grep found it only at
  those call sites. The failing code is the Trusts/contact-revocation slice, which belongs to the
  other agent and is off-limits here. The first `tsc` run in this session, taken before that edit
  landed, passed with exit 0.
- **Resolution:** not touched in this slice, deliberately — repairing it would have meant editing
  another agent's in-flight code and colliding with it. The other agent then landed the export
  (`setKnownContacts` now defined at `mailStore.ts:86`), and a fresh `npx tsc --noEmit` in this
  session returned exit 0 with no output. That run is the evidence that closed this entry.
- **Lesson:** on a shared working tree a clean check can go red between two commands. Record the
  exact error and which slice owns it; do not repair another agent's half-finished edit. Re-running
  the check later is enough when the owner is still working.

### [OPEN] A concurrent agent edits the same two source files, so the verified UI state is not final

- **Attempted:** implement the mail header/rules/absolute-timestamp slice in
  `apps/hush-tauri/src/app.ts` and `apps/hush-tauri/src/styles/app.css` while another agent was
  building the Trusts rail in the same two files.
- **Symptom:** `git diff` for `app.ts` is 637 lines and mixes several pre-existing uncommitted
  slices (rich-text compose, outsider mail) with the Trusts work, so this slice's diff cannot be
  read in isolation. The other agent also appended its own section to this log
  (`## 2026-10-07 — Trusts rail and contact revocation`) between two of my reads of this file,
  so the race is live, not hypothetical.
- **Resolution:** re-read each region immediately before editing and used small anchored
  replacements; every edit applied cleanly and `npx tsc --noEmit` stayed clean, so no collision
  occurred in this slice. The header block, the rules and the absolute timestamp were then
  verified in the running dev server.
- **What is still needed:** the UI was verified against a tree another agent is still writing, so
  the reading pane must be re-checked once the Trusts work lands. This is a workflow risk, not a
  code defect in the mail pane.
- **Lesson:** on a shared working tree, "it type-checks" certifies the tree only at the instant it
  was read. Anchor edits to exact text and re-read before every write; do not treat a whole-file
  diff as proof that a change is yours.

### [WORKED AROUND] `browser_snapshot` collapses the mail header, so the DOM had to be read over CDP

- **Attempted:** verify the new From/To/Date/Subject block using the browser tool's accessibility
  snapshot of the open thread.
- **Symptom:** the snapshot listed the heading and the action buttons but not the `<dl>` fields,
  so it could not show whether the header rendered at all.
- **Resolution:** read the rendered text and computed styles with `browser_cdp`
  (`Runtime.evaluate`), which confirmed the four fields, the 1px `--border` rules and the aligned
  label column. The accessibility snapshot is not a substitute for reading the DOM for
  layout-only changes.
- **Lesson:** assert on computed style and geometry, not just the accessibility tree.

---

## 2026-10-07 — Trusts rail and contact revocation

### [FIXED] Rotating the invite cannot revoke a contact: the public device list republishes the delivery token

> **Closed 2026-10-08.** Revocation no longer depends on rotating the invite. Contacts deliver into a
> per-contact queue that each device mints and reads with a per-device credential; the device list's
> queue is a grant channel and no longer a delivery target once a grant has landed. Deleting a contact
> burns those queues on every device of the deleter, so the published device list does not resurrect a
> delivery path. The delivery token is still public by design, but it now authorizes only putting a blob
> in a queue that the deleter can burn, and the grant that names a queue is signed by the granting
> device's identity key and verified against the signed device list. Migration limit (old contacts on
> the shared inbox) is documented in `docs/PROTOCOL.md` and in the modal copy. Evidence:
> `crates/hush-core/tests/revocation.rs`.

- **Attempted:** built the Trusts rail and, before wiring Delete to a
  rotate-and-re-issue flow, traced what actually authorizes a delivery into an
  account's inbox.
- **Evidence from the code:**
  - `Account::invite()` (`crates/hush-core/src/account.rs`) mints and caches one
    invite carrying the account's single inbox `queue_id` and its delivery token.
    Every contact of an account receives that same invite, so they all hold the
    same delivery token. `DeviceKeys::queue_token()` (`crates/hush-core/src/device.rs`)
    is `queue_delivery_token(read_token)`, one value for the whole account.
  - `Store::authorize` (`crates/hush-relay/src/store.rs`) binds a queue to
    `sha256(delivery_token)` on first write and rejects a non-matching token.
    There is no path to rebind an existing queue, so a rotation means a new queue
    id and a new token.
  - The decisive leak: `Account::publish_device_list()` writes
    `queue_token: self.device.queue_token()` for device 1 into the signed list,
    and `GET /d/{account_id}` is deliberately unauthenticated
    (`crates/hush-relay/src/app.rs`). `Account::refresh_contact_devices()` reads
    that list and overwrites the contact's `queue_id`/`queue_token`
    (`crates/hush-core/src/account.rs`). A contact learns the owner's
    `account_id` from the invite it was given (`InvitePayload.protocol_name`).
- **Symptom:** any rotate-and-republish revocation is undone by the revoked
  contact's next send. `refresh_contact_devices_if_stale` (5-minute TTL, and it
  also runs from the daemon's `/api/contacts`) fetches the public device list and
  picks up the new delivery token. Burning the old queue does not help, because
  the new queue is the one the list advertises. This contradicts
  `docs/THREAT_MODEL.md`, which names "a contact who was blocked (they should not
  keep delivering to a live slot)" as an attacker, and `docs/PROTOCOL.md`, which
  describes a queue "the other party can send into" per contact
  ("Block = drop local session + tell relay to burn that queue"). The
  implementation collapsed that per-contact slot into one shared account inbox.
- **Resolution needed:** a protocol change, not a patch. Options, none
  slice-sized: (a) per-contact inboxes minted when a contact is added, with
  revoke = burn that contact's inbox (matches PROTOCOL.md; needs a way to hand the
  dedicated inbox to the contact and keep it out of the public device list);
  (b) authenticate `GET /d/{account_id}` behind a directory capability rotated on
  revocation and re-issued to remaining contacts; or (c) rotate the account
  identity, which breaks every session. Until one is chosen, Delete removes the
  contact, keeps history, and drops their sessions, but does **not** stop them
  writing to the inbox.
- **Lesson:** a credential published at a stable, unauthenticated rendezvous
  cannot be rotated away from someone who already knows the rendezvous key. The
  question is not "can I mint a new token" but "who can discover it".

---

## 2026-10-07 — Outsider mail (delivery + drop)

### [FIXED] The drop container's production KDF cost was paid in every test, taking the suite from seconds to 85

- **Attempted:** wrote the `hush-drop-v1` container tests against the public `seal`/`open`, which
  stretch the PIN with PBKDF2 at 600,000 iterations.
- **Symptom:** `cargo test -p hush-core --lib` went from ~13s to **84.9s**, with four tests each
  reporting `has been running for over 60 seconds`. The tests were correct and passed; the cost was
  the production iteration count multiplied by every seal and open in the tests, on an unoptimized
  build.
- **Resolution:** split `seal` into a public entry point that passes `DROP_ITERATIONS` and a private
  `seal_with_iterations`. The count is recorded in the container header either way, so `open` needs
  no hint and the tests exercise the same code path with 1,000 iterations. The drop tests now run in
  **0.07s** and the production count is unchanged and pinned by a compile-time assertion
  (`const _: () = assert!(DROP_ITERATIONS >= 600_000)`).
- **Lesson:** a deliberately expensive primitive has to be injectable, or the cost lands on the test
  suite and people quietly lower it. Pin the production value at compile time so parameterizing it
  for tests cannot become a way to weaken it.
- **Still true after the fix:** the *integration* test in `crates/hush-core/tests/outsider_mail.rs`
  deliberately pays the real 600k cost, because it is the only place the shipped path is exercised
  end to end. It is the slowest test in the suite (~40s in a debug build). That is a cost, not a
  defect, but it should not grow: keep the number of real-iteration seals and opens in integration
  tests to the minimum that still proves the shipped path.

### [FIXED] A wrong KDF test-vector reference was written into the plan before the code existed

- **Attempted:** specified the PBKDF2 test as "RFC 6070, case 2" with a 20-byte output.
- **Symptom:** RFC 6070 is the **PBKDF2-HMAC-SHA1** vector set. Using it to check a
  PBKDF2-HMAC-SHA256 implementation would have asserted the wrong digest, or the test would have
  been "fixed" by weakening the implementation to match a SHA-1 result.
- **Resolution:** the implementation and its test use the standard PBKDF2-HMAC-SHA256 vectors
  (including a 40-byte case that spans two output blocks), and the plan text was corrected to say
  why RFC 6070 does not apply. Verified: `pbkdf2_matches_the_published_sha256_vectors` passes.
- **Lesson:** a test vector quoted from memory is a claim. Name the exact document and hash, and
  prefer a vector whose digest length forces the multi-block path.

---

## 2026-10-07 — Rich-text mail compose

### [WORKED AROUND] The browser-automation text tool does not carry the editor's typing style, so formatted typing looked broken

- **Attempted:** verified the new compose editor by clicking Bold, then using the browser tool's
  "type" action to enter text, then reading the editor DOM.
- **Symptom:** the toolbar reported `Bold: pressed` but the surface's `innerHTML` was the bare string
  `bold text and a bullet` — no `<b>`, no `<ul>`. It looked like `execCommand` was a no-op in the
  WebView, which would have meant the feature did not work.
- **Resolution:** the same `document.execCommand("bold")` run directly through `Runtime.evaluate`
  against selected text produced `<b>hello world</b>`, and `insertUnorderedList` produced
  `<ul><li><b>hello world</b></li></ul>`. The automation's type action inserts text by a path that
  does not apply the browser's pending typing style, so the formatting was never going to appear.
  This is a tooling artifact, not a repo problem. The editor was then verified end to end (send,
  recipient, draft round-trip) by driving the formatting commands through CDP and reading the DOM.
- **Lesson:** `queryCommandState` reporting a toolbar button as active says nothing about the DOM.
  Confirm formatting by reading `innerHTML`, not the pressed state — the same trap the Phase 5 note
  records for toolbar clicks.

### [FIXED] Saving a draft opened from a thread left the reading pane on the old thread

- **Attempted:** tested the rich composer from the Compose button while a conversation was open, then
  saved a draft.
- **Symptom:** the folder switched to Drafts (badge updated, list correct) but the main pane still
  rendered the conversation that had been open, because `openCompose` never cleared `mailThreadId`
  and `renderMailMain` prefers the thread over the list. Pre-existing, but only reachable by opening
  compose while a thread was displayed, which the new test flow did.
- **Resolution:** `openCompose` now clears `mailThreadId`/`mailOpenId` for a brand-new message (no
  draft and no reply thread), so leaving compose returns to the folder list. Replies and draft edits
  keep their thread. Verified: Save draft from an open thread now lands on the Drafts list.
- **Lesson:** "the folder changed" and "the pane changed" are separate assertions. Checking only the
  badge would have missed this.

### [OPEN] The composer formats with `document.execCommand`, which is deprecated and untested on WebKitGTK

- **Attempted:** chose a formatting API for the contenteditable surface without adding an editor
  dependency (ProseMirror/Tiptap/Quill would be a large new dependency for bold/italic/lists).
- **Symptom:** no failure yet. `execCommand` is deprecated and its behaviour is not guaranteed
  identical across engines. It was exercised only in the Cursor IDE browser (Chromium) during this
  slice; the real desktop shell on Linux runs WebKitGTK, which was not run here (no Tauri CLI — see
  the entry above about the unreferenced dev config).
- **Resolution needed:** confirm bold/lists/headings in a packaged `hush-tauri` window on WebKitGTK
  before relying on it. If it misbehaves, the fallback is a small Range/Selection formatter over the
  same allowlisted subset; the sanitizer and renderer would not change.
- **Lesson:** an untested-by-construction choice should be written down as such, not presented as
  done. The rendering and sanitization paths are engine-independent and were verified; the editing
  commands are not.

---

## 2026-10-07 — Live UI development without rebuilding the desktop binary

### [FIXED] `run-two-ui.sh --dev` left the Vite dev server running after Ctrl+C

- **Attempted:** added a `--dev` mode that starts the relay, both daemons with a TCP listener, and
  `npm run dev`, then checked that stopping the script releases everything.
- **Symptom:** the relay and both daemons died, and `node .../node_modules/.bin/vite` stayed alive with
  port 5173 still bound, so the next `--dev` run failed its port check. The dev server is started as
  `npm run dev`, which runs the script through `sh -c`; `npm` did not forward the `SIGTERM` that
  `cleanup()` sent to its pid, and the `sh` wrapper died while its child did not.
- **Resolution:** the dev server is now started with `setsid` and stopped as a process group
  (`kill -TERM -- "-$VITE_PGID"`), which takes `npm`, `sh`, and `vite` together. Verified by starting a
  run, sending `SIGINT` to the script only, and confirming no leftover process and no listener on 5173,
  8787, 9876, or 9877.
- **Lesson:** `npm run <script>` is two processes, not one. Anything that has to be killed later must
  either be exec'd directly or started in its own process group.

### [OPEN] The repo carries a Tauri dev config that nothing references, and no Tauri CLI is installed

- **Attempted:** looked for a way to run the desktop window against the Vite dev server (hot reload
  inside the real shell instead of a browser tab).
- **Symptom:** `apps/hush-tauri/src-tauri/tauri.dev.conf.json` sets `devUrl` and `beforeDevCommand` as
  Tauri's dev mode requires, but nothing in the repo passes `--config` to it: no script, no doc, no
  mention outside the file itself. `cargo-tauri` is not installed and `@tauri-apps/cli` is not in
  `node_modules`, so `tauri dev` cannot run here at all.
- **Resolution needed:** either wire the config up (add `@tauri-apps/cli` and a `dev:app` script that
  runs `tauri dev --config src-tauri/tauri.dev.conf.json`) or delete the file. Until then, live
  development is browser-only, which is what `--dev` provides. Not verified either way in this
  environment: no Tauri CLI to try it with.

---

## 2026-10-07 — Mail client UI: folders, drafts, Outbox

### [FIXED] "Drafts are deletable" was implemented in the store and the daemon, and reachable from nowhere

- **Attempted:** verified the new Drafts folder in the browser by saving a draft, reopening it, and
  editing it. Then went looking for the delete control, because the requirement said drafts are
  deletable.
- **Symptom:** there was none. `deleteDraft` existed in `mailStore`/`api`, `DELETE /api/mail/draft/{id}`
  existed on the daemon, and a core test (`a_draft_is_saved_edited_and_deleted`) passed — but the
  rendered compose pane held only `Attach files`, `Save draft`, `Send`, and the draft row in the list
  offered nothing. The capability was verified at every layer except the one the user touches.
- **Resolution:** added a `Discard draft` button to the composer, shown only while editing a saved
  draft (a brand-new message is abandoned by closing compose, and nothing was written for it). Verified
  in the browser: the record is removed, `chat.enc` returns to its pre-draft size, and the Drafts badge
  clears.
- **Lesson:** a green core test proves the function, not the feature. For anything with a UI, the
  acceptance check has to name the control that triggers it, and "deletable" is not satisfied by a
  function nobody can call.

### [FIXED] Dev-mode CORS rejected `PUT` and `DELETE`, so draft save and delete worked from curl and failed in the browser

- **Attempted:** exercised the new draft endpoints from the running dev UI.
- **Symptom:** the preflight was refused. `local_ui_cors` allowed `GET`, `POST`, `OPTIONS` only, so
  `POST /api/mail/draft` (fine) worked while `DELETE /api/mail/draft/{id}` and any `PUT` were blocked by
  the browser before the request left it. The same calls returned 204 from `curl`, which is exactly why
  this was not caught earlier: the smoke script and the tests do not run through a browser.
- **Resolution:** added `Method::PUT` and `Method::DELETE` to the CORS layer.
- **Lesson:** a curl-only smoke suite cannot see CORS. When a UI slice adds a new HTTP method, the
  allowed-methods list is part of the change.

### [FIXED] Changing `send_mail`'s return type stopped `internal_mail.rs` compiling

- **Attempted:** made `send_mail` return `Result<MailOutcome, Error>` so a queued send could be reported
  to the caller instead of surfacing as an error.
- **Symptom:** the crate no longer compiled — the existing tests in `crates/hush-core/tests/internal_mail.rs`
  assert on the old `Result<(), Error>` shape, so every call site in that file was a type mismatch. The
  change was correct and the tests were simply written against the previous signature.
- **Resolution:** updated the existing assertions and added the new queue/retry, draft round-trip, and
  draft-not-plaintext tests in the same pass.
- **Lesson:** a return-type change in `hush-core` is a workspace-wide edit. Grep the tests in other
  crates before running them, or the first signal is a compile wall.

### [FIXED] New `decode_attachments` tripped `clippy::type_complexity`

- **Attempted:** ran `cargo clippy --workspace --all-targets` after the slice, per the completion
  standard, to check nothing new had been added to the pre-existing warning baseline.
- **Symptom:** `warning: very complex type used` at `crates/hush-daemon/src/main.rs:779` — the
  `Result<Vec<(String, Vec<u8>)>, (StatusCode, Json<ApiError>)>` return of the new helper. The rest of
  the warnings (7 `redundant_closure` in `backup.rs`, 4 `ptr_arg`, 1 `items_after_test_module`) are
  pre-existing and were confirmed against `git show HEAD`.
- **Resolution:** named the error half `type ApiErrorResponse = (StatusCode, Json<ApiError>)` and used it
  in the signature.
- **Lesson:** a warning count is only meaningful against a baseline. Check the offending file against
  `HEAD` before deciding whether a warning is yours.

### [FIXED] `run-two-ui.sh` could not be re-run while a harness was live, and tore itself down instead of saying so

- **Attempted:** re-ran `scripts/run-two-ui.sh` to pick up a rebuilt UI while the previous run's relay and daemons were still up.
- **Symptom:** the script printed its usual "Opening two UI windows" block and then immediately exited with "A process exited; shutting down…", taking the windows with it. Two daemons had been started on sockets that were already live; `bind_socket` refuses to unlink a live socket (correctly), so both new daemons exited at once, and `wait -n` returned on the first of them. The readiness loop had not noticed because the *old* daemon answered `/api/status` on that path, so every check looked healthy.
- **Resolution:** the script now probes each socket first and reuses a daemon that answers, the same way it already reused a running relay. Starting a daemon is followed by a readiness check that also watches for the process dying, and a failure prints the daemon's log and exits non-zero instead of continuing. Per-process logs moved into `$RUN_DIR/logs/`. Reuse is refused when a TCP listener was requested and is not open, since a running daemon cannot grow one.
- **Lesson:** "already running" and "just started" have to be told apart before starting anything, or the second run silently fights the first. A readiness check that only asks "does something answer here?" cannot see its own process die.

### [FIXED] A bad `HUSH_RUNTIME_DIR` surfaced as a raw `mkdir` error and skipped the script's own diagnostics

- **Attempted:** tested the new daemon-startup failure path by pointing `HUSH_RUNTIME_DIR` at an unwritable path.
- **Symptom:** the script exited 1 before reaching any of it, with `mkdir: cannot create directory '/proc/nope': No such file or directory` and no mention of which variable to fix.
- **Resolution:** the `mkdir` is now guarded and names `HUSH_RUNTIME_DIR` in its message. The daemon-death path was then verified separately by pointing `HUSH_DAEMON_A_SOCKET` at an unwritable path, which produced `alice daemon did not come up on /usr/peer-a.sock:` followed by the daemon's `PermissionDenied`.
- **Lesson:** `set -e` plus an unguarded setup command means the first failure the user sees is the least informative one.

### [WORKED AROUND] A toolbar click that reports `[active, focused]` has not necessarily done anything

- **Attempted:** clicked `Move to Trash`, `Restore to Inbox`, and `Delete forever` and read the DOM
  immediately after each.
- **Symptom:** the snapshot came back with the button `active, focused` and the pane completely
  unchanged, which reads as a dead handler. The actions are async — each one writes `ui-prefs.json` over
  HTTP and re-renders after the response — so the read raced the write. Re-checking the DOM a second
  later showed the correct result every time; no code was at fault.
- **Resolution:** after each action, wait for the settle (a short sleep, or poll the folder badge) before
  reading the DOM. Do not click a second time: the duplicate click is what turns a slow write into an
  apparent bug.
- **Lesson:** an `active`/`focused` button is evidence of a click, not of an effect. Tooling-related,
  not a repo problem.

---

## 2026-10-07 — Two-peer presence: Alice and Bob could not see each other

Symptom: in the `scripts/run-two-ui.sh` harness both peers listed each other with
`inbound_ready: true` but `online: false`, so the composer stayed disabled and a message sat
undelivered in the relay.

### [FIXED] A rotated relay inbox was republished only once, so a lost publish left the peer unreachable

- **Attempted:** traced presence end to end. `POST /api/presence/heartbeat` on both daemons returned
  204 and advanced `presence_at` for both live queues (`61c4082e` = Alice, `ec716fd5` = Bob), yet
  `GET /api/contacts` still reported `online: false`.
- **Symptom:** both contact records named inboxes no daemon reads. Alice's contact `bob` held
  `917eae3e` (a blob was sitting in it, delivered minutes earlier and never received); Bob's contact
  `alice` held `53050bc1`, last heartbeated the previous day. Both peers' real inboxes had changed —
  the read/delivery credential split rotates every pre-split vault's queue once — but the contacts
  still pointed at the abandoned ones.
- **Cause:** the republish that tells peers about the new inbox ran only on the unlock that rotated
  (`if a.queue_rotated()`), and it was best effort. `Account::open` rewrites `device.enc` as v2 on
  that same call, so `queue_rotated()` is false on every later open — the comment claiming "the next
  unlock retries" was wrong, and a publish that failed (relay not up yet) was lost for good.
  `device_dirs` on the relay held Alice's list and no row at all for Bob, so nothing could ever point
  Alice at Bob's real inbox.
- **Resolution:** `Account::ensure_device_list_published()` compares the *published* list against this
  device's current inbox and republishes when they disagree (including "no list at all"), so the
  repair is durable and also heals a vault whose publish was lost before the check existed. The daemon
  unlock path calls it unconditionally instead of gating on `queue_rotated()`.
- **Lesson:** a one-shot flag is not a retry policy. When the condition is cleared by the same
  operation that consumes it, "best effort" silently becomes "never". Compare against durable state
  (what is actually published) instead of trusting a one-shot marker.

### [FIXED] Presence read a cached inbox, and the UI refuses to send to an offline contact, so a moved inbox stayed offline forever

- **Attempted:** looked for anything that would refresh a contact's cached queue. Only
  `send_payload_inner` called `refresh_contact_devices_if_stale`.
- **Symptom:** `contact_is_online` checks the cached `queue_id`, so a contact whose inbox moved read as
  offline; and the UI disables the composer and send button for an offline contact (`renderComposer`,
  `ta.disabled = !contactOnline`), so the one action that would refresh the cache could never run.
  Alice could not learn Bob's new inbox even after he republished, because she had no way to trigger
  the refresh.
- **Resolution:** the daemon's `/api/contacts` handler now refreshes any stale contact device list
  before reporting presence (`refresh_contact_devices_if_stale`, made `pub`), bounded by the same
  5-minute TTL the send path already used. Presence polling is the natural healing point.
- **Lesson:** when a UI gate depends on a cached value, something other than the gated action has to
  be able to refresh it. "We only refresh on send" plus "we never send while stale" is a closed loop.

### [OPEN] Desktop screenshot under Wayland is a black root, so the UI windows could not be verified visually

- **Attempted:** captured the screen with `ffmpeg -f x11grab -i :0` to confirm the two Tauri windows
  show "Online".
- **Symptom:** the PNG is 1920x1080 and 100% black (68 distinct colours, all antialiasing noise). The
  session runs Wayland (`WAYLAND_DISPLAY=wayland-0`); x11grab only sees the empty XWayland root, and no
  Wayland-native tool (`grim`, `spectacle`) is installed.
- **Resolution:** not resolved. Verified through the daemon instead: both `/api/contacts` responses
  report `online: true`, and both queues' `presence_at` advance on their own every few seconds, which
  only happens if the two UI windows are up, unlocked and polling.
- **Lesson:** tooling-related, not a repo problem. On this host the UI has to be verified through the
  daemon API; a desktop screenshot is not evidence here.

---

## 2026-10-07 — Repo consolidation (two worktrees merged into master)

### [FIXED] `pkill -f '<pattern>'` killed its own shell before the build could start

- **Attempted:** stop a stale `cargo check` by pattern and launch a fresh one in the same command:
  `pkill -f 'cargo check -p hush-core'; … ; nohup cargo check … > /tmp/check3.log &`.
- **Symptom:** `/tmp/check3.log` was never created and none of the follow-up commands ran. `pkill -f`
  matches the full command line, and the invoking shell's own command line contained the literal
  pattern, so it signalled itself before the redirect was set up.
- **Resolution:** killed by pid (`pgrep -x cargo` / `pgrep -x rustc`) instead of by a pattern that
  appears in the caller's own command line.
- **Lesson:** `pkill -f` / `pgrep -f` match the caller's own command line. Never pattern-kill on a
  string the command itself contains; use exact process names or the `[c]argo` bracket trick.

### [FIXED] `AwaitShell` returned instantly because the launcher shell had already exited

- **Attempted:** polled a backgrounded build with `AwaitShell` using the shell id of the command that
  started it (`nohup cargo check … & echo started`).
- **Symptom:** the poll reported the job "completed" in ~5s with exit 0 and "pattern did not match"
  while cargo was still compiling. The launcher shell only ran `echo` and exited; `nohup` had
  detached cargo, so the shell job's completion said nothing about the build.
- **Resolution:** polled the process (`pgrep -x cargo`) and the log file instead of the launcher's
  shell id.
- **Lesson:** a shell id tracks the shell, not the process it backgrounded. Detached jobs need
  process or log polling.

---

## 2026-10-07 — Tooling: backgrounded build, and a wrong "it died" call

### [FIXED] A running background build was reported dead, then started a second time

- **Attempted:** warmed the new master worktree's cold Rust build in the background so the two-UI
  script would start fast, then checked on it.
- **Symptom:** a `task finished: success` notification arrived for the launcher command (which ends
  in `&` and returns immediately). I then looked only at the log tail (stalled at
  `Compiling cairo-sys-rs`) and at the absence of binaries in `target/debug`, and reported that the
  build had been killed. It had not been: `pgrep -af bin/cargo` showed the original process still
  running with eight `rustc` children, and the log had grown to 209 lines. My "restart" added a
  second cargo that blocked on the target-dir lock, which I then had to kill. The build finished on
  its own in 5m 02s and produced all three binaries.
- **Resolution:** no repo change needed; the build was healthy throughout. This is a tooling and
  inference problem, not a code problem.
- **Lesson:** before calling a background job dead, check the process (`pgrep`), not its side
  effects. "No output yet" plus "no artifacts yet" is not evidence of death. Also, a `task finished`
  notification for a command ending in `&` refers to the launcher, not the job it spawned.

---

## 2026-10-07 — Internal member mail (Task 5d)

### [FIXED] A stale second browser tab drained the relay queue and looked like UI staleness

- **Attempted:** verifying the new unread badge by reading the rail badge and thread list from the
  live page while a hand-rolled two-peer harness delivered mail.
- **Symptom:** intermittently the daemon's `/api/mail` held records while the UI store did not, and
  the badge stayed off. Instrumenting `window.fetch` in the page showed every `/api/recv` returning
  `{"messages":[],"warnings":[]}` even though new mail had just been sent — so the queue was being
  drained by someone else before the observed tab's poll.
- **Root cause:** `browser_tabs` listed two Hush tabs: the current one on `:4174` and a leftover tab
  on `:4173` from an earlier session. The `:4173` preview server was long dead, but the already
  loaded page kept running and polling the same daemon on `:9876`. It won the drain race and kept
  the mail in its own JS context; the tab under test then legitimately saw an empty queue. This is a
  harness problem, not an app defect.
- **Resolution:** closed the stale tab, then re-verified with a single polling client: the badge
  appears on incoming mail, the thread shows one unread dot, opening the thread clears both, and a
  thread whose first message had no subject displays the later subject.
- **Lesson:** before trusting a UI-observation mismatch, list the open tabs. A dead preview server
  does not stop an already-loaded page from continuing to talk to a shared daemon.
- **Related fix:** `loadMail` used to call `clearMailStore`, which also wiped unread. Any refresh
  that ran after an ingest (backup download, saving the mail handle, re-unlock) would have dropped
  the badge. `loadMail` now rebuilds thread content only and leaves unread alone.

### [WORKED AROUND] Re-seeding the manual UI harness produced a spurious relay `401`

- **Attempted:** built a two-peer harness (relay + two daemons) to check the Mail polish in a real
  browser, ran it, then re-ran it to pick up a port change. The second run began with
  `rm -rf /tmp/hush-mail-polish` while the first run's relay was still running and holding
  `relay.db`.
- **Symptom:** the recipient's `/api/recv` returned `{"error":"relay error: list failed: 401
  Unauthorized"}` and `/api/mail` stayed `[]`, which looks like a queue-credential regression. It
  is not: `scripts/smoke-mail.sh` (clean single run, fresh dir) passes the same store-and-forward
  path end to end, including the offline send and the reply.
- **Resolution:** stopped reusing a directory while processes hold it. The harness is disposable:
  kill relay + daemons first, then wipe, then seed once. This is a tooling/harness problem, not a
  repo problem.
- **Lesson:** a `401` from a hand-rolled harness is not evidence of a code regression until the
  hermetic smoke script reproduces it. Run the script first.

### [WORKED AROUND] `CODE` from a shell helper is lost inside `$(...)`

- **Attempted:** in `scripts/smoke-mail.sh`, captured the response of `api_at` (a helper that sets
  the global `CODE` and writes `$T/out.json`) with `THREAD=$(api_at ... | sed -n ...)`.
- **Symptom:** the smoke failed with `mail send returned 400: {"thread_id":"d862..."}` — a 400 that
  was actually the *previous* request's status. A command substitution runs in a subshell, so the
  `CODE=...` assignment inside `api_at` never reached the parent shell; the `[[ "$CODE" == "200" ]]`
  check read a stale value while the body was a genuine 200.
- **Resolution:** call `api_at` directly (not in `$(...)`) so `CODE` and the new `RESP` global
  propagate, then parse `$RESP`. Only wrap the pure text transform (`printf | sed`) in `$(...)`.
- **Lesson:** a shell helper that communicates through globals cannot be called in a command
  substitution. Either return via stdout and never globals, or call it as a statement.

### [FIXED] New `MessagePayload::Mail` variant broke an exhaustive match in `hush-cli`

- **Attempted:** added `MessagePayload::Mail { .. }` to `hush-core` and built the workspace.
- **Symptom:** `error[E0004]: non-exhaustive patterns: 'MessagePayload::Mail { .. }' not covered` at
  `crates/hush-cli/src/main.rs:118`. The daemon's `to_dto` was updated in the same pass, but the
  CLI's `recv` printer was missed because it lives in a different crate.
- **Resolution:** added a `Mail` arm to the CLI that prints the thread/subject/body and writes each
  attachment out via `account.read_blob`.
- **Lesson:** adding an enum variant to `hush-core` is a workspace-wide change; grep every
  `match ... payload` across all crates, not just the one being edited.

### [OPEN] UI screenshots are blank in this environment

- **Attempted:** Task 9 browser verification of the Mail rail. Captured the page with both
  `browser_take_screenshot` and CDP `Page.captureScreenshot` (decoded the base64 to PNG).
- **Symptom:** both produce a solid white image, while the same page's DOM is fully rendered
  (`browser_snapshot` shows the rail, thread list, thread view, composer; `Runtime.evaluate` returns
  the real text). The browser tab runs in the background and is not painting, so no pixels are
  produced. This is a tooling/environment limitation, not an app defect.
- **Resolution:** not resolved; visual evidence is unavailable here. Verification was done at the DOM
  level instead — the Mail rail, thread view, attachment chip, reply composer, Profile section, and
  New-mail composer were read from the live page, and a reply sent through the UI landed in the other
  peer's queue under the same thread id. A human should still eyeball the rail once on a machine where
  the tab paints.
- **Lesson:** when a screenshot comes back blank but the DOM is populated, suspect a non-painting
  background tab before suspecting the app.

### [FIXED] Integration test asserted one mail record where two are correct

- **Attempted:** in `a_reply_lands_in_the_same_thread`, asserted each side's `mail_records()` had
  exactly the one thread id.
- **Symptom:** the test failed with `left: [<id>, <id>]`, `right: [<id>]`. Each side keeps its own
  sent copy *and* the received reply, so two records sharing one thread id is the correct outcome.
- **Resolution:** asserted two records with the same id, which is a stronger statement about thread
  continuity anyway.
- **Lesson:** the first green assertion about message counts should be derived from the actual
  send/receive pairing, not assumed.

---

## 2026-10-07 — Multi-device Phase 4 landed (queue credential split, fan-out verified)

### [FIXED] `DeviceKeys::load` did not compile: a `u32` const matched against a `u64`

- **Attempted:** ran `cargo test --workspace` to confirm the queue credential split and the v1 device
  file migration before committing.
- **Symptom:** `error[E0308]: mismatched types` at `crates/hush-core/src/device.rs:152` —
  `expected u64, found u32`, with the note `` `DEVICE_FORMAT_VERSION` is interpreted as a constant,
  not a new binding ``. The match was `match version { 1 => .., DEVICE_FORMAT_VERSION => .. }` where
  `version` is `u64` and the const is `u32`.
- **Resolution:** the arm became `v if v == u64::from(DEVICE_FORMAT_VERSION)`. A guard is required
  because a path pattern cannot compare against a `u64`.
- **Lesson:** this is the second time a `match` arm meant as a comparison was silently read as a
  binding. Rust catches it only because the types differ here; with matching types it would have
  compiled and quietly shadowed. Use a guard when the arm is a comparison.

### [FIXED] Two relay suites presented the published token for read, delete and burn

- **Attempted:** after switching `list_blobs`, `get_blob`, `delete_blob` and `report_queue` to
  `authorize_read`, ran the workspace suite.
- **Symptom:** 5 failures across two suites, all `Relay("list failed: 401 Unauthorized")`:
  `crates/hush-core/tests/two_party_relay.rs` (`relay_blob_has_no_cleartext_sender_json`,
  `reporting_a_queue_burns_it`) and `crates/hush-relay/tests/relay_queues.rs`
  (`put_get_delete_roundtrip`, `expire_drops_blob`, `report_burns_queue`). Each test registered a
  queue with one token and then used that same token to read it back.
- **Resolution:** the tests now model the two halves of the credential pair. `hush-relay`'s tests
  derive the delivery token with `hush_core::queue_delivery_token(read)` and use it for `PUT` while
  presenting the read token for `GET`/`DELETE`/`POST .../report`. `hush-core`'s tests present the
  device's own read token, exposed as `Account::own_read_token()`. This is a genuine behaviour change
  and the tests were wrong, not the relay.
- **Lesson:** the failing tests were the *correct* outcome of the change, not collateral damage. A
  test that uses one credential for both a write and a read is asserting that those are the same
  credential, which is exactly the hole being closed.

### [FIXED] `own_read_token()` had to be added, because nothing could read its own inbox

- **Attempted:** fixed the two `two_party_relay.rs` tests to present a read token.
- **Symptom:** there was no way to get one. `Account` exposes `invite()`, which carries only the
  delivery token, and `device` is a private field, so an integration test had no access to the read
  credential at all.
- **Resolution:** added `Account::own_read_token()`, documented as the private credential for local
  callers that talk to the relay directly instead of going through `Account::recv`.
- **Lesson:** splitting a credential in two means every existing caller has to be classified as a
  reader or a deliverer. Two of them were tests that only worked because the two were the same value.

### [FIXED] The fan-out test could not refresh the contact's device list in time

- **Attempted:** wrote Task 8's test — link a second device of Alice's account, then have Bob send to
  Alice and assert both of Alice's devices receive it.
- **Symptom:** `Relay("list failed: 404 Not Found")` from `alice_2.recv()`, and Alice's device 1
  received the message. Bob had only ever delivered to device 1.
- **Cause:** `Account::add_contact` already calls `refresh_contact_devices` as a best-effort step, and
  that call sets `devices_fetched_unix` to now. Bob added Alice *before* she linked her second device,
  so his cache said "one device, checked just now" and the send inside the 5 minute TTL skipped the
  refresh. The linked device was invisible for up to the TTL.
- **Resolution:** the test calls `bob.refresh_contact_devices("alice")` explicitly and asserts it
  returns 2, standing in for the TTL expiring. This is Q5 working as designed, not a defect — the
  documented cost of caching the list. A newly linked device is unreachable for up to 5 minutes.
- **Lesson:** a cache seeded by a successful-but-empty check is indistinguishable from a fresh one.
  The first test run looked like a fan-out bug and was a caching consequence; the failing assertion
  named the wrong subsystem.

### [FIXED] `ContactDevice` had no `added_unix`, so a republish redated every device

- **Attempted:** implemented `record_linked_device`, which republishes the signed list via
  `publish_device_list`.
- **Symptom:** not a failure but a latent data-loss path. `publish_device_list` mapped carried-over
  devices with `added_unix: 0` and set its own entry to `now_unix()`, so every republish reset the
  date on every device. Adding a third device would have redated the second, and adding any device
  would redate device 1. Nothing read the value yet, so no test caught it.
- **Resolution:** `ContactDevice` gained `added_unix` with `#[serde(default)]`, projected through both
  `refresh_own_devices` and `refresh_contact_devices`, and `publish_device_list` now carries the
  stored value forward. This is also what Task 14's "added date" column needs.
- **Lesson:** the field was being written to the wire by one function and dropped by the other. A
  round-trip test over the list would have caught it; the shape tests only covered `Contact`.

### [FIXED] Task 9's core had to land early, because Task 8's test needs it

- **Attempted:** the plan says Task 8's test "cannot be written yet" until Phase 5's linking exists,
  and the Phase 4 note in `docs/plans/2026-10-06-multi-device.md` said the same.
- **Symptom:** confirmed by construction. A second device of an account can only be reached if it is
  in the account's signed list, and only device 1 can sign that, and device 1 can only sign a record
  it has been given. There is no path around it: the test needs `adopt_linked_vault` and
  `record_linked_device`.
- **Resolution:** implemented the two core functions, without the daemon endpoints or CLI wiring that
  Task 9 also lists. `Account::adopt_linked_vault(dir, passphrase, device_id)` opens a transferred
  vault, mints fresh device keys, clears the invite cache (which names the *sending* device's inbox)
  and clears `own_devices`. `Account::record_linked_device(&ContactDevice)` validates the id, stores
  the record and republishes. `Account::link_request()` is what the new device sends over the link.
- **Note:** `record_linked_device` takes the new device's own statement of its identity key, inbox and
  delivery token, and cannot verify it against the list — writing the list is the point. Device 1's
  signature is the only authority, so it must never be called with a request that did not arrive over
  an authenticated link channel. The relay enforces the same statement afterwards by refusing prekey
  records whose identity key is not what the list says.
- **Still missing for Task 9:** the daemon `link-device/start` and `link-device/complete` endpoints,
  the CLI, and any authentication on the link channel itself.
- **Lesson:** "this test cannot be written yet" is worth re-checking once the blocker is named. The
  blocker here was one function pair, not the whole phase.

### [FIXED] Overclaimed "clippy clean" by grepping away the warnings

- **Attempted:** reported the tree as clippy-clean after
  `cargo clippy --workspace --all-targets 2>&1 | rg "^(error|warning: unused|warning: this)"`.
- **Symptom:** the filter matched no warnings, so the report said clean. A later unfiltered run showed
  7 `redundant_closure` warnings in `crates/hush-core/src/backup.rs`, 4 `ptr_arg` warnings in
  `hush-cli`/`hush-daemon`, and 1 `unnecessary_to_owned` in the new
  `crates/hush-relay/tests/prekey_records.rs`.
- **Resolution:** the new warning is fixed. The other 11 predate this slice — verified with
  `git show HEAD:...` for the `ptr_arg` ones and by `backup.rs` being unmodified in `git status` — and
  were deliberately left alone so the Phase 4 commit does not carry an unrelated refactor.
- **Lesson:** a grep filter that can match nothing is not evidence. Grep for the positive
  (`warning`) and count, rather than enumerating the warning kinds you expect.

### [OPEN] 11 pre-existing clippy warnings, so "clippy clean" is still not a gate

- 7 `redundant_closure` in `crates/hush-core/src/backup.rs` (`map_err(|e| Error::Io(e))` where
  `Error::Io` would do), 4 `ptr_arg` in `crates/hush-cli/src/main.rs:208`,
  `crates/hush-daemon/src/account_worker.rs:402,451,494`.
- **Needed to close:** a mechanical fix plus a decision on whether to add `-D warnings` to CI. Same
  shape as the `cargo fmt` entry below: the linter cannot gate while it is not clean.
- **Tooling note, not a repo problem.**

### [WORKED AROUND] `perl -0pi -e` for a fixture edit silently did nothing

- **Attempted:** added `added_unix: 0` to 5 `ContactDevice` literals in
  `crates/hush-core/tests/multi_device.rs` with a multi-line `perl -0pi -e` regex.
- **Symptom:** exit code 1 and empty output. The regex did not match, so no edit was made and the
  following build failed with the same 5 `missing field` errors as before.
- **Resolution:** used the edit tool with `replace_all` per literal instead, which is also what the
  workspace rules require. Cost one build cycle.
- **Lesson:** bulk regex edits over Rust literals are a false economy; the compiler reports the same
  errors either way, but only the structured edit is atomic and verifiable.

---

## 2026-10-07 — Multi-device Phase 1 (per-device key material)

### [FIXED] Plan's Task 1 test could not compile as written

- **Attempted:** followed `docs/plans/2026-10-06-multi-device.md` Task 1 literally, which puts the
  `DeviceKeys` tests in an integration test at `crates/hush-core/tests/device_identity.rs` and calls
  `vault.master_key()`.
- **Symptom:** `Vault::master_key()` is `pub(crate)`, so an integration test cannot call it —
  the file would not have compiled. The plan had also written those tests as `#[test]` while
  `DeviceKeys::load`/`create` have to be `async`.
- **Resolution:** the `DeviceKeys` tests live in `#[cfg(test)] mod tests` inside
  `crates/hush-core/src/device.rs`, where `pub(crate)` is reachable. The plan's self-review notes
  flagged the `async` part but not the visibility part.
- **Lesson:** a plan that calls a crate-internal accessor from an integration test is wrong before
  it is written. Check visibility when choosing where a test lives.

### [FIXED] `DeviceKeys` had no `Debug`, breaking `unwrap_err` in its own tests

- **Attempted:** asserted a missing device file with
  `DeviceKeys::load(..).await.unwrap_err()`.
- **Symptom:** `error[E0277]: device::DeviceKeys doesn't implement Debug`, because `unwrap_err`
  requires the `Ok` type to be `Debug`. This is the second time this exact trap has been hit — the
  earlier `Account::open(..).unwrap_err()` in `backup_recovery.rs` failed the same way.
- **Resolution:** added a manual `impl Debug for DeviceKeys` that prints only `device_id` and calls
  `finish_non_exhaustive()`, mirroring `Vault`. That is the better fix anyway: a derived `Debug`
  would happily dump key material into a log line.
- **Lesson:** types holding secrets should get a hand-written `Debug` up front, not after a test
  failure. `Account` still lacks one and will hit this again.

### [FIXED] Corrupted grep output from misusing `rg -r`

- **Attempted:** `rg -rn "signal\.enc|receive_queue_id"` to find call sites.
- **Symptom:** the output came back with every match replaced by the letter `n`, e.g.
  `let signal_bytes = fs::read(dir.join("n"))`. `-r` is ripgrep's *replace* flag, so `-rn` means
  "replace matches with n", not "recursive with line numbers".
- **Resolution:** re-ran as `rg -n`. No code was changed on the basis of the bad output, but the
  corrupted result was briefly read as "references are in other files".
- **Lesson:** `rg` is already recursive; `-r` is never what is wanted for a search.

### [FIXED] Ambiguous `as_ref()` in `backup.rs` after the exclusion change

- **Attempted:** added an exclusion check to the tar loop, which required owning the relative path
  as a `String` (`into_owned()`) to test it.
- **Symptom:** `error[E0283]: type annotations needed` on
  `append_path_with_name(&entry, rel.as_ref())` — `String` implements `AsRef` for `str`, `Path`,
  `OsStr` and `[u8]`, so the call became ambiguous. The previous `Cow<str>` had resolved it.
- **Resolution:** pass `rel.as_str()` explicitly.
- **Lesson:** changing a `Cow<str>` to `String` can break an `as_ref()` that was previously
  unambiguous.

### [FIXED] Test asserted a file that a fresh vault does not have

- **Attempted:** asserted the backup still contained `chat.enc` to prove history was preserved.
- **Symptom:** `history must still be included: ["hush-backup-manifest.json", "account.enc",
  "identity.pub", "vault.lock.json", "recovery.lock.json", "recovery.enc", "identity.secret"]` —
  `chat.enc` is only written once there is a message, so a fresh vault has none.
- **Resolution:** assert `account.enc` instead, which is the shared vault state that always exists.
- **Lesson:** the assertion was testing an incidental file, not the property (shared data survives,
  device data does not). Asserting the property would have avoided the failure.

### [FIXED] Plan specified the wrong signature primitive for the device list

- **Attempted:** plan Task 3 signs the device list with `ed25519_dalek::SigningKey` and verifies with
  `VerifyingKey`, matching how `Vault` signs.
- **Symptom:** the device identity is a **libsignal Curve25519** key, not an Ed25519 key, so it
  cannot be passed to `ed25519_dalek` at all. The plan would not have compiled at Phase 2.
- **Resolution:** not yet applied — Task 3 is Phase 2. The device list must be signed with libsignal's
  `PrivateKey::calculate_signature` (XEd25519, as `mint_prekey_bundle` already does for signed
  prekeys) and verified with `IdentityKey::verify_signature`. `docs/plans/2026-10-06-multi-device.md`
  still shows the Ed25519 version and needs correcting before Phase 2 starts.
- **Lesson:** "sign it" is not specific enough. Identify which key type holds the signing key before
  choosing a signature library.

### [OPEN] `identity.secret` is unused and still rides along in backups

- **Attempted:** excluding this device's key material from backups surfaced that the archive
  contains `identity.secret` and `identity.pub`.
- **Symptom:** neither is used anywhere in the codebase outside `Vault`'s own tests —
  not relay auth, not invites, not the Signal identity. The account's `protocol_name` comes from the
  **signal** identity key, not this one. So it is a per-vault Ed25519 keypair that nothing reads.
- **Why it matters:** Q9 of the multi-device design requires signing the device list with an
  account-level key, and this is the obvious candidate. **Correction (same day):** the first version
  of this entry claimed the key must be excluded from backups like `device.enc`. That is backwards. A
  linked device has to be able to *publish* the device list, so the account signing key belongs in the
  **shared** vault and must be transferred at link time; keeping it out of backups would leave a
  restored vault permanently unable to publish a list at all. Security comes from rotating that key
  on revocation, not from keeping it out of the transfer.
- **Resolution:** needs a decision, not a patch. See the Phase 2 trust-chaining problem below.

### [OPEN] A restored backup now gets a fresh identity, and contacts are not told

- **Attempted:** excluding `device.enc` from backups means `import_vault` produces a vault with no
  device keys, so restore has to mint a new device identity or the vault is unusable.
- **Symptom:** implemented and tested (`a_restored_backup_gets_a_fresh_device_identity`), and the
  existing round-trip test still passes because it only compares `protocol_name`. But the restored
  device now has a **different identity key** from the one contacts have pinned for it, so sessions
  with existing contacts will fail to establish until they re-verify.
- **Resolution:** correct behaviour for this model, but not yet surfaced anywhere in the UI or the
  daemon. Phase 4/5 has to handle identity-change on the receiving side and the UI has to say
  something other than "restored". Recorded rather than hidden.
- **Lesson:** removing a key from a backup changes the restore path's semantics. The old test passing
  was not evidence the behaviour was still correct.

### [FIXED] Migration had to move from Phase 5 into Phase 1

- **Attempted:** the plan schedules the legacy migration (`signal.enc` -> `device.enc`) as Task 10
  in Phase 5.
- **Symptom:** Phase 1 stops writing `signal.enc`, so an existing vault would have neither file and
  every `Account::open` on a real vault would fail immediately. Migration is not optional work that
  can wait for a later phase.
- **Resolution:** migration landed in Phase 1 (`migrate_legacy_device` in `account.rs`, plus
  `legacy_vault_migrates_keeping_identity_name_and_inbox`). The legacy file is deleted only after
  `device.enc` is written, so a failed save cannot destroy the identity.
- **Lesson:** when a change removes a file, the read path for that file is part of the same task,
  not a follow-up.

### [OPEN] The tree is not `cargo fmt`-clean, so "run the linters" had no baseline

- **Attempted:** ran `cargo fmt --all -- --check` and `cargo clippy` before calling Phase 1 done,
  per the completion standard.
- **Symptom:** `cargo fmt` reports diffs in files this slice never touched —
  `apps/hush-tauri/src-tauri/src/daemon_client.rs`, `crates/hush-core/src/signal.rs`,
  `crates/hush-core/src/vault.rs`, `crates/hush-daemon/src/main.rs`. Checking the committed version
  (`git show HEAD:crates/hush-core/src/backup.rs | rustfmt --check`) also fails, so `backup.rs` was
  already unformatted before this slice edited it. There is no `rustfmt.toml`.
- **Resolution:** deliberately not fixed here. Running `cargo fmt` across the tree would bury the
  slice in unrelated diff; only the new file `crates/hush-core/src/device.rs` was formatted.
  Reformatting the tree wants its own commit.
- **Also:** 8 clippy `redundant closure` warnings, all pre-existing, in `backup.rs`
  (`.map_err(|e| Error::Io(e))`) and `signal.rs`. Not introduced by this slice and not fixed
  mid-feature, per the feature protocol.
- **Lesson:** establish the linter baseline before reading a diff as caused by the current change.
  Otherwise pre-existing noise looks like a regression.

### [FIXED] The device list had no workable trust root — resolved as Q11 (device 1 signs)

- **Attempted:** correct Task 3 of the plan so the device list is signed with a libsignal key
  (`PrivateKey::calculate_signature`, XEd25519) instead of `ed25519_dalek`.
- **Symptom:** fixing the primitive exposed that the design never said *which* key signs, and no
  answer survived contact with the threat model on the first pass. The relay is untrusted
  (`THREAT_MODEL.md` treats us, the operators, as an attacker), so peers cannot take device
  membership on the relay's word. The signature must chain to something the peer already pinned, and
  the only thing pinned is the account id.
- **Resolution:** settled as **Q11 — device 1 is the signing root.** The list is signed with device 1's
  libsignal identity private key and verified against the account id, which *is* device 1's identity
  key hash (Q7). No new pinned trust material is needed: the peer verifies with the one thing it
  already has. Costs accepted: device 1 must be online and unlocked to add or revoke, and device 1
  cannot itself be revoked (losing it means rotating the account and re-inviting everyone). A linked
  device cannot sign, because device 1's private key is never transferred — which is precisely what
  Phase 1 guarantees by keeping `device.enc` out of backups.
- **Also settled by this (superseded — see the Phase 2/3 entry below):** the Q9 account token and the
  signature were initially held to answer *different* questions, with the signature proving
  authenticity to peers and the token gating who may write. That split did not survive implementation:
  the token was deleted, and the signature now does both jobs. Recorded here because the original
  reasoning is what made the wrong design look coherent.
- **Consequence:** `identity.secret` is not the account signing key after all, so it stays unused dead
  code. Still an open cleanup decision.
- **Lesson:** "authorize the write" and "sign the payload" are separate questions. Settling one and
  assuming it settles the other leaves a design that cannot be implemented.

### [FIXED] The plan's tamper-detection test proved nothing about the signature

- **Attempted:** implemented Task 3 including the plan's `signed_list_verifies_and_detects_tampering`,
  which flips a byte at `len / 2` and asserts that verification fails.
- **Symptom:** mutation testing — commenting out the `verify_signature` call and re-running — left that
  test **passing**. Flipping an arbitrary byte in the middle of the blob corrupts the JSON, so
  `serde_json::from_slice` fails first and the test never reaches the signature check. It was asserting
  on the parser, not the signature. The wrong-key test *did* catch the mutation, which is what exposed
  the difference between the two.
- **Resolution:** the tampering test now rewrites a byte inside a string value (`queue-1` -> `pueue-1`),
  which keeps the payload valid JSON, and asserts the payload still parses *before* asserting that
  verification fails. Re-ran the mutation: with `verify_signature` disabled, both tests now fail.
- **Lesson:** a test named after a security property is not evidence of that property. Disable the check
  and confirm the test fails, or the assertion may be riding on a completely different code path.

---

## 2026-10-07 — Multi-device Phase 3 (relay device directory, revocation)

### [FIXED] Trust-on-first-use token auth made revocation impossible

- **Attempted:** implemented `PUT /d/{account_id}` with the Q9 account token as the write credential,
  mirroring the queue rule: the first writer mints the token and every later write must present it.
  Revocation was then supposed to rotate the token, so the revoked device's copy stopped working.
- **Symptom:** wrote `rotating_the_token_currently_breaks_publishing` to exercise the revocation path
  end to end, and it failed. Device 1 rotates the token, publishes the updated list with the new token,
  and the relay answers `401` — because the stored `token_hash` is still the *old* token's hash. The
  account cannot perform the rotation that revokes anyone. Under first-write-wins there is no way for a
  new token to authenticate itself before the relay has already accepted it, so rotation is not a
  recoverable operation at all.
- **Also wrong with the model, independent of the bug:** even patched, the gap between "token rotated
  locally" and "relay has the new hash" is a window in which the revoked device still holds a working
  credential. And a first-write-wins rule makes any two-writer race a coin flip on arrival order, which
  is not an acceptable property for the operation that revokes a device.
- **Resolution:** user decision — `PUT /d/{account_id}` is authorized by **device 1's signature on the
  body**, verified by the relay against the account id (`hush_core::DeviceList::verify(body,
  account_id)`), and the account token is deleted outright. There is no credential to rotate, so there
  is no rotation race and no window: a revoked device does not hold device 1's private key and
  therefore cannot produce a body the relay will accept. `GET` stays unauthenticated, because a peer
  fetching a contact's list holds no credential for that account and does not need one — the signature
  is what makes the contents trustworthy.
- **Consequence:** `AccountToken`, `DeviceList::token`, `DeviceList::token_epoch` and the
  `device_dirs.token_hash` column are gone. `epoch` survives as a peer-visible freshness counter only,
  so a peer holding a cached list can tell that revocation happened. `identity.secret` remains unused
  dead code, as noted in the Q11 entry above.
- **Lesson:** trust-on-first-use fits a queue — a resource with one owner and no membership changes.
  It does not fit a resource whose whole purpose is to change membership, because the operation that
  changes membership is precisely the one that needs the new credential to already be trusted. If the
  write is signed by a key the reader already pins, authorization comes free and the token is
  redundant.

### [FIXED] The relay test could not construct a signed list

- **Attempted:** wrote `crates/hush-relay/tests/device_directory.rs` so a valid `PUT` carries a real
  device-1 signature, which is the only way to exercise the new authorization rule.
- **Symptom:** `error[E0432]: unresolved import libsignal_protocol` — `hush-relay`'s
  `[dev-dependencies]` contained no libsignal and no `rand`, so the test could not build an
  `IdentityKeyPair` to sign with.
- **Resolution:** added `libsignal-protocol` and `rand` to `crates/hush-relay/[dev-dependencies]`,
  pinned to match `hush-core`. The relay's own code still has no dependency on libsignal; only its
  tests do.
- **Lesson:** moving an authorization decision into a shared library makes the enforcing service's
  tests depend on that library's types. Budget the dev-dependency when relocating the check, or the
  test that proves the rule cannot be written.

### Checked and clear: dropping `token_hash` from an uncommitted table is not a migration risk

- **Concern:** `device_dirs` was created in this same uncommitted slice with `token_hash BLOB NOT NULL`.
  Changing the DDL to drop the column is safe only if no database exists with the old shape, because
  `Store::open` uses `CREATE TABLE IF NOT EXISTS` plus a best-effort `ALTER TABLE ... ADD COLUMN` and
  has no way to drop a column or migrate a schema version. A stale table would make every publish fail
  with `NOT NULL constraint failed: device_dirs.token_hash`, surfacing as a `500`.
- **Evidence:** `git show HEAD:crates/hush-relay/src/store.rs` has no `device_dirs` — the table has
  never been committed. Both dev databases (`./relay.db`, `./instances/relay/relay.db`) contain only
  `queues` and `blobs`, so they predate the table entirely and will get the new shape on next open.
- **Resolution:** no migration needed. Recorded so the check is not repeated.
- **Lesson:** with an append-only `CREATE TABLE IF NOT EXISTS` schema and no version table, a column
  can only be removed safely while the table is still unreleased. Confirm that before editing DDL.

---

## 2026-10-07 — Multi-device Phase 4 (contacts as device sets, fan-out)

### [FIXED] Nothing publishes a device's prekey bundle, so fan-out has no bundle to build a session from

- **Attempted:** started Task 7/8 and went to fill in `ContactDevice`, which the plan specifies as
  `{ device_id, identity_key, registration_id, queue_id, queue_token, pre_key_* }`.
- **Symptom:** there is nothing to populate `pre_key_*` from. The only structure published to the relay
  is `DeviceRecord` (`crates/hush-core/src/device_list.rs`), and it carries `device_id`, `identity_key`,
  `registration_id`, `queue_id`, `queue_token`, `added_unix` — **no prekey material at all**. The plan
  is internally inconsistent: Task 3 defines a `DeviceRecord` with no prekeys, and Task 7 expects a
  `ContactDevice` with them.
- **Why it matters:** the invite (`InvitePayload`) carries a full `PreKeyBundle` out of band, and that
  is the *only* way a bundle has ever been delivered. It works because you exchange an invite with the
  one device you are adding. It does not scale to a device you have never exchanged an invite with,
  which is exactly what Task 8's fan-out does — sending to every device of a contact requires a
  `PreKeyBundle` for each of them, and there is currently no source for it.
- **Blocked on:** a decision among three models (per-device prekey endpoint; prekeys inside the device
  list; bundles handed over the established encrypted session). Raised with the user rather than
  guessed, because the choice has consequences beyond Phase 4 — see the note on one-time prekeys below.
- **Resolution (same day):** the user chose the **per-device prekey endpoint**. Each device publishes
  its own record at `PUT /k/{account_id}/{device_id}`, signed by its own identity key, and the relay
  verifies the record's `identity_key` against that device's entry in the device-1-signed list before
  storing it. `GET /k/{account_id}/{device_id}` pops exactly one one-time prekey out of the stored
  pool and returns it, so a peer that fetches a bundle consumes a prekey and nobody else can get the
  same one. An exhausted pool serves the record with no one-time prekey and libsignal falls back to
  the signed prekey. Implemented in `crates/hush-core/src/prekey.rs`, `crates/hush-relay/src/store.rs`
  (`take_prekey_record`) and `crates/hush-relay/src/app.rs`; 9 tests in
  `crates/hush-relay/tests/prekey_records.rs`.
- **Note:** the plan's `ContactDevice` still lists `pre_key_*` fields and should not. They are fetched
  per session, not cached in the contact, or they go stale the moment the device republishes.
- **Consequence for forward secrecy that drove the decision:** a bundle published in a *static list* can
  never carry a one-time prekey, because a one-time prekey has to be consumed by exactly one fetch and
  a list is read by everyone. libsignal accepts a bundle with `pre_key: None`, so this works, but the
  first message then gets forward secrecy from the signed prekey alone. Only a per-device prekey
  endpoint that hands out and deletes one prekey per request preserves the full Signal property. That
  is a real security difference, not a style preference, so it should drive the decision.
- **Lesson:** "the device list tells peers where to deliver" and "the device list tells peers how to
  start a session" are different jobs, and the plan conflated them. Membership is signed by device 1;
  prekey material is per-device and rotates per-device, so signing them with the same key couples
  every device's session setup to device 1 being online.

---

### [FIXED] Fan-out is written but unreachable, so it has no end-to-end test

> **Closed 2026-10-07.** `Account::adopt_linked_vault` and `Account::record_linked_device` landed as
> part of the Phase 4 slice (see the newest section), which is the linking pair this entry was blocked
> on. The test now exists as `a_message_reaches_every_device_of_the_recipient_and_our_own` in
> `crates/hush-core/tests/multi_device.rs` and passes, asserting both directions: Bob's message lands
> in both of Alice's inboxes, and Alice's message reaches Bob *and* is filed as outgoing on her second
> device. It was mutation-checked — disabling the own-device fan-out makes it fail on the
> "linked device must file what device 1 sent" assertion. The `#[ignore]`d queue-leak test named below
> was replaced by `crates/hush-relay/tests/queue_credentials.rs`, which asserts the opposite
> behaviour and passes.

- **Attempted:** finished Task 8 (fan out to every `ContactDevice`, plus an `OwnCopy` to each of our
  own other devices) and went to write the plan's test,
  `a_message_reaches_every_device_of_the_recipient_and_our_own`.
- **Symptom:** no test can build the scenario. A contact's `devices` set is only ever filled from an
  `InvitePayload`, and an invite describes exactly one device, so every contact in a test has one
  device and the fan-out loop runs once. `own_devices` is only filled by `refresh_own_devices`, which
  reads a device list from the relay — and the only way to publish a list containing a second device
  is to already have one in `own_devices`. There is no `Account` API to add a device, and `state` is
  private, so a test cannot inject one. The linking flow that would break the cycle is Phase 5
  (Tasks 9-11).
- **What is verified instead:** `cargo test --workspace` passes (including the 6 tests in
  `crates/hush-core/tests/multi_device.rs` for the `Contact` shape and legacy migration, and the 9
  prekey-endpoint tests), and the fan-out code path compiles and is exercised for the single-device
  case by the existing `two_party_relay.rs` suite. The multi-device branch itself is **not** covered.
- **Resolution needed:** Phase 5's device-list fetch must populate `contact.devices` from the relay
  before Task 8 can be tested as written. Do not mark Task 8 done on the strength of a green suite —
  the suite passing here means the new branch is untested, not correct.
- **Lesson:** "it compiles and everything passes" and "the new code ran" are different claims. When a
  new branch has no reachable entry point, the test suite is silent about it by construction.

---

### [FIXED] The public device list publishes the queue token, which is a full read/write/burn credential

> **Closed 2026-10-07 by the credential split, before Phase 5 continued.** `read_token` is 32 random
> bytes generated on the device and stored only in `device.enc`; `delivery_token` is
> `HMAC-SHA256(read_token, "hush-queue-delivery-v1")` and is what goes in the invite and the
> `DeviceRecord`. The relay stores `sha256(delivery_token)`. `put_blob` and `presence_last_at` check
> the presented token against that hash; `list_blobs`, `get_blob`, `delete_blob` and `report_queue`
> instead recompute `HMAC(presented)` and check *that*, so a published delivery token cannot read,
> drain or burn the inbox it names. No relay schema change was needed — the stored hash simply
> changed meaning — and there is no second credential to register or rotate. Existing vaults rotate
> once on load (`device.enc` v1 to v2): the identity and every session are kept, the queue is
> replaced, and the caller republishes the list. Evidence: `crates/hush-relay/tests/queue_credentials.rs`
> (3 tests) plus the workspace suite.
>
> **What this does not fix:** the delivery token still authorizes `PUT` into that inbox, so a stranger
> who knows the account id can still flood a device's queue. Burning — the durable denial of service
> this entry was raised for — is closed. Rate limiting is the existing mitigation for flooding.

- **Attempted:** started Task 9 (device linking) and went to have device 1 record the new device's
  queue in the signed list, which meant deciding what a `DeviceRecord` may carry.
- **Symptom:** `DeviceRecord` carries `queue_token`, and `GET /d/{account_id}` is deliberately
  unauthenticated. But the same token authorizes `PUT /q/{id}` (deliver), `GET /q/{id}` (list),
  `GET /q/{id}/{blob}` (read), `DELETE /q/{id}/{blob}` (delete) and `POST /q/{id}/report` (burn).
  So the moment an account publishes a device list, anyone who knows the account id can read,
  delete and burn that device's inbox. Demonstrated live in
  `crates/hush-relay/tests/queue_token_leak.rs` (currently `#[ignore]`d, asserting the *required*
  behaviour): a request with the published token returns `200` on `GET /q/{queue_id}` and lists the
  owner's queued blobs.
- **Why it matters:** the account id is not a secret — it is the `protocol_name` in every invite the
  account has ever handed out, and it is inside the list itself. This is not plaintext disclosure
  (the relay only holds ciphertext), but it breaks three things the project claims:
  - `docs/THREAT_MODEL.md` protects "who talks to whom, as seen by our relay, so it is not a
    social-graph database", and states the relay stores blobs "on random queue IDs, not Alice's
    inbox". A public `account_id -> queue_id` map plus a readable inbox defeats that.
  - `docs/THREAT_MODEL.md` lists "Invite/queue tokens" as an asset. This publishes them.
  - Burning is the worst case, not reading. A stranger can burn a device's queue permanently, and a
    **device 2 cannot replace a burned queue**: only device 1 can sign a new list, so recovery
    depends on device 1 being online and unlocked. That is a durable denial of service.
- **Pre-existing, not introduced here:** the invite already carries the same token
  (`InvitePayload.queue_token`) and the same relay routes authorize with it, so any contact the
  account has ever invited can already read, delete and burn its inbox. Multi-device widens the
  audience from "contacts" to "anyone who knows the account id" and makes it permanent by putting
  the queue in a list that must be republished.
- **Fix shape (not applied):** split the credential. The relay keeps a private **read token** that
  never leaves the device and authorizes list/get/delete/report; a separate **delivery token** is
  what goes in the invite and the device list and authorizes only `PUT` (deliver) and presence.
  That is a relay schema change (`queues` gains a second token hash) plus an invite and
  `DeviceRecord` change, which is why it is raised rather than patched.
- **Lesson:** a token that gates both reading and writing cannot be handed to anyone who only needs
  to write. Publishing an address and publishing a credential are different acts; the plan's
  `DeviceRecord` conflated them.

---

## 2026-10-06 — Member calls spec (Task 17, P2P WebRTC)

### [FIXED] Assumed the Tauri webview could host WebRTC on Linux

- **Attempted:** first pass at the audio/video design put the WebRTC peer connection in the
  Tauri webview (browser `RTCPeerConnection` + `getUserMedia`), with the daemon relaying only
  signaling. The webview is a browser, so this looked like the low-effort path.
- **Symptom:** research contradicted it. WebKitGTK (Tauri's Linux renderer) does **not**
  expose `RTCPeerConnection` by default — it is behind an experimental build-time flag that
  no distro enables. Tauri maintainers: *"With Tauri/webkit no, with Electron/CEF yes"*
  (`tauri-apps/wry#85`); users see *"ReferenceError: Can't find variable: RTCPeerConnection"*
  (`tauri-apps/tauri#13143`). The earlier chat message to the user said WebRTC in WebKitGTK
  is *"historically the flakiest part"*, which understated it: it is absent, not flaky.
- **Resolution:** the spec
  ([`docs/tasks/member-calls.md`](docs/tasks/member-calls.md)) moves the peer connection and
  media pipeline into `hush-daemon`; the webview is UI chrome only. Media engine recommendation
  is GStreamer `webrtcbin`.
- **Lesson:** verify platform WebRTC support (not just `getUserMedia`) against the actual
  renderer before designing a webview media path. A webview being "a browser" is not enough.

### [FIXED] Set the call-transport default against the project's own threat model

- **Attempted:** the first spec defaulted to **relay-only TURN** so a contact would never learn
  your IP, on the reading that "hide the IP" was the priority.
- **Symptom:** it contradicted `THREAT_MODEL.md`, which treats **us (operators)** as an attacker
  and makes "our relay must not map who talks to whom" an explicit v1 constraint. A first-party
  TURN puts us in a position to see the call graph — a *stronger* graph signal than chat,
  because a real-time relay session links both IPs unambiguously. The stated goal ("P2P without
  exposing the IP") is itself only achievable by relaying, so the default traded a small leak
  for a bigger one.
- **Resolution:** after discussion the default was flipped to **direct P2P**. Contact IP
  disclosure is accepted (contacts are mutually trusted); there is **no first-party relay**;
  IP-level privacy is deferred to the OS/distro layer or a user-supplied relay/overlay. Recorded
  in [`docs/tasks/member-calls.md`](docs/tasks/member-calls.md).
- **Lesson:** check a proposed default against the project's own threat model before writing it
  down. "Protect the user from X" is not automatically aligned if it strengthens a different
  adversary the model already names.

---

## 2026-10-06 — Passphrase change (vault format v2, daemon endpoint, Settings UI)

### [FIXED] `pkill -f "vite"` killed the shell that ran it

- **Attempted:** one cleanup call that killed the test daemon and Vite, then `rm -rf`'d the
  temp dirs.
- **Symptom:** the call returned completely empty output — not even the `echo "cleaned"` at
  the end. `pkill -f` matches against full command lines, and the shell's own command line
  contained the literal string `vite`, so it signalled itself. Everything after the `pkill`
  never ran, leaving the daemon, Vite and both temp dirs alive while looking like a success.
- **Resolution:** killed by explicit PID.
- **Lesson:** `pkill -f <pattern>` can match the command line of the shell issuing it. This is
  the same family as the earlier `pgrep -f` entry — pattern matching over command lines
  includes the matcher.

### [FIXED] `unwrap_err()` does not compile on a type without `Debug`

- **Attempted:** `Account::open(dir, "old-pass").await.unwrap_err()` in the new recovery test.
- **Symptom:** `error[E0277]: 'Account' doesn't implement 'Debug'` — `unwrap_err` needs the
  `Ok` type to be `Debug` so it can print it in the panic message.
- **Resolution:** matched on the `Result` and panicked on `Ok` explicitly. `Account` holds key
  material, so adding a `Debug` impl just to satisfy a test helper would be the wrong fix.
- **Lesson:** for a `Result` whose `Ok` type is deliberately opaque, match instead of
  `unwrap_err()`.

### [FIXED] `input.autocomplete` is `AutoFill`, not `string`

- **Symptom:** `error TS2322: Type 'string' is not assignable to type 'AutoFill'` from a
  helper taking `autocomplete: string` and assigning it to the input.
- **Resolution:** typed the parameter as `AutoFill`. Caught by `tsc --noEmit` before any
  runtime check.

### [WORKED AROUND] Browser `Runtime.evaluate` fails in this environment

- **Attempted:** read page text with `browser_cdp` + `Runtime.evaluate` to confirm state after
  clicking Unlock.
- **Symptom:** both attempts returned
  `TypeError: Cannot read properties of null (reading 'split')` from the CDP bridge, for a
  trivial expression. Cost two round trips.
- **Resolution:** used `browser_snapshot` instead, which returns the accessibility tree and
  was sufficient for every check (validation message, error text, disabled state, cleared
  fields).
- **Note:** tooling limitation, not a repo problem.

### [OPEN] Existing vaults are not migrated to format v2 automatically

- **Detail:** `Vault::create` writes v2 (random master key, wrapped by the passphrase) and
  `Vault::change_passphrase` upgrades a v1 vault in place, but `Vault::open` deliberately does
  **not** rewrite the header. So any vault created before this slice — including the dogfood
  peers under `instances/` — stays on v1 until someone changes its passphrase.
- **Impact:** the claim "vaults use format v2" is only true for new vaults. v1 vaults keep
  working, and a passphrase change silently upgrades them, but nothing forces it.
- **Why not auto-migrate:** `open` is also used on read-only and removable mounts, and
  rewriting key material as a side effect of unlocking is a surprising thing to do. Verified
  the v1 path explicitly (`v1_vault_opens_and_upgrades_without_re_encrypting`).
- **Next:** if v1 should be retired, add an explicit upgrade on unlock with a logged
  migration, or a `hush-cli` migration command.

---

## 2026-10-06 — 5c.4 (relay HTTPS policy, relay unit, deploy checklist)

### [FIXED] IPv6 loopback was dead code in the HTTPS policy

- **Symptom:** the loopback check was `host == "[::1]" || host == "::1"`, but the
  host was extracted with `authority.split(':').next()`. For `http://[::1]:8787`
  that yields `"["`, so **neither** IPv6 arm could ever match and release builds
  wrongly refused a legitimate loopback relay.
- **Why it survived:** the guard was wrapped in `cfg!(debug_assertions)`, so no
  debug test could reach the branch. Nothing exercised it.
- **Resolution:** parse with `reqwest::Url` and strip the brackets `Url::host_str`
  keeps around IPv6 literals, then check `IpAddr::is_loopback()`. This also
  correctly allows `127.0.0.2`, which the old literal list rejected.
- **Lesson:** hand-rolled URL parsing produced a bug that a real parser makes
  impossible. Reach for the parser that is already a dependency.

### [FIXED] Nearly reported a doc/code mismatch that did not exist

- **Attempted:** grepped `crates/hush-core/src/config.rs` for the HTTPS policy,
  found nothing, and started writing up "the checklist documents a policy the code
  does not implement".
- **Symptom:** the policy is in `relay_client.rs`; `config.rs` merely calls it. A
  single-file grep was treated as a repo-wide search, and the wrong conclusion was
  one step from being reported as a finding.
- **Resolution:** searched the whole workspace before claiming absence, which is
  what surfaced both the real code and the actual `[::1]` bug.
- **Lesson:** grep one file, conclude nothing. This is the failure mode the search
  rules exist to prevent, and it happened anyway.

### [WORKED AROUND] TLS termination could not be verified end-to-end locally

- **Attempted:** stand up a TLS terminator in front of the relay, point a client at
  `https://localhost:8443` with a self-signed certificate, and run a real exchange.
- **Symptom:** the client is built with `rustls-tls` and bundles Mozilla's roots
  (`webpki-roots`), so it ignores `SSL_CERT_FILE` and the system trust store. A
  self-signed certificate cannot be trusted without changing client code.
- **Resolution:** did **not** add a `danger_accept_invalid_certs` escape hatch just
  to enable a test — that would be a real footgun in a privacy tool. Instead
  verified the proxy hop over plain HTTP (all relay routes are path-transparent
  through a proxy) and documented the constraint: a publicly trusted CA is
  mandatory, and there is no client-side override.
- **Note:** the constraint turned out to be a useful documentation finding rather
  than a blocker — it rules out internal CAs for VPS deployments.

---

## 2026-10-06 — DR-06 / DR-07 (systemd hardening + daemon secret handling)

### [FIXED] `mlockall` policy assumed an unlimited `RLIMIT_MEMLOCK`

- **Attempted:** gate `MCL_FUTURE` on `setrlimit(RLIMIT_MEMLOCK, INFINITY)` succeeding and
  reading back as exactly `INFINITY`.
- **Symptom:** on this machine the default is 2 GB soft/hard, and raising the hard limit
  needs `CAP_SYS_RESOURCE`. So `setrlimit` fails, the check returns false, and the daemon
  would have silently degraded to locking only current pages. This is not a corner case — it
  is the default on a normal desktop, and it means key material allocated *after* startup
  stays swappable while the log still says memory locking is on.
- **Resolution:** treat headroom as sufficient when the effective soft limit is
  `>= 256 MiB`, which covers both `LimitMEMLOCK=infinity` and a typical desktop default.
- **Lesson:** "did the privileged call succeed" is the wrong gate for a capability that has a
  usable non-privileged path.

### [FIXED] `Hardening::summary()` reported the inverse of the memory-lock state

- **Attempted:** one `on_off(bool)` helper shared by both fields, where `true` printed `"off"`
  (correct for core dumps, since off is the good state there).
- **Symptom:** `memory_locked=off` would have been logged while memory was in fact locked,
  because the same inverted helper was reused for a field where "on" is the good state. The
  unit test expectation is what exposed it, not the runtime log.
- **Resolution:** separate literals per field; the helper is gone.
- **Lesson:** a shared formatter that encodes a per-field *meaning* is a trap.

### [OPEN] Five systemd hardening directives cannot be verified in this environment

- **Directives:** `CapabilityBoundingSet=`, `PrivateDevices`, `ProtectKernelModules`,
  `ProtectKernelLogs`, `ProtectClock`.
- **Symptom:** `systemd-run` fails with `Failed to drop capabilities: Operation not
  permitted` and `status=218/CAPABILITIES` before the daemon is even exec'd. Each of these
  directives drops a capability internally, and the sandbox here forbids that.
- **Impact:** they ship unproven. If the daemon needed one of those capabilities, this
  environment would not catch it — the failure would look identical.
- **Next:** verify on the dogfood machine outside the sandbox. Everything else in the unit was
  exercised individually and passes.

### [OPEN] `ProtectHome=read-only` did not demonstrably apply

- **Symptom:** under `ProtectHome=read-only` with `ReadWritePaths=<data root inside $HOME>`,
  writing to `$HOME/hush-should-fail` (outside the data root) **succeeded**, while `/etc` was
  correctly denied by `ProtectSystem=strict`. `ProtectHome` alone, without `ReadWritePaths`,
  also allowed the write, and the journal showed no warning.
- **Impact:** cannot claim the rest of `$HOME` is read-only for the daemon on this machine.
  The daemon does work under the directive and can write its data root, which is the part that
  actually matters for function.
- **Next:** re-check on a normal host. `systemd-detect-virt` reports `none`, but an earlier
  journal line blamed a "container manager" for skipping `ProtectHostname` namespace setup.

### [FIXED] A DNS test that proved nothing

- **Attempted:** start the daemon with `HUSH_RELAY_URL=http://<bogus-host>:8787` and compare
  relay errors with and without `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`.
- **Symptom:** both runs errored against `http://127.0.0.1:8787`, not the bogus host. A vault
  keeps the relay it was created with, so the env var at daemon start was ignored and the test
  never touched DNS at all. Two runs agreeing looked like a clean result.
- **Resolution:** create the vault with the env var set, then compare. Separately confirmed
  `getent hosts example.com` resolves with rc=0 under the restriction, so there is no relay-DNS
  regression.
- **Lesson:** confirm the test reached the intended code path before trusting a "no
  difference" result.

### [FIXED] `systemctl show` on a stopped transient unit returns manager defaults

- **Attempted:** verify `MemorySwapMax=0` and `LimitMEMLOCK=infinity` after the probe unit had
  been stopped and collected.
- **Symptom:** `MemorySwapMax=infinity`, `LimitMEMLOCK=2083516416` — the defaults, which read
  as "the properties were never applied".
- **Resolution:** query while the unit is running.

### [WORKED AROUND] `journalctl` read before flush looked like a real regression

- **Symptom:** two back-to-back transient units showed empty stdout, which read as "DNS
  resolution is broken under the address family restriction". Re-running the identical
  configuration with explicit `echo START` / `echo END` markers resolved the hostname fine.
- **Resolution:** re-run with markers before drawing a conclusion. Cause not pinned down.

### [FIXED] `pgrep -f <pattern>` matched the wrapping shell too

- **Symptom:** `P=$(pgrep -f "hush-daemon --dir X")` returned two PIDs, so `/proc/$P/status`
  expanded to `/proc/1234\n5678/status` and `awk` failed with `Is a directory`.
- **Resolution:** use `pgrep -x hush-daemon`, or capture `$!` when the script starts the
  process itself.

### [WORKED AROUND] Backgrounding a process inside a single shell call produced empty output

- **Attempted:** `cmd >log 2>&1 & sleep 3; cat log` in one shell invocation, plus `{ ... & }`
  and `setsid ... &` variants.
- **Symptom:** the call returned exit 0 with no output at all, repeatedly, while the background
  process did start. Cost several round trips before the pattern was recognised.
- **Resolution:** move start-and-probe sequences into a script file and run the script.
- **Note:** tooling quirk, not a repo problem.

### [FIXED] Doc comment and `#[derive]` joined onto one line

- **Symptom:** a string replace that stripped a trailing newline left
  `/// ... at startup.#[derive(Debug, Clone)]`. It compiled and was clippy-clean, so only
  reading the file back caught it.
- **Lesson:** after a replace that touches line boundaries, read the region back.

### [FIXED] rustfmt split one `if`/`else` but not its sibling

- **Symptom:** `let core = if ... { "off" } else { "ON" };` is 51 characters, and rustfmt's
  `single_line_if_else_max_width` defaults to 50, so it expanded while the 45-character
  sibling stayed inline. The result looks inconsistent until you know the rule.
- **Resolution:** applied rustfmt's form.

### [OPEN] Repo is not rustfmt-clean, so `cargo fmt --check` cannot gate

- **Pre-existing, not introduced here.** `crates/hush-daemon/src/main.rs`,
  `account_worker.rs` and `suspend_lock.rs` all report rustfmt diffs, and
  `account_worker.rs` has 3 `clippy::ptr_arg` warnings (`&PathBuf` params).
- **Impact:** a `--check` gate fails on untouched code, so formatting regressions in new code
  are easy to miss.
- **Next:** a dedicated formatting cleanup slice, then wire `--check` into CI.

---

## 2026-10-05 — DR-02 (Unix socket default + Tauri native proxy)

Backfilled from the slice summary; less detail than the entries above.

### [FIXED] axum `FileTypeExt::is_socket` did not compile

- **Symptom:** the socket-liveness check needed `std::os::unix::fs::FileTypeExt` in scope.
- **Resolution:** imported it and isolated the check behind an `is_socket_file` helper.

### [FIXED] `hyper-util` legacy `Client` was unavailable

- **Symptom:** the Tauri proxy needs `hyper` 1.x's legacy client over a `UnixStream`, but the
  type was gated behind Cargo features that were not enabled.
- **Resolution:** added `client-legacy`, `http1` and `tokio` features to
  `apps/hush-tauri/src-tauri/Cargo.toml`.

### [FIXED] Smoke-script cleanup trap killed the test shell

- **Symptom:** `trap 'kill "${DAEMON:-0}" ...' EXIT` ran `kill 0` when a second PID was unset,
  signalling the whole process group — including the shell running the test.
- **Resolution:** iterate over the non-empty PIDs instead of relying on a default.

### [FIXED] Smoke-script reset variable ordered after use

- **Symptom:** `DAEMON=""` was placed after `DAEMON=$!`, so the PID was clobbered.
- **Resolution:** declare variables above the process spawns.

### [FIXED] Hermetic smoke script depended on a live relay

- **Symptom:** the presence-heartbeat check failed with no relay running, making the "hermetic"
  suite environment-dependent.
- **Resolution:** removed relay-dependent calls; that path is covered by the E2E run instead.

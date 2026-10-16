# Hush

Local-first, end-to-end encrypted communication. A vault on your own disk, a
daemon that is the only process holding the keys, a relay that only ever holds
ciphertext it cannot read, plus a CLI and a Tauri desktop UI.

## Prerequisites

| Tool | Why | Install |
|---|---|---|
| Rust (stable) | every crate in the workspace | https://rustup.rs |
| Node.js LTS + npm | the UI in `apps/hush-tauri` | https://nodejs.org |
| `protoc` | libsignal's build script compiles `protos/WhisperTextProtocol.proto`, so nothing depending on `hush-core` builds without it | `sudo apt install protobuf-compiler`, `brew install protobuf`, or see below |

On Linux the desktop shell and the voice path also need development packages:

```
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libappindicator3-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libssl-dev libasound2-dev patchelf build-essential
```

That is the list the release workflow installs (`.github/workflows/release.yml`)
plus `libasound2-dev`, which the daemon's audio capture needs and the release
build does not.

`protoc` alternatives, in the order `crates/hush-core/build.rs` looks: an
explicit `PROTOC=/path/to/protoc`, then a release unzipped into
`.tools/protoc-dist/`, then whatever is on `PATH`. `.tools/` is not in the
repository, so a fresh clone relies on the system copy. When `protoc` is missing,
`cargo build` and the run scripts print the install lines instead of failing
inside prost-build.

## Run it

One vault, one daemon, one window. With no vault on the device, the window opens
on a **Welcome** screen that creates one: it asks for the vault name, a passphrase
you choose, and the relay (prefilled with the shared one), then shows the recovery
code once. A fresh clone plus this command is a new user's first run — the same
first run a packaged build would have, with nothing pre-seeded.

```
./scripts/run-one-ui.sh
```

```
./scripts/run-one-ui.sh --relay http://host:8790      # relay a new vault is created against
./scripts/run-one-ui.sh --vault instances/me          # keep the vault somewhere else
./scripts/run-one-ui.sh --no-relay                    # never start a local relay
```

An existing vault is reused as it is, relay included, so the passphrase is asked
for once and later runs go straight to the window. Add a contact with the invite
the other person sends you; send yours back with **Copy my invite**.

Two peers on one machine, one local relay, both vaults created for you:

```
./scripts/run-two-ui.sh
```

It creates `instances/peer-a` and `instances/peer-b` (passphrases `peer-a-pass`
and `peer-b-pass`), starts a relay on `127.0.0.1:8787`, builds, and opens both
windows. It is safe to re-run: a relay or daemon that is already up is reused.

```
./scripts/run-two-ui.sh --dev     # Vite dev server, hot reload, one browser URL per peer
./scripts/run-two-ui.sh --relay http://relay.example:8787   # use a relay elsewhere; none is started
```

Both scripts install the UI's npm dependencies (`npm ci`) when `node_modules` is
missing, and stop with install lines when cargo, node, npm or protoc is absent.

The desktop window needs a graphical session. On a headless machine use `--dev`
and open the browser URL, or use the CLI.

### Without the UI

The window is the only place a vault is created interactively; the CLI does the
same thing for automation and for machines without a display:

```
cargo build -p hush-cli
cargo run -p hush-cli -- --dir instances/peer-a init --passphrase peer-a-pass --relay http://127.0.0.1:8787
cargo run -p hush-cli -- --dir instances/peer-a invite
cargo run -p hush-cli -- --help
```

## Test together, on two machines

Both sides point at the same relay, and the relay carries the mail; the two vaults
never talk to each other directly. Each side runs one command, and the first run
creates the vault it needs. The relay URL is printed by the first of these:

```
./scripts/relay-tunnel-url.sh              # prints the HTTPS relay that is live now

# one side: a new vault on the shared relay, then the window
./scripts/run-one-ui.sh --relay https://<tunnel-host>

# the other side: two vaults, both on the same relay
./scripts/run-two-ui.sh --relay https://<tunnel-host>
```

The second person needs the URL, not the script: `relay-tunnel-url.sh` reaches the
lab box over SSH, so whoever hands the URL out runs it.

Then exchange invites inside the app: **Copy my invite** on one side, **Add
contact** on the other. The relay address in the vault is what makes that work, so
the two runs have to name the same one.

The lab VPS relay is reachable two ways, and they are not equivalent:

| Relay URL | Build it needs |
|---|---|
| `https://hushrelay.space` — the production relay (Hetzner, ARM, Caddy TLS) | **release** (what a packaged build does) |
| `http://<tunnel-host>` — Cloudflare quick tunnel on the lab box, changes on restart | debug only |
| `http://137.184.129.205:8790` — the lab VPS, plain HTTP, no TLS | debug only |

`hush-core`'s `HttpPolicy` refuses a plain-HTTP relay that is not loopback or a v3
onion in a release build, so the plain-HTTP URLs force the run scripts to build the
daemon in **debug** and say so. The production relay has a publicly trusted
certificate, so a release daemon dials it with no client configuration — release
builds default to it, which is the path to test the way it ships.

The lab box's tunnel hostname is a `trycloudflare.com` name minted when
`cloudflared-hush-quick.service` on the box last restarted, so it changes and it
expires. `./scripts/relay-tunnel-url.sh` prints the one that is live now and checks
it; `--refresh` re-mints it if it has died. The durable alternatives — a Public
Hostname on the named `hush` tunnel, or TLS on the box — are in
`docs/ops/relay-lab-vps.md`. A vault stores the relay it was created against, so a
vault made against a dead hostname has to be repointed. Set `HUSH_CLOUD_RELAY_URL` to
move the default the scripts fall back to.

## Relays

The **production relay** is `https://hushrelay.space`, with the onion twin
`http://qvo7toe6urizihhbj7tp4ibsf2rhkilsrwvnkdk5jlwubjm3e7rnrlad.onion`
(release builds carry both as defaults; Tor relay mode is a per-vault switch and
fails closed when either half is missing). Deploy steps, the reboot-test record
and the onion key backup are in `docs/ops/relay-prod-vps.md`.

Each vault stores the relay it was created against and the daemon dials that one;
a loopback URL means a relay on this machine, which the run scripts start from
`crates/hush-relay`. A relay on another host is used as-is:

```
HUSH_RELAY_URL=http://relay.example:8787 ./scripts/setup-two-peers.sh
```

An existing vault keeps its stored relay: `--relay` on a run warns when the two
disagree rather than moving the vault, because the mailbox lives on the old one.
To move a vault deliberately:

```
cargo run -p hush-cli -- --dir instances/me relay set --url https://hushrelay.space
```

The lab relay (`http://137.184.129.205:8790`) and its onion address, with the
deploy steps, are in `docs/ops/relay-lab-vps.md`.

## Tor

Packaged Linux builds bundle Tor
(`apps/hush-tauri/src-tauri/resources/tor/`, x86_64). On macOS and Windows,
install Tor or point `HUSH_TOR_BIN` at one. The Relay panel reports which copy is
in use, and onion mail waits until there is one.

## Tests

`cargo test --workspace` builds the desktop app, and `tauri-build` refuses to
compile it without the daemon staged as a sidecar — the bundle carries the daemon,
so it is a build-time dependency of the app, not just a runtime one. Stage it once
after cloning; the run scripts do this for you.

```
./scripts/stage-daemon-sidecar.sh        # debug daemon, enough for tests
cargo test --workspace
(cd apps/hush-tauri && npm test)
```

`scripts/` also holds smoke harnesses for single subsystems (`smoke-mail.sh`,
`smoke-tor-relay.sh`, `smoke-relay-lab.sh`, `voice-dogfood.sh`, `paper-dogfood.sh`)
and they assume the same prerequisites as the run scripts.

## Layout

```
crates/hush-core      keys, vault, Signal sessions, payloads
crates/hush-daemon    the only process that unlocks the vault; local API for the UI
crates/hush-relay     the relay: ciphertext queues, no user names
crates/hush-cli       dogfood client, no UI required
crates/hush-wallet    BTC and ETH
crates/hush-atproto   the public lane
crates/hush-audio     Opus capture and playback for calls
apps/hush-tauri       desktop UI; talks only to the daemon
scripts/              run and smoke harnesses
docs/                 protocol, threat model, design, tasks, ops
```

## Documentation

- `docs/PROTOCOL.md` — wire protocol, and what the UI is allowed to call end-to-end encrypted
- `docs/THREAT_MODEL.md` — what the relay sees, what a compromised host sees
- `docs/DESIGN.md`, `docs/OPEN_QUESTIONS.md` — design decisions and what is still open
- `RELEASING.md` — bundles, updater manifest, signing

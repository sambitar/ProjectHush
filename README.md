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

## Install the desktop app

Release builds are published to `https://hushrelay.space/updates/` — served from the
relay so an installer can be downloaded without a GitHub account (see **Relays**). Linux ships as an
AppImage, which is the one artifact the updater can replace in place.

An AppImage installs nothing. A freshly downloaded copy therefore has no menu entry
and no icon the desktop can find: the `.desktop` file and the PNGs live inside the
file, and `AppRun` puts them on `XDG_DATA_DIRS` for the app's own processes only,
never for the shell that draws the launcher. Register it once:

```
chmod +x ~/Applications/Hush_0.5.0_amd64.AppImage
./scripts/install-appimage-desktop.sh
```

That copies the entry and the icons into `~/.local/share`, which is where the
launcher already looks, and points `Exec` at the AppImage file so the app keeps
updating itself in place. No root and no package are involved. `--dry-run` shows
what it would write; `--uninstall` reverses it.

Skipping this step is why the icon shows on one machine and not another. With no
desktop entry visible to the shell, KDE Plasma falls back to the window's own icon
on X11 and still shows the mark, while GNOME Shell has no such fallback and draws a
placeholder. On Wayland there is no window icon at all, so neither desktop shows it
until the entry is in place.

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
./scripts/run-one-ui.sh --vault instances/me          # keep the vault somewhere else
```

An existing vault is reused as it is, relay included, so the passphrase is asked
for once and later runs go straight to the window. Add a contact with the invite
the other person sends you; send yours back with **Copy my invite**.

Two peers on one machine, both on the production relay, both vaults created for you:

```
./scripts/run-two-ui.sh
```

It creates `instances/peer-a` and `instances/peer-b` (passphrases `peer-a-pass`
and `peer-b-pass`), builds, and opens both windows on `https://hushrelay.space`.
It is safe to re-run: a daemon that is already up is reused.

```
./scripts/run-two-ui.sh --dev     # Vite dev server, hot reload, one browser URL per peer
./scripts/run-two-ui.sh --relay https://hushrelay.space   # explicit; local relays are refused
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
cargo run -p hush-cli -- --dir instances/peer-a init --passphrase peer-a-pass --relay https://hushrelay.space
cargo run -p hush-cli -- --dir instances/peer-a invite
cargo run -p hush-cli -- --help
```

## Test together, on two machines

Both sides point at the production relay, `https://hushrelay.space`, and the relay
carries the mail; the two vaults never talk to each other directly. Each side runs
one command:

```
# one side: a new vault on the production relay, then the window
./scripts/run-one-ui.sh

# the other side: Alice and Bob, both on the production relay
./scripts/run-two-ui.sh --dev
```

Then exchange invites inside the app: **Copy my invite** on one side, **Add
contact** on the other. The relay address stored in the vault is what makes that
work, so both vaults must name `https://hushrelay.space`.

Local relays are not used in this project. The run scripts refuse a loopback
`--relay` or `HUSH_RELAY_URL`; automated tests start an in-process relay that never
touches the production host.

## Relays

The **production relay** is `https://hushrelay.space`, with the onion twin
`http://qvo7toe6urizihhbj7tp4ibsf2rhkilsrwvnkdk5jlwubjm3e7rnrlad.onion`
 (every build carries both as defaults; Tor relay mode is a per-vault switch and
fails closed when either half is missing). Deploy steps, the reboot-test record
and the onion key backup are in `docs/ops/relay-prod-vps.md`.

**Relay policy:** production only. If the relay needs to change, update
`hushrelay.space` itself (and its onion twin) rather than pointing a build at a
local or ad-hoc relay.

The same origin serves the releases: `https://hushrelay.space/updates/` holds the
installers and the updater manifest (`latest.json`) that installed copies poll.
That is not a convenience — the GitHub release page cannot hand an installer to
someone without a GitHub account, and the updater fetches without credentials. Publishing is
automatic when a release is published, via
`.github/workflows/publish-updates.yml`; the whole path is in `RELEASING.md`.

Each vault stores the relay it was created against and the daemon dials that one.
There is no local-relay path in the app or the run scripts:

```
HUSH_RELAY_URL=https://hushrelay.space ./scripts/setup-two-peers.sh
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

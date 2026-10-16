# Releasing Hush

Hush updates itself through the official Tauri updater plugin. Each release is a
GitHub Release built by `.github/workflows/release.yml`, and installed copies
poll `releases/latest/download/latest.json` for a newer version.

Two properties of that design drive everything below:

- The updater only trusts an artifact whose signature matches the public key
  compiled into the app. That key is a one-time decision: change it and no
  already-installed copy will accept anything you publish again.
- `releases/latest` ignores drafts. The workflow creates drafts on purpose, so
  nothing reaches users until you publish, and a half-built release cannot be
  picked up by mistake.

The desktop app lives in `apps/hush-tauri`, not at the repository root. Every
command below is written from the repository root unless it says otherwise.

---

## One-time setup

### 1. Point the app at your repository

`apps/hush-tauri/src-tauri/tauri.conf.json` names the repository that holds the
releases:

```json
"endpoints": ["https://github.com/sambitar/ProjectHush/releases/latest/download/latest.json"]
```

Already set for this repository. Change the owner and name only if you fork, and
change them before the first release: the endpoint is compiled into the binary, so
an installed copy keeps asking the old URL until it is reinstalled or updated from
it. Nothing else in that file needs to change.

### 2. Generate the signing key

```bash
cd apps/hush-tauri
npm run tauri signer generate -- -w ~/.tauri/hush.key
```

It prompts for a password (pick a strong one and store it in your password
manager) and prints the **public** key. The private key is written to
`~/.tauri/hush.key` and is never printed.

Do not commit the key, do not paste it into a chat, and do not put it in the
repository. `*.key` and `.tauri/` are already in `.gitignore` as a backstop, but
the intended home is `~/.tauri`, outside the tree entirely.

### 3. Paste the public key into the config

`npm run tauri signer generate` prints the public key and also writes it beside
the private key as `~/.tauri/hush.key.pub`, so it can be read from there instead
of copied out of the terminal. It goes where the placeholder was:

```json
"pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDY2ODlDQjdFOTU3MkM2RTQKUldUa3huS1Zmc3VKWmgyOGJaRkVlSFVtMXVNaTI3UW1udzJlVTVTT2tyd2krRWdOMXRkUGtXYjYK"
```

This is the only key that goes in the repository. It is a public key; publishing
it is what lets an installed copy verify your releases.

### 4. Add the repository secrets

The workflow reads two secrets. Add them in
**Settings → Secrets and variables → Actions**, or with the `gh` CLI:

```bash
# The whole contents of the key file, including any header line.
gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/hush.key

# The password you chose in step 2.
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD
```

The private key goes in the secret **value**, not as a file path — the runner
has no `~/.tauri`.

### 5. Back up the private key

Copy `~/.tauri/hush.key` and its password somewhere durable and offline. If you
lose them you cannot ship another update that existing installs will accept;
you would have to generate a new key pair and have every user reinstall by hand.

### 6. Icons

`src-tauri/icons/` carries `icon.icns` and `icon.ico` alongside the PNGs. They
were generated once from the 512×512 `icons/icon.png` with:

```bash
cd apps/hush-tauri
npm run tauri icon src-tauri/icons/icon.png -o /tmp/hush-icons
cp /tmp/hush-icons/icon.icns /tmp/hush-icons/icon.ico src-tauri/icons/
```

`bundle.icon` in `tauri.conf.json` names them. That list is load-bearing: Tauri
bundles the icons it is told about and no others, so an unset `bundle.icon` ships
iconless bundles on every platform and fails the AppImage build outright
("couldn't find a square icon to use as AppImage icon"). If you add a size,
add it to that list too.

Nothing to do at release time. Regenerate the same way only if the app icon
changes; the source is only 512×512, so a larger `icon.png` is worth adding
first if a crisper `.icns` is wanted.

### 7. Push the repository to GitHub

The repository has no remote configured yet, and `.github/workflows/release.yml`
only runs on GitHub. Create the repository, add it as `origin`, and push. The
`OWNER/REPO` you used in step 1 must match.

> **Exercised once, on 2026-10-16.** The first tag (`v0.1.0`) ran the workflow on
> real runners and settled what local checks could not: the action's argument
> forwarding, the nested `apps/hush-tauri` `projectPath`, the sidecar staging, and
> the platform keys in `latest.json` all work, and both macOS legs built and
> published. Four things it did not have right, all since fixed: the apt list
> named two conflicting appindicator packages; the Linux leg needed
> `libasound2-dev` for the daemon's audio dependency; `bundle.icon` was unset, so
> the AppImage refused to build and macOS had been shipping iconless; and the
> rust-cache key carried the comma in the Linux args, which rust-cache rejects, so
> that leg rebuilt libsignal from scratch on every run.
>
> The Windows leg is out of the matrix until the daemon has a Windows transport.
> Its socket path is `tokio::net::UnixListener`, which does not exist on Windows
> at all: the first run failed with `E0433`/`E0425` rather than the
> build-and-see this note used to predict. See "Platform notes".

---

## What a bundle carries

Two binaries: the app, and the daemon. `bundle.externalBin` names
`binaries/hush-daemon`, which Tauri resolves to a file carrying the target triple
— `binaries/hush-daemon-aarch64-apple-darwin` — and copies into the bundle beside
the executable (`Contents/MacOS/hush-daemon` in a `.app`).

Cargo names nothing that way, so `scripts/stage-daemon-sidecar.sh` builds the
daemon and copies it under the name Tauri looks for. This is not optional:
`tauri-build` refuses to compile the app when the file is missing —

```
resource path `binaries/hush-daemon-x86_64-unknown-linux-gnu` doesn't exist
```

— which makes the daemon a build-time dependency of the app, not just a runtime
one. The run scripts call the stager, the release workflow calls it once per
matrix leg, and a developer who only wants `cargo check -p hush-tauri` calls it
once by hand.

Because the bundle now carries the daemon, the workflow needs `protoc` on every
leg: the app does not link `hush-core`, the daemon does.

At runtime the app starts that daemon itself, but only when nothing else has:
`HUSH_API_URL` set — a run script, or a daemon started by hand — means someone
else owns the socket, and the app spawns the sidecar only when the environment
names no transport. That is why `scripts/run-one-ui.sh` behaves exactly as it did
before. The vault and the socket live under the app's local data directory
(`~/Library/Application Support/com.hush.capsule` on macOS), and the daemon is
killed when the app exits, so the vault lock is released for the next launch.

---

## Building locally

`npm run tauri build` needs the staged daemon before it will compile:

```bash
./scripts/stage-daemon-sidecar.sh --profile release
```

`npm run tauri build` now requires the signing key, because
`bundle.createUpdaterArtifacts` is on. Without it the build finishes the bundle
and then exits 1:

```
A public key has been found, but no private key.
Make sure to set `TAURI_SIGNING_PRIVATE_KEY` environment variable.
```

Export the key for local release builds:

```bash
export TAURI_SIGNING_PRIVATE_KEY="$HOME/.tauri/hush.key"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="your password"
cd apps/hush-tauri && npm run tauri build
```

The development flow is unaffected: `scripts/run-one-ui.sh` calls `cargo build`
directly, and cargo does not sign anything. Only `tauri build` does.

---

## Cutting a release

### 1. Bump the version

The version is written in three places, and the updater compares them, so they
have to agree:

| File | Why |
| --- | --- |
| `apps/hush-tauri/package.json` | what the JS tooling reads |
| `apps/hush-tauri/src-tauri/Cargo.toml` | what cargo stamps into the binary |
| `apps/hush-tauri/src-tauri/tauri.conf.json` | what the bundle reports to the updater |

Use the script rather than editing by hand:

```bash
node scripts/bump-version.mjs 0.2.0
node scripts/bump-version.mjs --check   # prints all three and fails if they differ
```

Then refresh `Cargo.lock`, which still records the old version:

```bash
cargo metadata --format-version 1 > /dev/null
```

Commit all four files together.

### 2. Tag and push

The tag is what starts the build. It must start with `v`, and the version in it
must match the files above — the updater compares them as strings:

```bash
git tag v0.2.0
git push origin v0.2.0
```

The workflow builds macOS (Apple Silicon and Intel) and Ubuntu in parallel and
attaches everything to a **draft** release named after the tag. There is no
Windows leg; see "Platform notes".

### 3. Publish the draft

Wait for all three jobs to finish, then check the draft before publishing:

```bash
gh release list
gh release view v0.2.0 --json assets --jq '.assets[].name'
```

Confirm `latest.json` is present and that it lists an entry for every platform
you intend to support (`darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`),
alongside the installers. A platform missing from `latest.json` is a platform
whose existing installs will never see the update. `windows-x86_64` will appear
there again once the Windows leg returns.

```bash
gh release edit v0.2.0 --draft=false
```

Publishing is the moment users start seeing the update. There is no staged
rollout: the next check every running copy makes will offer it.

---

## Testing an update locally

The updater replaces an installed bundle, not a development build. `cargo tauri
dev` and `scripts/run-one-ui.sh` run the binary straight out of `target/`, where
the updater has nothing to replace.

### The realistic test

1. **Install the old version.** Build and run the v0.1.0 release as a real
   install. On Linux, run the published AppImage — the updater detects it via
   the `APPIMAGE` environment variable and rewrites that file in place. Keep a
   copy of the AppImage somewhere you can compare against afterwards.

2. **Confirm it reports the old version.** Open **Settings → About**. The
   `Version` row must read `0.1.0`. If it does not, the version files disagree —
   run `node scripts/bump-version.mjs --check`.

3. **Publish the new version.** Bump to `0.1.1`, tag, push, wait for the build,
   and publish the draft (see above). Until it is published, the old copy sees
   nothing, which is worth verifying once: click **Check for updates** and
   confirm it says you are on the latest version.

4. **Trigger the update.** In the still-running 0.1.0 copy, click **Check for
   updates** in **Settings → About**. It should find `0.1.1` and show the
   release notes. Click **Update now**: the bar fills, the app restarts, and
   **Settings → About** now reads `0.1.1`.

5. **Confirm the silent path too.** Quit and relaunch the 0.1.1 copy, publish
   0.1.2, and leave the app alone. Within a few seconds of launch the update
   prompt should appear on its own, without touching anything.

### A faster loop, without GitHub

To test the plumbing without publishing anything, serve a manifest locally and
point the config at it:

```bash
# A directory holding latest.json and the .tar.gz/.sig the manifest names.
python3 -m http.server 8000
```

Set `plugins.updater.endpoints` to `["http://localhost:8000/latest.json"]`,
build, and run. Remember to put the GitHub URL back before releasing — the
endpoint is baked into the binary.

### What a failure looks like

- **Wrong public key or a tampered artifact:** the download completes and then
  fails its signature check. Nothing is installed and the app keeps running;
  the message says the update did not pass verification. This is the case worth
  testing deliberately, by signing with a different key.
- **Offline or a 404 endpoint:** the manual check reports the error; the
  launch-time check says nothing at all.
- **`Version` row empty in About:** the app is running outside the Tauri shell,
  or `getVersion()` failed. In the shell it should always be populated.

---

## Platform notes

- **Linux:** the updater picks its payload by how the running copy was
  installed. An AppImage rewrites itself in place and is the one Linux artifact
  that updates cleanly. A deb install asks for `linux-x86_64-deb` first, does
  not find it, falls back to `linux-x86_64` — the AppImage entry — and then
  refuses the download, because an AppImage is not a deb. So the `.deb` the
  workflow publishes is for first installs; point anyone who wants in-place
  updates at the AppImage.
- **Windows: not built.** The leg is out of the matrix because the daemon cannot
  be built for Windows yet. It listens on a Unix socket —
  `tokio::net::UnixListener`, which does not exist on Windows; the build fails
  with `E0433`/`E0425` in `crates/hush-daemon` — so a Windows bundle would have
  no daemon in it and the app would find nothing to start. The port is a Windows
  transport in the daemon (a named pipe, or loopback TCP), and the leg comes back
  with it. Everything below describes how Windows will behave once it does.
  Tauri generates updater artifacts for both the MSI and the NSIS installer, and
  the plugin looks for `windows-x86_64-nsis` or `windows-x86_64-msi` before
  falling back. The workflow built `--bundles nsis` so the release would carry a
  single Windows installer type and the manifest could not disagree with how a
  copy was installed. If you also want the MSI, add it to the matrix `args` and
  check which artifact `latest.json` ends up pointing at before publishing.
- **macOS:** the `.app.tar.gz` is what the updater downloads; the `.dmg` is for
  first installs. Builds are **unsigned** — no Apple certificate and no
  notarization — so Gatekeeper refuses the first launch. See the section below.
- **Signing keys are a pair.** The `pubkey` in `tauri.conf.json` and the
  `TAURI_SIGNING_PRIVATE_KEY` secret must come from the same `signer generate`
  run. Replacing one without the other makes every future update fail
  verification.

---

## Installing an unsigned macOS build

Nothing in the workflow signs with an Apple certificate, so a downloaded copy is
quarantined and the first launch is refused — "Apple cannot check it for
malicious software", or "is damaged and can't be opened". Both bypasses are on the
user's machine:

```bash
xattr -dr com.apple.quarantine /Applications/Hush.app
```

or, in Finder, right-click the app → **Open** → **Open** in the dialog, which is
offered only the first time.

To remove the warning properly: join the Apple Developer Program, add a Developer
ID certificate, and pass `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID` to
`tauri-action`. Until then the bypass is the documented path.

Two more things a first run on macOS needs:

- **The relay.** The compiled-in default is loopback `http://127.0.0.1:8787`, and a
  bundled copy has no run script to override it, so the **Relay** field on the
  Welcome screen is where the URL goes. The vault stores it from then on. A
  release daemon accepts HTTPS and a v3 onion address; it refuses plain HTTP that
  is not loopback. To skip the typing, launch from a terminal with the relay in
  the environment, which the daemon the app spawns inherits:

  ```bash
  HUSH_RELAY_URL=https://relay.example open -a Hush
  ```

- **Tor.** The bundled sidecar in `resources/tor/` is a Linux binary, so the Mac
  reports no Tor and onion mail waits. `brew install tor` or `HUSH_TOR_BIN` covers
  it; mail over an HTTPS relay does not involve Tor at all.

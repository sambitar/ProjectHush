# Releasing Hush

Hush updates itself through the official Tauri updater plugin. Each release is a
GitHub Release built by `.github/workflows/release.yml`, and installed copies
poll `https://hushrelay.space/updates/latest.json` for a newer version.

Three properties of that design drive everything below:

- The updater only trusts an artifact whose signature matches the public key
  compiled into the app. That key is a one-time decision: change it and no
  already-installed copy will accept anything you publish again.
- The update origin is the relay, not GitHub. The updater fetches `latest.json`
  and the bundles it names without credentials, and a GitHub release page cannot
  hand an installer to someone who has no GitHub account.
  `.github/workflows/publish-updates.yml`
  mirrors the manifest and every bundle it names to the relay after a release is
  published. The same directory is what makes the installers publicly
  downloadable.

  > This section used to justify the mirror by saying the repository was private
  > and GitHub 404s unauthenticated release downloads. The repository became
  > **public on 2026-10-18**, so that reason is gone; the reason above is the one
  > that still holds. Do not repeat the private-repo version.
- The build creates a **draft** release, and nothing is mirrored until that draft
  is published. Publishing is the single gate, so a half-built release cannot
  reach an installed copy.

The desktop app lives in `apps/hush-tauri`, not at the repository root. Every
command below is written from the repository root unless it says otherwise.

---

## One-time setup

### 1. Point the app at the update origin

`apps/hush-tauri/src-tauri/tauri.conf.json` names where the updater looks:

```json
"endpoints": ["https://hushrelay.space/updates/latest.json"]
```

Already set for this deployment. It has to be a public origin that can serve
`latest.json` and the bundles it names without credentials — see the note above
about why the origin has to be credential-free. A fork needs its own relay, or any HTTPS host holding
that directory.

Change it before the first release, not after: the endpoint is compiled into the
binary, so an installed copy keeps asking the old URL until it is reinstalled, or
updated from the old URL one last time. Nothing else in that file needs to change.

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

### 8. Add the relay deploy key

`.github/workflows/publish-updates.yml` mirrors each published release to the
relay over ssh, as an unprivileged account that may only write to the one
directory being served. Two repository secrets carry that:

| Secret | What it is |
| --- | --- |
| `RELAY_UPDATES_SSH_KEY` | the private half of the deploy key pair |
| `RELAY_UPDATES_KNOWN_HOSTS` | the relay's pinned host key, so an upload cannot be redirected to another host |

Set both from a workstation that already holds the pair:

```bash
gh secret set RELAY_UPDATES_SSH_KEY < ~/.ssh/hush-updates-deploy
ssh-keyscan -t ed25519 37.27.150.250 2>/dev/null \
  | gh secret set RELAY_UPDATES_KNOWN_HOSTS
```

`docs/ops/relay-prod-vps.md` has the other half of this: creating the
`relay-updates` account, installing the public key with the `restrict` option,
the served directory, and the Caddy route. A key rotation is the same procedure
with `gh secret set` at the end, and needs no rebuild — nothing about it is
compiled into the app.

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

Publishing starts `publish-updates.yml`, which downloads the release's assets,
rewrites every URL in `latest.json` onto the relay, uploads the bundles, checks
each one is publicly reachable, and publishes the manifest last. Watch it finish,
then check what an installed copy will actually fetch:

```bash
gh run watch "$(gh run list --workflow=publish-updates.yml --limit 1 \
  --json databaseId --jq '.[0].databaseId')"
curl -sS https://hushrelay.space/updates/latest.json \
  | jq '{version, platforms: (.platforms | keys)}'
```

`version` must be the tag you just published, and `platforms` must list every
platform the release carried. If the mirror fails, the release itself is fine —
fix the cause and re-run it from the Actions tab, or:

```bash
gh workflow run publish-updates.yml -f tag=v0.2.0
```

Publishing is the moment users start seeing the update. There is no staged
rollout: the next check every running copy makes will offer it.

---

## Re-releasing an existing tag

Needed when a release has to be rebuilt without a version bump — a signing or
packaging fix, say. The version files stay where they are, so the tag has to move
to the commit carrying the fix. Two things make that less obvious than it sounds:

- `release.yml` runs `actions/checkout` at the **tag's** commit, not the branch
  tip. Leaving the tag behind rebuilds the old, unfixed code.
- tauri-action looks for a **draft** release when `releaseDraft: true`. If the
  release is already published it logs `Found release with tag v0.2.0 but it's NOT
  a draft!` and then fails with `release not found` — after the build has already
  run, which is a wasted multi-leg build.

So the order is: unpublish, move the tag, build, publish.

```bash
# 1. Unpublish. Keeps the release and its assets, and does not touch the relay's
#    copy, so downloads keep working from the old manifest throughout.
gh release edit v0.2.0 --draft=true

# 2. Move the tag to the commit carrying the fix. This is what triggers the build.
git tag -f v0.2.0 <commit>
git push --force-with-lease=refs/tags/v0.2.0:<old-commit> origin refs/tags/v0.2.0

# 3. Wait for every leg, then publish again. Publishing is what fires
#    publish-updates.yml and re-mirrors the relay.
gh run watch "$(gh run list --workflow=release.yml --limit 1 \
  --json databaseId --jq '.[0].databaseId')"
gh release edit v0.2.0 --draft=false --latest
```

tauri-action deletes any existing asset of the same name before uploading, so the
rebuild replaces the bundles in place. Asset names are version-derived and the
version has not changed, so nothing is left stale.

Verify the result from outside, as for a normal release — read `latest.json` back
off the relay, and confirm the bundle itself carries a signature:

```bash
curl -sS https://hushrelay.space/updates/Hush_aarch64.app.tar.gz \
  | tar tzf - | grep _CodeSignature
```

That line is the difference between a copy a user can open and one Gatekeeper tells
them to move to the Trash.

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
   publish the draft, then wait for the mirror (see above). Until both are done
   the old copy sees nothing, which is worth verifying once: click **Check for
   updates** and confirm it says you are on the latest version.

4. **Trigger the update.** In the still-running 0.1.0 copy, click **Check for
   updates** in **Settings → About**. It should find `0.1.1` and show the
   release notes. Click **Update now**: the bar fills, the app restarts, and
   **Settings → About** now reads `0.1.1`.

5. **Confirm the silent path too.** Quit and relaunch the 0.1.1 copy, publish
   0.1.2, and leave the app alone. Within a few seconds of launch the update
   prompt should appear on its own, without touching anything.

### A faster loop, without GitHub

The mirror script is runnable by hand, and `--dry-run` exercises all of it except
the upload: it downloads the tag's assets, rewrites the manifest, and checks every
signature against the `.sig` the release carries.

```bash
scripts/publish-updates-to-relay.sh --tag v0.1.1 --dry-run
```

To test the app's own plumbing, serve a manifest locally and point the config at
it:

```bash
# A directory holding latest.json and the .tar.gz/.sig the manifest names.
python3 -m http.server 8000
```

Set `plugins.updater.endpoints` to `["http://localhost:8000/latest.json"]`,
build, and run. Remember to put the relay URL back before releasing — the
endpoint is baked into the binary.

### What a failure looks like

- **Wrong public key or a tampered artifact:** the download completes and then
  fails its signature check. Nothing is installed and the app keeps running;
  the message says the update did not pass verification. This is the case worth
  testing deliberately, by signing with a different key.

  When it happens for real, check the pairing from the outside instead of guessing
  which end is wrong:

  ```bash
  # Downloads and verifies every artifact the public manifest names.
  scripts/verify-update-signature.py \
    --manifest https://hushrelay.space/updates/latest.json --all
  ```

  `OK` for every platform means the published artifacts match the key the app
  carries, so the fault is local — a stale install, a proxy, a truncated download.
  `FAIL` naming a different key id means the release was signed with a key other
  than the one compiled into the app, and no installed copy will accept it.
  Needs `pip install cryptography`; it is deliberately not part of the publish
  path, which runs on `jq`, `rsync` and `curl` alone.
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
  A downloaded AppImage also has to be registered with the desktop before it has
  a menu entry or an icon the shell can resolve — `scripts/install-appimage-desktop.sh`
  does that, and README's "Install the desktop app" explains why the icon shows on
  some desktops and not others without it. It is a per-user step, so it is not part
  of the release job; the AppImage cannot do it for itself.
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
  first installs. Those bundle names carry no version — the release produces
  `Hush_aarch64.app.tar.gz`, and the next release overwrites it on the relay.
  That is why the mirror uploads bundles before the manifest: a client that has
  read a manifest always finds its artifact present, and one that races a release
  either verifies against the new file or fails cleanly and retries. Builds are
  **ad-hoc signed** but carry no Apple certificate and no notarization, so
  Gatekeeper still refuses the first launch. See the section below.
- **Signing keys are a pair.** The `pubkey` in `tauri.conf.json` and the
  `TAURI_SIGNING_PRIVATE_KEY` secret must come from the same `signer generate`
  run. Replacing one without the other makes every future update fail
  verification.

---

## Installing an ad-hoc-signed macOS build

There is no Apple certificate and no notarization. What the build does carry is an
**ad-hoc signature**, set by `bundle.macOS.signingIdentity` in
`apps/hush-tauri/src-tauri/tauri.conf.json`:

```json
"macOS": {
  "signingIdentity": "-"
}
```

That one line is the difference between the two Gatekeeper refusals, and the reason
it is not optional:

- With **no** signature at all, a quarantined copy is refused as **"Hush is damaged
  and can't be opened. You should move it to the Trash."** That dialog offers only
  *Move to Trash* and *Cancel* — there is no Open button, and nothing in Finder gets
  past it. This is what v0.2.0 shipped: `Hush.app` had no
  `Contents/_CodeSignature/CodeResources`, because `tauri build` signs only when it
  is given an identity, and none was configured.
- With an ad-hoc signature, the same copy is refused as **"Hush cannot be opened
  because Apple cannot check it for malicious software."** That one the user can
  clear.

Ad-hoc signing is also load-bearing rather than cosmetic on Apple Silicon, where the
kernel requires a valid signature for any executable to run. The linker writes one
automatically, but `tauri build` patches the main executable in place to record the
bundle type, and that patch invalidates it. Tauri re-signs after patching — again,
only when it has an identity to sign with.

Ad-hoc signing does **not** remove the warning; it makes the warning one the user can
clear. Both bypasses are on the user's machine:

```bash
xattr -dr com.apple.quarantine /Applications/Hush.app
```

or, in Finder, right-click the app → **Open** → **Open** in the dialog, which is
offered only the first time. The Finder route exists for the ad-hoc case only: it is
*not* offered for "damaged", so it is not a fallback for a build with no signature.

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

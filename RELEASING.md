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

`apps/hush-tauri/src-tauri/tauri.conf.json` ships with a placeholder endpoint:

```json
"endpoints": ["https://github.com/OWNER/REPO/releases/latest/download/latest.json"]
```

Replace `OWNER/REPO` with the GitHub owner and repository that will hold the
releases. Nothing else in that file needs to change.

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

Put the printed public key where the placeholder is:

```json
"pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6..."
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

### 6. macOS and Windows icons

`src-tauri/icons/` carries `icon.icns` and `icon.ico` alongside the PNGs, which
is what the macOS and Windows matrix legs need. They were generated once from
the 512×512 `icons/icon.png` with:

```bash
cd apps/hush-tauri
npm run tauri icon src-tauri/icons/icon.png -o /tmp/hush-icons
cp /tmp/hush-icons/icon.icns /tmp/hush-icons/icon.ico src-tauri/icons/
```

Nothing to do at release time. Regenerate the same way only if the app icon
changes; the source is only 512×512, so a larger `icon.png` is worth adding
first if a crisper `.icns` is wanted.

### 7. Push the repository to GitHub

The repository has no remote configured yet, and `.github/workflows/release.yml`
only runs on GitHub. Create the repository, add it as `origin`, and push. The
`OWNER/REPO` you used in step 1 must match.

> **Not yet exercised.** The workflow has only been checked as far as this
> machine allows: the YAML parses, `npm ci` succeeds against the committed
> lockfile, the `updater`/`process` permission namespaces resolve, and the CLI
> accepts the bundle args. Nothing has run on a GitHub or macOS/Windows runner,
> so the first tagged release is where the action's argument forwarding, the
> nested `apps/hush-tauri` `projectPath`, the platform keys it writes into
> `latest.json`, and the actual update round trip get their first real test.
> Treat the first tag as a rehearsal and read the draft release before
> publishing it.

---

## Building locally

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

The workflow builds macOS (Apple Silicon and Intel), Windows, and Ubuntu in
parallel and attaches everything to a **draft** release named after the tag.

### 3. Publish the draft

Wait for all four jobs to finish, then check the draft before publishing:

```bash
gh release list
gh release view v0.2.0 --json assets --jq '.assets[].name'
```

Confirm `latest.json` is present and that it lists an entry for every platform
you intend to support (`darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`,
`windows-x86_64`), alongside the installers. A platform missing from
`latest.json` is a platform whose existing installs will never see the update.

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
- **Windows:** Tauri generates updater artifacts for both the MSI and the NSIS
  installer, and the plugin looks for `windows-x86_64-nsis` or
  `windows-x86_64-msi` before falling back. The workflow builds `--bundles nsis`
  so the release carries a single Windows installer type and the manifest
  cannot disagree with how a copy was installed. If you also want the MSI, add
  it to the matrix `args` and check which artifact `latest.json` ends up
  pointing at before publishing.
- **macOS:** the `.app.tar.gz` is what the updater downloads; the `.dmg` is for
  first installs.
- **Signing keys are a pair.** The `pubkey` in `tauri.conf.json` and the
  `TAURI_SIGNING_PRIVATE_KEY` secret must come from the same `signer generate`
  run. Replacing one without the other makes every future update fail
  verification.

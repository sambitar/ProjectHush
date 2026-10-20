#!/usr/bin/env bash
# Build the macOS bundles the way .github/workflows/release.yml does, for when the
# account's Actions minutes are exhausted. Run this on a Mac; it refuses anywhere
# else.
#
# Why it cannot run on the Linux workstation that builds the AppImage: the DMG
# step shells out to `hdiutil`, the ad-hoc signature needs `codesign`, and linking
# `aarch64-apple-darwin` needs the macOS SDK. All three are macOS-only, and the
# SDK's licence restricts it to Apple hardware. This box is x86_64 Linux, so a
# local macOS VM is not an option either — an arm64 guest needs an Apple Silicon
# host.
#
# What it produces, matching the workflow's macOS leg:
#
#   Hush_<version>_<arch>.dmg          first installs
#   Hush_<arch>.app.tar.gz             what the updater downloads
#   Hush_<arch>.app.tar.gz.sig         the signature over it
#
# The .dmg is the one people ask for and the one the updater ignores; an installed
# copy updates from the .app.tar.gz. If the goal is only "existing Mac installs
# see the new version", the tarball is the artifact that matters.
#
# Usage:
#   scripts/local-build/build-macos-dmg.sh
#   scripts/local-build/build-macos-dmg.sh --target x86_64-apple-darwin
#   scripts/local-build/build-macos-dmg.sh --password-file ~/.tauri/hush.key.pass
#
# Signing needs the password for the encrypted key at ~/.tauri/hush.key. The
# updater artifacts are only produced with it; without a password the run stops
# at the signing step and there is no .sig.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

TARGET="aarch64-apple-darwin"
OUT_DIR=""
KEY_FILE="${TAURI_SIGNING_KEY_FILE:-$HOME/.tauri/hush.key}"
PASSWORD_FILE=""
SKIP_VERIFY=0

log()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)           TARGET="${2:?--target needs a value}"; shift 2 ;;
    --target=*)         TARGET="${1#--target=}"; shift ;;
    --out)              OUT_DIR="${2:?--out needs a value}"; shift 2 ;;
    --out=*)            OUT_DIR="${1#--out=}"; shift ;;
    --password-file)    PASSWORD_FILE="${2:?--password-file needs a value}"; shift 2 ;;
    --password-file=*)  PASSWORD_FILE="${1#--password-file=}"; shift ;;
    --key-file)         KEY_FILE="${2:?--key-file needs a value}"; shift 2 ;;
    --key-file=*)       KEY_FILE="${1#--key-file=}"; shift ;;
    --skip-verify)      SKIP_VERIFY=1; shift ;;
    -h | --help)        usage; exit 0 ;;
    *)                  die "unknown argument: $1 (try --help)" ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] \
  || die "this builds macOS bundles and only runs on macOS. For Linux, use scripts/local-build/build-linux-appimage.sh."

case "$TARGET" in
  aarch64-apple-darwin | x86_64-apple-darwin) ;;
  *) die "--target takes aarch64-apple-darwin (Apple Silicon) or x86_64-apple-darwin (Intel)." ;;
esac

VERSION="$(node -e "process.stdout.write(require('$REPO_ROOT/apps/hush-tauri/package.json').version)")"
[[ -n "$VERSION" ]] || die "could not read the version from apps/hush-tauri/package.json"
ARCH="${TARGET%%-*}"
[[ -n "$OUT_DIR" ]] || OUT_DIR="$HOME/hush-release/$VERSION"

step "prerequisites"
for tool in cargo node npm; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool not found on PATH"
done
# `xcode-select -p` is the check the workflow cannot make and a Mac must: without
# the command line tools there is no codesign, no hdiutil and no SDK.
xcode-select -p >/dev/null 2>&1 \
  || die "Xcode command line tools not found. Run: xcode-select --install"
for tool in codesign hdiutil; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool not found; install the Xcode command line tools"
done

if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  log "adding the $TARGET rust target"
  rustup target add "$TARGET"
fi

if [[ -n "$PASSWORD_FILE" ]]; then
  [[ -f "$PASSWORD_FILE" ]] || die "password file not found: $PASSWORD_FILE"
  [[ -f "$KEY_FILE" ]] || die "signing key not found: $KEY_FILE"
  log "signing with $KEY_FILE"
else
  # Without the key, `createUpdaterArtifacts` fails the build after it has already
  # bundled. That is a useless ending for the common case — someone with no key
  # who only wants a .dmg to install and test — so the flag is turned off for the
  # build and put back afterwards. The .dmg does not carry an updater signature
  # and does not need one; only the .app.tar.gz does, and that is the artifact
  # this mode deliberately does not produce.
  log "no --password-file: building a .dmg only, with createUpdaterArtifacts off"
fi

step "install the frontend dependencies"
if [[ -d "$REPO_ROOT/apps/hush-tauri/node_modules" ]]; then
  log "node_modules already present; run 'npm ci' by hand to refresh it"
else
  (cd "$REPO_ROOT/apps/hush-tauri" && npm ci)
fi

step "stage the daemon sidecar"
# Tauri resolves bundle.externalBin to a file named for the target triple, and
# tauri-build fails the compile when it is missing, so this runs before the build.
"$REPO_ROOT/scripts/stage-daemon-sidecar.sh" --target "$TARGET" --profile release

step "build"
CONF="$REPO_ROOT/apps/hush-tauri/src-tauri/tauri.conf.json"
CONF_BACKUP=""
restore_conf() {
  if [[ -n "$CONF_BACKUP" && -f "$CONF_BACKUP" ]]; then
    cp -p "$CONF_BACKUP" "$CONF"
    rm -f "$CONF_BACKUP"
    log "restored tauri.conf.json"
  fi
}
if [[ -z "$PASSWORD_FILE" ]]; then
  CONF_BACKUP="$(mktemp)"
  cp -p "$CONF" "$CONF_BACKUP"
  trap restore_conf EXIT
  # A literal string replacement, not a JSON round-trip: re-serialising the file
  # would reformat every other key too, and the backup exists only to be restored.
  python3 - "$CONF" <<'PY'
import sys
path = sys.argv[1]
src = open(path).read()
old = '"createUpdaterArtifacts": true'
new = '"createUpdaterArtifacts": false'
if old not in src:
    sys.exit("could not find %s in %s" % (old, path))
open(path, "w").write(src.replace(old, new, 1))
PY
  log "set bundle.createUpdaterArtifacts = false for this build (restored on exit)"
fi

cd "$REPO_ROOT/apps/hush-tauri"
if [[ -n "$PASSWORD_FILE" ]]; then
  export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_FILE")"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$PASSWORD_FILE")"
fi
# No --bundles flag: macOS defaults to app + dmg, which is exactly the pair the
# workflow's leg produces and the pair latest.json names.
npm run tauri -- build --target "$TARGET"

BUNDLE_DIR="$REPO_ROOT/target/$TARGET/release/bundle"

if [[ "$SKIP_VERIFY" == 0 ]]; then
  step "verify the bundle is ad-hoc signed"
  # v0.2.0 shipped with no Contents/_CodeSignature at all and nothing in the
  # pipeline noticed; the only symptom was a Gatekeeper dialog on a user's Mac.
  # The workflow grew this check after that, and it is worth as much locally.
  APP="$(find "$BUNDLE_DIR/macos" -maxdepth 1 -type d -name '*.app' | head -1)"
  [[ -n "$APP" ]] || die "no .app under $BUNDLE_DIR/macos; nothing was bundled"
  codesign --verify --deep --strict --verbose=2 "$APP"
  CODESIGN_LOG="$(mktemp)"
  codesign -dv --verbose=4 "$APP" 2>&1 | tee "$CODESIGN_LOG"
  if ! grep -q 'Signature=adhoc' "$CODESIGN_LOG"; then
    die "the bundle is not ad-hoc signed; a downloaded copy would be refused as damaged. Check bundle.macOS.signingIdentity in tauri.conf.json."
  fi
  rm -f "$CODESIGN_LOG"
  log "OK: $(basename "$APP") carries an ad-hoc signature"
fi

step "collect the artifacts"
mkdir -p "$OUT_DIR"
for f in "$BUNDLE_DIR/dmg/"*.dmg "$BUNDLE_DIR/macos/"*.app.tar.gz "$BUNDLE_DIR/macos/"*.app.tar.gz.sig; do
  [[ -e "$f" ]] || continue
  cp -p "$f" "$OUT_DIR/"
done

step "done"
log "artifacts for $VERSION in $OUT_DIR"
ls -la "$OUT_DIR"
if ! compgen -G "$OUT_DIR/*.sig" >/dev/null; then
  log ""
  log "note: no .sig was produced, so the updater cannot use these. Re-run with"
  log "--password-file before publishing."
fi
log ""
log "next: attach these to the v$VERSION release, and carry the darwin entries in"
log "latest.json. A Linux-only manifest leaves Mac installs on their current version."

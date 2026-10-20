#!/usr/bin/env bash
# Write the `latest.json` the updater reads, for a release assembled by hand.
#
# `.github/workflows/release.yml` gets this file from tauri-action, which builds it
# out of whatever the matrix legs uploaded. A locally built release has no such
# step, and `scripts/publish-updates-to-relay.sh` refuses to run without it — it
# reads the version, every platform URL and every signature out of this file, so a
# release that lacks one cannot be mirrored.
#
# The shape and the platform keys are copied from what tauri-action produces; the
# v0.5.0 release is the reference:
#
#   darwin-aarch64, darwin-aarch64-app   -> Hush_aarch64.app.tar.gz
#   darwin-x86_64,  darwin-x86_64-app    -> Hush_x64.app.tar.gz
#   linux-x86_64,   linux-x86_64-appimage -> Hush_<version>_amd64.AppImage
#
# Each `-app` / `-appimage` key is the platform-specific twin of the plain one and
# points at the same artifact. Only the artifacts actually present in --assets-dir
# get an entry, so a Linux-only build produces a Linux-only manifest. That is the
# honest result, and worth stating plainly: a manifest without a darwin entry
# leaves existing Mac installs on their current version, because the updater finds
# no target matching the machine it is running on.
#
# The URLs point at the GitHub release. `publish-updates-to-relay.sh` rewrites
# them onto the relay; the signature is over the artifact bytes, so rewriting the
# URL cannot invalidate it.
#
# Usage:
#   scripts/local-build/make-latest-json.sh --assets-dir /var/tmp/hush-build/out/0.6.0
#   scripts/local-build/make-latest-json.sh --assets-dir DIR --notes-file notes.md
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

VERSION=""
TAG=""
ASSETS_DIR=""
REPO="${GITHUB_REPOSITORY:-sambitar/ProjectHush}"
NOTES=""
NOTES_FILE=""
OUT=""

log()  { printf '%s\n' "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --assets-dir)     ASSETS_DIR="${2:?--assets-dir needs a value}"; shift 2 ;;
    --assets-dir=*)   ASSETS_DIR="${1#--assets-dir=}"; shift ;;
    --version)        VERSION="${2:?--version needs a value}"; shift 2 ;;
    --version=*)      VERSION="${1#--version=}"; shift ;;
    --tag)            TAG="${2:?--tag needs a value}"; shift 2 ;;
    --tag=*)          TAG="${1#--tag=}"; shift ;;
    --repo)           REPO="${2:?--repo needs a value}"; shift 2 ;;
    --repo=*)         REPO="${1#--repo=}"; shift ;;
    --notes)          NOTES="${2:?--notes needs a value}"; shift 2 ;;
    --notes=*)        NOTES="${1#--notes=}"; shift ;;
    --notes-file)     NOTES_FILE="${2:?--notes-file needs a value}"; shift 2 ;;
    --notes-file=*)   NOTES_FILE="${1#--notes-file=}"; shift ;;
    --out)            OUT="${2:?--out needs a value}"; shift 2 ;;
    --out=*)          OUT="${1#--out=}"; shift ;;
    -h | --help)      usage; exit 0 ;;
    *)                die "unknown argument: $1 (try --help)" ;;
  esac
done

command -v jq >/dev/null 2>&1 || die "jq not found on PATH"

[[ -n "$VERSION" ]] || VERSION="$(node -e "process.stdout.write(require('$REPO_ROOT/apps/hush-tauri/package.json').version)")"
[[ -n "$VERSION" ]] || die "could not read the version from apps/hush-tauri/package.json"
[[ -n "$TAG" ]] || TAG="v$VERSION"
[[ -n "$ASSETS_DIR" ]] || die "--assets-dir is required (the directory holding the bundles and their .sig files)"
[[ -d "$ASSETS_DIR" ]] || die "no such directory: $ASSETS_DIR"
[[ -n "$OUT" ]] || OUT="$ASSETS_DIR/latest.json"

if [[ -n "$NOTES_FILE" ]]; then
  [[ -f "$NOTES_FILE" ]] || die "notes file not found: $NOTES_FILE"
  NOTES="$(cat "$NOTES_FILE")"
fi

PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
BASE_URL="https://github.com/$REPO/releases/download/$TAG"

# One artifact serves two platform keys. Each entry is "<artifact> <key> <key>".
CANDIDATES=(
  "Hush_${VERSION}_amd64.AppImage linux-x86_64 linux-x86_64-appimage"
  "Hush_aarch64.app.tar.gz darwin-aarch64 darwin-aarch64-app"
  "Hush_x64.app.tar.gz darwin-x86_64 darwin-x86_64-app"
)

ENTRIES="{}"
FOUND=0
MISSING=()

for row in "${CANDIDATES[@]}"; do
  # shellcheck disable=SC2086
  set -- $row
  artifact="$1"; shift
  keys=("$@")

  if [[ ! -f "$ASSETS_DIR/$artifact" ]]; then
    MISSING+=("$artifact")
    continue
  fi
  # Every entry needs a signature; the updater refuses an artifact without one,
  # so a bundle with no .sig is a bug in the build, not something to publish.
  if [[ ! -f "$ASSETS_DIR/$artifact.sig" ]]; then
    die "$artifact has no .sig beside it; the updater would refuse it. Was the build signed?"
  fi

  sig="$(tr -d '\n' < "$ASSETS_DIR/$artifact.sig")"
  [[ -n "$sig" ]] || die "$artifact.sig is empty"

  for key in "${keys[@]}"; do
    ENTRIES="$(jq -c --arg k "$key" --arg u "$BASE_URL/$artifact" --arg s "$sig" \
      '. + {($k): {signature: $s, url: $u}}' <<<"$ENTRIES")"
  done
  log "  $artifact -> ${keys[*]}"
  FOUND=$((FOUND + 1))
done

[[ "$FOUND" -gt 0 ]] || die "no bundles found in $ASSETS_DIR (looked for ${CANDIDATES[*]%% *})"

jq -n \
  --arg version "$VERSION" \
  --arg notes "$NOTES" \
  --arg pub_date "$PUB_DATE" \
  --argjson platforms "$ENTRIES" \
  '{version: $version, notes: $notes, pub_date: $pub_date, platforms: $platforms}' > "$OUT"

log "wrote $OUT for $VERSION ($FOUND artifact(s))"
jq -r '.platforms | keys[]' "$OUT" | sed 's/^/  platform: /'

if [[ ${#MISSING[@]} -gt 0 ]]; then
  log ""
  log "not in this manifest (not built here): ${MISSING[*]}"
  log "A platform missing from latest.json does not break an existing install; it"
  log "simply is never offered this version, because the updater finds no target"
  log "matching that machine."
fi

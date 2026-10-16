#!/usr/bin/env bash
# Mirror a published GitHub release to the relay host, and rewrite the updater
# manifest so it points at the relay instead of GitHub.
#
# Why this exists: the repository is private, and GitHub answers 404 for every
# unauthenticated /releases/latest/download/... request on a private repo. The
# Tauri updater fetches latest.json with no credentials, so both the manifest and
# every bundle it names have to live on a public origin. They live under
# https://hushrelay.space/updates/, served straight off disk by Caddy (see
# docs/ops/relay-prod-vps.md). The same directory is also what makes the installers
# publicly downloadable, which the private repo's release page cannot do.
#
# Order matters. Bundles go up first and are verified reachable and signed;
# latest.json goes last. A client that has just read the manifest therefore always
# finds the artifact it names already in place, rather than a 404 mid-update.
#
# The upload user is a dedicated, unprivileged account whose key may only reach
# this one directory (restrict,no-pty,no-port-forwarding; see the ops doc). The
# script never needs sudo anywhere, on either end.
#
# Usage:
#   scripts/publish-updates-to-relay.sh --tag v0.1.1
#   scripts/publish-updates-to-relay.sh --tag v0.1.1 --dry-run
#   scripts/publish-updates-to-relay.sh --tag v0.1.1 --assets-dir ~/hush-release --skip-download
#
# --dry-run downloads (unless --skip-download), rewrites, checks signatures, and
# prints the exact rsync it would run. Nothing is uploaded and nothing public is
# touched.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TAG=""
REPO="${GITHUB_REPOSITORY:-}"
ASSETS_DIR=""
HOST="${RELAY_UPDATES_HOST:-37.27.150.250}"
USER="${RELAY_UPDATES_USER:-relay-updates}"
REMOTE_DIR="${RELAY_UPDATES_DIR:-/var/www/hush-updates}"
BASE_URL="${RELAY_UPDATES_BASE_URL:-https://hushrelay.space/updates}"
SSH_KEY="${RELAY_UPDATES_SSH_KEY_FILE:-$HOME/.ssh/hush-updates-deploy}"
KNOWN_HOSTS="${RELAY_UPDATES_KNOWN_HOSTS_FILE:-$HOME/.ssh/known_hosts}"
DRY_RUN=0
SKIP_DOWNLOAD=0
KEEP_ASSETS=0
CREATED_ASSETS_DIR=0

log()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
Mirror a published GitHub release to the relay and point the updater manifest at it.

  --tag TAG            release tag to publish, e.g. v0.1.1 (required)
  --repo OWNER/NAME    default: GITHUB_REPOSITORY, else the current gh repo
  --assets-dir DIR     default: a fresh temp dir (kept only on success)
  --host HOST          default: 37.27.150.250
  --user USER          default: relay-updates
  --remote-dir DIR     default: /var/www/hush-updates
  --base-url URL       default: https://hushrelay.space/updates
  --ssh-key FILE       default: $RELAY_UPDATES_SSH_KEY_FILE, else ~/.ssh/hush-updates-deploy
  --known-hosts FILE   default: $RELAY_UPDATES_KNOWN_HOSTS_FILE, else ~/.ssh/known_hosts
  --dry-run            download, rewrite, check signatures; upload nothing
  --skip-download      use the files already in --assets-dir
  --keep-assets        do not delete a temp --assets-dir on success
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)            TAG="${2:?--tag needs a value}"; shift 2 ;;
    --repo)           REPO="${2:?--repo needs a value}"; shift 2 ;;
    --assets-dir)     ASSETS_DIR="${2:?--assets-dir needs a value}"; shift 2 ;;
    --host)           HOST="${2:?--host needs a value}"; shift 2 ;;
    --user)           USER="${2:?--user needs a value}"; shift 2 ;;
    --remote-dir)     REMOTE_DIR="${2:?--remote-dir needs a value}"; shift 2 ;;
    --base-url)       BASE_URL="${2:?--base-url needs a value}"; shift 2 ;;
    --ssh-key)        SSH_KEY="${2:?--ssh-key needs a value}"; shift 2 ;;
    --known-hosts)    KNOWN_HOSTS="${2:?--known-hosts needs a value}"; shift 2 ;;
    --dry-run)        DRY_RUN=1; shift ;;
    --skip-download)  SKIP_DOWNLOAD=1; shift ;;
    --keep-assets)    KEEP_ASSETS=1; shift ;;
    -h|--help)        usage; exit 0 ;;
    *)                die "unknown argument: $1 (try --help)" ;;
  esac
done

[[ -n "$TAG" ]] || die "--tag is required, e.g. --tag v0.1.1"

# The manifest's version has no "v"; the tag does. Everything downstream compares
# the two, so normalize once here.
VERSION="${TAG#v}"

cleanup() {
  local rc=$?
  rm -rf "$WORK_DIR"
  if [[ "$CREATED_ASSETS_DIR" == 1 && "$KEEP_ASSETS" == 0 ]]; then
    # Keep a failed run's downloads: the rewritten manifest and the bundles are
    # what a post-mortem needs, and re-downloading 100 MB to look at them is rude.
    if [[ "$rc" == 0 ]]; then
      rm -rf "$ASSETS_DIR"
    else
      warn "exit $rc; keeping $ASSETS_DIR for inspection"
    fi
  fi
}
trap cleanup EXIT

# Scratch for the rewritten manifest and verification downloads. Deliberately not
# inside ASSETS_DIR: everything in there is a candidate for upload, and a leftover
# rewritten manifest from an earlier run would otherwise be mirrored to the relay
# as if it were a bundle.
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/hush-relay-publish-XXXXXX")"

step "prerequisites"
for tool in jq curl rsync ssh; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool not found on PATH"
done
if [[ "$SKIP_DOWNLOAD" == 0 ]]; then
  command -v gh >/dev/null 2>&1 || die "gh not found on PATH (needed to download the release)"
  if [[ -z "$REPO" ]]; then
    REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)" \
      || die "could not determine the repository; pass --repo owner/name"
  fi
fi
[[ -f "$SSH_KEY" ]] || die "ssh key not found: $SSH_KEY (pass --ssh-key, or set RELAY_UPDATES_SSH_KEY_FILE)"
[[ -f "$KNOWN_HOSTS" ]] || die "known_hosts not found: $KNOWN_HOSTS (pass --known-hosts, or set RELAY_UPDATES_KNOWN_HOSTS_FILE)"

# One key, one host, no agent: IdentitiesOnly keeps a stray agent key from being
# offered first and burning an auth attempt against the host's MaxAuthTries.
SSH_CMD=(ssh -i "$SSH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes
         -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$KNOWN_HOSTS")
REMOTE="$USER@$HOST"

step "assets for $TAG"
if [[ -z "$ASSETS_DIR" ]]; then
  ASSETS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/hush-release-XXXXXX")"
  CREATED_ASSETS_DIR=1
else
  mkdir -p "$ASSETS_DIR"
  KEEP_ASSETS=1
fi
if [[ "$SKIP_DOWNLOAD" == 1 ]]; then
  log "using the files already in $ASSETS_DIR (--skip-download)"
else
  log "downloading $TAG from $REPO into $ASSETS_DIR"
  gh release download "$TAG" --repo "$REPO" --dir "$ASSETS_DIR" --clobber
fi

MANIFEST="$ASSETS_DIR/latest.json"
[[ -f "$MANIFEST" ]] || die "$TAG has no latest.json asset; the build either failed or was not signed"
[[ "$(jq -r '.version' "$MANIFEST")" == "$VERSION" ]] \
  || die "latest.json says version $(jq -r '.version' "$MANIFEST") but the tag says $VERSION"
PLATFORMS="$(jq -r '.platforms | length' "$MANIFEST")"
[[ "$PLATFORMS" -gt 0 ]] || die "latest.json lists no platforms"

# Bundles to upload: everything but the manifest, which is held back for last.
# `latest.json.*` is excluded too, so a hand-made or half-written copy of the
# manifest in a reused --assets-dir cannot end up served under a bundle name.
BUNDLES=()
while IFS= read -r f; do
  BUNDLES+=("$f")
done < <(find "$ASSETS_DIR" -maxdepth 1 -type f ! -name 'latest.json' ! -name 'latest.json.*' | sort)
[[ ${#BUNDLES[@]} -gt 0 ]] || die "no bundles in $ASSETS_DIR; refusing to publish a manifest with nothing behind it"

step "check every platform entry has a bundle, and a signature that matches it"
for key in $(jq -r '.platforms | keys[]' "$MANIFEST"); do
  url="$(jq -r --arg k "$key" '.platforms[$k].url' "$MANIFEST")"
  base="$(basename "$url")"
  [[ -f "$ASSETS_DIR/$base" ]] || die "platform $key points at $base, which is not in the release"
  sig="$(jq -r --arg k "$key" '.platforms[$k].signature' "$MANIFEST" | tr -d '\n')"
  [[ -n "$sig" ]] || die "platform $key has no signature; the updater would refuse the update"
  if [[ -f "$ASSETS_DIR/$base.sig" ]]; then
    # The manifest's signature is the .sig file, base64-encoded. They agree today;
    # if they ever stop agreeing, the updater would reject the artifact, so fail
    # here rather than publish a manifest nobody can install from.
    file_sig="$(tr -d '\n' < "$ASSETS_DIR/$base.sig")"
    [[ "$sig" == "$file_sig" ]] || die "platform $key: signature in latest.json does not match $base.sig"
    log "  $key -> $base (signature matches)"
  else
    warn "platform $key: no $base.sig in the release; cannot cross-check the signature"
    log "  $key -> $base"
  fi
done

step "rewrite the manifest onto $BASE_URL"
# Only the URL changes. The signature is over the artifact bytes, so a rewritten
# URL cannot invalidate it; notes and pub_date are carried through untouched.
REWRITTEN="$WORK_DIR/latest.json"
jq --arg base "$BASE_URL" '
  .platforms = (.platforms | with_entries(
    .value.url = ($base + "/" + (.value.url | split("/") | last))
  ))
' "$MANIFEST" > "$REWRITTEN"
[[ "$(jq -r '.platforms | length' "$REWRITTEN")" == "$PLATFORMS" ]] \
  || die "rewriting dropped platform entries"
for url in $(jq -r '.platforms[].url' "$REWRITTEN"); do
  case "$url" in
    "$BASE_URL"/*) ;;
    *) die "rewritten url is not under $BASE_URL: $url" ;;
  esac
done
jq -r '.platforms | to_entries[] | "  \(.key) -> \(.value.url)"' "$REWRITTEN"

if [[ "$DRY_RUN" == 1 ]]; then
  step "dry run: nothing uploaded"
  log "would run:"
  log "  rsync -az --chmod=D755,F644 -e \"${SSH_CMD[*]}\" <${#BUNDLES[@]} bundles> $REMOTE:$REMOTE_DIR/"
  log "  then HEAD each bundle over HTTPS, then:"
  log "  rsync -az --chmod=F644 -e \"${SSH_CMD[*]}\" $REWRITTEN $REMOTE:$REMOTE_DIR/latest.json"
  exit 0
fi

step "upload the bundles"
# -z because the AppImage is ~100 MB. Files land 0644 so the Caddy user, which is
# not in the deploy group, can read them.
rsync -az --chmod=D755,F644 -e "${SSH_CMD[*]}" "${BUNDLES[@]}" "$REMOTE:$REMOTE_DIR/"

step "verify every bundle is publicly reachable"
for f in "${BUNDLES[@]}"; do
  base="$(basename "$f")"
  code="$(curl -sS -o /dev/null -w '%{http_code}' -I "$BASE_URL/$base")"
  [[ "$code" == "200" ]] || die "$BASE_URL/$base returned $code"
  log "  $base -> 200"
done

step "publish the manifest last"
rsync -az --chmod=F644 -e "${SSH_CMD[*]}" "$REWRITTEN" "$REMOTE:$REMOTE_DIR/latest.json"

step "verify what the updater will actually see"
curl -fsS "$BASE_URL/latest.json" -o "$WORK_DIR/latest.json.public" \
  || die "$BASE_URL/latest.json is not fetchable"
if ! diff -q <(jq -S . "$REWRITTEN") <(jq -S . "$WORK_DIR/latest.json.public") >/dev/null; then
  die "the public manifest differs from the one that was uploaded"
fi
[[ "$(jq -r '.version' "$WORK_DIR/latest.json.public")" == "$VERSION" ]] \
  || die "the public manifest does not report $VERSION"
for key in $(jq -r '.platforms | keys[]' "$WORK_DIR/latest.json.public"); do
  url="$(jq -r --arg k "$key" '.platforms[$k].url' "$WORK_DIR/latest.json.public")"
  code="$(curl -sS -o /dev/null -w '%{http_code}' -I "$url")"
  [[ "$code" == "200" ]] || die "$key: $url returned $code"
done
log "  $BASE_URL/latest.json serves $VERSION for $PLATFORMS platform entries"

step "done"
log "$TAG is live. Installed copies of an older version will see it on their next check."

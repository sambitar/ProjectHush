#!/usr/bin/env bash
# Build the Linux AppImage the way .github/workflows/release.yml does, but on this
# workstation — for when the account's Actions minutes are exhausted and a release
# still has to go out.
#
# Only the Linux leg is possible here. The two macOS legs need an Apple toolchain
# and an SDK that this machine does not have, so a release built this way carries
# no darwin bundles; see RELEASING.md, "Platform notes".
#
# It builds inside a container rather than on the host for two reasons, both of
# which would otherwise ship a worse artifact than CI:
#
#   * The host is Ubuntu 24.04 (glibc 2.39). The AppImage links the system glibc,
#     so a bundle built against it will not start on 22.04 or Debian 12
#     ("GLIBC_2.39 not found"). The image is ubuntu:22.04, matching the runner.
#   * linuxdeploy needs patchelf and libevent-2.1.so.7, and neither is installed
#     here; installing them needs a sudo password. They are in the image.
#
# The build itself writes to the SSD, not the working copy: this checkout is on
# /dev/sdb2, a spinning disk, while / is an SSD. The repository is copied into the
# container and `target/` is redirected onto the SSD, so the host tree gains no
# root-owned build output and the heavy I/O stays off the HDD.
#
# Usage:
#   scripts/local-build/build-linux-appimage.sh
#   scripts/local-build/build-linux-appimage.sh --out ~/hush-0.6.0
#   scripts/local-build/build-linux-appimage.sh --password-file ~/.tauri/hush.key.pass
#   scripts/local-build/build-linux-appimage.sh --rebuild-image
#
# Signing needs the password for the encrypted key at ~/.tauri/hush.key. Without
# it the compile and the bundle still run and the AppImage is still produced, but
# the run ends with "no private key" and there is no .sig for the updater. Re-run
# with --password-file once the password is available; the warm cache makes the
# second pass a bundling step rather than a rebuild.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

IMAGE="hush-linux-appimage:22.04"
DOCKERFILE="$REPO_ROOT/scripts/local-build/Dockerfile.linux-appimage"
CONTAINER="hush-local-build-$$"

# The SSD. /var/tmp is on the root filesystem, which is /dev/sda2 here.
BUILD_ROOT="${HUSH_BUILD_ROOT:-/var/tmp/hush-build}"
OUT_DIR=""
KEY_FILE="${TAURI_SIGNING_KEY_FILE:-$HOME/.tauri/hush.key}"
PASSWORD_FILE=""
REBUILD_IMAGE=0
KEEP_CONTAINER=0

log()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)              OUT_DIR="${2:?--out needs a value}"; shift 2 ;;
    --out=*)            OUT_DIR="${1#--out=}"; shift ;;
    --password-file)    PASSWORD_FILE="${2:?--password-file needs a value}"; shift 2 ;;
    --password-file=*)  PASSWORD_FILE="${1#--password-file=}"; shift ;;
    --key-file)         KEY_FILE="${2:?--key-file needs a value}"; shift 2 ;;
    --key-file=*)       KEY_FILE="${1#--key-file=}"; shift ;;
    --build-root)       BUILD_ROOT="${2:?--build-root needs a value}"; shift 2 ;;
    --build-root=*)     BUILD_ROOT="${1#--build-root=}"; shift ;;
    --rebuild-image)    REBUILD_IMAGE=1; shift ;;
    --keep-container)   KEEP_CONTAINER=1; shift ;;
    -h | --help)        usage; exit 0 ;;
    *)                  die "unknown argument: $1 (try --help)" ;;
  esac
done

VERSION="$(node -e "process.stdout.write(require('$REPO_ROOT/apps/hush-tauri/package.json').version)")"
[[ -n "$VERSION" ]] || die "could not read the version from apps/hush-tauri/package.json"

[[ -n "$OUT_DIR" ]] || OUT_DIR="$BUILD_ROOT/out/$VERSION"

step "prerequisites"
command -v docker >/dev/null 2>&1 || die "docker not found on PATH"
docker info >/dev/null 2>&1 || die "the docker daemon is not reachable"
command -v tar >/dev/null 2>&1 || die "tar not found on PATH"

if [[ -n "$PASSWORD_FILE" ]]; then
  [[ -f "$PASSWORD_FILE" ]] || die "password file not found: $PASSWORD_FILE"
  [[ -f "$KEY_FILE" ]] || die "signing key not found: $KEY_FILE"
elif [[ -f "$KEY_FILE" ]]; then
  log "no --password-file: building unsigned (the AppImage is produced, the .sig is not)"
else
  log "no signing key at $KEY_FILE and no --password-file: building unsigned"
fi

# Refuse to build onto the spinning disk by accident. This is the whole reason the
# script exists rather than a bare `npm run tauri build`.
mkdir -p "$BUILD_ROOT/target" "$BUILD_ROOT/npm-cache"
BUILD_DEV="$(df --output=source "$BUILD_ROOT" | tail -1)"
ROOT_DEV="$(df --output=source / | tail -1)"
[[ "$BUILD_DEV" == "$ROOT_DEV" ]] \
  || die "$BUILD_ROOT is on $BUILD_DEV, not the root filesystem ($ROOT_DEV). Set --build-root to a directory on the SSD."
log "build root $BUILD_ROOT (on $BUILD_DEV)"

step "container image"
if [[ "$REBUILD_IMAGE" == 1 ]] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  log "building $IMAGE from $DOCKERFILE"
  docker build -f "$DOCKERFILE" -t "$IMAGE" "$REPO_ROOT/scripts/local-build"
else
  log "$IMAGE already exists (pass --rebuild-image to refresh it)"
fi

cleanup() {
  if [[ "$KEEP_CONTAINER" == 1 ]]; then
    log "container $CONTAINER left in place (--keep-container); remove with: docker rm -f $CONTAINER"
  else
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

step "stage the source"
docker create --name "$CONTAINER" \
  -v "$BUILD_ROOT/target:/build/target" \
  -v "$BUILD_ROOT/npm-cache:/build/npm-cache" \
  "$IMAGE" sleep infinity >/dev/null
docker start "$CONTAINER" >/dev/null

# `target/` is excluded because it is a symlink onto the host's SSD cache, built by
# the host toolchain; reusing it inside a 22.04 container would link stale objects
# from a different libc and defeat the point. `node_modules` is excluded because
# `npm ci` reinstalls it. `.git` is not needed to build.
tar -C "$REPO_ROOT" \
    --exclude=./target \
    --exclude=./.git \
    --exclude='./apps/hush-tauri/node_modules' \
    -cf - . | docker exec -i "$CONTAINER" tar -xf - -C /src
log "copied the working tree into /src"

# The sidecar stager resolves the daemon it built at "$ROOT/target/<profile>", so
# `target` has to be a directory in the tree rather than an environment variable.
docker exec "$CONTAINER" ln -sfn /build/target /src/target

SIGN_ENV=()
if [[ -n "$PASSWORD_FILE" ]]; then
  docker exec "$CONTAINER" mkdir -p /keys
  docker cp "$KEY_FILE" "$CONTAINER:/keys/hush.key" >/dev/null
  docker cp "$PASSWORD_FILE" "$CONTAINER:/keys/password" >/dev/null
  docker exec "$CONTAINER" chmod 600 /keys/hush.key /keys/password
  SIGN_ENV=(--sign)
  log "the signing key and password are in the container; the bundle will be signed"
fi

step "build"
log "this compiles the whole workspace from scratch and takes a while; watch with:"
log "  docker logs -f $CONTAINER"
set +e
docker exec -i \
  -e CARGO_TARGET_DIR=/build/target \
  -e npm_config_cache=/build/npm-cache \
  -e HUSH_SIGN="${SIGN_ENV[0]:-}" \
  "$CONTAINER" bash -s <<'INNER'
set -euo pipefail
cd /src/apps/hush-tauri

echo "--- npm ci"
npm ci

echo "--- stage the daemon sidecar"
cd /src
./scripts/stage-daemon-sidecar.sh --target x86_64-unknown-linux-gnu --profile release

echo "--- tauri build (--bundles appimage)"
cd /src/apps/hush-tauri
if [[ "${HUSH_SIGN:-}" == "--sign" ]]; then
  export TAURI_SIGNING_PRIVATE_KEY="$(cat /keys/hush.key)"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat /keys/password)"
fi
npm run tauri -- build --bundles appimage
INNER
BUILD_RC=$?
set -e

if [[ "$BUILD_RC" != 0 ]]; then
  log ""
  log "the build exited $BUILD_RC"
  if [[ -z "$PASSWORD_FILE" ]]; then
    log "with no signing key this is expected at the very end: Tauri bundles first and"
    log "signs after, so the AppImage below should still be there. Re-run with"
    log "--password-file to get the .sig the updater needs."
  fi
fi

step "collect the artifacts"
BUNDLE_DIR=/build/target/release/bundle/appimage
if ! docker exec "$CONTAINER" test -d "$BUNDLE_DIR"; then
  die "no bundle directory at $BUNDLE_DIR; the build did not get far enough. Re-run with --keep-container to inspect."
fi
docker exec "$CONTAINER" ls -la "$BUNDLE_DIR"

mkdir -p "$OUT_DIR"
docker cp "$CONTAINER:$BUNDLE_DIR/." "$OUT_DIR/" >/dev/null
chmod -R u+rwX "$OUT_DIR"

step "done"
log "artifacts for $VERSION in $OUT_DIR"
ls -la "$OUT_DIR"
if ! compgen -G "$OUT_DIR/*.sig" >/dev/null; then
  log ""
  log "note: no .sig was produced, so these are unsigned. The updater will refuse them;"
  log "re-run with --password-file before publishing."
fi

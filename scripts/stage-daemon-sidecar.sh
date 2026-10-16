#!/usr/bin/env bash
# Put the daemon where Tauri looks for it as a sidecar.
#
# `tauri.conf.json` names the daemon in `bundle.externalBin`, and Tauri resolves
# that entry to a file carrying the *target triple* — `binaries/hush-daemon-
# x86_64-unknown-linux-gnu` — because a bundle has to carry the binary for the
# platform it was built for. Cargo names nothing that way, so the file is staged
# here, and `tauri-build` fails the compile outright when it is missing:
#
#   resource path `binaries/hush-daemon-x86_64-unknown-linux-gnu` doesn't exist
#
# That makes the daemon a build-time dependency of the app, not just a runtime
# one, so this runs before any build of the app: the run scripts call it, the
# release workflow calls it once per matrix leg, and a developer who only wants
# `cargo check -p hush-tauri` calls it once.
#
# The host triple builds into the shared `target/<profile>` directory so a dev
# run does not rebuild the whole dependency tree in a triple-named one; a cross
# target builds into `target/<triple>/<profile>`, which is where cargo puts it.
#
# Usage:
#   ./scripts/stage-daemon-sidecar.sh                        # host triple, debug
#   ./scripts/stage-daemon-sidecar.sh --profile release      # what a bundle needs
#   ./scripts/stage-daemon-sidecar.sh --target aarch64-apple-darwin --profile release
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/lib/hush.sh"
hush_check_prereqs

PROFILE=debug
TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      [[ $# -ge 2 ]] || { echo "--profile needs debug or release." >&2; exit 2; }
      PROFILE="$2"
      shift 2
      ;;
    --profile=*) PROFILE="${1#--profile=}"; shift ;;
    --target)
      [[ $# -ge 2 ]] || { echo "--target needs a triple." >&2; exit 2; }
      TARGET="$2"
      shift 2
      ;;
    --target=*) TARGET="${1#--target=}"; shift ;;
    -h | --help)
      awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"
      exit 0
      ;;
    *)
      echo "Unknown argument: $1 (options: --profile <debug|release>, --target <triple>)." >&2
      exit 2
      ;;
  esac
done

case "$PROFILE" in
  debug | release) ;;
  *)
    echo "--profile takes debug or release, not '$PROFILE'." >&2
    exit 2
    ;;
esac

# Cargo's development profile is named `dev`, and `--profile debug` is rejected
# outright, so the flag is built here rather than passed through. The output
# directory is still `target/debug`, which is what the copy below reads.
CARGO_FLAGS=()
[[ "$PROFILE" == "release" ]] && CARGO_FLAGS=(--profile release)

HOST="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "$TARGET" ]] || TARGET="$HOST"

# Windows binaries carry the extension, and so does the name Tauri looks for.
EXE="hush-daemon"
DEST="$ROOT/apps/hush-tauri/src-tauri/binaries/hush-daemon-$TARGET"
if [[ "$TARGET" == *windows* ]]; then
  EXE="hush-daemon.exe"
  DEST="$DEST.exe"
fi

if [[ "$TARGET" == "$HOST" ]]; then
  cargo build "${CARGO_FLAGS[@]}" -p hush-daemon
  SRC="$ROOT/target/$PROFILE/$EXE"
else
  echo "Cross-compiling the daemon for $TARGET (this needs that target's toolchain)."
  cargo build "${CARGO_FLAGS[@]}" -p hush-daemon --target "$TARGET"
  SRC="$ROOT/target/$TARGET/$PROFILE/$EXE"
fi

[[ -x "$SRC" || -f "$SRC" ]] || { echo "cargo produced no $SRC." >&2; exit 1; }

mkdir -p "$(dirname "$DEST")"
cp "$SRC" "$DEST"
chmod +x "$DEST" 2>/dev/null || true
echo "staged $(realpath --relative-to="$ROOT" "$DEST")"

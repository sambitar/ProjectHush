#!/usr/bin/env bash
# One Hush desktop window: one vault, one daemon, the production relay.
#
# The single-instance counterpart of run-two-ui.sh. Safe to re-run: a relay or
# daemon that is already up is reused, and a fresh dist is embedded into the
# desktop binary before it starts (the dist is baked in at compile time, so a
# UI change without this rebuild shows the old interface).
#
# A first run with no vault at VAULT_DIR creates one in the app: the window opens
# on a "Welcome" screen that asks for the vault name, a passphrase, and the relay,
# then shows the recovery code once. Nothing is pre-seeded, so cloning the
# repository and running this is a new user's first run. Later runs reuse that
# vault and go straight to the passphrase.
#
# Usage:
#   ./scripts/run-one-ui.sh                      # vault instances/peer-a, created in the app if absent
#   ./scripts/run-one-ui.sh --relay http://host:8790   # relay a new vault is created against
#   ./scripts/run-one-ui.sh --vault instances/me # somewhere else to keep the vault
#   VAULT_DIR=instances/peer-b ./scripts/run-one-ui.sh
#   ./scripts/run-one-ui.sh --no-relay           # accepted; local relays are never started
#
# An existing vault keeps the relay stored inside it: that is the one the daemon
# dials, and --relay only warns when the two disagree. For a vault that does not
# exist yet, --relay (then HUSH_RELAY_URL, then the shared relay) is what the
# create screen starts from.
#
# The desktop binary reads HUSH_API_URL (unix socket) and HUSH_WINDOW_TITLE.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# Shared checks: cargo, node/npm, and protoc (needed by libsignal's build script,
# and not provided by the repository). Fails here with install lines rather than
# inside prost-build three crates deep.
source "$ROOT/scripts/lib/hush.sh"
hush_check_prereqs

NO_RELAY=0
RELAY_ARG=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-relay) NO_RELAY=1; shift ;;
    --relay)
      [[ $# -ge 2 ]] || { echo "--relay needs a URL." >&2; exit 2; }
      RELAY_ARG="$2"
      shift 2
      ;;
    --relay=*) RELAY_ARG="${1#--relay=}"; shift ;;
    --vault)
      [[ $# -ge 2 ]] || { echo "--vault needs a path." >&2; exit 2; }
      VAULT_DIR="$2"
      shift 2
      ;;
    --vault=*) VAULT_DIR="${1#--vault=}"; shift ;;
    -h | --help)
      awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"
      exit 0
      ;;
    *)
      echo "Unknown argument: $1 (options: --relay <url>, --vault <path>, --no-relay)." >&2
      exit 2
      ;;
  esac
done

VAULT_DIR="${VAULT_DIR:-${ROOT}/instances/peer-a}"
# A relative --vault is relative to the repository root, which is also where the
# script runs from.
[[ "$VAULT_DIR" == /* ]] || VAULT_DIR="$ROOT/$VAULT_DIR"

RUN_DIR="${HUSH_RUNTIME_DIR:-${XDG_RUNTIME_DIR:-/tmp}/hush-one}"
SOCK="${HUSH_DAEMON_SOCKET:-${RUN_DIR}/hush.sock}"
LOG_DIR="${RUN_DIR}/logs"
mkdir -p "$RUN_DIR" "$LOG_DIR"
chmod 700 "$RUN_DIR" 2>/dev/null || true

# A new vault is created by the app, not here: the first screen asks for the
# passphrase and the vault name, so a fresh clone gets the same first run a
# packaged build would have. `hush-cli init` remains the path for automation —
# see the README.

# Tor is bundled as an app resource (src-tauri/resources/tor/) so a packaged
# build needs no system install. A dev run does not get that for free, and the
# failure is silent: Tauri resolves the resource directory from the executable,
# so it looks for the sidecar beside the binary. Here that is
# /var/tmp/hush-target/debug (target/ is a symlink), which is not even a
# directory Tauri recognises as cargo output — the check wants a grandparent
# literally named `target` — so it falls through to /usr/lib/hush-tauri, which
# does not exist. The shell then reports "Tor was not found" with the sidecar
# sitting in the repository.
#
# Point the shell at the sidecar explicitly, and give the dynamic loader the
# libevent beside it: that library is not installed system-wide, so the bundled
# Tor cannot start without it. HUSH_TOR_BIN wins if you have set one, which is
# the same precedence the app uses.
TOR_DIR="$ROOT/apps/hush-tauri/src-tauri/resources/tor"
TOR_BIN=""
TOR_LIB_DIR=""
if [[ -x "$TOR_DIR/tor" ]]; then
  TOR_BIN="${HUSH_TOR_BIN:-$TOR_DIR/tor}"
  TOR_LIB_DIR="$TOR_DIR"
fi

# The relay for a vault that does not exist yet: --relay, then HUSH_RELAY_URL, then
# the production relay. There is no local-relay path: two machines reach each
# other through the production host, and a local one would only talk to itself.
NEW_VAULT=0
if hush_vault_present "$VAULT_DIR"; then
  VAULT_RELAY="$(hush_vault_relay "$VAULT_DIR")"
  if [[ -z "$VAULT_RELAY" ]]; then
    # No client-config.json: the vault was created by hand (or by an older script),
    # and its own copy of the relay is encrypted. Release is the default, which is
    # right for loopback, HTTPS and onion relays; a plaintext remote one needs the
    # override below, and the daemon will say so if it is refused.
    echo "Relay for $VAULT_DIR is not recorded in client-config.json; assuming release."
    echo "If the daemon refuses its relay as plaintext, re-run with HUSH_PROFILE=debug."
  elif [[ -n "$RELAY_ARG" && "$RELAY_ARG" != "$VAULT_RELAY" ]]; then
    echo "note: $VAULT_DIR is on $VAULT_RELAY, not $RELAY_ARG. The vault's own relay wins:" >&2
    echo "      the mailbox lives there. To move it, run" >&2
    echo "      cargo run -p hush-cli -- --dir $VAULT_DIR relay set --url $RELAY_ARG" >&2
    echo "      and keep this file in step with it, which is what the next run reads:" >&2
    echo "      cargo run -p hush-cli -- --dir $VAULT_DIR config set --relay $RELAY_ARG" >&2
  fi
else
  # No vault yet. The window's first screen creates one, so nothing is created
  # here: the relay below is handed to the daemon as the value that screen starts
  # with, and the profile it needs is decided the same way as for a real vault.
  VAULT_RELAY="${RELAY_ARG:-${HUSH_RELAY_URL:-$HUSH_CLOUD_RELAY_URL}}"
  NEW_VAULT=1
fi

case "${VAULT_RELAY#*://}" in
  127.0.0.1* | localhost* | \[::1\]*)
    echo "This vault is on a local relay ($VAULT_RELAY). Local relays are disabled in this project; move it to https://hushrelay.space first." >&2
    exit 2
    ;;
esac

# A release daemon refuses plain HTTP to a relay that is not loopback or a v3
# onion (hush-core HttpPolicy::LoopbackOnly), so a plain-HTTP relay on a public
# host has to be dialled by a debug daemon. Applying the rule here keeps that
# failure from arriving as an unexplained relay error — the same choice
# smoke-relay-lab.sh documents in its header.
DAEMON_CARGO_FLAGS=(--release)
DAEMON_PROFILE=release
if [[ "${HUSH_PROFILE:-}" == "debug" ]]; then
  DAEMON_CARGO_FLAGS=()
  DAEMON_PROFILE=debug
elif [[ -n "${HUSH_PROFILE:-}" && "${HUSH_PROFILE}" != "release" ]]; then
  echo "Unknown HUSH_PROFILE '$HUSH_PROFILE' (expected debug or release)." >&2
  exit 2
elif hush_relay_needs_debug_daemon "$VAULT_RELAY"; then
  DAEMON_CARGO_FLAGS=()
  DAEMON_PROFILE=debug
  echo "Relay $VAULT_RELAY is plain HTTP and not loopback: a release daemon refuses it, so"
  echo "the daemon is built and run in debug. For a release daemon, terminate TLS in front"
  echo "of the relay or use its onion address — see docs/ops/relay-lab-vps.md."
fi

echo "Building UI (Vite) and desktop binary…"
hush_ensure_node_modules "$ROOT/apps/hush-tauri"
(cd apps/hush-tauri && npm run build)
# Tauri resolves `bundle.externalBin` to a triple-named file beside the frontend
# and refuses to compile the app without it, so the daemon is staged first — in
# the profile this run already builds, so the copy costs no extra build.
"$ROOT/scripts/stage-daemon-sidecar.sh" --profile "$DAEMON_PROFILE"
cargo build -p hush-tauri -p hush-cli
# The daemon runs in release. Its vault store encrypts multi-GB torrent files
# chunk by chunk, and AEAD is ~40x slower in a debug build (measured 1.7 MiB/s
# debug vs 73 MiB/s release), which made large downloads look stalled at 100%
# for ~17 minutes. The relay and desktop stay in debug: neither is CPU-bound.
cargo build "${DAEMON_CARGO_FLAGS[@]}" -p hush-daemon

DAEMON_BIN="$ROOT/target/$DAEMON_PROFILE/hush-daemon"
TAURI_BIN="$ROOT/target/debug/hush-tauri"

daemon_ready() {
  curl -sf -o /dev/null --unix-socket "$1" http://localhost/api/status 2>/dev/null
}

# The relay is always the production relay, or the one stored in the vault.
# Local relays are not used in this project outside automated tests.
if [[ "${WITH_LOCAL_RELAY:-0}" -eq 1 ]]; then
  echo "Local relays are disabled in this project; use https://hushrelay.space." >&2
  exit 2
fi

RELAY_PID=""
if [[ "$NEW_VAULT" -eq 1 ]]; then
  echo "Relay for the new vault: $VAULT_RELAY"
else
  echo "Using the relay stored in the vault${VAULT_RELAY:+ ($VAULT_RELAY)}"
fi
# Worth saying out loud before the window opens: with the production relay
# down, a new user's first invite has nowhere to go and every failure looks local.
if [[ -n "$VAULT_RELAY" ]] && ! curl -sf -o /dev/null --connect-timeout 5 "${VAULT_RELAY%/}/health"; then
  echo "warning: ${VAULT_RELAY%/}/health did not answer — sends will queue until the relay is back." >&2
fi

# The daemon is RESTARTED on every run, never reused: it serves the API the UI
# was compiled against, so a stale daemon (from before a rebuild) silently
# serves old responses — e.g. a /api/relay without fields the new UI reads.
# State lives in the vault on disk, so a restart is free.
#
# Match on the vault dir, not on our own socket. The vault lock is per directory,
# so a daemon serving this vault on some *other* socket blocks the run exactly as
# hard as a stale one on ours — and a socket check cannot see it: the daemon then
# dies at startup with "another hush-daemon is already running on <dir>", three
# minutes into the build. Match the dir in both absolute and relative form too: a
# daemon started by hand with a relative --dir or binary path (e.g.
# `./target/debug/hush-daemon --dir instances/peer-a`) does not match the
# absolute path, so an exact command-line match misses it.
VAULT_DIR_REL="${VAULT_DIR#"$ROOT"/}"
DAEMON_MATCH="hush-daemon.*--dir ($VAULT_DIR|$VAULT_DIR_REL)( |\$)"
if pgrep -f "$DAEMON_MATCH" >/dev/null 2>&1; then
  echo "Stopping the daemon holding $VAULT_DIR…"
  pkill -f "$DAEMON_MATCH" 2>/dev/null || true
  # Wait for the vault lock to be released. Polling our socket would not do it:
  # the daemon just stopped may never have served it.
  for _ in $(seq 1 50); do
    pgrep -f "$DAEMON_MATCH" >/dev/null 2>&1 || break
    sleep 0.1
  done
fi
rm -f "$SOCK" 2>/dev/null || true
# The relay stored in the vault is the runtime source of truth; an inherited
# HUSH_RELAY_URL would override it while the vault is locked. On a first run there
# is no vault and no stored relay yet, so the relay this run resolved is passed in:
# the create screen starts from it, and creating the vault stores it.
if [[ "$NEW_VAULT" -eq 1 ]]; then
  env HUSH_RELAY_URL="$VAULT_RELAY" HUSH_RUNTIME_DIR="$RUN_DIR" \
    "$DAEMON_BIN" --dir "$VAULT_DIR" --socket "$SOCK" >"$LOG_DIR/daemon.log" 2>&1 &
else
  env -u HUSH_RELAY_URL HUSH_RUNTIME_DIR="$RUN_DIR" \
    "$DAEMON_BIN" --dir "$VAULT_DIR" --socket "$SOCK" >"$LOG_DIR/daemon.log" 2>&1 &
fi
DAEMON_PID=$!
for _ in $(seq 1 100); do
  daemon_ready "$SOCK" && break
  kill -0 "$DAEMON_PID" 2>/dev/null || { tail -n 20 "$LOG_DIR/daemon.log" >&2; exit 1; }
  sleep 0.1
done
daemon_ready "$SOCK" || { echo "Daemon did not come up:" >&2; tail -n 20 "$LOG_DIR/daemon.log" >&2; exit 1; }
echo "Daemon started (pid $DAEMON_PID, log $LOG_DIR/daemon.log)"

echo "Opening the desktop window…"
# Tor is reported before the window opens, because the Relay panel is where the
# answer would otherwise show up as a failure the reader has to interpret.
if [[ -n "$TOR_BIN" ]]; then
  echo "Tor: the bundled sidecar ($TOR_BIN)"
elif command -v tor >/dev/null 2>&1; then
  echo "Tor: the system copy ($(command -v tor))"
else
  echo "Tor: none found — the Relay panel will say so, and onion mail waits until there is one."
fi
UI_ENV=()
[[ -n "$TOR_BIN" ]] && UI_ENV+=(HUSH_TOR_BIN="$TOR_BIN")
if [[ -n "$TOR_LIB_DIR" ]]; then
  UI_ENV+=(LD_LIBRARY_PATH="${TOR_LIB_DIR}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}")
fi
env HUSH_API_URL="unix://$SOCK" HUSH_WINDOW_TITLE="Hush — one" \
  "${UI_ENV[@]}" "$TAURI_BIN" >"$LOG_DIR/ui.log" 2>&1 &
UI_PID=$!
sleep 2
kill -0 "$UI_PID" 2>/dev/null || { echo "UI exited at startup:" >&2; tail -n 20 "$LOG_DIR/ui.log" >&2; exit 1; }

echo ""
if [[ "$NEW_VAULT" -eq 1 ]]; then
  echo "Window is up. This device has no vault yet: the Welcome screen creates one."
  echo "Pick the vault name (it ends your mail address), a passphrase, and the relay"
  echo "(prefilled with $VAULT_RELAY). The recovery code is shown once — save it."
else
  echo "Window is up. Unlock with the vault's passphrase (peer vaults: peer-a-pass)."
fi
echo "The Tor switch (login card or Settings → Relay) is all there is to it: Tor"
echo "connects on its own and the shield in the top rail shows the connection."
echo "Logs: $LOG_DIR"
echo "Stop everything with: kill $UI_PID ${DAEMON_PID:-} ${RELAY_PID:-}"
# Wait for the first of this run's processes to exit — normally the window
# closing. What was reused stays up; what was started here stays up too until
# killed with the line above, so the window can be reopened cheaply.
wait -n
echo "A process exited. The daemon and relay this run started are still up;"
echo "stop them with: kill ${DAEMON_PID:-} ${RELAY_PID:-} 2>/dev/null"

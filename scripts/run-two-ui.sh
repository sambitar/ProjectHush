#!/usr/bin/env bash
# Two Hush UI windows: Alice (peer-a) and Bob (peer-b), each with its own daemon.
#
# Safe to re-run. A relay, daemon, or Vite server that is already up is reused
# instead of being started a second time, so this doubles as "just reopen the
# windows": a second daemon on a live socket would fail to bind and take the
# whole run down with it.
#
# The daemons listen on Unix sockets by default (DR-02). Set HUSH_DAEMON_A_ADDR /
# HUSH_DAEMON_B_ADDR to also open a TCP listener (so a browser on another machine
# can reach a daemon).
#
# The relay is named by --relay <url> (falling back to HUSH_RELAY_URL, then
# http://127.0.0.1:8787). A loopback URL means "start a local relay"; any other
# host is used as-is and no local relay is started. The daemons read the relay
# from their vault, never from this script's environment, so --relay only decides
# whether a local relay is started (and where a brand-new vault is created). Point
# it at a remote relay to run the stack with no local relay:
#   ./scripts/run-two-ui.sh --dev --relay http://relay.example:8787
#
# --dev skips the UI and desktop builds entirely: it starts the relay (only when
# it is local) and both daemons with a TCP listener plus one Vite dev server, and
# prints a browser URL per peer. Edits under apps/hush-tauri/src hot reload; only
# crates/ changes need a rebuild, and then only the daemon (this script does that
# part).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEV=0
RELAY_ARG=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dev) DEV=1; shift ;;
    --relay)
      [[ $# -ge 2 ]] || { echo "--relay needs a URL." >&2; exit 2; }
      RELAY_ARG="$2"
      shift 2
      ;;
    --relay=*) RELAY_ARG="${1#--relay=}"; shift ;;
    -h | --help)
      awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"
      exit 0
      ;;
    *)
      echo "Unknown argument: $1 (options: --dev, --relay <url>)." >&2
      exit 2
      ;;
  esac
done
# Shared checks: cargo, node/npm, and protoc (needed by libsignal's build script,
# and not provided by the repository). Fails here with install lines rather than
# inside prost-build three crates deep.
source "$ROOT/scripts/lib/hush.sh"
hush_check_prereqs
PEER_A="${ROOT}/instances/peer-a"
PEER_B="${ROOT}/instances/peer-b"
RELAY_URL="${RELAY_ARG:-${HUSH_RELAY_URL:-http://127.0.0.1:8787}}"

# True when the relay URL points at this machine, i.e. a relay this script may
# start. Anything else is remote and is used where it is.
relay_is_local() {
  local host="${1#*://}"
  host="${host%%/*}"
  host="${host##*@}"
  case "$host" in
    \[*\]*) host="${host%%]*}"; host="${host#\[}" ;;
    *) host="${host%%:*}" ;;
  esac
  case "$host" in
    127.0.0.1 | localhost | ::1) return 0 ;;
    *) return 1 ;;
  esac
}
RELAY_IS_LOCAL=0
if relay_is_local "$RELAY_URL"; then
  RELAY_IS_LOCAL=1
fi

RUN_DIR="${HUSH_RUNTIME_DIR:-${XDG_RUNTIME_DIR:-/tmp}/hush-two-peers}"
SOCK_A="${HUSH_DAEMON_A_SOCKET:-${RUN_DIR}/peer-a.sock}"
SOCK_B="${HUSH_DAEMON_B_SOCKET:-${RUN_DIR}/peer-b.sock}"
# Empty by default: TCP stays closed unless explicitly requested. Dev mode needs
# it, because a browser cannot speak to a Unix socket.
DAEMON_A="${HUSH_DAEMON_A_ADDR:-}"
DAEMON_B="${HUSH_DAEMON_B_ADDR:-}"
VITE_PORT=5173
if [[ "$DEV" == 1 ]]; then
  DAEMON_A="${DAEMON_A:-127.0.0.1:9876}"
  DAEMON_B="${DAEMON_B:-127.0.0.1:9877}"
fi

PASS_A="${HUSH_PEER_A_PASS:-peer-a-pass}"
PASS_B="${HUSH_PEER_B_PASS:-peer-b-pass}"

BIN="${ROOT}/target/debug"
RELAY_BIN="${BIN}/hush-relay"
# The daemon runs in release: its vault store encrypts multi-GB torrent files
# chunk by chunk, and AEAD is ~40x slower in a debug build (see run-one-ui.sh),
# which made large downloads look stalled at 100%.
#
# HUSH_PROFILE=debug opts out for work that never touches the vault (paper/UI
# iteration). It keeps the daemon's ~1550-crate dependency tree in the same
# profile as `cargo test`, so the tree is compiled once instead of twice.
PROFILE="${HUSH_PROFILE:-release}"
if [[ "$PROFILE" == "debug" ]]; then
  DAEMON_BIN="${ROOT}/target/debug/hush-daemon"
  DAEMON_CARGO_FLAGS=()
elif [[ "$PROFILE" == "release" ]]; then
  DAEMON_BIN="${ROOT}/target/release/hush-daemon"
  DAEMON_CARGO_FLAGS=(--release)
else
  echo "Unknown HUSH_PROFILE '$PROFILE' (expected debug or release)." >&2
  exit 2
fi

# A loopback RELAY_URL with both vaults already present usually means "the
# operator did not pass --relay", not "start a relay nobody will use": the
# daemons read the relay stored in their vault, so a local one would come up and
# carry no traffic. Follow the vaults instead. This runs before the build because
# the relay it settles on decides which profile the daemon needs.
if [[ "$RELAY_IS_LOCAL" == 1 && -z "$RELAY_ARG" && -z "${HUSH_RELAY_URL:-}" ]] \
     && [[ -d "$PEER_A" && -d "$PEER_B" ]]; then
  STORED="$(hush_vault_relay "$PEER_A")"
  STORED_B="$(hush_vault_relay "$PEER_B")"
  if [[ -n "$STORED" && "$STORED" == "$STORED_B" ]] && ! relay_is_local "$STORED"; then
    RELAY_URL="$STORED"
    RELAY_IS_LOCAL=0
    echo "Vaults point at $RELAY_URL (from client-config.json); no local relay."
  fi
fi

# A release daemon refuses plain HTTP to a relay that is not loopback or a v3
# onion (hush-core HttpPolicy::LoopbackOnly), so the shared lab relay — plain HTTP
# on a public host — needs a debug daemon. Applied here so the failure does not
# arrive later as an unexplained relay error. An explicit HUSH_PROFILE still wins.
if [[ -z "${HUSH_PROFILE:-}" ]] && hush_relay_needs_debug_daemon "$RELAY_URL"; then
  PROFILE=debug
  DAEMON_BIN="${ROOT}/target/debug/hush-daemon"
  DAEMON_CARGO_FLAGS=()
  echo "Relay $RELAY_URL is plain HTTP and not loopback: a release daemon refuses it, so"
  echo "the daemon runs in debug. For a release daemon, terminate TLS in front of the relay"
  echo "or use its onion address — see docs/ops/relay-lab-vps.md."
fi

TAURI_BIN="${BIN}/hush-tauri"

# The bundled Tor sidecar, for the reason spelled out in run-one-ui.sh: a dev run
# does not resolve the app's resource directory, so the sidecar has to be handed
# to the window directly, along with the libevent it links against.
TOR_DIR="${ROOT}/apps/hush-tauri/src-tauri/resources/tor"
TOR_BIN=""
TOR_LIB_DIR=""
if [[ -x "$TOR_DIR/tor" ]]; then
  TOR_BIN="${HUSH_TOR_BIN:-$TOR_DIR/tor}"
  TOR_LIB_DIR="$TOR_DIR"
fi

LOG_DIR="${RUN_DIR}/logs"
PIDS=()
RELAY_PID=""
VITE_PGID=""

# Everything this run started dies with it; anything it reused is left alone.
cleanup() {
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  # `npm run dev` wraps vite in `sh -c`, which swallows the signal, so the dev
  # server is started as its own process group and stopped as one.
  if [[ -n "$VITE_PGID" ]]; then
    kill -TERM -- "-$VITE_PGID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

# Both branches below need the UI's dependencies: the full run builds the dist
# from apps/hush-tauri, and dev mode runs the Vite server there.
hush_ensure_node_modules "${ROOT}/apps/hush-tauri"

# The relay first, before anything that needs one to be up: the vaults are
# created further down, and setup-two-peers.sh registers the first contacts
# through the relay, so a fresh clone fails there if this comes later.
if [[ "$RELAY_IS_LOCAL" == 1 ]]; then
  echo "Building the relay…"
  cargo build -p hush-relay
fi

if [[ "$DEV" == 1 ]]; then
  echo "Dev mode: building the daemon (no UI or desktop build)."
  cargo build "${DAEMON_CARGO_FLAGS[@]}" -p hush-daemon
else
  echo "Building UI (Vite)…"
  (cd "${ROOT}/apps/hush-tauri" && npm run build)
  echo "Building the desktop binary and the daemon…"
  # Tauri resolves `bundle.externalBin` to a triple-named file beside the
  # frontend and refuses to compile the app without it, so the daemon is staged
  # first — in the profile this run already builds, so the copy costs no extra
  # build. Dev mode runs `tauri dev`, which compiles the app too, so it stages
  # as well.
  "${ROOT}/scripts/stage-daemon-sidecar.sh" --profile "$PROFILE"
  cargo build -p hush-tauri
  cargo build "${DAEMON_CARGO_FLAGS[@]}" -p hush-daemon
fi

REQUIRED_BINS=("$DAEMON_BIN")
if [[ "$RELAY_IS_LOCAL" == 1 ]]; then
  REQUIRED_BINS=("$RELAY_BIN" "${REQUIRED_BINS[@]}")
fi
[[ "$DEV" == 1 ]] || REQUIRED_BINS+=("$TAURI_BIN")
for bin in "${REQUIRED_BINS[@]}"; do
  if [[ ! -x "$bin" ]]; then
    echo "Missing $bin — the build above did not produce it." >&2
    exit 1
  fi
done

if ! mkdir -p "$RUN_DIR" "$LOG_DIR" 2>/dev/null; then
  echo "Cannot create the runtime dir $RUN_DIR — set HUSH_RUNTIME_DIR to a writable path." >&2
  exit 1
fi
chmod 700 "$RUN_DIR" 2>/dev/null || true

port_busy() {
  local addr="$1"
  [[ -z "$addr" ]] && return 1
  ss -ltn 2>/dev/null | grep -q ":${addr##*:} " && return 0
  return 1
}

# A socket that answers /api/status has a live daemon behind it. A leftover
# socket file from a killed run does not, which is how stale files are told apart.
daemon_ready() {
  curl -sf -o /dev/null --unix-socket "$1" http://localhost/api/status 2>/dev/null
}

relay_ready() {
  curl -s -o /dev/null --connect-timeout 1 "$RELAY_URL/" 2>/dev/null
}

# The relay's liveness route. `/` is 404 on the deployed relay, so /health is the
# check that says "a relay answered" rather than "something answered".
relay_health() {
  [[ "$(curl -s -o /dev/null -w '%{http_code}' --connect-timeout 3 "${1%/}/health" 2>/dev/null)" == "200" ]]
}

if [[ "$RELAY_IS_LOCAL" == 1 ]]; then
  if relay_ready; then
    echo "Local relay already up at $RELAY_URL"
  else
    RELAY_ADDR="${RELAY_URL#*://}"
    RELAY_ADDR="${RELAY_ADDR%%/*}"
    if port_busy "$RELAY_ADDR"; then
      echo "Port $RELAY_ADDR is in use but the relay did not respond — fix or stop that process." >&2
      exit 1
    fi
    echo "Starting local relay at $RELAY_URL …"
    HUSH_RELAY_DB="${ROOT}/instances/relay/relay.db" HUSH_RELAY_ADDR="$RELAY_ADDR" \
      "$RELAY_BIN" >"$LOG_DIR/relay.log" 2>&1 &
    RELAY_PID=$!
    PIDS+=("$RELAY_PID")
    for _ in $(seq 1 100); do
      if relay_ready; then break; fi
      kill -0 "$RELAY_PID" 2>/dev/null || break
      sleep 0.1
    done
    if ! relay_ready; then
      echo "Relay did not come up on $RELAY_URL:" >&2
      tail -n 20 "$LOG_DIR/relay.log" >&2 || true
      exit 1
    fi
  fi
else
  echo "Using remote relay at $RELAY_URL (no local relay will be started)."
  if relay_health "$RELAY_URL"; then
    echo "  $RELAY_URL/health is up."
  else
    echo "  warning: $RELAY_URL/health did not answer 200; daemons will retry and sends will queue." >&2
  fi
fi

# Vaults come last of the setup steps, once the relay they are registered
# against is up: setup-two-peers.sh adds the first contacts through that relay
# and fails with "relay did not answer /health" when it is not running yet.
if [[ ! -d "$PEER_A" || ! -d "$PEER_B" ]]; then
  echo "Creating peer vaults (./scripts/setup-two-peers.sh)…"
  HUSH_RELAY_URL="$RELAY_URL" "$ROOT/scripts/setup-two-peers.sh"
fi

# Daemons this run started, as "name:socket:pid", to wait on below.
STARTED=()

start_daemon() { # <name> <vault dir> <socket> [tcp addr]
  local name="$1" dir="$2" sock="$3" addr="${4:-}"
  if daemon_ready "$sock"; then
    # Reusing a live daemon is fine, but it cannot grow a TCP listener that it
    # was not started with, and silently not opening the port would be worse.
    if [[ -n "$addr" ]] && ! port_busy "$addr"; then
      echo "$name daemon is already up on $sock but not listening on $addr." >&2
      echo "Stop that daemon, then re-run so it can open the port." >&2
      exit 1
    fi
    echo "  $name: already up on $sock"
    return 0
  fi
  if port_busy "$addr"; then
    echo "Port $addr is in use but no daemon answers on $sock — stop that process." >&2
    exit 1
  fi
  # A socket file with nothing behind it is left over from a killed run; the
  # daemon clears it too, but doing it here keeps the two failures apart.
  rm -f "$sock" 2>/dev/null || true
  # `env -u HUSH_RELAY_URL`: the relay stored in the vault is the runtime source
  # of truth, and an inherited env var would override it while the vault is
  # locked. --relay only chooses a local relay, never a vault's relay.
  env -u HUSH_RELAY_URL HUSH_RUNTIME_DIR="$RUN_DIR" \
    "$DAEMON_BIN" --dir "$dir" --socket "$sock" ${addr:+--addr "$addr"} \
    >"$LOG_DIR/$name.log" 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  STARTED+=("$name:$sock:$pid")
  echo "  $name: started (pid $pid, log $LOG_DIR/$name.log)"
}

echo "Starting daemons …"
echo "  Alice socket: $SOCK_A${DAEMON_A:+  tcp: http://$DAEMON_A}"
echo "  Bob   socket: $SOCK_B${DAEMON_B:+  tcp: http://$DAEMON_B}"
start_daemon alice "$PEER_A" "$SOCK_A" "$DAEMON_A"
start_daemon bob "$PEER_B" "$SOCK_B" "$DAEMON_B"

for entry in "${STARTED[@]}"; do
  IFS=: read -r name sock pid <<<"$entry"
  ready=""
  for _ in $(seq 1 100); do
    if daemon_ready "$sock"; then
      ready=1
      break
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  if [[ -z "$ready" ]]; then
    echo "$name daemon did not come up on $sock:" >&2
    tail -n 20 "$LOG_DIR/$name.log" >&2 || true
    exit 1
  fi
done

# One Vite server, one tab per peer: the UI reads `?api=` for its daemon.
start_vite() {
  local url="http://127.0.0.1:${VITE_PORT}"
  if port_busy "127.0.0.1:${VITE_PORT}"; then
    if curl -sf -o /dev/null "$url/"; then
      echo "Vite dev server already up at ${url} (reusing it)."
      return 0
    fi
    echo "Port ${VITE_PORT} is in use but no Vite server answers on it — stop that process first." >&2
    exit 1
  fi
  echo "Starting the Vite dev server on port ${VITE_PORT} …"
  if command -v setsid >/dev/null 2>&1; then
    # Own process group, so cleanup can stop npm, sh, and vite together.
    (cd "${ROOT}/apps/hush-tauri" && exec setsid npm run dev) >"$LOG_DIR/vite.log" 2>&1 &
    local pid=$!
    VITE_PGID="$pid"
  else
    (cd "${ROOT}/apps/hush-tauri" && exec npm run dev) >"$LOG_DIR/vite.log" 2>&1 &
    local pid=$!
  fi
  PIDS+=("$pid")
  for _ in $(seq 1 200); do
    if curl -sf -o /dev/null "$url/"; then break; fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  if ! curl -sf -o /dev/null "$url/"; then
    echo "The Vite dev server did not come up:" >&2
    tail -n 20 "$LOG_DIR/vite.log" >&2 || true
    exit 1
  fi

  echo ""
  echo "Open one tab per peer. Edits under apps/hush-tauri/src hot reload:"
  echo "  Alice: ${url}/?api=http://${DAEMON_A}"
  echo "  Bob:   ${url}/?api=http://${DAEMON_B}"
  echo ""
  echo "Unlock with:"
  echo "  Alice: passphrase $PASS_A  → contact bob"
  echo "  Bob:   passphrase $PASS_B  → contact alice"
  echo "  Both sides must add each other's invite (Copy my invite)."
  echo ""
  echo "After a change under crates/, rebuild the daemon and re-run with --dev:"
  echo "  cargo build ${DAEMON_CARGO_FLAGS[*]} -p hush-daemon   # then Ctrl+C here and run $0 --dev"
  echo "The desktop binary is neither built nor started in dev mode."
}

if [[ "$DEV" == 1 ]]; then
  start_vite
else
  echo ""
  echo "Opening two UI windows. Unlock with:"
  echo "  Alice (unix://$SOCK_A): passphrase $PASS_A  → contact bob"
  echo "  Bob   (unix://$SOCK_B): passphrase $PASS_B  → contact alice"
  echo "  Both sides must add each other's invite (Copy my invite)."
  echo ""

  UI_NAMES=(alice-ui bob-ui)
  UI_PIDS=()
  # See run-one-ui.sh: the bundled Tor is an app resource a packaged build finds
  # beside itself, but a dev run resolves the resource directory from the
  # executable and misses it, so the Relay panel would report no Tor. Hand the
  # windows the sidecar and its libevent instead.
  UI_ENV=()
  if [[ -n "$TOR_BIN" ]]; then
    UI_ENV+=(HUSH_TOR_BIN="$TOR_BIN")
  fi
  if [[ -n "$TOR_LIB_DIR" ]]; then
    UI_ENV+=(LD_LIBRARY_PATH="${TOR_LIB_DIR}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}")
  fi
  if [[ -n "$TOR_BIN" ]]; then
    echo "Tor: the bundled sidecar ($TOR_BIN)"
  elif command -v tor >/dev/null 2>&1; then
    echo "Tor: the system copy ($(command -v tor))"
  else
    echo "Tor: none found — the Relay panel will say so, and onion mail waits until there is one."
  fi
  echo ""
  env HUSH_API_URL="unix://${SOCK_A}" HUSH_WINDOW_TITLE="Hush — Alice" \
    "${UI_ENV[@]}" "$TAURI_BIN" >"$LOG_DIR/alice-ui.log" 2>&1 &
  UI_PIDS+=($!)
  env HUSH_API_URL="unix://${SOCK_B}" HUSH_WINDOW_TITLE="Hush — Bob" \
    "${UI_ENV[@]}" "$TAURI_BIN" >"$LOG_DIR/bob-ui.log" 2>&1 &
  UI_PIDS+=($!)
  PIDS+=("${UI_PIDS[@]}")

  sleep 1
  for i in "${!UI_PIDS[@]}"; do
    if ! kill -0 "${UI_PIDS[$i]}" 2>/dev/null; then
      echo "The ${UI_NAMES[$i]} window exited at startup:" >&2
      tail -n 20 "$LOG_DIR/${UI_NAMES[$i]}.log" >&2 || true
      exit 1
    fi
  done
fi

echo "Inspect a vault from the shell:"
echo "  curl --unix-socket $SOCK_A http://localhost/api/status"
echo ""
echo "Mail: it is store-and-forward, so Bob can be locked or offline."
echo "  Alice's mailbox:  curl --unix-socket $SOCK_A http://localhost/api/mail"
echo "  Bob's mailbox:    curl --unix-socket $SOCK_B http://localhost/api/mail"
echo "  Drain Bob's queue: curl -X POST --unix-socket $SOCK_B http://localhost/api/recv"
if [[ "$RELAY_IS_LOCAL" == 1 ]]; then
  if [[ -n "$RELAY_PID" ]]; then
    echo "  Kill the relay ($RELAY_PID) to watch sends queue in the Outbox, then restart it"
    echo "  and the daemon's 30s outbox loop delivers them."
  else
    echo "  Kill the local relay process to watch sends queue in the Outbox and go out on retry."
  fi
else
  echo "  The relay is remote ($RELAY_URL); stop it on its host to watch sends queue."
fi
echo ""
echo "Logs: $LOG_DIR"
echo "Press Ctrl+C here to stop the relay, daemons, and UI this run started."
echo ""

if [[ ${#PIDS[@]} -eq 0 ]]; then
  echo "Everything this stack needs was already running; this run started nothing to supervise."
  exit 0
fi
wait -n
echo "A process exited; shutting down…"

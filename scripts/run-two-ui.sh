#!/usr/bin/env bash
# Two Hush UI windows: Alice (peer-a) and Bob (peer-b), each with its own daemon.
#
# Safe to re-run. A relay or daemon that is already up is reused instead of being
# started a second time, so this doubles as "just reopen the windows": a second
# daemon on a live socket would fail to bind and take the whole run down with it.
#
# The daemons listen on Unix sockets by default (DR-02). Set HUSH_DAEMON_A_ADDR /
# HUSH_DAEMON_B_ADDR to also open a TCP listener (so a browser on another machine
# can reach a daemon).
#
# --dev skips the UI and desktop builds entirely: it starts the relay and both
# daemons with a TCP listener plus one Vite dev server, and prints a browser URL
# per peer. Edits under apps/hush-tauri/src hot reload; only crates/ changes need
# a rebuild, and then only the daemon (this script does that part).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEV=0
case "${1:-}" in
  --dev) DEV=1 ;;
  "") ;;
  -h | --help)
    sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "Unknown argument: $1 (the only option is --dev)." >&2
    exit 2
    ;;
esac
export PROTOC="${PROTOC:-${ROOT}/.tools/protoc-dist/bin/protoc}"
PEER_A="${ROOT}/instances/peer-a"
PEER_B="${ROOT}/instances/peer-b"
RELAY_URL="${HUSH_RELAY_URL:-http://127.0.0.1:8787}"

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
DAEMON_BIN="${BIN}/hush-daemon"
TAURI_BIN="${BIN}/hush-tauri"

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

if [[ ! -d "$PEER_A" || ! -d "$PEER_B" ]]; then
  echo "Creating peer vaults (./scripts/setup-two-peers.sh)…"
  "$ROOT/scripts/setup-two-peers.sh"
fi

if [[ "$DEV" == 1 ]]; then
  echo "Dev mode: building the relay and daemon only (no UI or desktop build)."
  cargo build -p hush-relay -p hush-daemon
else
  echo "Building UI (Vite)…"
  (cd "${ROOT}/apps/hush-tauri" && npm run build)
  echo "Building Rust binaries…"
  cargo build -p hush-relay -p hush-daemon -p hush-tauri
fi

REQUIRED_BINS=("$RELAY_BIN" "$DAEMON_BIN")
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

if relay_ready; then
  echo "Relay already up at $RELAY_URL"
else
  if port_busy "${RELAY_URL#http://}"; then
    echo "Port ${RELAY_URL#http://} is in use but the relay did not respond — fix or stop that process." >&2
    exit 1
  fi
  echo "Starting relay at $RELAY_URL …"
  HUSH_RELAY_DB="${ROOT}/instances/relay/relay.db" HUSH_RELAY_ADDR="${RELAY_URL#http://}" \
    "$RELAY_BIN" >"$LOG_DIR/relay.log" 2>&1 &
  RELAY_PID=$!
  PIDS+=("$RELAY_PID")
  for _ in $(seq 1 100); do
    relay_ready && break
    kill -0 "$RELAY_PID" 2>/dev/null || break
    sleep 0.1
  done
  if ! relay_ready; then
    echo "Relay did not come up on $RELAY_URL:" >&2
    tail -n 20 "$LOG_DIR/relay.log" >&2 || true
    exit 1
  fi
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
  HUSH_RUNTIME_DIR="$RUN_DIR" "$DAEMON_BIN" --dir "$dir" --socket "$sock" ${addr:+--addr "$addr"} \
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
    echo "Port ${VITE_PORT} is already in use — stop that process (a stale dev server?) first." >&2
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
    curl -sf -o /dev/null "$url/" && break
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
  echo "  cargo build -p hush-daemon   # then Ctrl+C here and run $0 --dev"
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
  HUSH_API_URL="unix://${SOCK_A}" HUSH_WINDOW_TITLE="Hush — Alice" \
    "$TAURI_BIN" >"$LOG_DIR/alice-ui.log" 2>&1 &
  UI_PIDS+=($!)
  HUSH_API_URL="unix://${SOCK_B}" HUSH_WINDOW_TITLE="Hush — Bob" \
    "$TAURI_BIN" >"$LOG_DIR/bob-ui.log" 2>&1 &
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
if [[ -n "$RELAY_PID" ]]; then
  echo "  Kill the relay ($RELAY_PID) to watch sends queue in the Outbox, then restart it"
  echo "  and the daemon's 30s outbox loop delivers them."
else
  echo "  Kill the relay process to watch sends queue in the Outbox and go out on retry."
fi
echo ""
echo "Logs: $LOG_DIR"
echo "Press Ctrl+C here to stop the relay, daemons, and UI this run started."
echo ""

wait -n
echo "A process exited; shutting down…"

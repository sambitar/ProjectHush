#!/usr/bin/env bash
# Two Hush UI windows: Alice (peer-a) and Bob (peer-b), each with its own daemon.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${PROTOC:-${ROOT}/.tools/protoc-dist/bin/protoc}"
PEER_A="${ROOT}/instances/peer-a"
PEER_B="${ROOT}/instances/peer-b"
RELAY_URL="${HUSH_RELAY_URL:-http://127.0.0.1:8787}"
DAEMON_A="${HUSH_DAEMON_A_ADDR:-127.0.0.1:9876}"
DAEMON_B="${HUSH_DAEMON_B_ADDR:-127.0.0.1:9877}"
PASS_A="${HUSH_PEER_A_PASS:-peer-a-pass}"
PASS_B="${HUSH_PEER_B_PASS:-peer-b-pass}"

BIN="${ROOT}/target/debug"
RELAY_BIN="${BIN}/hush-relay"
DAEMON_BIN="${BIN}/hush-daemon"
TAURI_BIN="${BIN}/hush-tauri"

if [[ ! -d "$PEER_A" || ! -d "$PEER_B" ]]; then
  echo "Creating peer vaults (./scripts/setup-two-peers.sh)…"
  "$ROOT/scripts/setup-two-peers.sh"
fi

echo "Building UI (Vite)…"
(cd "${ROOT}/apps/hush-tauri" && npm run build)
echo "Building Rust binaries…"
cargo build -p hush-relay -p hush-daemon -p hush-tauri

port_busy() {
  local addr="$1"
  ss -ltn 2>/dev/null | grep -q ":${addr##*:} " && return 0
  return 1
}

RELAY_PID=""
if ! curl -s -o /dev/null --connect-timeout 1 "$RELAY_URL/" 2>/dev/null; then
  if port_busy "${RELAY_URL#http://}"; then
    echo "Port ${RELAY_URL#http://} is in use but relay did not respond — fix or stop that process."
    exit 1
  fi
  echo "Starting relay at $RELAY_URL …"
  HUSH_RELAY_DB="${ROOT}/instances/relay/relay.db" HUSH_RELAY_ADDR="${RELAY_URL#http://}" "$RELAY_BIN" &
  RELAY_PID=$!
  sleep 0.5
else
  echo "Relay already up at $RELAY_URL"
fi

PIDS=()

cleanup() {
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  [[ -n "$RELAY_PID" ]] && kill "$RELAY_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

for pair in "$DAEMON_A:$PEER_A" "$DAEMON_B:$PEER_B"; do
  addr="${pair%%:*}"
  dir="${pair#*:}"
  if port_busy "$addr"; then
    echo "Port $addr already in use — stop the old hush-daemon or set HUSH_DAEMON_A_ADDR / HUSH_DAEMON_B_ADDR."
    exit 1
  fi
done

echo "Starting daemons on $DAEMON_A and $DAEMON_B …"
HUSH_DAEMON_ADDR="$DAEMON_A" "$DAEMON_BIN" --dir "$PEER_A" &
PIDS+=($!)
HUSH_DAEMON_ADDR="$DAEMON_B" "$DAEMON_BIN" --dir "$PEER_B" &
PIDS+=($!)

for addr in "$DAEMON_A" "$DAEMON_B"; do
  for _ in $(seq 1 30); do
    if curl -sf -o /dev/null "http://${addr}/api/status" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
done

echo ""
echo "Opening two UI windows. Unlock with:"
echo "  Alice ($DAEMON_A): passphrase $PASS_A  → contact bob"
echo "  Bob   ($DAEMON_B): passphrase $PASS_B  → contact alice"
echo "  Both sides must add each other's invite (Copy my invite)."
echo ""
echo "Press Ctrl+C here to stop daemons."
echo ""

HUSH_API_URL="http://${DAEMON_A}" HUSH_WINDOW_TITLE="Hush — Alice" "$TAURI_BIN" &
PIDS+=($!)
HUSH_API_URL="http://${DAEMON_B}" HUSH_WINDOW_TITLE="Hush — Bob" "$TAURI_BIN" &
PIDS+=($!)

wait -n
echo "A process exited; shutting down…"

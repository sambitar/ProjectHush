#!/usr/bin/env bash
# One Hush desktop window: one vault, one daemon, one local relay when needed.
#
# The single-instance counterpart of run-two-ui.sh. Safe to re-run: a relay or
# daemon that is already up is reused, and a fresh dist is embedded into the
# desktop binary before it starts (the dist is baked in at compile time, so a
# UI change without this rebuild shows the old interface).
#
# Usage:
#   ./scripts/run-one-ui.sh                # vault instances/peer-a
#   VAULT_DIR=instances/peer-b ./scripts/run-one-ui.sh
#   ./scripts/run-one-ui.sh --no-relay     # vault's stored relay is used as-is
#
# The desktop binary reads HUSH_API_URL (unix socket) and HUSH_WINDOW_TITLE.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PROTOC="${PROTOC:-${ROOT}/.tools/protoc-dist/bin/protoc}"

NO_RELAY=0
[[ "${1:-}" == "--no-relay" ]] && NO_RELAY=1

VAULT_DIR="${VAULT_DIR:-${ROOT}/instances/peer-a}"
RUN_DIR="${HUSH_RUNTIME_DIR:-${XDG_RUNTIME_DIR:-/tmp}/hush-one}"
SOCK="${HUSH_DAEMON_SOCKET:-${RUN_DIR}/hush.sock}"
LOG_DIR="${RUN_DIR}/logs"
mkdir -p "$RUN_DIR" "$LOG_DIR"
chmod 700 "$RUN_DIR" 2>/dev/null || true

[[ -d "$VAULT_DIR" ]] || { echo "No vault at $VAULT_DIR — create one first (hush-cli init)." >&2; exit 1; }

echo "Building UI (Vite) and desktop binary…"
(cd apps/hush-tauri && npm run build)
cargo build -p hush-relay -p hush-tauri -p hush-cli
# The daemon runs in release. Its vault store encrypts multi-GB torrent files
# chunk by chunk, and AEAD is ~40x slower in a debug build (measured 1.7 MiB/s
# debug vs 73 MiB/s release), which made large downloads look stalled at 100%
# for ~17 minutes. The relay and desktop stay in debug: neither is CPU-bound.
cargo build --release -p hush-daemon

DAEMON_BIN="$ROOT/target/release/hush-daemon"
TAURI_BIN="$ROOT/target/debug/hush-tauri"

daemon_ready() {
  curl -sf -o /dev/null --unix-socket "$1" http://localhost/api/status 2>/dev/null
}

# Which relay does this vault talk to? The one stored in the vault — never a
# local one unless you ask for it (peer vaults created against the loopback
# relay by setup-two-peers.sh): WITH_LOCAL_RELAY=1.
WITH_LOCAL_RELAY="${WITH_LOCAL_RELAY:-0}"

RELAY_PID=""
if [[ "$NO_RELAY" -eq 1 ]]; then
  echo "Skipping the local relay (--no-relay)."
elif [[ "$WITH_LOCAL_RELAY" -eq 1 ]]; then
  RELAY_URL="${HUSH_RELAY_URL:-http://127.0.0.1:8787}"
  relay_host="${RELAY_URL#*://}"; relay_host="${relay_host%%:*}"
  case "$relay_host" in
    127.0.0.1|localhost|::1)
      if curl -sf -o /dev/null --connect-timeout 1 "${RELAY_URL%/}/health"; then
        echo "Local relay already up at $RELAY_URL"
      else
        echo "Starting local relay at $RELAY_URL …"
        HUSH_RELAY_DB="$ROOT/instances/relay/relay.db" \
          HUSH_RELAY_ADDR="${RELAY_URL#*://}" \
          "$ROOT/target/debug/hush-relay" >"$LOG_DIR/relay.log" 2>&1 &
        RELAY_PID=$!
        for _ in $(seq 1 100); do
          curl -sf -o /dev/null --connect-timeout 1 "${RELAY_URL%/}/health" && break
          sleep 0.1
        done
        curl -sf -o /dev/null --connect-timeout 1 "${RELAY_URL%/}/health" \
          || { echo "Relay did not come up:" >&2; tail -n 20 "$LOG_DIR/relay.log" >&2; exit 1; }
      fi
      ;;
    *)
      echo "Using relay at $RELAY_URL (no local relay started)."
      ;;
  esac
else
  echo "Using the relay stored in the vault (no local relay started; WITH_LOCAL_RELAY=1 to run one)."
fi

# The daemon is RESTARTED on every run, never reused: it serves the API the UI
# was compiled against, so a stale daemon (from before a rebuild) silently
# serves old responses — e.g. a /api/relay without fields the new UI reads.
# State lives in the vault on disk, so a restart is free.
if daemon_ready "$SOCK"; then
  echo "Stopping the stale daemon on $SOCK…"
  # Match the vault dir in both absolute and relative form: a daemon started by
  # hand with a relative --dir or binary path (e.g. `./target/debug/hush-daemon
  # --dir instances/peer-a`) does not match the absolute path, so an exact
  # command-line match misses it. With the vault lock in place a second daemon
  # now refuses to start, so missing the stale one would fail this script.
  VAULT_DIR_REL="${VAULT_DIR#"$ROOT"/}"
  pkill -f "hush-daemon.*--dir ($VAULT_DIR|$VAULT_DIR_REL)( |$)" 2>/dev/null || true
  for _ in $(seq 1 50); do
    daemon_ready "$SOCK" || break
    sleep 0.1
  done
fi
rm -f "$SOCK" 2>/dev/null || true
# The relay stored in the vault is the runtime source of truth; an inherited
# HUSH_RELAY_URL would override it while the vault is locked.
env -u HUSH_RELAY_URL HUSH_RUNTIME_DIR="$RUN_DIR" \
  "$DAEMON_BIN" --dir "$VAULT_DIR" --socket "$SOCK" >"$LOG_DIR/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 100); do
  daemon_ready "$SOCK" && break
  kill -0 "$DAEMON_PID" 2>/dev/null || { tail -n 20 "$LOG_DIR/daemon.log" >&2; exit 1; }
  sleep 0.1
done
daemon_ready "$SOCK" || { echo "Daemon did not come up:" >&2; tail -n 20 "$LOG_DIR/daemon.log" >&2; exit 1; }
echo "Daemon started (pid $DAEMON_PID, log $LOG_DIR/daemon.log)"

echo "Opening the desktop window…"
HUSH_API_URL="unix://$SOCK" HUSH_WINDOW_TITLE="Hush — one" \
  "$TAURI_BIN" >"$LOG_DIR/ui.log" 2>&1 &
UI_PID=$!
sleep 2
kill -0 "$UI_PID" 2>/dev/null || { echo "UI exited at startup:" >&2; tail -n 20 "$LOG_DIR/ui.log" >&2; exit 1; }

echo ""
echo "Window is up. Unlock with the vault's passphrase (peer vaults: peer-a-pass)."
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

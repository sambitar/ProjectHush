#!/usr/bin/env bash
# DR-02 smoke: the daemon API is a Unix socket in the runtime dir, TCP is opt-in.
#
# Checks permissions, socket lifecycle (clean exit, stale replacement, live-socket
# protection), that a non-socket file is never deleted, and that the UI's whole API
# surface works over the socket.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
T="${ROOT}/.tmp-socket-smoke"
R="${ROOT}/.tmp-socket-smoke-rt"
SOCK="$R/hush-daemon.sock"
rm -rf "$T" "$R"
mkdir -p "$T" "$R"
unset HUSH_RELAY_URL HUSH_DAEMON_ADDR HUSH_DAEMON_SOCKET HUSH_RUNTIME_DIR XDG_RUNTIME_DIR

cargo build -p hush-daemon -p hush-cli --quiet
CLI="${ROOT}/target/debug/hush-cli"
DMN="${ROOT}/target/debug/hush-daemon"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

HUSH_RUNTIME_DIR="$R" "$CLI" --dir "$T" init --passphrase socket-smoke >/dev/null

DAEMON=""
DAEMON2=""
cleanup() {
  for pid in "$DAEMON" "$DAEMON2"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$T" "$R"
}
trap cleanup EXIT

# --- default: socket only -----------------------------------------------------
HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon.log" 2>&1 &
DAEMON=$!
for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 0.1; done

[[ -S "$SOCK" ]] || fail "daemon did not create $SOCK"
[[ "$(stat -c '%a' "$SOCK")" == "600" ]] || fail "socket mode is $(stat -c '%a' "$SOCK"), want 600"
[[ "$(stat -c '%a' "$R")" == "700" ]] || fail "runtime dir mode is $(stat -c '%a' "$R"), want 700"
pass "socket and runtime dir are 0600 / 0700"

STATUS=$(curl -sf --unix-socket "$SOCK" http://localhost/api/status)
[[ "$STATUS" == *"\"tcp_addr\":null"* ]] || fail "TCP listener is on by default: $STATUS"
pass "status over socket, TCP off by default"

curl -sf -o /dev/null -X POST -H 'Content-Type: application/json' \
  -d '{"passphrase":"socket-smoke"}' --unix-socket "$SOCK" http://localhost/api/unlock
[[ "$(curl -sf --unix-socket "$SOCK" http://localhost/api/status)" == *'"locked":false'* ]] \
  || fail "unlock over socket did not stick"
pass "unlock over socket"

# --- full UI API surface over the socket -------------------------------------
# Relay-dependent paths (presence, send/recv) are covered by the two-peer
# dogfood; this script stays hermetic and needs no relay.
PREFS='{"v":1,"theme":"dark","avatars":{},"muted":[],"blocked":[],"nick_overrides":{},"auto_lock_minutes":15}'
CODE=$(curl -s -o "$T/prefs.out" -w '%{http_code}' -X PUT -H 'Content-Type: application/json' \
  -d "$PREFS" --unix-socket "$SOCK" http://localhost/api/ui-prefs)
[[ "$CODE" == "204" ]] || fail "ui-prefs PUT returned $CODE: $(cat "$T/prefs.out")"
[[ "$(curl -sf --unix-socket "$SOCK" http://localhost/api/ui-prefs)" == *'"theme":"dark"'* ]] \
  || fail "ui-prefs round trip failed over socket"
curl -sf -o /dev/null --unix-socket "$SOCK" http://localhost/api/contacts
curl -sf -o /dev/null --unix-socket "$SOCK" http://localhost/api/messages
pass "contacts / messages / ui-prefs over socket"

# --- clean exit removes the socket -------------------------------------------
kill "$DAEMON" 2>/dev/null || true
wait "$DAEMON" 2>/dev/null || true
[[ -e "$SOCK" ]] && fail "socket file leaked after SIGTERM"
pass "socket removed on shutdown"

# --- a stale socket is replaced ----------------------------------------------
HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon2.log" 2>&1 &
DAEMON2=$!
for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 0.1; done
kill -9 "$DAEMON2" 2>/dev/null || true
wait "$DAEMON2" 2>/dev/null || true
[[ -S "$SOCK" ]] || fail "expected a stale socket after SIGKILL"
HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon3.log" 2>&1 &
DAEMON2=$!
for _ in $(seq 1 60); do curl -sf -o /dev/null --unix-socket "$SOCK" http://localhost/api/status && break; sleep 0.1; done
curl -sf -o /dev/null --unix-socket "$SOCK" http://localhost/api/status || fail "could not rebind over stale socket"
pass "stale socket replaced"

# --- a live socket is not stolen ---------------------------------------------
if HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon4.log" 2>&1; then
  fail "a second daemon took over a live socket"
fi
grep -q "already listening" "$T/daemon4.log" || fail "unexpected second-daemon error: $(cat "$T/daemon4.log")"
curl -sf -o /dev/null --unix-socket "$SOCK" http://localhost/api/status || fail "first daemon stopped serving"
pass "second daemon refused, first still serving"

kill "$DAEMON2" 2>/dev/null || true
wait "$DAEMON2" 2>/dev/null || true

# --- a non-socket file is refused, never deleted ------------------------------
rm -f "$SOCK"
printf 'do not delete me' >"$SOCK"
if HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon5.log" 2>&1; then
  fail "daemon started over a regular file"
fi
[[ "$(cat "$SOCK")" == "do not delete me" ]] || fail "daemon deleted a non-socket file"
pass "regular file at the socket path refused and left intact"

# --- --addr opts TCP back in, alongside the socket ---------------------------
rm -f "$SOCK"
HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" --addr 127.0.0.1:19891 >"$T/daemon6.log" 2>&1 &
DAEMON2=$!
for _ in $(seq 1 60); do curl -sf -o /dev/null http://127.0.0.1:19891/api/status && break; sleep 0.1; done
TCP_STATUS=$(curl -sf http://127.0.0.1:19891/api/status) \
  || fail "TCP listener did not come up: $(cat "$T/daemon6.log")"
[[ "$TCP_STATUS" == *"127.0.0.1:19891"* ]] || fail "tcp_addr missing from status: $TCP_STATUS"
curl -sf -o /dev/null --unix-socket "$SOCK" http://localhost/api/status || fail "socket closed when TCP opened"
pass "--addr enables TCP alongside the socket"

echo "socket smoke ok"

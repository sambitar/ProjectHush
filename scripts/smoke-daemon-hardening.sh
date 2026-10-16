#!/usr/bin/env bash
# DR-07 smoke: the daemon keeps key material out of core dumps and swap.
#
# Verifies, for a running daemon:
#   1. startup log reports core_dumps=off and memory_locked=on
#   2. RLIMIT_CORE is 0 (no core file can be written)
#   3. VmLck > 0 (address space pinned in RAM, so keys cannot be paged out)
#   4. /proc/<pid>/mem is unreadable and root-owned (PR_SET_DUMPABLE=0, so a
#      same-uid process cannot read the daemon's memory or environment)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
T="${ROOT}/.tmp-hardening-smoke"
R="${ROOT}/.tmp-hardening-smoke-rt"
rm -rf "$T" "$R"; mkdir -p "$T" "$R"
unset HUSH_DAEMON_ADDR HUSH_DAEMON_SOCKET HUSH_RUNTIME_DIR XDG_RUNTIME_DIR

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

cargo build -p hush-daemon -p hush-cli --quiet
"${ROOT}/target/debug/hush-cli" --dir "$T" init --passphrase hardening-smoke >/dev/null

DAEMON=""
cleanup() {
  [[ -n "$DAEMON" ]] && kill "$DAEMON" 2>/dev/null || true
  rm -rf "$T" "$R"
}
trap cleanup EXIT

HUSH_RUNTIME_DIR="$R" "${ROOT}/target/debug/hush-daemon" --dir "$T" >"$T/daemon.log" 2>&1 &
DAEMON=$!
for _ in $(seq 1 60); do [ -S "$R/hush-daemon.sock" ] && break; sleep 0.1; done
[ -S "$R/hush-daemon.sock" ] || fail "daemon did not start: $(cat "$T/daemon.log")"

LOG=$(grep -h "hardening" "$T/daemon.log" || true)
[[ "$LOG" == *"core_dumps=off"* ]] || fail "core dumps not disabled: ${LOG:-no hardening line}"
[[ "$LOG" == *"memory_locked=on"* ]] || fail "memory not locked: ${LOG:-no hardening line}"
pass "startup reports core_dumps=off memory_locked=on"

LIMITS=$(awk '/Max core file size/{print $5, $6}' "/proc/$DAEMON/limits")
[[ "$LIMITS" == "0 0" ]] || fail "RLIMIT_CORE not 0 0: '$LIMITS'"
pass "RLIMIT_CORE is 0"

VMLCK=$(awk '/^VmLck/{print $2}' "/proc/$DAEMON/status")
[[ -n "$VMLCK" && "$VMLCK" -gt 0 ]] || fail "VmLck is ${VMLCK:-missing}, expected > 0"
pass "VmLck ${VMLCK} kB pinned in RAM"

if head -c 1 "/proc/$DAEMON/mem" >/dev/null 2>&1; then
  fail "/proc/$DAEMON/mem is readable; PR_SET_DUMPABLE did not take effect"
fi
if head -c 1 "/proc/$DAEMON/environ" >/dev/null 2>&1; then
  fail "/proc/$DAEMON/environ is readable; PR_SET_DUMPABLE did not take effect"
fi
pass "same-uid processes cannot read daemon memory or environment"

echo "hardening smoke ok"

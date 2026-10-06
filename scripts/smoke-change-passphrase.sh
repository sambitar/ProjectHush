#!/usr/bin/env bash
# Passphrase rotation smoke: changing the passphrase re-wraps the master key and
# must not touch anything encrypted by it.
#
# The important property is negative: account.enc, signal.enc, the chat log and
# the recovery wrap are all encrypted with the master key, so if rotation derived
# a new master they would have to be rewritten. We hash them before and after and
# require the bytes to be identical, which is only possible if the master key
# survived. That also keeps the recovery code working, since it wraps that same
# key.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${ROOT}/.tools/protoc-dist/bin/protoc"
T="${ROOT}/.tmp-rekey-smoke"
R="${ROOT}/.tmp-rekey-smoke-rt"
SOCK="$R/hush-daemon.sock"
rm -rf "$T" "$R"
mkdir -p "$T" "$R"
unset HUSH_RELAY_URL HUSH_DAEMON_ADDR HUSH_DAEMON_SOCKET HUSH_RUNTIME_DIR XDG_RUNTIME_DIR

cargo build -p hush-daemon -p hush-cli --quiet
CLI="${ROOT}/target/debug/hush-cli"
DMN="${ROOT}/target/debug/hush-daemon"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

api() { # api <method> <path> [json] -> body, sets CODE
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    CODE=$(curl -s -o "$T/out.json" -w '%{http_code}' -X "$method" \
      -H 'Content-Type: application/json' -d "$body" \
      --unix-socket "$SOCK" "http://localhost$path")
  else
    CODE=$(curl -s -o "$T/out.json" -w '%{http_code}' -X "$method" \
      --unix-socket "$SOCK" "http://localhost$path")
  fi
  cat "$T/out.json"
}

digest() { # digest <file> -> sha256, or "absent"
  [[ -e "$1" ]] && sha256sum "$1" | cut -d' ' -f1 || echo "absent"
}

HUSH_RUNTIME_DIR="$R" "$CLI" --dir "$T" init --passphrase "first-pass" >/dev/null

DAEMON=""
cleanup() {
  [[ -n "$DAEMON" ]] && kill "$DAEMON" 2>/dev/null || true
  rm -rf "$T" "$R"
}
trap cleanup EXIT

# Fingerprint everything the master key protects, plus the recovery wrap.
BEFORE_ACCOUNT=$(digest "$T/account.enc")
BEFORE_SIGNAL=$(digest "$T/signal.enc")
BEFORE_CHAT=$(digest "$T/chat.enc")
BEFORE_RECOVERY=$(digest "$T/recovery.enc")
BEFORE_IDENTITY=$(digest "$T/identity.secret")
BEFORE_LOCK=$(digest "$T/vault.lock.json")
[[ "$BEFORE_RECOVERY" != "absent" ]] || fail "init did not write a recovery wrap"
pass "vault created; master-protected files fingerprinted"

HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon.log" 2>&1 &
DAEMON=$!
for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 0.1; done
[[ -S "$SOCK" ]] || fail "daemon did not come up: $(cat "$T/daemon.log")"

# --- locked: rotation must be refused ----------------------------------------
api POST /api/change-passphrase '{"current_passphrase":"first-pass","new_passphrase":"second-pass"}' >/dev/null
[[ "$CODE" == "401" ]] || fail "rotation while locked returned $CODE, want 401"
pass "rotation refused while locked (401)"

api POST /api/unlock '{"passphrase":"first-pass"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "unlock returned $CODE: $(cat "$T/out.json")"

# --- wrong current passphrase is rejected ------------------------------------
api POST /api/change-passphrase '{"current_passphrase":"not-the-pass","new_passphrase":"second-pass"}' >/dev/null
[[ "$CODE" == "403" ]] || fail "wrong current passphrase returned $CODE, want 403: $(cat "$T/out.json")"
pass "wrong current passphrase rejected (403)"

# --- the real rotation --------------------------------------------------------
api POST /api/change-passphrase '{"current_passphrase":"first-pass","new_passphrase":"second-pass"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "rotation returned $CODE: $(cat "$T/out.json")"
pass "passphrase rotated (204)"

# --- nothing the master key protects was rewritten ---------------------------
for pair in "account.enc:$BEFORE_ACCOUNT" "signal.enc:$BEFORE_SIGNAL" "chat.enc:$BEFORE_CHAT" \
            "recovery.enc:$BEFORE_RECOVERY" "identity.secret:$BEFORE_IDENTITY"; do
  file="${pair%%:*}"; want="${pair##*:}"
  got=$(digest "$T/$file")
  [[ "$got" == "$want" ]] || fail "$file changed during rotation (master key moved?)"
done
pass "master-protected files byte-identical: no re-encryption, recovery intact"

# The header itself must change, or the passphrase did not actually change.
[[ "$(digest "$T/vault.lock.json")" != "$BEFORE_LOCK" ]] || fail "vault.lock.json was not rewritten"
pass "vault.lock.json rewritten with a new salt and wrap"

# --- session survives rotation ------------------------------------------------
api GET /api/status >/dev/null
[[ "$(cat "$T/out.json")" == *'"locked":false'* ]] || fail "rotation locked the vault"
pass "still unlocked after rotation (no re-unlock needed)"

# --- old passphrase is dead, new one works, data still decrypts ---------------
api POST /api/lock >/dev/null
[[ "$CODE" == "204" ]] || fail "lock returned $CODE"

api POST /api/unlock '{"passphrase":"first-pass"}' >/dev/null
[[ "$CODE" == "401" ]] || fail "old passphrase still unlocked the vault ($CODE)"
pass "old passphrase no longer works (401)"

api POST /api/unlock '{"passphrase":"second-pass"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "new passphrase did not unlock ($CODE): $(cat "$T/out.json")"
pass "new passphrase unlocks (204)"

# account.enc / chat.enc must still decrypt under the re-wrapped master key.
api GET /api/status >/dev/null
[[ "$(cat "$T/out.json")" == *'"locked":false'* ]] || fail "unlock did not stick"
api GET /api/messages >/dev/null
[[ "$CODE" == "200" ]] || fail "/api/messages failed after rotation ($CODE): $(cat "$T/out.json")"
api GET /api/contacts >/dev/null
[[ "$CODE" == "200" ]] || fail "/api/contacts failed after rotation ($CODE)"
pass "account state and chat log still decrypt after re-unlock"

# --- empty new passphrase is refused -----------------------------------------
api POST /api/change-passphrase '{"current_passphrase":"second-pass","new_passphrase":""}' >/dev/null
[[ "$CODE" == "400" ]] || fail "empty new passphrase returned $CODE, want 400"
pass "empty new passphrase refused (400)"

echo "change-passphrase smoke ok"

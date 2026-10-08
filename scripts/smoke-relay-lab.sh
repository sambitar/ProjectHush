#!/usr/bin/env bash
# Cross-machine smoke: two LOCAL daemons talking to a REMOTE hush relay.
#
# Unlike smoke-mail.sh, this does not start a relay. It points both peers at a
# relay that is already running elsewhere (the lab VPS by default) and proves
# the whole store-and-forward path works over the public internet.
#
# The client refuses plain-HTTP relays that are not on loopback in a RELEASE
# build (hush-core HttpPolicy::LoopbackOnly). So this uses DEBUG binaries: debug
# builds allow plain HTTP anywhere, which is the only way to test against a
# plain-HTTP lab relay without TLS. See docs/ops/relay-lab-vps.md.
#
# Usage:
#   HUSH_RELAY_URL=http://137.184.129.205:8790 ./scripts/smoke-relay-lab.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${PROTOC:-${ROOT}/.tools/protoc-dist/bin/protoc}"
RELAY_URL="${HUSH_RELAY_URL:-http://137.184.129.205:8790}"

T="${ROOT}/.tmp-relay-lab"
R="${ROOT}/.tmp-relay-lab-rt"
rm -rf "$T" "$R"
mkdir -p "$T" "$R"
unset HUSH_DAEMON_ADDR HUSH_DAEMON_SOCKET HUSH_RUNTIME_DIR XDG_RUNTIME_DIR

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

echo "relay under test: $RELAY_URL"
curl -sS --max-time 10 -o /dev/null -w "relay /health -> %{http_code}\n" "$RELAY_URL/health" \
  || fail "relay $RELAY_URL is not reachable from this machine"

cargo build -p hush-cli -p hush-daemon --quiet
CLI="${ROOT}/target/debug/hush-cli"
DMN="${ROOT}/target/debug/hush-daemon"
[[ -x "$CLI" && -x "$DMN" ]] || fail "debug binaries missing"

DAEMON_A=""; DAEMON_B=""
cleanup() {
  for pid in "$DAEMON_A" "$DAEMON_B"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$T" "$R"
}
trap cleanup EXIT

VA="$T/a"; VB="$T/b"
RA="$R/a"; RB="$R/b"
SOCK_A="$RA/hush-daemon.sock"; SOCK_B="$RB/hush-daemon.sock"
mkdir -p "$VA" "$VB" "$RA" "$RB"

RESP=""
api_at() { # api_at <socket> <method> <path> [json]
  local sock="$1" method="$2" path="$3" body="${4:-}"
  if [[ -n "$body" ]]; then
    CODE=$(curl -s -o "$T/out.json" -w '%{http_code}' -X "$method" \
      -H 'Content-Type: application/json' -d "$body" \
      --unix-socket "$sock" "http://localhost$path")
  else
    CODE=$(curl -s -o "$T/out.json" -w '%{http_code}' -X "$method" \
      --unix-socket "$sock" "http://localhost$path")
  fi
  RESP=$(cat "$T/out.json")
}

# --- vaults created against the REMOTE relay ---------------------------------
HUSH_PASSPHRASE=pa "$CLI" --dir "$VA" init --passphrase pa --relay "$RELAY_URL" \
  | grep -q "relay: $RELAY_URL" || fail "vault A did not record the remote relay"
HUSH_PASSPHRASE=pb "$CLI" --dir "$VB" init --passphrase pb --relay "$RELAY_URL" \
  | grep -q "relay: $RELAY_URL" || fail "vault B did not record the remote relay"
pass "both vaults created with relay $RELAY_URL"

# Show the effective relay the daemon will use (config surface, DR-14).
HUSH_PASSPHRASE=pa "$CLI" --dir "$VA" config show | sed 's/^/    /'

# --- two local daemons -------------------------------------------------------
HUSH_RUNTIME_DIR="$RA" "$DMN" --dir "$VA" >"$T/daemon-a.log" 2>&1 &
DAEMON_A=$!
HUSH_RUNTIME_DIR="$RB" "$DMN" --dir "$VB" >"$T/daemon-b.log" 2>&1 &
DAEMON_B=$!
for _ in $(seq 1 60); do [ -S "$SOCK_A" ] && [ -S "$SOCK_B" ] && break; sleep 0.1; done
[[ -S "$SOCK_A" && -S "$SOCK_B" ]] || fail "peer daemons did not come up"

api_at "$SOCK_A" POST /api/unlock '{"passphrase":"pa"}'
[[ "$CODE" == "204" ]] || fail "peer A unlock returned $CODE"
api_at "$SOCK_B" POST /api/unlock '{"passphrase":"pb"}'
[[ "$CODE" == "204" ]] || fail "peer B unlock returned $CODE"
pass "two local daemons up, both pointed at the remote relay"

# --- contact exchange over the remote relay ----------------------------------
api_at "$SOCK_B" POST /api/mail/handle '{"handle":"bob"}'
[[ "$CODE" == "204" ]] || fail "peer B set handle returned $CODE"
api_at "$SOCK_B" POST /api/invite
INV_B=$(printf '%s' "$RESP" | sed -n 's/.*"invite":"\([^"]*\)".*/\1/p')
api_at "$SOCK_A" POST /api/invite
INV_A=$(printf '%s' "$RESP" | sed -n 's/.*"invite":"\([^"]*\)".*/\1/p')
[[ -n "$INV_A" && -n "$INV_B" ]] || fail "could not read peer invites"
api_at "$SOCK_A" POST /api/contacts/add "{\"nickname\":\"bob\",\"invite\":\"$INV_B\"}"
[[ "$CODE" == "204" ]] || fail "A add-contact returned $CODE"
api_at "$SOCK_B" POST /api/contacts/add "{\"nickname\":\"alice\",\"invite\":\"$INV_A\"}"
[[ "$CODE" == "204" ]] || fail "B add-contact returned $CODE"
pass "invite/contact exchange completed through the remote relay"

# --- mail: store-and-forward while the recipient is offline ------------------
api_at "$SOCK_B" POST /api/mail/send \
  '{"to":"alice","subject":"cross-machine","body":"hello over the public internet","attachments":[]}'
[[ "$CODE" == "200" ]] || fail "mail send returned $CODE: $RESP"
THREAD=$(printf '%s' "$RESP" | sed -n 's/.*"thread_id":"\([^"]*\)".*/\1/p')
[[ -n "$THREAD" ]] || fail "mail send returned no thread id: $RESP"
pass "mail accepted from B and queued on the remote relay"

api_at "$SOCK_A" POST /api/recv
RECV="$(cat "$T/out.json")"
[[ "$RECV" == *'"kind":"mail"'* ]] || fail "A did not receive mail: $RECV"
[[ "$RECV" == *"hello over the public internet"* ]] || fail "A received the wrong body: $RECV"
[[ "$RECV" == *"\"thread_id\":\"$THREAD\""* ]] || fail "A lost the thread id: $RECV"
pass "A received the mail that travelled through the remote relay"

# --- reply in the other direction --------------------------------------------
api_at "$SOCK_A" POST /api/mail/send \
  "{\"to\":\"bob\",\"thread_id\":\"$THREAD\",\"subject\":\"Re: cross-machine\",\"body\":\"reply over the relay\",\"attachments\":[]}"
[[ "$CODE" == "200" ]] || fail "reply send returned $CODE: $RESP"
api_at "$SOCK_B" POST /api/recv
[[ "$(cat "$T/out.json")" == *"reply over the relay"* ]] || fail "B did not receive the reply"
pass "reply travelled back through the remote relay"

echo "cross-machine relay smoke ok"

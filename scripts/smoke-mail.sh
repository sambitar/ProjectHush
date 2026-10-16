#!/usr/bin/env bash
# Internal mail smoke: set a handle, read it back, reject an invalid one.
#
# The handle is a label bound to a key, not a directory entry: this checks the
# daemon surface (status + contacts + the set endpoint) without a relay, so it
# stays hermetic like the other smoke scripts.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
T="${ROOT}/.tmp-mail-smoke"
R="${ROOT}/.tmp-mail-smoke-rt"
SOCK="$R/hush-daemon.sock"
rm -rf "$T" "$R"
mkdir -p "$T" "$R"
unset HUSH_RELAY_URL HUSH_DAEMON_ADDR HUSH_DAEMON_SOCKET HUSH_RUNTIME_DIR XDG_RUNTIME_DIR

cargo build -p hush-daemon -p hush-cli -p hush-relay --quiet
CLI="${ROOT}/target/debug/hush-cli"
DMN="${ROOT}/target/debug/hush-daemon"
RELAY="${ROOT}/target/debug/hush-relay"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

RESP=""
api_at() { # api_at <socket> <method> <path> [json] -> prints body, sets CODE and RESP
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
  printf '%s' "$RESP"
}

api() { api_at "$SOCK" "$@"; }

HUSH_RUNTIME_DIR="$R" "$CLI" --dir "$T" init --passphrase "pw" >/dev/null

DAEMON=""
DAEMON_A=""
DAEMON_B=""
RELAY_PID=""
cleanup() {
  for pid in "$DAEMON" "$DAEMON_A" "$DAEMON_B" "$RELAY_PID"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$T" "$R"
}
trap cleanup EXIT

HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" >"$T/daemon.log" 2>&1 &
DAEMON=$!
for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 0.1; done
[[ -S "$SOCK" ]] || fail "daemon did not come up: $(cat "$T/daemon.log")"

# --- locked: setting a handle must be refused --------------------------------
api POST /api/mail/handle '{"handle":"sandy"}' >/dev/null
[[ "$CODE" == "401" ]] || fail "set handle while locked returned $CODE, want 401"
pass "setting a handle is refused while locked (401)"

api POST /api/unlock '{"passphrase":"pw"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "unlock returned $CODE: $(cat "$T/out.json")"

# --- no handle yet, and the domain is always advertised ----------------------
api GET /api/status >/dev/null
[[ "$(cat "$T/out.json")" == *'"mail_domain":"myserver.com"'* ]] || fail "mail_domain missing from status"
[[ "$(cat "$T/out.json")" == *'"mail_handle":null'* ]] || fail "a fresh vault must have no handle"
pass "fresh vault has no handle and advertises the domain"

# --- set a handle, read it back ----------------------------------------------
api POST /api/mail/handle '{"handle":"  Sandy "}' >/dev/null
[[ "$CODE" == "204" ]] || fail "set handle returned $CODE: $(cat "$T/out.json")"
api GET /api/status >/dev/null
[[ "$(cat "$T/out.json")" == *'"mail_handle":"sandy"'* ]] || fail "handle not normalized/read back: $(cat "$T/out.json")"
pass "handle set, normalized to lowercase, and read back"

# --- invalid handle is rejected with 400 -------------------------------------
api POST /api/mail/handle '{"handle":"bad handle"}' >/dev/null
[[ "$CODE" == "400" ]] || fail "invalid handle returned $CODE, want 400: $(cat "$T/out.json")"
pass "invalid handle rejected (400)"

# The rejected write must not have clobbered the stored handle.
api GET /api/status >/dev/null
[[ "$(cat "$T/out.json")" == *'"mail_handle":"sandy"'* ]] || fail "a rejected handle change mutated state"
pass "rejected handle change left the stored handle intact"

# --- two peers: mail is queued while the recipient is offline ----------------
# Chat needs the recipient to heartbeat presence; mail deliberately does not,
# so this proves the store-and-forward path end to end over a real relay.
PORT=19877
RELAY_URL="http://127.0.0.1:$PORT"
VA="$T/a"; VB="$T/b"
RA="$R/a"; RB="$R/b"
SOCK_A="$RA/hush-daemon.sock"; SOCK_B="$RB/hush-daemon.sock"
mkdir -p "$VA" "$VB" "$RA" "$RB"

HUSH_RELAY_DB="$T/relay.db" HUSH_RELAY_ADDR="127.0.0.1:$PORT" "$RELAY" >"$T/relay.log" 2>&1 &
RELAY_PID=$!
for _ in $(seq 1 60); do curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done

HUSH_PASSPHRASE=pa "$CLI" --dir "$VA" init --passphrase pa --relay "$RELAY_URL" >/dev/null
HUSH_PASSPHRASE=pb "$CLI" --dir "$VB" init --passphrase pb --relay "$RELAY_URL" >/dev/null

HUSH_RUNTIME_DIR="$RA" "$DMN" --dir "$VA" >"$T/daemon-a.log" 2>&1 &
DAEMON_A=$!
HUSH_RUNTIME_DIR="$RB" "$DMN" --dir "$VB" >"$T/daemon-b.log" 2>&1 &
DAEMON_B=$!
for _ in $(seq 1 60); do [ -S "$SOCK_A" ] && [ -S "$SOCK_B" ] && break; sleep 0.1; done
[[ -S "$SOCK_A" && -S "$SOCK_B" ]] || fail "peer daemons did not come up"

api_at "$SOCK_A" POST /api/unlock '{"passphrase":"pa"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "peer A unlock returned $CODE"
api_at "$SOCK_B" POST /api/unlock '{"passphrase":"pb"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "peer B unlock returned $CODE"

# Bob picks a handle before inviting, so Alice learns it from his invite.
api_at "$SOCK_B" POST /api/mail/handle '{"handle":"bob"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "peer B set handle returned $CODE"
api_at "$SOCK_B" POST /api/invite >/dev/null
INV_B=$(printf '%s' "$RESP" | sed -n 's/.*"invite":"\([^"]*\)".*/\1/p')
api_at "$SOCK_A" POST /api/invite >/dev/null
INV_A=$(printf '%s' "$RESP" | sed -n 's/.*"invite":"\([^"]*\)".*/\1/p')
[[ -n "$INV_A" && -n "$INV_B" ]] || fail "could not read peer invites"
api_at "$SOCK_A" POST /api/contacts/add "{\"nickname\":\"bob\",\"invite\":\"$INV_B\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "A add-contact returned $CODE"
api_at "$SOCK_B" POST /api/contacts/add "{\"nickname\":\"alice\",\"invite\":\"$INV_A\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "B add-contact returned $CODE"

api_at "$SOCK_A" GET /api/contacts >/dev/null
[[ "$(cat "$T/out.json")" == *'"mail_handle":"bob"'* ]] || fail "Alice did not learn bob's handle: $(cat "$T/out.json")"
pass "handle published in an invite reaches the other peer's contact list"

# Alice never heartbeats: she is offline. Live chat would be refused here.
api_at "$SOCK_B" POST /api/send '{"to":"alice","text":"live"}' >/dev/null
[[ "$CODE" == "400" ]] || fail "chat to an offline peer returned $CODE, want 400"
pass "chat still refuses an offline recipient (400)"

api_at "$SOCK_B" POST /api/mail/send \
  '{"to":"alice","subject":"Q3","body":"see attached","attachments":[{"name":"q3.txt","data_base64":"cmV2ZW51ZQ=="}]}' \
  >/dev/null
[[ "$CODE" == "200" ]] || fail "mail send returned $CODE: $RESP"
THREAD=$(printf '%s' "$RESP" | sed -n 's/.*"thread_id":"\([^"]*\)".*/\1/p')
[[ -n "$THREAD" ]] || fail "mail send returned no thread id: $RESP"
pass "mail to an offline peer is accepted and filed under a thread"

api_at "$SOCK_B" GET /api/mail >/dev/null
[[ "$(cat "$T/out.json")" == *"\"thread_id\":\"$THREAD\""* ]] || fail "sent mail not listed: $(cat "$T/out.json")"
pass "sent mail appears on the sender's mail rail"

api_at "$SOCK_A" POST /api/recv >/dev/null
RECV="$(cat "$T/out.json")"
[[ "$RECV" == *'"kind":"mail"'* ]] || fail "offline recipient did not receive mail: $RECV"
[[ "$RECV" == *"\"thread_id\":\"$THREAD\""* ]] || fail "received mail lost its thread id: $RECV"
[[ "$RECV" == *'"subject":"Q3"'* ]] || fail "received mail lost its subject: $RECV"
[[ "$RECV" == *'"name":"q3.txt"'* ]] || fail "received mail lost its attachment: $RECV"
pass "offline recipient receives the mail with thread, subject, and attachment"

# Alice replies in the same thread; Bob sees it under the same id.
api_at "$SOCK_A" POST /api/mail/send \
  "{\"to\":\"bob\",\"thread_id\":\"$THREAD\",\"subject\":\"Re: Q3\",\"body\":\"got it\",\"attachments\":[]}" >/dev/null
[[ "$CODE" == "200" ]] || fail "reply send returned $CODE: $(cat "$T/out.json")"
api_at "$SOCK_B" POST /api/recv >/dev/null
[[ "$(cat "$T/out.json")" == *"\"thread_id\":\"$THREAD\""* ]] || fail "reply did not land in the same thread: $(cat "$T/out.json")"
pass "a reply lands in the same thread on the other side"

# Attachments must never sit in cleartext on the relay.
if command -v strings >/dev/null 2>&1; then
  if strings "$T/relay.db" 2>/dev/null | grep -q "revenue"; then
    fail "relay stored attachment plaintext"
  fi
  pass "relay disk holds no attachment plaintext"
fi

echo "mail smoke ok"

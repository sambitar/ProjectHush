#!/usr/bin/env bash
# Outsider drop smoke: seal a message for a non-member, park it on the relay, and
# hand the sender a link.
#
# This exercises the real path end to end — daemon -> hush-core seal -> relay
# storage — because the parts that can silently be wrong here are the ones no
# unit test sees: whether the container the daemon uploaded is the one the relay
# stored, and whether the notification that went out leaks the PIN.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${ROOT}/.tools/protoc-dist/bin/protoc"
T="${ROOT}/.tmp-drop-smoke"
R="${ROOT}/.tmp-drop-smoke-rt"
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
CODE=""
api() { # api <method> <path> [json]
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    CODE=$(curl -s -o "$T/out.json" -w '%{http_code}' -X "$method" \
      -H 'Content-Type: application/json' -d "$body" \
      --unix-socket "$SOCK" "http://localhost$path")
  else
    CODE=$(curl -s -o "$T/out.json" -w '%{http_code}' -X "$method" \
      --unix-socket "$SOCK" "http://localhost$path")
  fi
  RESP=$(cat "$T/out.json")
}

PORT=19878
RELAY_URL="http://127.0.0.1:$PORT"
# A base URL that is obviously not real, so a link that "works" cannot be
# accidentally right for the wrong reason.
DROP_BASE="https://drop.example.test"
MAILER_DIR="$T/notifications"

DAEMON=""
RELAY_PID=""
cleanup() {
  for pid in "$DAEMON" "$RELAY_PID"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$T" "$R"
}
trap cleanup EXIT

HUSH_RELAY_DB="$T/relay.db" HUSH_RELAY_ADDR="127.0.0.1:$PORT" "$RELAY" >"$T/relay.log" 2>&1 &
RELAY_PID=$!
for _ in $(seq 1 60); do curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done

HUSH_PASSPHRASE=pw "$CLI" --dir "$T" init --passphrase pw --relay "$RELAY_URL" >/dev/null

HUSH_RUNTIME_DIR="$R" \
  HUSH_DROP_BASE_URL="$DROP_BASE" \
  HUSH_MAILER="file:$MAILER_DIR" \
  "$DMN" --dir "$T" >"$T/daemon.log" 2>&1 &
DAEMON=$!
for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 0.1; done
[[ -S "$SOCK" ]] || fail "daemon did not come up: $(cat "$T/daemon.log")"

# --- locked: a drop must be refused ------------------------------------------
api POST /api/mail/outsider \
  '{"to":"bob@gmail.com","body":"secret","pin":"correct horse"}' >/dev/null
[[ "$CODE" == "401" ]] || fail "drop while locked returned $CODE, want 401: $RESP"
pass "a drop is refused while locked (401)"

api POST /api/unlock '{"passphrase":"pw"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "unlock returned $CODE: $(cat "$T/out.json")"

# --- the daemon re-validates, whatever the form sent -------------------------
api POST /api/mail/outsider '{"to":"bob","body":"secret","pin":"correct horse"}' >/dev/null
[[ "$CODE" == "400" ]] || fail "a malformed address returned $CODE, want 400: $RESP"
[[ "$RESP" == *"email address"* ]] || fail "wrong error for a malformed address: $RESP"
pass "a malformed address is refused by the daemon (400)"

api POST /api/mail/outsider '{"to":"bob@gmail.com","body":"secret","pin":"short"}' >/dev/null
[[ "$CODE" == "400" ]] || fail "a short PIN returned $CODE, want 400: $RESP"
[[ "$RESP" == *"PIN"* ]] || fail "wrong error for a short PIN: $RESP"
pass "a PIN under the floor is refused by the daemon (400)"

# --- a real drop --------------------------------------------------------------
SECRET="the quick brown fox jumps"
api POST /api/mail/outsider \
  "{\"to\":\"Bob@Gmail.com\",\"subject\":\"Q3\",\"body\":\"$SECRET\",\"pin\":\"correct horse\",\"attachments\":[{\"name\":\"q3.txt\",\"data_base64\":\"cmV2ZW51ZQ==\"}]}" \
  >/dev/null
[[ "$CODE" == "200" ]] || fail "drop send returned $CODE: $RESP"
ID=$(printf '%s' "$RESP" | sed -n 's/.*"delivery_id":"\([^"]*\)".*/\1/p')
LINK=$(printf '%s' "$RESP" | sed -n 's/.*"link":"\([^"]*\)".*/\1/p')
EXPIRES=$(printf '%s' "$RESP" | sed -n 's/.*"expires_unix":\([0-9]*\).*/\1/p')
[[ -n "$ID" && -n "$LINK" && -n "$EXPIRES" ]] || fail "incomplete drop response: $RESP"
[[ "$ID" =~ ^[0-9a-f]{32}$ ]] || fail "delivery id is not 32 hex chars: $ID"
[[ "$LINK" == "$DROP_BASE/open/$ID" ]] || fail "link is not <base>/open/<id>: $LINK"
pass "a drop returns a 32-hex capability and a link built from the configured base"

# --- the relay is holding ciphertext, not the message ------------------------
CODE=$(curl -s -o "$T/drop.bin" -w '%{http_code}' "http://127.0.0.1:$PORT/drop/$ID")
[[ "$CODE" == "200" ]] || fail "relay fetch of the drop returned $CODE"
[[ "$(head -c 4 "$T/drop.bin")" == "HSHD" ]] || fail "relay blob is not a hush-drop container"
pass "the relay stored a hush-drop container"

if command -v strings >/dev/null 2>&1; then
  strings "$T/drop.bin" | grep -q "$SECRET" && fail "the body is in cleartext in the container"
  strings "$T/drop.bin" | grep -q "gmail.com" && fail "the recipient is in cleartext in the container"
  strings "$T/relay.db" | grep -q "$SECRET" && fail "the body is in cleartext on the relay disk"
  strings "$T/relay.db" | grep -q "revenue" && fail "the attachment is in cleartext on the relay disk"
  pass "neither the container nor the relay disk holds the body, recipient, or attachment"
fi

# A wrong capability is a 404, not somebody else's message.
CODE=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/drop/$(printf 'f%.0s' $(seq 1 32))")
[[ "$CODE" == "404" ]] || fail "an unknown capability returned $CODE, want 404"
pass "an unknown capability is a 404"

# --- the sender keeps the record ---------------------------------------------
api GET /api/mail >/dev/null
[[ "$RESP" == *"\"delivery_id\":\"$ID\""* ]] || fail "the drop is not on the sender's mail rail: $RESP"
[[ "$RESP" == *"\"to\":\"bob@gmail.com\""* ]] || fail "the drop did not normalize the address: $RESP"
[[ "$RESP" == *'"state":"sent"'* ]] || fail "a drop must be recorded as sent, never queued: $RESP"
pass "the drop is recorded on the sender's mail rail, addressed and sent"

# --- the notification, if one went out, must not carry the PIN ----------------
if [[ -d "$MAILER_DIR" ]]; then
  NOTE=$(find "$MAILER_DIR" -type f | head -n 1)
  [[ -n "$NOTE" ]] || fail "the file mailer was configured but wrote nothing"
  grep -q "$LINK" "$NOTE" || fail "the notification does not contain the link: $(cat "$NOTE")"
  grep -qi "PIN" "$NOTE" || fail "the notification does not tell the recipient a PIN is needed"
  grep -q "correct horse" "$NOTE" && fail "the notification leaked the PIN"
  pass "the notification carries the link and says a PIN is needed, without the PIN"
else
  fail "the file mailer wrote no notification directory"
fi

echo "drop smoke ok"

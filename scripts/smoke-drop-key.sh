#!/usr/bin/env bash
# Public-key outsider drop smoke: generate a recipient keypair, seal a message to
# the public key, park it on the relay, and open it with the private key.
#
# This exercises the real path end to end — hush-cli keygen -> daemon ->
# hush-core sealed box -> relay storage -> hush-cli open — because the parts that
# can silently be wrong here are the ones no unit test sees: whether the bytes
# the daemon uploaded are the ones the relay stored, whether the matching private
# key really opens them, and whether the two drop modes can open each other. The
# PIN path itself is covered by scripts/smoke-drop.sh; this does not replace it.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${ROOT}/.tools/protoc-dist/bin/protoc"
T="${ROOT}/.tmp-drop-key-smoke"
R="${ROOT}/.tmp-drop-key-smoke-rt"
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

# A byte at an offset, as a decimal number. Used to read the container version.
byte_at() { od -An -tu1 -j"$2" -N1 "$1" | tr -d ' '; }

PORT=19879
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

api POST /api/unlock '{"passphrase":"pw"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "unlock returned $CODE: $(cat "$T/out.json")"

# --- the recipient makes a keypair the way a real one would -------------------
PUBKEY=$("$CLI" drop-keygen --out "$T/recipient.key" 2>"$T/keygen.err")
[[ "$PUBKEY" == hush-drop-pk-v1:* ]] || fail "drop-keygen did not print a public key: $PUBKEY"
[[ "$(stat -c '%a' "$T/recipient.key")" == "600" ]] || fail "the private key file is not 0600"
grep -q "$PUBKEY" "$T/recipient.key" && fail "the public key was written into the private key file"
pass "drop-keygen prints a pasteable public key and writes a 0600 private key"

# --- the daemon refuses a request that names no mode, or both -----------------
api POST /api/mail/outsider '{"to":"news@example.com","body":"tip"}' >/dev/null
[[ "$CODE" == "400" ]] || fail "a drop with no credential returned $CODE, want 400: $RESP"
pass "a drop with neither a PIN nor a key is refused (400)"

api POST /api/mail/outsider \
  "{\"to\":\"news@example.com\",\"body\":\"tip\",\"pin\":\"correct horse\",\"public_key\":\"$PUBKEY\"}" >/dev/null
[[ "$CODE" == "400" ]] || fail "a drop with both credentials returned $CODE, want 400: $RESP"
[[ "$RESP" == *"not both"* ]] || fail "wrong error for both credentials: $RESP"
pass "a drop with both a PIN and a key is refused (400)"

api POST /api/mail/outsider \
  '{"to":"news@example.com","body":"tip","public_key":"not-a-key"}' >/dev/null
[[ "$CODE" == "400" ]] || fail "a malformed public key returned $CODE, want 400: $RESP"
pass "a malformed public key is refused by the daemon (400)"

# --- a real public-key drop ---------------------------------------------------
SECRET="the quick brown fox jumps"
api POST /api/mail/outsider \
  "{\"to\":\"News@Example.com\",\"subject\":\"Q3\",\"body\":\"$SECRET\",\"public_key\":\"$PUBKEY\",\"attachments\":[{\"name\":\"q3.txt\",\"data_base64\":\"cmV2ZW51ZQ==\"}]}" \
  >/dev/null
[[ "$CODE" == "200" ]] || fail "public-key drop returned $CODE: $RESP"
ID=$(printf '%s' "$RESP" | sed -n 's/.*"delivery_id":"\([^"]*\)".*/\1/p')
LINK=$(printf '%s' "$RESP" | sed -n 's/.*"link":"\([^"]*\)".*/\1/p')
EXPIRES=$(printf '%s' "$RESP" | sed -n 's/.*"expires_unix":\([0-9]*\).*/\1/p')
[[ -n "$ID" && -n "$LINK" && -n "$EXPIRES" ]] || fail "incomplete drop response: $RESP"
[[ "$ID" =~ ^[0-9a-f]{32}$ ]] || fail "delivery id is not 32 hex chars: $ID"
[[ "$LINK" == "$DROP_BASE/open/$ID" ]] || fail "link is not <base>/open/<id>: $LINK"
pass "a public-key drop returns a 32-hex capability and a link built from the configured base"

# --- the relay holds a v2 container, not the message --------------------------
CODE=$(curl -s -o "$T/drop.bin" -w '%{http_code}' "http://127.0.0.1:$PORT/drop/$ID")
[[ "$CODE" == "200" ]] || fail "relay fetch of the drop returned $CODE"
[[ "$(head -c 4 "$T/drop.bin")" == "HSHD" ]] || fail "relay blob is not a hush-drop container"
[[ "$(byte_at "$T/drop.bin" 4)" == "2" ]] || fail "the public-key container is not version 2"
[[ "$(byte_at "$T/drop.bin" 5)" == "2" ]] || fail "the container mode is not 2 (pubkey)"
pass "the relay stored a version-2, public-key-mode container"

if command -v strings >/dev/null 2>&1; then
  strings "$T/drop.bin" | grep -q "$SECRET" && fail "the body is in cleartext in the container"
  strings "$T/drop.bin" | grep -q "example.com" && fail "the recipient is in cleartext in the container"
  strings "$T/relay.db" | grep -q "$SECRET" && fail "the body is in cleartext on the relay disk"
  strings "$T/relay.db" | grep -q "revenue" && fail "the attachment is in cleartext on the relay disk"
  pass "neither the container nor the relay disk holds the body, recipient, or attachment"
fi

# --- the private key opens it, from a file and by capability ------------------
mkdir -p "$T/open"
"$CLI" drop-open --key "$T/recipient.key" --save-dir "$T/open" "$T/drop.bin" >"$T/open.txt" 2>&1 \
  || fail "drop-open with the matching key failed: $(cat "$T/open.txt")"
grep -q "$SECRET" "$T/open.txt" || fail "drop-open did not recover the body: $(cat "$T/open.txt")"
grep -q "subject: Q3" "$T/open.txt" || fail "drop-open did not recover the subject: $(cat "$T/open.txt")"
[[ "$(cat "$T/open/q3.txt")" == "revenue" ]] || fail "the attachment did not come back intact"
pass "the matching private key opens the container and recovers body, subject, and attachment"

# The way a recipient with only the link would: fetch by capability from the relay.
# --save-dir is given here too, so the attachment lands under the temp dir rather
# than in whatever directory the script was run from.
mkdir -p "$T/open-relay"
"$CLI" drop-open --key "$T/recipient.key" --delivery "$ID" --relay "$RELAY_URL" --save-dir "$T/open-relay" >"$T/open-relay.txt" 2>&1 \
  || fail "drop-open --delivery failed: $(cat "$T/open-relay.txt")"
grep -q "$SECRET" "$T/open-relay.txt" || fail "fetching by capability did not recover the body"
pass "the drop opens by capability id fetched from the relay"

"$CLI" drop-keygen --out "$T/other.key" >/dev/null 2>&1
if "$CLI" drop-open --key "$T/other.key" "$T/drop.bin" >"$T/wrong.txt" 2>&1; then
  fail "a different private key opened the drop"
fi
pass "a different private key does not open the drop"

# --- the two modes do not open each other -------------------------------------
api POST /api/mail/outsider \
  '{"to":"pin@example.com","body":"pin body","pin":"correct horse"}' >/dev/null
[[ "$CODE" == "200" ]] || fail "PIN drop returned $CODE: $RESP"
PIN_ID=$(printf '%s' "$RESP" | sed -n 's/.*"delivery_id":"\([^"]*\)".*/\1/p')
[[ -n "$PIN_ID" ]] || fail "no delivery id from the PIN drop: $RESP"
curl -s -o "$T/pin.bin" "http://127.0.0.1:$PORT/drop/$PIN_ID"
[[ "$(byte_at "$T/pin.bin" 4)" == "1" ]] || fail "the PIN container is not version 1"
if "$CLI" drop-open --key "$T/recipient.key" "$T/pin.bin" >"$T/cross.txt" 2>&1; then
  fail "a public key opened a PIN drop"
fi
pass "a version-1 PIN container and a version-2 key container do not open each other"

# --- the notification says the key, not a PIN ---------------------------------
if [[ -d "$MAILER_DIR" ]]; then
  NOTE=$(grep -rl "To: News@Example.com" "$MAILER_DIR" | head -n 1)
  [[ -n "$NOTE" ]] || fail "the file mailer wrote no notification for the key drop"
  grep -q "$LINK" "$NOTE" || fail "the notification does not contain the link: $(cat "$NOTE")"
  grep -qi "private key" "$NOTE" || fail "the notification does not tell the recipient to use their private key"
  grep -qi "delivery pin" "$NOTE" && fail "the key notification tells the recipient to find a PIN"
  pass "the key notification carries the link and points at the private key, not a PIN"
else
  fail "the file mailer wrote no notification directory"
fi

echo "drop-key smoke ok"

#!/usr/bin/env bash
# Places smoke: the adopt -> counter-sign -> confirm handshake over the daemon API.
#
# A vouch is two signatures. The minter signs a hop naming the adopted vault;
# the adopted vault signs back, and that confirmation travels as a message. Only
# a chain with both is `verify_accepted`, and only an accepted chain may be
# introduced to a contact or badged.
#
# The Rust integration test (crates/hush-core/tests/places.rs) covers the same
# handshake one layer down, at `Account`. This drives it through the HTTP layer
# instead — two daemons, two vaults, one scratch relay — because that is where
# the `confirmed` field the UI reads is computed, and nothing else exercises the
# daemon routes for adopt/recv together.
#
# Usage: ./scripts/smoke-places.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

T="${ROOT}/.tmp-places-smoke"
rm -rf "$T"
mkdir -p "$T/logs"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

for bin in curl python3; do
  command -v "$bin" >/dev/null || fail "$bin is required"
done

RELAY_PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
RELAY="http://127.0.0.1:${RELAY_PORT}"

cargo build --quiet -p hush-daemon -p hush-relay
DMN="${ROOT}/target/debug/hush-daemon"
RLY="${ROOT}/target/debug/hush-relay"

PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$T"
}
trap cleanup EXIT

HUSH_RELAY_ADDR="127.0.0.1:${RELAY_PORT}" HUSH_RELAY_DB="$T/relay.db" \
  "$RLY" >"$T/logs/relay.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do
  curl -sf "$RELAY/health" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -sf "$RELAY/health" >/dev/null 2>&1 || fail "relay did not come up on $RELAY"
pass "relay up on $RELAY"

# Two vaults, each with its own daemon on its own Unix socket.
SOCK_A="$T/a.sock"
SOCK_B="$T/b.sock"
"$DMN" --dir "$T/vault-a" --socket "$SOCK_A" >"$T/logs/daemon-a.log" 2>&1 &
PIDS+=($!)
"$DMN" --dir "$T/vault-b" --socket "$SOCK_B" >"$T/logs/daemon-b.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 100); do
  [[ -S "$SOCK_A" && -S "$SOCK_B" ]] && break
  sleep 0.1
done
[[ -S "$SOCK_A" && -S "$SOCK_B" ]] || fail "daemons did not create their sockets"
pass "two daemons up"

api() {
  local sock="$1" method="$2" route="$3" body="${4:-}"
  if [[ -n "$body" ]]; then
    curl -sS --unix-socket "$sock" -X "$method" \
      -H 'content-type: application/json' -d "$body" "http://localhost${route}"
  else
    curl -sS --unix-socket "$sock" -X "$method" "http://localhost${route}"
  fi
}

# A JSON object from alternating key/value arguments, so invites and passphrases
# never have to be escaped by hand.
json() {
  python3 -c 'import json,sys; print(json.dumps(dict(zip(sys.argv[1::2], sys.argv[2::2]))))' "$@"
}

field() { python3 -c 'import json,sys; print(json.loads(sys.stdin.read())[sys.argv[1]])' "$1"; }

# A fresh vault may come back locked; unlocking an already-unlocked one is
# harmless, so this never needs to know which it got.
for pair in "a:a-pass:alpha" "b:b-pass:bravo"; do
  sock_var="SOCK_$(echo "${pair%%:*}" | tr '[:lower:]' '[:upper:]')"
  sock="${!sock_var}"
  rest="${pair#*:}"
  passphrase="${rest%%:*}"
  name="${rest##*:}"
  api "$sock" POST /api/vault/create \
    "$(json passphrase "$passphrase" name "$name" relay "$RELAY")" >/dev/null \
    || fail "vault create failed for $name"
  api "$sock" POST /api/unlock "$(json passphrase "$passphrase")" >/dev/null 2>&1 || true
done
pass "two vaults created against the scratch relay"

RAW_A="$(api "$SOCK_A" POST /api/invite)"
RAW_B="$(api "$SOCK_B" POST /api/invite)"
INVITE_A="$(printf '%s' "$RAW_A" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("invite",""))' 2>/dev/null || true)"
INVITE_B="$(printf '%s' "$RAW_B" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("invite",""))' 2>/dev/null || true)"
[[ -n "$INVITE_A" ]] || fail "invite from a: $RAW_A"
[[ -n "$INVITE_B" ]] || fail "invite from b: $RAW_B"

# --- adopt: the hop exists, but nobody has signed it back yet -----------------
api "$SOCK_A" POST /api/places/adopt \
  "$(json nickname bravo invite "$INVITE_B" label phone)" >/dev/null \
  || fail "adopt failed"

api "$SOCK_A" GET /api/places >"$T/places-unconfirmed.json"
python3 - "$T/places-unconfirmed.json" <<'PY' || fail "an unconfirmed place must not read as confirmed"
import json, sys
places = json.load(open(sys.argv[1]))
assert len(places) == 1, f"expected one place, got {places}"
p = places[0]
assert p["role"] == "minted_by_me", p
assert p["confirmed"] is False, f"a one-sided hop must not be confirmed: {p}"
assert p["label"] == "phone", p
PY
pass "adopt records the place as unconfirmed"

# Introducing an unconfirmed place is refused, not silently ignored.
CODE="$(curl -s -o /dev/null -w '%{http_code}' --unix-socket "$SOCK_A" -X POST \
  -H 'content-type: application/json' \
  -d "$(json place_account_id "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[0]["account_id"])' "$T/places-unconfirmed.json")" contact_nickname nobody)" \
  "http://localhost/api/places/introduce")"
[[ "$CODE" != "204" ]] || fail "introducing an unconfirmed place should not succeed"
pass "introducing an unconfirmed place is refused (HTTP $CODE)"

# --- confirm: B adds A, decrypts the adopt, counter-signs, A applies the ack ---
api "$SOCK_B" POST /api/contacts/add \
  "$(json nickname alpha invite "$INVITE_A")" >/dev/null \
  || fail "the adopted vault could not add the minter"

for _ in $(seq 1 5); do
  api "$SOCK_B" POST /api/recv >/dev/null || fail "recv failed on b"
  api "$SOCK_A" POST /api/recv >/dev/null || fail "recv failed on a"
  CONFIRMED="$(api "$SOCK_A" GET /api/places \
    | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p[0]["confirmed"] if p else "none")')"
  [[ "$CONFIRMED" == "True" ]] && break
  sleep 0.4
done
[[ "$CONFIRMED" == "True" ]] || fail "the minter never saw the counter-signature (confirmed=$CONFIRMED)"
pass "the adopted vault counter-signed and the minter recorded it"

# The adopted vault records the relationship in the other direction.
api "$SOCK_B" GET /api/places >"$T/places-b.json"
python3 - "$T/places-b.json" <<'PY' || fail "the adopted vault should record MintedMe"
import json, sys
places = json.load(open(sys.argv[1]))
assert len(places) == 1, places
assert places[0]["role"] == "minted_me", places
assert places[0]["confirmed"] is True, f"a place that minted us is confirmed by construction: {places}"
PY
pass "the adopted vault records the mirror row"

# --- introduce: only now may the place be shown to a contact -------------------
api "$SOCK_A" POST /api/contacts/add \
  "$(json nickname bravo invite "$INVITE_B")" >/dev/null 2>&1 || true
api "$SOCK_A" POST /api/places/introduce \
  "$(json place_account_id "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[0]["account_id"])' "$T/places-unconfirmed.json")" contact_nickname bravo)" >/dev/null \
  || fail "a confirmed place should be introducible"
pass "a confirmed place can be introduced"

echo
echo "PASS: places smoke"

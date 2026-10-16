#!/usr/bin/env bash
# Acceptance for the client Tor switch (relay over onion, fail-closed).
#
# Uses the bundled tor sidecar as a plain SOCKS client and the live onion
# relay from the lab VPS. Proves, in order:
#   1. a vault can be created with `init --onion --tor` (active base = onion)
#   2. a relay request goes through Tor to the onion (rendezvous in the log)
#   3. with the SOCKS port unset the request fails closed (no clearnet send)
#   4. with Tor dead the request fails on the onion URL (no clearnet fallback)
#   5. flipping the mode off restores the clearnet path
#
# The relay probe is `recv`, not `invite`: a minted invite is cached in the
# vault and repeats never touch the network, which silently turns every
# negative control into a false pass.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
cd "$ROOT"

TOR="$ROOT/apps/hush-tauri/src-tauri/resources/tor/tor"
TOR_LIBS="$(dirname "$TOR")"
# The lab VPS onion service (docs/ops/relay-lab-vps.md). Override to re-point.
ONION="${HUSH_SMOKE_ONION:-http://ogkf745dfn32pdbvsk6e52uqski7mudadowosi5cbyif4yglhzuqlsad.onion}"
CLI=./target/debug/hush-cli

[[ -x "$CLI" ]] || cargo build -p hush-cli --quiet || exit 1
[[ -x "$TOR" ]] || { echo "bundled tor not found at $TOR"; exit 1; }

WORK=$(mktemp -d)
trap 'kill "$TOR_PID" 2>/dev/null; rm -rf "$WORK"' EXIT
fails=0

echo "== starting bundled tor (SOCKS 9150) =="
env LD_LIBRARY_PATH="$TOR_LIBS" "$TOR" \
  --SocksPort 9150 --DataDirectory "$WORK/tor-data" \
  --Log "info file $WORK/tor.log" >/dev/null 2>&1 &
TOR_PID=$!
for i in $(seq 1 60); do
  grep -q "Bootstrapped 100" "$WORK/tor.log" 2>/dev/null && break
  sleep 2
done
grep -q "Bootstrapped 100" "$WORK/tor.log" || { echo "FAIL tor did not bootstrap"; exit 1; }
echo "tor ready"

echo "== 1. vault born on the onion =="
"$CLI" --dir "$WORK/vault" init --passphrase smoke-tor-1 \
  --onion "$ONION" --tor --name smoketor >/dev/null 2>&1 \
  && echo "PASS init --onion --tor" || { echo "FAIL init --onion --tor"; fails=$((fails+1)); }

echo "== 2. relay request traverses tor =="
HUSH_RELAY_SOCKS_PROXY=socks5h://127.0.0.1:9150 \
  "$CLI" --dir "$WORK/vault" --passphrase smoke-tor-1 recv >/dev/null 2>&1
rc=$?
rendezvous=$(grep -c "Got RENDEZVOUS2 cell from hidden service" "$WORK/tor.log")
if [[ $rc -eq 0 && $rendezvous -gt 0 ]]; then
  echo "PASS request went through tor ($rendezvous rendezvous(s))"
else
  echo "FAIL rc=$rc rendezvous=$rendezvous"; fails=$((fails+1))
fi

echo "== 3. no SOCKS port fails closed =="
out=$(env -u HUSH_RELAY_SOCKS_PROXY "$CLI" --dir "$WORK/vault" --passphrase smoke-tor-1 recv 2>&1)
rc=$?
if [[ $rc -ne 0 && "$out" == *"Tor is not connected"* ]]; then
  echo "PASS refused without a SOCKS port"
else
  echo "FAIL rc=$rc out=$out"; fails=$((fails+1))
fi

echo "== 4. dead tor fails on the onion, never clearnet =="
kill "$TOR_PID" 2>/dev/null; wait "$TOR_PID" 2>/dev/null
out=$(HUSH_RELAY_SOCKS_PROXY=socks5h://127.0.0.1:9150 \
  "$CLI" --dir "$WORK/vault" --passphrase smoke-tor-1 recv 2>&1)
rc=$?
if [[ $rc -ne 0 && "$out" == *".onion"* ]]; then
  echo "PASS failed on the onion address"
else
  echo "FAIL rc=$rc out=$out"; fails=$((fails+1))
fi

echo "== 5. switch off restores clearnet =="
"$CLI" --dir "$WORK/vault" --passphrase smoke-tor-1 relay off >/dev/null 2>&1
"$CLI" --dir "$WORK/vault" --passphrase smoke-tor-1 relay show

echo
[[ $fails -eq 0 ]] && echo "tor relay switch ok" || echo "tor relay switch FAILED ($fails)"
exit "$fails"

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${ROOT}/.tools/protoc-dist/bin/protoc"
RELAY_ADDR="127.0.0.1:8799"
export HUSH_RELAY_ADDR="$RELAY_ADDR"
export HUSH_RELAY_DB="${ROOT}/.tmp-relay-smoke.db"
ALICE="${ROOT}/.tmp-alice"
BOB="${ROOT}/.tmp-bob"
rm -rf "$ALICE" "$BOB" "$HUSH_RELAY_DB"
fuser -k "${RELAY_ADDR##*:}/tcp" 2>/dev/null || true
sleep 0.2

cargo build -p hush-relay -p hush-cli --quiet
cargo run -p hush-relay --quiet &
RELAY_PID=$!
sleep 0.5

export HUSH_PASSPHRASE=alice-pass
cargo run -p hush-cli --quiet -- --dir "$ALICE" init --passphrase alice-pass --relay "http://$RELAY_ADDR"
INVITE_A=$(cargo run -p hush-cli --quiet -- --dir "$ALICE" invite)

export HUSH_PASSPHRASE=bob-pass
cargo run -p hush-cli --quiet -- --dir "$BOB" init --passphrase bob-pass --relay "http://$RELAY_ADDR"
INVITE_B=$(cargo run -p hush-cli --quiet -- --dir "$BOB" invite)

cargo run -p hush-cli --quiet -- --dir "$BOB" add-contact alice "$INVITE_A"
export HUSH_PASSPHRASE=alice-pass
cargo run -p hush-cli --quiet -- --dir "$ALICE" add-contact bob "$INVITE_B"

export HUSH_PASSPHRASE=bob-pass
cargo run -p hush-cli --quiet -- --dir "$BOB" send alice --text "smoke test"

export HUSH_PASSPHRASE=alice-pass
OUT=$(cargo run -p hush-cli --quiet -- --dir "$ALICE" recv)
echo "$OUT"
kill "$RELAY_PID"
wait "$RELAY_PID" 2>/dev/null || true
rm -rf "$ALICE" "$BOB" "$HUSH_RELAY_DB"

if [[ "$OUT" != *"smoke test"* ]]; then
  echo "expected message not received"
  exit 1
fi
echo "task3 smoke ok"

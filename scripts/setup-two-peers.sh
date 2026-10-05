#!/usr/bin/env bash
# Create two local vaults, exchange invites, smoke-test send/recv.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${PROTOC:-${ROOT}/.tools/protoc-dist/bin/protoc}"
RELAY_URL="${HUSH_RELAY_URL:-http://127.0.0.1:8787}"
PEER_A="${ROOT}/instances/peer-a"
PEER_B="${ROOT}/instances/peer-b"
PASS_A="${HUSH_PEER_A_PASS:-peer-a-pass}"
PASS_B="${HUSH_PEER_B_PASS:-peer-b-pass}"

mkdir -p "${ROOT}/instances"
rm -rf "$PEER_A" "$PEER_B"

cargo build -p hush-cli --quiet

export HUSH_PASSPHRASE="$PASS_A"
cargo run -p hush-cli --quiet -- --dir "$PEER_A" init --passphrase "$PASS_A" --relay "$RELAY_URL"
INVITE_A=$(cargo run -p hush-cli --quiet -- --dir "$PEER_A" invite)

export HUSH_PASSPHRASE="$PASS_B"
cargo run -p hush-cli --quiet -- --dir "$PEER_B" init --passphrase "$PASS_B" --relay "$RELAY_URL"
INVITE_B=$(cargo run -p hush-cli --quiet -- --dir "$PEER_B" invite)

cargo run -p hush-cli --quiet -- --dir "$PEER_B" add-contact alice "$INVITE_A"
export HUSH_PASSPHRASE="$PASS_A"
cargo run -p hush-cli --quiet -- --dir "$PEER_A" add-contact bob "$INVITE_B"

export HUSH_PASSPHRASE="$PASS_B"
cargo run -p hush-cli --quiet -- --dir "$PEER_B" send alice --text "hello from bob"

export HUSH_PASSPHRASE="$PASS_A"
OUT=$(cargo run -p hush-cli --quiet -- --dir "$PEER_A" recv)
echo "$OUT"

if [[ "$OUT" != *"hello from bob"* ]]; then
  echo "setup failed: alice did not receive bob's message"
  exit 1
fi

cat <<EOF

Two peers ready (relay: $RELAY_URL)

  Peer A (alice)  $PEER_A
    passphrase:   $PASS_A
    contact nick: bob

  Peer B (bob)    $PEER_B
    passphrase:   $PASS_B
    contact nick: alice

Send from A → B:
  export HUSH_PASSPHRASE='$PASS_A'
  cargo run -p hush-cli -- --dir '$PEER_A' send bob --text 'hi bob'
  export HUSH_PASSPHRASE='$PASS_B'
  cargo run -p hush-cli -- --dir '$PEER_B' recv

Send from B → A:
  export HUSH_PASSPHRASE='$PASS_B'
  cargo run -p hush-cli -- --dir '$PEER_B' send alice --text 'hi alice'
  export HUSH_PASSPHRASE='$PASS_A'
  cargo run -p hush-cli -- --dir '$PEER_A' recv

Daemon + Tauri (one vault at a time):
  cargo run -p hush-daemon -- --dir '$PEER_A'
  cargo run -p hush-tauri

EOF

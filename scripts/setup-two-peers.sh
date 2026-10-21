#!/usr/bin/env bash
# Create two local vaults, exchange invites, smoke-test send/recv.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
RELAY_URL="${HUSH_RELAY_URL:-https://hushrelay.space}"
PEER_A="${ROOT}/instances/peer-a"
PEER_B="${ROOT}/instances/peer-b"
PASS_A="${HUSH_PEER_A_PASS:-peer-a-pass}"
PASS_B="${HUSH_PEER_B_PASS:-peer-b-pass}"

mkdir -p "${ROOT}/instances"
rm -rf "$PEER_A" "$PEER_B"

cargo build -p hush-cli --quiet

export HUSH_PASSPHRASE="$PASS_A"
cargo run -p hush-cli --quiet -- --dir "$PEER_A" init --passphrase "$PASS_A" --relay "$RELAY_URL"
# Record the relay in client-config.json as well. The vault keeps its own copy, but
# that one is encrypted and cannot be read without the passphrase, so this plaintext
# copy is what a later run reads to see which relay the vault is on — and therefore
# which daemon profile that relay needs. See DEEPSEEKPROBLEMS.MD.
cargo run -p hush-cli --quiet -- --dir "$PEER_A" config set --relay "$RELAY_URL"
INVITE_A=$(cargo run -p hush-cli --quiet -- --dir "$PEER_A" invite)

export HUSH_PASSPHRASE="$PASS_B"
cargo run -p hush-cli --quiet -- --dir "$PEER_B" init --passphrase "$PASS_B" --relay "$RELAY_URL"
cargo run -p hush-cli --quiet -- --dir "$PEER_B" config set --relay "$RELAY_URL"
INVITE_B=$(cargo run -p hush-cli --quiet -- --dir "$PEER_B" invite)

cargo run -p hush-cli --quiet -- --dir "$PEER_B" add-contact alice "$INVITE_A"
export HUSH_PASSPHRASE="$PASS_A"
cargo run -p hush-cli --quiet -- --dir "$PEER_A" add-contact bob "$INVITE_B"

# A live chat send is presence-gated (Account::send_text -> require_online) and
# presence is heartbeated by a running daemon. This script starts no daemons, so
# a send here can only ever fail with ContactOffline; the old send/recv smoke was
# removed rather than faked. What is checkable without daemons is that the relay
# both vaults were created against is actually up.
if ! curl -sf -o /dev/null --connect-timeout 5 "${RELAY_URL%/}/health"; then
  echo "setup failed: relay at $RELAY_URL did not answer /health" >&2
  exit 1
fi
echo "relay reachable: ${RELAY_URL%/}/health"

cat <<EOF

Two peers ready (relay: $RELAY_URL)

  Peer A (alice)  $PEER_A
    passphrase:   $PASS_A
    contact nick: bob

  Peer B (bob)    $PEER_B
    passphrase:   $PASS_B
    contact nick: alice

Chat sends below need a daemon running on both sides: chat is presence-gated,
and presence is heartbeated by the daemon. Start the dev stack first:
  ./scripts/run-two-ui.sh --dev --relay '$RELAY_URL'

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

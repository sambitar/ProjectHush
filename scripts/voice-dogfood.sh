#!/usr/bin/env bash
# Voice dogfood: two daemons carry a live voice room over a real relay, and
# both ends agree on the room's state at every step without any on-disk trace.
#
# This is the Phase 1+2 acceptance check from docs/tasks/conclave-voice.md,
# scripted so it can be re-run after any change to the voice path:
#
#   1. Alice opens the room; the open fans out to Bob over the relay and
#      rings him (ringing_until_ms is set until he answers)
#   2. Bob joins; the offer is the join announcement and the answer closes it
#   3. Both ends agree on the roster: two live speakers. The mute flag is
#      advisory and only ever moves by an explicit VoiceMute
#   3b. Real Opus audio crosses both ways: each daemon's stats line shows
#       decoded frames from the peer (HUSH_VOICE_TEST_TONE=1 supplies the
#       capture source; no microphone is needed headless)
#   4. Bob mutes; Alice sees the mute
#   5. Bob leaves; the room auto-ends for Alice (below two speakers)
#   6. A re-open survives, and the opener's own leave ends the room for Bob
#
# The relay sees Signal ciphertext only: the SDP inside the payloads must
# never appear in relay.db in plaintext.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
T="${ROOT}/.tmp-voice-dogfood"
R="${ROOT}/.tmp-voice-dogfood-rt"
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

jget() { # jget <file> <dot.path> -> the value, or empty when absent
  node -e '
    const fs = require("fs");
    let v = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    for (const k of process.argv[2].split(".")) { v = v?.[k]; }
    process.stdout.write(v == null ? "" : String(v));
  ' "$1" "$2"
}

roster() { # roster <file> -> "account:muted" per member, joined with spaces
  node -e '
    const fs = require("fs");
    const v = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write((v.joined ?? []).map((m) => `${m.account}:${m.muted}`).join(" "));
  ' "$1"
}

n_joined() { # n_joined <file> -> the member count
  node -e '
    const fs = require("fs");
    const v = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(String((v.joined ?? []).length));
  ' "$1"
}

drain() { # drain <socket> -> the /api/recv body (drains the relay queue)
  api_at "$1" POST /api/recv >/dev/null
  printf '%s' "$RESP"
}

PIDS=()
CLEAN=0
cleanup() {
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [[ "$CLEAN" == 1 ]]; then
    rm -rf "$T" "$R"
  else
    echo "logs and vaults kept in $T" >&2
  fi
}
trap cleanup EXIT

PORT=19879
RELAY_URL="http://127.0.0.1:$PORT"
VA="$T/a"; VB="$T/b"
RA="$R/a"; RB="$R/b"
SOCK_A="$RA/hush-daemon.sock"; SOCK_B="$RB/hush-daemon.sock"
mkdir -p "$VA" "$VB" "$RA" "$RB"

HUSH_RELAY_DB="$T/relay.db" HUSH_RELAY_ADDR="127.0.0.1:$PORT" "$RELAY" >"$T/relay.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done

for pair in "a:$VA:pa" "b:$VB:pb"; do
  dir="${pair#*:}"; dir="${dir%%:*}"
  pass_="${pair##*:}"
  HUSH_PASSPHRASE="$pass_" "$CLI" --dir "$dir" init --passphrase "$pass_" --relay "$RELAY_URL" >/dev/null
done

for pair in "$VA:$RA:pa" "$VB:$RB:pb"; do
  dir="${pair%%:*}"; rest="${pair#*:}"; rt="${rest%%:*}"
  # Tone mode: the voice plane's capture source is a synthetic 440 Hz tone
  # and playout is counted, so the run needs no microphone and no speaker.
  HUSH_RUNTIME_DIR="$rt" HUSH_VOICE_TEST_TONE=1 "$DMN" --dir "$dir" \
    >"$T/daemon-$(basename "$dir").log" 2>&1 &
  PIDS+=($!)
done
for _ in $(seq 1 60); do
  [ -S "$SOCK_A" ] && [ -S "$SOCK_B" ] && break
  sleep 0.1
done
[[ -S "$SOCK_A" && -S "$SOCK_B" ]] || fail "daemons did not come up"

for pair in "$SOCK_A:pa" "$SOCK_B:pb"; do
  api_at "${pair%%:*}" POST /api/unlock "{\"passphrase\":\"${pair##*:}\"}" >/dev/null
  [[ "$CODE" == "204" ]] || fail "unlock on ${pair%%:*} returned $CODE"
done

api_at "$SOCK_A" POST /api/invite >/dev/null; INV_A=$(jget "$T/out.json" invite)
api_at "$SOCK_B" POST /api/invite >/dev/null; INV_B=$(jget "$T/out.json" invite)
[[ -n "$INV_A" && -n "$INV_B" ]] || fail "could not read peer invites"

api_at "$SOCK_A" POST /api/contacts/add "{\"nickname\":\"bob\",\"invite\":\"$INV_B\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "A add-contact bob returned $CODE"
api_at "$SOCK_B" POST /api/contacts/add "{\"nickname\":\"alice\",\"invite\":\"$INV_A\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "B add-contact returned $CODE"
pass "two peers on one relay, contacts exchanged"

# --- The conclave -------------------------------------------------------------
api_at "$SOCK_A" POST /api/conclaves '{"name":"voice-dogfood","trustees":["bob"]}' >/dev/null
[[ "$CODE" == "200" ]] || fail "create conclave returned $CODE: $RESP"
CONCLAVE=$(jget "$T/out.json" id)
[[ -n "$CONCLAVE" ]] || fail "create conclave returned no id: $RESP"

ACCEPTED=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" POST /api/conclaves/trustees/accept "{\"conclave_id\":\"$CONCLAVE\"}" >/dev/null
  [[ "$CODE" == "200" ]] && { ACCEPTED=1; break; }
  sleep 0.2
done
[[ -n "$ACCEPTED" ]] || fail "bob never accepted the invite (last: $CODE $RESP)"
pass "bob received the invite over the relay and joined"

# Alice must see the join before she opens the room: voice payloads go to the
# joined set only, and an unjoined Bob would be silently skipped.
SEEN=""
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET /api/conclaves >/dev/null
  node -e '
    const fs = require("fs");
    const list = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const c = list.find((x) => x.id === process.argv[2]);
    const t = c?.trustees?.find((x) => x.nick === "bob");
    process.exit(t?.state === "joined" ? 0 : 1);
  ' "$T/out.json" "$CONCLAVE" && { SEEN=1; break; }
  sleep 0.2
done
[[ -n "$SEEN" ]] || fail "alice never observed bob joining (roster: $(cat "$T/out.json"))"
pass "alice sees bob in the joined set"

# --- Alice opens the room; the open fans out ---------------------------------
api_at "$SOCK_A" POST "/api/voice/$CONCLAVE/open" >/dev/null
[[ "$CODE" == "200" ]] || fail "voice open returned $CODE: $RESP"
[[ "$(jget "$T/out.json" i_am_in)" == "true" ]] || fail "the opener is not in her own room: $RESP"

ALONE_B=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" GET "/api/voice/$CONCLAVE" >/dev/null
  [[ "$CODE" == "200" ]] && { ALONE_B=1; break; }
  sleep 0.2
done
[[ -n "$ALONE_B" ]] || fail "bob never learned of the open room (last: $CODE $RESP)"
[[ "$(jget "$T/out.json" i_am_in)" == "false" ]] \
  || fail "bob should read as outside the room he has not joined: $RESP"
[[ "$(roster "$T/out.json")" == "$(jget "$T/out.json" opener):false" ]] \
  || fail "bob's copy of the room does not show alice alone and live: $RESP"
# The open rings the vault it reached: an unanswered knock must read as one,
# and answering (the join below) must end it.
[[ -n "$(jget "$T/out.json" ringing_until_ms)" ]] \
  || fail "an open bob did not make must ring him: $RESP"
pass "the open reached bob over the relay; it rings, and he reads the room as joinable"

# --- Bob joins; the offer is the announcement, the answer closes it ----------
api_at "$SOCK_B" POST "/api/voice/$CONCLAVE/join" >/dev/null
[[ "$CODE" == "200" ]] || fail "bob's join returned $CODE: $RESP"
[[ "$(jget "$T/out.json" i_am_in)" == "true" ]] || fail "bob is not in the room after joining: $RESP"
# A knock that was answered is no longer a knock: the answered vault's view
# must not ring its own room.
[[ -z "$(jget "$T/out.json" ringing_until_ms)" ]] \
  || fail "bob's own join must end the ring: $RESP"
pass "bob's join answered the ring; he is in the room"

BOTH=""
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET "/api/voice/$CONCLAVE" >/dev/null
  [[ "$(jget "$T/out.json" i_am_in)" == "true" && "$(n_joined "$T/out.json")" == "2" ]] && { BOTH=1; break; }
  sleep 0.2
done
[[ -n "$BOTH" ]] || fail "alice's room never showed two speakers (last: $(cat "$T/out.json"))"
BOB_MUTED_A=$(jget "$T/out.json" joined.1.muted)
[[ "$BOB_MUTED_A" == "false" ]] \
  || fail "an offer puts the joiner on the receiver's roster unmuted: $(roster "$T/out.json")"
pass "bob's offer landed at alice: two speakers on both sides"

# The answer is handshake traffic, not a mic label: the mute flag is advisory
# and only ever moves by an explicit VoiceMute. What the answer must produce is
# agreement: bob's copy of the roster holds the opener live and unmuted, with
# himself beside her. Rosters speak in account ids, so the opener is read from
# the view rather than guessed from a nickname.
BOB_OK=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" GET "/api/voice/$CONCLAVE" >/dev/null
  OPENER_ID=$(jget "$T/out.json" opener)
  [[ "$(roster "$T/out.json")" == "$OPENER_ID:false "*":false" ]] && { BOB_OK=1; break; }
  sleep 0.2
done
[[ -n "$BOB_OK" ]] \
  || fail "bob's copy of the roster never agreed with alice's (saw: $(cat "$T/out.json"))"
[[ "$(n_joined "$T/out.json")" == "2" ]] \
  || fail "bob's roster is not two speakers after the answer: $R_B"
pass "the answer closed the handshake: both ends agree on two live speakers"

# --- Real audio crosses both ways (Phase 2) -----------------------------------
# The media handshake rides the same payloads: Alice answered Bob's offer with
# real gathered SDP, so DTLS-SRTP connects and each side decodes the other's
# tone. The stats line prints every 2 s while a connection is live; both
# daemons must show decoded frames from the peer, played by the playout clock.
audio_crossed() { # audio_crossed <log> <label>
  for _ in $(seq 1 100); do
    if grep -q "voice stats sent=[1-9]" "$1" 2>/dev/null; then
      local line
      line=$(grep "voice stats" "$1" | tail -1)
      local decoded
      decoded=$(echo "$line" | sed -n 's/.*decoded=\([0-9]*\).*/\1/p')
      if [[ "${decoded:-0}" -ge 10 ]]; then
        pass "$2: audio crossed ($line)"
        return 0
      fi
    fi
    sleep 0.2
  done
  fail "$2: no decoded audio after 20 s (last stats: $(grep 'voice stats' "$1" | tail -1 || echo none))"
}
audio_crossed "$T/daemon-a.log" "alice decodes bob's tone"
audio_crossed "$T/daemon-b.log" "bob decodes alice's tone"

# --- Bob mutes; alice sees it ------------------------------------------------
api_at "$SOCK_B" POST "/api/voice/$CONCLAVE/mute" '{"muted":true}' >/dev/null
[[ "$CODE" == "200" ]] || fail "bob's mute returned $CODE: $RESP"

MUTED=""
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET "/api/voice/$CONCLAVE" >/dev/null
  OPENER_ID=$(jget "$T/out.json" opener)
  R_A=$(roster "$T/out.json")
  # Rosters speak in account ids, never nicknames: the opener first, the
  # joiner second, and the joiner's flag is the one that must have moved.
  [[ "$R_A" == "$OPENER_ID:false "*":true" ]] && { MUTED=1; break; }
  sleep 0.2
done
[[ -n "$MUTED" ]] || fail "alice never saw bob's mute (roster: $(cat "$T/out.json"))"
pass "bob's mute reached alice"

# --- Bob leaves; the room auto-ends for alice --------------------------------
api_at "$SOCK_B" POST "/api/voice/$CONCLAVE/leave" >/dev/null
[[ "$CODE" == "200" ]] || fail "bob's leave returned $CODE: $RESP"

ENDED_A=""
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET "/api/voice/$CONCLAVE" >/dev/null
  [[ "$CODE" == "404" ]] && { ENDED_A=1; break; }
  sleep 0.2
done
[[ -n "$ENDED_A" ]] || fail "alice's room did not end after bob left (last: $CODE $RESP)"
pass "bob's leave ended the room on alice's side: one speaker is below the floor"

# --- A re-open, ended by the opener's own leave -------------------------------
api_at "$SOCK_A" POST "/api/voice/$CONCLAVE/open" >/dev/null
[[ "$CODE" == "200" ]] || fail "re-opening the room returned $CODE: $RESP"

REJOINED=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" POST "/api/voice/$CONCLAVE/join" >/dev/null
  [[ "$CODE" == "200" ]] && { REJOINED=1; break; }
  sleep 0.2
done
[[ -n "$REJOINED" ]] || fail "bob could not join the re-opened room (last: $CODE $RESP)"
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET "/api/voice/$CONCLAVE" >/dev/null
  [[ "$(n_joined "$T/out.json")" == "2" ]] && break
  sleep 0.2
done

# The opener leaving ends the room for everyone; the code on bob's side is a
# settled ended state, not a refusal, so his GET must go to 404 once the close
# reaches him.
api_at "$SOCK_A" POST "/api/voice/$CONCLAVE/leave" >/dev/null
[[ "$CODE" == "200" ]] || fail "the opener's leave returned $CODE: $RESP"

ENDED_B=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" GET "/api/voice/$CONCLAVE" >/dev/null
  [[ "$CODE" == "404" ]] && { ENDED_B=1; break; }
  sleep 0.2
done
[[ -n "$ENDED_B" ]] || fail "the opener's leave did not end bob's room (last: $CODE $RESP)"
pass "the opener's own leave ended the room on bob's side"

# --- Nothing about the room on disk ------------------------------------------
# The voice plane is in-memory only: no vault file may exist for it.
[[ -s "$T/relay.db" ]] || fail "relay.db is missing, so the run did not use the relay"
VOICE_FILES=$(find "$VA" "$VB" -type f -name '*voice*' || true)
[[ -z "$VOICE_FILES" ]] || fail "voice files exist in a vault, but the live plane must be memory-only: $VOICE_FILES"
pass "no voice file on disk: the live plane stayed in memory"

# The relay must never have seen the room's innards: the SDP (its audio
# m-line, its ICE credentials) rode inside Signal ciphertext, so a plaintext
# strings scan of relay.db finds neither.
SDP_LEAK=$(strings "$T/relay.db" | grep -cE 'm=audio|ice-ufrag|a=fingerprint' || true)
[[ "$SDP_LEAK" == "0" ]] || fail "relay.db carries plaintext SDP: $SDP_LEAK matches"
pass "no SDP plaintext on the relay: the payloads were ciphertext end to end"

echo
echo "voice dogfood ok: two daemons agreed on a live room through open, join,"
echo "mute, and both leave paths over a real relay; Opus audio crossed both"
echo "ways on the media plane; and nothing about the room — not even the SDP —"
echo "was ever written down in plaintext"
CLEAN=1

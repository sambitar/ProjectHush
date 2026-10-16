#!/usr/bin/env bash
# Paper dogfood: three daemons converge one collaborative document over a real
# relay, a late joiner is caught up by the hub, and nothing on disk holds the
# text in the clear.
#
# This is the Phase 2 / Task 4 acceptance check from
# docs/plans/2026-10-12-conclave-paper.md, scripted so it can be re-run after
# any change to the paper path:
#
#   1. Alice creates the paper; the descriptor fans out to Bob
#   2. Alice edits; Bob applies the delta and reads the same text
#   3. Bob edits; Alice applies it; both documents are byte-identical
#   4. Carol joins late, after the edits, and the hub catches her up
#   5. relay.db and every vault hold no plaintext
#
# Edits come from scripts/paper-edit.mjs, which builds them with the UI's own
# `yjs` in the shape the editor sends (an XmlFragment named "prosemirror"), so
# this moves the bytes the webview would move rather than a stand-in.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT}/scripts/lib/hush.sh"
hush_require_protoc || exit 1
T="${ROOT}/.tmp-paper-dogfood"
R="${ROOT}/.tmp-paper-dogfood-rt"
rm -rf "$T" "$R"
mkdir -p "$T" "$R"
unset HUSH_RELAY_URL HUSH_DAEMON_ADDR HUSH_DAEMON_SOCKET HUSH_RUNTIME_DIR XDG_RUNTIME_DIR

cargo build -p hush-daemon -p hush-cli -p hush-relay --quiet
CLI="${ROOT}/target/debug/hush-cli"
DMN="${ROOT}/target/debug/hush-daemon"
RELAY="${ROOT}/target/debug/hush-relay"
EDIT="${ROOT}/scripts/paper-edit.mjs"

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

trustee_state() { # trustee_state <conclaves-json> <conclave-id> <nick> -> invited|joined|...
  node -e '
    const fs = require("fs");
    const list = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const conclave = list.find((c) => c.id === process.argv[2]);
    const trustee = conclave?.trustees?.find((t) => t.nick === process.argv[3]);
    process.stdout.write(trustee ? trustee.state : "");
  ' "$1" "$2" "$3"
}

presence_frame() { # presence_frame <presence-json> -> the first peer's frame
  node -e '
    const fs = require("fs");
    const j = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(j.peers?.[0]?.frame ?? "");
  ' "$1"
}

contact_id() { # contact_id <contacts-json> <nick> -> the account id
  node -e '
    const fs = require("fs");
    const list = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const c = list.find((x) => x.nickname === process.argv[2]);
    process.stdout.write(c ? c.protocol_name : "");
  ' "$1" "$2"
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

PORT=19878
RELAY_URL="http://127.0.0.1:$PORT"
VA="$T/a"; VB="$T/b"; VC="$T/c"
RA="$R/a"; RB="$R/b"; RC="$R/c"
SOCK_A="$RA/hush-daemon.sock"; SOCK_B="$RB/hush-daemon.sock"; SOCK_C="$RC/hush-daemon.sock"
mkdir -p "$VA" "$VB" "$VC" "$RA" "$RB" "$RC"

HUSH_RELAY_DB="$T/relay.db" HUSH_RELAY_ADDR="127.0.0.1:$PORT" "$RELAY" >"$T/relay.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break; sleep 0.1; done

for pair in "a:$VA:pa" "b:$VB:pb" "c:$VC:pc"; do
  dir="${pair#*:}"; dir="${dir%%:*}"
  pass_="${pair##*:}"
  HUSH_PASSPHRASE="$pass_" "$CLI" --dir "$dir" init --passphrase "$pass_" --relay "$RELAY_URL" >/dev/null
done

for pair in "$VA:$RA:pa" "$VB:$RB:pb" "$VC:$RC:pc"; do
  dir="${pair%%:*}"; rest="${pair#*:}"; rt="${rest%%:*}"
  HUSH_RUNTIME_DIR="$rt" "$DMN" --dir "$dir" >"$T/daemon-$(basename "$dir").log" 2>&1 &
  PIDS+=($!)
done
for _ in $(seq 1 60); do
  [ -S "$SOCK_A" ] && [ -S "$SOCK_B" ] && [ -S "$SOCK_C" ] && break
  sleep 0.1
done
[[ -S "$SOCK_A" && -S "$SOCK_B" && -S "$SOCK_C" ]] || fail "daemons did not come up"

for pair in "$SOCK_A:pa" "$SOCK_B:pb" "$SOCK_C:pc"; do
  api_at "${pair%%:*}" POST /api/unlock "{\"passphrase\":\"${pair##*:}\"}" >/dev/null
  [[ "$CODE" == "204" ]] || fail "unlock on ${pair%%:*} returned $CODE"
done

api_at "$SOCK_A" POST /api/invite >/dev/null; INV_A=$(jget "$T/out.json" invite)
api_at "$SOCK_B" POST /api/invite >/dev/null; INV_B=$(jget "$T/out.json" invite)
api_at "$SOCK_C" POST /api/invite >/dev/null; INV_C=$(jget "$T/out.json" invite)
[[ -n "$INV_A" && -n "$INV_B" && -n "$INV_C" ]] || fail "could not read peer invites"

# Alice must know both peers; Bob and Carol only need the convener to accept.
api_at "$SOCK_A" POST /api/contacts/add "{\"nickname\":\"bob\",\"invite\":\"$INV_B\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "A add-contact bob returned $CODE"
api_at "$SOCK_A" POST /api/contacts/add "{\"nickname\":\"carol\",\"invite\":\"$INV_C\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "A add-contact carol returned $CODE"
api_at "$SOCK_B" POST /api/contacts/add "{\"nickname\":\"alice\",\"invite\":\"$INV_A\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "B add-contact returned $CODE"
api_at "$SOCK_C" POST /api/contacts/add "{\"nickname\":\"alice\",\"invite\":\"$INV_A\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "C add-contact returned $CODE"
pass "three peers on one relay, contacts exchanged"

# --- Alice creates the conclave with Bob; Carol is invited later -------------
# The invite is store-and-forward, so Bob is not required to be online.
api_at "$SOCK_A" POST /api/conclaves '{"name":"paper-dogfood","trustees":["bob"]}' >/dev/null
[[ "$CODE" == "200" ]] || fail "create conclave returned $CODE: $RESP"
CONCLAVE=$(jget "$T/out.json" id)
[[ -n "$CONCLAVE" ]] || fail "create conclave returned no id: $RESP"
pass "conclave created by alice ($CONCLAVE)"

ACCEPTED=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" POST /api/conclaves/trustees/accept "{\"conclave_id\":\"$CONCLAVE\"}" >/dev/null
  [[ "$CODE" == "200" ]] && { ACCEPTED=1; break; }
  sleep 0.2
done
[[ -n "$ACCEPTED" ]] || fail "bob never accepted the invite (last: $CODE $RESP)"
pass "bob received the invite over the relay and joined"

# Alice must see the join before she fans anything out: paper payloads go to
# the joined set only, and an unjoined Bob would be silently skipped.
#
# `/api/recv` reports the join internally (the daemon needs it to decide about
# catch-up snapshots) but does not return it, so the roster is the observable
# signal here.
wait_for_join() { # wait_for_join <socket> <nick>
  local sock="$1" nick="$2" seen=""
  for _ in $(seq 1 50); do
    drain "$sock" >/dev/null
    api_at "$sock" GET /api/conclaves >/dev/null
    [[ "$(trustee_state "$T/out.json" "$CONCLAVE" "$nick")" == "joined" ]] && { seen=1; break; }
    sleep 0.2
  done
  [[ -n "$seen" ]] || fail "alice never observed $nick joining (roster: $(cat "$T/out.json"))"
}
wait_for_join "$SOCK_A" bob
pass "alice sees bob in the joined set"

# --- Alice creates the paper; the descriptor fans out ------------------------
api_at "$SOCK_A" POST "/api/paper/$CONCLAVE/create" '{"title":"Dogfood paper"}' >/dev/null
[[ "$CODE" == "200" ]] || fail "paper create returned $CODE: $RESP"
PAPER_ID=$(jget "$T/out.json" paper.paper_id)
[[ -n "$PAPER_ID" ]] || fail "paper create returned no paper_id: $RESP"
pass "alice created the paper ($PAPER_ID)"

ARRIVED=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
  [[ "$CODE" == "200" ]] && { ARRIVED=1; break; }
  sleep 0.2
done
[[ -n "$ARRIVED" ]] || fail "the paper descriptor never reached bob (last: $CODE $RESP)"
pass "the descriptor reached bob over the relay"

# --- Alice edits; Bob applies the delta and reads the same text --------------
EDIT_1="Alice: the paper works"
UPD_1=$(node "$EDIT" new "$EDIT_1")
api_at "$SOCK_A" POST "/api/paper/$CONCLAVE/update" "{\"update\":\"$UPD_1\"}" >/dev/null
[[ "$CODE" == "200" ]] || fail "alice's update returned $CODE: $RESP"

SEEN_B=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
  STATE_B=$(jget "$T/out.json" state)
  [[ -n "$STATE_B" ]] && TEXT_B=$(node "$EDIT" read "$STATE_B")
  if [[ "${TEXT_B:-}" == *"$EDIT_1"* ]]; then SEEN_B=1; break; fi
  sleep 0.2
done
[[ -n "$SEEN_B" ]] || fail "bob never converged on alice's edit (saw: ${TEXT_B:-<nothing>})"
pass "bob applied alice's delta and read the text back"

# --- Bob edits; Alice applies it; both documents are byte-identical ----------
EDIT_2="Bob: it does"
UPD_2=$(node "$EDIT" append "$STATE_B" "$EDIT_2")
api_at "$SOCK_B" POST "/api/paper/$CONCLAVE/update" "{\"update\":\"$UPD_2\"}" >/dev/null
[[ "$CODE" == "200" ]] || fail "bob's update returned $CODE: $RESP"
api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
FINAL_B=$(jget "$T/out.json" state)

CONVERGED=""
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
  FINAL_A=$(jget "$T/out.json" state)
  [[ "$FINAL_A" == "$FINAL_B" ]] && { CONVERGED=1; break; }
  sleep 0.2
done
[[ -n "$CONVERGED" ]] || fail "documents did not converge: a=$FINAL_A b=$FINAL_B"
TEXT_A=$(node "$EDIT" read "$FINAL_A")
[[ "$TEXT_A" == *"$EDIT_1"* && "$TEXT_A" == *"$EDIT_2"* ]] \
  || fail "the converged document lost text: $TEXT_A"
pass "bob's edit reached alice; both documents are byte-identical"

# --- Carol joins late, after both edits, and the hub catches her up ---------
# She was never a trustee while the deltas flew, and deltas are never replayed,
# so the only way she can have the document is the hub's catch-up snapshot.
api_at "$SOCK_A" POST /api/conclaves/trustees/invite \
  "{\"conclave_id\":\"$CONCLAVE\",\"nickname\":\"carol\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "inviting carol returned $CODE: $RESP"

ACCEPTED_C=""
for _ in $(seq 1 50); do
  drain "$SOCK_C" >/dev/null
  api_at "$SOCK_C" POST /api/conclaves/trustees/accept "{\"conclave_id\":\"$CONCLAVE\"}" >/dev/null
  [[ "$CODE" == "200" ]] && { ACCEPTED_C=1; break; }
  sleep 0.2
done
[[ -n "$ACCEPTED_C" ]] || fail "carol never accepted the invite (last: $CODE $RESP)"
wait_for_join "$SOCK_A" carol

CAUGHT_UP=""
for _ in $(seq 1 50); do
  drain "$SOCK_C" >/dev/null
  api_at "$SOCK_C" GET "/api/paper/$CONCLAVE" >/dev/null
  STATE_C=$(jget "$T/out.json" state)
  [[ "$STATE_C" == "$FINAL_A" ]] && { CAUGHT_UP=1; break; }
  sleep 0.2
done
[[ -n "$CAUGHT_UP" ]] || fail "carol never caught up: c=$STATE_C a=$FINAL_A"
TEXT_C=$(node "$EDIT" read "$STATE_C")
[[ "$TEXT_C" == *"$EDIT_1"* && "$TEXT_C" == *"$EDIT_2"* ]] \
  || fail "carol's document lost text: $TEXT_C"
pass "carol joined late and the hub's snapshot caught her up"

# --- The DataChannel fast path, proven by not draining the relay ------------
# The offer/answer are themselves relay traffic, so both sides have to drain
# until the channel is up. After that the relay is left untouched on purpose:
# an edit that still arrives has nowhere else to have come from.
LIVE_A=""; LIVE_B=""
for _ in $(seq 1 60); do
  drain "$SOCK_A" >/dev/null
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_A=$(jget "$T/out.json" fast_path.live_peers)
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_B=$(jget "$T/out.json" fast_path.live_peers)
  [[ "${LIVE_A:-0}" -ge 1 && "${LIVE_B:-0}" -ge 1 ]] && break
  sleep 0.2
done
[[ "${LIVE_A:-0}" -ge 1 && "${LIVE_B:-0}" -ge 1 ]] \
  || fail "the fast path never came up (alice=$LIVE_A bob=$LIVE_B)"
pass "the DataChannel is up between alice and bob (live peers: alice=$LIVE_A bob=$LIVE_B)"

# No `drain` in this loop, deliberately. Alice's relay fan-out is still queuing
# a copy for Bob; if Bob's document changes, the bytes came over the channel.
EDIT_3="Alice: over the channel"
UPD_3=$(node "$EDIT" append "$FINAL_A" "$EDIT_3")
api_at "$SOCK_A" POST "/api/paper/$CONCLAVE/update" "{\"update\":\"$UPD_3\"}" >/dev/null
[[ "$CODE" == "200" ]] || fail "alice's fast-path update returned $CODE: $RESP"

FAST=""
for _ in $(seq 1 40); do
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
  STATE_B=$(jget "$T/out.json" state)
  [[ -n "$STATE_B" ]] && TEXT_B=$(node "$EDIT" read "$STATE_B")
  if [[ "${TEXT_B:-}" == *"$EDIT_3"* ]]; then FAST=1; break; fi
  sleep 0.1
done
[[ -n "$FAST" ]] || fail "the fast path never delivered the edit (bob saw: ${TEXT_B:-<nothing>})"
pass "an edit crossed the DataChannel with the relay left undrained"

# --- Locking a hub degrades the peer rather than breaking the paper ---------
# Alice is the hub, so locking her vault closes every channel to her. Bob must
# fall back to the relay with no user action, and the edit must still land once
# the relay is drained again.
api_at "$SOCK_A" POST /api/lock >/dev/null
[[ "$CODE" == "204" ]] || fail "locking alice returned $CODE"

api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
LIVE_B=$(jget "$T/out.json" fast_path.live_peers)
[[ "${LIVE_B:-0}" -eq 0 ]] || fail "bob still reports a live peer after the hub locked ($LIVE_B)"
pass "locking the hub drops the peer to relay cadence, with no user action"

api_at "$SOCK_A" POST /api/unlock '{"passphrase":"pa"}' >/dev/null
[[ "$CODE" == "204" ]] || fail "unlocking alice returned $CODE"

EDIT_4="Bob: back on the relay"
api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
STATE_A=$(jget "$T/out.json" state)
UPD_4=$(node "$EDIT" append "$STATE_A" "$EDIT_4")
api_at "$SOCK_B" POST "/api/paper/$CONCLAVE/update" "{\"update\":\"$UPD_4\"}" >/dev/null
[[ "$CODE" == "200" ]] || fail "bob's post-lock update returned $CODE: $RESP"

RELAYED=""
for _ in $(seq 1 50); do
  drain "$SOCK_A" >/dev/null
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
  STATE_A=$(jget "$T/out.json" state)
  [[ -n "$STATE_A" ]] && TEXT_A=$(node "$EDIT" read "$STATE_A")
  if [[ "${TEXT_A:-}" == *"$EDIT_4"* ]]; then RELAYED=1; break; fi
  sleep 0.2
done
[[ -n "$RELAYED" ]] || fail "the relay did not carry the post-lock edit (alice saw: ${TEXT_A:-<nothing>})"
pass "an edit made after the hub locked still lands over the relay"

# --- The channel comes back, and presence rides it -------------------------
# The lock closed every channel, so both sides have to be drained until it is
# up again. The presence frames are then sent with nothing drained at all:
# presence has no relay copy, so a frame that arrives cannot have come from
# anywhere else.
LIVE_A=""; LIVE_B=""
for _ in $(seq 1 60); do
  drain "$SOCK_A" >/dev/null
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_A=$(jget "$T/out.json" fast_path.live_peers)
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_B=$(jget "$T/out.json" fast_path.live_peers)
  [[ "${LIVE_A:-0}" -ge 1 && "${LIVE_B:-0}" -ge 1 ]] && break
  sleep 0.2
done
[[ "${LIVE_A:-0}" -ge 1 && "${LIVE_B:-0}" -ge 1 ]] \
  || fail "the fast path never came back after the lock (alice=$LIVE_A bob=$LIVE_B)"
pass "the DataChannel came back on its own after the hub unlocked"

PRESENCE_B=$(node "$EDIT" presence bob "#2b6cb0")
api_at "$SOCK_B" POST "/api/paper/$CONCLAVE/presence" "{\"frame\":\"$PRESENCE_B\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "bob's presence frame returned $CODE: $RESP"

CURSOR_A=""
for _ in $(seq 1 40); do
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE/presence" >/dev/null
  FRAME=$(presence_frame "$T/out.json")
  [[ -n "$FRAME" ]] && CURSOR_A=$(node "$EDIT" cursor "$FRAME")
  [[ "$CURSOR_A" == "bob|#2b6cb0" ]] && break
  sleep 0.1
done
[[ "$CURSOR_A" == "bob|#2b6cb0" ]] \
  || fail "alice never saw bob's cursor (saw: ${CURSOR_A:-<nothing>})"
pass "bob's cursor reached the hub over the channel, name and colour intact"

PRESENCE_A=$(node "$EDIT" presence alice "#b7791f")
api_at "$SOCK_A" POST "/api/paper/$CONCLAVE/presence" "{\"frame\":\"$PRESENCE_A\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "alice's presence frame returned $CODE: $RESP"

CURSOR_B=""
for _ in $(seq 1 40); do
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE/presence" >/dev/null
  FRAME=$(presence_frame "$T/out.json")
  [[ -n "$FRAME" ]] && CURSOR_B=$(node "$EDIT" cursor "$FRAME")
  [[ "$CURSOR_B" == "alice|#b7791f" ]] && break
  sleep 0.1
done
[[ "$CURSOR_B" == "alice|#b7791f" ]] \
  || fail "bob never saw alice's cursor (saw: ${CURSOR_B:-<nothing>})"
pass "the hub's cursor reached the trustee: cursors are distinct in both directions"

# --- The convener's word moves the rendezvous ------------------------------
# The explicit half of succession. Alice is the convener, so she can name a new
# hub even though the current one is alive and reachable — the case that has no
# roster event behind it (a dead machine, a lost device).
api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
ALICE_ID=$(jget "$T/out.json" paper.hub)
[[ -n "$ALICE_ID" ]] || fail "could not read the hub out of the descriptor: $RESP"
api_at "$SOCK_A" GET /api/contacts >/dev/null
BOB_ID=$(contact_id "$T/out.json" bob)
[[ -n "$BOB_ID" ]] || fail "could not read bob's account id from alice's contacts"

api_at "$SOCK_A" POST "/api/paper/$CONCLAVE/hub/rehome" "{\"hub\":\"$BOB_ID\"}" >/dev/null
[[ "$CODE" == "200" ]] || fail "re-home returned $CODE: $RESP"
[[ "$(jget "$T/out.json" paper.hub)" == "$BOB_ID" ]] \
  || fail "the descriptor did not move: $(jget "$T/out.json" paper.hub)"
pass "the convener re-homed the paper to bob"

# A trustee cannot: the core refuses the claim, so there is no second copy of
# the roster in the daemon for it to be judged against.
api_at "$SOCK_B" POST "/api/paper/$CONCLAVE/hub/rehome" "{\"hub\":\"$BOB_ID\"}" >/dev/null
[[ "$CODE" == "403" ]] || fail "a trustee's re-home returned $CODE, expected 403: $RESP"
pass "a trustee cannot re-home the paper"

# Bob is the rendezvous now, so the other two dial him: the hub waits, the
# trustees offer. Bob's descriptor moves too, which is what tells him to wait.
MOVED_TO_B=""
for _ in $(seq 1 60); do
  drain "$SOCK_A" >/dev/null
  drain "$SOCK_B" >/dev/null
  drain "$SOCK_C" >/dev/null
  api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
  HUB_B=$(jget "$T/out.json" paper.hub)
  LIVE_B=$(jget "$T/out.json" fast_path.live_peers)
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_A=$(jget "$T/out.json" fast_path.live_peers)
  if [[ "$HUB_B" == "$BOB_ID" && "${LIVE_B:-0}" -ge 1 && "${LIVE_A:-0}" -ge 1 ]]; then
    MOVED_TO_B=1
    break
  fi
  sleep 0.2
done
[[ -n "$MOVED_TO_B" ]] \
  || fail "the fast path did not follow the rendezvous to bob (bob=$LIVE_B alice=$LIVE_A hub=$HUB_B)"
pass "both trustees re-dialled the new hub and liveness resumed"

# --- A kicked hub hands the paper to the convener, with no user action -----
# The automatic half. Alice kicks the hub: her own roster changes without a
# payload to notice it, which is why the rule is checked against the roster
# rather than against a departure event.
api_at "$SOCK_A" POST /api/conclaves/trustees/kick \
  "{\"conclave_id\":\"$CONCLAVE\",\"nickname\":\"bob\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "kicking bob returned $CODE: $RESP"

api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
[[ "$(jget "$T/out.json" paper.hub)" == "$ALICE_ID" ]] \
  || fail "the convener did not inherit the hub after kicking it: $(jget "$T/out.json" paper.hub)"
pass "kicking the hub moved the rendezvous to the convener, with no user action"

# The kicked vault keeps its copy and its history — the document is its own —
# but once it is told it was kicked it can no longer write: the fan-out would be
# refused, so a local-only edit nobody else will ever see is worse than a
# refusal.
api_at "$SOCK_B" GET "/api/paper/$CONCLAVE" >/dev/null
[[ "$CODE" == "200" ]] || fail "the kicked vault lost its copy of the paper ($CODE)"
STATE_B=$(jget "$T/out.json" state)
TEXT_B=$(node "$EDIT" read "$STATE_B")
[[ "$TEXT_B" == *"$EDIT_3"* ]] || fail "the kicked vault's copy lost text: $TEXT_B"

READONLY_B=""
for _ in $(seq 1 50); do
  drain "$SOCK_B" >/dev/null
  api_at "$SOCK_B" POST "/api/paper/$CONCLAVE/update" \
    "{\"update\":\"$(node "$EDIT" new "bob: still here")\"}" >/dev/null
  [[ "$CODE" == "403" ]] && { READONLY_B=1; break; }
  sleep 0.2
done
[[ -n "$READONLY_B" ]] || fail "the kicked vault could still write (last: $CODE $RESP)"
pass "the kicked vault keeps a readable copy and is refused writes once it is told"

# Carol learns the new rendezvous from the hub claim and dials the convener.
REDIALED=""
for _ in $(seq 1 60); do
  drain "$SOCK_A" >/dev/null
  drain "$SOCK_C" >/dev/null
  api_at "$SOCK_C" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_C=$(jget "$T/out.json" fast_path.live_peers)
  api_at "$SOCK_A" GET "/api/paper/$CONCLAVE" >/dev/null
  LIVE_A=$(jget "$T/out.json" fast_path.live_peers)
  if [[ "${LIVE_C:-0}" -ge 1 && "${LIVE_A:-0}" -ge 1 ]]; then REDIALED=1; break; fi
  sleep 0.2
done
[[ -n "$REDIALED" ]] \
  || fail "liveness never resumed on the inherited hub (carol=$LIVE_C alice=$LIVE_A)"
pass "the surviving trustees re-dialled the inherited hub and liveness resumed"

# --- An ended conclave's paper is read-only everywhere ---------------------
api_at "$SOCK_A" POST /api/conclaves/end "{\"conclave_id\":\"$CONCLAVE\"}" >/dev/null
[[ "$CODE" == "204" ]] || fail "ending the conclave returned $CODE: $RESP"

api_at "$SOCK_A" POST "/api/paper/$CONCLAVE/update" \
  "{\"update\":\"$(node "$EDIT" new "alice: after the end")\"}" >/dev/null
[[ "$CODE" == "403" ]] || fail "the convener could still write after the end ($CODE): $RESP"

READONLY_C=""
for _ in $(seq 1 50); do
  drain "$SOCK_C" >/dev/null
  api_at "$SOCK_C" POST "/api/paper/$CONCLAVE/update" \
    "{\"update\":\"$(node "$EDIT" new "carol: after the end")\"}" >/dev/null
  [[ "$CODE" == "403" ]] && { READONLY_C=1; break; }
  sleep 0.2
done
[[ -n "$READONLY_C" ]] \
  || fail "carol's paper stayed writable after the end (last: $CODE $RESP)"

# Read-only is not gone: the document is still there, and still the one the
# trustees converged on.
api_at "$SOCK_C" GET "/api/paper/$CONCLAVE" >/dev/null
[[ "$CODE" == "200" ]] || fail "the ended paper is unreadable ($CODE): $RESP"
STATE_C=$(jget "$T/out.json" state)
TEXT_C=$(node "$EDIT" read "$STATE_C")
[[ "$TEXT_C" == *"$EDIT_1"* && "$TEXT_C" == *"$EDIT_4"* ]] \
  || fail "the ended paper lost text: $TEXT_C"
pass "an ended conclave's paper is read-only on every vault, and still readable"

# --- Nothing on disk in the clear -------------------------------------------
# The relay's only store is this SQLite file (blobs live in its `blobs` table),
# and each vault's copy is `papers.enc`. Both must hold ciphertext.
[[ -s "$T/relay.db" ]] || fail "relay.db is missing or empty, so 'no plaintext' would be vacuous"
for vault in "$VA" "$VB" "$VC"; do
  [[ -s "$vault/papers.enc" ]] || fail "no papers.enc in $vault"
done

# Scan every file the run left behind rather than a hand-picked list, so a WAL
# or a sidecar that starts carrying payloads cannot slip past. Only the
# harness's own scratch response is excluded; a daemon log is deliberately in
# scope, since a payload printed into one would be plaintext on a trustee's
# disk, which is the thing being ruled out.
mapfile -t ON_DISK < <(find "$T" -type f ! -name 'out.json')
[[ ${#ON_DISK[@]} -gt 0 ]] || fail "found no files to scan, so 'no plaintext' would be vacuous"

for needle in "$EDIT_1" "$EDIT_2" "$EDIT_3" "$EDIT_4"; do
  if MATCH=$(grep -al "$needle" "${ON_DISK[@]}"); then
    fail "paper plaintext on disk in ${MATCH//$'\n'/, }: $needle"
  fi
done
pass "relay.db, all three vaults, and every other file on disk hold no paper plaintext"

echo
echo "paper dogfood ok: three daemons converged one paper over a real relay,"
echo "the DataChannel fast path was proven with the relay left undrained, and a"
echo "locked hub degraded its peer to relay cadence without losing the edit"
CLEAN=1

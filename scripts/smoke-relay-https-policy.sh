#!/usr/bin/env bash
# 5c.4 acceptance: release builds refuse non-loopback HTTP relays, debug allows them.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PROTOC="${ROOT}/.tools/protoc-dist/bin/protoc"
cd "$ROOT"
REL=./target/release/hush-cli
DBG=./target/debug/hush-cli

# The release path is the one that matters, so build it if it is missing.
[[ -x "$REL" ]] || { echo "building release hush-cli (first run)..."; cargo build --release -p hush-cli --quiet || exit 1; }
[[ -x "$DBG" ]] || cargo build -p hush-cli --quiet || exit 1

check() {
  local bin="$1" label="$2" url="$3" want="$4"
  local dir
  dir=$(mktemp -d)
  local out rc
  out=$("$bin" --dir "$dir" config set --relay "$url" 2>&1)
  rc=$?
  local got="ok"
  [[ $rc -ne 0 ]] && got="refused"
  local mark="PASS"
  [[ "$got" == "$want" ]] || mark="FAIL"
  printf '%-6s %-9s %-34s -> %-8s (want %s)\n' "$mark" "$label" "$url" "$got" "$want"
  [[ "$mark" == "FAIL" ]] && echo "        output: $out"
  rm -rf "$dir"
  [[ "$mark" == "PASS" ]]
}

fails=0
check "$REL" "release" "http://203.0.113.1:8787"   refused || fails=$((fails+1))
check "$REL" "release" "http://relay.example.com"  refused || fails=$((fails+1))
check "$REL" "release" "http://127.0.0.1:8787"     ok      || fails=$((fails+1))
check "$REL" "release" "http://localhost:8787"     ok      || fails=$((fails+1))
check "$REL" "release" "http://[::1]:8787"         ok      || fails=$((fails+1))
check "$REL" "release" "https://relay.example.com" ok      || fails=$((fails+1))
check "$DBG" "debug"   "http://10.0.0.5:8787"      ok      || fails=$((fails+1))
check "$DBG" "debug"   "https://relay.example.com" ok      || fails=$((fails+1))

echo
[[ $fails -eq 0 ]] && echo "5c.4 relay policy ok" || echo "5c.4 relay policy FAILED ($fails)"
exit "$fails"

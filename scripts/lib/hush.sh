#!/usr/bin/env bash
# Shared helpers for the scripts beside this file: prerequisite checks, the relay
# a run should use, and the npm bootstrap.
#
# A missing tool should be reported as "install this", not as whatever the build
# does when the tool is absent. The case that motivated this: libsignal's build
# script compiles a .proto file, so a missing protoc surfaces as a prost-build
# error three crates deep that never mentions protoc (see README.md).
#
# Sourced, never executed: nothing here exits on its own. Callers run the checks
# and let a non-zero return (or their own `set -e`) stop them.

HUSH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# The shared lab relay, for testing across machines (README: "Test together").
# Plain HTTP on a public host, which a release daemon refuses — see
# hush_relay_needs_debug_daemon. Address, onion service and deploy steps:
# docs/ops/relay-lab-vps.md.
HUSH_CLOUD_RELAY_URL="${HUSH_CLOUD_RELAY_URL:-http://137.184.129.205:8790}"

hush_prereq_note() { printf '%s\n' "$@" >&2; }

# Resolve protoc and export it.
#
# The export is the whole point. The crate that needs protoc (libsignal-rust)
# runs its own build script, in a process cargo starts separately, so
# crates/hush-core/build.rs cannot hand it one — only the environment the caller
# gives to cargo reaches it.
#
# Order: an explicit PROTOC, then the vendored copy, then PATH.
hush_require_protoc() {
  local vendored="${HUSH_ROOT}/.tools/protoc-dist/bin/protoc"
  local found

  if [[ -n "${PROTOC:-}" ]]; then
    if [[ -x "$PROTOC" ]]; then
      export PROTOC
      return 0
    fi
    hush_prereq_note "PROTOC is set to $PROTOC, which is not executable — looking for another protoc."
  fi
  if [[ -x "$vendored" ]]; then
    export PROTOC="$vendored"
    return 0
  fi
  if found="$(command -v protoc)"; then
    export PROTOC="$found"
    return 0
  fi

  hush_prereq_note \
    "protoc not found." \
    "" \
    "The Signal session code (libsignal-rust) compiles protos/WhisperTextProtocol.proto" \
    "from its build script, so a protoc binary is required before cargo can build anything" \
    "that depends on hush-core. Install one of:" \
    "" \
    "  Debian/Ubuntu    sudo apt install protobuf-compiler" \
    "  Fedora           sudo dnf install protobuf-compiler" \
    "  macOS            brew install protobuf" \
    "  any platform     unzip a protoc release into .tools/protoc-dist/" \
    "  already have one PROTOC=/path/to/protoc $0"
  return 1
}

hush_require_toolchain() {
  local missing=0
  if ! command -v cargo >/dev/null 2>&1; then
    hush_prereq_note "cargo not found. Install the Rust toolchain: https://rustup.rs"
    missing=1
  fi
  if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    hush_prereq_note "node/npm not found. Install Node.js LTS: https://nodejs.org"
    missing=1
  fi
  return "$missing"
}

# Everything a run script needs before it builds anything.
hush_check_prereqs() {
  local rc=0
  hush_require_toolchain || rc=1
  hush_require_protoc || rc=1
  return "$rc"
}

# Install the UI's npm dependencies when they are absent, so a fresh clone runs
# on the first try instead of failing at `npm run build` with "vite: not found".
# `npm ci` against the committed lockfile, so every clone gets the same tree; an
# existing node_modules is left alone (delete it to force a reinstall).
hush_ensure_node_modules() {
  local dir="$1"
  [[ -d "$dir/node_modules" ]] && return 0
  echo "No node_modules in ${dir#"$HUSH_ROOT"/} — installing with npm ci…"
  (cd "$dir" && npm ci)
}

# The relay a vault's config file carries (empty when the file is absent or sets
# none). This is plaintext client config, not vault state, so it reads without
# unlocking; the vault's own stored relay still wins at runtime, and for existing
# vaults the two agree by construction (see DEEPSEEKPROBLEMS.MD).
hush_vault_relay() {
  local cfg="$1/client-config.json"
  [[ -f "$cfg" ]] || return 0
  node -e '
    try {
      const c = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
      if (typeof c.relay_url === "string") process.stdout.write(c.relay_url);
    } catch {}
  ' "$cfg" 2>/dev/null
}

# Whether a vault exists in a directory, without opening it. `vault.lock.json` is
# the file hush-core's `Vault::create` writes first and refuses to overwrite, so
# this agrees with `vault_exists` in crates/hush-core/src/vault.rs.
hush_vault_present() {
  [[ -f "$1/vault.lock.json" ]]
}

# True when a release daemon would refuse this relay URL, so the daemon has to be
# built and run in debug instead.
#
# hush-core's HttpPolicy is the rule: a release build requires HTTPS unless the
# host is loopback or a v3 onion (an onion authenticates itself and Tor encrypts
# the link, so there is no certificate layer to demand). Debug builds allow plain
# HTTP anywhere, which is what makes a plain-HTTP relay usable for testing across
# machines without TLS in front of it.
hush_relay_needs_debug_daemon() {
  local url="${1:-}"
  url="${url,,}"
  [[ -z "$url" ]] && return 1
  [[ "$url" == https://* ]] && return 1
  [[ "$url" == *".onion"* ]] && return 1
  case "$url" in
    http://127.0.0.1* | http://localhost* | http://\[::1\]*) return 1 ;;
  esac
  return 0
}

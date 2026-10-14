#!/usr/bin/env bash
# A second Hush peer inside a lightweight VM, so two users can really talk:
# the host keeps one identity (UI + daemon), the VM runs the other as a
# headless daemon on the same relays. Both meet on the hakim relay (or any
# relay you pass), so this is a genuinely separate machine on the wire —
# separate vault, keys, NAT, and network position — not a second process
# pretending to be someone else.
#
# The VM is deliberately minimal: an Ubuntu server cloud image (~600 MB,
# 1 GB RAM, 2 vCPUs, thin qcow2) with no desktop. The prebuilt daemon binary
# is handed over at first boot over QEMU's builtin HTTP (10.0.2.2), so the
# guest installs nothing and matches this host's glibc exactly. The voice
# peer inside runs with HUSH_VOICE_TEST_TONE=1: it transmits a 440 Hz tone
# and counts what it receives, so a human on the host hears the tone and
# watches decode counters climb — cross-machine audio proven in both
# directions without a second microphone.
#
# Storage is on the SSD by default (/var/tmp — /home is the HDD here), and
# the base image is kept pristine with a thin overlay per instance, so
# --fresh (delete the overlay, re-provision) costs seconds, not a download.
#
#   ./scripts/vm-peer.sh              # provision if needed, boot, wait for the daemon
#   ./scripts/vm-peer.sh --status     # is the VM up, is the daemon answering
#   ./scripts/vm-peer.sh --down       # graceful shutdown (SSH), fallback power-off
#   ./scripts/vm-peer.sh --fresh      # wipe the disk overlay and re-provision
#   ./scripts/vm-peer.sh --ssh        # shell into the VM
#
# The relay comes from the host's peer-a vault config when that is remote
# (the hakim tunnel), falling back to the local default; --relay overrides.
# Passphrase defaults to vm-peer-pass (--pass to change).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="${HUSH_VM_DIR:-/var/tmp/hush-vm-peer}"
PASS="vm-peer-pass"
RELAY_ARG=""
ACTION="up"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --down | --status | --fresh | --ssh) ACTION="${1#--}"; shift ;;
    --relay) [[ $# -ge 2 ]] || { echo "--relay needs a URL." >&2; exit 2; } ; RELAY_ARG="$2"; shift ;;
    --relay=*) RELAY_ARG="${1#--relay=}"; shift ;;
    --pass) [[ $# -ge 2 ]] || { echo "--pass needs a value." >&2; exit 2; } ; PASS="$2"; shift ;;
    --dir) [[ $# -ge 2 ]] || { echo "--dir needs a path." >&2; exit 2; } ; DIR="$2"; shift ;;
    -h | --help) awk 'NR>1 && /^#/ { sub(/^# ?/, ""); print; next } NR>1 { exit }' "$0"; exit 0 ;;
    *) echo "Unknown argument: $1 (see --help)." >&2; exit 2 ;;
  esac
done

# The VM lives on the SSD: /var/tmp persists across reboots and this host's
# /home is the spinning disk. Refuse HDD paths outright — a slow image disk
# is exactly what this setup exists to avoid.
if [[ -e "$DIR" ]]; then
  FS_OF_DIR="$(df -T "$DIR" | awk 'NR==2 {print $2}')"
else
  mkdir -p "$DIR"
  FS_OF_DIR="$(df -T "$DIR" | awk 'NR==2 {print $2}')"
fi
if df -T "$DIR" 2>/dev/null | awk 'NR==2 {print $2}' | grep -qE '^(none|proc|sysfs|tmpfs)$'; then
  echo "Refusing $DIR: RAM-backed filesystem; the disk image would eat RAM." >&2
  exit 1
fi

IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
BASE="$DIR/base.qcow2"
DISK="$DIR/disk.qcow2"
SSH_PORT="2222"
API_PORT="9877"
SEED_PORT="8009"
SSH_KEY="$DIR/id_ed25519"
WWW="$DIR/www"
CONSOLE="$DIR/console.log"

vm_pid() { [[ -f "$DIR/qemu.pid" ]] && kill -0 "$(cat "$DIR/qemu.pid")" 2>/dev/null; }
api_ready() {
  curl -sf -o /dev/null --connect-timeout 2 "http://127.0.0.1:${API_PORT}/api/status"
}
vm_ssh() {
  ssh -i "$SSH_KEY" -p "$SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR -o ConnectTimeout=3 peer@127.0.0.1 "$@"
}

# Relay: explicit --relay wins; otherwise the host vaults' choice (they are
# the source of truth the daemons use anyway); otherwise the local default.
resolve_relay() {
  if [[ -n "$RELAY_ARG" ]]; then
    echo "$RELAY_ARG"
    return
  fi
  local cfg="${ROOT}/instances/peer-a/client-config.json"
  if [[ -f "$cfg" ]]; then
    local url
    url="$(node -e '
      try {
        const c = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
        if (typeof c.relay_url === "string") process.stdout.write(c.relay_url);
      } catch {}
    ' "$cfg" 2>/dev/null || true)"
    if [[ -n "$url" && "$url" != http://127.0.0.1:* && "$url" != http://localhost:* ]]; then
      echo "$url"
      return
    fi
  fi
  echo "http://127.0.0.1:8787"
}
RELAY_URL="$(resolve_relay)"

case "$ACTION" in
  status)
    if vm_pid; then
      echo "VM: running (pid $(cat "$DIR/qemu.pid"))"
    else
      echo "VM: not running"
    fi
    if api_ready; then
      echo "Peer daemon: answering on http://127.0.0.1:${API_PORT}"
      curl -s "http://127.0.0.1:${API_PORT}/api/status" | node -e '
        let b = ""; process.stdin.on("data", (d) => (b += d)).on("end", () => {
          const s = JSON.parse(b);
          console.log(`  locked: ${s.locked}  relay: ${s.relay ?? "?"} (${s.relay_source ?? "?"})`);
        });'
    else
      echo "Peer daemon: not answering"
    fi
    exit 0
    ;;
  down)
    if ! vm_pid; then
      echo "VM: not running."
      exit 0
    fi
    PID="$(cat "$DIR/qemu.pid")"
    echo "Powering off the VM (pid $PID) …"
    if vm_ssh "systemctl poweroff" 2>/dev/null; then
      for _ in $(seq 1 30); do
        kill -0 "$PID" 2>/dev/null || break
        sleep 1
      done
    fi
    if kill -0 "$PID" 2>/dev/null; then
      echo "Graceful path timed out; stopping QEMU."
      kill "$PID" 2>/dev/null || true
      sleep 2
      kill -9 "$PID" 2>/dev/null || true
    fi
    pkill -f "python3 -m http.server ${SEED_PORT}" 2>/dev/null || true
    echo "VM is down."
    exit 0
    ;;
  ssh)
    exec ssh -i "$SSH_KEY" -p "$SSH_PORT" -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null peer@127.0.0.1
    ;;
esac

# --- ACTION == up (and fresh) -------------------------------------------

if [[ "$ACTION" == "fresh" ]]; then
  vm_pid && { echo "Stop the VM first (--down)." >&2; exit 1; }
  rm -f "$DISK"
  echo "Disk overlay removed; the VM will re-provision."
fi

if vm_pid; then
  echo "VM already running (pid $(cat "$DIR/qemu.pid"))."
else
  command -v qemu-system-x86_64 >/dev/null || { echo "qemu-system-x86_64 is not installed." >&2; exit 1; }
  [[ -w /dev/kvm ]] || { echo "/dev/kvm is not accessible (KVM acceleration unavailable)." >&2; exit 1; }

  if [[ ! -f "$BASE" ]]; then
    mkdir -p "$DIR"
    echo "Downloading the base image to $BASE (once, ~600 MB) …"
    curl -fL --retry 3 -o "$BASE.part" "$IMAGE_URL"
    mv "$BASE.part" "$BASE"
  fi

  if [[ ! -f "$DISK" ]]; then
    echo "Creating the disk overlay …"
    qemu-img create -f qcow2 -b "$BASE" -F qcow2 "$DISK" >/dev/null
  fi

  [[ -f "$SSH_KEY" ]] || ssh-keygen -q -t ed25519 -N "" -f "$SSH_KEY" -C hush-vm-peer

  # NoCloud seed + binaries, served to the guest at 10.0.2.2:$SEED_PORT.
  # cloud-init reads the seed from the SMBIOS serial; the binaries ride the
  # same server so the guest installs nothing.
  mkdir -p "$WWW"
  cat > "$WWW/meta-data" <<EOF
instance-id: hush-vm-peer
local-hostname: hush-vm-peer
EOF
  PUBKEY="$(cat "$SSH_KEY.pub")"
  cat > "$WWW/user-data" <<EOF
#cloud-config
# libasound2t64: the daemon links cpal -> ALSA, and the server cloud image
# ships no ALSA userspace. The voice peer runs in tone mode and never opens
# a device, but the dynamic link must resolve.
packages: [libasound2t64]
users:
  - name: peer
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - $PUBKEY
write_files:
  - path: /etc/systemd/system/hush-vm-peer.service
    content: |
      [Unit]
      Description=Hush VM peer daemon
      After=network-online.target
      Wants=network-online.target
      [Service]
      User=peer
      Environment=HUSH_VOICE_TEST_TONE=1
      ExecStart=/usr/local/bin/hush-daemon --dir /home/peer/vault --socket /home/peer/daemon.sock --addr 0.0.0.0:${API_PORT}
      Restart=always
      [Install]
      WantedBy=multi-user.target
  # The peer is headless: nothing clicks "receive", so a 10 s tick drains the
  # relay queue (conclave invites, mail) into the vault. Store-and-forward
  # semantics make the tick advisory — mail waits either way.
  - path: /etc/systemd/system/hush-vm-drain.service
    content: |
      [Unit]
      Description=Hush VM peer relay drain tick
      After=hush-vm-peer.service
      [Service]
      User=peer
      ExecStart=/bin/sh -c "while true; do curl -sf -X POST http://127.0.0.1:${API_PORT}/api/recv >/dev/null 2>&1 || true; sleep 10; done"
      Restart=always
      [Install]
      WantedBy=multi-user.target
runcmd:
  - set -e
  - curl -sf -o /usr/local/bin/hush-daemon http://10.0.2.2:${SEED_PORT}/hush-daemon
  - curl -sf -o /usr/local/bin/hush-cli http://10.0.2.2:${SEED_PORT}/hush-cli
  - chmod 755 /usr/local/bin/hush-daemon /usr/local/bin/hush-cli
  - install -d -o peer -g peer /home/peer/vault
  - sudo -u peer hush-cli --dir /home/peer/vault init --passphrase '${PASS}' --relay '${RELAY_URL}'
  - systemctl enable --now hush-vm-peer.service hush-vm-drain.service
EOF

  # Fresh seed on every boot: a new instance-id makes cloud-init re-run, which
  # is what --fresh relies on; harmless when the disk is already provisioned
  # (the init command fails harmlessly, the unit is already enabled).
  echo "instance-id: hush-vm-peer-$(date +%s)" > "$WWW/meta-data"

  # The binaries must be readable while the seed server runs; copy them out
  # of the (HDD) target dir once, so the guest fetches from fast local disk
  # and the server's directory never points into the build tree.
  cp -f "${ROOT}/target/release/hush-daemon" "${ROOT}/target/release/hush-cli" "$WWW/"

  echo "Relay: $RELAY_URL"
  echo "Serving the seed and binaries on 127.0.0.1:${SEED_PORT} …"
  pkill -f "http.server ${SEED_PORT}" 2>/dev/null || true
  (python3 -m http.server "$SEED_PORT" --bind 127.0.0.1 --directory "$WWW" \
    >"$DIR/seed-server.log" 2>&1 &) || true
  for _ in $(seq 1 20); do
    curl -sf -o /dev/null "http://127.0.0.1:${SEED_PORT}/user-data" && break
    sleep 0.2
  done

  echo "Booting the VM …"
  qemu-system-x86_64 \
    -enable-kvm -cpu host -m 1G -smp 2 \
    -drive file="$DISK",if=virtio,format=qcow2 \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:22,hostfwd=tcp:127.0.0.1:${API_PORT}-:${API_PORT} \
    -display none -daemonize -pidfile "$DIR/qemu.pid" \
    -serial "file:$CONSOLE" \
    -smbios "type=1,serial=ds=nocloud;s=http://10.0.2.2:${SEED_PORT}/"
  echo "VM up (pid $(cat "$DIR/qemu.pid")). First boot provisions the peer: binary, vault, daemon."
fi

echo "Waiting for the peer daemon on port ${API_PORT} (first boot: a few minutes) …"
READY=""
for _ in $(seq 1 240); do
  if api_ready; then READY=1; break; fi
  vm_pid || { echo "QEMU exited unexpectedly — see $CONSOLE" >&2; exit 1; }
  sleep 2
done
if [[ -z "$READY" ]]; then
  echo "The peer daemon did not come up; last console lines:" >&2
  tail -n 20 "$CONSOLE" >&2 || true
  exit 1
fi

# A lab peer is only useful unlocked; say so plainly rather than hide it.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
  -H 'Content-Type: application/json' -d "{\"passphrase\":\"$PASS\"}" \
  http://127.0.0.1:${API_PORT}/api/unlock || true)"
if [[ "$CODE" == "204" || "$CODE" == "200" ]]; then
  echo "Vault unlocked (passphrase: $PASS — ./scripts/vm-peer.sh --pass to change)."
else
  echo "warning: unlock returned $CODE — unlock by hand:" \
    "curl -X POST -d '{\"passphrase\":\"$PASS\"}' http://127.0.0.1:${API_PORT}/api/unlock" >&2
fi

echo ""
echo "The VM peer is live:  http://127.0.0.1:${API_PORT}  (relay: $RELAY_URL)"
echo ""
echo "Open the host side and exchange invites:"
echo "  ./scripts/run-two-ui.sh"
echo "  Alice's window: Copy my invite  -> paste on the VM side (or reverse)."
echo ""
echo "Drive the VM peer directly:"
echo "  curl -s http://127.0.0.1:${API_PORT}/api/status"
echo "  ./scripts/vm-peer.sh --ssh            # shell into the VM"
echo "  ./scripts/vm-peer.sh --status"
echo ""
echo "Voice: the VM speaks a 440 Hz tone and counts what it hears"
echo "(HUSH_VOICE_TEST_TONE=1). Join a conclave with it and listen."

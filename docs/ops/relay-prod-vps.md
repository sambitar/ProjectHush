# Production relay VPS (37.27.150.250)

Provisioned 2026-10-16. This is the **production** relay box (Hetzner, instance line
`CAX` — **aarch64**). Do not follow the lab doc's build-and-scp path verbatim here: the
workstation is x86_64, so the binary must be cross-compiled — see "Deploying a new binary".
The lab box and its constraints are documented separately in `relay-lab-vps.md`.

Host: `37.27.150.250` (Ubuntu 24.04.4, glibc 2.39, 2 vCPU, 3.8 GB RAM, 38 GB disk,
hostname `Relay`). SSH: `ssh hush-relay-prod` (alias in `~/.ssh/config`, key
`~/.ssh/hush_relay_vps`, user `deploy`, passwordless sudo). Root SSH is **disabled**
(`PermitRootLogin no`, `PasswordAuthentication no` via
`/etc/ssh/sshd_config.d/99-hush-hardening.conf`).

## What is installed

| Service | Unit | Bind | DB / state |
|---|---|---|---|
| hush relay | `hush-relay.service` (enabled) | `127.0.0.1:8787` loopback only | `/var/lib/hush-relay/relay/relay.db` |
| Caddy 2.6.2 (TLS terminator) | `caddy.service` | `:80`, `:443` | `/etc/caddy/Caddyfile` |
| Tor 0.4.9.11 (onion service) | `tor@default.service` | loopback, HS→`127.0.0.1:8787` | `/var/lib/tor/hush-relay/` |

Firewall (ufw): `22/tcp LIMIT`, `80/tcp`, `443/tcp`. Everything else denied. 8787 is not
reachable from outside — verify with `curl --max-time 5 http://37.27.150.250:8787/health`
(must time out).

Other pieces: `deploy` user (`sudo` NOPASSWD via `/etc/sudoers.d/deploy`), 2 GB swapfile at
`/swapfile` with `vm.swappiness=10` (`/etc/sysctl.d/70-hush-relay.conf`),
`unattended-upgrades` active. The systemd unit is a verbatim copy of
`packaging/systemd/system/hush-relay.service` (loopback bind, sandbox, `MemoryMax=512M`).
`packaging/cloud-init/relay-vps.yaml` documents the intended provisioning but was **not**
attached at instance creation (cloud-init ran with no user data); the pieces were installed
by hand in the same shape. SSH hardening and the `deploy` user were added after first boot
because the provider put the panel key on root.

## Deploying a new binary

The box is **aarch64**; the workstation is x86_64. Cross-compile in a disposable container
(source mounted read-only, build artifacts kept out of the host `target/`), then ship:

```sh
docker run --rm -v /home/sam/ProjectHush:/hush:ro -v /tmp/hush-cross:/out ubuntu:24.04 bash -c '
  set -e
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq && apt-get install -y -qq build-essential gcc-aarch64-linux-gnu protobuf-compiler curl git ca-certificates >/dev/null
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable --target aarch64-unknown-linux-gnu >/dev/null
  . "$HOME/.cargo/env" && cd /hush
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc CARGO_TARGET_DIR=/tmp/target
  cargo build --release --locked -p hush-relay --target aarch64-unknown-linux-gnu
  cp /tmp/target/aarch64-unknown-linux-gnu/release/hush-relay /out/hush-relay.aarch64'

scp target…/hush-relay.aarch64 hush-relay-prod:/tmp/hush-relay
ssh hush-relay-prod 'sudo install -Dm755 /tmp/hush-relay /usr/local/bin/hush-relay \
  && sudo systemctl restart hush-relay'
```

glibc matches (workstation container and box both Ubuntu 24.04 / 2.39), so no sysroot work
is needed — only the architecture differs. First deployment needed
`sudo systemctl reset-failed hush-relay` because the unit had crash-looped
(`203/EXEC`, x86_64 binary on ARM) and hit its start limit.

## Tor onion service

- **Onion address:** `qvo7toe6urizihhbj7tp4ibsf2rhkilsrwvnkdk5jlwubjm3e7rnrlad.onion`
- Three-hop service (no `HiddenServiceSingleHopMode`), so the VPS location stays hidden.
  Config: `/etc/tor/torrc.d/hush-relay.torrc` (included from `/etc/tor/torrc` via
  `%include /etc/tor/torrc.d`). Key material: `/var/lib/tor/hush-relay/` (mode 700, owner
  `debian-tor`) — back it up to keep the address across a rebuild.
- **Changed 2026-10-16:** tor config edits require `sudo systemctl restart tor@default`;
  the package's own enable had started tor before the `%include` line existed, so a running
  `Bootstrapped 100%` does not mean the hidden service is loaded.
- Verified 2026-10-16: `curl --socks5-hostname 127.0.0.1:9050 http://<onion>/health` → 200
  (~2 s, real onion round trip).
- Clients: release builds accept a plain-HTTP v3 onion base; route the client through Tor
  with `HUSH_RELAY_SOCKS_PROXY=socks5h://127.0.0.1:9050` (see `relay-lab-vps.md`).

## Not done (blocker: domain)

- **TLS on Caddy.** `/etc/caddy/Caddyfile` still carries the placeholder
  `relay.example.com`, so Caddy holds only port 80 and logs a failing ACME attempt; release
  clients need `https://`. Fix: point a DNS A record at `37.27.150.250`, replace the
  placeholder with the real name, `sudo systemctl reload caddy`, verify
  `curl https://<name>/health` returns 200 with a publicly trusted chain. The requirements
  (verbatim path forwarding, no URI logs, 2 MB body cap) are already in the Caddyfile and
  match `relay-https-checklist.md`.
- A vault stores the relay it was created against; once TLS is up, create vaults against
  `https://<name>` (or repoint existing ones with `hush-cli relay set --url …`).
- Reboot test not run.

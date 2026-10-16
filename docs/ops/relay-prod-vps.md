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
- **Key backup (2026-10-16):** the whole `hush-relay` service dir is copied off-box to
  `~/.config/hush-relay-prod/onion-backup/hush-relay/` on the workstation (mode 600),
  so the onion address survives even losing the droplet. Restore = untar to
  `/var/lib/tor/` before starting tor.
- **Changed 2026-10-16:** tor config edits require `sudo systemctl restart tor@default`;
  the package's own enable had started tor before the `%include` line existed, so a running
  `Bootstrapped 100%` does not mean the hidden service is loaded.
- Verified 2026-10-16: `curl --socks5-hostname 127.0.0.1:9050 http://<onion>/health` → 200
  (~2 s, real onion round trip), again after the reboot test below.
- Clients: release builds accept a plain-HTTP v3 onion base and now carry this address as
  the baked-in default (`DEFAULT_RELAY_ONION_URL` in `crates/hush-core/src/config.rs`);
  a per-vault switch selects Tor relay mode and fails closed when Tor is not connected.
  Manual route: `HUSH_RELAY_SOCKS_PROXY=socks5h://127.0.0.1:9050` (see `relay-lab-vps.md`).

## Done 2026-10-16: TLS origin is live

- **Domain:** `hushrelay.space` (registered at Namecheap 2026-10-16; A record
  `@` → `37.27.150.250`, `www` CNAME → apex). DNS-only, no proxy in front.
- **Certificate:** issued by Let's Encrypt via Caddy's automatic ACME (HTTP-01), renewed
  automatically. `/etc/caddy/Caddyfile` uses the real name; paths forward verbatim,
  access logs discarded, 2 MB body cap — see `relay-https-checklist.md`.
- **Verified 2026-10-16:** `curl https://hushrelay.space/health` → `200`,
  `ssl_verify_result=0` (publicly trusted, so release clients accept it with no
  configuration). External `:8787` probe times out; relay remains loopback-only.

## Vault relay URL

Create vaults against **`https://hushrelay.space`**. A vault stores the relay it was
created against; repoint existing ones with `hush-cli relay set --url https://hushrelay.space`.
The onion base above remains valid for clients that route through Tor.

## Permanence record (2026-10-16)

- **Reboot test passed.** `systemctl reboot` on the box: `hush-relay`, `caddy` and
  `tor@default` all came back `active`; `https://hushrelay.space/health` → 200
  (`ssl_verify_result=0`); onion address unchanged, onion `/health` → 200. All units are
  `enabled`, so a crash or provider reboot self-heals.
- **Defaults baked into release builds** (`crates/hush-core/src/config.rs`): a release
  build dials `https://hushrelay.space` when nothing is configured and carries the onion
  twin for Tor relay mode; debug builds keep the loopback default for local dev. The run
  scripts (`scripts/lib/hush.sh`) default `HUSH_CLOUD_RELAY_URL` to the production relay
  and export `HUSH_RELAY_ONION_URL`.
- **Onion identity survives loss of the droplet:** key backup on the workstation (above).

## Not done

- **Onion auto-discovery.** The relay cannot currently tell a client its own onion address
  (the daemon would have to read `/var/lib/tor/hush-relay/hostname`, outside the unit's
  sandbox). The address is instead baked into release builds — which means a relay change
  needs a client release. If the relay ever moves, add an authenticated discovery route or
  a well-known blob on the clearnet origin as the bootstrap answer.

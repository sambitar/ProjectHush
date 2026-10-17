# Production relay VPS (37.27.150.250)

Provisioned 2026-10-16. This is the **production** relay box (Hetzner, instance line
`CAX` — **aarch64**). Do not follow the lab doc's build-and-scp path verbatim here: the
workstation is x86_64, so the binary must be cross-compiled — see "Deploying a new binary".
The lab box and its constraints are documented separately in `relay-lab-vps.md`.

**Deployed 2026-10-17:** new binary with the WAL store (`PRAGMA journal_mode = WAL`,
`synchronous = NORMAL`, `busy_timeout = 5000`) and the two-phase batched drain routes
(`POST /q/{id}/drain` peek page, `POST /q/{id}/drain/ack` confirm; legacy per-blob routes
kept). Verified after restart: service `active`; `https://hushrelay.space/health` → 200;
drain route answers 401 to a tokenless probe (registered, not 404); onion `/health` → 200
through tor; `relay.db-wal`/`-shm` beside the DB. Previous binary kept at
`/usr/local/bin/hush-relay.bak-20261016`. Cross-built from the working tree at commit-time
with the container command below (`--locked`).

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

## Done 2026-10-16: release artifacts and the updater manifest are served from this box

Installed copies of Hush update from `https://hushrelay.space/updates/latest.json`. The
GitHub release cannot serve that: the repository is private and GitHub 404s unauthenticated
release downloads, which is exactly what the updater makes. The same directory is also the
public download location for the installers, which the private release page cannot be.

**Caddy route.** One block ahead of the proxy in `/etc/caddy/Caddyfile`. `handle_path`
strips the `/updates` prefix, so `latest.json` lives at the root of the served directory:

```caddyfile
hushrelay.space {
	handle_path /updates/* {
		root * /var/www/hush-updates
		# The manifest must never be served stale; artifacts revalidate via ETag.
		header Cache-Control "no-cache"
		file_server
	}

	reverse_proxy 127.0.0.1:8787
	header Strict-Transport-Security "max-age=31536000; includeSubDomains"
	request_body {
		max_size 2MB
	}
	log {
		output discard
	}
}
```

The live file is **not** in version control; the block above is the copy to restore from.
Validate before installing — `sudo caddy validate --config /tmp/Caddyfile.new --adapter
caddyfile` — then `sudo install -m644 /tmp/Caddyfile.new /etc/caddy/Caddyfile && sudo
systemctl reload caddy`, which is zero-downtime.

**Served directory.** `/var/www/hush-updates`, owner `relay-updates:relay-updates`, mode
755, files 0644. Caddy runs as `caddy`, which is not in that group, so the world-readable
bit is what makes the files servable — `rsync --chmod=D755,F644` is not cosmetic.

**Upload account.** `relay-updates` is a system user with **no sudo** (`sudo -l -U
relay-updates` → "not allowed to run sudo"). Its key is installed as:

```sh
sudo install -d -o relay-updates -g relay-updates -m 700 /home/relay-updates/.ssh
printf 'restrict %s\n' "$(cat deploy_key.pub)" \
  | sudo -u relay-updates tee /home/relay-updates/.ssh/authorized_keys
```

`restrict` covers `no-pty`, `no-port-forwarding`, `no-agent-forwarding`, `no-X11-forwarding`
and `no-user-rc`, so the CI key cannot open a tunnel or a shell for interactive use — it can
only run commands as this unprivileged account. Verified 2026-10-16: a `-L` tunnel dies with
`channel 2: open failed: administratively prohibited`, `ssh -tt` reports `PTY allocation
request failed`, `rsync` into `/etc` fails with code 23, and `rsync` into
`/var/www/hush-updates` succeeds. There is deliberately **no forced command**: rsync needs
to exec `rsync --server` on the far side.

The private half lives on the workstation at `~/.ssh/hush-updates-deploy` (mode 600) and in
the repository secrets `RELAY_UPDATES_SSH_KEY` and `RELAY_UPDATES_KNOWN_HOSTS` (the pinned
ed25519 host key, so an upload cannot be redirected to another host).

**Publishing.** `scripts/publish-updates-to-relay.sh` does the work, and
`.github/workflows/publish-updates.yml` runs it on `release: published` (or by hand:
`gh workflow run publish-updates.yml -f tag=v0.1.1`). It downloads the release assets,
rewrites every URL in `latest.json` onto this origin, uploads the bundles, HEADs each one
over HTTPS, and uploads the manifest **last** — a client that has just read a manifest
always finds its artifact in place. It then re-fetches the public manifest and checks every
platform URL resolves, so a green run means the origin is actually serving.

By hand, from the workstation:

```sh
scripts/publish-updates-to-relay.sh --tag v0.1.1                 # downloads, uploads, verifies
scripts/publish-updates-to-relay.sh --tag v0.1.1 --dry-run       # uploads nothing
```

Check the origin directly:

```sh
curl -sS https://hushrelay.space/updates/latest.json | jq '{version, platforms: (.platforms | keys)}'
curl -sSI https://hushrelay.space/updates/Hush_aarch64.app.tar.gz | head -1
```

**Names and accumulation.** The Linux bundles carry the version
(`Hush_0.1.0_amd64.AppImage`), so releases accumulate; the macOS updater artifacts do not
(`Hush_aarch64.app.tar.gz`), so each release overwrites the last. `v0.1.0` is 210 MB in
total against 34 GB free, so pruning is not needed yet — revisit after roughly a hundred
releases, and keep at least the previous release's files when doing it, since a client that
has read an older manifest still needs them.

**Note for automation:** ufw rate-limits port 22 (`22/tcp LIMIT`). A loop of ssh/rsync
connections to this box starts getting `Connection refused` after six new connections in 30
seconds, with every other port still healthy. Space remote calls out; the publish workflow
opens one rsync connection per run and is unaffected.


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

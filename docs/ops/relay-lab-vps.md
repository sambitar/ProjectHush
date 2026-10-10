# Lab VPS relay (137.184.129.205)

Provisioned 2026-10-08. This is a **lab** box: plain HTTP, no TLS, no firewall lockdown,
no hardening. It is deliberately not production-shaped.

Host: `root@137.184.129.205` (`ubuntu-s-1vcpu-1gb-nyc1`, Ubuntu 24.04.4, glibc 2.39,
x86_64, 1 vCPU, 961 MB RAM). Access with `-i /home/sam/.ssh/alhakim_deploy`; the
`kalshi-vm` ssh-config alias points at the same IP with a different key that does not
authenticate.

## What is installed

| Service | Unit | Bind | DB / state |
|---|---|---|---|
| hush relay | `hush-relay.service` (enabled) | `0.0.0.0:8790` | `/var/lib/hush-relay/relay/relay.db` |

OpenSMTPD (`opensmtpd.service`, `127.0.0.1:25`) is still installed on the box from the
outsider-mail era, but nothing in the repo uses it any more — see the removal note below.
It is kept off the books: do not build on it.

Neither service existed before this change; both are additions. Pre-existing services
(nginx:80, alhakim:3000, mcp-ironlink:8787, ipfs, cloudflared, BIRD:179, the wg-* units)
were not touched. The prior listening-port and unit list is saved on the box at
`/root/hush-lab/prior-state.txt`.

### Why port 8790 and not 8787

`0.0.0.0:8787` — the relay's default port — was already listening when this box was
reached. It is **not** a hush relay: it is `mcp-ironlink.service`
(`/usr/bin/node dist/http.js`, cwd `/opt/mcp-ironlink`, `MCP_HTTP_PORT=8787`). It was left
untouched. The relay therefore binds `8790` via a drop-in rather than a second process on
8787.

## Relay: build and deploy

The box has ~140 MB free RAM, so `cargo build` on it would thrash or OOM. The binary is
built on the workstation and copied over. That is safe here because the workstation and
the box are both Ubuntu 24.04 / **glibc 2.39 / x86_64**, so the binary's glibc floor
(`GLIBC_2.34`) is satisfied. No container build was needed. If the workstation glibc ever
exceeds the box's, build in a container targeting `ubuntu:24.04` instead.

```sh
cargo build --release --locked -p hush-relay
scp target/release/hush-relay root@137.184.129.205:/tmp/hush-relay
scp packaging/systemd/system/hush-relay.service            root@137.184.129.205:/tmp/
scp packaging/systemd/system/hush-relay.service.d/10-lab-public.conf root@137.184.129.205:/tmp/
```

On the box:

```sh
useradd --system --no-create-home --shell /usr/sbin/nologin hush-relay
install -Dm755 /tmp/hush-relay /usr/local/bin/hush-relay
install -Dm644 /tmp/hush-relay.service /etc/systemd/system/hush-relay.service
install -Dm644 /tmp/10-lab-public.conf \
  /etc/systemd/system/hush-relay.service.d/10-lab-public.conf
systemctl daemon-reload && systemctl enable --now hush-relay
```

The shipped `packaging/systemd/system/hush-relay.service` is used **unmodified**. It binds
loopback and expects TLS in front, so the lab override is a drop-in
(`packaging/systemd/system/hush-relay.service.d/10-lab-public.conf`) that sets
`HUSH_RELAY_ADDR=0.0.0.0:8790`. systemd applies the drop-in's `Environment=` after the
main unit's, so the address override wins. The DB path is the unit's
`StateDirectory=hush-relay` + `HUSH_DATA_DIR=/var/lib/hush-relay` →
`/var/lib/hush-relay/relay/relay.db`.

Verify:

```sh
ss -ltnp | grep 8790                     # 0.0.0.0:8790 hush-relay
curl -sS http://127.0.0.1:8790/health    # 200
curl -sS http://137.184.129.205:8790/health   # 200 from off-box
systemctl restart hush-relay             # comes back active, DB intact
```

## Pointing a client at the VPS relay

The relay URL is resolved in `hush-core::config` with precedence
**flag/env > `client-config.json` > default**, but a **vault stores the relay it was created
with, and that stored value wins at runtime**.

```sh
# New vault against the lab relay (stored in the vault):
hush-cli --dir <vault> init --passphrase <pw> --relay http://137.184.129.205:8790

# Or set the default for new vaults:
hush-cli --dir <vault> config set --relay http://137.184.129.205:8790
hush-cli --dir <vault> config show      # shows relay_url and its source

# Or override at runtime for a daemon:
HUSH_RELAY_URL=http://137.184.129.205:8790 hush-daemon --dir <vault>
```

### Release builds refuse plain HTTP off loopback

`hush-core`'s `HttpPolicy` is `LoopbackOnly` in release builds and `Allow` in debug builds.
A release binary rejects `http://137.184.129.205:8790`:

```
$ hush-cli --dir <vault> config set --relay http://137.184.129.205:8790
Error: relay error: insecure relay URL: use HTTPS except for localhost
```

So a **release** daemon cannot use this plain-HTTP relay; it needs `https://` (terminate TLS
in front — see `docs/ops/relay-https-checklist.md`) or a **debug** build. The lab
cross-machine test therefore runs debug binaries.

## Cross-machine test

`scripts/smoke-relay-lab.sh` runs two **local** daemons against the remote relay and
exchanges mail both ways. It does not start a relay.

```sh
HUSH_RELAY_URL=http://137.184.129.205:8790 ./scripts/smoke-relay-lab.sh
```

Observed 2026-10-08: `relay /health -> 200`; both vaults recorded the remote relay; invite
exchange, a B→A mail and an A→B reply all completed. The VPS relay DB shows the queues the
two peers created (`queues: 4`, `blobs: 1` mid-test), proving the traffic went through the
VPS and not a local relay.

## Removed: outsider-mail notification egress (2026-10-08)

Mail is members-only now, and the outsider drop path was taken out of the relay, the
daemon, and the core crate: the `/drop/{id}` storage routes, the `/open/{id}` pickup page,
and the `POST /drop/{id}/notify` hook are gone, along with `SmtpNotifier`
(`crates/hush-relay/src/notify.rs`) and `HUSH_RELAY_MAILER`. The OpenSMTPD instance above
was its submission server; the box also has no outbound TCP 25 (provider blocks it), which
is one more reason the email-notification idea never worked from here and was not kept.

## Onion service (v3)

Added 2026-10-08. The relay is additionally reachable as a Tor onion service, so a client
routing through Tor never exposes its IP to the relay, and the relay's hosting location is
not published to clients.

- **Onion address:** `ogkf745dfn32pdbvsk6e52uqski7mudadowosi5cbyif4yglhzuqlsad.onion`
- **Package:** `tor` 0.4.9.11 (Ubuntu noble-updates/universe), service `tor.service` (enabled).
- **Config:** `/etc/tor/torrc` gained a `HiddenServiceDir /var/lib/tor/hush-relay/` +
  `HiddenServicePort 80 127.0.0.1:8790` block (full three-hop service, so the VPS location
  stays hidden; no `HiddenServiceSingleHopMode`). `SocksPort 9050` comes from
  `/usr/share/tor/tor-service-defaults-torrc`, bound loopback by the Debian defaults.
- **Key material:** the service's private key lives in `/var/lib/tor/hush-relay/` (mode 700,
  owner `debian-tor`). Back it up if the address should survive a rebuild; deleting it mints
  a new address.
- **Verified 2026-10-08:** `curl --socks5-hostname 127.0.0.1:9050 http://<onion>/health`
  returns 200 through a real onion round trip (~2 s warm, ~4.5 s cold).
- **Redeployed 2026-10-10** with the drops slice (`d81837b`): the Oct 8 binary predated
  the `/drop` routes and answered 404 — a blank page in a browser. After the redeploy,
  onion `GET /drop` returns 200 with the 53837-byte page and the `/drop/{id}`
  PUT/GET/DELETE family round-trips through the onion.
- **Clients:** with `hush-core` as of this change, a release build accepts a plain-HTTP v3
  onion relay base (`host_is_v3_onion` in `normalize_relay_base`), and a client routes the
  whole relay path through Tor with:

  ```sh
  HUSH_RELAY_URL=http://ogkf745dfn32pdbvsk6e52uqski7mudadowosi5cbyif4yglhzuqlsad.onion \
  HUSH_RELAY_SOCKS_PROXY=socks5h://127.0.0.1:9050 hush-daemon --dir <vault>
  ```

  The proxy value is validated strictly (only `socks5`/`socks5h` with a host) and ambient
  proxy environment (`ALL_PROXY` etc.) is deliberately ignored for the relay path.
- **RAM:** the box had ~155 MB available before tor; ~92 MB after. No swap configured. Fine
  for a lab, do not add services to this droplet.
- **The clearnet listener stays up** (`0.0.0.0:8790`, plain HTTP, debug clients only per the
  release HTTP policy). The onion path is additive; nothing moved.

## Not done (flagged, not built)

- **TLS.** The lab is plain HTTP by request. The relay is exposed directly; do not reuse this
  shape anywhere real.
- **Reboot test.** The relay is `enabled` and survives `systemctl restart`; a full reboot was
  not run (the box hosts unrelated services).

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

## Third-party domains land on this box (nginx is a catch-all)

Checked 2026-10-16 because the access log carries referers for hostnames nobody here
configured. The only nginx site is `alhakim` — `server_name _;` with
`listen 80 default_server;` — so **any** hostname whose A record points at
`137.184.129.205` is served the clinic app, and the app builds its redirect from
whatever `Host` it was asked for:

```
$ curl -H "Host: qualifireprotection.com" http://127.0.0.1/
307  →  location: http://qualifireprotection.com/login?next=%2F
```

Five names point here and none are ours, with nothing on the box referencing any of
them (`grep -ril -e qualifire -e hagerbet /etc/nginx /etc/systemd/system /home /etc/cron*`
returns nothing):

| Hostname | Note |
|---|---|
| `qualifireprotection.com`, `www.` | registered 2015-05-07 via eNom, expires 2027-05-07 (RDAP) |
| `hagerbet.net`, `www.`, `feedback.` | betting domain, NS `mysecurecloudhost.com` |

Most likely a recycled DigitalOcean IP: the box's ssh host keys were born
`2026-08-17`, and these records still point at the address their owner held before this
droplet took it. They are **not ours to use** — do not put a certificate on them, the
DNS is not ours to validate. Serving them is harmless while the clinic app is only
reached by IP, and was left as-is by choice on 2026-10-16.

## HTTPS origin: current state (2026-10-16)

The quick tunnel hostnames this document used to name are **all dead** — Cloudflare
reclaimed them again, the same way it did on 2026-10-10. Checked 2026-10-16:

| Hostname | Serves | DNS |
|---|---|---|
| `ban-therapeutic-daughter-organic.trycloudflare.com` | nginx (relay via path ingress) | NXDOMAIN |
| `sources-baghdad-sullivan-olive.trycloudflare.com` | nginx (cricket `CRICKET_PUBLIC_BASE`) | NXDOMAIN |
| `remedy-relief-patricia-main.trycloudflare.com` | relay directly | NXDOMAIN |

The connectors were still `active` with their old hostnames in the log, which is the
trap: a healthy-looking connector is not a working hostname.

**The relay-facing quick tunnel is `cloudflared-hush-quick.service`** —
`cloudflared tunnel --no-autoupdate --url http://127.0.0.1:8790`, `enabled`,
`Restart=always`. It points straight at the relay, so no nginx path ingress is
involved. Restarting it mints a new hostname:

```sh
ssh -i /home/sam/.ssh/alhakim_deploy root@137.184.129.205 \
  "systemctl restart cloudflared-hush-quick.service && sleep 12 && \
   journalctl -u cloudflared-hush-quick.service --no-pager \
   | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1"
```

Verified 2026-10-16 against the then-current hostname
`bolt-heroes-mailed-grass.trycloudflare.com` (now itself dead — it was superseded
by a later restart):

```
/health        200, ssl_verify_result=0 (publicly trusted, so webpki-roots accepts it)
/drop          200, 53837 bytes (the drop page)
/k/<zero key>  404 (no such key)
```

`HUSH_RELAY_URL=https://<hostname> ./scripts/smoke-relay-lab.sh` passes end to end
through it: both vaults record the relay, invite/contact exchange completes, a mail
is queued and received, and the reply comes back.

**`scripts/relay-tunnel-url.sh` reports the live URL** and checks it, so nobody has
to run the SSH command above by hand:

```
$ ./scripts/relay-tunnel-url.sh
relay: https://screenshot-threats-bodies-aug.trycloudflare.com  (/health 200)

$ ./scripts/relay-tunnel-url.sh --refresh      # re-mint after it dies
```

It reads the hostname from the connector's **current pid** on purpose: the journal
still holds the previous process's hostname, and `journalctl -n 200 | tail -1` hands
that stale name back — which is exactly the failure mode this box has produced
three times. It then makes a real `/health` request before printing anything.

A restart mints a new hostname **and kills the old one**: the name in use before a
refresh answered `530` (Cloudflare's "tunnel not found") afterwards. So a URL handed
to someone is good until the next restart of the unit, no longer. A vault stores the
relay it was created against, so a vault pointed at a dead URL needs
`hush-cli --dir <vault> relay set --url <new url>`.

A **release** client accepts this URL — which is the point of the whole exercise:

```
$ hush-cli --dir <vault> init --passphrase pw --relay https://bolt-heroes-mailed-grass.trycloudflare.com
vault ready …  relay: https://bolt-heroes-mailed-grass.trycloudflare.com

$ hush-cli --dir <vault> init --passphrase pw --relay http://137.184.129.205:8790
Error: Relay("insecure relay URL: use HTTPS except for localhost")
```

**Lifetime: still ephemeral.** The hostname changes on every restart of the unit and
Cloudflare gives account-less tunnels no uptime guarantee — it has now reclaimed
three hostnames on this box. Treat any `trycloudflare.com` URL as good for a
session, not for a teammate's vault.

### Durable options

1. **Public Hostname on the `hush` named tunnel** (the right fix). Account
   `75743fae14d6c875d63ca54494668df8`, tunnel `e8c187f1-e516-4371-ab7d-18053dcf5b11`,
   connector `cloudflared-hush.service` already `active` with 4 QUIC connections.
   Dashboard → Zero Trust → Networks → Tunnels → `hush` → Public Hostname → add a
   subdomain on a zone **in that account** → service **HTTP** → `localhost:8790`.
   Point it at the relay, not nginx: every relay route then works verbatim. Until a
   hostname exists the connector logs `No ingress rules were defined … cloudflared
   will return 503 for all incoming HTTP requests`, so the tunnel is up and useless.
   No `cert.pem` and no API token are on the box, so this cannot be done from the
   CLI there — it is a dashboard action.
2. **Proxied DNS record on an existing Cloudflare zone.** `ironline-pal.com` is the
   only Cloudflare-hosted zone visible from here (`nucum.ns.cloudflare.com`), and it
   is not this box: `api.ironline-pal.com` answers 404 for `/health`, `/drop` and
   `/ironlink-health`, so it is the IronLine API elsewhere. A record such as
   `relay.ironline-pal.com` → this origin would need the zone's SSL mode to allow
   Cloudflare→origin plain HTTP, and it exposes the origin address.
3. **TLS on the box.** `qualifireprotection.com` and `hagerbet.net` both resolve
   straight to `137.184.129.205` (neither is on Cloudflare), and nginx already serves
   the relay paths on port 80, so `http://qualifireprotection.com/health` is 200
   today. A certificate plus a `listen 443` block on this nginx would make that a
   stable HTTPS origin with no Cloudflare in the path — see
   `docs/ops/relay-https-checklist.md` for the nginx shape and the path-transparency
   requirements. This changes the listener that also fronts the alhakim app, so do it
   deliberately.

## HTTPS origin (Cloudflare quick tunnel + nginx path ingress)

Added 2026-10-08 for the drop opener, extended 2026-10-10 to carry the whole relay protocol so
a **release** daemon can use the box.

- **Hostname:** `https://ban-therapeutic-daughter-organic.trycloudflare.com`
- **Tunnel:** `cloudflared-quick.service` — `cloudflared tunnel --no-autoupdate --url
  http://127.0.0.1:80`, `Restart=always`. It fronts **nginx**, not the relay.
- **Ingress:** nginx (`/etc/nginx/sites-available/alhakim`, `server_name _`) path-proxies the
  relay's routes to `http://127.0.0.1:8790`; everything else falls through to the alhakim app
  on `127.0.0.1:3000`:

  | nginx location | Relay route | Purpose |
  |---|---|---|
  | `location = /health` | `GET /health` | relay liveness; the client's ping |
  | `location /q/` | `/q/{queue_id}[/{blob_id}\|/presence\|/report]` | queues, blobs, presence |
  | `location /k/` | `/k/{account_id}` | prekey/key directory |
  | `location = /drop` | `GET /drop` | browser drop page (what `drop_page_url()` publishes) |
  | `location /drop/` | `/drop/{id}` | drop container put/get/delete |
  | `location /open/` | — | legacy per-id pickup page; the current relay has no such route and 404s |

  None of those paths exist as routes in the alhakim app (`src/app` has only `(staff)`,
  `(staff-print)`, `api`, `portal`), so the ingress is additive. A backup of the previous file
  is on the box at `/etc/nginx/sites-available/alhakim.bak-20261010`.
- **A release client uses the bare hostname as its relay base:**

  ```sh
  hush-cli --dir <vault> --passphrase <pw> relay set --url https://<hostname>
  ```

  `client-config.json`'s `relay_url` is advisory (new vaults only); the vault's stored relay
  wins at runtime, so the vault must be repointed, not just the file.
- **`HUSH_RELAY_DROP_BASE_URL` is inert.** The current relay build does not read it (no
  `drop_base_url` anywhere in `crates/hush-relay/src`); the drop page is a static `GET /drop`
  and the published URL is composed client-side by `drop_page_url()` in
  `crates/hush-daemon/src/account_worker.rs`. The drop-in
  (`/etc/systemd/system/hush-relay.service.d/20-lab-drop-base.conf`) is kept as history only.
- **Lifetime: ephemeral.** A quick tunnel's hostname is assigned at process start and changes
  on every restart, and Cloudflare gives account-less tunnels no uptime guarantee — it
  reclaimed this origin on 2026-10-10 (the hostname went NXDOMAIN while the connector sat in a
  re-registration loop; see `DEEPSEEKPROBLEMS.MD`). After any restart, re-read the hostname and
  re-point the vaults:

  ```sh
  ssh -i /home/sam/.ssh/alhakim_deploy root@137.184.129.205 \
    "journalctl -u cloudflared-quick.service | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1"
  ```

  A named tunnel with a stable DNS record is the durable fix. The box already runs one
  (`cloudflared.service`, tunnel `cricket-po`) pointing at the same nginx, but its public
  hostname is dashboard-managed and not readable from the box (no `cert.pem`, no API token).

### Durable tunnel `hush` (installed 2026-10-10, awaiting a public hostname)

A second, purpose-built named tunnel exists in a **different** Cloudflare account
(`75743fae…` vs the box's `8bccbccb…`). Its connector runs on the box as its own unit:

- **Unit:** `cloudflared-hush.service` — `cloudflared --no-autoupdate --config
  /etc/cloudflared/hush.yml tunnel run --token-file /etc/cloudflared/hush.token`
- **Why its own unit:** `cloudflared service install <token>` writes
  `/etc/systemd/system/cloudflared.service` and `/etc/cloudflared/config.yml`, both of which
  already belong to `cricket-po`. Installing the new tunnel the documented way would have
  overwritten them and moved the alhakim medical app, the cricket PWA, and the IronLine MCP onto
  the new tunnel. The explicit `--config` also keeps the default `config.yml` out of this process.
- **State:** `active`, `enabled`, 4 QUIC connections registered; `cricket-po` untouched.
- **Blocked on:** a public hostname. The connector logs `No ingress rules were defined in provided
  config (if any) nor from the cli, cloudflared will return 503 for all incoming HTTP requests`,
  so the tunnel has no route yet. In the dashboard: Zero Trust → Networks → Tunnels → `hush` →
  Public Hostname → add subdomain + zone, service **HTTP → `localhost:8790`**. Pointing it
  straight at the relay (not nginx) is the right target: every relay route (`/health`, `/q/`,
  `/k/`, `/drop`) then works verbatim with no path ingress. This needs a domain/zone in that
  account; if it has none, the tunnel cannot get a hostname.
- **Once it has a hostname:** repoint the vaults at `https://<hostname>` and the ephemeral quick
  tunnel above (and its `cloudflared-quick.service`) can be retired.

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

- **TLS.** The relay's own listener is still plain HTTP by request, exposed directly on
  `0.0.0.0:8790`; do not reuse this shape anywhere real. A release client can still use the box
  through the Cloudflare HTTPS origin above — that is path ingress, not a TLS terminator in
  front of the relay.
- **Reboot test.** The relay is `enabled` and survives `systemctl restart`; a full reboot was
  not run (the box hosts unrelated services).

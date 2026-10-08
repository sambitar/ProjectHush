# Lab VPS relay + SMTP (137.184.129.205)

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
| SMTP submission | `opensmtpd.service` (enabled) | `127.0.0.1:25`, `[::1]:25` | maildirs under recipient homes |

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

## SMTP submission server (OpenSMTPD)

Installed for the daemon's outsider-mail notification egress. `opensmtpd` (7.4.0), config at
`/etc/smtpd.conf`, listens **loopback only** (`127.0.0.1:25`). It is not bound publicly on
purpose: this box has a public IP and a public submission port would be an open relay.

Config (functionally the Ubuntu default, with the lab intent recorded in comments):

```
listen on localhost
table aliases file:/etc/aliases
action "local" maildir alias <aliases>
action "relay" relay
match for local action "local"
match from local for any action "relay"
```

Verified send (submission → local delivery):

```
$ python3 -c 'import smtplib; ...'      # send to hushmail@localhost via 127.0.0.1:25
# journalctl -u opensmtpd:
mda delivery ... to=<hushmail@localhost> user=hushmail result=Ok stat=Delivered
# file present: /home/hushmail/Maildir/new/...
```

`root` cannot be a recipient (OpenSMTPD refuses: `not allowed to deliver to: root`), so a
dedicated unprivileged `hushmail` user owns the verification mailbox.

### Gap: the daemon cannot use this yet

The daemon has **no SMTP client**. `hush-core/src/mailer.rs` ships only `NullMailer` and
`FileMailer`, and `build_mailer` in `hush-daemon/src/main.rs` maps only `file:<dir>` —
everything else, including `smtp:host`, becomes `NullMailer`. So there are no SMTP settings
to wire today; the notification egress is a no-op unless `HUSH_MAILER=file:<dir>` is set.
Implementing `HUSH_MAILER=smtp://<host>:<port>` is a follow-up in `hush-core`/`hush-daemon`.
See `DEEPSEEKPROBLEMS.MD` (`[OPEN] The daemon has no SMTP client`).

### Gap: no external egress from this droplet

Even once the client exists, the box cannot send internet mail: the provider blocks outbound
TCP 25 (verified: MTA `Connection failed: Network is unreachable` to the Gmail MX; 443 works).
Real notification delivery needs a smarthost/provider relay or a DO egress exception.

## Not done (flagged, not built)

- **Inbound `drop@` mail ingress.** Out of scope; it is still a pending design task. No
  `drop@` alias, no MTA ingress hook, nothing added for it.
- **TLS.** The lab is plain HTTP by request. The relay is exposed directly; do not reuse this
  shape anywhere real.
- **Reboot test.** The relay is `enabled` and survives `systemctl restart`; a full reboot was
  not run (the box hosts unrelated services).

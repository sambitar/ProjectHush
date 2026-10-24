# Voice STUN checklist (Task 17, Phase 2)

Use when standing up the **self-hosted STUN** endpoint that member calls use for
NAT discovery. STUN reveals a client's IP+port to the server but does **not**
link the two parties, so it adds no social-graph signal. This is the only new
server voice needs: media is direct peer-to-peer and no Hush server carries it.

**Related:** [`docs/tasks/member-calls.md`](../tasks/member-calls.md) Section 13,
[`docs/ops/relay-https-checklist.md`](relay-https-checklist.md) (relay + TLS).

**Deployed:** `hushrelay.space:3478` (stun:hushrelay.space:3478), installed on the
`hush-relay-prod` VPS on 2026-10-24 as coturn. It serves **both STUN and TURN**
(since 2026-10-24): STUN answers Binding requests, and TURN allocates relays for
peers a symmetric NAT or client-isolating firewall keeps out of reach of
peer-to-peer UDP. Verified: a mapped reply and a relay allocation both come back
from `37.27.150.250`. This is the shipped default in `hush-core`
(`config::DEFAULT_STUN_URL` and `config::resolve_turn_config`), so the phone
media path has discovery and firewall traversal without any operator setup.

## Scope

- **In scope:** a coturn endpoint on the relay VPS (or a sibling host) serving
  STUN and TURN. TURN relays media only for the peers that cannot connect
  directly; it sees call *metadata* (who, when, volume) but not the audio,
  which stays end-to-end encrypted (DTLS-SRTP).
- Do not fold STUN into the `hush-relay` binary. The relay stays a dumb
  store-and-forward; STUN is a separate process.

## Topology

| Role | Host | Port | TLS |
|---|---|---|---|
| Relay | `relay.example.com` | 443 (Caddy/nginx → 127.0.0.1:8787) | yes |
| **STUN** | `stun.example.com` | 3478/udp (primary), 3478/tcp, 5349/tcp | TCP/TLS optional |

Publish A and AAAA records for `stun.example.com` (dual-stack).

## 1. Install

Prefer **coturn in STUN-only mode** (mature, packaged). A pinned minimal
STUN-only responder is acceptable if coturn's attack surface is judged too large.

```sh
apt-get install -y coturn
# Debian/Ubuntu: enable the service (default is disabled).
sed -i 's/^#TURNSERVER_ENABLED=1/TURNSERVER_ENABLED=1/' /etc/default/coturn
```

## 2. Configure STUN-only

`/etc/turnserver.conf` — no realm, no credentials, no TURN allocation:

```conf
listening-port=3478
tls-listening-port=5349
fingerprint
no-cli
no-multicast-peers
no-loopback-peers
# No lt-cred-mech / no user= lines: TURN allocations are refused,
# STUN Binding requests are answered.
# Do not retain client IPs; keep logging minimal.
log-file=stdout
simple-log
```

**Do not** add `realm=`, `lt-cred-mech`, `user=`, or `use-auth-secret`. With no
authentication mechanism configured, coturn serves STUN and refuses TURN.

## 3. Firewall

```sh
ufw allow 3478/udp
ufw allow 3478/tcp   # only if the TCP fallback is enabled
ufw allow 5349/tcp   # only if the TLS fallback is enabled
```

STUN is a DDoS **amplification/reflection** vector: keep it to the ports above,
rate-limit at the host or in coturn, and never expose a TURN relay.

## 4. DNS and reachability

```sh
dig +short stun.example.com A
dig +short stun.example.com AAAA
ss -lunp | grep 3478          # must be bound to the intended interface
```

## 5. Verify

```sh
# coturn ships turnutils_stunclient.
turnutils_stunclient stun.example.com

# Expected: a Binding response with a mapped (srflx) address, no TURN allocation.
```

Then confirm from a client machine that hole punching succeeds and that the
relay database holds no media or call plaintext:

```sh
./scripts/run-two-ui.sh
strings /var/lib/hush-relay/relay/relay.db | grep -c 'call' || true
```

## 6. Client configuration

STUN is part of the single client config surface
([`crates/hush-core/src/config.rs`](../../crates/hush-core/src/config.rs)):

- `stun_url` (`crates/hush-core/src/config.rs::DEFAULT_STUN_URL`) defaults to
  our own endpoint, `stun:hushrelay.space:3478`. It is **not** a third-party
  service, and it learns only an IP+port, never a party pair.
- `HUSH_STUN_URL` overrides it, then `stun_url` in `client-config.json`.
- No TURN in v1 (BYO only). A peer behind symmetric NAT or CGNAT fails with a
  visible error rather than being routed through a first-party relay.

## 7. Abuse and privacy

- **Do not log per-request client IPs.** STUN sees IP+port and no party linkage;
  logs would recreate the metadata the relay is built to avoid.
- Rate-limit and firewall the STUN port.
- No credentials exist for STUN-only, so there is no secret to leak.

## Known gaps

- TLS/TCP fallback for UDP-blocked networks is listed but not yet exercised.
- A second STUN host for redundancy is a later, cheap add — not v1.
- coturn is a large binary; if its surface is judged unacceptable, swap in a
  pinned minimal STUN-only responder without changing the client config.

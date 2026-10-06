# Relay HTTPS checklist (Task 5c.4)

Use when pointing dogfood or production clients at a VPS relay (not `127.0.0.1`).
The relay itself speaks plain HTTP and must never be exposed directly: it is a
dumb store-and-forward that will happily accept plaintext HTTP from any client
that reaches it.

## Client policy (enforced in code, verified)

`normalize_relay_base` in `hush-core` refuses a non-loopback `http://` relay URL:

| Build | Non-loopback `http://` | Loopback `http://` | `https://` |
|-------|------------------------|--------------------|------------|
| release | refused | allowed | allowed |
| debug | allowed (dogfood) | allowed | allowed |

Loopback means `localhost` (any case) or any address in `127.0.0.0/8` / `::1`,
including the bracketed form `http://[::1]:8787`. Names that merely start with a
loopback literal (`127.0.0.1.example.com`, `localhost.example.com`) are refused.

`scripts/smoke-relay-https-policy.sh` checks all of this against the real release
and debug binaries.

### Certificate constraint: a publicly trusted CA is required

The client is built with `rustls-tls` and **bundles Mozilla's root store**
(`webpki-roots`). It does not read the system trust store, and `SSL_CERT_FILE`
has no effect.

- A Let's Encrypt or other publicly trusted certificate works with no client
  configuration at all.
- A **self-signed or internal-CA certificate will not work**, and there is no
  client-side escape hatch. Plan for a real certificate.

This is deliberate: there is no system CA store on the client for an attacker to
write to. Do not add an "insecure" switch to work around it.

## Server

### 1. Install the relay

```sh
cargo build --release --locked -p hush-relay
install -Dm755 target/release/hush-relay /usr/local/bin/hush-relay
useradd --system --no-create-home --shell /usr/sbin/nologin hush-relay
install -Dm644 packaging/systemd/system/hush-relay.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now hush-relay
```

The unit binds `127.0.0.1:8787` and keeps its database at
`/var/lib/hush-relay/relay/relay.db`, both configurable via `HUSH_RELAY_ADDR` and
`HUSH_DATA_DIR`. Confirm it is loopback-only before continuing:

```sh
ss -ltnp | grep 8787          # must show 127.0.0.1:8787, never 0.0.0.0:8787
curl -sS http://127.0.0.1:8787/health
```

### 2. Terminate TLS

Both examples below forward paths **verbatim**. The client builds request URLs as
`{relay_base}/q/{queue_id}`, so any prefix stripping or path rewriting breaks it.

**Caddy** (handles issuance and renewal automatically):

```caddyfile
relay.example.com {
	reverse_proxy 127.0.0.1:8787
	header Strict-Transport-Security "max-age=31536000; includeSubDomains"
	# Access logs record request URIs, which carry queue IDs. Keep them off.
	log {
		output discard
	}
}
```

**nginx** with certbot:

```nginx
server {
	listen 80;
	server_name relay.example.com;
	return 301 https://$host$request_uri;
}

server {
	listen 443 ssl;
	http2 on;
	server_name relay.example.com;

	ssl_certificate     /etc/letsencrypt/live/relay.example.com/fullchain.pem;
	ssl_certificate_key /etc/letsencrypt/live/relay.example.com/privkey.pem;
	ssl_protocols TLSv1.2 TLSv1.3;
	add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;

	# Default is 1m, which is exactly max_blob_bytes: a maximum-size blob plus
	# request headers would be rejected with 413. Keep this above the cap.
	client_max_body_size 2m;

	location / {
		proxy_pass http://127.0.0.1:8787;
		proxy_http_version 1.1;
		proxy_set_header Host $host;
		proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
		proxy_set_header X-Forwarded-Proto $scheme;
		# URIs contain queue IDs; do not write them to disk.
		access_log off;
	}
}
```

WebSockets and streaming are not used — every route is a short request/response.

### 3. Firewall

```sh
ufw default deny incoming
ufw allow 22/tcp
ufw allow 80/tcp     # ACME + redirect
ufw allow 443/tcp
ufw enable
```

Port 8787 must not be reachable from outside. Verify from another host:

```sh
curl -sS --max-time 5 http://203.0.113.1:8787/health   # must fail or time out
```

### 4. Rate limits, TTL, and blob size

These are compiled into `RelayConfig::default()` and are **not** configurable by
environment variable today:

| Setting | Default | Effect |
|---------|---------|--------|
| `max_blob_bytes` | 1 MiB | larger PUTs rejected |
| `blob_ttl` | 7 days | undelivered blobs expire |
| `rate_limit_max` / `rate_limit_window` | 60 per 60s | per queue, on PUT |

Confirm the limits are acceptable before exposing the relay, and open a follow-up
task if you need different values — changing them means editing
`crates/hush-relay/src/config.rs` and rebuilding.

`POST /q/{queue_id}/report` burns the queue: every later operation on it returns
`403 queue burned`. That is the report-burn path from `PROTOCOL.md`, not an error.

### 5. Logging

Do not log request bodies, and do not log request URIs — they carry queue IDs,
which is exactly the metadata the relay is designed not to retain. The relay
itself logs nothing per request; only the reverse proxy could leak this.

## Client

1. Create or migrate the vault with an `https://` relay URL:
   `hush-cli --dir <vault> init --passphrase … --relay https://relay.example.com`
   or `hush-cli --dir <vault> config set --relay https://relay.example.com`.
2. A vault keeps the relay it was created with; `client-config.json` only decides
   the default for *new* vaults.
3. Confirm reachability: `curl -sS https://relay.example.com/health`.

## Verify

```sh
# Release binary refuses plaintext off-loopback, accepts HTTPS and loopback.
./scripts/smoke-relay-https-policy.sh

# Relay is reachable over TLS and the certificate chain is publicly trusted.
curl -sS -o /dev/null -w '%{http_code} %{ssl_verify_result}\n' https://relay.example.com/health

# No plaintext message content in the relay database (unchanged from 5c.1).
strings /var/lib/hush-relay/relay/relay.db | grep -c 'hello' || true

# Two peers, end to end.
./scripts/run-two-ui.sh
```

Expected: the policy script passes all rows; `ssl_verify_result` is `0`; the
`strings` scan finds no message content; both peers exchange text and files.

## Known gaps

- The reverse-proxy configs above were validated for **path transparency** against
  a local proxy hop (all routes: `GET /health`, `PUT|GET /q/{id}`,
  `GET|DELETE /q/{id}/{blob}`, `POST /q/{id}/report`). TLS termination itself has
  not been exercised against a real certificate on a VPS.
- Queue IDs are unguessable but the relay does not rate-limit by source address,
  only per queue. Revisit if the relay is abused.
- Sealed-sender review (5c.1) is still recommended before calling the transport
  done.

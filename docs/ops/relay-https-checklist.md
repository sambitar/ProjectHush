# Relay HTTPS checklist (Task 5c.4)

Use when pointing dogfood or production clients at a VPS relay (not `127.0.0.1`).

## Release builds

- `hush-core` **refuses** `http://` relay URLs unless the host is loopback (`127.0.0.1`, `localhost`, `::1`). Use `https://your-relay.example` in account relay settings.
- Debug builds allow any `http://` URL for local `cargo test` and `./scripts/run-two-ui.sh`.

## Server

1. Terminate TLS at Caddy, nginx, or similar; proxy to `hush-relay` on a private port.
2. Use a real certificate (Let’s Encrypt or your CA).
3. Keep `relay.db` on disk only on the VPS; restrict file permissions.
4. Rate limits and TTL are in `hush-relay` config — verify defaults before exposing publicly.
5. Do not log request bodies.

## Client

1. Set relay URL to `https://…` when creating or migrating vaults (`hush-cli init --relay`, or account settings when exposed).
2. Confirm connectivity with `curl -sS https://your-relay/health` (or relay health route).

## Verify

- Release binary: `http://203.0.113.1:8787` fails at account open; `https://relay.example` succeeds.
- `strings` / integration tests on relay DB still show no plaintext message content (unchanged from 5c.1).

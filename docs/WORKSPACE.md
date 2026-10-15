# Planned Rust workspace (do not scaffold until implementation is approved)

One repo. Develop on Linux. No UI in the core crate.

```
hush/                         # working name; product name later
  crates/
    hush-core/                # keys, vault, Signal sessions, queue client, cloud replica I/O
    hush-relay/               # HTTP relay binary (dumb)
    hush-daemon/              # local API over the core (unix socket or localhost)
    hush-cli/                 # optional dogfood without UI
    hush-atproto/             # the public lane: AT Protocol / Standard.site (Task 11/22)
    hush-audio/               # Opus capture and playback for calls
    hush-wallet/              # BTC/ETH wallet (Task 8)
  apps/
    hush-tauri/               # Tauri shell; UI talks only to hush-daemon
  docs/                       # already started at repo root docs/
```

## crate rules

- **hush-core:** no Tauri, no HTTP server, no HTML. Tested with `cargo test`. Vault: SQLite (SQLCipher or app-level encryption) + content-addressed encrypted blobs.
- **hush-relay:** no user names. SQLite or on-disk blob store + TTL job. Rate limits per queue.
- **hush-daemon:** the only process that unlocks the vault. UI never holds raw keys.
- **hush-tauri:** local assets only. Updater public key compiled in.

## packaging (when we ship)

- Linux: AppImage and/or Flatpak, signed.
- Android: later, same Tauri project.
- Relay: one binary + Caddy/nginx TLS on a small VPS.

## libraries (intent)

- `libsignal` (or the current supported Rust Signal crates) for ratchets
- `sqlx` or `rusqlite` for local DB
- `axum` or `hyper` for relay
- `reqwest` for relay client
- rustls

Do not add a custom ratchet crate.

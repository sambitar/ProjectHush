# Distro readiness (guidance only)

**Do not build the OS image from this document.** This records how the **application** should evolve so it can later ship inside a hardened, image-based Linux distro (persistent live USB, then immutable measured boot). The OS is a distribution layer around Hush; the app must not require a rewrite to move into it.

**Related:** [`docs/plans/2026-10-04-v1-milestones.md`](plans/2026-10-04-v1-milestones.md) (distro checklist + per-task notes), [`docs/tasks/distro-readiness-gaps.md`](tasks/distro-readiness-gaps.md) (known codebase violations as tracked tasks).

---

## Long-term vision

**Not a new kernel.** A hardened, image-based distro (Debian live-build, mkosi, NixOS, or Fedora bootc — **TBD**) that preinstalls this app.

| Stage | Target |
|-------|--------|
| 1 | App on a normal distro (**current work**) |
| 2 | Hardened packaging (Flatpak/deb), sandbox profiles, external audit |
| 3 | **Persistent live USB** (first OS release): read-only OS in RAM; **LUKS2** persistent partition for Hush vault, config, identity only (Tails-style) |
| 4 | Immutable, signed, measured-boot image with atomic updates |
| 5 | Possible hardware partnerships |

---

## Rules for the app now

When implementing any milestone task, follow these rules. If a task conflicts, **stop and flag** before proceeding.

### State and storage

- **One data root.** All persistent state (vault, config, keys, identity, relay settings, lease state, outsider contacts) under a single configurable directory, e.g. `HUSH_DATA_DIR`, defaulting to an XDG path. No scattered state in `~/.cache`, `/tmp`, `/var`, or ad hoc dotfiles.
- **One client config surface.** Relay endpoint and daemon address resolve in `hush-core::config` from `{data_root}/client-config.json`, overridable by `HUSH_RELAY_URL` / `HUSH_DAEMON_ADDR` / `HUSH_DAEMON_SOCKET` and per-invocation flags (flag/env > file > default). Binaries and scripts must not re-implement this. A vault keeps the relay it was created with; `client-config.json` decides the default for new vaults. `hush-cli config show|set` reads and writes it; the daemon reports the active relay in `/api/status`.
- **Socket-first local API.** The daemon binds `{runtime_dir}/hush-daemon.sock` (`0600`) by default; TCP is opt-in and never on in release unless explicitly configured. The UI never opens a socket itself — the Tauri shell proxies API calls from Rust (`hush_api`). Multiple daemons on one machine need distinct `--socket` paths; a daemon refuses to unlink a socket that still accepts connections.
- **Hardened by default.** The daemon is the only process holding plaintext keys, so it disables core dumps (`PR_SET_DUMPABLE=0`, `RLIMIT_CORE=0`) and locks its address space with `mlockall` before reading a passphrase; `MCL_FUTURE` is requested only when the memlock limit leaves real headroom, so a tight limit degrades to locking current pages instead of making later allocations fail. The systemd unit carries the sandbox (`ProtectSystem=strict`, `ProtectHome=read-only`, `PrivateDevices`, `SystemCallFilter=@system-service`, empty `CapabilityBoundingSet=`, `MemoryDenyWriteExecute`) plus `LimitMEMLOCK=infinity` and `MemorySwapMax=0`. Anything the unit sandboxes must stay writable through `ReadWritePaths=` — the data root, and any removable mount holding the vault. `scripts/smoke-daemon-hardening.sh` checks the process-level guarantees.
- **Vault path is configurable** and must work when pointed at a **mount point** (LUKS volume that appears/disappears).
- **Volatile data** in a configurable runtime dir (`XDG_RUNTIME_DIR` or `HUSH_RUNTIME_DIR`, often tmpfs): sockets, locks, temp files — **not** in the data root.
- **No plaintext on disk, ever.** No plaintext temp files, swap-friendly secret buffers, crash dumps, or logs with content/keys. Use `mlock` / `zeroize` for key material where practical. Disable core dumps for the daemon (`PR_SET_DUMPABLE`, `RLIMIT_CORE=0`).
- **Daemon tolerates vault disappearance** (USB pulled, mount lost): detect, auto-lock, wipe in-memory keys, clear UI state, recover cleanly on remount. **Test this.**
- **Auto-lock on storage removal and on suspend / lid close.**

### Daemon and process model

- Run as a **systemd** service (user unit now, system unit later). Ship unit file(s) in-repo with hardening: `NoNewPrivileges`, `ProtectSystem`, `ProtectHome`, `PrivateTmp`, `PrivateDevices`, `RestrictAddressFamilies`, `SystemCallFilter`, `CapabilityBoundingSet=`, `MemoryDenyWriteExecute` where compatible.
- Test early under **seccomp / Landlock / AppArmor**. Document required paths and syscalls. Minimal permissions is a feature.
- Local API over **Unix domain socket** in the runtime dir, strict permissions. **No TCP listener by default.**
- CLI and UI are thin clients of the daemon API. UI must remain replaceable.

### UI

- No dependency on a specific desktop environment.
- Tauri / WebKitGTK is a large surface — keep UI behind the daemon API for swap/sandbox later.
- No remote assets (existing rule). No auto-open external links; no thumbnails/previews that trigger network or heavy parsers on untrusted files.
- Treat received files as **untrusted** — no auto-render or auto-open.

### Network

- Outbound traffic through **one relay client** with a configurable endpoint list (OS can force firewall or Tor later).
- **HTTPS required** for non-loopback relay URLs in release builds (Task 5c.4).
- No telemetry, analytics, or update pings except the **signed updater** path.
- **The Paper DataChannel is a second, direct egress path** (added 2026-10-12). A conclave's shared document opens a pure-Rust WebRTC `DataChannel` (SCTP over UDP) straight to the paper's hub, so a hardened sandbox must allow UDP/SCTP egress for it and must not assume the relay is the only socket the daemon holds. STUN is configured in exactly one place (`ClientConfig::stun_url`, the `client-config.json` surface — no second config system) and defaults to our own host, `stun:hushrelay.space:3478`, never a third-party service; it sees an IP+port, not who is talking to whom. There is **no TURN**: a peer behind symmetric NAT falls back to relay cadence rather than opening a relay-side data path. **No new devices:** the channel is a userspace UDP socket, not a capture or render device, so `PrivateDevices` stays.
- **The Zines bulletin adds no client egress path** (added 2026-10-13). Publishing a post or a comment and draining the lane both go through the same relay client and the same endpoint as everything else — no second socket, no DHT, no peer-to-peer path, and no new device. What it adds is **relay-side storage**: a flat append-only log of opaque blobs (`PUT /b/{queue_id}`, `GET /b/{queue_id}?since={seq}`), capped at 256 KiB per blob with a 30-day TTL and a per-member publish rate limit. Because a holder re-posts what it holds about once a week to keep it inside that TTL, the relay's steady state is bounded by the corpus the members publish rather than by message traffic — and a re-post keeps its original `seq`, so refreshing the window does not multiply rows. This is the first table whose growth is not bounded by one member's own sending, so an operator's disk budget has to account for it; text-only bodies and the 256 KiB cap are what keep it small.
- **The public lane is a second, HTTPS egress path** (added 2026-10-14). Publishing a zine and refreshing the reading list open ATProto XRPC requests to the member's own PDS and to every followed publication's PDS, through the **same proxy setting the relay uses** (direct when Tor is off, the SOCKS port when it is on) — one switch, not a second. The path is **lazy**: no ATProto request is made until Connect, Refresh or Publish is pressed, and opening a cached post makes no request at all. A sandbox must therefore allow HTTPS egress beyond the relay endpoint when the member uses this feature, and must not assume the relay is the only TLS peer. The credential is an **app password exchanged for a session** (access + refresh JWT) in its own encrypted sidecar (`atproto-connection.enc`), revocable by the member at their PDS without Hush's help; nothing is minted and no PDS is operated by Hush. **No new devices.**

### Updates and signing

- Task 7 signing/verification as a **general reusable layer** (library + CLI), not Tauri-only. OS image updater reuses it.
- Pin signing keys; support rotation; refuse unsigned or downgraded artifacts.
- **Reproducible builds:** `rust-toolchain.toml`, committed `Cargo.lock`, `cargo build --release --locked`, no build-time network or embedded timestamps. Document release reproduction.

### Packaging

- Clean `cargo build --release --locked` per binary; no runtime downloads.
- Minimal audited deps (`cargo-deny`, `cargo-audit` in CI). The Paper feature (2026-10-12) added two: **`yrs`** (CRDT, pure computation, no network or I/O) and **`webrtc-rs`** (pure-Rust WebRTC — userspace UDP sockets only, no native `libwebrtc` and no new device access). Both belong in the `cargo-deny` / `cargo-audit` scope, and `webrtc-rs`'s UDP use is the SCTP egress the sandbox must permit above.
- Flatpak/deb later: no hard-coded absolute paths; config via env + config file under data root.

### Keys, TPM, and Confidential (Task 10)

- Abstract unlock behind **`KeyProvider`**: passphrase, FIDO2/YubiKey, TPM-sealed, lease-gated — vault and Confidential must not hardcode one mechanism.
- Vault may **travel on persistent live USB**; machine-bound TPM may not apply. Confidential lease may bind to **machine TPM** or **stick FIDO2/secure element** (configurable).
- Lease server stays a **separate trust domain** from `hush-relay`.
- Recovery (Task 6) is **mandatory**; encrypted backup to a second device should be a **first-run** option (flash wear, dead stick).

### Security hygiene (OS + app)

- Document leak paths: thumbnailers, indexers, backups, cloud sync, swap/hibernation, clipboard, screenshots — what the **app** closes vs what the **OS** must close.
- Persistent vault mount: `nosuid,nodev,noexec`. No executables or setuid inside data root.
- Treat vault file contents as **untrusted** when parsing.

---

## Documentation to produce (tasks, not now)

| Doc | Purpose |
|-----|---------|
| This file | Living guidance |
| Filesystem map | Every path read/written and why |
| Daemon permissions / syscall inventory | Sandbox + systemd |
| Leak paths | App vs OS responsibilities |
| Threat model addendum | Host-hardware trust, USB loss/clone, offline brute force, persistent area as malware foothold |

---

## Open decisions (do not decide now)

- Amnesic vs persistent mode (persistent likely default)
- Base distro and image tooling (live-build, mkosi, NixOS, bootc)
- TPM binding scope for portable USB
- Separate vault stick
- Audit timing and who audits

---

## Acceptance criteria — “distro-ready” (end of app phase)

- [ ] All state under one configurable **data root** and one **runtime** dir.
- [ ] Daemon runs under hardened **systemd** unit + documented sandbox profile.
- [ ] Vault storage removal → auto-lock, key wipe, clean remount recovery.
- [ ] No plaintext on disk; no secrets in logs (verified by test).
- [ ] Reproducible, signed builds with reusable verification layer.
- [ ] **`KeyProvider`** abstraction in place.
- [ ] Recovery and encrypted backup work.
- [ ] UI is a replaceable daemon API client.

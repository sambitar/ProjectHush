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

### Updates and signing

- Task 7 signing/verification as a **general reusable layer** (library + CLI), not Tauri-only. OS image updater reuses it.
- Pin signing keys; support rotation; refuse unsigned or downgraded artifacts.
- **Reproducible builds:** `rust-toolchain.toml`, committed `Cargo.lock`, `cargo build --release --locked`, no build-time network or embedded timestamps. Document release reproduction.

### Packaging

- Clean `cargo build --release --locked` per binary; no runtime downloads.
- Minimal audited deps (`cargo-deny`, `cargo-audit` in CI).
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

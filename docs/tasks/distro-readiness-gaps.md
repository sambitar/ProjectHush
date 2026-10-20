# Distro-readiness gaps (tracked tasks)

**Source:** [`docs/DISTRO_READINESS.md`](../DISTRO_READINESS.md). Do **not** fix silently during unrelated work — either address under the listed milestone or open a dedicated slice.

| ID | Violation | Location / notes | Suggested owner task |
|----|-----------|------------------|----------------------|
| DR-01 | No unified **`HUSH_DATA_DIR`**; vault via `--dir` only; relay DB often `relay.db` in cwd | `hush-core::paths`, daemon/cli/relay | Task 5 — **done** (`resolve_data_dir`, relay under data root) |
| DR-02 | Daemon listens on **TCP** `127.0.0.1:9876` by default, not Unix socket in runtime dir | `crates/hush-daemon/src/main.rs` | Task 5 — **done** (socket `{runtime_dir}/hush-daemon.sock` at `0600` by default; TCP only via `--addr` / `HUSH_DAEMON_ADDR` / config; Tauri proxies over the socket) |
| DR-03 | **`CorsLayer::permissive()`** on daemon HTTP API | `hush-daemon/src/main.rs` | Task 5 — **done** (loopback + Tauri origins only) |
| DR-04 | **Plaintext `inbox/*`** after file recv | `hush-daemon` `to_dto` | Task 5c.2 — **done** |
| DR-05 | UI state in **`localStorage`** (theme, avatar photos) outside data root | `ui-prefs.json` + `/api/ui-prefs` | Task 5 — **done** |
| DR-06 | No **systemd** unit or hardening directives in repo | `packaging/systemd/user/` | Task 5 — **done** (user unit with `ProtectSystem=strict`, `ProtectHome=read-only` + `ReadWritePaths` for the data root, `PrivateTmp`/`PrivateDevices`, `SystemCallFilter=@system-service`, empty `CapabilityBoundingSet=`, `MemoryDenyWriteExecute`, `RestrictAddressFamilies`; verified by running the daemon under the directives) |
| DR-07 | No **core dump disable** / `mlock` for daemon secrets | `hush-daemon/security.rs` | Task 5 — **done** (`PR_SET_DUMPABLE=0`, `RLIMIT_CORE=0`, `mlockall` with `LimitMEMLOCK=infinity` + `MemorySwapMax=0` in the unit; `scripts/smoke-daemon-hardening.sh`) |
| DR-08 | No handling for **vault mount loss** (USB unplug) | vault path watch | Task 5 — **done** (auto-lock when path missing) |
| DR-09 | No **auto-lock on suspend** / lid close | `suspend_lock.rs` (Linux logind) + `auto_lock_minutes` in ui-prefs | Task 5 — **done** (lid close ≈ suspend on most laptops) |
| DR-10 | No **`rust-toolchain.toml`**; no **`cargo-deny` / `cargo-audit`** in CI | repo root | Task 7 |
| DR-11 | Signing (Task 7) not yet a **standalone library + CLI** | `crates/hush-verify` | Task 7 — **library landed 2026-10-19** (`hush-verify`: license format, `sign_license`/`verify_license`/`check_encoded_license`, 14 tests). The **CLI** half and the release-artifact verifier are still open |
| DR-12 | No **`KeyProvider`** trait; passphrase hardcoded in unlock path | `hush-core`, `hush-daemon` | Task 6 + 10 |
| DR-13 | **Cleartext `from`** in relay wire envelope | `hush-core` `WireEnvelope` | Task 5c.1 — **done** |
| DR-14 | Relay URL not centralized in single client config surface for Tor/firewall policy | `RelayClient`, env per script | Task 2 + 5 — **done** (`hush-core::config`, `client-config.json` in the data root, precedence flag/env > file > default, `hush-cli config show/set`; daemon reports the active relay in `/api/status`) |
| DR-15 | Received files may be opened/saved to **user Downloads** via browser (plaintext export) | Tauri uses native open/save | Task 5b.1 — **mitigated** (export is explicit user action) |
| DR-16 | **Recovery / first-run backup** not implemented | — | Task 6 — **done** |
| DR-17 | Chat history only in **UI memory** (not in data root encrypted store) | `chat.enc` + API | Task 5c.3 — **done** |
| DR-18 | **Public lane** adds an outbound path: ATProto XRPC over HTTPS to the member's PDS and every followed PDS, through the relay's own proxy setting (native zines use only the existing relay bulletin) | Task **11** / **22** (public lane) | **Implemented 2026-10-14** — egress is lazy (Connect / Refresh / Publish only) and reuses the relay's Tor switch; record it in the network inventory |
| DR-19 | **Public content is permanent** — a published document cannot be un-published from Hush | public lane | **Mitigated** — explicit per-post confirmation, plain disclosure in the dialog, the record lives in the member's own account where it can be edited or deleted |

**Process:** Before implementing Tasks **1, 2, 5, 5c, 6, 7, 10**, read the distro checklist in the milestones file and check this table for conflicts.

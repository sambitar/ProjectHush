# Windows Release Implementation Plan

**Goal:** Ship a signed, self-updating Windows installer (NSIS) with full feature parity except the
self-custody wallet, by giving the app and the daemon an authenticated loopback-TCP transport that
replaces the Unix socket on Windows.

**Architecture:** The daemon and the Tauri shell are separate processes joined by one local
transport. Today that transport is a `0600` Unix socket and the socket's file permissions are the
only access control. Windows has no `tokio::net::UnixListener`, so the transport becomes loopback
TCP, and the access control the socket used to provide is replaced by a **per-launch bearer token**:
the shell mints a random token, passes it to the daemon it spawns, and injects it into the webview
so the UI's `fetch` carries it. The daemon refuses to bind TCP without a token, and the token
middleware is layered on the TCP service only — the Unix listener stays exactly as it is, so every
existing `unix://` run script keeps working untouched. Because the TCP listener has no socket, the
daemon's existing `wallet_routes = state.socket.is_some()` gate already disables the wallet on
Windows, which is the agreed scope.

**Tech Stack:** Rust (axum 0.8, tokio 1, hyper 1 / hyper-util 0.1), Tauri 2, `fs4` for a
cross-platform file lock, `tauri-action` NSIS bundling, the existing `minisign` updater key.

**Spec:** This conversation (transport = loopback TCP + per-launch token; scope = full parity except
the wallet); `RELEASING.md` § "Platform notes" (the current "Windows: not built" note);
`docs/DISTRO_READINESS.md` § "Rules for the app now" (socket-first local API, one config surface,
fail-closed posture); `crates/hush-daemon/src/main.rs` (the `api_router` wallet gate and the two
`axum::serve` calls); `apps/hush-tauri/src-tauri/src/daemon_supervisor.rs` and `daemon_client.rs`.

## Global Constraints

- **Transport is loopback TCP plus a bearer token.** No named pipe in this plan.
- **TCP is always authenticated.** The daemon refuses to bind a TCP listener without a token. The
  Unix listener is *not* token-checked — `0600` remains its boundary — so `scripts/run-one-ui.sh`
  and `scripts/run-two-ui.sh` need no change for their normal (unix) path.
- **The wallet stays Unix-socket-only.** `api_router`'s `let wallet_routes = state.socket.is_some();`
  is left exactly as written. On Windows `state.socket` is `None`, so the wallet is not mounted and
  the UI must not offer it. This is the agreed scope, not a bug to patch.
- **Voice is in scope; the wallet is not.**
- **No new config system.** The token travels as a `--auth-token` flag / `HUSH_DAEMON_TOKEN` env var,
  the same shape as `--addr` / `HUSH_DAEMON_ADDR`. `client-config.json` is not extended.
- **The token never touches disk and is never logged.** It lives in the daemon's memory, the shell's
  memory, and the webview global — nothing else.
- **macOS and Linux behavior does not change** except: the vault lock becomes cross-platform
  (Task 1), and the Linux TCP debug path gains a required token (Task 2). Both are called out where
  they land.
- **Never commit to `main`.** Work on a feature branch; PR to trunk.
- **Do not run a bare `cargo fmt`.** The repo is not rustfmt-clean and there is no `rustfmt.toml`;
  see `DEEPSEEKPROBLEMS.MD`, 2026-10-16, "Bare `cargo fmt` reformatted 35 untouched files".

## File map

| File | Responsibility in this plan |
| --- | --- |
| `crates/hush-daemon/Cargo.toml` | Add `fs4`; the daemon's only new dependency |
| `crates/hush-daemon/src/main.rs` | Gate the Unix socket, require TCP off-Unix, token flag, token middleware, layer it on the TCP service |
| `apps/hush-tauri/src-tauri/Cargo.toml` | `hyper-util` needs `client-legacy`'s `HttpConnector`; no new crate |
| `apps/hush-tauri/src-tauri/src/daemon_client.rs` | `cfg(unix)`-gate the Unix connector; add a TCP connector; one `DaemonClient` over both |
| `apps/hush-tauri/src-tauri/src/daemon_supervisor.rs` | Choose the endpoint (Unix / TCP), mint the token, pick the port, pass `--addr`/`--auth-token`, poll readiness over the chosen endpoint |
| `apps/hush-tauri/src-tauri/src/lib.rs` | Map the endpoint to a `Transport`, keep the token, inject `window.__HUSH_TOKEN__` |
| `apps/hush-tauri/src-tauri/tauri.conf.json` | CSP `connect-src` must allow any loopback port |
| `apps/hush-tauri/src/lib/api/daemon.ts` | Send the bearer header in TCP mode; expose `walletAvailable()` |
| `apps/hush-tauri/src/vite-env.d.ts` | Declare `window.__HUSH_TOKEN__` beside the existing globals |
| `apps/hush-tauri/src/app.ts` | Hide the Wallet tile when the wallet is unavailable |
| `apps/hush-tauri/src-tauri/src/tor.rs` | `tor.exe` lookup; the no-signal kill path on Windows |
| `scripts/run-two-ui.sh` | Mint a token for the `--dev` TCP browser flow |
| `.github/workflows/release.yml` | Restore the Windows leg |
| `RELEASING.md`, `README.md`, `docs/DISTRO_READINESS.md` | Record the new transport and the Windows scope |

---

## Task 1: Cross-platform vault lock in the daemon

The daemon refuses a second instance on one vault with an `flock` on
`{vault}/.hush-daemon.lock` — but only under `#[cfg(target_os = "linux")]`
(`crates/hush-daemon/src/main.rs:733`). On macOS and Windows that guard is a no-op today. Windows
has no socket for `bind_socket`'s liveness check to fall back on, so without this the Windows daemon
has *no* one-instance guard at all: two daemons would share one vault and clobber each other's state.
Replacing the raw `libc::flock` with `fs4` (the maintained `fs2` fork) gives `try_lock_exclusive` on
Unix *and* Windows, deletes the `cfg(target_os = "linux")` split, and — the point — makes the lock
exercisable on Linux CI. macOS gains the guard it never had.

**Files:**
- Modify: `crates/hush-daemon/Cargo.toml`
- Modify: `crates/hush-daemon/src/main.rs` (the `acquire_vault_lock` pair, lines 726–760)
- Test: `crates/hush-daemon/src/main.rs` in-file `#[cfg(test)]`

**Interfaces:**
- Produces: `acquire_vault_lock(dir: &Path) -> Result<Option<std::fs::File>, String>` — unchanged
  signature, one definition for every platform, `Ok(Some(file))` holding the lock for the process
  lifetime and `Err` naming the directory when another daemon holds it.
- Consumes: `fs4::fs_std::FileExt` (`try_lock_exclusive`).

- [ ] **Step 1: Add the dependency.**
  ```bash
  cargo add fs4 --features sync -p hush-daemon
  ```
  Let `cargo add` resolve the version; do not hand-pin one. Confirm `fs4` appears in
  `crates/hush-daemon/Cargo.toml` and `Cargo.lock`.

- [ ] **Step 2: Write the failing test.** Add to `main.rs`'s test module (the same
      `TempDir`/`tempfile` shape the other daemon tests use):
  ```rust
  #[test]
  fn the_vault_lock_refuses_a_second_holder() {
      let dir = tempfile::tempdir().unwrap();
      let first = acquire_vault_lock(dir.path()).expect("first lock");
      assert!(first.is_some(), "the first holder gets the lock");
      let second = acquire_vault_lock(dir.path());
      assert!(
          second.is_err(),
          "a second holder must be refused, got {second:?}"
      );
      drop(first);
      let third = acquire_vault_lock(dir.path()).expect("lock is free again after release");
      assert!(third.is_some());
  }
  ```

- [ ] **Step 3: Run — expect FAIL.**
  ```bash
  cargo test -p hush-daemon the_vault_lock_refuses_a_second_holder
  ```
  Expected: the test compiles but fails, because the second `acquire_vault_lock` on this Linux host
  currently *succeeds* (the `libc::flock` path is Linux-only and the test is exercising the same
  process — if it passes here, the lock is already working and the point of the task is the
  cross-platform unification, so keep the test and proceed).

- [ ] **Step 4: Implement.** Replace both `acquire_vault_lock` definitions (the
      `#[cfg(target_os = "linux")]` one and the `#[cfg(not(target_os = "linux"))]` no-op) with a
      single definition:
  ```rust
  /// Take an exclusive, non-blocking lock on the vault directory.
  ///
  /// The socket check in [`bind_socket`] only catches a peer that is *answering*
  /// on the socket. A daemon that is alive but busy — for example blocked in a
  /// long vault store, so it is not accepting — looks dead there, and a second
  /// daemon will unlink the socket and take over. Both then run against one
  /// vault: they share the state sidecar and the vault, so they clobber each
  /// other's state and duplicate downloads. This lock closes that hole. The OS
  /// releases it when the process exits, so a crash never leaves a stale lock.
  ///
  /// `fs4` gives the same `try_lock_exclusive` on Unix (`flock`) and Windows
  /// (`LockFileEx`). On Windows there is no socket for the check above to fall
  /// back on, so this is the only one-instance guard there is.
  fn acquire_vault_lock(dir: &std::path::Path) -> Result<Option<std::fs::File>, String> {
      use fs4::fs_std::FileExt;
      std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
      let path = dir.join(".hush-daemon.lock");
      let file = std::fs::OpenOptions::new()
          .create(true)
          .read(true)
          .write(true)
          .truncate(false)
          .open(&path)
          .map_err(|e| format!("could not open {}: {e}", path.display()))?;
      match file.try_lock_exclusive() {
          Ok(()) => Ok(Some(file)),
          Err(e) => Err(format!(
              "another hush-daemon is already running on {} ({e}); stop it before starting a new one",
              dir.display()
          )),
      }
  }
  ```
  Remove the now-unused `libc` import if the only remaining `libc` uses are in `security.rs` — check
  with `cargo build -p hush-daemon` and drop the dependency only if nothing else in the crate uses it.
  `libc` is `[target.'cfg(target_os = "linux")'.dependencies]` and `security.rs` still uses it, so it
  stays.

- [ ] **Step 5: Run — expect PASS.**
  ```bash
  cargo test -p hush-daemon the_vault_lock_refuses_a_second_holder
  ```

- [ ] **Step 6: Whole daemon suite.**
  ```bash
  cargo test -p hush-daemon --no-fail-fast
  ```
  `--no-fail-fast` on purpose: a fail-fast run hides later suites, which is exactly how the
  pointer-invite regression hid `revocation` (see `DEEPSEEKPROBLEMS.MD`, 2026-10-16).

---

## Task 2: Daemon — gate the Unix socket, require TCP off-Unix, add the token

Two changes to `crates/hush-daemon/src/main.rs`: the socket only exists on Unix, and a TCP listener
is only ever opened behind a token.

**Files:**
- Modify: `crates/hush-daemon/src/main.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `Args` gains `auth_token: Option<String>` (`--auth-token` / `HUSH_DAEMON_TOKEN`);
  `App` gains `auth_token: Option<String>`; `bind_socket` is `#[cfg(unix)]`; a
  `require_token(State<Arc<App>>, Request, Next) -> Response` middleware; the TCP `axum::serve` runs
  a router wrapped in that middleware.

- [ ] **Step 1: Gate `bind_socket` and its helpers.** `bind_socket`
      (`crates/hush-daemon/src/main.rs:684`) returns `tokio::net::UnixListener` unconditionally.
      Put `#[cfg(unix)]` on `bind_socket`; the `is_socket_file` and `socket_is_live` helpers already
      have `#[cfg(not(unix))]` fallbacks and need no change. In `main`, the socket listener becomes:
  ```rust
  #[cfg(unix)]
  let socket_listener = if args.no_socket {
      None
  } else {
      Some(bind_socket(&socket)?)
  };
  ```
  Do **not** add a `#[cfg(not(unix))]` counterpart: `tokio::net::UnixListener` is itself
  `cfg(unix)`, so a non-Unix `None` annotated with that type will not compile. On non-Unix the
  variable simply does not exist, and Step 5 references it only inside `#[cfg(unix)]` blocks.
  The `App.socket` field must agree, so the wallet gate is honest:
  ```rust
  socket: if cfg!(unix) && !args.no_socket {
      Some(socket.clone())
  } else {
      None
  },
  ```

- [ ] **Step 2: Add the `--auth-token` flag.** In `Args`, beside `--addr`:
  ```rust
  /// Bearer token every TCP request must carry. Required whenever TCP is
  /// enabled: the socket's 0600 is the Unix boundary, and this is the TCP one.
  #[arg(long, env = "HUSH_DAEMON_TOKEN")]
  auth_token: Option<String>,
  ```
  Add the field to `App` (`auth_token: Option<String>`) and set it in the constructor from `args`.

- [ ] **Step 3: Refuse an unauthenticated or listener-less startup.** Replace the existing
      `if args.no_socket && tcp_addr.is_none()` guard (`main.rs:539`) with:
  ```rust
  let socket_enabled = cfg!(unix) && !args.no_socket;
  if !socket_enabled && tcp_addr.is_none() {
      return Err(
          "no listener configured: a Unix socket is unavailable on this platform, \
           so --addr (or HUSH_DAEMON_ADDR) is required"
              .into(),
      );
  }
  if tcp_addr.is_some() && args.auth_token.as_deref().unwrap_or("").is_empty() {
      return Err(
          "TCP is enabled without --auth-token (or HUSH_DAEMON_TOKEN); \
           TCP is always authenticated"
              .into(),
      );
  }
  ```

- [ ] **Step 4: Add the middleware.** Near `local_ui_cors`:
  ```rust
  /// Reject any TCP request that does not carry the daemon's bearer token.
  ///
  /// Layered on the TCP service only: the Unix socket is reached through the
  /// filesystem at 0600, so it is already scoped to this user and needs no
  /// token. A constant-time compare is not warranted here — the token is a
  /// fresh 256-bit value per launch and the only peer that can reach loopback
  /// TCP is already running as this user.
  async fn require_token(
      axum::extract::State(state): axum::extract::State<Arc<App>>,
      req: axum::extract::Request,
      next: axum::middleware::Next,
  ) -> axum::response::Response {
      use axum::response::IntoResponse;
      let expected = match state.auth_token.as_deref() {
          Some(t) if !t.is_empty() => t,
          // Unreachable: startup refuses TCP without a token. Fail closed.
          _ => {
              return (
                  StatusCode::UNAUTHORIZED,
                  Json(ApiError { error: "no token configured".into() }),
              )
                  .into_response();
          }
      };
      let presented = req
          .headers()
          .get(header::AUTHORIZATION)
          .and_then(|v| v.to_str().ok())
          .and_then(|v| v.strip_prefix("Bearer "));
      if presented == Some(expected) {
          next.run(req).await
      } else {
          (
              StatusCode::UNAUTHORIZED,
              Json(ApiError { error: "missing or bad token".into() }),
          )
              .into_response()
      }
  }
  ```

- [ ] **Step 5: Layer it on the TCP service only, and keep the non-Unix build single-path.** The
      existing `match (socket_listener, tcp_listener)` block (`main.rs:609`) cannot survive as-is: on
      non-Unix the first tuple element has no type, so inference fails. Match on the TCP listener
      alone and gate the socket serving:
  ```rust
  let tcp_router = router
      .clone()
      .layer(axum::middleware::from_fn_with_state(state.clone(), require_token));

  match tcp_listener {
      Some(tcp) => {
          // Both listeners: the socket keeps its own unauthenticated service,
          // the TCP one carries the token layer.
          #[cfg(unix)]
          if let Some(unix) = socket_listener {
              let socket_router = router.clone();
              let mut unix_rx = shutdown_rx.clone();
              tokio::spawn(async move {
                  let _ = axum::serve(unix, socket_router)
                      .with_graceful_shutdown(async move {
                          let _ = unix_rx.changed().await;
                      })
                      .await;
              });
          }
          let mut tcp_rx = shutdown_rx.clone();
          axum::serve(tcp, tcp_router)
              .with_graceful_shutdown(async move {
                  let _ = tcp_rx.changed().await;
              })
              .await?;
      }
      None => {
          #[cfg(unix)]
          {
              let unix = socket_listener.expect("a Unix listener when TCP is off");
              let mut unix_rx = shutdown_rx.clone();
              axum::serve(unix, router)
                  .with_graceful_shutdown(async move {
                      let _ = unix_rx.changed().await;
                  })
                  .await?;
          }
          #[cfg(not(unix))]
          return Err("no listener configured".into());
      }
  }
  ```
  `state` is already in scope there (it is the `Arc<App>`). On non-Unix the `#[cfg(unix)]` blocks
  compile out and the only reachable arm is `Some(tcp)`; the `None` arm is unreachable because
  startup (Step 3) refuses a listener-less daemon, and it returns an error rather than panicking.

- [ ] **Step 6: Allow the header through CORS.** `local_ui_cors` (`main.rs:279`) allows only
      `header::CONTENT_TYPE`. The webview's request to `http://127.0.0.1:<port>` is cross-origin, so
      the preflight must permit the token header:
  ```rust
  .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
  ```

- [ ] **Step 7: Build.**
  ```bash
  cargo build -p hush-daemon
  ```
  On Linux this compiles the Unix path; the `#[cfg(not(unix))]` arm is not exercised here — it is
  exercised by the Windows CI leg in Task 10 and by the daemon tests in Task 3.

- [ ] **Step 8: Prove the Linux socket path is unchanged.**
  ```bash
  ./scripts/smoke-daemon-socket.sh
  ```
  This script exercises `--addr` alongside the socket (line ~111) and will now fail at the
  `--addr` step unless it passes a token — that failure is the correct signal. Fix it in Task 3's
  step 4 rather than weakening the rule.

---

## Task 3: Daemon — tests for the token rule and the listener matrix

**Files:**
- Modify: `crates/hush-daemon/src/main.rs` (test module)
- Modify: `scripts/smoke-daemon-socket.sh`

**Interfaces:**
- Consumes: `require_token`, `api_router`, the existing `request(&router, method, path, body)` test
  helper.
- Produces: tests that pin the three rules — TCP needs a token, a bad token is 401, a Unix-only
  daemon has no token check.

- [ ] **Step 1: Write the failing tests.** `wallet_app` returns `Arc<App>`
      (`crates/hush-daemon/src/main.rs:7307`), so its `auth_token` cannot be set after construction.
      Refactor it into a parameterised helper and keep the old name as a thin wrapper:
  ```rust
  async fn wallet_app(dir: &std::path::Path, worker: AccountWorker, socket: Option<PathBuf>) -> Arc<App> {
      wallet_app_with_token(dir, worker, socket, None).await
  }

  async fn wallet_app_with_token(
      dir: &std::path::Path,
      worker: AccountWorker,
      socket: Option<PathBuf>,
      auth_token: Option<String>,
  ) -> Arc<App> {
      // The same App literal wallet_app builds today, with auth_token set.
      Arc::new(App { /* existing fields */ auth_token })
  }
  ```
  Then the two tests:
  ```rust
  #[tokio::test]
  async fn a_tcp_request_without_the_token_is_unauthorized() {
      let dir = TempDir::new().unwrap();
      let worker = unlocked(dir.path()).await;
      // No socket plus a token: the Windows shape.
      let state = wallet_app_with_token(dir.path(), worker, None, Some("s3cret".into())).await;
      let router = api_router(state.clone())
          .layer(axum::middleware::from_fn_with_state(state.clone(), require_token));

      let (status, _) = request(&router, "GET", "/api/status", None).await;
      assert_eq!(status, StatusCode::UNAUTHORIZED);

      let (status, _) = request_with_auth(&router, "GET", "/api/status", None, "Bearer wrong").await;
      assert_eq!(status, StatusCode::UNAUTHORIZED);

      let (status, _) = request_with_auth(&router, "GET", "/api/status", None, "Bearer s3cret").await;
      assert_eq!(status, StatusCode::OK);
  }

  #[tokio::test]
  async fn a_unix_only_daemon_needs_no_token() {
      let dir = TempDir::new().unwrap();
      let worker = unlocked(dir.path()).await;
      let state = wallet_app(dir.path(), worker, Some(dir.path().join("hush-daemon.sock"))).await;
      // The socket service is the bare router: no token layer, no token header.
      let router = api_router(state);
      let (status, _) = request(&router, "GET", "/api/status", None).await;
      assert_eq!(status, StatusCode::OK);
  }
  ```
  `request_with_auth` is a copy of the existing `request` helper (`main.rs:7336`) that adds
  `.header(header::AUTHORIZATION, value)`. The existing
  `wallet_routes_are_not_mounted_without_a_unix_socket` test (`main.rs:7385`) shows the
  `wallet_app` / `request` call shape.

- [ ] **Step 2: Run — expect FAIL.**
  ```bash
  cargo test -p hush-daemon a_tcp_request_without_the_token_is_unauthorized
  ```
  Fails to compile until `require_token` and the `auth_token` field exist; that is the red state.

- [ ] **Step 3: Run — expect PASS** (after Task 2 is in).
  ```bash
  cargo test -p hush-daemon --no-fail-fast
  ```

- [ ] **Step 4: Fix the socket smoke script.** `scripts/smoke-daemon-socket.sh:111` starts a daemon
      with `--addr 127.0.0.1:19891` and no token, which now fails at startup. Add a token to that
      invocation and to its `curl` calls:
  ```bash
  HUSH_RUNTIME_DIR="$R" "$DMN" --dir "$T" --addr 127.0.0.1:19891 --auth-token smoke-token \
    >"$T/daemon6.log" 2>&1 &
  ...
  curl -sf -o /dev/null -H "Authorization: Bearer smoke-token" \
    http://127.0.0.1:19891/api/status && break
  ```
  The Unix-socket `curl --unix-socket` calls on the same daemon need no header.
  ```bash
  ./scripts/smoke-daemon-socket.sh
  ```

---

## Task 4: App — `daemon_client` over both Unix and TCP

`apps/hush-tauri/src-tauri/src/daemon_client.rs` imports `tokio::net::UnixStream` unconditionally
(line 24) and builds a `UnixConnector`, so the app crate does not compile on Windows.

**Files:**
- Modify: `apps/hush-tauri/src-tauri/src/daemon_client.rs`
- Modify: `apps/hush-tauri/src-tauri/Cargo.toml` (confirm `hyper-util` features)
- Test: `daemon_client.rs` in-file `#[cfg(test)]`

**Interfaces:**
- Produces: `DaemonClient::new_unix(path: &Path) -> Self` (`#[cfg(unix)]`),
  `DaemonClient::new_tcp(base: &str, token: Option<String>) -> Self`,
  `DaemonClient::request(&self, method, path_and_query, body) -> Result<ApiReply, String>` — the last
  unchanged in signature and now sending `Authorization: Bearer <token>` when one is held.
- Consumes: `hyper_util::client::legacy::connect::HttpConnector`,
  `hyper_util::client::legacy::Client`, `TokioExecutor`.

- [ ] **Step 1: Confirm the connector is available.**
  ```bash
  cargo tree -p hush-tauri -i hyper-util
  ```
  `HttpConnector` needs hyper-util's `client-legacy` feature, which `Cargo.toml` already enables
  (`hyper-util = { version = "0.1", features = ["client-legacy", "http1", "tokio"] }`). If
  `cargo build` in Step 3 disagrees, add `client` to that feature list — do not add a crate.

- [ ] **Step 2: Gate the Unix pieces.** Put `#[cfg(unix)]` on `use tokio::net::UnixStream;`,
  `UnixConnector`, `UnixStreamIo`, and their `AsyncRead`/`AsyncWrite`/`Connection`/`Service` impls.
  `daemon_client.rs`'s tests bind a `tokio::net::UnixListener` (line 221): mark that test
  `#[cfg(unix)]` too.

- [ ] **Step 3: Make `DaemonClient` an enum over the two connectors.** Import hyper's client under
      an alias so the inner enum name is unambiguous:
  ```rust
  use hyper_util::client::legacy::Client as HyperClient;

  pub struct DaemonClient {
      inner: Conn,
      token: Option<String>,
  }

  enum Conn {
      #[cfg(unix)]
      Unix(HyperClient<UnixConnector, Full<Bytes>>),
      Tcp(HyperClient<HttpConnector, Full<Bytes>>),
  }
  ```
  Constructors:
  ```rust
  #[cfg(unix)]
  pub fn new_unix(path: &Path) -> Self {
      Self {
          inner: Conn::Unix(
              HyperClient::builder(TokioExecutor::new()).build(UnixConnector {
                  path: Arc::new(path.to_path_buf()),
              }),
          ),
          token: None,
      }
  }

  pub fn new_tcp(base: &str, token: Option<String>) -> Self {
      Self {
          inner: Conn::Tcp(
              HyperClient::builder(TokioExecutor::new()).build(HttpConnector::new()),
          ),
          token,
      }
  }
  ```
  `new_tcp` must remember `base` so `request` can build the URI — store it beside `token` (a
  `base: Option<String>` field, `None` for the Unix variant, is the smallest change).
  `request` builds the URI as it does today for Unix (`http://localhost{path}`) and as
  `{base}{path}` for TCP, then adds the header when `self.token` is `Some`:
  ```rust
  let mut builder = Request::builder()
      .method(method)
      .uri(uri)
      .header("content-type", "application/json");
  if let Some(token) = &self.token {
      builder = builder.header("authorization", format!("Bearer {token}"));
  }
  let req = builder
      .body(Full::new(Bytes::from(body.unwrap_or_default())))
      .map_err(|e| e.to_string())?;
  ```
  Dispatch the send to the right arm; both yield `Result<Response<Incoming>, _>` with the same
  `map_err(|e| format!("daemon unreachable: {e}"))`.

- [ ] **Step 4: Add a TCP test that runs on Linux.** Next to the existing
      `proxies_get_and_post_over_unix_socket` test, bind a `tokio::net::TcpListener` on
      `127.0.0.1:0`, read the port, serve the same tiny router, and drive it with
      `DaemonClient::new_tcp(&format!("http://127.0.0.1:{port}"), Some("tok".into()))`. Assert a
      request with the token is 200 and — a second client with a wrong token — that a bare
      `request` still succeeds (the client sends whatever token it holds; the *daemon* enforces it,
      so this test only pins that the header is attached and the TCP connector works). To pin the
      header itself, assert against a handler that echoes the `authorization` header back.

- [ ] **Step 5: Run — expect PASS.**
  ```bash
  cargo test -p hush-tauri --lib daemon_client
  ```

---

## Task 5: App — the supervisor chooses and reports the endpoint

`daemon_supervisor::Daemon::start` always passes `--socket`
(`apps/hush-tauri/src-tauri/src/daemon_supervisor.rs:129`), and `Daemon` stores a `socket: PathBuf`.
On Windows it must instead pick a free loopback port, mint a token, and pass `--no-socket --addr …
--auth-token …`.

**Files:**
- Modify: `apps/hush-tauri/src-tauri/src/daemon_supervisor.rs`
- Test: `daemon_supervisor.rs` in-file `#[cfg(test)]`

**Interfaces:**
- Produces:
  ```rust
  pub enum Endpoint {
      Unix { path: PathBuf },
      Tcp { base: String, token: String },
  }
  impl Daemon {
      pub fn start(bin: &Path, socket: &Path, vault: &Path, log_dir: &Path) -> Result<Self, String>;
      pub fn endpoint(&self) -> &Endpoint;
      pub fn wait_ready(&mut self, timeout: Duration) -> bool;
  }
  ```
- Consumes: `crate::tor::pick_free_loopback_port()` (already `pub`, `apps/hush-tauri/src-tauri/src/tor.rs:569`),
  `DaemonClient::{new_unix, new_tcp}`.

- [ ] **Step 1: Add the `Endpoint` type and switch `Daemon` to it.** Replace
      `struct Daemon { child, socket: PathBuf }` with `struct Daemon { child, endpoint: Endpoint }`,
      and replace `pub fn socket(&self) -> &Path` with `pub fn endpoint(&self) -> &Endpoint`.

- [ ] **Step 2: Choose the endpoint in `start`.** Keep the Unix path exactly as it is today, and add
      the TCP path behind a platform test *and* an override so Linux CI can exercise it:
  ```rust
  /// Force the TCP endpoint on a platform that has a Unix socket, so the
  /// Windows path is exercisable on the Linux runner.
  const FORCE_TCP_ENV: &str = "HUSH_DAEMON_FORCE_TCP";

  let use_tcp = cfg!(windows) || std::env::var(FORCE_TCP_ENV).is_ok();
  ```
  TCP branch:
  ```rust
  let port = crate::tor::pick_free_loopback_port()
      .ok_or_else(|| "no free loopback port for the daemon".to_string())?;
  let token = new_launch_token();
  let base = format!("http://127.0.0.1:{port}");
  let child = Command::new(bin)
      .arg("--dir").arg(vault)
      .arg("--no-socket")
      .arg("--addr").arg(format!("127.0.0.1:{port}"))
      .arg("--auth-token").arg(&token)
      .stdin(Stdio::null())
      .stdout(Stdio::from(log))
      .stderr(Stdio::from(log_err))
      .spawn()
      .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
  Ok(Self { child, endpoint: Endpoint::Tcp { base, token } })
  ```
  `new_launch_token` is a small helper — 32 random bytes, hex:
  ```rust
  /// A fresh bearer token per launch. Never written to disk, never logged.
  fn new_launch_token() -> String {
      let mut bytes = [0u8; 32];
      // getrandom through rand, which the crate already depends on.
      rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
      bytes.iter().map(|b| format!("{b:02x}")).collect()
  }
  ```
  Use whatever RNG the crate already pulls in (`rand` is a dependency); if `rand` is not reachable
  from this crate, `getrandom` or `uuid::Uuid::new_v4().simple()` twice is acceptable — the token
  only has to be unguessable and unique per launch.

- [ ] **Step 3: Make `wait_ready` follow the endpoint.**
  ```rust
  pub fn wait_ready(&mut self, timeout: Duration) -> bool {
      let client = match &self.endpoint {
          #[cfg(unix)]
          Endpoint::Unix { path } => DaemonClient::new_unix(path),
          Endpoint::Tcp { base, token } => DaemonClient::new_tcp(base, Some(token.clone())),
      };
      // ... unchanged deadline loop ...
  }
  ```

- [ ] **Step 4: Update the tests.** `a_staged_sidecar_is_started_here` asserts `plan`'s socket and
      vault paths — `Plan` is unchanged, so it still passes. Add a TCP variant of the real-daemon
      test that runs on Linux:
  ```rust
  #[test]
  fn a_spawned_daemon_answers_over_tcp_when_forced() {
      std::env::set_var(FORCE_TCP_ENV, "1");
      let scratch = Scratch::new("spawn-tcp");
      let vault = scratch.path().join(VAULT_DIR_NAME);
      let socket = scratch.path().join(SOCKET_FILE_NAME);
      let bin = staged_daemon();

      let mut daemon =
          Daemon::start(&bin, &socket, &vault, scratch.path()).expect("the daemon starts");
      let (base, token) = match daemon.endpoint() {
          Endpoint::Tcp { base, token } => (base.clone(), token.clone()),
          other => panic!("expected a TCP endpoint, got {other:?}"),
      };
      assert!(base.starts_with("http://127.0.0.1:"), "{base}");
      assert!(!token.is_empty(), "the launch token is minted");
      assert!(
          daemon.wait_ready(READY_TIMEOUT),
          "the daemon never answered over TCP on {base}"
      );

      // The same pair the window reads: no vault, locked.
      let client = DaemonClient::new_tcp(&base, Some(token));
      let status = tauri::async_runtime::block_on(async {
          client.request("GET", "/api/status", None).await.expect("status").body
      });
      assert!(status.contains("\"vault_present\":false"), "{status}");
      assert!(status.contains("\"locked\":true"), "{status}");

      daemon.stop();
      std::env::remove_var(FORCE_TCP_ENV);
  }
  ```
  The existing `status_of(socket: &Path)` helper takes a path; add a `status_of_endpoint(&Endpoint)`
  that branches the same way `wait_ready` does, and have both tests use it. Serialize the env
  mutation with the crate's existing test-lock convention (the `daemon_client` test that sets
  `HUSH_API_URL` already notes single-threaded safety; if these run in parallel, guard with a
  `static Mutex`).

- [ ] **Step 5: Run — expect PASS.**
  ```bash
  cargo test -p hush-tauri --lib daemon_supervisor
  ```
  This is the test that proves the Windows transport end to end on the Linux runner: a real daemon
  process, a real TCP socket, a real token.

---

## Task 6: App — wire the endpoint into the shell and inject the token

**Files:**
- Modify: `apps/hush-tauri/src-tauri/src/lib.rs`
- Modify: `apps/hush-tauri/src-tauri/tauri.conf.json`

**Interfaces:**
- Consumes: `Endpoint` (Task 5), `Transport` (unchanged).
- Produces: the webview globals `window.__HUSH_API__`, `window.__HUSH_TRANSPORT__` (unchanged) plus
  `window.__HUSH_TOKEN__` (TCP only); `ProxyState.client` populated in TCP mode too.

- [ ] **Step 1: Map the endpoint to a transport.** Add `Endpoint` to the existing import
      (`apps/hush-tauri/src-tauri/src/lib.rs:11`, `use daemon_supervisor::{DaemonState, Plan};`). In
      the `Plan::Sidecar` arm (`lib.rs:229`), the current code takes
      `let socket = daemon.socket().display().to_string();` (`lib.rs:247`) — that call no longer
      exists. Replace it and the `Transport::Unix { … }` construction with:
  ```rust
  let (transport, token) = match daemon.endpoint() {
      Endpoint::Unix { path } => (Transport::Unix { path: path.display().to_string() }, None),
      Endpoint::Tcp { base, token } => (
          Transport::Tcp { base: base.clone() },
          Some(token.clone()),
      ),
  };
  ```
  `Endpoint` is declared without `#[cfg]` (Task 5), so both arms exist on every platform. The
  `Err(why)` arm keeps the message it has today; a failed spawn on Windows should still open the
  window, and that arm only feeds the "unreachable" text.

- [ ] **Step 2: Manage a client for TCP too.** `lib.rs:266` currently manages `client: None` for
      `Transport::Tcp`. Give it a client so `hush_api` works in TCP mode as well — and gate the Unix
      arm, because `DaemonClient::new_unix` is `#[cfg(unix)]` and `transport_from_env()` can still
      return `Transport::Unix` on Windows if someone sets `HUSH_API_URL=unix://…`:
  ```rust
  match &transport {
      #[cfg(unix)]
      Transport::Unix { path } => app.manage(ProxyState {
          client: Some(DaemonClient::new_unix(std::path::Path::new(path))),
      }),
      #[cfg(not(unix))]
      Transport::Unix { .. } => app.manage(ProxyState { client: None }),
      Transport::Tcp { base } => app.manage(ProxyState {
          client: Some(DaemonClient::new_tcp(base, token.clone())),
      }),
  };
  ```
  The UI still uses `fetch` in TCP mode (it must — the token lives in the webview), so this is
  belt-and-braces, but it removes the "daemon proxy unavailable in TCP mode" error path.

- [ ] **Step 3: Inject the token.** Extend the init script (`lib.rs:290`):
  ```rust
  let token_js = match &token {
      Some(t) => format!("window.__HUSH_TOKEN__ = {};", serde_json::to_string(t).expect("token json")),
      None => "window.__HUSH_TOKEN__ = null;".to_string(),
  };
  let init_js = format!(
      "window.__HUSH_API__ = {}; window.__HUSH_TRANSPORT__ = {}; {}",
      serde_json::to_string(&display).expect("api url json"),
      serde_json::to_string(&transport).expect("transport json"),
      token_js
  );
  ```

- [ ] **Step 4: Widen the CSP.** The TCP port is chosen per launch, so `connect-src` cannot name it.
      In `apps/hush-tauri/src-tauri/tauri.conf.json`, change
      `connect-src 'self' http://127.0.0.1:9876 http://127.0.0.1:9877 http://127.0.0.1:8787` to
      `connect-src 'self' http://127.0.0.1:*`. Keep the specific ports for the relay if the CSP
      previously constrained them; a single loopback wildcard covers the daemon and the dev
      servers alike. Note in the commit why the wildcard is required (dynamic port).

- [ ] **Step 5: Build and run the existing suite.**
  ```bash
  cargo test -p hush-tauri --lib
  cargo check -p hush-tauri
  ```

---

## Task 7: UI — send the token, and do not offer the wallet where it does not exist

**Files:**
- Modify: `apps/hush-tauri/src/vite-env.d.ts` (the `Window` global the shell injects)
- Modify: `apps/hush-tauri/src/lib/api/daemon.ts`
- Modify: `apps/hush-tauri/src/app.ts`
- Test: `apps/hush-tauri/src/**/*.test.ts` (the UI suite's convention)

**Interfaces:**
- Produces: `walletAvailable(): boolean` from `daemon.ts`; `window.__HUSH_TOKEN__` read once.
- Consumes: `window.__HUSH_TOKEN__` (Task 6).

- [ ] **Step 1: Declare and read the token.** `apps/hush-tauri/src/vite-env.d.ts` already declares
      `__HUSH_API__` and `__HUSH_TRANSPORT__`; add the token the same way:
  ```ts
  interface Window {
    __HUSH_API__?: string;
    /** Set by the Tauri shell; absent when the UI runs in a plain browser. */
    __HUSH_TRANSPORT__?:
      | { kind: "unix"; path: string }
      | { kind: "tcp"; base: string };
    /** Per-launch bearer token; set only in TCP mode. */
    __HUSH_TOKEN__?: string | null;
  }
  ```
  Then in `daemon.ts`, near `transport()`:
  ```ts
  /**
   * The per-launch bearer token the shell injects in TCP mode. Null on the
   * Unix socket (0600 is the boundary there) and in a plain browser.
   */
  function token(): string | null {
    const t = window.__HUSH_TOKEN__;
    return typeof t === "string" && t.length > 0 ? t : null;
  }
  ```
  In `send()`'s TCP branch (`daemon.ts:115`), attach the header:
  ```ts
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const bearer = token();
  if (bearer) headers.Authorization = `Bearer ${bearer}`;
  const res = await fetch(t.base + path, { method, headers, body });
  ```
  A plain browser against a `--dev` daemon gets the token from the query string instead — extend
  `apiFromQuery`'s neighbourhood with a `tokenFromQuery()` reading `?token=`, mirroring `?api=`.

- [ ] **Step 2: Expose wallet availability.**
  ```ts
  /**
   * The wallet moves funds, so the daemon mounts its routes only when it holds a
   * Unix socket (DR-02). A TCP daemon — a Windows release, or a `--dev` browser
   * session — answers 404 there, so the UI must not offer it.
   */
  export function walletAvailable(): boolean {
    return transport().kind === "unix";
  }
  ```

- [ ] **Step 3: Hide the Wallet tile.** In `app.ts`, the wallet is opened from the Vault bento
      (`renderVault`, `app.ts:13292`, and `walletOpenWallet`, `app.ts:11366`). Gate the tile's
      construction on `walletAvailable()` so a TCP build shows a Vault bento with no Wallet tile.
      Also guard the `refreshWallet()` call at `app.ts:2381` so it does not run when unavailable.
      Do not delete `renderWallet` or the store — the Unix build still uses both.

- [ ] **Step 4: Add a test.** In the UI suite's style, assert `walletAvailable()` is `false` for a
      `{ kind: "tcp", base }` transport and `true` for `{ kind: "unix", path }`, by stubbing
      `window.__HUSH_TRANSPORT__`.

- [ ] **Step 5: Run.**
  ```bash
  cd apps/hush-tauri && npm test
  ```

---

## Task 8: Tor on Windows

`tor.rs` looks up Tor by name (`find_on_path("tor")`, `apps/hush-tauri/src-tauri/src/tor.rs:610`) and
kills a stale child with `libc::kill` inside `#[cfg(unix)]` blocks. On Windows the binary is
`tor.exe` and there is no `kill`.

**Files:**
- Modify: `apps/hush-tauri/src-tauri/src/tor.rs`

- [ ] **Step 1: Look for the right name.** Make the lookup platform-aware:
  ```rust
  let names: &[&str] = if cfg!(windows) { &["tor.exe", "tor"] } else { &["tor"] };
  ```
  used wherever `find_on_path` is called for Tor. `HUSH_TOR_BIN` already overrides everything and is
  the documented path for macOS — it becomes the same escape hatch on Windows.

- [ ] **Step 2: Stub the stale-kill path.** The `#[cfg(unix)]` `is_alive` and the `kill` sequence are
  already gated, so they compile out on Windows. Add a `#[cfg(not(unix))]` `is_alive` returning
  `false` if one does not already exist, so `clear_stale` compiles; the Windows path simply does not
  reap an orphaned Tor. Record that as a known limitation in the module header rather than pretending
  it is handled.

- [ ] **Step 3: Do not bundle Tor on Windows.** `tauri.conf.json`'s `bundle.resources` names
      `resources/tor/tor` (a Linux ELF). Leave it: Tauri only bundles resources it can read, and a
      Windows build must not ship a Linux binary as `tor/tor`. If the build errors on the missing
      resource, split the `resources` block by platform — do not add a Linux ELF to a Windows bundle.

- [ ] **Step 4: Confirm the README already covers it.** `README.md`'s Tor section says "On macOS and
      Windows, install Tor or point `HUSH_TOR_BIN` at one." No change needed; Task 12 only verifies it.

---

## Task 9: Voice on Windows (Opus + WASAPI)

Voice is in scope, so the daemon must build `hush-audio` for Windows. `crates/hush-audio/build.rs`
already selects Opus's `USE_ALLOCA` mode for `target_env == "msvc"` (line 45) — the vendored build is
Windows-ready. What is *not* committed is the vendored Opus tree itself; the runner must have it or a
system libopus.

**Files:**
- Modify: `.github/workflows/release.yml` (the Windows leg, Task 10)
- Modify: `crates/hush-audio/vendor/README.md` (only if the fetch needs a Windows note)

- [ ] **Step 1: Establish how the runner gets Opus.** Two options; pick one and write it down:
      (a) the workflow fetches and unzips the pinned Opus tarball into `crates/hush-audio/vendor/opus/`
      before `tauri-action` runs, mirroring `crates/hush-audio/vendor/README.md`'s pinned fetch; or
      (b) `vcpkg install opus` and let `pkg_config::probe_library("opus")` find it. (a) is closer to
      how the vendored path already works and needs no vcpkg wiring.

- [ ] **Step 2: Prove the crate builds for the MSVC triple.** On the Windows runner:
  ```pwsh
  cargo build -p hush-audio --target x86_64-pc-windows-msvc
  ```
  This is the check that catches an Opus or `cc` problem before it hides inside a Tauri bundle build.

- [ ] **Step 3: Confirm capture/playout.** `cpal` selects WASAPI on Windows with no code change. The
  device-present smoke test in `voice_data.rs` is `#[ignore]`d when no device exists — a CI runner has
  no capture device, so it stays ignored there, which is correct. Note it; do not try to force it.

---

## Task 10: CI — restore the Windows leg

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Add the matrix entry.** Replace the comment block at
      `.github/workflows/release.yml:45` with:
  ```yaml
  - platform: windows-latest
    target: x86_64-pc-windows-msvc
    # NSIS only: one installer type, so the updater manifest cannot disagree
    # with how a copy was installed. See RELEASING.md, "Platform notes".
    args: "--bundles nsis"
  ```
  The `install protoc (Windows)` step (`release.yml:104`) and the `shell: bash` sidecar step already
  handle Windows; `stage-daemon-sidecar.sh` already appends `.exe` for a `*windows*` triple.

- [ ] **Step 2: Fetch Opus before the build** (Task 9, option a). Add a step before
      `stage the daemon sidecar`:
  ```yaml
  - name: vendor Opus (Windows)
    if: matrix.platform == 'windows-latest'
    shell: bash
    run: ./scripts/fetch-opus-vendor.sh
  ```
  and create `scripts/fetch-opus-vendor.sh` if `crates/hush-audio/vendor/README.md`'s fetch is not
  already scripted — a small wrapper around the pinned URL and hash it names.

- [ ] **Step 3: Verify `latest.json` carries the Windows platform.** After a release build:
  ```bash
  gh release view <tag> --json assets --jq '.assets[].name'
  gh release download <tag> -p latest.json -O - | jq '.platforms | keys'
  ```
  Expect a `windows-x86_64-nsis` key alongside `darwin-*` and `linux-x86_64`. If it is missing, the
  leg built no updater artifact — check `createUpdaterArtifacts` and the signing secrets reached the
  job.

- [ ] **Step 4: Confirm the mirror carries it.**
  ```bash
  curl -sS https://hushrelay.space/updates/latest.json | jq '{version, platforms: (.platforms | keys)}'
  ```
  `publish-updates.yml` mirrors whatever platforms the manifest names, so no change is needed there;
  this step only proves it.

---

## Task 11: Scripts — token for the `--dev` TCP flows, and a Windows smoke

`scripts/run-two-ui.sh --dev` starts daemons on `127.0.0.1:9876/9877` (line 87) and points browsers
at them; with TCP now authenticated those daemons need a token and the browser URL needs `?token=`.

**Files:**
- Modify: `scripts/run-two-ui.sh`
- Modify: `scripts/run-one-ui.sh` (only if its `--dev` path uses TCP — verify first)

- [ ] **Step 1: Mint one token per dev session and thread it through.** In `run-two-ui.sh`, near the
      `DAEMON_A`/`DAEMON_B` defaults:
  ```bash
  DEV_TOKEN="${DEV_TOKEN:-$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')}"
  ```
  Pass `--auth-token "$DEV_TOKEN"` to both daemons' invocations (the `${addr:+--addr "$addr"}` line,
  `run-two-ui.sh:315`) and append `&token=$DEV_TOKEN` to the browser URLs it prints.

- [ ] **Step 2: Confirm the Unix paths are untouched.** `run-one-ui.sh` passes `--socket` and
      `HUSH_API_URL=unix://…` (lines 252, 281) — no token, no change. Run it and confirm the window
      opens as before:
  ```bash
  ./scripts/run-one-ui.sh
  ```

- [ ] **Step 3: Add a Windows smoke script.** Create `scripts/smoke-windows.ps1` (or a bash script run
      under the Windows runner) that: stages the sidecar for `x86_64-pc-windows-msvc`, starts
      `hush-daemon.exe --dir <tmp> --no-socket --addr 127.0.0.1:<port> --auth-token <tok>`, asserts
      `/api/status` is 401 without the header and 200 with it, then stops it. This is the daemon-level
      proof on the real platform, independent of the Tauri bundle.

---

## Task 12: Docs and honest bookkeeping

**Files:**
- Modify: `RELEASING.md` (the "Windows: not built" note, the "Building locally" and "Platform notes"
  sections)
- Modify: `README.md` (the relay/transport description and the Tor note)
- Modify: `docs/DISTRO_READINESS.md` (the "Socket-first local API" rule gains the Windows exception)
- Modify: `DEEPSEEKPROBLEMS.MD`

- [ ] **Step 1: Rewrite the Windows platform note.** `RELEASING.md` currently says the Windows leg is
      out because `tokio::net::UnixListener` does not exist there. Replace it with: Windows runs the
      daemon over loopback TCP behind a per-launch bearer token; the wallet is not mounted because
      there is no socket; NSIS is the single installer type; the updater looks for
      `windows-x86_64-nsis`.

- [ ] **Step 2: Record the transport exception where the rule lives.** `docs/DISTRO_READINESS.md`'s
      "Socket-first local API" bullet says "TCP is opt-in and never on in release unless explicitly
      configured." Add that on Windows TCP is the transport, it is always token-authenticated, and the
      Unix socket remains the default everywhere it exists.

- [ ] **Step 3: Log the problems this slice hits.** Per the workspace rule, append to
      `DEEPSEEKPROBLEMS.MD` as they happen — the `fs4` version `cargo add` resolved, any Windows-only
      compile error the runner surfaces that this machine cannot reproduce, the Opus vendoring
      decision, and the un-reaped-Tor-on-Windows limitation (an `OPEN` entry, not a passing note).

- [ ] **Step 4: Final full verification.**
  ```bash
  cargo test --workspace --no-fail-fast
  (cd apps/hush-tauri && npm test)
  ./scripts/smoke-daemon-socket.sh
  ./scripts/smoke-mail.sh
  ```
  Then a release dry run on the branch, and read `latest.json` back as in Task 10 Step 3.

---

## Verification summary

| Claim | Evidence |
| --- | --- |
| The vault lock is one-instance on every platform | `cargo test -p hush-daemon the_vault_lock_refuses_a_second_holder` |
| TCP is always authenticated | `cargo test -p hush-daemon a_tcp_request_without_the_token_is_unauthorized` |
| The Unix path is unchanged | `./scripts/smoke-daemon-socket.sh`, `./scripts/run-one-ui.sh` |
| The app speaks TCP and the token crosses | `cargo test -p hush-tauri --lib daemon_supervisor` (real daemon, real port, Linux runner) |
| The wallet is not offered off-Unix | `npm test` in `apps/hush-tauri`; `/api/wallet` is 404 on a TCP daemon |
| Windows builds, bundles, and updates | the `windows-latest` leg; `latest.json` carries `windows-x86_64-nsis`; the relay mirror serves it |
| Voice builds for MSVC | `cargo build -p hush-audio --target x86_64-pc-windows-msvc` |

## Out of scope

- The self-custody wallet on Windows (no socket, so no mount — agreed).
- A Windows named pipe. Loopback TCP plus a token is the transport.
- Bundled Tor for Windows. Install Tor or set `HUSH_TOR_BIN`, as on macOS.
- `mlock`/core-dump hardening on Windows. `security::harden_process` stays a non-Linux stub; note it,
  do not fake it.
- Code signing the Windows installer (Authenticode). The updater's minisign signature is what the
  plugin verifies; a SmartScreen warning on first run is the Windows analogue of the macOS Gatekeeper
  note and belongs in `RELEASING.md` if it bites.

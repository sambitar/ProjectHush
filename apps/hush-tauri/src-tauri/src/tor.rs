//! Local Tor lifecycle for the Drops feature.
//!
//! Hush starts Tor as a child process that listens on loopback only. The SOCKS
//! port it opens is handed to the Drop view (a webview) and, from the Rust
//! side, to the IRC client when a connection is routed through Tor; it is never
//! given to the main window, the daemon proxy, or anything else. That is the
//! whole point: the Hush relay is not in the path, and neither is any other
//! part of Hush.
//!
//! Nothing in this module records a destination. Tor's log is scanned in
//! memory for the bootstrap marker (and teed to `tor.log` in Tor's private
//! data directory so a connection failure is diagnosable; SafeLogging is on
//! and scrubs addresses) — no destination, circuit, or timestamp is surfaced
//! to the UI.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

/// How long `ensure` waits for a cold start before giving up. Tor's first
/// bootstrap (fetching a consensus) can take a while on a slow link.
pub const DEFAULT_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TorPhase {
    Stopped,
    Starting,
    Ready,
    Failed,
    /// No Tor binary could be found on this machine.
    Unavailable,
}

/// The UI-facing shape of the Tor connection. Deliberately carries no
/// destination, no circuit, and no timestamp.
#[derive(Clone, Debug, Serialize)]
pub struct TorStatus {
    pub phase: TorPhase,
    pub socks_port: Option<u16>,
    pub binary: Option<String>,
    pub message: Option<String>,
}

struct Inner {
    phase: TorPhase,
    socks_port: Option<u16>,
    binary: Option<PathBuf>,
    message: Option<String>,
    child: Option<Child>,
    /// True when this manager spawned the Tor it talks to, and so must stop
    /// it. False when it adopted a Tor that was already running (the user's
    /// system Tor): that one is never killed, only used.
    owned: bool,
}

/// Owns the Tor child process and the state the UI polls.
///
/// Cloning shares one process: every clone sees the same state and the same
/// child, so a second `ensure` cannot start a second Tor.
#[derive(Clone)]
pub struct TorManager {
    inner: Arc<Mutex<Inner>>,
    data_dir: PathBuf,
    resource_dir: Option<PathBuf>,
}

impl TorManager {
    /// `data_dir` is where Tor keeps its state; `resource_dir` is where a
    /// bundled Tor would live, if one is shipped next to the app.
    pub fn new(data_dir: PathBuf, resource_dir: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                phase: TorPhase::Stopped,
                socks_port: None,
                binary: None,
                message: None,
                child: None,
                owned: true,
            })),
            data_dir,
            resource_dir,
        }
    }

    /// The current status, cheap to call from a command or a poll.
    pub fn status(&self) -> TorStatus {
        let inner = self.inner.lock().expect("tor state poisoned");
        TorStatus {
            phase: inner.phase,
            socks_port: inner.socks_port,
            binary: inner.binary.as_ref().map(|p| p.display().to_string()),
            message: inner.message.clone(),
        }
    }

    /// Start Tor if it is not already running and block until it has
    /// bootstrapped, the timeout elapses, or it fails. Callers on the async
    /// runtime should invoke this inside `spawn_blocking`.
    pub fn ensure_blocking(&self, timeout: Duration) -> TorStatus {
        let phase = {
            let inner = self.inner.lock().expect("tor state poisoned");
            inner.phase
        };
        match phase {
            TorPhase::Ready => {
                // An adopted Tor can go away (its owner closed it); a cheap
                // probe on this explicit start path notices. A Tor we own is
                // our own child and trusted without a probe.
                if self.adopted_tor_still_live() {
                    return self.status();
                }
                if let Err(status) = self.spawn() {
                    return status;
                }
            }
            // A hard failure (no binary, unusable data dir) is not retried on
            // every click; the user sees the reason and can fix it.
            TorPhase::Unavailable => return self.status(),
            // Another caller is already bringing it up; just wait below.
            TorPhase::Starting => {}
            TorPhase::Stopped | TorPhase::Failed => {
                // Prefer a Tor the user already runs: if their system Tor (or
                // Tor Browser's) is live on the usual loopback SOCKS ports,
                // use it as-is — instant, no second process, no second
                // bootstrap. Spawn our own only when none answers.
                //
                // Not when the operator pinned HUSH_TOR_BIN: an explicit
                // override means "use this binary", and silently adopting a
                // live system Tor instead would hide a broken override (a
                // wrong path, a stale install) with a different Tor the
                // operator does not control. Leaving the override unresolvable
                // must stay a visible failure, not a fallback.
                if std::env::var_os("HUSH_TOR_BIN").is_none() && self.adopt_system_tor_if_live() {
                    return self.status();
                }
                if let Err(status) = self.spawn() {
                    return status;
                }
            }
        }

        let deadline = Instant::now() + timeout;
        loop {
            {
                let inner = self.inner.lock().expect("tor state poisoned");
                match inner.phase {
                    TorPhase::Ready | TorPhase::Failed | TorPhase::Unavailable => {
                        drop(inner);
                        return self.status();
                    }
                    TorPhase::Stopped | TorPhase::Starting => {}
                }
            }
            // A child that exited on its own (bad config, port taken) would
            // otherwise leave us waiting out the whole timeout.
            let exited = {
                let mut inner = self.inner.lock().expect("tor state poisoned");
                match inner.child.as_mut().and_then(|c| c.try_wait().ok().flatten()) {
                    Some(status) => {
                        inner.phase = TorPhase::Failed;
                        inner.socks_port = None;
                        inner.child = None;
                        inner.message = Some(format!(
                            "Tor stopped before it finished connecting ({}).",
                            status
                        ));
                        true
                    }
                    None => false,
                }
            };
            if exited {
                return self.status();
            }
            if Instant::now() >= deadline {
                self.stop();
                let mut inner = self.inner.lock().expect("tor state poisoned");
                inner.phase = TorPhase::Failed;
                inner.message = Some(
                    "Tor did not finish connecting in time. Your Drop has not been opened."
                        .to_string(),
                );
                drop(inner);
                return self.status();
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    /// Stop the child, if any. Idempotent, and called on app exit.
    pub fn stop(&self) {
        let mut inner = self.inner.lock().expect("tor state poisoned");
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Nothing of ours is running now, so there is nothing for a later run
        // to clean up.
        let _ = std::fs::remove_file(self.pid_file());
        inner.phase = TorPhase::Stopped;
        inner.socks_port = None;
        inner.message = None;
    }

    /// Where we record the pid of the Tor we started, so a later run can tell
    /// whether a process sitting on the data directory is one of ours.
    fn pid_file(&self) -> PathBuf {
        self.data_dir.join("tor.pid")
    }

    /// Stop a Tor left behind by a previous run.
    ///
    /// Closing the window stops Tor (the `RunEvent::Exit` handler), but a crash,
    /// a SIGKILL, or a logout does not. The orphan still holds Tor's
    /// data-directory lock, so the next launch's Tor exits at once and every
    /// Drop fails with an error about Tor stopping. Find it, confirm it is ours,
    /// and stop it before starting a replacement.
    fn clear_stale(&self) {
        let pid_file = self.pid_file();
        let Ok(raw) = std::fs::read_to_string(&pid_file) else {
            return;
        };
        // The file is a hint, not a source of truth: clear it whether or not it
        // turns out to name anything.
        let _ = std::fs::remove_file(&pid_file);

        #[cfg(unix)]
        {
            let Ok(pid) = raw.trim().parse::<i32>() else {
                return;
            };
            // Never signal our own pid or the catch-alls.
            if pid <= 1 || !self.is_our_tor(pid) {
                return;
            }
            // SAFETY: signal 0 only probes; SIGTERM goes to a pid just confirmed
            // by `is_our_tor` to be the Tor we started.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            // Tor releases the data-directory lock as it shuts down, so give it a
            // moment rather than racing it into the replacement's lock.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if !is_alive(pid) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            // Still there, so it is not shutting down on its own; insist, or the
            // replacement cannot take the lock at all.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && is_alive(pid) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Whether `pid` is a Tor we started.
    ///
    /// Identity is judged from the process's command line naming our data
    /// directory. That path is private to us (mode 0700) and unique, so a pid
    /// recycled by an unrelated process cannot match it — which matters, because
    /// the caller signals whatever this returns true for.
    #[cfg(unix)]
    fn is_our_tor(&self, pid: i32) -> bool {
        let Ok(output) = Command::new("ps")
            .args(["-o", "args=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        let cmdline = String::from_utf8_lossy(&output.stdout);
        cmdline.contains(&self.data_dir.display().to_string())
    }

    /// Adopt a Tor that is already running, when one is live on the usual
    /// loopback SOCKS ports (system Tor's 9050, Tor Browser's 9150).
    ///
    /// The probe is a real SOCKS5 greeting: a bare TCP connect could be any
    /// service that happens to hold the port. Returns true when a live Tor
    /// was found and adopted; the manager then reports Ready with its port
    /// and never touches that process. If an adopted Tor later disappears,
    /// `adopted_tor_still_live` notices on the next explicit start and a
    /// replacement is spawned.
    fn adopt_system_tor_if_live(&self) -> bool {
        for port in [9050u16, 9150u16] {
            if !probe_socks5(port) {
                continue;
            }
            let mut inner = self.inner.lock().expect("tor state poisoned");
            // Re-check under the lock: a concurrent ensure may have moved
            // the state while the probe ran. Never switch away from our
            // own running Tor.
            if inner.phase == TorPhase::Ready && inner.owned {
                return true;
            }
            if matches!(inner.phase, TorPhase::Starting | TorPhase::Unavailable) {
                return false;
            }
            inner.owned = false;
            inner.phase = TorPhase::Ready;
            inner.socks_port = Some(port);
            inner.child = None;
            inner.message = None;
            return true;
        }
        false
    }

    /// On an explicit start with Tor already marked Ready: confirm an adopted
    /// Tor is still alive (its owner may have closed it). A Tor we spawned is
    /// our own child and trusted without a probe. Returns false only when an
    /// adopted Tor has died — the state is reset so the caller can spawn a
    /// replacement.
    fn adopted_tor_still_live(&self) -> bool {
        let mut inner = self.inner.lock().expect("tor state poisoned");
        if inner.owned {
            return true;
        }
        let Some(port) = inner.socks_port else {
            inner.owned = true;
            inner.phase = TorPhase::Stopped;
            return false;
        };
        if probe_socks5(port) {
            return true;
        }
        inner.owned = true;
        inner.phase = TorPhase::Stopped;
        inner.socks_port = None;
        inner.message = Some("The Tor we adopted went away; starting our own.".into());
        false
    }

    /// Spawn Tor and wire up log scanning. Returns a status when it could not
    /// even be started.
    fn spawn(&self) -> Result<(), TorStatus> {
        let binary = match self.locate_binary() {
            Some(b) => b,
            None => {
                let mut inner = self.inner.lock().expect("tor state poisoned");
                inner.phase = TorPhase::Unavailable;
                inner.binary = None;
                inner.message = Some(
                    "Tor was not found. Install Tor, or point HUSH_TOR_BIN at a Tor binary."
                        .to_string(),
                );
                drop(inner);
                return Err(self.status());
            }
        };

        if let Err(e) = std::fs::create_dir_all(&self.data_dir) {
            let mut inner = self.inner.lock().expect("tor state poisoned");
            inner.phase = TorPhase::Failed;
            inner.message = Some(format!("Could not prepare Tor's data directory: {e}"));
            drop(inner);
            return Err(self.status());
        }
        // Tor state is private; keep it owner-only where the platform allows it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &self.data_dir,
                std::fs::Permissions::from_mode(0o700),
            );
        }

        // A previous run may have been killed before it could stop its Tor. That
        // orphan holds the data-directory lock, so clear it before starting.
        self.clear_stale();

        let port = match pick_free_loopback_port() {
            Some(p) => p,
            None => {
                let mut inner = self.inner.lock().expect("tor state poisoned");
                inner.phase = TorPhase::Failed;
                inner.message = Some("Could not reserve a local port for Tor.".to_string());
                drop(inner);
                return Err(self.status());
            }
        };

        // Empty config files keep Tor hermetic: without these it would read the
        // system torrc and could open listeners or use a directory we do not own.
        let torrc = self.data_dir.join("torrc");
        let defaults = self.data_dir.join("defaults-torrc");
        let _ = std::fs::write(&torrc, b"");
        let _ = std::fs::write(&defaults, b"");

        let mut command = Command::new(&binary);
        // A bundled Tor ships its own libevent beside it. Point the dynamic
        // loader at that directory so the app never depends on a system copy
        // (and never picks up an older one).
        #[cfg(unix)]
        if let Some(lib_dir) = self.bundled_lib_dir(&binary) {
            let mut paths = vec![lib_dir];
            if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                if !existing.is_empty() {
                    paths.extend(std::env::split_paths(&existing));
                }
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("LD_LIBRARY_PATH", joined);
            }
        }
        command
            .arg("--defaults-torrc")
            .arg(&defaults)
            .arg("-f")
            .arg(&torrc)
            // Loopback only: Tor binds this listener to 127.0.0.1, so it is not
            // reachable from the LAN.
            .arg("--SocksPort")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--DataDirectory")
            .arg(&self.data_dir)
            // This process is only ever a client; it must not become an exit.
            .arg("--ClientOnly")
            .arg("1")
            .arg("--AvoidDiskWrites")
            .arg("1")
            // Notice level is enough for the bootstrap line and keeps the log
            // quiet. SafeLogging is on by default and scrubs addresses.
            .arg("--Log")
            .arg("notice stdout")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let mut inner = self.inner.lock().expect("tor state poisoned");
                inner.phase = TorPhase::Failed;
                inner.message = Some(format!("Could not start Tor: {e}"));
                drop(inner);
                return Err(self.status());
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid = child.id();

        {
            let mut inner = self.inner.lock().expect("tor state poisoned");
            inner.phase = TorPhase::Starting;
            inner.socks_port = Some(port);
            inner.binary = Some(binary);
            inner.message = Some("Connecting to Tor…".to_string());
            inner.child = Some(child);
        }

        // Record the pid so that if we are killed before we can stop this child,
        // the next run can recognise and clear it instead of failing to start.
        let _ = std::fs::write(self.pid_file(), pid.to_string());

        // Both streams are scanned for the bootstrap marker. Their contents are
        // not stored and not logged.
        if let Some(out) = stdout {
            self.watch_stream(out);
        }
        if let Some(err) = stderr {
            self.watch_stream(err);
        }
        Ok(())
    }

    fn watch_stream<R: std::io::Read + Send + 'static>(&self, stream: R) {
        let inner = self.inner.clone();
        let log_path = self.data_dir.join("tor.log");
        thread::spawn(move || {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                // Tee to the private data directory: when Tor does not
                // connect, this file is the only evidence of why. SafeLogging
                // is on, so the lines carry no addresses.
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    let _ = writeln!(f, "{line}");
                }
                if line.contains("Bootstrapped 100%") {
                    if let Ok(mut state) = inner.lock() {
                        if state.phase == TorPhase::Starting {
                            state.phase = TorPhase::Ready;
                            state.message = None;
                        }
                    }
                }
            }
        });
    }

    /// The directory beside a bundled Tor binary, when that binary came from the
    /// resource directory. Returns `None` for a system or overridden binary,
    /// whose libraries are found the normal way.
    fn bundled_lib_dir(&self, binary: &Path) -> Option<PathBuf> {
        let resource = self.resource_dir.as_ref()?;
        let dir = binary.parent()?;
        dir.starts_with(resource).then(|| dir.to_path_buf())
    }

    /// Find a Tor binary, preferring an explicit override, then a bundled copy,
    /// then the system.
    fn locate_binary(&self) -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("HUSH_TOR_BIN") {
            let path = PathBuf::from(explicit.trim());
            if path.is_file() {
                return Some(path);
            }
        }
        if let Some(dir) = &self.resource_dir {
            for candidate in ["tor", "tor/tor", "bin/tor"] {
                let path = dir.join(candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        if let Some(found) = find_on_path("tor") {
            return Some(found);
        }
        for candidate in [
            "/usr/bin/tor",
            "/usr/local/bin/tor",
            "/snap/bin/tor",
            "/opt/homebrew/bin/tor",
            "/usr/local/opt/tor/bin/tor",
        ] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
        None
    }
}

impl Drop for TorManager {
    fn drop(&mut self) {
        // Only the last clone should tear the process down. `Arc::strong_count`
        // is 1 here exactly when the final handle goes away.
        if Arc::strong_count(&self.inner) == 1 {
            self.stop();
        }
    }
}

/// Reserve a free port on loopback by binding to port 0 and reading the kernel's
/// choice, then releasing it for Tor to claim. There is a small window between
/// the release and Tor's bind, which is the standard trade for a dynamic port.
pub fn pick_free_loopback_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|addr| addr.port())
}

/// Whether a Tor SOCKS proxy is live on `127.0.0.1:{port}`.
///
/// Sends a SOCKS5 greeting and checks the reply, so a random service holding
/// the port does not pass as Tor. Bounded timeouts keep the probe off any
/// caller's hot path.
fn probe_socks5(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    if stream.set_read_timeout(Some(Duration::from_millis(300))).is_err()
        || stream.set_write_timeout(Some(Duration::from_millis(300))).is_err()
    {
        return false;
    }
    // Greet: SOCKS5, one method, no auth. A Tor replies "5 0" (no auth needed).
    if stream.write_all(&[0x05, 0x01, 0x00]).is_err() {
        return false;
    }
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).is_ok() && reply == [0x05, 0x00]
}

/// Whether a process exists. `kill` with signal 0 performs the existence and
/// permission checks without delivering anything.
#[cfg(unix)]
fn is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 is a no-op probe.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Resolve a bare program name against `PATH`, mirroring `which`.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_bindable_loopback_port() {
        // The picker deliberately releases the port so Tor can claim it, so a
        // rebind here can lose a race with anything else the kernel hands that
        // port to. That window is the documented trade for a dynamic port, so
        // retry instead of asserting something the design does not promise.
        // A picker that returned a port already in use would still fail: the
        // rebind would lose every attempt, not just one.
        let mut bindable = false;
        for _ in 0..20 {
            let port = pick_free_loopback_port().expect("a free port");
            assert!(port > 0);
            if TcpListener::bind(("127.0.0.1", port)).is_ok() {
                bindable = true;
                break;
            }
        }
        assert!(bindable, "no rebindable port in 20 attempts");
    }

    #[test]
    fn missing_binary_reports_unavailable() {
        let dir = std::env::temp_dir().join(format!("hush-tor-test-{}", std::process::id()));
        let manager = TorManager::new(dir.clone(), None);
        // Force the locator down the not-found path regardless of the host.
        std::env::set_var("HUSH_TOR_BIN", "/nonexistent/tor-binary-for-tests");
        let status = manager.ensure_blocking(Duration::from_millis(200));
        std::env::remove_var("HUSH_TOR_BIN");
        assert_eq!(status.phase, TorPhase::Unavailable);
        assert!(status.message.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live check against a real Tor binary. Ignored by default because CI has
    /// no Tor and no network; run it with:
    /// `HUSH_TOR_BIN=/path/to/tor cargo test -p hush-tauri -- --ignored`
    #[test]
    #[ignore = "needs a Tor binary and network; set HUSH_TOR_BIN"]
    fn bootstraps_with_a_real_tor() {
        let dir = std::env::temp_dir().join(format!("hush-tor-live-{}", std::process::id()));
        let manager = TorManager::new(dir.clone(), None);
        let status = manager.ensure_blocking(Duration::from_secs(150));
        assert_eq!(
            status.phase,
            TorPhase::Ready,
            "tor did not bootstrap: {:?}",
            status.message
        );
        assert!(status.socks_port.is_some());
        manager.stop();
        assert_eq!(manager.status().phase, TorPhase::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_is_idempotent_when_never_started() {
        let manager = TorManager::new(std::env::temp_dir(), None);
        manager.stop();
        assert_eq!(manager.status().phase, TorPhase::Stopped);
        assert_eq!(manager.status().socks_port, None);
    }

    /// A stale pid file must always be cleared, or one bad file would wedge
    /// every later start.
    #[test]
    fn clears_a_stale_pid_file() {
        let dir = std::env::temp_dir().join(format!("hush-tor-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let manager = TorManager::new(dir.clone(), None);

        for contents in ["not-a-pid", "999999999", "0"] {
            std::fs::write(manager.pid_file(), contents).expect("write pid");
            manager.clear_stale();
            assert!(
                !manager.pid_file().exists(),
                "pid file survived {contents:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The identity check is the only thing between a stale pid file and a
    /// signal sent to an unrelated process, so it must refuse anything that is
    /// not our Tor. This test process is alive but is not our Tor.
    #[cfg(unix)]
    #[test]
    fn does_not_claim_a_live_process_that_is_not_our_tor() {
        let dir = std::env::temp_dir().join(format!("hush-tor-ident-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let manager = TorManager::new(dir.clone(), None);

        let self_pid = std::process::id() as i32;
        assert!(is_alive(self_pid), "the test process should be alive");
        assert!(
            !manager.is_our_tor(self_pid),
            "a process whose command line does not name our data directory must not be claimed"
        );
        // The same must hold for a pid that is not running at all.
        assert!(!manager.is_our_tor(999_999));

        // And clear_stale must not have signalled us.
        std::fs::write(manager.pid_file(), self_pid.to_string()).expect("write pid");
        manager.clear_stale();
        assert!(is_alive(self_pid), "clear_stale must not kill an unrelated process");
        assert!(!manager.pid_file().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

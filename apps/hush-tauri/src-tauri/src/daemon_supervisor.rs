//! Start the bundled daemon, for a copy of the app that has no run script.
//!
//! In the repository the daemon is started by `scripts/run-one-ui.sh`, which sets
//! `HUSH_API_URL` and keeps the socket, the vault and the relay under its own
//! control. A bundled copy has no script: `bundle.externalBin` stages
//! `hush-daemon` beside the executable — `Contents/MacOS/hush-daemon` in a macOS
//! bundle, next to the binary elsewhere — and this module starts it, waits for it
//! to answer `/api/status`, and stops it when the app exits.
//!
//! Nothing here runs when the environment names a transport. That is the rule
//! that keeps one owner per socket: a run script, a daemon started by hand, or
//! this. It is also why `scripts/run-one-ui.sh` keeps working unchanged.
//!
//! The wait matters. `/api/status` is what tells the window whether this device
//! has a vault, and the two answers are different screens: no vault is the
//! Welcome screen that creates one, an unreachable daemon is the unlock card
//! asking for a passphrase that cannot exist yet. Opening the window before the
//! daemon answers shows the wrong one, and the UI stops polling once status is
//! null, so it stays wrong.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::Manager;

use crate::daemon_client::{self, DaemonClient};

/// The daemon's binary name, as `stage-daemon-sidecar.sh` leaves it.
const DAEMON_BIN_NAME: &str = "hush-daemon";
/// The socket a bundled app owns, inside its own data directory.
const SOCKET_FILE_NAME: &str = "hush-daemon.sock";
/// Where a bundled app keeps the vault it creates.
const VAULT_DIR_NAME: &str = "vault";
/// How long the daemon gets to answer `/api/status` before the window opens
/// anyway. A first run creates the vault after this, so the budget only has to
/// cover process start and socket bind.
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(100);

/// Who starts the daemon for this process.
#[derive(Debug)]
pub enum Plan {
    /// The environment names a transport, or there is no sidecar beside the
    /// executable (a dev build, a test): someone else owns the daemon.
    External,
    /// Nothing names a transport and the sidecar is there: this process starts it.
    Sidecar {
        bin: PathBuf,
        socket: PathBuf,
        vault: PathBuf,
    },
}

/// Decide who starts the daemon, from where the executable and the app's data
/// directory are. Split out from the Tauri plumbing so it can be tested without
/// an app handle.
pub fn plan(exe_dir: &Path, data_dir: &Path) -> Plan {
    if daemon_client::env_transport().is_some() {
        return Plan::External;
    }
    match sidecar_binary(exe_dir) {
        Some(bin) => Plan::Sidecar {
            bin,
            socket: data_dir.join(SOCKET_FILE_NAME),
            vault: data_dir.join(VAULT_DIR_NAME),
        },
        None => Plan::External,
    }
}

/// Decide from the running app: its executable's directory is where Tauri put
/// the sidecar, and its local data directory is where the vault goes.
pub fn plan_for_app(app: &tauri::AppHandle) -> Plan {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let data_dir = app.path().app_local_data_dir().ok();
    match (exe_dir, data_dir) {
        (Some(exe_dir), Some(data_dir)) => plan(&exe_dir, &data_dir),
        // Without both there is nowhere to put a vault or a socket, so the app
        // falls back to whatever the environment names, exactly as before.
        _ => Plan::External,
    }
}

/// The staged daemon beside the executable, if this build has one.
fn sidecar_binary(exe_dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "hush-daemon.exe"
    } else {
        DAEMON_BIN_NAME
    };
    let candidate = exe_dir.join(name);
    candidate.is_file().then_some(candidate)
}

/// The daemon this process started, kept so it can be stopped on exit.
#[derive(Default)]
pub struct DaemonState(pub Mutex<Option<Daemon>>);

pub struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    /// Start the daemon on `socket` with `vault` as its directory.
    ///
    /// `--dir` is not optional: the daemon defaults to `.`, which for an app
    /// opened from Finder or Explorer is `/` or `C:\`.
    pub fn start(bin: &Path, socket: &Path, vault: &Path, log_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(vault)
            .map_err(|e| format!("vault directory {}: {e}", vault.display()))?;
        std::fs::create_dir_all(log_dir)
            .map_err(|e| format!("log directory {}: {e}", log_dir.display()))?;

        // The log is the only place a startup failure explains itself: the window
        // opens either way, and a bundled app has no terminal for stderr.
        let log_path = log_dir.join("daemon.log");
        let log = std::fs::File::create(&log_path)
            .map_err(|e| format!("log file {}: {e}", log_path.display()))?;
        let log_err = log
            .try_clone()
            .map_err(|e| format!("log file {}: {e}", log_path.display()))?;

        let child = Command::new(bin)
            .arg("--dir")
            .arg(vault)
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", bin.display()))?;

        Ok(Self {
            child,
            socket: socket.to_path_buf(),
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Wait until the daemon answers `/api/status`.
    ///
    /// `false` means the wait ran out or the daemon died first. The caller opens
    /// the window regardless: a daemon that failed to start should not look like
    /// a hung application, and `daemon.log` says what happened.
    pub fn wait_ready(&mut self, timeout: Duration) -> bool {
        let client = DaemonClient::new(&self.socket);
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return false, // exited; nothing left to wait for
                Ok(None) => {}
                Err(_) => return false,
            }
            let answered = tauri::async_runtime::block_on(async {
                client.request("GET", "/api/status", None).await.is_ok()
            });
            if answered {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(READY_POLL);
        }
    }

    /// Stop the daemon and reap it, releasing the vault lock.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    /// An app that exits without reaching the exit hook must not leave the
    /// daemon holding the vault lock, or the next run cannot unlock.
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A directory of this test's own, removed when the guard drops.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hush-supervisor-test-{}-{name}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The environment decides: a run script that set `HUSH_API_URL` keeps its
    /// daemon, sidecar or not.
    #[test]
    fn an_environment_transport_wins_over_the_sidecar() {
        let scratch = Scratch::new("env-wins");
        let exe_dir = scratch.path().join("bin");
        let data_dir = scratch.path().join("data");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join(DAEMON_BIN_NAME), b"not really a daemon").unwrap();

        // SAFETY: single-threaded test, and the variable is removed before the
        // next test could read it.
        unsafe { std::env::set_var("HUSH_API_URL", "unix:///tmp/somewhere-else.sock") };
        let decided = plan(&exe_dir, &data_dir);
        unsafe { std::env::remove_var("HUSH_API_URL") };

        assert!(matches!(decided, Plan::External), "{decided:?}");
    }

    /// No environment and no sidecar is a dev build: the app leaves the daemon
    /// to whoever starts it, and reports unreachable if nobody does.
    #[test]
    fn no_sidecar_is_external() {
        let scratch = Scratch::new("no-sidecar");
        let decided = plan(&scratch.path().join("bin"), &scratch.path().join("data"));
        assert!(matches!(decided, Plan::External), "{decided:?}");
    }

    /// A staged sidecar with nothing in the environment is this process's daemon,
    /// on a socket and a vault inside the app's own data directory.
    #[test]
    fn a_staged_sidecar_is_started_here() {
        let scratch = Scratch::new("sidecar");
        let exe_dir = scratch.path().join("bin");
        let data_dir = scratch.path().join("data");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join(DAEMON_BIN_NAME), b"not really a daemon").unwrap();

        match plan(&exe_dir, &data_dir) {
            Plan::Sidecar { bin, socket, vault } => {
                assert_eq!(bin, exe_dir.join(DAEMON_BIN_NAME));
                assert_eq!(socket, data_dir.join(SOCKET_FILE_NAME));
                assert_eq!(vault, data_dir.join(VAULT_DIR_NAME));
            }
            other => panic!("expected a sidecar plan, got {other:?}"),
        }
    }

    /// The daemon Tauri staged for this target. It is not optional: `tauri-build`
    /// refuses to compile this crate without it, so a test that runs at all can
    /// read it, and this test cannot quietly pass on a build that has none.
    fn staged_daemon() -> PathBuf {
        let triple = env!("TAURI_ENV_TARGET_TRIPLE");
        let name = if cfg!(windows) {
            format!("hush-daemon-{triple}.exe")
        } else {
            format!("hush-daemon-{triple}")
        };
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(name)
    }

    fn status_of(socket: &Path) -> String {
        let client = DaemonClient::new(socket);
        tauri::async_runtime::block_on(async {
            client
                .request("GET", "/api/status", None)
                .await
                .expect("status")
                .body
        })
    }

    /// Start the real daemon the way the app does, and ask it the question the
    /// Welcome screen depends on.
    #[test]
    fn a_spawned_daemon_answers_and_reports_no_vault() {
        let scratch = Scratch::new("spawn");
        let vault = scratch.path().join(VAULT_DIR_NAME);
        let socket = scratch.path().join(SOCKET_FILE_NAME);
        let bin = staged_daemon();
        assert!(
            bin.is_file(),
            "{} is missing — run scripts/stage-daemon-sidecar.sh",
            bin.display()
        );

        let mut daemon =
            Daemon::start(&bin, &socket, &vault, scratch.path()).expect("the daemon starts");
        assert!(
            daemon.wait_ready(READY_TIMEOUT),
            "the daemon never answered on {}",
            socket.display()
        );

        // This is the pair the window reads to choose between the Welcome screen
        // and the unlock card: no vault, and locked because nothing is unlocked.
        let status = status_of(&socket);
        assert!(status.contains("\"vault_present\":false"), "{status}");
        assert!(status.contains("\"locked\":true"), "{status}");

        daemon.stop();
    }

    /// Stopping the daemon releases the vault lock, so the next launch can start
    /// its own. A leaked daemon would wedge the app rather than fail loudly.
    #[test]
    fn stopping_releases_the_vault_for_the_next_start() {
        let scratch = Scratch::new("restart");
        let vault = scratch.path().join(VAULT_DIR_NAME);
        let socket = scratch.path().join(SOCKET_FILE_NAME);
        let bin = staged_daemon();

        let mut first =
            Daemon::start(&bin, &socket, &vault, scratch.path()).expect("the first daemon starts");
        assert!(first.wait_ready(READY_TIMEOUT), "the first daemon never answered");
        first.stop();

        let mut second = Daemon::start(&bin, &socket, &vault, scratch.path())
            .expect("the second daemon starts");
        assert!(
            second.wait_ready(READY_TIMEOUT),
            "the second daemon never answered; the first still held {}",
            vault.display()
        );
        second.stop();
    }
}

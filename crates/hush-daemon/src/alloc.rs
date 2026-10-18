//! Allocator tuning for the daemon's real-time work.
//!
//! glibc gives each thread its own arena once threads contend on the main one,
//! up to eight per core. Every arena is a heap of its own — a 64 MiB region on
//! 64-bit, taken with `mmap` and handed out piece by piece — and a block freed
//! by one thread is not reusable by another until that thread's arena is the one
//! asked. The daemon is exactly the shape that suffers from this: a tokio
//! runtime's workers, the capture and playout threads, and one worker per voice
//! and paper plane all burst-allocate and burst-free, so instead of reusing a
//! pool, the process grows a new heap per thread and touches fresh pages.
//!
//! What that costs is latency, not only RSS. A fresh page is a page fault, and a
//! page fault on the thread filling the next Opus frame is audible. Capping the
//! arenas makes the threads share two heaps, so a buffer freed by a request
//! handler is the buffer the next frame is encoded into.
//!
//! The cap is applied in-process rather than by the launcher so no launcher can
//! forget it — the repository's run scripts, the bundled sidecar the desktop
//! shell starts, the systemd unit, and a daemon started by hand all get it, and
//! none of them has to know. `main` builds the runtime by hand for the same
//! reason: `#[tokio::main]` would run the body with the workers already
//! spawned, and this has to happen while the process still has one thread.
//!
//! Two caveats, both deliberate. The arenas that already exist when this runs
//! are not destroyed — none has been created yet, which is the point of running
//! it first — and a cap of two on a many-core host trades a little lock
//! contention for the reuse, which is the right way round for a daemon whose
//! allocation rate is per-request and per-frame rather than per-cycle.
//!
//! Not addressed here, and the next thing to reach for if a latency measurement
//! ever shows the cap is not enough: a jemalloc or mimalloc `#[global_allocator]`
//! would do the same job with per-thread caches and decayed purging, at the cost
//! of a dependency and a second allocator in the tree. That is a change to make
//! against a measurement, not on the strength of this reasoning — see
//! `docs/tasks/app-smoothness.md`, item 6, which is where the measurement is
//! written down as outstanding.

use std::ffi::OsStr;

/// The environment variable glibc reads for this, and the name an operator would
/// set to override what follows.
const ARENA_MAX_ENV: &str = "MALLOC_ARENA_MAX";

/// How many heaps the daemon's threads share. Two is enough to keep the main
/// arena off the hot path without giving every thread a private one.
const ARENA_MAX: i32 = 2;

/// Whether this process should set the cap, given what the environment says.
///
/// An unset variable is the only case where this process's opinion is wanted.
/// A set one is an operator naming the tunable — glibc has already applied it
/// by the time `main` runs — and overriding that from inside the daemon would
/// make the one escape hatch for a latency investigation useless.
fn should_tune(env: Option<&OsStr>) -> bool {
    env.is_none()
}

/// Cap glibc's per-thread arenas, and return the line the daemon logs for it.
///
/// The return value is a summary rather than nothing because this is a fix whose
/// effect is invisible from outside the process: whether it applied, and whether
/// an operator's override took precedence, is the first thing a latency
/// investigation needs to know, and the log is where it has to be readable.
pub fn tune_arenas() -> String {
    if !should_tune(std::env::var_os(ARENA_MAX_ENV).as_deref()) {
        return format!("arenas=set by {ARENA_MAX_ENV}");
    }
    cap_arenas(ARENA_MAX)
}

/// The glibc-specific half, so the target guard is in one place.
///
/// `M_ARENA_MAX` is glibc's, and so is `mallopt`; a target that uses a different
/// allocator (musl, macOS, Windows) has no arenas to cap and says so rather than
/// pretending. `mallopt` returns zero when it refuses the parameter, which is
/// worth reporting rather than swallowing: it is the difference between a
/// daemon that is tuned and one that only looks tuned.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn cap_arenas(max: i32) -> String {
    // SAFETY: `mallopt` takes two ints and touches only libc's own tunables.
    if unsafe { libc::mallopt(libc::M_ARENA_MAX, max) } != 0 {
        format!("arenas<={max}")
    } else {
        "arenas=glibc default (mallopt refused)".to_string()
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn cap_arenas(_max: i32) -> String {
    "arenas=n/a (not glibc)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_variable_is_tuned() {
        assert!(should_tune(None));
    }

    #[test]
    fn an_operators_value_is_left_alone() {
        // Including an empty one: it is still the operator having spoken, and
        // silently overriding it would hide the override that was asked for.
        assert!(!should_tune(Some(OsStr::new("4"))));
        assert!(!should_tune(Some(OsStr::new(""))));
    }

    #[test]
    fn every_target_reports_what_it_did_with_the_arenas() {
        // `cap_arenas` rather than `tune_arenas`: the latter reads the real
        // environment, and a developer with `MALLOC_ARENA_MAX` exported would
        // make an assertion about this host's answer into a flake.
        assert!(!cap_arenas(ARENA_MAX).is_empty());
    }

    /// The real call on the target that has the tunable: glibc accepting the
    /// parameter, which is the only part of the fix that can be checked without
    /// a latency measurement.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn glibc_accepts_the_arena_cap() {
        assert_eq!(cap_arenas(ARENA_MAX), format!("arenas<={ARENA_MAX}"));
    }
}

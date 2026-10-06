//! Process hardening for the vault daemon.
//!
//! The daemon is the only process that holds plaintext key material, so before
//! it reads a passphrase it disables core dumps and locks its address space out
//! of swap. Everything here is best-effort: a failure is reported and logged,
//! never fatal, because hardening depends on the host's limits and sandbox.

/// Below this much memlock headroom, `MCL_FUTURE` is unsafe to request.
#[cfg(target_os = "linux")]
const FUTURE_LOCK_MIN_BYTES: u64 = 256 * 1024 * 1024;

/// What hardening actually took effect; the daemon logs this at startup.
#[derive(Debug, Clone)]
pub struct Hardening {
    pub core_dumps_disabled: bool,
    /// Address space pinned in RAM, so keys cannot be written to swap.
    pub memory_locked: bool,
    /// Set when locking only partly applied, or did not apply at all.
    pub memory_lock_note: Option<String>,
}

impl Hardening {
    pub fn summary(&self) -> String {
        let core = if self.core_dumps_disabled {
            "off"
        } else {
            "ON"
        };
        let lock = if self.memory_locked { "on" } else { "OFF" };
        let mut s = format!("core_dumps={core} memory_locked={lock}");
        if let Some(note) = &self.memory_lock_note {
            s.push_str(&format!(" ({note})"));
        }
        s
    }
}

/// Best-effort hardening before handling passphrases or keys.
pub fn harden_process() -> Hardening {
    #[cfg(target_os = "linux")]
    {
        let core_dumps_disabled = disable_core_dumps_linux();
        let (memory_locked, memory_lock_note) = lock_memory_linux();
        Hardening {
            core_dumps_disabled,
            memory_locked,
            memory_lock_note,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Hardening {
            core_dumps_disabled: false,
            memory_locked: false,
            memory_lock_note: Some("unsupported platform".into()),
        }
    }
}

#[cfg(target_os = "linux")]
fn disable_core_dumps_linux() -> bool {
    // PR_SET_DUMPABLE=0 also blocks ptrace by other processes of this uid.
    let dumpable = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0;
    let lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let core = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &lim) } == 0;
    dumpable && core
}

/// Lock the address space into RAM. Returns `(locked, note)`.
///
/// `MCL_FUTURE` is only used when the memlock limit is comfortably larger than
/// any plausible growth: with a tight limit, future allocations would fail
/// outright instead of merely paging, turning a hardening measure into a crash.
#[cfg(target_os = "linux")]
fn lock_memory_linux() -> (bool, Option<String>) {
    let headroom = memlock_headroom();
    let roomy = headroom.is_some_and(|h| h >= FUTURE_LOCK_MIN_BYTES);
    let flags = if roomy {
        libc::MCL_CURRENT | libc::MCL_FUTURE
    } else {
        libc::MCL_CURRENT
    };
    if unsafe { libc::mlockall(flags) } == 0 {
        return match roomy {
            true => (true, None),
            false => (
                true,
                Some("current pages only; set LimitMEMLOCK=infinity for full coverage".into()),
            ),
        };
    }
    let err = std::io::Error::last_os_error();
    if flags != libc::MCL_CURRENT && unsafe { libc::mlockall(libc::MCL_CURRENT) } == 0 {
        return (
            true,
            Some(format!("current pages only; MCL_FUTURE failed: {err}")),
        );
    }
    (false, Some(err.to_string()))
}

/// Raise `RLIMIT_MEMLOCK` as far as the host allows, returning the effective
/// soft limit in bytes (`u64::MAX` when unlimited).
#[cfg(target_os = "linux")]
fn memlock_headroom() -> Option<u64> {
    let want = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // Best effort: raising the hard limit needs CAP_SYS_RESOURCE.
    unsafe {
        libc::setrlimit(libc::RLIMIT_MEMLOCK, &want);
    }
    let mut got = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut got) } != 0 {
        return None;
    }
    Some(if got.rlim_cur == libc::RLIM_INFINITY {
        u64::MAX
    } else {
        got.rlim_cur
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn harden_reports_core_dumps_disabled() {
        let h = harden_process();
        assert!(h.core_dumps_disabled, "core dumps should be disabled");
    }

    #[test]
    fn summary_is_human_readable() {
        let h = Hardening {
            core_dumps_disabled: true,
            memory_locked: true,
            memory_lock_note: None,
        };
        assert_eq!(h.summary(), "core_dumps=off memory_locked=on");
        let h = Hardening {
            core_dumps_disabled: false,
            memory_locked: false,
            memory_lock_note: Some("nope".into()),
        };
        assert_eq!(h.summary(), "core_dumps=ON memory_locked=OFF (nope)");
    }
}

/// Best-effort hardening before handling passphrases or keys.
pub fn harden_process() {
    #[cfg(target_os = "linux")]
    {
        disable_core_dumps_linux();
    }
}

#[cfg(target_os = "linux")]
fn disable_core_dumps_linux() {
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &lim);
    }
}

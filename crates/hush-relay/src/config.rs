use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub max_blob_bytes: usize,
    pub blob_ttl: Duration,
    /// Max PUTs per queue per window.
    pub rate_limit_max: u32,
    pub rate_limit_window: Duration,
    /// Cap on one bulletin blob. Smaller than `max_blob_bytes` on purpose: a
    /// zine is text, and the bulletin is the corpus store rather than a drained
    /// inbox, so its rows live far longer and are held by everyone.
    pub bulletin_max_blob_bytes: usize,
    /// How long a bulletin blob stays. Longer than `blob_ttl` because the
    /// bulletin is what a member browses, not a mailbox they empty; survival
    /// past the window is the holders' vaults plus the re-post sweep.
    pub bulletin_ttl: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_blob_bytes: 1024 * 1024,
            blob_ttl: Duration::from_secs(7 * 24 * 60 * 60),
            rate_limit_max: 60,
            rate_limit_window: Duration::from_secs(60),
            bulletin_max_blob_bytes: 256 * 1024,
            bulletin_ttl: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

impl RelayConfig {
    pub fn for_tests() -> Self {
        Self {
            max_blob_bytes: 1024 * 1024,
            blob_ttl: Duration::from_secs(1),
            rate_limit_max: 3,
            rate_limit_window: Duration::from_secs(60),
            bulletin_ttl: Duration::from_secs(1),
            ..Self::default()
        }
    }

    /// A config whose bulletin TTL is short but whose chat TTL is not, for the
    /// bulletin tests: `for_tests` shortens both, which would expire the queue
    /// fixtures too.
    pub fn for_bulletin_tests() -> Self {
        Self {
            bulletin_ttl: Duration::from_secs(1),
            ..Self::default()
        }
    }
}

use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub max_blob_bytes: usize,
    pub blob_ttl: Duration,
    /// Max PUTs per queue per window.
    pub rate_limit_max: u32,
    pub rate_limit_window: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_blob_bytes: 1024 * 1024,
            blob_ttl: Duration::from_secs(7 * 24 * 60 * 60),
            rate_limit_max: 60,
            rate_limit_window: Duration::from_secs(60),
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
            ..Self::default()
        }
    }
}

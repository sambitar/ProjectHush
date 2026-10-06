use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use uuid::Uuid;

const LINK_TTL: Duration = Duration::from_secs(10 * 60);

struct PendingLink {
    data: Vec<u8>,
    expires: Instant,
}

pub struct LinkDeviceStore {
    pending: Mutex<HashMap<String, PendingLink>>,
}

impl LinkDeviceStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub async fn store(&self, data: Vec<u8>) -> (String, u64) {
        let id = Uuid::new_v4().to_string();
        let expires = Instant::now() + LINK_TTL;
        let expires_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + LINK_TTL.as_secs();
        let mut map = self.pending.lock().await;
        map.retain(|_, v| v.expires > Instant::now());
        map.insert(
            id.clone(),
            PendingLink {
                data,
                expires,
            },
        );
        (id, expires_unix)
    }

    pub async fn take(&self, id: &str) -> Option<Vec<u8>> {
        let mut map = self.pending.lock().await;
        map.retain(|_, v| v.expires > Instant::now());
        let entry = map.remove(id)?;
        if entry.expires <= Instant::now() {
            return None;
        }
        Some(entry.data)
    }
}

pub fn validate_source_api(base: &str) -> Result<(), String> {
    let base = base.trim();
    if base.starts_with("http://") || base.starts_with("https://") {
        if base.len() > 512 {
            return Err("source URL too long".into());
        }
        return Ok(());
    }
    Err("source API must be http:// or https://".into())
}

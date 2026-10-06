use reqwest::Client;
use serde::Deserialize;

use crate::error::Error;

pub struct RelayClient {
    base: String,
    http: Client,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    blobs: Vec<BlobMeta>,
}

#[derive(Debug, Deserialize)]
struct BlobMeta {
    id: String,
}

/// Task 5c.4: HTTPS for non-loopback relays; plain HTTP allowed for localhost (and any HTTP in debug builds).
pub fn normalize_relay_base(base: &str) -> Result<String, Error> {
    let base = base.trim().trim_end_matches('/').to_string();
    if base.starts_with("https://") {
        return Ok(base);
    }
    if base.starts_with("http://") {
        if relay_http_allowed(&base) {
            return Ok(base);
        }
        return Err(Error::Relay(
            "insecure relay URL: use HTTPS except for localhost".into(),
        ));
    }
    Err(Error::Relay(
        "relay URL must start with http:// or https://".into(),
    ))
}

fn relay_http_allowed(url: &str) -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    let rest = url.strip_prefix("http://").unwrap_or("");
    let host = rest.split('/').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or(host);
    host == "127.0.0.1" || host == "localhost" || host == "[::1]" || host == "::1"
}

impl RelayClient {
    pub fn new(base: &str) -> Self {
        Self::try_new(base).unwrap_or_else(|_| Self {
            base: base.trim().trim_end_matches('/').to_string(),
            http: Client::builder()
                .build()
                .expect("http client"),
        })
    }

    pub fn try_new(base: &str) -> Result<Self, Error> {
        let base = normalize_relay_base(base)?;
        Ok(Self {
            base,
            http: Client::builder()
                .build()
                .expect("http client"),
        })
    }

    fn auth(token: &str) -> String {
        format!("Bearer {token}")
    }

    pub async fn put(&self, queue_id: &str, token: &str, body: &[u8]) -> Result<(), Error> {
        let res = self
            .http
            .put(format!("{}/q/{}", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("put failed: {}", res.status())));
        }
        Ok(())
    }

    pub async fn list(&self, queue_id: &str, token: &str) -> Result<Vec<String>, Error> {
        let res = self
            .http
            .get(format!("{}/q/{}", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("list failed: {}", res.status())));
        }
        let parsed: ListResponse = res.json().await.map_err(|e| Error::Relay(e.to_string()))?;
        Ok(parsed.blobs.into_iter().map(|b| b.id).collect())
    }

    pub async fn get(
        &self,
        queue_id: &str,
        token: &str,
        blob_id: &str,
    ) -> Result<Vec<u8>, Error> {
        let res = self
            .http
            .get(format!("{}/q/{}/{}", self.base, queue_id, blob_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("get failed: {}", res.status())));
        }
        res.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Relay(e.to_string()))
    }

    pub async fn delete(&self, queue_id: &str, token: &str, blob_id: &str) -> Result<(), Error> {
        let res = self
            .http
            .delete(format!("{}/q/{}/{}", self.base, queue_id, blob_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("delete failed: {}", res.status())));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_relay_base;

    #[test]
    fn https_relay_ok() {
        assert!(normalize_relay_base("https://relay.example.com").is_ok());
    }

    #[test]
    fn loopback_http_ok() {
        assert!(normalize_relay_base("http://127.0.0.1:8080").is_ok());
        assert!(normalize_relay_base("http://localhost:9999").is_ok());
    }
}

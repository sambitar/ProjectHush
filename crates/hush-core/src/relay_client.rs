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

impl RelayClient {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: Client::builder()
                .build()
                .expect("http client"),
        }
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

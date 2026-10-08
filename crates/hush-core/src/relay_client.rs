use reqwest::Client;
use serde::Deserialize;

use crate::error::Error;

pub struct RelayClient {
    base: String,
    http: Client,
}

/// How long a single relay request may take.
///
/// Without this, a relay address that black-holes traffic hangs on the OS TCP
/// timeout — minutes — and every other request in the account worker queues
/// behind it. The Outbox retry makes that load-bearing: it runs on a timer with
/// nobody waiting on the result, so an unbounded request would freeze the UI.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct ListResponse {
    blobs: Vec<BlobMeta>,
}

#[derive(Debug, Deserialize)]
struct BlobMeta {
    id: String,
}

/// Whether plain HTTP may be used for a relay that is not on loopback.
///
/// This is an explicit parameter rather than an inline `cfg!(debug_assertions)`
/// so that both branches are reachable from tests. The release rule is the one
/// that protects shipped clients, and a debug test run can never exercise it
/// otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpPolicy {
    /// Debug builds: HTTP anywhere, for local dogfood and tests.
    Allow,
    /// Release builds: HTTPS required unless the relay is on loopback.
    LoopbackOnly,
}

impl HttpPolicy {
    /// The policy for the build this code was compiled into.
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Allow
        } else {
            Self::LoopbackOnly
        }
    }
}

/// Task 5c.4: HTTPS for non-loopback relays; plain HTTP allowed for localhost (and any HTTP in debug builds).
pub fn normalize_relay_base(base: &str) -> Result<String, Error> {
    normalize_relay_base_with(base, HttpPolicy::current())
}

/// [`normalize_relay_base`] with the HTTP policy supplied explicitly.
pub fn normalize_relay_base_with(base: &str, policy: HttpPolicy) -> Result<String, Error> {
    let base = base.trim().trim_end_matches('/').to_string();
    if base.starts_with("https://") {
        return Ok(base);
    }
    if base.starts_with("http://") {
        if http_url_allowed(&base, policy) {
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

fn http_url_allowed(url: &str, policy: HttpPolicy) -> bool {
    match policy {
        HttpPolicy::Allow => true,
        HttpPolicy::LoopbackOnly => reqwest::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .is_some_and(|host| host_is_loopback(&host)),
    }
}

/// Whether a parsed URL host is loopback.
///
/// `Url::host_str` returns IPv6 literals still bracketed (`[::1]`), so brackets
/// come off before the address is parsed. A host that is neither `localhost` nor
/// a parseable loopback address is refused, which fails closed for names like
/// `127.0.0.1.example.com`.
fn host_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

impl RelayClient {
    pub fn new(base: &str) -> Self {
        Self::try_new(base).unwrap_or_else(|_| Self {
            base: base.trim().trim_end_matches('/').to_string(),
            http: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("http client"),
        })
    }

    pub fn try_new(base: &str) -> Result<Self, Error> {
        let base = normalize_relay_base(base)?;
        Ok(Self {
            base,
            http: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("http client"),
        })
    }

    fn auth(token: &str) -> String {
        format!("Bearer {token}")
    }

    pub async fn touch_presence(&self, queue_id: &str, token: &str) -> Result<(), Error> {
        let res = self
            .http
            .put(format!("{}/q/{}/presence", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("presence failed: {}", res.status())));
        }
        Ok(())
    }

    pub async fn contact_presence_online(
        &self,
        queue_id: &str,
        token: &str,
    ) -> Result<bool, Error> {
        let res = self
            .http
            .get(format!("{}/q/{}/presence", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Ok(false);
        }
        #[derive(serde::Deserialize)]
        struct Body {
            online: bool,
        }
        let body: Body = res.json().await.map_err(|e| Error::Relay(e.to_string()))?;
        Ok(body.online)
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

    /// List a queue, treating a missing or burned queue as empty.
    ///
    /// A per-contact queue that a delete has burned reports 403 (`Burned`), not
    /// 404: the row is kept so the burn cannot be undone by re-creating the
    /// queue with the same credential. Both mean the same thing to a drain —
    /// there is nothing to read — so both are handled here rather than surfacing
    /// as an error. A wrong-token failure is 401 and still errors.
    pub async fn list_optional(&self, queue_id: &str, token: &str) -> Result<Vec<String>, Error> {
        let res = self
            .http
            .get(format!("{}/q/{}", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND
            || res.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Ok(Vec::new());
        }
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

    /// Burn a queue so nothing more can be delivered to it.
    ///
    /// Used on revocation: the contact's inbox is destroyed and its token stops
    /// working, so anything that contact still has queued is dropped rather than
    /// sitting on the relay for someone who should no longer reach us.
    pub async fn report_queue(&self, queue_id: &str, token: &str) -> Result<(), Error> {
        let res = self
            .http
            .post(format!("{}/q/{}/report", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("report failed: {}", res.status())));
        }
        Ok(())
    }

    /// Burn a queue if it exists, treating a missing one as already gone.
    ///
    /// Revocation burns the queue for every contact, including ones that never
    /// delivered. A queue that was never created has nothing to burn, and the
    /// relay cannot distinguish "never created" from "already burned" — both
    /// refuse — so a refusal here is not a failure the caller should act on.
    pub async fn burn_if_exists(&self, queue_id: &str, token: &str) -> Result<(), Error> {
        let res = self
            .http
            .post(format!("{}/q/{}/report", self.base, queue_id))
            .header("authorization", Self::auth(token))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND
            || res.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Ok(());
        }
        if !res.status().is_success() {
            return Err(Error::Relay(format!("report failed: {}", res.status())));
        }
        Ok(())
    }

    /// Park an outsider delivery container on the relay.
    ///
    /// No credential: the id is a capability the sender chose, and the body is
    /// already ciphertext under a PIN the relay never sees.
    pub async fn put_drop(&self, delivery_id: &str, body: &[u8]) -> Result<(), Error> {
        let res = self
            .http
            .put(format!("{}/drop/{}", self.base, delivery_id))
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("drop put failed: {}", res.status())));
        }
        Ok(())
    }

    /// Fetch a delivery container. Used by the pickup page and by tests; the
    /// member's own client does not read its own outgoing drops back.
    pub async fn get_drop(&self, delivery_id: &str) -> Result<Vec<u8>, Error> {
        let res = self
            .http
            .get(format!("{}/drop/{}", self.base, delivery_id))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("drop get failed: {}", res.status())));
        }
        res.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Relay(e.to_string()))
    }

    /// Publish this account's prekey record so peers can open a session without
    /// an invite.
    ///
    /// The record is keyed by account alone. An account has exactly one device,
    /// so the `device_id` segment that used to distinguish them is gone; the
    /// relay verifies the record's identity key against the account id instead.
    pub async fn put_prekey_record(&self, account_id: &str, body: &[u8]) -> Result<(), Error> {
        let res = self
            .http
            .put(format!("{}/k/{}", self.base, account_id))
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("prekey put failed: {}", res.status())));
        }
        Ok(())
    }

    /// Fetch an account's prekey record, consuming one of its one-time prekeys.
    pub async fn get_prekey_record(&self, account_id: &str) -> Result<Vec<u8>, Error> {
        let res = self
            .http
            .get(format!("{}/k/{}", self.base, account_id))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("prekey get failed: {}", res.status())));
        }
        res.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Relay(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_relay_base, normalize_relay_base_with, HttpPolicy};

    #[test]
    fn https_relay_ok() {
        assert!(normalize_relay_base("https://relay.example.com").is_ok());
    }

    #[test]
    fn loopback_http_ok() {
        assert!(normalize_relay_base("http://127.0.0.1:8080").is_ok());
        assert!(normalize_relay_base("http://localhost:9999").is_ok());
    }

    #[test]
    fn release_refuses_remote_http() {
        for url in [
            "http://relay.example.com",
            "http://203.0.113.1:8787",
            "http://10.0.0.5:8787",
            "http://192.168.1.10:8787",
            "http://0.0.0.0:8787",
            // Names that merely start with a loopback literal must not pass.
            "http://127.0.0.1.example.com",
            "http://localhost.example.com",
            // Userinfo must not be mistaken for the host.
            "http://127.0.0.1@relay.example.com",
        ] {
            assert!(
                normalize_relay_base_with(url, HttpPolicy::LoopbackOnly).is_err(),
                "{url} should be refused in a release build"
            );
        }
    }

    #[test]
    fn release_allows_loopback_http() {
        for url in [
            "http://127.0.0.1:8787",
            "http://127.0.0.1",
            "http://127.0.0.2:8787",
            "http://localhost:8787",
            "http://LOCALHOST:8787",
            "http://[::1]:8787",
            "http://[::1]",
        ] {
            assert!(
                normalize_relay_base_with(url, HttpPolicy::LoopbackOnly).is_ok(),
                "{url} is loopback and should be allowed in a release build"
            );
        }
    }

    #[test]
    fn release_allows_https_anywhere() {
        assert!(
            normalize_relay_base_with("https://relay.example.com", HttpPolicy::LoopbackOnly)
                .is_ok()
        );
        assert!(
            normalize_relay_base_with("https://203.0.113.1:8787", HttpPolicy::LoopbackOnly).is_ok()
        );
    }

    #[test]
    fn debug_policy_allows_remote_http() {
        assert!(
            normalize_relay_base_with("http://10.0.0.5:8787", HttpPolicy::Allow).is_ok(),
            "debug builds keep plain HTTP for dogfood"
        );
    }

    #[test]
    fn non_http_scheme_refused_under_both_policies() {
        for policy in [HttpPolicy::Allow, HttpPolicy::LoopbackOnly] {
            assert!(normalize_relay_base_with("ftp://relay.example", policy).is_err());
            assert!(normalize_relay_base_with("relay.example", policy).is_err());
        }
    }

    /// The production entry point must follow the build it was compiled into.
    /// Run under `--release` for this to prove the shipped behaviour.
    #[test]
    fn production_entry_point_follows_build_mode() {
        let remote = "http://10.0.0.5:8787";
        if cfg!(debug_assertions) {
            assert!(
                normalize_relay_base(remote).is_ok(),
                "debug builds keep plain HTTP for dogfood"
            );
        } else {
            assert!(
                normalize_relay_base(remote).is_err(),
                "release builds must refuse non-loopback HTTP"
            );
        }
        assert!(normalize_relay_base("https://relay.example.com").is_ok());
        assert!(normalize_relay_base("http://127.0.0.1:8787").is_ok());
    }
}

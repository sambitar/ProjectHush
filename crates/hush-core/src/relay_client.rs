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

/// Environment override for a SOCKS proxy placed in front of every relay
/// request, e.g. a local Tor instance: `socks5h://127.0.0.1:9050`. The `h`
/// in `socks5h` sends DNS through the proxy too, which is what makes `.onion`
/// hosts resolvable. Read by [`RelayClient::try_new`]; unset means connect
/// directly.
pub const ENV_RELAY_SOCKS_PROXY: &str = "HUSH_RELAY_SOCKS_PROXY";

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
    /// Release builds: HTTPS required unless the relay is on loopback or the
    /// host is a v3 `.onion`. An onion address authenticates itself (the host
    /// name is the service's public key) and Tor encrypts the link end to end,
    /// so there is no certificate layer to demand — the same rule the Drops
    /// feature applies to webview targets.
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
            .is_some_and(|host| host_is_loopback(&host) || is_v3_onion_host(&host)),
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

/// Whether a parsed URL host is a v3 onion address: exactly 56 base32
/// characters (a-z, 2-7) followed by `.onion`.
///
/// Matching the host alone cannot be fooled by userinfo (`http://x@<onion>`)
/// because `Url::host_str` excludes userinfo, and a lookalike like
/// `evil.onion.attacker.com` fails the suffix strip. The same rule lives in
/// the Tauri app's `drops` module for webview targets; the app does not depend
/// on this crate, so the two validate their own surfaces.
pub fn is_v3_onion_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    match host.strip_suffix(".onion") {
        Some(label) => {
            label.len() == 56
                && label
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
        }
        None => false,
    }
}

/// Validate a relay onion address and return it normalized.
///
/// The host must be a v3 onion and the scheme `http` or `https`. Plain `http`
/// is fine even in a release build — the onion layer authenticates the
/// address and Tor encrypts the link end to end, the same rule the Drops
/// feature applies — but anything that is not an onion is rejected outright:
/// this seam exists to attach the Tor form of a relay, not to point the relay
/// path at arbitrary hosts.
pub fn validate_onion_relay(raw: &str) -> Result<String, Error> {
    let base = normalize_relay_base_with(raw, HttpPolicy::LoopbackOnly)?;
    let host = reqwest::Url::parse(&base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    if !is_v3_onion_host(&host) {
        return Err(Error::Relay(format!("not a v3 onion address: {raw}")));
    }
    Ok(base)
}

/// Validate a SOCKS proxy value for [`ENV_RELAY_SOCKS_PROXY`]-style use: the
/// same strictness [`RelayClient::try_new_with_proxy`] applies, exposed for
/// callers that want to reject a bad value before handing it to a client.
pub fn validate_socks_proxy(value: &str) -> Result<(), Error> {
    socks_proxy(value).map(|_| ())
}

/// The configured SOCKS proxy, if any. Whitespace-only counts as unset so a
/// half-edited env line does not become a broken client.
fn proxy_from_env() -> Option<String> {
    std::env::var(ENV_RELAY_SOCKS_PROXY)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Build a SOCKS proxy from a validated value.
///
/// Strict on purpose. `reqwest::Proxy::all` accepts schemeless strings and
/// foreign schemes without complaint (scheme matching is deferred to request
/// time), so a typo in the environment could silently produce a proxy that
/// never matches — i.e. direct, un-Tor'd traffic — or an `http://` proxy where
/// SOCKS was intended. This seam exists to put Tor in the path or fail loudly;
/// only `socks5` / `socks5h` with a host qualify.
fn socks_proxy(value: &str) -> Result<reqwest::Proxy, Error> {
    let url = reqwest::Url::parse(value)
        .map_err(|e| Error::Relay(format!("bad relay SOCKS proxy URL: {e}")))?;
    let scheme = url.scheme();
    if !matches!(scheme, "socks5" | "socks5h") || url.host_str().is_none() {
        return Err(Error::Relay(format!(
            "relay SOCKS proxy must be socks5h://host:port (e.g. a local Tor), got {value:?}"
        )));
    }
    reqwest::Proxy::all(url).map_err(|e| Error::Relay(format!("bad relay SOCKS proxy URL: {e}")))
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
        Self::try_new_with_proxy(base, proxy_from_env().as_deref())
    }

    /// [`RelayClient::try_new`] with the SOCKS proxy supplied explicitly
    /// instead of read from [`ENV_RELAY_SOCKS_PROXY`]. `None` means connect
    /// directly — and *only* directly: ambient proxy environment variables
    /// (`ALL_PROXY` and friends) are deliberately ignored so the relay path
    /// goes through Tor exactly when this seam says it should.
    pub fn try_new_with_proxy(base: &str, proxy: Option<&str>) -> Result<Self, Error> {
        let base = normalize_relay_base(base)?;
        let mut builder = Client::builder().timeout(REQUEST_TIMEOUT);
        match proxy {
            Some(proxy) => {
                builder = builder.proxy(socks_proxy(proxy)?);
            }
            None => builder = builder.no_proxy(),
        }
        Ok(Self {
            base,
            http: builder.build().expect("http client"),
        })
    }

    fn auth(token: &str) -> String {
        format!("Bearer {token}")
    }

    /// Whether the relay answers a health check within `timeout`.
    ///
    /// The probe takes the same path as real traffic — the SOCKS proxy when
    /// one is set — so it answers the one question the UI must never lie
    /// about: can this vault reach its relay *right now*? Never errors; a
    /// failure to connect is simply `false`.
    pub async fn ping(&self, timeout: std::time::Duration) -> bool {
        let request = self.http.get(format!("{}/health", self.base)).send();
        matches!(
            tokio::time::timeout(timeout, request).await,
            Ok(Ok(res)) if res.status().is_success()
        )
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

    /// Park a sealed drop under its rendezvous id.
    ///
    /// No token, by design: the id is the capability and the body is ciphertext
    /// the dropper already sealed to the recipient's published key. The relay
    /// rate-limits and size-caps per id; the response is only a status.
    pub async fn put_drop(&self, id: &str, body: &[u8]) -> Result<(), Error> {
        let res = self
            .http
            .put(format!("{}/drop/{}", self.base, id))
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("drop put failed: {}", res.status())));
        }
        Ok(())
    }

    /// Fetch the drop parked on a rendezvous slot.
    ///
    /// `None` when the slot is empty — the common case on every drain pass, so
    /// a 404 is an answer, not an error. Non-consuming by design: the member
    /// deletes only after the container opened and filed, so a drain that dies
    /// mid-flight finds the drop still there.
    pub async fn get_drop(&self, id: &str) -> Result<Option<Vec<u8>>, Error> {
        let res = self
            .http
            .get(format!("{}/drop/{}", self.base, id))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(Error::Relay(format!("drop get failed: {}", res.status())));
        }
        res.bytes()
            .await
            .map(|b| Some(b.to_vec()))
            .map_err(|e| Error::Relay(e.to_string()))
    }

    /// Consume a drained drop.
    ///
    /// Idempotent on purpose: two of the member's devices may drain together,
    /// and the second delete finding an empty slot is success, not failure.
    pub async fn delete_drop(&self, id: &str) -> Result<(), Error> {
        let res = self
            .http
            .delete(format!("{}/drop/{}", self.base, id))
            .send()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        if !res.status().is_success() {
            return Err(Error::Relay(format!("drop delete failed: {}", res.status())));
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
    use super::{
        is_v3_onion_host, normalize_relay_base, normalize_relay_base_with,
        ENV_RELAY_SOCKS_PROXY, HttpPolicy, RelayClient,
    };
    use std::sync::Mutex;

    /// `try_new` reads process-global env; serialize tests that touch it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// The hostname of the lab relay's onion service (docs/ops/relay-lab-vps.md).
    /// Doubling as a test vector keeps the deployed address and the validator
    /// honest against each other.
    const LAB_ONION: &str = "ogkf745dfn32pdbvsk6e52uqski7mudadowosi5cbyif4yglhzuqlsad.onion";

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

    /// A plain-HTTP onion relay is legitimate in release builds: the v3
    /// hostname authenticates the service and Tor encrypts the link, so there
    /// is no TLS layer to require.
    #[test]
    fn release_allows_http_for_v3_onion() {
        for url in [
            format!("http://{LAB_ONION}"),
            // Tor onion services can publish any virtual port.
            format!("http://{LAB_ONION}:8790"),
            // Host matching is case-insensitive on the scheme side.
            format!("http://{}", LAB_ONION.to_ascii_uppercase()),
        ] {
            assert!(
                normalize_relay_base_with(&url, HttpPolicy::LoopbackOnly).is_ok(),
                "{url} is a v3 onion and should be allowed in a release build"
            );
        }
    }

    /// The onion allowance must not become a hole: a name that merely contains
    /// or extends `.onion`, a short label, or a label outside base32 is not a
    /// v3 onion and keeps the HTTPS requirement.
    #[test]
    fn release_refuses_onion_lookalikes() {
        let label = &LAB_ONION[..LAB_ONION.len() - ".onion".len()];
        assert_eq!(label.len(), 56, "the lab vector must be a real v3 label");
        let short = format!("http://{}.onion", &label[..55]);
        let bad_char = format!("http://{}8.onion", &label[..55]); // '8' is not base32
        for url in [
            "http://evil.onion.attacker.com".to_string(),
            "http://attacker.com?x=ogkf.onion".to_string(),
            short,
            bad_char,
            "http://.onion".to_string(),
        ] {
            assert!(
                normalize_relay_base_with(&url, HttpPolicy::LoopbackOnly).is_err(),
                "{url} must be refused in a release build"
            );
        }
        assert!(!is_v3_onion_host(&format!("{LAB_ONION}.evil.com")));
        // And the real one passes, so the refusals above are not vacuous.
        assert!(is_v3_onion_host(LAB_ONION));
    }

    /// An explicitly supplied proxy must parse, and a bad one must fail the
    /// constructor rather than surfacing later as a request error.
    #[test]
    fn proxy_urls_are_validated_at_construction() {
        let ok = RelayClient::try_new_with_proxy(
            &format!("http://{LAB_ONION}"),
            Some("socks5h://127.0.0.1:9050"),
        );
        assert!(matches!(ok, Ok(RelayClient { .. })));
        assert!(RelayClient::try_new_with_proxy("http://127.0.0.1:8787", Some("not a url")).is_err());
        // `http://` proxies are valid to reqwest, so the refusal needs a
        // scheme it genuinely does not support.
        assert!(
            RelayClient::try_new_with_proxy("http://127.0.0.1:8787", Some("ftp://proxy.invalid"))
                .is_err()
        );
    }

    #[test]
    fn try_new_reads_the_socks_proxy_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_RELAY_SOCKS_PROXY);
        assert!(RelayClient::try_new(&format!("http://{LAB_ONION}")).is_ok());

        std::env::set_var(ENV_RELAY_SOCKS_PROXY, "garbage");
        assert!(RelayClient::try_new("http://127.0.0.1:8787").is_err());

        // Whitespace-only counts as unset, not as a broken proxy.
        std::env::set_var(ENV_RELAY_SOCKS_PROXY, "   ");
        assert!(RelayClient::try_new("http://127.0.0.1:8787").is_ok());

        std::env::remove_var(ENV_RELAY_SOCKS_PROXY);
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

//! The XRPC client: the one module here that touches the network.
//!
//! ATProto is HTTPS + JSON over a fixed set of endpoints, so this is small
//! enough to read in one sitting. Two things about it are deliberate.
//!
//! **The proxy seam.** [`Xrpc::new`] takes an optional SOCKS proxy and is the
//! only constructor. When one is given, every request goes through it; when
//! none is, the client is built with [`reqwest::ClientBuilder::no_proxy`] so
//! ambient `ALL_PROXY`-style variables are ignored. That mirrors the relay path
//! exactly (`hush_core::relay_client`), and for the same reason: the public lane
//! is the only place this app's bytes leave for a third party, so whether they
//! go through Tor is decided here and nowhere else.
//!
//! **Refresh on refusal, once.** An access token lives about two hours. Rather
//! than trust a clock, a write that comes back 401 is retried exactly once
//! after a refresh, and the caller is handed the session as it now stands. A
//! second 401 is a real refusal — a revoked app password, a disabled account —
//! and is reported rather than retried into a loop.

use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::error::Error;
use crate::session::Session;

/// How long one XRPC request may take.
///
/// The same reasoning as the relay client's bound: a PDS that black-holes
/// traffic would otherwise hang on the OS TCP timeout, and every other request
/// in the account worker queues behind it.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// One record's address on a PDS: where it is and which bytes it is.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RecordRef {
    /// `at://<did>/<collection>/<rkey>`.
    pub uri: String,
    /// The content hash the PDS assigned.
    pub cid: String,
}

/// The session fields a `createSession` or `refreshSession` answer carries.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    access_jwt: String,
    refresh_jwt: String,
    did: String,
    #[serde(default)]
    handle: String,
}

/// An ATProto error body. `error` is the machine name, `message` the prose.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct ListRecordsResponse {
    #[serde(default)]
    records: Vec<ListedRecord>,
}

/// The `resolveHandle` answer.
#[derive(Debug, Deserialize)]
struct ResolveHandleResponse {
    #[serde(default)]
    did: String,
}

/// The `describeRepo` answer, of which only the handle is wanted.
#[derive(Debug, Deserialize)]
struct DescribeRepoResponse {
    #[serde(default)]
    handle: String,
}

#[derive(Debug, Deserialize)]
struct ListedRecord {
    uri: String,
    #[serde(default)]
    value: serde_json::Value,
}

/// One record as `listRecords` returns it: the address, and the record itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub uri: String,
    pub value: serde_json::Value,
}

/// The DID document, trimmed to the one thing this needs: where the PDS is.
#[derive(Debug, Deserialize)]
struct DidDocument {
    #[serde(default)]
    service: Vec<DidService>,
}

#[derive(Debug, Deserialize)]
struct DidService {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "serviceEndpoint")]
    endpoint: String,
}

/// A client for one member's public network traffic.
pub struct Xrpc {
    http: Client,
    /// The directory `did:plc` identifiers resolve against. A field rather than
    /// a constant so the read lane can be tested against a stub, and so a
    /// deployment that must use a mirror can name one.
    plc: String,
}

/// The PLC directory a `did:plc` resolves against.
pub const DEFAULT_PLC_DIRECTORY: &str = "https://plc.directory";

impl Xrpc {
    /// Build a client, optionally behind a SOCKS proxy.
    ///
    /// `proxy` is `socks5h://host:port` (the `h` sends DNS through the proxy, so
    /// no lookup happens on this machine). A value that is not one of the two
    /// SOCKS schemes fails here rather than at the first request: `reqwest`
    /// accepts an `http://` proxy silently and would then send traffic the
    /// caller believed was going through Tor.
    pub fn new(proxy: Option<&str>) -> Result<Self, Error> {
        Self::with_plc(proxy, DEFAULT_PLC_DIRECTORY)
    }

    /// [`Xrpc::new`] with the PLC directory supplied explicitly.
    pub fn with_plc(proxy: Option<&str>, plc: &str) -> Result<Self, Error> {
        let mut builder = Client::builder().timeout(REQUEST_TIMEOUT);
        builder = match proxy {
            Some(value) => builder.proxy(socks_proxy(value)?),
            None => builder.no_proxy(),
        };
        Ok(Self {
            http: builder
                .build()
                .map_err(|e| Error::Transport(e.to_string()))?,
            plc: plc.trim().trim_end_matches('/').to_string(),
        })
    }

    /// Sign in with an app password and return the session it mints.
    ///
    /// The PDS is supplied rather than discovered, because the member knows it
    /// (or accepts the default) and a lookup would be a second failure mode on
    /// the first thing they ever do.
    pub async fn create_session(
        &self,
        pds: &str,
        identifier: &str,
        password: &str,
    ) -> Result<Session, Error> {
        let pds = normalize_pds(pds)?;
        let body = serde_json::json!({ "identifier": identifier, "password": password });
        let res = self
            .http
            .post(format!("{pds}/xrpc/com.atproto.server.createSession"))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let answer: SessionResponse = self.parse(res).await?;
        Ok(Session {
            did: answer.did,
            handle: answer.handle,
            pds,
            password: password.to_string(),
            access_jwt: answer.access_jwt,
            refresh_jwt: answer.refresh_jwt,
        })
    }

    /// Mint fresh tokens from the refresh token.
    ///
    /// A failure here is not necessarily fatal to the session: the app password
    /// is still on file, so a caller can fall back to [`Self::create_session`].
    /// That is what makes a revoked refresh token a hiccup rather than a
    /// sign-out.
    pub async fn refresh(&self, session: &Session) -> Result<Session, Error> {
        if session.refresh_jwt.trim().is_empty() {
            return Err(Error::Invalid(
                "this session has no refresh token to rotate".to_string(),
            ));
        }
        let res = self
            .http
            .post(format!(
                "{}/xrpc/com.atproto.server.refreshSession",
                session.pds
            ))
            .bearer_auth(&session.refresh_jwt)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let answer: SessionResponse = self.parse(res).await?;
        // A refresh answer names the account it rotated. If that is not the one
        // this session belonged to, the tokens must not be adopted: writing
        // under the wrong repo is worse than failing.
        if !answer.did.is_empty() && answer.did != session.did {
            return Err(Error::Refused {
                status: 401,
                message: "the refreshed session names a different account".to_string(),
            });
        }
        Ok(session.with(answer.access_jwt, answer.refresh_jwt, Some(answer.handle)))
    }

    /// Revoke the session on the PDS, so the tokens stop working.
    ///
    /// Best-effort by contract: the caller is disconnecting, and a PDS that
    /// cannot be reached must not leave the vault holding a connection the
    /// member asked to drop.
    pub async fn delete_session(&self, session: &Session) -> Result<(), Error> {
        if session.refresh_jwt.trim().is_empty() {
            return Ok(());
        }
        let res = self
            .http
            .post(format!(
                "{}/xrpc/com.atproto.server.deleteSession",
                session.pds
            ))
            .bearer_auth(&session.refresh_jwt)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        if res.status().is_success() || res.status() == StatusCode::UNAUTHORIZED {
            // Already invalid is the same outcome as revoked.
            return Ok(());
        }
        Err(self.refusal(res).await)
    }

    /// Write one record, replacing whatever is at `rkey`.
    ///
    /// `putRecord` rather than `createRecord` because publishing is idempotent
    /// by address: a re-export of an edited post must replace the record, not
    /// add a second one beside it. Returns the record reference **and** the
    /// session as it now stands, because a refresh may have rotated the tokens.
    pub async fn put_record(
        &self,
        session: &Session,
        collection: &str,
        rkey: &str,
        record: &serde_json::Value,
    ) -> Result<(RecordRef, Session), Error> {
        let body = serde_json::json!({
            "repo": session.did,
            "collection": collection,
            "rkey": rkey,
            "record": record,
            "validate": true,
        });
        let url = format!("{}/xrpc/com.atproto.repo.putRecord", session.pds);

        let res = self
            .http
            .post(&url)
            .bearer_auth(&session.access_jwt)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;

        // The one retry: an expired access token. Anything else is the PDS's
        // real answer and is reported as it stands.
        let (res, session) = if res.status() == StatusCode::UNAUTHORIZED {
            let rotated = self.refresh(session).await?;
            let retry = self
                .http
                .post(&url)
                .bearer_auth(&rotated.access_jwt)
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;
            (retry, rotated)
        } else {
            (res, session.clone())
        };

        let reference: RecordRef = self.parse(res).await?;
        Ok((reference, session))
    }

    /// List one collection on one repo.
    ///
    /// Read from the author's own PDS rather than an index, so nothing about who
    /// reads whom leaves this machine and no AppView is put in the path. That is
    /// the whole reason the lane is curated: there is no firehose to subscribe
    /// to, only the repos the member asked for.
    pub async fn list_records(
        &self,
        pds: &str,
        repo: &str,
        collection: &str,
        limit: u32,
    ) -> Result<Vec<Listed>, Error> {
        let pds = normalize_pds(pds)?;
        let res = self
            .http
            .get(format!("{pds}/xrpc/com.atproto.repo.listRecords"))
            .query(&[
                ("repo", repo),
                ("collection", collection),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let answer: ListRecordsResponse = self.parse(res).await?;
        Ok(answer
            .records
            .into_iter()
            .map(|r| Listed {
                uri: r.uri,
                value: r.value,
            })
            .collect())
    }

    /// Where an account's repo lives, from its DID.
    ///
    /// `did:plc` resolves at the PLC directory, `did:web` at the domain it
    /// names. Both answer a DID document; the PDS is the service entry whose id
    /// is `#atproto_pds`. This is the only lookup the read lane does, and it
    /// asks about an account the member already chose to follow — never about
    /// anyone else.
    pub async fn resolve_pds(&self, did: &str) -> Result<String, Error> {
        let url = match did.strip_prefix("did:plc:") {
            Some(_) => format!("{}/{did}", self.plc),
            None => match did.strip_prefix("did:web:") {
                Some(domain) => format!("https://{}/.well-known/did.json", domain.replace(':', "/")),
                None => {
                    return Err(Error::Invalid(format!(
                        "`{did}` is not a did:plc or did:web identifier"
                    )))
                }
            },
        };
        let res = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let doc: DidDocument = self.parse(res).await?;
        doc.service
            .into_iter()
            .find(|s| s.id == "#atproto_pds")
            .map(|s| s.endpoint.trim_end_matches('/').to_string())
            .filter(|endpoint| !endpoint.is_empty())
            .ok_or_else(|| Error::Invalid(format!("{did} publishes no PDS endpoint")))
    }

    /// The DID behind a handle, asked of one PDS.
    ///
    /// Needed because a member knows the handle they read and not the DID that
    /// addresses the repo: `resolveHandle` is the standard way to cross that
    /// gap, and it is answered by a PDS rather than an AppView, so the lane
    /// still talks to hosts it was already talking to. A handle that is already
    /// a DID is passed through without a request.
    pub async fn resolve_handle(&self, pds: &str, handle: &str) -> Result<String, Error> {
        let handle = handle.trim().trim_start_matches('@');
        if handle.is_empty() {
            return Err(Error::Invalid("a follow needs a handle or a DID".to_string()));
        }
        if handle.starts_with("did:") {
            return Ok(handle.to_string());
        }
        let pds = normalize_pds(pds)?;
        let res = self
            .http
            .get(format!("{pds}/xrpc/com.atproto.identity.resolveHandle"))
            .query(&[("handle", handle)])
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let answer: ResolveHandleResponse = self.parse(res).await?;
        let did = answer.did.trim().to_string();
        if did.is_empty() {
            return Err(Error::Invalid(format!("`{handle}` resolves to no DID")));
        }
        Ok(did)
    }

    /// The handle a repo publishes, read from the PDS that serves it.
    ///
    /// The PDS is the authority on the handle it serves, so this asks the account
    /// rather than an index — which is what keeps discovery inside the same
    /// per-repo boundary the read lane already holds to. `handleIsCorrect` is
    /// deliberately ignored: a handle that has gone stale is still the name to
    /// show on a directory row, and refusing it would blank the row over a detail
    /// the reader cannot act on.
    pub async fn describe_repo(&self, pds: &str, repo: &str) -> Result<String, Error> {
        let pds = normalize_pds(pds)?;
        let res = self
            .http
            .get(format!("{pds}/xrpc/com.atproto.repo.describeRepo"))
            .query(&[("repo", repo)])
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let answer: DescribeRepoResponse = self.parse(res).await?;
        Ok(answer.handle.trim().to_string())
    }

    /// Turn a response into `T`, or into the refusal it actually is.
    async fn parse<T: serde::de::DeserializeOwned>(&self, res: reqwest::Response) -> Result<T, Error> {
        if !res.status().is_success() {
            return Err(self.refusal(res).await);
        }
        res.json::<T>()
            .await
            .map_err(|e| Error::Json(e.to_string()))
    }

    /// Read an error response, preferring the lexicon's own `message`.
    async fn refusal(&self, res: reqwest::Response) -> Error {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        match serde_json::from_str::<ErrorBody>(&body) {
            Ok(parsed) if !parsed.message.trim().is_empty() => Error::Refused {
                status,
                message: parsed.message,
            },
            Ok(parsed) if !parsed.error.trim().is_empty() => Error::Refused {
                status,
                message: parsed.error,
            },
            // Not a lexicon error: a proxy's HTML page, a truncated body. The
            // status is still the honest thing to report.
            _ => Error::Refused {
                status,
                message: if body.trim().is_empty() {
                    "no message".to_string()
                } else {
                    body.chars().take(200).collect()
                },
            },
        }
    }
}

/// A PDS base, normalized and checked.
///
/// HTTPS is required, except on loopback where plain HTTP is allowed so the
/// crate's own tests can run a stub PDS without a certificate. This is the same
/// shape as the relay's `HttpPolicy`, narrowed to the one host class that needs
/// the exception.
pub fn normalize_pds(base: &str) -> Result<String, Error> {
    let base = base.trim().trim_end_matches('/').to_string();
    if base.starts_with("https://") {
        return Ok(base);
    }
    if let Some(rest) = base.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = host.eq_ignore_ascii_case("localhost")
            || bare.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false);
        if loopback {
            return Ok(base);
        }
        return Err(Error::Invalid(
            "a PDS must be HTTPS (plain HTTP is allowed on loopback only)".to_string(),
        ));
    }
    Err(Error::Invalid(
        "a PDS address must start with https://".to_string(),
    ))
}

/// A strictly-validated SOCKS proxy.
///
/// Deliberately strict, for the reason the relay path documents: `reqwest`
/// accepts a foreign scheme without complaint and then silently matches no
/// requests, which would mean direct traffic the caller believed was Tor'd.
fn socks_proxy(value: &str) -> Result<reqwest::Proxy, Error> {
    let url = reqwest::Url::parse(value)
        .map_err(|e| Error::Proxy(format!("{value:?} is not a URL: {e}")))?;
    if !matches!(url.scheme(), "socks5" | "socks5h") || url.host_str().is_none() {
        return Err(Error::Proxy(format!(
            "expected socks5h://host:port (e.g. a local Tor), got {value:?}"
        )));
    }
    reqwest::Proxy::all(url).map_err(|e| Error::Proxy(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pds_must_be_https_except_on_loopback() {
        assert_eq!(
            normalize_pds("https://bsky.social/").unwrap(),
            "https://bsky.social"
        );
        assert!(normalize_pds("http://127.0.0.1:9876").is_ok());
        assert!(normalize_pds("http://localhost:9876").is_ok());
        for bad in [
            "http://pds.example",
            "http://10.0.0.5:9876",
            "bsky.social",
            "ftp://pds.example",
            // A name that merely starts with a loopback literal must not pass.
            "http://127.0.0.1.example.com",
        ] {
            assert!(normalize_pds(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn a_bad_proxy_fails_at_construction_not_at_the_first_request() {
        assert!(Xrpc::new(Some("socks5h://127.0.0.1:9050")).is_ok());
        // `reqwest` would accept these and then match nothing, i.e. go direct.
        for bad in ["http://proxy.invalid:8080", "not a url", "socks5://"] {
            assert!(
                Xrpc::new(Some(bad)).is_err(),
                "{bad:?} must not produce a client"
            );
        }
        assert!(Xrpc::new(None).is_ok(), "no proxy is a valid client");
    }

    #[test]
    fn a_did_that_is_neither_plc_nor_web_is_refused() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = Xrpc::new(None).unwrap();
        let err = rt
            .block_on(client.resolve_pds("did:example:abc"))
            .unwrap_err();
        assert!(err.to_string().contains("did:plc or did:web"), "{err}");
    }
}

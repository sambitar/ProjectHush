//! The public publishing lane: a Hush zine out to the open social web, and the
//! open social web back in.
//!
//! This crate is the whole of Hush's relationship with a public network. It
//! speaks **AT Protocol** — the data model behind Bluesky and the surrounding
//! "ATmosphere" — and it speaks one dialect of it: **Standard.site**, the shared
//! long-form lexicons that `pckt.blog`, `leaflet.pub` and `offprint.app` agreed
//! on. A post written here is readable by every other client in that ecosystem
//! and their posts are readable here, with no Hush-operated server in the path.
//!
//! Four rules shape it, and they are structural rather than policy:
//!
//! - **The member owns the identity.** There is no key minted here and nothing
//!   to lose. The identity is a DID on a PDS the member already has, and Hush is
//!   a guest holding a revocable, scoped app password. Revoke it from the
//!   member's ATProto account settings and this vault has nothing left to leak.
//! - **Nothing private can leave.** [`Post`] has fields for a title, a body,
//!   tags and a date, and nowhere to put a pen name, a read key, a comment
//!   thread or a revision chain. Stripping a zine on the way out is not a step
//!   that can be forgotten.
//! - **The lane is curated, never a firehose.** Reads go to the repos the member
//!   asked for ([`AtprotoState::follows`]), one PDS at a time. There is no
//!   subscription to a global stream and no AppView in the path, which is what
//!   keeps this app from holding the social graph it refuses to hold.
//! - **It leaves by the same door as the rest of the app.** [`Xrpc::new`] is
//!   the only constructor the daemon builds, and the daemon hands it the SOCKS
//!   proxy when the member has the relay on Tor and nothing when they do not:
//!   one switch, the same one the relay and the wallet already answer to, rather
//!   than a second place to decide.
//!
//! The crate is protocol and nothing else. What a zine *is* belongs to the
//! daemon and `hush-core`; the crate is handed a [`Post`] and hands back a
//! record. [`refresh`] is the read half: ask each follow's PDS for its
//! documents, cache them, and report what changed.

mod error;
mod explore;
pub mod html;
mod session;
mod standard;
mod store;
mod xrpc;

pub use error::Error;
pub use explore::{explore, ExploreReport, PublicationSummary, CRAWL_CAP, SEEDS};
pub use session::{Connection, Publication, Session, CONNECTION_FILE, PUBLICATION_RKEY};
pub use standard::{
    did_of, document_uri, parse_rfc3339_secs, publication_record, publication_uri, resolve_link,
    rfc3339, BskyPostRef, Document, Post, DOCUMENT_COLLECTION, PUBLICATION_COLLECTION,
};
pub use store::{AtprotoState, Follow, DIRECTORY_CAP, DOCUMENTS_CAP, FOLLOWS_CAP, STATE_FILE};
pub use xrpc::{Listed, RecordRef, Xrpc, DEFAULT_PLC_DIRECTORY};

/// How many documents one refresh asks each follow for.
///
/// A reading surface, not an archive: the newest this many from a publication
/// is what fills a page, and the number bounds the request count so a lane with
/// a full follow list cannot ask for an unbounded corpus on every poll.
pub const REFRESH_LIMIT: u32 = 50;

/// What one refresh pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Documents that were new or had changed.
    pub documents: usize,
    /// Follows that could not be read, each with its reason.
    ///
    /// Reported rather than raised: one publication whose PDS is down must not
    /// cost the member every other page in the lane. A thread that stops
    /// updating has a cause, and this is where it is said out loud.
    pub failed: Vec<String>,
}

/// One refresh pass: read every followed publication's documents and merge them
/// into the cache.
///
/// An empty follow list is a no-op, not an error — a member who has cleared the
/// list is asking for nothing, not reporting a fault.
pub async fn refresh(xrpc: &Xrpc, state: &mut AtprotoState, limit: u32) -> RefreshReport {
    let follows = state.follows().to_vec();
    if follows.is_empty() {
        return RefreshReport::default();
    }
    let mut incoming = Vec::new();
    let mut failed = Vec::new();
    for follow in &follows {
        match fetch_follow(xrpc, follow, limit).await {
            Ok(mut documents) => incoming.append(&mut documents),
            Err(message) => failed.push(format!("{}: {message}", follow.label())),
        }
    }
    RefreshReport {
        documents: state.absorb(incoming),
        failed,
    }
}

/// Read one publication's documents from its own PDS.
///
/// The DID is resolved to a PDS here rather than stored, so a publication that
/// moves hosts keeps working: the DID is the stable half and the host is not.
///
/// The account's publication records are read in the same pass, because a
/// document may be a **pointer** rather than a body and only the publication
/// knows where it points: the document carries a `path` and the publication
/// carries the `url` the path hangs off. One extra request per follow buys the
/// difference between a post that reads and a post that is a bare title.
async fn fetch_follow(xrpc: &Xrpc, follow: &Follow, limit: u32) -> Result<Vec<Document>, Error> {
    let pds = xrpc.resolve_pds(&follow.did).await?;
    let listed = xrpc
        .list_records(&pds, &follow.did, DOCUMENT_COLLECTION, limit)
        .await?;
    // Not a failure if this one does not come back: the documents still read,
    // they just cannot be linked, and a missing link must not cost the page.
    let publications = match xrpc
        .list_records(&pds, &follow.did, PUBLICATION_COLLECTION, PUBLICATION_LIMIT)
        .await
    {
        Ok(records) => publication_urls(&records),
        Err(_) => std::collections::HashMap::new(),
    };
    Ok(listed
        .into_iter()
        .filter_map(|item| {
            // A record another client wrote is not this crate's to validate; one
            // that does not map is skipped, and the rest of the page stands.
            let mut document = Document::from_record(&item.uri, &item.value)?;
            document.handle = follow.handle.clone();
            resolve_link(&mut document, &publications);
            Some(document)
        })
        .collect())
}

/// How many publications one account may be asked for.
///
/// An account publishing to more than a handful of publications is not a shape
/// the ecosystem has, and the bound keeps a hostile or broken repo from turning
/// one follow into an unbounded request.
const PUBLICATION_LIMIT: u32 = 50;

/// Map each publication's `at://` address to the site it lives at.
fn publication_urls(
    records: &[Listed],
) -> std::collections::HashMap<String, String> {
    records
        .iter()
        .filter_map(|item| {
            let url = item.value.get("url")?.as_str()?.trim();
            if url.is_empty() {
                return None;
            }
            Some((item.uri.trim().to_string(), url.to_string()))
        })
        .collect()
}

/// Publish one post under a publication.
///
/// Answers with the record's address and the session as it now stands: a write
/// may have rotated the tokens, and the caller owns the stored session.
pub async fn publish(
    xrpc: &Xrpc,
    session: &Session,
    publication_uri: &str,
    post: &Post,
) -> Result<(RecordRef, Session), Error> {
    let record = post.record(publication_uri)?;
    xrpc.put_record(session, DOCUMENT_COLLECTION, &post.rkey, &record)
        .await
}

/// Write (or replace) this member's publication record.
///
/// Written at a fixed record key, so running setup again replaces the blog
/// rather than starting a second one beside it.
pub async fn ensure_publication(
    xrpc: &Xrpc,
    session: &Session,
    name: &str,
    url: &str,
) -> Result<(Publication, Session), Error> {
    let record = publication_record(name, url)?;
    let (reference, session) = xrpc
        .put_record(session, PUBLICATION_COLLECTION, PUBLICATION_RKEY, &record)
        .await?;
    Ok((
        Publication {
            uri: reference.uri,
            name: name.trim().to_string(),
            url: url.trim().trim_end_matches('/').to_string(),
        },
        session,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// A stub PDS that also answers as the PLC directory, so the whole lane runs
    /// over real HTTP through the real client. A trait standing in for the
    /// transport would test the pipeline's logic and none of its wiring.
    struct Stub {
        /// Repo → its documents, as `listRecords` would return them.
        repos: HashMap<String, Vec<serde_json::Value>>,
        /// A PDS endpoint to answer for a DID, when it is not this stub itself.
        foreign: HashMap<String, String>,
        /// This stub's own base, filled in once the listener is bound — the DID
        /// document has to name the address the stub actually got.
        self_base: String,
        /// Whether the next stale-token write answers 401 once, to exercise the
        /// refresh-once path.
        expire_once: AtomicBool,
        /// Every `Authorization` header a write arrived with, in order.
        seen_auth: Mutex<Vec<String>>,
    }

    async fn plc(State(stub): State<Arc<Stub>>, Path(did): Path<String>) -> Json<serde_json::Value> {
        let endpoint = stub.foreign.get(&did).cloned().unwrap_or_else(|| stub.self_base.clone());
        Json(serde_json::json!({
            "id": did,
            "service": [{ "id": "#atproto_pds", "serviceEndpoint": endpoint }],
        }))
    }

    async fn list_records(
        State(stub): State<Arc<Stub>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let repo = query.get("repo").cloned().unwrap_or_default();
        let records: Vec<serde_json::Value> = stub
            .repos
            .get(&repo)
            .map(|docs| {
                docs.iter()
                    .enumerate()
                    .map(|(i, value)| {
                        serde_json::json!({
                            "uri": format!("at://{repo}/site.standard.document/{i}"),
                            "cid": "bafy",
                            "value": value,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Json(serde_json::json!({ "records": records }))
    }

    async fn resolve_handle(
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let handle = query.get("handle").cloned().unwrap_or_default();
        Json(serde_json::json!({ "did": format!("did:plc:{handle}") }))
    }

    async fn create_session() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "accessJwt": "access-1",
            "refreshJwt": "refresh-1",
            "did": "did:plc:writer",
            "handle": "ada.test",
        }))
    }

    async fn refresh_session() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "accessJwt": "access-2",
            "refreshJwt": "refresh-2",
            "did": "did:plc:writer",
            "handle": "ada.test",
        }))
    }

    async fn put_record(
        State(stub): State<Arc<Stub>>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> Result<Json<serde_json::Value>, StatusCode> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        stub.seen_auth.lock().unwrap().push(auth.clone());

        // The first write with the stale token is refused, so the client has to
        // rotate and retry. A second stale write would be a real failure.
        if auth == "Bearer access-1" && stub.expire_once.swap(false, Ordering::SeqCst) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let collection = body["collection"].as_str().unwrap_or_default();
        let rkey = body["rkey"].as_str().unwrap_or_default();
        let repo = body["repo"].as_str().unwrap_or_default();
        Ok(Json(serde_json::json!({
            "uri": format!("at://{repo}/{collection}/{rkey}"),
            "cid": "bafyrecord",
        })))
    }

    /// Bind a stub PDS, serve it, and return a client pointed at it plus the
    /// write headers it has seen.
    async fn spawn(
        repos: HashMap<String, Vec<serde_json::Value>>,
        foreign: HashMap<String, String>,
        expire_once: bool,
    ) -> (Xrpc, Arc<Stub>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stub = Arc::new(Stub {
            repos,
            foreign,
            self_base: base.clone(),
            expire_once: AtomicBool::new(expire_once),
            seen_auth: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/xrpc/com.atproto.server.createSession", post(create_session))
            .route(
                "/xrpc/com.atproto.server.refreshSession",
                post(refresh_session),
            )
            .route("/xrpc/com.atproto.repo.putRecord", post(put_record))
            .route("/xrpc/com.atproto.repo.listRecords", get(list_records))
            .route(
                "/xrpc/com.atproto.identity.resolveHandle",
                get(resolve_handle),
            )
            .route("/{did}", get(plc))
            .with_state(stub.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = Xrpc::with_plc(None, &base).unwrap();
        (client, stub)
    }

    fn doc(title: &str, secs: u64) -> serde_json::Value {
        serde_json::json!({
            "title": title,
            "textContent": "The body.",
            "tags": ["rust"],
            "publishedAt": rfc3339(secs * 1000),
            "path": "/p",
        })
    }

    fn follow(did: &str, handle: &str) -> Follow {
        Follow {
            did: did.into(),
            handle: handle.into(),
            name: String::new(),
        }
    }

    fn writer(pds: &str) -> Session {
        Session {
            did: "did:plc:writer".into(),
            handle: "ada.test".into(),
            pds: pds.to_string(),
            password: "app-pw".into(),
            access_jwt: "access-1".into(),
            refresh_jwt: "refresh-1".into(),
        }
    }

    #[tokio::test]
    async fn refresh_reads_each_followed_repo_and_caches_them() {
        let mut repos = HashMap::new();
        repos.insert("did:plc:one".to_string(), vec![doc("One", 100)]);
        repos.insert(
            "did:plc:two".to_string(),
            vec![doc("Two", 200), doc("Three", 50)],
        );
        let (client, _) = spawn(repos, HashMap::new(), false).await;

        let mut state = AtprotoState::default();
        state
            .set_follows(vec![follow("did:plc:one", "one.test"), follow("did:plc:two", "two.test")])
            .unwrap();

        let report = refresh(&client, &mut state, REFRESH_LIMIT).await;
        assert_eq!(report.documents, 3);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let titles: Vec<&str> = state
            .index(None, None)
            .iter()
            .map(|d| d.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Two", "One", "Three"], "newest first across follows");
        // The handle comes from the follow, because a record does not carry one.
        assert_eq!(state.index(None, Some("did:plc:two"))[0].handle, "two.test");

        // A second pass over unchanged repos is not a change.
        assert_eq!(refresh(&client, &mut state, REFRESH_LIMIT).await.documents, 0);
    }

    #[tokio::test]
    async fn one_unreachable_publication_does_not_cost_the_rest() {
        let mut repos = HashMap::new();
        repos.insert("did:plc:good".to_string(), vec![doc("Good", 100)]);
        let mut foreign = HashMap::new();
        // A PDS nothing is listening on: reading this follow must fail alone.
        foreign.insert("did:plc:down".to_string(), "http://127.0.0.1:1".to_string());
        let (client, _) = spawn(repos, foreign, false).await;

        let mut state = AtprotoState::default();
        state
            .set_follows(vec![
                follow("did:plc:down", "down.test"),
                follow("did:plc:good", "good.test"),
            ])
            .unwrap();

        let report = refresh(&client, &mut state, REFRESH_LIMIT).await;
        assert_eq!(report.documents, 1, "the reachable publication still read");
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].starts_with("down.test: "), "{:?}", report.failed);
        assert_eq!(state.index(None, None).len(), 1);
    }

    #[tokio::test]
    async fn an_empty_follow_list_reads_nothing_and_is_not_an_error() {
        let (client, _) = spawn(HashMap::new(), HashMap::new(), false).await;
        let mut state = AtprotoState::default();
        assert_eq!(
            refresh(&client, &mut state, REFRESH_LIMIT).await,
            RefreshReport::default()
        );
    }

    #[tokio::test]
    async fn an_unreachable_pds_is_reported_as_transport_not_as_a_refusal() {
        let (client, _) = spawn(HashMap::new(), HashMap::new(), false).await;
        let error = client
            .create_session("http://127.0.0.1:1", "ada.test", "app-pw")
            .await
            .expect_err("nothing is listening on port 1");
        assert!(error.to_string().contains("could not reach"), "{error}");
    }

    #[tokio::test]
    async fn a_publish_writes_a_record_and_rotates_an_expired_token() {
        let (client, stub) = spawn(HashMap::new(), HashMap::new(), true).await;
        let session = writer(&stub.self_base);
        let post = Post {
            rkey: Post::rkey_for("zine-1"),
            title: "Hello".into(),
            text: "Body".into(),
            description: None,
            tags: vec!["rust".into()],
            published_at_ms: 1_700_000_000_000,
            updated_at_ms: None,
        };
        let (reference, rotated) = publish(
            &client,
            &session,
            "at://did:plc:writer/site.standard.publication/self",
            &post,
        )
        .await
        .unwrap();

        assert_eq!(reference.cid, "bafyrecord");
        assert!(reference.uri.ends_with(&post.rkey));
        assert_eq!(
            rotated.access_jwt, "access-2",
            "the caller is handed the rotated session"
        );
        let auths = stub.seen_auth.lock().unwrap().clone();
        assert_eq!(
            auths,
            vec!["Bearer access-1".to_string(), "Bearer access-2".to_string()],
            "the write is retried exactly once, with the fresh token"
        );
    }

    #[tokio::test]
    async fn a_handle_resolves_to_a_did_and_a_did_is_passed_through() {
        let (client, stub) = spawn(HashMap::new(), HashMap::new(), false).await;
        let did = client
            .resolve_handle(&stub.self_base, "@ada.test")
            .await
            .unwrap();
        assert_eq!(did, "did:plc:ada.test", "the `@` is a display habit, not a name");

        // A DID is already the answer, so it is not asked about — a request to a
        // host nothing is listening on would fail if it were.
        assert_eq!(
            client
                .resolve_handle("http://127.0.0.1:1", "did:plc:known")
                .await
                .unwrap(),
            "did:plc:known"
        );
        assert!(client.resolve_handle(&stub.self_base, "  ").await.is_err());
    }

    #[tokio::test]
    async fn writing_a_publication_at_a_fixed_key_replaces_rather_than_adds() {
        let (client, stub) = spawn(HashMap::new(), HashMap::new(), false).await;
        let session = writer(&stub.self_base);
        let (publication, _) =
            ensure_publication(&client, &session, "Ada's zine", "https://ada.example/")
                .await
                .unwrap();
        assert_eq!(
            publication.uri,
            "at://did:plc:writer/site.standard.publication/self",
            "one publication, at a fixed key"
        );
        assert_eq!(publication.url, "https://ada.example", "the url is normalized");
    }
}

//! Discovery: what else is out there, found without asking anyone's permission.
//!
//! The read lane is curated — a member reads what they named, and nothing else.
//! That is the right shape for a reader who knows what they want, and it leaves
//! a member who has just installed the app with an empty lane and no way to fill
//! it. Nothing in Hush suggests anything, so "explore" had no first step.
//!
//! This module is that first step. It turns a handful of **seed** accounts into a
//! browsable directory of publications by walking **one hop** of the follow
//! graph and keeping the accounts that actually publish.
//!
//! Three properties make it fit this crate rather than fight it:
//!
//! - **No account.** Every read is a public read. A member who will never sign up
//!   can still find something to read, which is the whole point.
//! - **No AppView.** The follow list is `app.bsky.graph.follow` records in the
//!   account's own repo, and the handle comes from `describeRepo` on the PDS that
//!   serves it. Nothing here asks an index who anyone is, so the lane keeps its
//!   "pull-only, per-repo" property all the way through discovery.
//! - **One hop, bounded.** It reads who the seeds follow, checks which of those
//!   publish, and stops. It is a crawl, not a graph traversal, and it does not
//!   pretend to be an index. Two hops would find far more and cost far more; the
//!   ceiling is stated rather than discovered by watching it time out.

use std::collections::HashSet;

use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::standard::PUBLICATION_COLLECTION;
use crate::xrpc::{Listed, Xrpc};

/// The accounts a crawl starts from.
///
/// DIDs rather than handles, deliberately. A DID needs no resolver, so a crawl
/// cannot be stopped by a handle that will not resolve or by a member having no
/// session to resolve it with — and a handle would have to be re-resolved on
/// every crawl, so a seed that changed its name would silently vanish from the
/// directory. The comments name the account, because a DID does not.
///
/// This list is a **curation decision**, not a technical one. Whoever ships Hush
/// decides who is reachable at all, because one hop from a seed only ever finds
/// that seed's neighbourhood. It is short on purpose and meant to be argued
/// about.
pub const SEEDS: &[&str] = &[
    "did:plc:btxrwcaeyodrap5mnjw2fvmz", // leaflet.pub
    "did:plc:revjuqmkvrw6fnkxppqtszpv", // pckt.blog
];

/// The follow record, read straight out of the repo that made it.
const FOLLOW_COLLECTION: &str = "app.bsky.graph.follow";

/// How many follows are read from each seed.
///
/// A seed with a thousand follows is a well-connected account, and reading all
/// of them would make one crawl cost a thousand probes. The newest are taken,
/// which is the same choice the read lane makes and for the same reason.
pub const FOLLOW_LIMIT: u32 = 50;

/// The ceiling on how many accounts one crawl will probe.
///
/// Probes are the expensive half: each is a PLC lookup, a `describeRepo` and a
/// `listRecords`. This bounds a crawl that started from unusually social seeds.
pub const CRAWL_CAP: usize = 120;

/// How many publications are listed per account.
pub const PUBLICATION_LIMIT: u32 = 20;

/// How many probes are in flight at once.
///
/// Measured: 55 accounts sequentially took 32 seconds, which is too long to sit
/// behind a button. Concurrency is bounded rather than unbounded because this
/// runs against strangers' servers, and a crawl that opens 120 sockets at once
/// is a small denial of service aimed at whoever the seeds follow.
const PROBE_CONCURRENCY: usize = 8;

/// One publication the crawl found.
///
/// Everything here comes from the publication's own record or its own PDS. The
/// name, the description and the URL are the author's words, unvalidated, and
/// the icon is a blob reference that is deliberately not fetched: discovery must
/// not become a reason to make a stranger's server serve us a hundred images.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationSummary {
    /// The `site.standard.publication` record's `at://` address. The identity.
    pub uri: String,
    /// The account that hosts it. What a follow would be keyed by.
    pub did: String,
    /// The handle that account publishes, read from its own PDS. Display only,
    /// and empty when the PDS would not say.
    #[serde(default)]
    pub handle: String,
    /// The publication's own name, as the author set it. Never empty: a
    /// publication without one is not listed, because a row with no name is not
    /// something a member can choose between.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The publication's public URL, when it declares one.
    #[serde(default)]
    pub url: String,
    /// Whether the record carries an icon blob. The blob itself is not fetched.
    #[serde(default)]
    pub has_icon: bool,
    /// The seed this was reached through, by handle where one was readable.
    ///
    /// Provenance rather than decoration: every entry in a directory came from
    /// somewhere specific, and a crawl that showed no trace of its origin would
    /// make two seeds' very different neighbourhoods look like one unbiased
    /// list.
    #[serde(default)]
    pub via: String,
}

/// What one crawl found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExploreReport {
    pub found: Vec<PublicationSummary>,
    /// Accounts that could not be probed, each with its reason.
    ///
    /// Reported rather than raised, like a refresh: one account whose PDS is
    /// down must not cost the member every other publication in the directory.
    pub failed: Vec<String>,
}

/// One crawl: walk a hop from each seed and return every publication found.
///
/// An empty seed list finds nothing and is not an error — the same shape as an
/// empty follow list, because a caller asking for nothing is not reporting a
/// fault.
pub async fn explore(xrpc: &Xrpc, seeds: &[&str]) -> ExploreReport {
    let mut report = ExploreReport::default();
    let candidates = gather(xrpc, seeds, &mut report.failed).await;
    if candidates.is_empty() {
        return report;
    }

    let probed = stream::iter(candidates.into_iter().map(|(did, via)| async move {
        probe(xrpc, &did, &via).await
    }))
    .buffer_unordered(PROBE_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    for (found, failure) in probed {
        report.found.extend(found);
        if let Some(failure) = failure {
            report.failed.push(failure);
        }
    }
    // Sorted here rather than by the caller so the directory has one order
    // wherever it is shown, and so a re-crawl that found the same things does
    // not shuffle the list a member is looking at.
    report.found.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.uri.cmp(&b.uri))
    });
    report
}

/// The accounts worth probing: each seed, plus everyone the seeds follow.
///
/// The seed is a candidate as well as a starting point, because the accounts
/// worth starting from are usually publications themselves.
async fn gather(xrpc: &Xrpc, seeds: &[&str], failed: &mut Vec<String>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for seed in seeds {
        let seed = seed.trim();
        if seed.is_empty() || !seen.insert(seed.to_string()) {
            continue;
        }
        // Best effort: the label is for a directory row, and failing to read it
        // must not cost the seed its place in the crawl.
        let label = match xrpc.resolve_pds(seed).await {
            Ok(pds) => xrpc
                .describe_repo(&pds, seed)
                .await
                .unwrap_or_else(|_| seed.to_string()),
            Err(_) => seed.to_string(),
        };
        let label = if label.trim().is_empty() {
            seed.to_string()
        } else {
            label
        };

        out.push((seed.to_string(), label.clone()));
        match follows_of(xrpc, seed).await {
            Ok(subjects) => {
                for subject in subjects {
                    if seen.insert(subject.clone()) {
                        out.push((subject, label.clone()));
                    }
                }
            }
            Err(message) => failed.push(format!("{label}: {message}")),
        }
    }

    out.truncate(CRAWL_CAP);
    out
}

/// Who one account follows, read from that account's own repo.
async fn follows_of(xrpc: &Xrpc, did: &str) -> Result<Vec<String>, Error> {
    let pds = xrpc.resolve_pds(did).await?;
    let listed = xrpc
        .list_records(&pds, did, FOLLOW_COLLECTION, FOLLOW_LIMIT)
        .await?;
    Ok(listed
        .into_iter()
        .filter_map(|item| follow_subject(&item.value))
        .collect())
}

/// Every publication one account hosts, or the reason it could not be read.
async fn probe(
    xrpc: &Xrpc,
    did: &str,
    via: &str,
) -> (Vec<PublicationSummary>, Option<String>) {
    let pds = match xrpc.resolve_pds(did).await {
        Ok(pds) => pds,
        Err(e) => return (Vec::new(), Some(format!("{did}: {e}"))),
    };

    // Display only, so a refusal is not a failure: a repo whose handle will not
    // read still has publications worth listing, and the row falls back to the
    // name the publication itself declares.
    let handle = xrpc.describe_repo(&pds, did).await.unwrap_or_default();

    match xrpc
        .list_records(&pds, did, PUBLICATION_COLLECTION, PUBLICATION_LIMIT)
        .await
    {
        Ok(listed) => (
            listed
                .into_iter()
                .filter_map(|item| summary_of(&item, did, &handle, via))
                .collect(),
            None,
        ),
        // Not an error worth reporting: most accounts in the follow graph of a
        // publication are readers, and a reader publishing nothing is the
        // expected case rather than a fault.
        Err(_) => (Vec::new(), None),
    }
}

/// Map one listed record to a directory entry, or `None` if it is not one.
fn summary_of(item: &Listed, did: &str, handle: &str, via: &str) -> Option<PublicationSummary> {
    let name = item.value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(PublicationSummary {
        uri: item.uri.clone(),
        did: did.to_string(),
        handle: handle.trim().to_string(),
        name,
        description: text_of(&item.value, "description"),
        url: text_of(&item.value, "url"),
        has_icon: item
            .value
            .get("icon")
            .map(|v| !v.is_null())
            .unwrap_or(false),
        via: via.trim().to_string(),
    })
}

/// The DID a follow record points at.
///
/// A follow whose subject is not a DID is skipped rather than carried: the
/// subject is what every later lookup is keyed by, so one that addresses nothing
/// would become a probe that could only fail.
fn follow_subject(value: &serde_json::Value) -> Option<String> {
    let subject = value.get("subject")?.as_str()?.trim();
    if subject.starts_with("did:") {
        Some(subject.to_string())
    } else {
        None
    }
}

/// A trimmed string field, or empty when absent or not a string.
fn text_of(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query, State};
    use axum::routing::get;
    use axum::{Json, Router};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A stub PDS that answers the three calls a crawl makes, with the
    /// collection actually honoured — the read lane's stub returns documents
    /// whatever it is asked for, which would make every account here look like a
    /// publication.
    struct Stub {
        self_base: String,
        foreign: HashMap<String, String>,
        /// repo -> collection -> records
        repos: HashMap<String, HashMap<String, Vec<serde_json::Value>>>,
        handles: HashMap<String, String>,
    }

    async fn plc(State(stub): State<Arc<Stub>>, Path(did): Path<String>) -> Json<serde_json::Value> {
        let endpoint = stub
            .foreign
            .get(&did)
            .cloned()
            .unwrap_or_else(|| stub.self_base.clone());
        Json(serde_json::json!({
            "id": did,
            "service": [{ "id": "#atproto_pds", "serviceEndpoint": endpoint }],
        }))
    }

    async fn describe_repo(
        State(stub): State<Arc<Stub>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let repo = query.get("repo").cloned().unwrap_or_default();
        Json(serde_json::json!({
            "did": repo,
            "handle": stub.handles.get(&repo).cloned().unwrap_or_default(),
        }))
    }

    async fn list_records(
        State(stub): State<Arc<Stub>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let repo = query.get("repo").cloned().unwrap_or_default();
        let collection = query.get("collection").cloned().unwrap_or_default();
        let records: Vec<serde_json::Value> = stub
            .repos
            .get(&repo)
            .and_then(|by_collection| by_collection.get(&collection))
            .map(|values| {
                values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        serde_json::json!({
                            "uri": format!("at://{repo}/{collection}/{i}"),
                            "cid": "bafy",
                            "value": value,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Json(serde_json::json!({ "records": records }))
    }

    async fn spawn(stub: Stub) -> Xrpc {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stub = Arc::new(Stub {
            self_base: base.clone(),
            ..stub
        });
        let router = Router::new()
            .route("/xrpc/com.atproto.repo.listRecords", get(list_records))
            .route("/xrpc/com.atproto.repo.describeRepo", get(describe_repo))
            .route("/{did}", get(plc))
            .with_state(stub);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Xrpc::with_plc(None, &base).unwrap()
    }

    fn publication(name: &str, url: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "url": url,
            "description": format!("{name} description"),
            "icon": { "ref": { "$link": "bafyicon" } },
        })
    }

    fn follows(subjects: &[&str]) -> serde_json::Value {
        // One record per subject is not the shape; the collection holds many
        // records, so the helper returns the first and the caller builds a list.
        serde_json::json!({ "subject": subjects[0] })
    }

    #[tokio::test]
    async fn a_seed_and_the_publishers_it_follows_become_a_directory() {
        let mut repos = HashMap::new();
        repos.insert(
            "did:plc:seed".to_string(),
            HashMap::from([
                (
                    FOLLOW_COLLECTION.to_string(),
                    vec![follows(&["did:plc:writer"]), follows(&["did:plc:reader"])],
                ),
                (
                    PUBLICATION_COLLECTION.to_string(),
                    vec![publication("The Seed Notes", "https://seed.example")],
                ),
            ]),
        );
        repos.insert(
            "did:plc:writer".to_string(),
            HashMap::from([(
                PUBLICATION_COLLECTION.to_string(),
                vec![publication("Ada Writes", "https://ada.example")],
            )]),
        );
        // A reader publishes nothing, which is the expected case, not a fault.
        repos.insert("did:plc:reader".to_string(), HashMap::new());

        let xrpc = spawn(Stub {
            self_base: String::new(),
            foreign: HashMap::new(),
            repos,
            handles: HashMap::from([
                ("did:plc:seed".to_string(), "seed.example".to_string()),
                ("did:plc:writer".to_string(), "ada.example".to_string()),
                ("did:plc:reader".to_string(), "reader.example".to_string()),
            ]),
        })
        .await;

        let report = explore(&xrpc, &["did:plc:seed"]).await;
        let names: Vec<&str> = report.found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Ada Writes", "The Seed Notes"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let ada = report.found.iter().find(|p| p.name == "Ada Writes").unwrap();
        assert_eq!(ada.did, "did:plc:writer");
        assert_eq!(ada.handle, "ada.example");
        assert_eq!(ada.url, "https://ada.example");
        assert!(ada.has_icon);
        // Provenance: the row says which seed it came from.
        assert_eq!(ada.via, "seed.example");
    }

    #[tokio::test]
    async fn an_account_that_publishes_nothing_is_not_a_directory_row() {
        let mut repos = HashMap::new();
        repos.insert(
            "did:plc:seed".to_string(),
            HashMap::from([(
                FOLLOW_COLLECTION.to_string(),
                vec![follows(&["did:plc:reader"])],
            )]),
        );
        repos.insert("did:plc:reader".to_string(), HashMap::new());

        let xrpc = spawn(Stub {
            self_base: String::new(),
            foreign: HashMap::new(),
            repos,
            handles: HashMap::new(),
        })
        .await;

        let report = explore(&xrpc, &["did:plc:seed"]).await;
        assert!(report.found.is_empty(), "{:?}", report.found);
        // And the silence is not reported as a failure: most accounts in a
        // publication's follow graph are readers.
        assert!(report.failed.is_empty(), "{:?}", report.failed);
    }

    #[tokio::test]
    async fn an_empty_seed_list_finds_nothing_and_is_not_an_error() {
        let xrpc = spawn(Stub {
            self_base: String::new(),
            foreign: HashMap::new(),
            repos: HashMap::new(),
            handles: HashMap::new(),
        })
        .await;
        let report = explore(&xrpc, &[]).await;
        assert_eq!(report, ExploreReport::default());
    }

    #[tokio::test]
    async fn a_publication_with_no_name_is_skipped() {
        let mut repos = HashMap::new();
        repos.insert(
            "did:plc:seed".to_string(),
            HashMap::from([(
                PUBLICATION_COLLECTION.to_string(),
                vec![
                    serde_json::json!({ "name": "  ", "url": "https://x.example" }),
                    publication("Named", "https://y.example"),
                ],
            )]),
        );
        let xrpc = spawn(Stub {
            self_base: String::new(),
            foreign: HashMap::new(),
            repos,
            handles: HashMap::new(),
        })
        .await;
        let report = explore(&xrpc, &["did:plc:seed"]).await;
        let names: Vec<&str> = report.found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Named"], "a row with no name is not choosable");
    }

    #[test]
    fn a_follow_whose_subject_is_not_a_did_is_skipped() {
        assert_eq!(
            follow_subject(&serde_json::json!({ "subject": "did:plc:abc" })),
            Some("did:plc:abc".to_string())
        );
        assert_eq!(follow_subject(&serde_json::json!({ "subject": "ada.test" })), None);
        assert_eq!(follow_subject(&serde_json::json!({})), None);
    }

    #[test]
    fn the_shipped_seeds_are_dids_so_the_crawl_needs_no_resolver() {
        for seed in SEEDS {
            assert!(seed.starts_with("did:plc:"), "{seed} is not a DID");
        }
    }
}

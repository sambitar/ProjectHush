//! The Nostr-facing crate: the **neighborhoods** read lane (Task 22) and, later,
//! the zine **export** bridge (Task 11).
//!
//! The lane is a pipeline. [`nip23`] maps a raw event to an [`Article`], [`Gate`]
//! decides which of those the page will show, and [`NeighborhoodsState`] dedupes
//! them into the cache and serves the index. [`nip22`] is the same for the
//! comments under an article: mapped by [`to_comment`], cached flat, and read
//! back a thread at a time. [`RelayClient`] feeds both: the one module here that
//! touches the network, and the only place the daemon's bytes leave for a public
//! network. It sits behind [`RelaySource`], so the pipeline is testable without a
//! relay and the transport can change underneath it.
//!
//! Two rules are structural rather than policy. Only `kind:30023` becomes an
//! article and only `kind:1111` becomes a comment, and the filters can only ever
//! ask for those two, so a short note cannot enter the lane by accident. And the
//! transport reads over Tor or not at all: [`RelayClient::via_tor`] is the only
//! constructor the daemon builds.

mod event;
mod export;
mod gate;
mod nip22;
mod nip23;
mod relay;
mod socks;
mod source;
mod store;

pub use event::{event_id, naddr, Event, UnsignedEvent};
pub use export::{summary_of, ExportPlan};
pub use gate::{reading_minutes, Gate};
pub use nip22::{to_comment, Comment, CommentParent, CommentPlan, CommentRoot, KIND_COMMENT};
pub use nip23::{to_article, Article, RawEvent, KIND_LONG_FORM};
pub use relay::RelayClient;
pub use source::{Filter, PublishReport, RelaySink, RelaySource};
pub use store::{
    NeighborhoodsState, Settings, ARTICLES_CAP, DEFAULT_ALLOWLIST, REPLIES_CAP, STATE_FILE,
};

use std::sync::Arc;

/// What one refresh pass changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Addresses that were new or replaced.
    pub articles: usize,
    /// Comments that were new.
    pub replies: usize,
    /// Why the reply pass failed, when it did.
    ///
    /// The articles were merged either way. A relay refusing a comment query is
    /// not a reason to tell the member their page could not be read, but it is
    /// also not something to swallow: a thread that stops updating has a cause,
    /// and this is it.
    pub replies_error: Option<String>,
}

/// One refresh pass: ask the allowlisted relays for long-form articles, keep
/// only what the gate admits, and merge it into the cache by address. Then, for
/// each address in `reply_scopes`, read the comments filed under it.
///
/// `reply_scopes` is what this vault has a stake in — the addresses of the
/// articles it exported — so a member who has published nothing makes no second
/// request at all. The reply pass is one request for every thread rather than one
/// per thread, and its failure is reported rather than raised.
///
/// An empty allowlist is a no-op, not an error: a user who has cleared the list
/// is asking for nothing, not reporting a fault.
pub async fn refresh(
    source: &Arc<dyn RelaySource>,
    state: &mut NeighborhoodsState,
    reply_scopes: &[String],
) -> Result<RefreshReport, String> {
    let settings = state.settings().clone();
    if settings.allowlist.is_empty() {
        return Ok(RefreshReport::default());
    }
    let gate = Gate::new(settings.min_minutes);
    let filter = Filter::long_form(REFRESH_LIMIT);
    let raw = source.fetch(&settings.allowlist, &filter).await?;
    let admitted: Vec<Article> = raw.iter().filter_map(|e| gate.admits(e)).collect();
    let articles = state.absorb(admitted);

    if reply_scopes.is_empty() {
        return Ok(RefreshReport {
            articles,
            ..RefreshReport::default()
        });
    }

    let filter = Filter::comments(reply_scopes, REPLIES_LIMIT);
    let (replies, replies_error) = match source.fetch(&settings.allowlist, &filter).await {
        Ok(raw) => {
            // A relay answers what it likes. Only the threads that were asked
            // for are admitted, so a comment about some other article cannot
            // take up room in a cache that will never show it.
            let admitted: Vec<Comment> = raw
                .iter()
                .filter_map(to_comment)
                .filter(|c| reply_scopes.contains(&c.root))
                .collect();
            (state.absorb_replies(admitted), None)
        }
        Err(message) => (0, Some(message)),
    };

    Ok(RefreshReport {
        articles,
        replies,
        replies_error,
    })
}

/// How many events one refresh asks each relay set for. A reading surface, not
/// an archive: the newest this many long-form events are enough to fill a page.
const REFRESH_LIMIT: usize = 200;

/// How many comments one refresh asks for across every thread it reads. Higher
/// than the article limit because a comment is small, and bounded because a
/// popular thread must not be able to grow the request without limit.
const REPLIES_LIMIT: usize = 500;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    /// A relay that returns a fixed batch per kind. The lane's logic is what is
    /// under test; the transport is the part this stands in for.
    struct FakeSource {
        articles: Vec<RawEvent>,
        replies: Vec<RawEvent>,
        /// Every filter the lane actually sent, so a test can assert what was
        /// asked for as well as what came back.
        asked: std::sync::Mutex<Vec<Filter>>,
    }

    impl FakeSource {
        fn new(articles: Vec<RawEvent>, replies: Vec<RawEvent>) -> Arc<Self> {
            Arc::new(Self {
                articles,
                replies,
                asked: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn filters(&self) -> Vec<Filter> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl RelaySource for FakeSource {
        async fn fetch(
            &self,
            _relays: &[String],
            filter: &Filter,
        ) -> Result<Vec<RawEvent>, String> {
            self.asked.lock().unwrap().push(filter.clone());
            if filter.kinds == vec![KIND_LONG_FORM] {
                Ok(self.articles.clone())
            } else {
                Ok(self.replies.clone())
            }
        }
    }

    /// A long body: enough words to clear any reading-time floor under test.
    fn long_body() -> String {
        "word ".repeat(1000)
    }

    fn article_event(pubkey: &str, d: &str, created_at: u64, body: &str) -> RawEvent {
        serde_json::from_value(json!({
            "id": format!("{pubkey}{d}{created_at}"),
            "pubkey": pubkey,
            "created_at": created_at,
            "kind": KIND_LONG_FORM,
            "tags": [["d", d], ["title", "A title"], ["t", "Rust"]],
            "content": body,
        }))
        .unwrap()
    }

    fn comment_event(id: &str, root: &str, parent: Option<&str>, created_at: u64) -> RawEvent {
        let mut tags = vec![json!(["A", root]), json!(["K", KIND_LONG_FORM.to_string()])];
        match parent {
            Some(parent) => {
                tags.push(json!(["e", parent]));
                tags.push(json!(["k", KIND_COMMENT.to_string()]));
            }
            None => tags.push(json!(["k", KIND_LONG_FORM.to_string()])),
        }
        serde_json::from_value(json!({
            "id": id,
            "pubkey": "bb",
            "created_at": created_at,
            "kind": KIND_COMMENT,
            "tags": tags,
            "content": format!("body {id}"),
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn refresh_keeps_long_form_and_drops_everything_else() {
        let keep = article_event("aa", "one", 100, &long_body());
        let note = RawEvent {
            kind: 1,
            ..article_event("aa", "two", 100, &long_body())
        };
        let stub = article_event("aa", "three", 100, "too short");
        let fake = FakeSource::new(vec![keep.clone(), note, stub], vec![]);
        let source: Arc<dyn RelaySource> = fake.clone();

        let mut state = NeighborhoodsState::default();
        let report = refresh(&source, &mut state, &[]).await.unwrap();

        assert_eq!(report.articles, 1, "only the long-form article is admitted");
        assert_eq!(state.articles().len(), 1);
        assert_eq!(state.articles()[0].address(), address_of(&keep));
    }

    #[tokio::test]
    async fn an_empty_allowlist_reads_nothing_and_is_not_an_error() {
        let fake = FakeSource::new(vec![article_event("aa", "one", 100, &long_body())], vec![]);
        let source: Arc<dyn RelaySource> = fake.clone();
        let mut state = NeighborhoodsState::default();
        let mut settings = state.settings().clone();
        settings.allowlist.clear();
        state.set_settings(settings);

        assert_eq!(
            refresh(&source, &mut state, &["30023:aa:one".to_string()])
                .await
                .unwrap(),
            RefreshReport::default()
        );
        assert!(state.articles().is_empty());
        assert!(
            fake.filters().is_empty(),
            "an empty allowlist is no request at all, not even for replies"
        );
    }

    /// With nothing exported there is nothing to read replies *to*, so the
    /// second request is not made. This is the egress rule: a member who has
    /// published nothing does not have the app ask public relays about threads.
    #[tokio::test]
    async fn with_no_exported_threads_no_reply_request_is_made() {
        let fake = FakeSource::new(vec![article_event("aa", "one", 100, &long_body())], vec![]);
        let source: Arc<dyn RelaySource> = fake.clone();
        let mut state = NeighborhoodsState::default();

        let report = refresh(&source, &mut state, &[]).await.unwrap();
        assert_eq!(report.articles, 1);
        assert_eq!(report.replies, 0);
        assert_eq!(
            fake.filters().len(),
            1,
            "only the article pass went out: {:?}",
            fake.filters()
        );
    }

    #[tokio::test]
    async fn a_refresh_reads_the_threads_it_was_given_and_nothing_else() {
        let scope = "30023:aa:one";
        let other = "30023:zz:elsewhere";
        let fake = FakeSource::new(
            vec![article_event("aa", "one", 100, &long_body())],
            vec![
                comment_event("c1", scope, None, 10),
                comment_event("c2", scope, Some("c1"), 20),
                // A relay answers what it likes. A comment about a thread this
                // vault did not ask for is not one of its threads.
                comment_event("c3", other, None, 30),
            ],
        );
        let source: Arc<dyn RelaySource> = fake.clone();
        let mut state = NeighborhoodsState::default();

        let report = refresh(&source, &mut state, &[scope.to_string()])
            .await
            .unwrap();
        assert_eq!(report.replies, 2);
        assert_eq!(report.replies_error, None);
        assert_eq!(state.replies_for(scope).len(), 2);
        assert!(state.replies_for(other).is_empty());

        let filters = fake.filters();
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[1].kinds, vec![KIND_COMMENT]);
        assert_eq!(
            filters[1].tags,
            vec![("A".to_string(), vec![scope.to_string()])]
        );
    }

    /// A relay that refuses the reply query has not lost the articles: the page
    /// still refreshes, and the report says why the thread did not.
    #[tokio::test]
    async fn a_failed_reply_pass_keeps_the_articles_and_says_why() {
        struct HalfBroken {
            articles: Vec<RawEvent>,
        }

        #[async_trait]
        impl RelaySource for HalfBroken {
            async fn fetch(
                &self,
                _relays: &[String],
                filter: &Filter,
            ) -> Result<Vec<RawEvent>, String> {
                if filter.kinds == vec![KIND_LONG_FORM] {
                    Ok(self.articles.clone())
                } else {
                    Err("wss://relay.example: rate limited".to_string())
                }
            }
        }

        let source: Arc<dyn RelaySource> = Arc::new(HalfBroken {
            articles: vec![article_event("aa", "one", 100, &long_body())],
        });
        let mut state = NeighborhoodsState::default();

        let report = refresh(&source, &mut state, &["30023:aa:one".to_string()])
            .await
            .unwrap();
        assert_eq!(report.articles, 1, "the page still refreshed");
        assert_eq!(report.replies, 0);
        assert_eq!(
            report.replies_error.as_deref(),
            Some("wss://relay.example: rate limited")
        );
        assert_eq!(state.articles().len(), 1);
    }

    fn address_of(event: &RawEvent) -> String {
        format!(
            "{KIND_LONG_FORM}:{}:{}",
            event.pubkey,
            event.tag("d").unwrap()
        )
    }
}

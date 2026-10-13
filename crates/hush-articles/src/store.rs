//! The cached public corpus and the lane's settings.

use serde::{Deserialize, Serialize};

use crate::nip22::Comment;
use crate::nip23::Article;

/// The encrypted sidecar the public cache and the lane's settings live in. The
/// crate only encodes and decodes it; the vault owns the key (the daemon writes
/// it through the vault, exactly as it does the zines sidecar).
pub const STATE_FILE: &str = "neighborhoods.enc";

/// The cap on cached articles. The cache is a reading surface, not an archive:
/// once it is full the oldest fall off, so a busy relay cannot grow it without
/// bound.
pub const ARTICLES_CAP: usize = 500;

/// The cap on cached replies, across every thread.
///
/// Higher than the article cap because a thread is many small rows rather than
/// one large one, and lower than any real firehose: this lane reads the threads
/// of articles this vault published, not a global comment stream.
pub const REPLIES_CAP: usize = 2000;

/// The relays shipped as the default allowlist. A curated set with **no
/// Hush-run relay**; user-editable (spec Section 2, #8).
pub const DEFAULT_ALLOWLIST: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
];

/// The lane's settings, all user-editable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The relays read from. Never a Hush-run relay.
    pub allowlist: Vec<String>,
    /// The reading-time floor, in whole minutes.
    pub min_minutes: u32,
    /// Whether the front page shows public posts by default. **Off** by default,
    /// because showing them is what triggers egress: a member who has not opted
    /// in must not have the app reach public relays on their behalf.
    pub default_show: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            allowlist: DEFAULT_ALLOWLIST.iter().map(|s| (*s).to_string()).collect(),
            min_minutes: 3,
            default_show: false,
        }
    }
}

/// The cached public corpus and the lane's settings.
#[derive(Default, Serialize, Deserialize)]
pub struct NeighborhoodsState {
    articles: Vec<Article>,
    /// Replies to threads this vault has a stake in, across every root. Cached
    /// flat and looked up by root, because a comment knows its root and its
    /// parent and nothing else: the tree is built where it is drawn.
    #[serde(default)]
    replies: Vec<Comment>,
    #[serde(default)]
    settings: Settings,
}

impl NeighborhoodsState {
    pub fn articles(&self) -> &[Article] {
        &self.articles
    }

    pub fn replies(&self) -> &[Comment] {
        &self.replies
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// Merge a batch from the relays into the cache, deduping on the address
    /// `(pubkey, d)` and keeping the newest `created_at`.
    ///
    /// NIP-23 is editable, so a relay can serve several versions of one address
    /// and they must collapse to one row. An older version never overwrites a
    /// newer one — a relay replaying history must not roll the page back.
    /// Returns how many addresses were new or replaced.
    pub fn absorb(&mut self, incoming: Vec<Article>) -> usize {
        let mut changed = 0;
        for article in incoming {
            match self
                .articles
                .iter_mut()
                .find(|a| a.address() == article.address())
            {
                Some(existing) => {
                    if article.created_at > existing.created_at {
                        *existing = article;
                        changed += 1;
                    }
                }
                None => {
                    self.articles.push(article);
                    changed += 1;
                }
            }
        }
        self.evict();
        changed
    }

    /// Merge a batch of comments into the cache, deduping on the event id.
    ///
    /// Unlike an article, a comment is a **regular** event: it is immutable, so
    /// there is no newest-version rule and a re-served id is simply already
    /// held. A comment that arrives again is not counted twice. Returns how many
    /// were new.
    pub fn absorb_replies(&mut self, incoming: Vec<Comment>) -> usize {
        let mut changed = 0;
        for comment in incoming {
            if self.replies.iter().any(|c| c.event_id == comment.event_id) {
                continue;
            }
            self.replies.push(comment);
            changed += 1;
        }
        self.evict_replies();
        changed
    }

    /// One thread: every held comment filed under `address`, oldest first.
    ///
    /// Oldest first, not newest: a thread is a conversation and is read in the
    /// order it happened. Ties break on the event id so the order is stable when
    /// a relay serves two comments in the same second.
    pub fn replies_for(&self, address: &str) -> Vec<Comment> {
        let mut out: Vec<Comment> = self
            .replies
            .iter()
            .filter(|c| c.root == address)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.event_id.cmp(&b.event_id))
        });
        out
    }

    /// The index: newest first, filtered by section and by author.
    ///
    /// The filters are advisory — an unknown section is an empty list, not an
    /// error, because a section that has gone away is not a failure to answer.
    pub fn index(&self, section: Option<&str>, pubkey: Option<&str>) -> Vec<Article> {
        let section = normalise(section);
        let pubkey = normalise(pubkey);
        let mut out: Vec<Article> = self
            .articles
            .iter()
            .filter(|a| {
                section
                    .as_deref()
                    .is_none_or(|s| a.sections.iter().any(|x| x == s))
            })
            .filter(|a| pubkey.as_deref().is_none_or(|p| a.pubkey == p))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.address().cmp(&b.address()))
        });
        out
    }

    /// One article by address (`kind:pubkey:d`).
    pub fn get(&self, address: &str) -> Option<&Article> {
        self.articles.iter().find(|a| a.address() == address)
    }

    /// The section facet over what is held: by count, most-used first, ties by
    /// name.
    pub fn sections(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for a in &self.articles {
            for s in &a.sections {
                match counts.iter_mut().find(|(name, _)| name == s) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((s.clone(), 1)),
                }
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        counts
    }

    /// Keep the cache under the cap, dropping the oldest first.
    fn evict(&mut self) {
        if self.articles.len() <= ARTICLES_CAP {
            return;
        }
        self.articles
            .sort_by_key(|a| std::cmp::Reverse(a.created_at));
        self.articles.truncate(ARTICLES_CAP);
    }

    /// The same, for replies: newest kept, oldest dropped.
    ///
    /// Dropping a comment can orphan a reply to it — the reply is kept and its
    /// parent is not. That is the honest failure of a bounded cache, and the
    /// thread renderer treats a comment whose parent it cannot find as a
    /// top-level one rather than losing it.
    fn evict_replies(&mut self) {
        if self.replies.len() <= REPLIES_CAP {
            return;
        }
        self.replies
            .sort_by_key(|c| std::cmp::Reverse(c.created_at));
        self.replies.truncate(REPLIES_CAP);
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }

    /// Decode the sidecar. `None` is a fresh, empty cache with default settings —
    /// the state before the first refresh, not an error.
    pub fn decode(raw: Option<&[u8]>) -> Result<Self, String> {
        match raw {
            None => Ok(Self::default()),
            Some(bytes) => serde_json::from_slice(bytes).map_err(|e| e.to_string()),
        }
    }
}

fn normalise(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nip23::KIND_LONG_FORM;

    fn article(pubkey: &str, d: &str, created_at: u64) -> Article {
        Article {
            event_id: format!("{pubkey}-{d}-{created_at}"),
            pubkey: pubkey.to_string(),
            d: d.to_string(),
            title: "T".to_string(),
            summary: None,
            sections: vec!["rust".to_string()],
            content: "body".to_string(),
            created_at,
            published_at: None,
        }
    }

    #[test]
    fn settings_default_to_a_curated_allowlist_a_floor_and_off() {
        let s = Settings::default();
        assert_eq!(s.allowlist, DEFAULT_ALLOWLIST);
        assert_eq!(s.min_minutes, 3);
        assert!(!s.default_show, "public posts are hidden until opted in");
    }

    #[test]
    fn an_address_collapses_to_its_newest_version() {
        let mut state = NeighborhoodsState::default();
        assert_eq!(state.absorb(vec![article("aa", "x", 10)]), 1);
        // A replay of the same version changes nothing.
        assert_eq!(state.absorb(vec![article("aa", "x", 10)]), 0);
        // An older version must not roll the page back.
        assert_eq!(state.absorb(vec![article("aa", "x", 5)]), 0);
        assert_eq!(state.articles().len(), 1);
        assert_eq!(state.articles()[0].created_at, 10);
        // A newer version replaces it.
        assert_eq!(state.absorb(vec![article("aa", "x", 20)]), 1);
        assert_eq!(state.articles().len(), 1);
        assert_eq!(state.articles()[0].created_at, 20);
        // A different address is a new row.
        assert_eq!(state.absorb(vec![article("aa", "y", 1)]), 1);
        assert_eq!(state.articles().len(), 2);
    }

    #[test]
    fn the_index_is_newest_first_and_filters_by_section_and_author() {
        let mut state = NeighborhoodsState::default();
        let mut older = article("aa", "x", 10);
        older.sections = vec!["news".to_string()];
        let mut newer = article("bb", "y", 30);
        newer.sections = vec!["news".to_string()];
        state.absorb(vec![older, newer, article("aa", "z", 20)]);

        let all = state.index(None, None);
        assert_eq!(
            all.iter().map(|a| a.created_at).collect::<Vec<_>>(),
            vec![30, 20, 10]
        );

        let news = state.index(Some("news"), None);
        assert_eq!(news.len(), 2);
        assert!(news.iter().all(|a| a.sections.contains(&"news".to_string())));

        let by_aa = state.index(None, Some("AA"));
        assert_eq!(by_aa.len(), 2);
        assert!(by_aa.iter().all(|a| a.pubkey == "aa"));

        assert!(state.index(Some("gone"), None).is_empty());
    }

    #[test]
    fn the_section_facet_counts_and_sorts() {
        let mut state = NeighborhoodsState::default();
        let mut a = article("aa", "x", 1);
        a.sections = vec!["rust".to_string(), "news".to_string()];
        let mut b = article("bb", "y", 2);
        b.sections = vec!["rust".to_string()];
        state.absorb(vec![a, b]);
        assert_eq!(
            state.sections(),
            vec![("rust".to_string(), 2), ("news".to_string(), 1)]
        );
    }

    #[test]
    fn the_cache_is_capped_at_the_newest() {
        let mut state = NeighborhoodsState::default();
        let batch: Vec<Article> = (0..ARTICLES_CAP as u64 + 50)
            .map(|i| article("aa", &format!("d{i}"), i))
            .collect();
        state.absorb(batch);
        assert_eq!(state.articles().len(), ARTICLES_CAP);
        // The oldest 50 fell off; the newest is still here.
        assert!(state.get(&format!("{KIND_LONG_FORM}:aa:d0")).is_none());
        assert!(state
            .get(&format!("{KIND_LONG_FORM}:aa:d{}", ARTICLES_CAP + 49))
            .is_some());
    }

    #[test]
    fn the_state_round_trips_and_an_absent_sidecar_is_default() {
        let mut state = NeighborhoodsState::default();
        state.absorb(vec![article("aa", "x", 1)]);
        let bytes = state.encode().unwrap();

        let back = NeighborhoodsState::decode(Some(&bytes)).unwrap();
        assert_eq!(back.articles(), state.articles());
        assert_eq!(back.settings(), state.settings());

        assert!(NeighborhoodsState::decode(None).unwrap().articles().is_empty());
        assert!(NeighborhoodsState::decode(Some(b"not json")).is_err());
    }

    fn comment(id: &str, root: &str, parent: Option<&str>, created_at: u64) -> Comment {
        Comment {
            event_id: id.to_string(),
            pubkey: "bb".to_string(),
            content: format!("body {id}"),
            created_at,
            root: root.to_string(),
            parent_id: parent.map(str::to_string),
        }
    }

    /// A comment is a regular event, so there is no newest-version rule: the
    /// same id arriving twice is the same comment, and an old one is not stale.
    #[test]
    fn a_comment_is_deduped_by_its_event_id_and_never_replaced() {
        let mut state = NeighborhoodsState::default();
        assert_eq!(state.absorb_replies(vec![comment("c1", "30023:aa:x", None, 10)]), 1);
        assert_eq!(state.absorb_replies(vec![comment("c1", "30023:aa:x", None, 10)]), 0);
        assert_eq!(state.replies().len(), 1);

        let mut edited = comment("c1", "30023:aa:x", None, 10);
        edited.content = "rewritten".to_string();
        assert_eq!(state.absorb_replies(vec![edited]), 0);
        assert_eq!(
            state.replies()[0].content,
            "body c1",
            "a regular event is immutable, so a re-served id is not an edit"
        );
    }

    /// A thread is a conversation, so it is read oldest first — and it is one
    /// thread, not the whole cache.
    #[test]
    fn a_thread_is_the_comments_under_one_root_oldest_first() {
        let mut state = NeighborhoodsState::default();
        state.absorb_replies(vec![
            comment("c3", "30023:aa:x", None, 30),
            comment("c1", "30023:aa:x", None, 10),
            comment("c2", "30023:bb:y", None, 20),
        ]);

        let thread = state.replies_for("30023:aa:x");
        assert_eq!(
            thread.iter().map(|c| c.event_id.as_str()).collect::<Vec<_>>(),
            vec!["c1", "c3"]
        );
        assert_eq!(state.replies_for("30023:bb:y").len(), 1);
        assert!(state.replies_for("30023:cc:z").is_empty());
    }

    /// Two comments in the same second still have one order, so a thread does
    /// not reshuffle between reads.
    #[test]
    fn a_thread_ties_break_on_the_event_id() {
        let mut state = NeighborhoodsState::default();
        state.absorb_replies(vec![
            comment("zz", "30023:aa:x", None, 10),
            comment("aa", "30023:aa:x", None, 10),
        ]);
        assert_eq!(
            state
                .replies_for("30023:aa:x")
                .iter()
                .map(|c| c.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aa", "zz"]
        );
    }

    #[test]
    fn the_reply_cache_is_capped_at_the_newest() {
        let mut state = NeighborhoodsState::default();
        let batch: Vec<Comment> = (0..REPLIES_CAP as u64 + 50)
            .map(|i| comment(&format!("c{i}"), "30023:aa:x", None, i))
            .collect();
        state.absorb_replies(batch);
        assert_eq!(state.replies().len(), REPLIES_CAP);
        assert!(state.replies_for("30023:aa:x").iter().all(|c| c.event_id != "c0"));
        assert!(state
            .replies_for("30023:aa:x")
            .iter()
            .any(|c| c.event_id == format!("c{}", REPLIES_CAP + 49)));
    }

    /// The replies travel in the same sealed sidecar as the articles.
    #[test]
    fn the_replies_round_trip_with_the_articles() {
        let mut state = NeighborhoodsState::default();
        state.absorb(vec![article("aa", "x", 1)]);
        state.absorb_replies(vec![comment("c1", "30023:aa:x", Some("c0"), 5)]);
        let bytes = state.encode().unwrap();

        let back = NeighborhoodsState::decode(Some(&bytes)).unwrap();
        assert_eq!(back.replies(), state.replies());
        assert_eq!(back.replies()[0].parent_id.as_deref(), Some("c0"));
    }

    /// A sidecar written before this lane cached replies has no `replies` key,
    /// and it must decode rather than be thrown away.
    #[test]
    fn a_sidecar_from_before_replies_were_cached_still_decodes() {
        let older = br#"{"articles":[],"settings":{"allowlist":[],"min_minutes":3,"default_show":false}}"#;
        let state = NeighborhoodsState::decode(Some(older)).unwrap();
        assert!(state.replies().is_empty());
    }
}

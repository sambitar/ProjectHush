//! The lane's local state: who this vault follows and what it has read.
//!
//! The same shape as the zines sidecar and for the same reasons: one encrypted
//! file, a cap with a reason rather than a silent drop, and a decode that treats
//! a missing file as the state before the first use. The daemon owns the file
//! (`atproto.enc`, written through the vault's encrypted-file path) and this
//! module owns the bytes in it.
//!
//! Two lists, deliberately separate. `follows` is what the member asked for — a
//! curated set, never a firehose, which is what keeps the public lane from
//! becoming the global graph this app refuses to hold. `documents` is what those
//! follows have published, held locally so reading costs no network.
//!
//! This vault's own footprint is deliberately **not** here. Which of its zines
//! are public is a fact about those zines, and it lives with them in the zines
//! sidecar ([`hush_atproto::Post::rkey_for`] is derived rather than stored, so
//! there is nothing to lose). A second copy would be a second answer to "is this
//! public" that a lost file could disagree with.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::error::Error;
use crate::explore::PublicationSummary;
use crate::standard::Document;

/// The encrypted sidecar this state lives in.
pub const STATE_FILE: &str = "atproto.enc";

/// How many publications one vault may follow.
///
/// A curated reading list, not a subscription service: the number is the same
/// order as the retired lane's allowlist, and it exists so a lane that reads
/// every follow on every refresh cannot grow its own request count without
/// bound.
pub const FOLLOWS_CAP: usize = 50;

/// How many documents the cache holds. Bounded like the zine corpus, and for
/// the same reason: the file has to decrypt into memory.
pub const DOCUMENTS_CAP: usize = 1024;

/// How many publications the directory holds.
///
/// A crawl of the shipped seeds finds a few dozen; this is generous against that
/// and still bounded, so a directory that somehow grew a thousand entries cannot
/// make the sidecar too large to decrypt.
pub const DIRECTORY_CAP: usize = 500;

/// One publication this vault reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Follow {
    /// The account whose repo is read. The stable half: a handle can move.
    pub did: String,
    /// The handle at the time it was added, for display only.
    #[serde(default)]
    pub handle: String,
    /// What to show on the page when the handle is not known.
    #[serde(default)]
    pub name: String,
}

impl Follow {
    /// What the page calls this publication.
    pub fn label(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let handle = self.handle.trim();
        if !handle.is_empty() {
            return handle.to_string();
        }
        self.did.clone()
    }
}

/// Everything the public lane remembers locally.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtprotoState {
    #[serde(default)]
    follows: Vec<Follow>,
    #[serde(default)]
    documents: Vec<Document>,
    /// The last directory a crawl found.
    ///
    /// Kept here rather than in a sidecar of its own because it is the same
    /// secret as the rest of the lane — a list of what this member was looking
    /// at — and because it is then encrypted, backed up and restored by
    /// machinery that already exists rather than by a second copy of it.
    #[serde(default)]
    directory: Vec<PublicationSummary>,
    /// When the directory was crawled, unix seconds. `0` means never.
    #[serde(default)]
    directory_unix: u64,
}

impl AtprotoState {
    /// The stored form, for the vault's encrypted sidecar.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|e| Error::Json(e.to_string()))
    }

    /// Read the stored form. A missing or empty sidecar is the state before the
    /// first refresh, not a fault.
    pub fn decode(raw: Option<&[u8]>) -> Result<Self, Error> {
        match raw {
            None => Ok(Self::default()),
            Some(bytes) if bytes.is_empty() => Ok(Self::default()),
            Some(bytes) => serde_json::from_slice(bytes).map_err(|e| Error::Json(e.to_string())),
        }
    }

    pub fn follows(&self) -> &[Follow] {
        &self.follows
    }

    /// The last crawled directory, and when it was crawled.
    ///
    /// The timestamp is the caller's, not this crate's: a clock belongs to
    /// whoever is deciding when a crawl is stale, and a crate that read the
    /// clock itself could not be tested against a fixed one.
    pub fn directory(&self) -> (&[PublicationSummary], u64) {
        (&self.directory, self.directory_unix)
    }

    /// Replace the directory with what a crawl found.
    ///
    /// Sorted and deduped here so the stored form has one order whatever a
    /// crawl returned, and so re-crawling cannot grow the list with the same
    /// publication twice.
    pub fn set_directory(&mut self, found: Vec<PublicationSummary>, now_unix: u64) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut deduped: Vec<PublicationSummary> = found
            .into_iter()
            .filter(|entry| seen.insert(entry.uri.clone()))
            .collect();
        deduped.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.uri.cmp(&b.uri))
        });
        deduped.truncate(DIRECTORY_CAP);
        self.directory = deduped;
        self.directory_unix = now_unix;
    }

    /// Replace the follow list, refusing one over the cap with a reason rather
    /// than quietly truncating it: a member who pasted 60 publications should
    /// learn that the sixtieth did not take.
    pub fn set_follows(&mut self, follows: Vec<Follow>) -> Result<(), String> {
        let mut seen = Vec::new();
        let mut deduped = Vec::new();
        for follow in follows {
            let did = follow.did.trim().to_string();
            if did.is_empty() {
                return Err("a follow needs a DID".to_string());
            }
            if seen.contains(&did) {
                continue;
            }
            seen.push(did.clone());
            deduped.push(Follow { did, ..follow });
        }
        if deduped.len() > FOLLOWS_CAP {
            return Err(format!("at most {FOLLOWS_CAP} publications can be followed"));
        }
        self.follows = deduped;
        // A follow that was dropped takes its documents with it: a cache of a
        // publication the member no longer reads is bytes with no reader.
        let kept: Vec<String> = self.follows.iter().map(|f| f.did.clone()).collect();
        self.documents.retain(|d| kept.contains(&d.did));
        Ok(())
    }

    /// The cached documents, newest first.
    pub fn documents(&self) -> &[Document] {
        &self.documents
    }

    /// The index the page renders: newest first, optionally filtered by tag and
    /// by author.
    pub fn index(&self, tag: Option<&str>, did: Option<&str>) -> Vec<&Document> {
        self.documents
            .iter()
            .filter(|d| match tag {
                Some(tag) => d.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)),
                None => true,
            })
            .filter(|d| match did {
                Some(did) => d.did == did,
                None => true,
            })
            .collect()
    }

    /// Every tag in the corpus with how many documents carry it, most first then
    /// alphabetical — the page's section strip.
    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for document in &self.documents {
            for tag in &document.tags {
                match counts
                    .iter_mut()
                    .find(|(name, _)| name.eq_ignore_ascii_case(tag))
                {
                    Some((_, count)) => *count += 1,
                    None => counts.push((tag.clone(), 1)),
                }
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        counts
    }

    /// One document by its address.
    pub fn get(&self, uri: &str) -> Option<&Document> {
        self.documents.iter().find(|d| d.uri == uri)
    }

    /// Merge a batch of documents in, newest first, and answer how many were new
    /// or replaced.
    ///
    /// Keyed by address, so a refresh that re-reads the same records is not a
    /// change — the same rule the zine corpus uses, and what makes a poll cheap
    /// enough to run on a timer.
    pub fn absorb(&mut self, incoming: Vec<Document>) -> usize {
        let mut changed = 0;
        for document in incoming {
            match self.documents.iter_mut().find(|d| d.uri == document.uri) {
                Some(existing) => {
                    if *existing != document {
                        *existing = document;
                        changed += 1;
                    }
                }
                None => {
                    self.documents.push(document);
                    changed += 1;
                }
            }
        }
        self.documents
            .sort_by(|a, b| b.published_secs.cmp(&a.published_secs).then_with(|| a.uri.cmp(&b.uri)));
        if self.documents.len() > DOCUMENTS_CAP {
            self.documents.truncate(DOCUMENTS_CAP);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(uri: &str, did: &str, secs: i64, tags: &[&str]) -> Document {
        Document {
            uri: uri.into(),
            did: did.into(),
            handle: String::new(),
            title: format!("Title {uri}"),
            text: "Body".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            published_at: crate::standard::rfc3339((secs * 1000) as u64),
            published_secs: secs,
            path: "/x".into(),
            description: String::new(),
            url: String::new(),
            bsky_post_ref: None,
            site: String::new(),
        }
    }

    fn follow(did: &str) -> Follow {
        Follow {
            did: did.into(),
            handle: format!("{did}.test"),
            name: String::new(),
        }
    }

    #[test]
    fn a_missing_sidecar_is_an_empty_lane() {
        let empty = AtprotoState::decode(None).unwrap();
        assert!(empty.follows().is_empty());
        assert!(empty.documents().is_empty());
        assert_eq!(empty, AtprotoState::decode(Some(b"")).unwrap());
    }

    #[test]
    fn absorbing_is_idempotent_and_orders_newest_first() {
        let mut state = AtprotoState::default();
        assert_eq!(state.absorb(vec![doc("a", "d1", 100, &[]), doc("b", "d1", 300, &[])]), 2);
        let uris: Vec<&str> = state.documents().iter().map(|d| d.uri.as_str()).collect();
        assert_eq!(uris, vec!["b", "a"], "newest first");

        // The same batch again changes nothing.
        assert_eq!(state.absorb(vec![doc("a", "d1", 100, &[]), doc("b", "d1", 300, &[])]), 0);
        assert_eq!(state.documents().len(), 2);

        // A replaced record counts as a change, and re-sorts.
        assert_eq!(state.absorb(vec![doc("a", "d1", 400, &[])]), 1);
        let uris: Vec<&str> = state.documents().iter().map(|d| d.uri.as_str()).collect();
        assert_eq!(uris, vec!["a", "b"]);
    }

    #[test]
    fn the_cache_is_capped_and_drops_the_oldest() {
        let mut state = AtprotoState::default();
        let batch: Vec<Document> = (0..DOCUMENTS_CAP as i64 + 10)
            .map(|i| doc(&format!("u{i}"), "d1", i, &[]))
            .collect();
        state.absorb(batch);
        assert_eq!(state.documents().len(), DOCUMENTS_CAP);
        assert!(
            state.get("u0").is_none(),
            "the oldest is what goes when the cache is full"
        );
        assert!(state.get(&format!("u{}", DOCUMENTS_CAP + 9)).is_some());
    }

    #[test]
    fn a_follow_list_is_deduplicated_and_capped_with_a_reason() {
        let mut state = AtprotoState::default();
        state.set_follows(vec![follow("d1"), follow("d1"), follow("d2")]).unwrap();
        assert_eq!(state.follows().len(), 2, "a repeated DID is one follow");

        let too_many: Vec<Follow> = (0..FOLLOWS_CAP + 1)
            .map(|i| follow(&format!("did:plc:{i}")))
            .collect();
        let err = state.set_follows(too_many).unwrap_err();
        assert!(err.contains("at most"), "{err}");
        assert_eq!(state.follows().len(), 2, "a refused list changes nothing");

        assert!(state.set_follows(vec![follow("  ")]).is_err());
    }

    #[test]
    fn dropping_a_follow_drops_its_documents() {
        let mut state = AtprotoState::default();
        state.set_follows(vec![follow("d1"), follow("d2")]).unwrap();
        state.absorb(vec![doc("a", "d1", 100, &[]), doc("b", "d2", 200, &[])]);
        state.set_follows(vec![follow("d2")]).unwrap();
        assert!(state.get("a").is_none(), "an unfollowed publication is unread");
        assert!(state.get("b").is_some());
    }

    #[test]
    fn the_index_filters_by_tag_and_author_case_insensitively() {
        let mut state = AtprotoState::default();
        state.absorb(vec![
            doc("a", "d1", 100, &["Rust", "notes"]),
            doc("b", "d2", 200, &["rust"]),
            doc("c", "d1", 300, &[]),
        ]);
        assert_eq!(state.index(None, None).len(), 3);
        assert_eq!(state.index(Some("rust"), None).len(), 2);
        assert_eq!(state.index(None, Some("d1")).len(), 2);
        assert_eq!(state.index(Some("rust"), Some("d1")).len(), 1);
        assert_eq!(
            state.tags(),
            vec![("rust".to_string(), 2), ("notes".to_string(), 1)],
            "most used first, then alphabetical"
        );
    }

    #[test]
    fn the_state_round_trips_through_its_stored_form() {
        let mut state = AtprotoState::default();
        state.set_follows(vec![follow("d1")]).unwrap();
        state.absorb(vec![doc("a", "d1", 100, &["rust"])]);
        let back = AtprotoState::decode(Some(&state.encode().unwrap())).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn a_follow_falls_back_from_name_to_handle_to_did() {
        let mut f = follow("did:plc:abc");
        assert_eq!(f.label(), "did:plc:abc.test");
        f.handle.clear();
        assert_eq!(f.label(), "did:plc:abc");
        f.name = "  Ada  ".into();
        assert_eq!(f.label(), "Ada");
    }

    fn summary(uri: &str, name: &str) -> PublicationSummary {
        PublicationSummary {
            uri: uri.into(),
            did: "did:plc:abc".into(),
            handle: "abc.test".into(),
            name: name.into(),
            description: String::new(),
            url: String::new(),
            has_icon: false,
            via: "seed.example".into(),
        }
    }

    #[test]
    fn a_directory_round_trips_and_remembers_when_it_was_crawled() {
        let mut state = AtprotoState::default();
        assert_eq!(state.directory(), (&[][..], 0), "never crawled is not an error");

        state.set_directory(vec![summary("at://a", "Ada Writes")], 1_700_000_000);
        let back = AtprotoState::decode(Some(&state.encode().unwrap())).unwrap();
        assert_eq!(back, state);
        assert_eq!(back.directory().1, 1_700_000_000);
        assert_eq!(back.directory().0[0].name, "Ada Writes");
    }

    /// Re-crawling finds the same publications again, and a directory that grew
    /// a duplicate row every time would be a list nobody could use.
    #[test]
    fn re_crawling_replaces_the_directory_rather_than_growing_it() {
        let mut state = AtprotoState::default();
        state.set_directory(
            vec![summary("at://b", "Zed"), summary("at://a", "Ada"), summary("at://a", "Ada")],
            1,
        );
        let (entries, at) = state.directory();
        assert_eq!(at, 1);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Ada", "Zed"], "deduped and sorted by name");

        state.set_directory(vec![summary("at://a", "Ada")], 2);
        let (entries, at) = state.directory();
        assert_eq!(at, 2);
        assert_eq!(entries.len(), 1, "a new crawl replaces, it does not append");
    }

    #[test]
    fn the_directory_is_capped() {
        let many: Vec<PublicationSummary> = (0..DIRECTORY_CAP + 20)
            .map(|i| summary(&format!("at://{i}"), &format!("Pub {i:04}")))
            .collect();
        let mut state = AtprotoState::default();
        state.set_directory(many, 1);
        assert_eq!(state.directory().0.len(), DIRECTORY_CAP);
    }
}

//! The Reads pane's personal state: news sources, saved stories, hidden
//! stories, read markers, reading history.
//!
//! Encrypted at rest in a vault sidecar (`reads-state.enc`, see
//! `Vault::write_encrypted_file`) — this is user data, unlike the public
//! content cache in [`crate::reads`]. The JSON schema here is what the UI
//! receives from `GET /api/reads/state` (minus nothing: the sidecar bytes are
//! exactly the API payload).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::reads::StoryDto;

/// The vault sidecar file name. Fixed here so the worker and any future
/// migration agree on one name.
pub const STATE_FILE: &str = "reads-state.enc";

/// Read markers are ids-with-timestamps; old ones fall off the front.
const READ_CAP: usize = 2000;
const HISTORY_CAP: usize = 500;
const HIDDEN_CAP: usize = 5000;
/// User-added RSS sources. A bound keeps the sidecar and the source list sane.
const SOURCES_CAP: usize = 50;

/// The built-in source: Hacker News "best of the week", shipped with the app.
/// It is not stored in state and cannot be removed; `HK_NEWS_ID` is how the
/// feed route and the UI refer to it.
pub const HK_NEWS_ID: &str = "hk-news";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RssSource {
    /// `rss-` + 16 hex chars of the URL's SHA-256: stable across restarts,
    /// opaque to the UI.
    pub id: String,
    pub url: String,
    /// The feed's own title, learned from the first successful fetch.
    pub title: String,
    /// Unix seconds.
    pub added_at: i64,
}

/// Deterministic source id for a feed URL; same URL, same id, always.
pub fn source_id_for(url: &str) -> String {
    let hash = Sha256::digest(url.as_bytes());
    format!("rss-{}", hex(&hash[..8]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedStory {
    /// Nested, not flattened: the UI's Saved list reads `x.story.title` and
    /// the feed annotator checks `x.story.id`; a flat shape would silently
    /// make both compare against `undefined`.
    pub story: StoryDto,
    /// Unix seconds.
    pub saved_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadMarker {
    pub id: String,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    /// Unix seconds.
    pub at: i64,
    /// Title snapshot, so "recently read" renders without refetching.
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReadsState {
    /// User-added RSS sources, oldest add first. The built-in HK News feed
    /// is not here — it cannot be removed, so it is not state.
    #[serde(default)]
    pub sources: Vec<RssSource>,
    /// Saved ("read later") stories, newest save first.
    #[serde(default)]
    pub saved: Vec<SavedStory>,
    /// Story ids hidden from every list; the story detail stays reachable.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Read markers, oldest first, capped.
    #[serde(default)]
    pub read: Vec<ReadMarker>,
    /// Reading history, most recent first, capped.
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

/// Decode sidecar bytes; `None` (never saved) decodes to the empty default.
/// A corrupt sidecar is an error, not silent data loss.
pub fn decode(bytes: Option<&[u8]>) -> Result<ReadsState, String> {
    match bytes {
        None | Some([]) => Ok(ReadsState::default()),
        Some(b) => serde_json::from_slice(b).map_err(|e| format!("reads state unreadable: {e}")),
    }
}

impl ReadsState {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("reads state unwritable: {e}"))
    }

    /// Register an RSS source. Re-adding a URL refreshes its title instead of
    /// duplicating. `Err` names the reason (cap reached).
    pub fn add_source(&mut self, url: &str, title: &str) -> Result<RssSource, String> {
        if let Some(existing) = self.sources.iter_mut().find(|s| s.url == url) {
            existing.title = title.to_string();
            return Ok(existing.clone());
        }
        if self.sources.len() >= SOURCES_CAP {
            return Err(format!("at most {SOURCES_CAP} feeds can be added"));
        }
        let source = RssSource {
            id: source_id_for(url),
            url: url.to_string(),
            title: title.to_string(),
            added_at: now_secs(),
        };
        self.sources.push(source.clone());
        Ok(source)
    }

    pub fn remove_source(&mut self, id: &str) -> bool {
        let before = self.sources.len();
        self.sources.retain(|s| s.id != id);
        self.sources.len() != before
    }

    pub fn source(&self, id: &str) -> Option<&RssSource> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// Save or unsave a story; returns whether it is saved after the call.
    pub fn toggle_saved(&mut self, story: StoryDto) -> bool {
        if self.saved.iter().any(|s| s.story.id == story.id) {
            self.saved.retain(|s| s.story.id != story.id);
            return false;
        }
        self.saved.insert(
            0,
            SavedStory {
                story,
                saved_at: now_secs(),
            },
        );
        true
    }

    /// Hide or unhide a story; returns whether it is hidden after the call.
    pub fn toggle_hidden(&mut self, id: &str) -> bool {
        if self.hidden.iter().any(|h| h == id) {
            self.hidden.retain(|h| h != id);
            return false;
        }
        self.hidden.insert(0, id.to_string());
        self.hidden.truncate(HIDDEN_CAP);
        true
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.iter().any(|h| h == id)
    }

    pub fn is_saved(&self, id: &str) -> bool {
        self.saved.iter().any(|s| s.story.id == id)
    }

    /// Drop hidden stories from a list view.
    pub fn filter_hidden(&self, stories: Vec<StoryDto>) -> Vec<StoryDto> {
        if self.hidden.is_empty() {
            return stories;
        }
        stories
            .into_iter()
            .filter(|s| !self.is_hidden(&s.id))
            .collect()
    }

    /// Record a read: upsert the marker and prepend history, newest first.
    pub fn mark_read(&mut self, id: &str, title: &str) {
        let now = now_secs();
        self.read.retain(|m| m.id != id);
        self.read.push(ReadMarker {
            id: id.to_string(),
            at: now,
        });
        if self.read.len() > READ_CAP {
            self.read.drain(0..self.read.len() - READ_CAP);
        }
        self.history.retain(|h| h.id != id);
        self.history.insert(
            0,
            HistoryEntry {
                id: id.to_string(),
                at: now,
                title: title.to_string(),
            },
        );
        self.history.truncate(HISTORY_CAP);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod reads_store_tests {
    use super::*;

    fn story(id: &str) -> StoryDto {
        StoryDto {
            id: id.to_string(),
            title: format!("Story {id}"),
            url: Some("https://example.com/a".into()),
            domain: Some("example.com".into()),
            author: "a".into(),
            points: 1,
            num_comments: 0,
            created_at: 1,
            hn_url: format!("https://news.ycombinator.com/item?id={id}"),
        }
    }

    #[test]
    fn state_roundtrips_through_bytes() {
        let mut state = ReadsState::default();
        state.toggle_saved(story("1"));
        state.mark_read("1", "Story 1");
        state.toggle_hidden("2");
        state
            .add_source("https://example.com/feed.xml", "Example Blog")
            .unwrap();
        let bytes = state.encode().unwrap();
        let back = decode(Some(&bytes)).unwrap();
        assert_eq!(back.saved.len(), 1);
        assert_eq!(back.saved[0].story.id, "1");
        assert_eq!(back.saved[0].story.title, "Story 1");
        assert_eq!(back.read.len(), 1);
        assert_eq!(back.history.len(), 1);
        assert_eq!(back.hidden, vec!["2".to_string()]);
        assert_eq!(back.sources.len(), 1);
        assert_eq!(back.sources[0].title, "Example Blog");
        assert_eq!(
            back.sources[0].id,
            source_id_for("https://example.com/feed.xml")
        );
    }

    #[test]
    fn sources_add_dedupe_remove_and_cap() {
        let mut state = ReadsState::default();
        let first = state.add_source("https://a.example/feed", "A").unwrap();
        assert_eq!(first.id, source_id_for("https://a.example/feed"));

        // Same URL, different reported title: dedupe + title refresh.
        let again = state
            .add_source("https://a.example/feed", "A renamed")
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(state.sources.len(), 1);
        assert_eq!(state.sources[0].title, "A renamed");

        assert!(state.source(&first.id).is_some());
        assert!(state.remove_source(&first.id));
        assert!(state.source(&first.id).is_none());
        assert!(!state.remove_source(&first.id), "removing twice is a no-op");

        for i in 0..SOURCES_CAP {
            state
                .add_source(&format!("https://s{i}.example/feed"), &format!("S{i}"))
                .unwrap();
        }
        assert_eq!(state.sources.len(), SOURCES_CAP);
        assert!(state
            .add_source("https://one-more.example/feed", "One more")
            .is_err());
    }

    #[test]
    fn source_ids_are_deterministic_and_url_shaped() {
        assert_eq!(
            source_id_for("https://x.example/f"),
            source_id_for("https://x.example/f")
        );
        assert_ne!(
            source_id_for("https://x.example/f"),
            source_id_for("https://y.example/f")
        );
        assert!(source_id_for("https://x.example/f").starts_with("rss-"));
        // Never collides with HN's all-digit ids.
        assert!(!source_id_for("https://x.example/f")
            .bytes()
            .all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn missing_and_empty_sidecars_decode_to_default() {
        assert!(decode(None).unwrap().saved.is_empty());
        assert!(decode(Some(b"")).unwrap().hidden.is_empty());
        assert!(
            decode(Some(b"garbage")).is_err(),
            "corrupt state is an error, not a reset"
        );
    }

    #[test]
    fn toggles_flip_and_restore() {
        let mut state = ReadsState::default();
        assert!(state.toggle_saved(story("7")), "first toggle saves");
        assert!(state.is_saved("7"));
        assert!(!state.toggle_saved(story("7")), "second toggle unsaves");
        assert!(!state.is_saved("7"));

        assert!(state.toggle_hidden("8"));
        assert!(state.is_hidden("8"));
        assert!(!state.toggle_hidden("8"));
        assert!(!state.is_hidden("8"));
    }

    #[test]
    fn read_markers_and_history_capped_and_deduped() {
        let mut state = ReadsState::default();
        for i in 0..(READ_CAP + 50) {
            state.mark_read(&i.to_string(), &format!("t{i}"));
        }
        assert_eq!(state.read.len(), READ_CAP);
        assert_eq!(state.history.len(), HISTORY_CAP);
        assert_eq!(
            state.history[0].id,
            (READ_CAP + 49).to_string(),
            "history is newest-first"
        );

        // Re-reading moves the entry to the front rather than duplicating.
        state.mark_read("10", "t10");
        assert_eq!(state.history[0].id, "10");
        assert_eq!(state.history.iter().filter(|h| h.id == "10").count(), 1);
    }

    #[test]
    fn filter_hidden_drops_only_hidden() {
        let mut state = ReadsState::default();
        state.toggle_hidden("2");
        let list = vec![story("1"), story("2"), story("3")];
        let kept = state.filter_hidden(list);
        assert_eq!(
            kept.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["1", "3"]
        );
    }
}

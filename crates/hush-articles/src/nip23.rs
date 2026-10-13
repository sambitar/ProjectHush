//! NIP-23: addressable long-form content, and the mapping to the shape the
//! front page reads.

use serde::{Deserialize, Serialize};

/// The NIP-23 kind for addressable long-form content. The only kind this crate
/// maps; see the crate docs for why that is structural.
pub const KIND_LONG_FORM: u64 = 30023;

/// A raw Nostr event, as it arrives from a relay. Only the fields the gate and
/// the article mapping read.
///
/// The signature is deliberately absent: Phase 2 reads from an allowlisted set
/// and does not verify authorship. Verification belongs with the phases that
/// *sign* — export and comments — and adding a field nothing checks would be
/// worse than not carrying it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    #[serde(default)]
    pub tags: Vec<Vec<String>>,
    #[serde(default)]
    pub content: String,
}

impl RawEvent {
    /// The first value of the first tag named `name`, if any.
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1))
            .map(String::as_str)
    }

    /// Every value of every tag named `name`.
    pub fn tags_named(&self, name: &str) -> Vec<&str> {
        self.tags
            .iter()
            .filter(|t| t.first().map(String::as_str) == Some(name))
            .filter_map(|t| t.get(1))
            .map(String::as_str)
            .collect()
    }
}

/// A public article, mapped from a NIP-23 `kind:30023` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Article {
    /// The event id of the version this was mapped from. A later revision of the
    /// same address carries a different id.
    pub event_id: String,
    /// The author's public key, hex, lowercased.
    pub pubkey: String,
    /// The `d` tag: the addressable id within the author's namespace.
    pub d: String,
    pub title: String,
    /// `summary` tag, when present and non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `t` tags, normalised to lowercase and deduped.
    #[serde(default)]
    pub sections: Vec<String>,
    /// Markdown body.
    pub content: String,
    /// `.created_at`, seconds: the version's timestamp.
    pub created_at: u64,
    /// `published_at` tag, when present (set on first publish).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<u64>,
}

impl Article {
    /// The NIP-01 addressable id, the `a` tag: `kind:pubkey:d`. This is the key
    /// an article is deduped and looked up by, and what a comment references.
    pub fn address(&self) -> String {
        format!("{KIND_LONG_FORM}:{}:{}", self.pubkey, self.d)
    }
}

/// Map a raw event to an article, structurally.
///
/// `None` when the event is not a `kind:30023`, or lacks the `d` or `title` the
/// format requires. This is the shape check; the reading-time floor is policy and
/// lives in [`crate::Gate`].
pub fn to_article(event: &RawEvent) -> Option<Article> {
    if event.kind != KIND_LONG_FORM {
        return None;
    }
    let d = event.tag("d")?.trim();
    if d.is_empty() {
        return None;
    }
    let title = event.tag("title")?.trim();
    if title.is_empty() {
        return None;
    }
    Some(Article {
        event_id: event.id.clone(),
        pubkey: event.pubkey.trim().to_ascii_lowercase(),
        d: d.to_string(),
        title: title.to_string(),
        summary: non_empty(event.tag("summary")),
        sections: normalise_sections(event.tags_named("t")),
        content: event.content.clone(),
        created_at: event.created_at,
        published_at: event.tag("published_at").and_then(|s| s.trim().parse().ok()),
    })
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn normalise_sections(raw: Vec<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in raw {
        let s = s.trim().to_ascii_lowercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: u64, tags: serde_json::Value) -> RawEvent {
        serde_json::from_value(json!({
            "id": "id-1",
            "pubkey": "AbCd",
            "created_at": 42,
            "kind": kind,
            "tags": tags,
            "content": "body",
        }))
        .unwrap()
    }

    #[test]
    fn a_long_form_event_maps_to_an_article() {
        let e = event(
            KIND_LONG_FORM,
            json!([
                ["d", "slug"],
                ["title", "  On relays  "],
                ["summary", "  a summary  "],
                ["t", "Rust"],
                ["t", "rust"],
                ["t", "  "],
                ["published_at", "10"],
            ]),
        );
        let a = to_article(&e).unwrap();
        assert_eq!(a.title, "On relays", "the title is trimmed");
        assert_eq!(a.summary.as_deref(), Some("a summary"));
        assert_eq!(a.sections, vec!["rust"], "tags are normalised and deduped");
        assert_eq!(a.pubkey, "abcd", "the pubkey is lowercased");
        assert_eq!(a.published_at, Some(10));
        assert_eq!(a.address(), "30023:abcd:slug");
    }

    #[test]
    fn a_note_is_never_an_article() {
        assert!(to_article(&event(1, json!([["d", "x"], ["title", "T"]]))).is_none());
    }

    #[test]
    fn a_missing_or_empty_d_or_title_is_refused() {
        assert!(to_article(&event(KIND_LONG_FORM, json!([["title", "T"]]))).is_none());
        assert!(to_article(&event(KIND_LONG_FORM, json!([["d", "  "], ["title", "T"]]))).is_none());
        assert!(to_article(&event(KIND_LONG_FORM, json!([["d", "x"]]))).is_none());
        assert!(to_article(&event(KIND_LONG_FORM, json!([["d", "x"], ["title", " "]]))).is_none());
    }
}

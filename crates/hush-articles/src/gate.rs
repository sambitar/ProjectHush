//! The quality gate: what turns a public event into a neighborhood.

use crate::nip23::{to_article, Article, RawEvent};

/// Words per minute used to turn a body into a reading time.
const WORDS_PER_MINUTE: usize = 200;

/// The gate that keeps short posts out of a long-form surface.
///
/// `kind:30023` is the first and strongest cut — a `kind:1` note cannot arrive as
/// one, so a tweet cannot pass no matter what else it carries. The reading-time
/// floor is the second: a 30023 event that is a tweet-sized stub is not a
/// neighborhood either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    min_minutes: u32,
}

impl Gate {
    pub fn new(min_minutes: u32) -> Self {
        Self { min_minutes }
    }

    pub fn min_minutes(&self) -> u32 {
        self.min_minutes
    }

    /// The article this event is, if it passes the gate.
    pub fn admits(&self, event: &RawEvent) -> Option<Article> {
        let article = to_article(event)?;
        if reading_minutes(&article.content) < self.min_minutes {
            return None;
        }
        Some(article)
    }
}

/// Reading time in whole minutes, rounded up, at [`WORDS_PER_MINUTE`]. Empty
/// text is 0, so an empty body fails any floor above zero.
pub fn reading_minutes(content: &str) -> u32 {
    let words = content.split_whitespace().count();
    if words == 0 {
        return 0;
    }
    words.div_ceil(WORDS_PER_MINUTE) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: u64, content: &str, tags: serde_json::Value) -> RawEvent {
        serde_json::from_value(json!({
            "id": "id-1",
            "pubkey": "ab",
            "created_at": 1,
            "kind": kind,
            "tags": tags,
            "content": content,
        }))
        .unwrap()
    }

    fn long_form(content: &str) -> RawEvent {
        event(
            crate::KIND_LONG_FORM,
            content,
            json!([["d", "x"], ["title", "T"]]),
        )
    }

    #[test]
    fn reading_time_rounds_up_and_empty_is_zero() {
        assert_eq!(reading_minutes(""), 0);
        assert_eq!(reading_minutes("one"), 1);
        assert_eq!(reading_minutes(&"w ".repeat(200)), 1);
        assert_eq!(reading_minutes(&"w ".repeat(201)), 2);
        assert_eq!(reading_minutes(&"w ".repeat(600)), 3);
    }

    #[test]
    fn the_floor_admits_a_real_article_and_refuses_a_stub() {
        let gate = Gate::new(3);
        // 3 minutes at 200 wpm is 401..600 words; the boundary is the point.
        assert!(gate.admits(&long_form(&"w ".repeat(401))).is_some());
        assert!(gate.admits(&long_form(&"w ".repeat(400))).is_none());
    }

    #[test]
    fn a_zero_floor_admits_anything_with_the_tags() {
        let gate = Gate::new(0);
        assert!(gate.admits(&long_form("")).is_some());
        assert!(gate.admits(&event(1, "long enough", json!([]))).is_none());
    }

    #[test]
    fn a_note_is_refused_even_when_it_is_long() {
        let gate = Gate::new(3);
        let note = event(
            1,
            &"w ".repeat(600),
            json!([["d", "x"], ["title", "T"]]),
        );
        assert!(gate.admits(&note).is_none());
    }
}

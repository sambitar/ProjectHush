//! Export: a private zine re-encoded as a public NIP-23 article.
//!
//! This is the one-way door. Everything about the export is in what
//! [`ExportPlan`] *is*: it carries a `d`, a title, a summary, sections and a
//! Markdown body, and it has no field for a comment thread, a revision history,
//! a read key or a pen name. Stripping the private parts of a zine is therefore
//! not a step that can be forgotten — there is nowhere for them to be carried.
//!
//! What this module does not do is talk to anyone, or hold a key. It builds the
//! event and the address; [`crate::event`] hashes and frames it, the vault signs
//! it, and [`crate::relay`] sends it.

use crate::event::{naddr, UnsignedEvent};
use crate::nip23::KIND_LONG_FORM;

/// How long a derived summary may be, in characters. Long enough to be worth
/// reading on a card, short enough not to be the article.
const SUMMARY_MAX_CHARS: usize = 200;

/// One zine, ready to be published: the whole of the boundary between the
/// private corpus and this crate.
///
/// The body arrives **already re-encoded to Markdown**. That re-encode is a
/// daemon-side step (`reads::rich_text_to_markdown`), because it is a fact about
/// the rich-text format the daemon owns, not about Nostr — and because it keeps
/// this crate free of an HTML parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportPlan {
    /// The NIP-23 `d` tag: the addressable id within the author's namespace.
    ///
    /// Minted once, on the first export, and then stable forever, because
    /// NIP-23 is editable *by address*: a second export under a new `d` would
    /// publish a second article beside the first rather than updating it. It is
    /// deliberately **not** the zine id — that would be a stable link from a
    /// public post back to a private one, for anyone holding both.
    pub d: String,
    pub title: String,
    /// What a reader sees before the article, on a card or in a feed. Derived
    /// from the body by [`summary_of`] when the author has not written one.
    pub summary: Option<String>,
    /// The `t` tags, already normalised by the caller.
    pub sections: Vec<String>,
    /// The article body, Markdown.
    pub markdown: String,
    /// When this article first went public, seconds. Set once and never moved:
    /// NIP-23's `published_at` is the first publish, so an edit that rewrote it
    /// would claim the article was written later than it was.
    pub published_at: u64,
}

impl ExportPlan {
    /// The NIP-23 tags, in the order the format reads best.
    ///
    /// `d` and `title` come first because they are what the gate requires: a
    /// reader's own admission rule is `kind:30023` **and** a `d` **and** a
    /// `title`, so an export that omitted either would be published to a network
    /// that then hides it.
    pub fn tags(&self) -> Vec<Vec<String>> {
        let mut tags = vec![
            vec!["d".to_string(), self.d.clone()],
            vec!["title".to_string(), self.title.clone()],
        ];
        if let Some(summary) = self.summary.as_deref().filter(|s| !s.trim().is_empty()) {
            tags.push(vec!["summary".to_string(), summary.trim().to_string()]);
        }
        for section in &self.sections {
            let section = section.trim();
            if !section.is_empty() {
                tags.push(vec!["t".to_string(), section.to_string()]);
            }
        }
        tags.push(vec![
            "published_at".to_string(),
            self.published_at.to_string(),
        ]);
        tags
    }

    /// The NIP-01 addressable id, the `a` tag: `kind:pubkey:d`.
    pub fn address(&self, pubkey: &str) -> String {
        format!("{KIND_LONG_FORM}:{}:{}", pubkey.trim(), self.d)
    }

    /// The event to sign. `created_at` is the *edit* time, which moves on every
    /// export; `published_at` in the tags is the first publish and does not.
    pub fn event(&self, pubkey: &str, created_at: u64) -> Result<UnsignedEvent, String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err("an export needs a title: the gate requires one".to_string());
        }
        if self.d.trim().is_empty() {
            return Err("an export needs a `d`".to_string());
        }
        UnsignedEvent::new(
            pubkey,
            created_at,
            KIND_LONG_FORM,
            self.tags(),
            self.markdown.clone(),
        )
    }

    /// The shareable handle for this article.
    pub fn naddr(&self, pubkey: &str, relays: &[String]) -> Result<String, String> {
        naddr(KIND_LONG_FORM, pubkey, &self.d, relays)
    }
}

/// A summary derived from a Markdown body, for when the author wrote none.
///
/// The first paragraph that is actually prose — not a heading, a fence, a rule
/// or a list marker — with its inline markers removed, collapsed to one line and
/// cut at a word boundary.
///
/// This is a preview of the body's own words, not new text: a card with no
/// preview reads as a broken card, and a summary assembled from the opening line
/// is what the reader was about to read anyway. `None` when the body opens with
/// nothing that can stand in for prose, rather than inventing a placeholder.
pub fn summary_of(markdown: &str) -> Option<String> {
    let paragraph = markdown
        .split("\n\n")
        .map(str::trim)
        .find(|block| !block.is_empty() && is_prose(block))?;
    let text = plain_text(paragraph);
    if text.is_empty() {
        return None;
    }
    Some(truncate_words(&text, SUMMARY_MAX_CHARS))
}

/// Whether a block can stand in for prose: not a heading, a fence, a rule, a
/// list, a quote or a table row.
fn is_prose(block: &str) -> bool {
    let first = block.lines().next().unwrap_or_default().trim_start();
    if first.starts_with('#')
        || first.starts_with("```")
        || first.starts_with("~~~")
        || first.starts_with('>')
        || first.starts_with("|")
    {
        return false;
    }
    // A rule, a list marker, or an ordered-list marker.
    if is_rule(first) {
        return false;
    }
    !first.starts_with("- ") && !first.starts_with("* ") && !first.starts_with("+ ")
}

fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && trimmed
            .chars()
            .all(|c| c == '-' || c == '*' || c == '_' || c.is_whitespace())
        && trimmed.chars().filter(|c| !c.is_whitespace()).count() >= 3
}

/// The words of a Markdown block, with the markers taken out.
///
/// Deliberately shallow: enough that a card does not show `**bold**` or a raw
/// link, not a second Markdown parser.
fn plain_text(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    let mut rest = block;
    while !rest.is_empty() {
        // `[label](url)` reads as its label.
        if rest.starts_with('[') {
            if let Some(close) = rest.find("](") {
                if let Some(end) = rest[close..].find(')') {
                    out.push_str(&rest[1..close]);
                    rest = &rest[close + end + 1..];
                    continue;
                }
            }
        }
        for marker in ["**", "~~", "`", "*", "_"] {
            if let Some(stripped) = rest.strip_prefix(marker) {
                rest = stripped;
                continue;
            }
        }
        let Some(c) = rest.chars().next() else {
            break;
        };
        if !matches!(c, '*' | '~' | '`' | '_') {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut at a word boundary, with an ellipsis only when something was dropped.
fn truncate_words(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut cut = String::new();
    for word in text.split(' ') {
        if cut.chars().count() + word.chars().count() + 1 > max_chars {
            break;
        }
        if !cut.is_empty() {
            cut.push(' ');
        }
        cut.push_str(word);
    }
    // A single word longer than the budget is cut mid-word rather than dropped.
    if cut.is_empty() {
        cut = text.chars().take(max_chars).collect();
    }
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn plan() -> ExportPlan {
        ExportPlan {
            d: "a-minted-id".to_string(),
            title: "On relays".to_string(),
            summary: Some("What a relay is for.".to_string()),
            sections: vec!["rust".to_string(), "nostr".to_string()],
            markdown: "# On relays\n\nThe body.".to_string(),
            published_at: 1_700_000_000,
        }
    }

    /// The tags are the article's contract with every other client: `d` and
    /// `title` because the gate requires them, `t` per section, `published_at`
    /// once.
    #[test]
    fn the_tags_are_the_nip23_set_in_order() {
        assert_eq!(
            plan().tags(),
            vec![
                vec!["d".to_string(), "a-minted-id".to_string()],
                vec!["title".to_string(), "On relays".to_string()],
                vec!["summary".to_string(), "What a relay is for.".to_string()],
                vec!["t".to_string(), "rust".to_string()],
                vec!["t".to_string(), "nostr".to_string()],
                vec!["published_at".to_string(), "1700000000".to_string()],
            ]
        );
    }

    #[test]
    fn an_absent_or_blank_summary_is_not_a_tag_and_a_blank_section_is_dropped() {
        let mut p = plan();
        p.summary = None;
        p.sections = vec!["rust".to_string(), "   ".to_string()];
        let tags = p.tags();
        assert!(!tags.iter().any(|t| t[0] == "summary"));
        assert_eq!(tags.iter().filter(|t| t[0] == "t").count(), 1);

        p.summary = Some("  ".to_string());
        assert!(!p.tags().iter().any(|t| t[0] == "summary"));
    }

    /// The address is what a comment references and what a re-export must
    /// replace, so it has to be exactly `kind:pubkey:d`.
    #[test]
    fn the_address_is_kind_pubkey_d() {
        assert_eq!(
            plan().address(PUBKEY),
            format!("30023:{PUBKEY}:a-minted-id")
        );
    }

    /// An export that omitted the `title` the gate requires would be published
    /// into a network that then hides it — a failure the author would never see
    /// from this side.
    #[test]
    fn an_export_without_a_title_or_a_d_is_refused() {
        let mut p = plan();
        p.title = "   ".to_string();
        let error = p.event(PUBKEY, 10).unwrap_err();
        assert!(error.contains("title"), "{error}");

        let mut p = plan();
        p.d = String::new();
        assert!(p.event(PUBKEY, 10).unwrap_err().contains('`'));

        // And a real one builds, with the body as the content and the kind the
        // gate will read.
        let event = plan().event(PUBKEY, 10).unwrap();
        assert_eq!(event.kind, KIND_LONG_FORM);
        assert_eq!(event.content, "# On relays\n\nThe body.");
        assert_eq!(event.pubkey, PUBKEY);
        assert_eq!(event.created_at, 10, "created_at is the edit time");
        assert!(event.tags.contains(&vec!["d".to_string(), "a-minted-id".to_string()]));
    }

    /// The one-way door, asserted structurally: a zine's private parts have
    /// nowhere to be carried. This is what "strip the comment thread and the
    /// revision history" means here — not a filter that could be skipped, but an
    /// absence of fields.
    #[test]
    fn a_plan_carries_no_private_zine_state() {
        let p = plan();
        let tags = p.tags();
        let content = &p.markdown;
        for forbidden in ["read_key", "pen_name", "pen_id", "zine_id", "comment", "prev_hash"] {
            assert!(
                !tags.iter().flatten().any(|v| v.contains(forbidden)),
                "`{forbidden}` reached the tags"
            );
            assert!(!content.contains(forbidden), "`{forbidden}` reached the body");
        }
    }

    /// The `published_at` is set once and the `created_at` moves: an edit is a
    /// new event at the same address, and a re-export must not claim the article
    /// was first published now.
    #[test]
    fn an_edit_moves_created_at_and_leaves_published_at_alone() {
        let first = plan().event(PUBKEY, 1_000).unwrap();
        let edited = plan().event(PUBKEY, 2_000).unwrap();
        assert_ne!(first.id, edited.id, "an edit is a new event");
        assert_eq!(first.pubkey, edited.pubkey);
        let published = |e: &UnsignedEvent| {
            e.tags
                .iter()
                .find(|t| t[0] == "published_at")
                .map(|t| t[1].clone())
        };
        assert_eq!(published(&first), published(&edited));
        assert_eq!(published(&first), Some("1700000000".to_string()));
    }

    #[test]
    fn a_summary_is_the_opening_prose_with_its_markers_taken_out() {
        assert_eq!(
            summary_of("# Heading\n\nThe **first** paragraph, with a [link](https://x.example).\n\nMore."),
            Some("The first paragraph, with a link.".to_string())
        );
        // A body that opens with a list or a fence still has prose after it.
        assert_eq!(
            summary_of("- item\n- item\n\nThen the prose."),
            Some("Then the prose.".to_string())
        );
        assert_eq!(
            summary_of("```\ncode\n```\n\nAfter the fence."),
            Some("After the fence.".to_string())
        );
        assert_eq!(summary_of("---\n\nAfter a rule."), Some("After a rule.".to_string()));
    }

    #[test]
    fn a_body_with_no_prose_has_no_summary_rather_than_a_placeholder() {
        assert_eq!(summary_of(""), None);
        assert_eq!(summary_of("# Only a heading"), None);
        assert_eq!(summary_of("```\ncode only\n```"), None);
        assert_eq!(summary_of("- only\n- items"), None);
    }

    #[test]
    fn a_long_summary_is_cut_at_a_word_boundary_with_an_ellipsis() {
        let body = "word ".repeat(100);
        let summary = summary_of(body.trim()).unwrap();
        assert!(summary.ends_with('…'), "{summary}");
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS + 1, "{summary}");
        assert!(!summary.contains("  "), "collapsed to one line: {summary}");
        assert!(summary.starts_with("word word"), "{summary}");

        // A short one is left exactly as it was, with no ellipsis.
        assert_eq!(summary_of("Short and complete.").unwrap(), "Short and complete.");
    }

    #[test]
    fn a_single_word_longer_than_the_budget_is_cut_rather_than_dropped() {
        let summary = summary_of(&"x".repeat(500)).unwrap();
        assert_eq!(summary.chars().count(), SUMMARY_MAX_CHARS + 1);
        assert!(summary.ends_with('…'));
    }
}

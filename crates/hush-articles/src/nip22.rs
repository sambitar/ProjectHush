//! NIP-22: public comments on an article, and the threading they carry.
//!
//! A comment is scoped twice. The **root** is the article the thread belongs to,
//! in uppercase tags (`A`, `K`, `P`); the **parent** is the thing this comment
//! answers, in lowercase ones (`a`, `e`, `k`, `p`). For a top-level comment the
//! parent is the article itself, which is why the lowercase tags repeat the
//! uppercase ones rather than being absent: a client that reads only the parent
//! pair would otherwise see a comment with no parent at all.
//!
//! That pairing is what makes a reply possible without a second kind. A reply to
//! a comment keeps the *same* root — the article — and moves only the parent to
//! `k:1111` and that comment's id. The kind of the parent is what tells the two
//! apart, so [`to_comment`] reads `k` and not `e`: reading `e` alone would file
//! every top-level comment as a reply to the article.
//!
//! Comments are plaintext by the spec, and this crate keeps them that way. What
//! it does not do is talk to anyone, or hold a key: [`CommentPlan`] builds the
//! event, [`crate::event`] hashes and frames it, the vault signs it, and
//! [`crate::relay`] sends it.

use serde::{Deserialize, Serialize};

use crate::event::UnsignedEvent;
use crate::nip23::{RawEvent, KIND_LONG_FORM};

/// The NIP-22 comment kind. A draft NIP, but the shape is what other clients
/// read, and the shape is all this crate depends on.
pub const KIND_COMMENT: u64 = 1111;

/// One comment, mapped from a `kind:1111` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub event_id: String,
    /// The commenter's public key, hex, lowercased. A public key, not a name:
    /// the lane fetches no kind:0 metadata, so the page names the key it holds.
    pub pubkey: String,
    /// Plaintext. The spec forbids HTML, Markdown and any other formatting.
    pub content: String,
    pub created_at: u64,
    /// The `A` tag: the article this comment is filed under, as
    /// `30023:pubkey:d`. This is what a thread is looked up by.
    pub root: String,
    /// The parent comment's id, when this answers a comment rather than the
    /// article. `None` is a top-level comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// Map a raw event to a comment, structurally.
///
/// `None` when the event is not a `kind:1111`, when it is not filed under a
/// NIP-23 article, or when it carries nothing to read. The lane is a NIP-23
/// surface, so a comment about some other root is not one of its comments — the
/// same rule that keeps a short note out of the article cache.
pub fn to_comment(event: &RawEvent) -> Option<Comment> {
    if event.kind != KIND_COMMENT {
        return None;
    }
    let root = normalise_root(event.tag("A")?)?;
    let content = event.content.trim();
    if content.is_empty() {
        return None;
    }
    Some(Comment {
        event_id: event.id.clone(),
        pubkey: event.pubkey.trim().to_ascii_lowercase(),
        content: content.to_string(),
        created_at: event.created_at,
        root,
        parent_id: parent_of(event),
    })
}

/// The parent comment's id, when this is a reply to a comment rather than a
/// comment on the article.
fn parent_of(event: &RawEvent) -> Option<String> {
    if event.tag("k")?.trim() != KIND_COMMENT.to_string() {
        return None;
    }
    let id = event.tag("e")?.trim();
    if id.is_empty() {
        return None;
    }
    Some(id.to_ascii_lowercase())
}

/// Normalise an `A`/`a` value to the address form [`crate::Article::address`]
/// produces.
///
/// The pubkey is hex and case-insensitive, so it is lowercased; the `d` is an
/// arbitrary string and is **not**, or two comments on the same article would
/// disagree about where they are filed. Anything that is not an addressable
/// long-form address is refused rather than passed through, so a root that
/// cannot name an article cannot enter the reply cache.
fn normalise_root(raw: &str) -> Option<String> {
    let (kind, rest) = raw.split_once(':')?;
    let (pubkey, d) = rest.split_once(':')?;
    if kind.trim() != KIND_LONG_FORM.to_string() {
        return None;
    }
    let pubkey = pubkey.trim();
    if pubkey.is_empty() || d.is_empty() {
        return None;
    }
    Some(format!("{KIND_LONG_FORM}:{}:{d}", pubkey.to_ascii_lowercase()))
}

/// The article a comment is filed under: NIP-22's root scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentRoot {
    /// The NIP-01 addressable id, `30023:pubkey:d`.
    pub address: String,
    /// The article version's event id. NIP-22 asks for it beside the address, so
    /// a client that cannot resolve an addressable id still lands on a version.
    pub event_id: String,
    /// The article's author, hex. The `P` tag.
    pub pubkey: String,
}

/// What a comment answers: NIP-22's parent item.
///
/// Only the parts a reply needs. The parent is always another comment, so its
/// kind is [`KIND_COMMENT`] by construction and is not a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentParent {
    /// The parent comment's event id.
    pub event_id: String,
    /// The parent comment's author, hex. The `p` tag.
    pub pubkey: String,
}

/// One comment, ready to be signed and posted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentPlan {
    pub root: CommentRoot,
    /// `None` is a top-level comment on the article; `Some` answers a comment.
    pub parent: Option<CommentParent>,
    /// Plaintext, already normalised by the caller. NIP-22 forbids formatting
    /// here, and so does the composer.
    pub content: String,
    /// Relay hints for the tags that can carry one. The allowlist, so a reader
    /// with no relays of its own can still find what is referenced.
    pub relays: Vec<String>,
}

impl CommentPlan {
    /// The NIP-22 tags, root first.
    ///
    /// One relay hint per tag rather than all of them: the fourth element is
    /// positional — the spec puts the referenced author's key there for an `e`
    /// tag — so a second relay would push a key into a slot a client reads as a
    /// relay. With no allowlist the tags carry no hint and no fourth element,
    /// which loses nothing: the `P` and `p` tags name the authors anyway.
    pub fn tags(&self) -> Vec<Vec<String>> {
        let relay = self.relays.first().map(String::as_str);
        let mut tags = vec![
            hinted("A", &self.root.address, relay, None),
            vec!["K".to_string(), KIND_LONG_FORM.to_string()],
            hinted("P", &self.root.pubkey, relay, None),
        ];
        match &self.parent {
            Some(parent) => {
                tags.push(hinted("e", &parent.event_id, relay, Some(&parent.pubkey)));
                tags.push(vec!["k".to_string(), KIND_COMMENT.to_string()]);
                tags.push(hinted("p", &parent.pubkey, relay, None));
            }
            // A top-level comment's parent *is* the root. The spec's own example
            // repeats it in the lowercase tags, and a client that reads only the
            // parent pair would otherwise see a comment with no parent.
            None => {
                tags.push(hinted("a", &self.root.address, relay, None));
                tags.push(hinted(
                    "e",
                    &self.root.event_id,
                    relay,
                    Some(&self.root.pubkey),
                ));
                tags.push(vec!["k".to_string(), KIND_LONG_FORM.to_string()]);
                tags.push(hinted("p", &self.root.pubkey, relay, None));
            }
        }
        tags
    }

    /// The event to sign. `created_at` is when the comment was posted; a comment
    /// is a regular event, so it never moves.
    pub fn event(&self, pubkey: &str, created_at: u64) -> Result<UnsignedEvent, String> {
        let content = self.content.trim();
        if content.is_empty() {
            return Err("a comment needs something in it".to_string());
        }
        if self.root.address.trim().is_empty() || self.root.event_id.trim().is_empty() {
            return Err("a comment needs the article it is filed under".to_string());
        }
        UnsignedEvent::new(
            pubkey,
            created_at,
            KIND_COMMENT,
            self.tags(),
            content.to_string(),
        )
    }
}

/// A tag with an optional relay hint and an optional key in the fourth slot.
fn hinted(name: &str, value: &str, relay: Option<&str>, pubkey: Option<&str>) -> Vec<String> {
    let mut tag = vec![name.to_string(), value.to_string()];
    if let Some(relay) = relay {
        tag.push(relay.to_string());
        if let Some(pubkey) = pubkey {
            tag.push(pubkey.to_string());
        }
    }
    tag
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ARTICLE_AUTHOR: &str = "3c9849383bdea883b0bd16fece1ed36d37e37cdde3ce43b17ea4e9192ec11289";
    const COMMENT_AUTHOR: &str = "252f10c83610ebca1a059c0bae8255eba2f95be4d1d7bcfa89d7248a82d9f111";
    const RELAY: &str = "wss://relay.example";
    const ADDRESS: &str = "30023:3c9849383bdea883b0bd16fece1ed36d37e37cdde3ce43b17ea4e9192ec11289:a-post";

    fn root() -> CommentRoot {
        CommentRoot {
            address: ADDRESS.to_string(),
            event_id: "aa".repeat(32),
            pubkey: ARTICLE_AUTHOR.to_string(),
        }
    }

    fn plan(parent: Option<CommentParent>) -> CommentPlan {
        CommentPlan {
            root: root(),
            parent,
            content: "Worth reading.".to_string(),
            relays: vec![RELAY.to_string()],
        }
    }

    fn event(kind: u64, tags: serde_json::Value, content: &str) -> RawEvent {
        serde_json::from_value(json!({
            "id": "id-1",
            "pubkey": COMMENT_AUTHOR,
            "created_at": 42,
            "kind": kind,
            "tags": tags,
            "content": content,
        }))
        .unwrap()
    }

    /// A top-level comment names the article as both its root and its parent.
    /// The lowercase pair is not decoration: a client that reads only the parent
    /// tags sees a comment with no parent if it is missing.
    #[test]
    fn a_comment_on_an_article_scopes_to_it_as_both_root_and_parent() {
        let tags = plan(None).tags();
        assert_eq!(
            tags,
            vec![
                vec!["A".to_string(), ADDRESS.to_string(), RELAY.to_string()],
                vec!["K".to_string(), "30023".to_string()],
                vec![
                    "P".to_string(),
                    ARTICLE_AUTHOR.to_string(),
                    RELAY.to_string()
                ],
                vec!["a".to_string(), ADDRESS.to_string(), RELAY.to_string()],
                vec![
                    "e".to_string(),
                    "aa".repeat(32),
                    RELAY.to_string(),
                    ARTICLE_AUTHOR.to_string()
                ],
                vec!["k".to_string(), "30023".to_string()],
                vec![
                    "p".to_string(),
                    ARTICLE_AUTHOR.to_string(),
                    RELAY.to_string()
                ],
            ]
        );
    }

    /// A reply keeps the article as its root and moves only the parent. That is
    /// the whole of NIP-22 threading: no second kind, no nesting in the wire.
    #[test]
    fn a_reply_scopes_to_the_article_and_points_at_the_comment() {
        let tags = plan(Some(CommentParent {
            event_id: "bb".repeat(32),
            pubkey: COMMENT_AUTHOR.to_string(),
        }))
        .tags();
        assert_eq!(
            tags,
            vec![
                vec!["A".to_string(), ADDRESS.to_string(), RELAY.to_string()],
                vec!["K".to_string(), "30023".to_string()],
                vec![
                    "P".to_string(),
                    ARTICLE_AUTHOR.to_string(),
                    RELAY.to_string()
                ],
                vec![
                    "e".to_string(),
                    "bb".repeat(32),
                    RELAY.to_string(),
                    COMMENT_AUTHOR.to_string()
                ],
                vec!["k".to_string(), "1111".to_string()],
                vec![
                    "p".to_string(),
                    COMMENT_AUTHOR.to_string(),
                    RELAY.to_string()
                ],
            ],
            "the root stays the article; only the parent moves"
        );
    }

    /// One hint, not all of them: the fourth element is positional, so a second
    /// relay would push the key into a slot a client reads as a relay.
    #[test]
    fn only_the_first_relay_is_a_hint_and_a_second_one_is_not_a_tag() {
        let mut p = plan(None);
        p.relays = vec![RELAY.to_string(), "wss://other.example".to_string()];
        let tags = p.tags();
        assert_eq!(tags[0].len(), 3);
        assert!(!tags.iter().flatten().any(|v| v == "wss://other.example"));
    }

    /// With no allowlist the tags carry no hint at all — and then no fourth
    /// element either, because the slot it would sit in is the relay's.
    #[test]
    fn with_no_allowlist_the_tags_carry_no_hint() {
        let mut p = plan(None);
        p.relays.clear();
        let tags = p.tags();
        assert!(tags.iter().all(|t| t.len() == 2), "{tags:?}");
        assert_eq!(tags[0], vec!["A".to_string(), ADDRESS.to_string()]);
    }

    #[test]
    fn an_empty_comment_or_one_with_no_article_is_refused() {
        let mut p = plan(None);
        p.content = "   ".to_string();
        assert!(p.event(COMMENT_AUTHOR, 10).unwrap_err().contains("in it"));

        let mut p = plan(None);
        p.root.event_id = String::new();
        assert!(p.event(COMMENT_AUTHOR, 10)
            .unwrap_err()
            .contains("filed under"));
    }

    #[test]
    fn a_comment_builds_as_kind_1111_with_the_plaintext_body() {
        let built = plan(None).event(COMMENT_AUTHOR, 10).unwrap();
        assert_eq!(built.kind, KIND_COMMENT);
        assert_eq!(built.content, "Worth reading.");
        assert_eq!(built.pubkey, COMMENT_AUTHOR);
        assert_eq!(built.created_at, 10);
    }

    #[test]
    fn a_long_form_comment_maps_to_a_comment() {
        let mapped = to_comment(&event(
            KIND_COMMENT,
            json!([
                ["A", ADDRESS],
                ["K", "30023"],
                ["P", ARTICLE_AUTHOR],
                ["a", ADDRESS],
                ["e", "aa"],
                ["k", "30023"],
                ["p", ARTICLE_AUTHOR],
            ]),
            "  Great post.  ",
        ))
        .unwrap();
        assert_eq!(mapped.content, "Great post.", "the body is trimmed");
        assert_eq!(mapped.root, ADDRESS);
        assert_eq!(mapped.pubkey, COMMENT_AUTHOR);
        assert_eq!(
            mapped.parent_id, None,
            "a top-level comment carries the article's id in `e`, and reading `e` \
             alone would file it as a reply to the article"
        );
    }

    #[test]
    fn a_reply_to_a_comment_maps_with_its_parent() {
        let mapped = to_comment(&event(
            KIND_COMMENT,
            json!([
                ["A", ADDRESS],
                ["K", "30023"],
                ["P", ARTICLE_AUTHOR],
                ["e", "BB"],
                ["k", "1111"],
                ["p", ARTICLE_AUTHOR],
            ]),
            "Same here.",
        ))
        .unwrap();
        assert_eq!(mapped.parent_id.as_deref(), Some("bb"), "lowercased");
        assert_eq!(mapped.root, ADDRESS, "the root is still the article");
    }

    /// The address is `kind:pubkey:d` and only the pubkey is case-insensitive.
    /// Lowercasing the `d` would file two comments on one article under two
    /// roots, and neither would match the article they were written on.
    #[test]
    fn the_root_normalises_the_pubkey_and_leaves_the_d_alone() {
        let mapped = to_comment(&event(
            KIND_COMMENT,
            json!([["A", "30023:ABCD:CaseSensitive-D"], ["K", "30023"]]),
            "body",
        ))
        .unwrap();
        assert_eq!(mapped.root, "30023:abcd:CaseSensitive-D");
    }

    /// The lane is a NIP-23 surface. A comment about another root, or a note,
    /// or one with nothing in it, is not one of its comments.
    #[test]
    fn anything_that_is_not_a_comment_on_an_article_is_refused() {
        let on_article = json!([["A", ADDRESS], ["K", "30023"]]);
        assert!(to_comment(&event(1, on_article.clone(), "body")).is_none());
        assert!(
            to_comment(&event(KIND_COMMENT, on_article.clone(), "body")).is_some(),
            "the shape itself is fine"
        );
        assert!(to_comment(&event(KIND_COMMENT, json!([["K", "30023"]]), "body")).is_none());
        assert!(to_comment(&event(
            KIND_COMMENT,
            json!([["A", "30023:abcd"], ["K", "30023"]]),
            "body"
        ))
        .is_none(), "not an address");
        assert!(to_comment(&event(
            KIND_COMMENT,
            json!([["A", "1:abcd:d"], ["K", "1"]]),
            "body"
        ))
        .is_none(), "not an article");
        assert!(to_comment(&event(
            KIND_COMMENT,
            json!([["A", "30023:abcd:"] , ["K", "30023"]]),
            "body"
        ))
        .is_none(), "an empty `d` names no article");
        assert!(to_comment(&event(KIND_COMMENT, on_article, "   ")).is_none());
    }
}

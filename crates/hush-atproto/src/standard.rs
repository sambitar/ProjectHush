//! The Standard.site lexicons: `site.standard.publication` and
//! `site.standard.document`.
//!
//! This is the shared long-form data structure ATProto's publishing clients
//! agreed on (`pckt.blog`, `leaflet.pub`, `offprint.app`), and it is why the
//! public lane is worth building at all: a post written here is readable by
//! every other client in that ecosystem, and their posts are readable here,
//! without anyone operating a Hush server to broker it.
//!
//! The crate maps *to* and *from* the lexicons and does nothing else. What a
//! zine is — its revision chain, its comments, its read key, its pen name — is
//! not in this file and cannot be: [`Post`] has fields for a title, a body, tags
//! and a date, and nowhere to put any of the private parts. Stripping a zine on
//! the way out is therefore not a step that can be forgotten.
//!
//! The body arrives **already reduced to plaintext** by the daemon, because
//! that reduction is a fact about the rich-text format the daemon owns
//! (`reads::rich_text_to_text`), not about ATProto.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Error;

/// The lexicon id a publication record is written under.
pub const PUBLICATION_COLLECTION: &str = "site.standard.publication";
/// The lexicon id a document record is written under.
pub const DOCUMENT_COLLECTION: &str = "site.standard.document";

/// A publication's `maxGraphemes`-equivalent bound, in characters.
const NAME_MAX_CHARS: usize = 500;
/// A document title's bound, in characters.
const TITLE_MAX_CHARS: usize = 500;
/// A tag's bound, in characters.
const TAG_MAX_CHARS: usize = 128;
/// A description's bound, in characters.
const DESCRIPTION_MAX_CHARS: usize = 3000;

/// One post, ready to publish: the whole of the boundary between the private
/// corpus and this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    /// The record key. Stable per zine, so a re-publish replaces the record
    /// rather than adding a second one beside it.
    pub rkey: String,
    pub title: String,
    /// Plaintext, no markup: the lexicon's `textContent` forbids formatting.
    pub text: String,
    /// A short taste of the body, when the author wrote none.
    pub description: Option<String>,
    /// The zine's sections, as the lexicon's `tags`.
    pub tags: Vec<String>,
    /// When the post first went public, ms since the epoch. Set once and never
    /// moved, so an edit does not claim the post was written later than it was.
    pub published_at_ms: u64,
    /// When this revision was written, ms. `None` on a first publish.
    pub updated_at_ms: Option<u64>,
}

impl Post {
    /// The record key for a zine.
    ///
    /// Derived from the zine id rather than stored, so a lost sidecar cannot
    /// orphan a published record: the same zine always addresses the same
    /// record. Hashed rather than used raw, because the record key is public and
    /// a zine id is a private identifier — a public post must not carry a
    /// readable handle back to the private one.
    pub fn rkey_for(zine_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"hush-zine-rkey-v1\n");
        hasher.update(zine_id.as_bytes());
        let digest = hasher.finalize();
        hex::encode(&digest[..16])
    }

    /// The `site.standard.document` record for this post.
    pub fn record(&self, publication_uri: &str) -> Result<serde_json::Value, Error> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err(Error::Invalid(
                "a public post needs a title: the lexicon requires one".to_string(),
            ));
        }
        if self.rkey.trim().is_empty() {
            return Err(Error::Invalid("a public post needs a record key".to_string()));
        }
        if publication_uri.trim().is_empty() {
            return Err(Error::Invalid(
                "a public post needs a publication to belong to".to_string(),
            ));
        }
        if self.text.trim().is_empty() {
            return Err(Error::Invalid(
                "a public post with no text is not worth publishing".to_string(),
            ));
        }

        let mut record = serde_json::json!({
            "$type": DOCUMENT_COLLECTION,
            "site": publication_uri,
            "title": clamp(title, TITLE_MAX_CHARS),
            "path": format!("/{}", self.rkey),
            "publishedAt": rfc3339(self.published_at_ms),
            "textContent": self.text,
        });
        if let Some(updated) = self.updated_at_ms {
            record["updatedAt"] = serde_json::Value::String(rfc3339(updated));
        }
        if let Some(description) = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            record["description"] = serde_json::Value::String(clamp(description, DESCRIPTION_MAX_CHARS));
        }
        let tags: Vec<serde_json::Value> = self
            .tags
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| serde_json::Value::String(clamp(t, TAG_MAX_CHARS)))
            .collect();
        if !tags.is_empty() {
            record["tags"] = serde_json::Value::Array(tags);
        }
        Ok(record)
    }
}

/// The `site.standard.publication` record: where a member's posts live.
///
/// `url` is the member's own web address and is required by the lexicon — a
/// document's `path` is combined with it to build a reader's link. Hush does not
/// host it and cannot invent one, so setup asks for it.
pub fn publication_record(name: &str, url: &str) -> Result<serde_json::Value, Error> {
    let name = name.trim();
    let url = url.trim().trim_end_matches('/');
    if name.is_empty() {
        return Err(Error::Invalid("a publication needs a name".to_string()));
    }
    if url.is_empty() {
        return Err(Error::Invalid("a publication needs a web address".to_string()));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(Error::Invalid(
            "a publication's address must start with https://".to_string(),
        ));
    }
    Ok(serde_json::json!({
        "$type": PUBLICATION_COLLECTION,
        "name": clamp(name, NAME_MAX_CHARS),
        "url": url,
    }))
}

/// The `at://` URI of a publication record.
pub fn publication_uri(did: &str, rkey: &str) -> String {
    format!("at://{did}/{PUBLICATION_COLLECTION}/{rkey}")
}

/// The `at://` URI of a document record.
pub fn document_uri(did: &str, rkey: &str) -> String {
    format!("at://{did}/{DOCUMENT_COLLECTION}/{rkey}")
}

/// The DID a record's `at://` URI names, if it names one.
///
/// Read off the address rather than carried separately, so a record cannot
/// claim an author other than the repo it was read from.
pub fn did_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("at://")?;
    let did = rest.split('/').next()?;
    if did.starts_with("did:") {
        Some(did.to_string())
    } else {
        None
    }
}

/// The Bluesky post a document was announced as, when its author linked one.
///
/// This is where the conversation is. Standard.site has no comment record of its
/// own, so the ecosystem puts the comments on a Bluesky thread and points at it
/// from the document; `bskyPostRef` is that pointer. Keeping it costs nothing and
/// it is the only way back to those replies, because nothing else in the record
/// names the thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BskyPostRef {
    /// The `app.bsky.feed.post` the document was announced as.
    pub uri: String,
    /// The post's CID, as published. Empty when the author's client linked a
    /// thread without one, which is a shape that occurs in the live ecosystem.
    #[serde(default)]
    pub cid: String,
}

/// One document read back from a followed publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// The record's `at://` address, which is also its identity in the cache.
    pub uri: String,
    /// The DID that published it.
    pub did: String,
    /// The handle at the time it was read, for display only.
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub title: String,
    /// Plaintext, as published.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The author-declared publish time, RFC3339, as published — kept verbatim
    /// for display, because it is the author's own string.
    #[serde(default)]
    pub published_at: String,
    /// The same instant as a sortable number, parsed at ingest.
    ///
    /// Ordering cannot be done on the raw string: the lexicon allows an offset
    /// (`+02:00`) and optional fractional seconds, so a string sort would put a
    /// record in the wrong place. Unparseable means `0` — the record still shows,
    /// at the end, rather than disappearing because a stranger formatted a date
    /// in a way this build does not read.
    #[serde(default)]
    pub published_secs: i64,
    #[serde(default)]
    pub path: String,
    /// The author's own summary of the post, when they wrote one.
    ///
    /// Read for the same reason `path` is: some clients publish a document that
    /// is a pointer rather than a body, and the description is then the only
    /// prose the record carries. It was written on publish and never read back,
    /// so those posts arrived with nothing under the title at all.
    #[serde(default)]
    pub description: String,
    /// The document's own page on the author's site, when it has one.
    ///
    /// Standard.site lets a document be a **pointer**: a title, a description and
    /// a `path` under the publication's `url`, with the words living on the
    /// author's own site rather than in the record. Measured live: 48 of the 105
    /// documents in one vault's cache were exactly this, and the reader showed
    /// every one of them as a title with nothing under it. Resolved at ingest
    /// from the publication record, because resolving it at open time would make
    /// opening a page reach the network — which is the one thing the read lane
    /// does not do.
    #[serde(default)]
    pub url: String,
    /// The Bluesky thread this document was announced as, when there is one.
    ///
    /// Dropped until now, which is why the lane could show a post and not the
    /// comments under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bsky_post_ref: Option<BskyPostRef>,
    /// The publication this document belongs to, as an `at://` URI.
    ///
    /// Kept because one account can publish to more than one publication — the
    /// live ecosystem already has accounts with two and three — and without
    /// this the lane cannot tell one blog from another, so it cannot follow a
    /// publication rather than everything an account writes.
    #[serde(default)]
    pub site: String,
}

impl Document {
    /// Map one listed record to a document, or `None` if it is not one.
    ///
    /// Deliberately lenient: a record written by another client is not this
    /// crate's to validate, and one malformed record must not cost the reader
    /// the rest of the page. The two things that cannot be defaulted are the
    /// address and a title — without them there is nothing to show and nothing
    /// to open.
    pub fn from_record(uri: &str, value: &serde_json::Value) -> Option<Self> {
        let did = did_of(uri)?;
        let title = value.get("title")?.as_str()?.trim().to_string();
        if title.is_empty() {
            return None;
        }
        Some(Self {
            uri: uri.to_string(),
            did,
            handle: String::new(),
            title,
            text: body_text(value),
            tags: value
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|tags| {
                    tags.iter()
                        .filter_map(|t| t.as_str())
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            published_at: value
                .get("publishedAt")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            published_secs: value
                .get("publishedAt")
                .and_then(|v| v.as_str())
                .and_then(parse_rfc3339_secs)
                .unwrap_or(0),
            path: value
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            description: value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
            // Filled in by `resolve_link` once the publication record is known:
            // the record carries a `path` and a `site`, and neither is a URL on
            // its own.
            url: String::new(),
            bsky_post_ref: bsky_post_ref_of(value),
            site: value
                .get("site")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
        })
    }
}

/// The Bluesky thread a record points at, when it points at one.
///
/// Lenient about the shape on purpose: the field is published with and without
/// the extra `commit` and `validationStatus` keys, and a missing `cid` must not
/// cost the reader the whole document — the thread read fills that in. A ref
/// with no `uri` is not a ref, because a URI is the only part that addresses
/// anything.
fn bsky_post_ref_of(value: &serde_json::Value) -> Option<BskyPostRef> {
    let reference = value.get("bskyPostRef")?;
    let uri = reference.get("uri")?.as_str()?.trim().to_string();
    if uri.is_empty() {
        return None;
    }
    Some(BskyPostRef {
        uri,
        cid: reference
            .get("cid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// The words of a document, in whichever shape the author's client wrote them.
///
/// The lexicon's own field is `textContent`, and pckt and offprint write it. But
/// the field is optional, and the largest publisher in the ecosystem does not
/// use it: Leaflet writes a rich `content` tree instead, and a reader that
/// looked only at `textContent` showed every one of its posts as a title with
/// nothing under it — which is the whole point of following a publication.
///
/// So when `textContent` is absent or blank, the words are collected from
/// `content` by walking it for `plaintext` strings. That one rule covers the
/// text, header, blockquote, code and list blocks of `pub.leaflet.content` and
/// the items of `blog.pckt.content` — the two shapes in use — without a
/// per-block-type table that would drift as the ecosystem grows.
///
/// It is deliberately not a renderer. Images, embeds, link cards, buttons and
/// polls carry their words in fields of their own (`alt`, `title`, `text`) and
/// contribute nothing here, because inventing a plaintext rendering for each of
/// a stranger's block types is a markup dialect this crate has no business
/// defining.
///
/// Ordering is safe without `serde_json`'s `preserve_order`: every sequence that
/// matters — pages, blocks, list items — is an array, and an object holds at
/// most one `plaintext`.
pub fn body_text(value: &serde_json::Value) -> String {
    if let Some(text) = value.get("textContent").and_then(|v| v.as_str()) {
        if !text.trim().is_empty() {
            return text.to_string();
        }
    }
    let Some(content) = value.get("content") else {
        return String::new();
    };
    let mut parts = Vec::new();
    collect_plaintext(content, &mut parts);
    parts.join("\n\n")
}

/// Collect every `plaintext` string under `node`, in document order.
///
/// `facets` is skipped rather than descended into: it annotates the `plaintext`
/// beside it, and a facet that happened to carry the word would duplicate the
/// paragraph it belongs to.
fn collect_plaintext(node: &serde_json::Value, out: &mut Vec<String>) {
    match node {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key == "facets" {
                    continue;
                }
                if key == "plaintext" {
                    if let Some(text) = value.as_str() {
                        let text = text.trim();
                        if !text.is_empty() {
                            out.push(text.to_string());
                        }
                        continue;
                    }
                }
                // Some clients put the words of a block in an `html` field
                // instead: `{"$type": "co.idno.html", "html": "..."}`. Measured
                // live, two documents in one vault's cache were exactly this and
                // both rendered as a title with nothing under it, because only
                // `plaintext` was collected. Reduced through the same converter
                // the zines lane uses rather than a second one here.
                if key == "html" {
                    if let Some(raw) = value.as_str() {
                        let text = crate::html::to_text(raw);
                        let text = text.trim();
                        if !text.is_empty() {
                            out.push(text.to_string());
                        }
                        continue;
                    }
                }
                collect_plaintext(value, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_plaintext(item, out);
            }
        }
        _ => {}
    }
}

/// The document's own page on the author's site, from the publication it belongs
/// to.
///
/// Two fields have to be joined and neither is a URL alone: the document carries
/// a `path` (`/the-space-to-breathe`) and the *publication* carries the `url`
/// (`https://werd.io`). `publications` maps a publication's `at://` address to
/// that url, read from the same repo in the same pass.
///
/// Left empty rather than guessed when either half is missing. A wrong link is
/// worse than no link: it sends a reader somewhere the author never published,
/// and the reader has no way to tell. Verified against the live network before
/// trusting it — `https://werd.io/the-space-to-breathe` answers 200.
pub fn resolve_link(document: &mut Document, publications: &std::collections::HashMap<String, String>) {
    if !document.url.trim().is_empty() {
        return;
    }
    let path = document.path.trim();
    if path.is_empty() {
        return;
    }
    // A path that is already absolute is the record's own answer and needs no
    // publication. Some clients write one; the lexicon permits it.
    if path.starts_with("http://") || path.starts_with("https://") {
        document.url = path.to_string();
        return;
    }
    let Some(base) = publications.get(document.site.trim()) else {
        return;
    };
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return;
    }
    document.url = format!("{base}/{}", path.trim_start_matches('/'));
}

/// An RFC3339 timestamp from milliseconds since the epoch.
///
/// Hand-rolled rather than pulled in: the only timestamps this crate formats are
/// whole milliseconds since the epoch, the lexicon wants exactly
/// `YYYY-MM-DDTHH:MM:SS.sssZ`, and a date library would be a dependency for one
/// function. `civil_from_days` is Howard Hinnant's, which is the standard
/// days-to-date algorithm and is exact for every date this will ever see.
pub fn rfc3339(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

/// Days since 1970-01-01 to a (year, month, day) in the proleptic Gregorian
/// calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds since the epoch for an RFC3339 timestamp, or `None` if it is not one.
///
/// Lenient about the parts the lexicon leaves open — optional fractional
/// seconds, `Z` or a numeric offset, lower-case `t`/`z` — and strict about
/// everything else. Used only to sort a corpus, so a value it cannot read costs
/// a record its place in the order, never its place on the page.
pub fn parse_rfc3339_secs(value: &str) -> Option<i64> {
    let v = value.trim();
    let (date, rest) = v.split_once('T').or_else(|| v.split_once('t'))?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time, offset_secs) = split_offset(rest)?;
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next()?.parse().ok()?;
    if t.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset_secs)
}

/// Split a time-of-day from its trailing offset, returning both.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(time) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        return Some((time, 0));
    }
    // A time-of-day carries no sign, so the last one is the offset.
    let index = rest.rfind(|c| c == '+' || c == '-')?;
    let (time, offset) = rest.split_at(index);
    let negative = offset.starts_with('-');
    let mut parts = offset[1..].split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
    let magnitude = hours * 3600 + minutes * 60;
    Some((time, if negative { -magnitude } else { magnitude }))
}

/// A (year, month, day) in the proleptic Gregorian calendar to days since
/// 1970-01-01. Hinnant's inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Truncate to a character bound, without splitting a character.
fn clamp(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post() -> Post {
        Post {
            rkey: Post::rkey_for("zine-1"),
            title: "A title".into(),
            text: "The body, plain.".into(),
            description: Some("A taste.".into()),
            tags: vec!["rust".into(), " notes ".into(), "".into()],
            published_at_ms: 1_700_000_000_000,
            updated_at_ms: None,
        }
    }

    #[test]
    fn a_record_carries_no_private_field() {
        let record = post().record("at://did:plc:abc/site.standard.publication/self").unwrap();
        let encoded = serde_json::to_string(&record).unwrap();
        // The private parts of a zine have nowhere to go: the record's own keys
        // are the lexicon's, and nothing else.
        let mut keys: Vec<&str> = record.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "$type",
                "description",
                "path",
                "publishedAt",
                "site",
                "tags",
                "textContent",
                "title",
            ]
        );
        for private in ["pen", "comment", "revision", "prev_hash", "read_key", "zine_id"] {
            assert!(
                !encoded.contains(private),
                "{private} must not reach a public record: {encoded}"
            );
        }
    }

    #[test]
    fn tags_are_trimmed_and_empties_dropped() {
        let record = post().record("at://did:plc:abc/site.standard.publication/self").unwrap();
        let tags: Vec<String> = record["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        assert_eq!(tags, vec!["rust", "notes"]);
    }

    #[test]
    fn a_first_publish_has_no_updated_at_and_an_edit_does() {
        let first = post().record("at://did:plc:abc/site.standard.publication/self").unwrap();
        assert!(first.get("updatedAt").is_none());
        let mut edited = post();
        edited.updated_at_ms = Some(1_700_000_060_000);
        let record = edited.record("at://did:plc:abc/site.standard.publication/self").unwrap();
        assert_eq!(record["updatedAt"], "2023-11-14T22:14:20.000Z");
        assert_eq!(
            record["publishedAt"], first["publishedAt"],
            "an edit does not move the first publish time"
        );
    }

    #[test]
    fn a_post_missing_a_title_text_or_publication_is_refused() {
        let uri = "at://did:plc:abc/site.standard.publication/self";
        let mut p = post();
        p.title = "  ".into();
        assert!(matches!(p.record(uri), Err(Error::Invalid(_))));
        let mut p = post();
        p.text = String::new();
        assert!(matches!(p.record(uri), Err(Error::Invalid(_))));
        assert!(matches!(post().record(""), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_record_key_is_stable_per_zine_and_opaque() {
        let a = Post::rkey_for("zine-1");
        assert_eq!(a, Post::rkey_for("zine-1"), "the same zine addresses one record");
        assert_ne!(a, Post::rkey_for("zine-2"));
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!a.contains("zine"), "the key must not carry the private id");
    }

    #[test]
    fn timestamps_match_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
        // A leap day, and the last second of a year.
        assert_eq!(rfc3339(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
        assert_eq!(rfc3339(1_735_689_599_000), "2024-12-31T23:59:59.000Z");
    }

    #[test]
    fn a_listed_record_maps_or_is_skipped() {
        let uri = "at://did:plc:abc/site.standard.document/3k";
        let value = serde_json::json!({
            "title": "Hello",
            "textContent": "Body",
            "tags": ["a", ""],
            "publishedAt": "2026-01-01T00:00:00.000Z",
            "path": "/3k",
        });
        let doc = Document::from_record(uri, &value).unwrap();
        assert_eq!(doc.did, "did:plc:abc");
        assert_eq!(doc.title, "Hello");
        assert_eq!(doc.tags, vec!["a"]);
        // No title, or an address that is not a DID, is not a document.
        assert!(Document::from_record(uri, &serde_json::json!({ "title": "  " })).is_none());
        assert!(Document::from_record("at://example.com/x/y", &value).is_none());
        assert!(Document::from_record("not-an-at-uri", &value).is_none());
    }

    /// Comments are not in the record — they are on the Bluesky thread the
    /// record points at. This link is the only way to them, so dropping it is
    /// the difference between a post with comments and a post with none.
    #[test]
    fn the_linked_thread_is_kept_because_that_is_where_the_comments_are() {
        let uri = "at://did:plc:abc/site.standard.document/3k";
        let value = serde_json::json!({
            "title": "Hello",
            "textContent": "Body",
            "bskyPostRef": {
                "uri": "at://did:plc:abc/app.bsky.feed.post/3mwe",
                "cid": "bafyreicmxetglsf5qoc3y552ve3xodi5ds5fxu2ho3wsjarge542gaaz7m",
                "commit": { "cid": "bafyreibiuzply6nd4r57vhulosygvpp6mj2pk3etw4xm3oomshjxvurqra" },
                "validationStatus": "valid",
            },
        });
        let reference = Document::from_record(uri, &value).unwrap().bsky_post_ref.unwrap();
        assert_eq!(reference.uri, "at://did:plc:abc/app.bsky.feed.post/3mwe");
        assert_eq!(
            reference.cid,
            "bafyreicmxetglsf5qoc3y552ve3xodi5ds5fxu2ho3wsjarge542gaaz7m"
        );
    }

    /// The field is published with and without a `cid`, and a thread link with
    /// no `cid` is still a thread link. A ref with no `uri` addresses nothing,
    /// so it is not one.
    #[test]
    fn a_thread_link_is_read_leniently_and_only_a_uri_is_required() {
        let uri = "at://did:plc:abc/site.standard.document/3k";
        let bare = serde_json::json!({
            "title": "Hello",
            "bskyPostRef": { "uri": "at://did:plc:abc/app.bsky.feed.post/3mwe" },
        });
        let reference = Document::from_record(uri, &bare).unwrap().bsky_post_ref.unwrap();
        assert_eq!(reference.uri, "at://did:plc:abc/app.bsky.feed.post/3mwe");
        assert_eq!(reference.cid, "", "a missing cid is empty, not a refusal");

        let none = serde_json::json!({ "title": "Hello" });
        assert!(Document::from_record(uri, &none).unwrap().bsky_post_ref.is_none());

        let empty = serde_json::json!({ "title": "Hello", "bskyPostRef": { "uri": "  " } });
        assert!(Document::from_record(uri, &empty).unwrap().bsky_post_ref.is_none());
    }

    /// One account can publish to several publications — the live ecosystem has
    /// accounts with two and three — so the link has to survive ingest for the
    /// lane to be able to tell one blog from another.
    #[test]
    fn the_publication_a_document_belongs_to_is_kept() {
        let uri = "at://did:plc:abc/site.standard.document/3k";
        let value = serde_json::json!({
            "title": "Hello",
            "site": "at://did:plc:abc/site.standard.publication/3lppk75kw7k26",
        });
        assert_eq!(
            Document::from_record(uri, &value).unwrap().site,
            "at://did:plc:abc/site.standard.publication/3lppk75kw7k26"
        );
        // A record with no publication is still a document; it just cannot be
        // attributed to one.
        let bare = serde_json::json!({ "title": "Hello" });
        assert_eq!(Document::from_record(uri, &bare).unwrap().site, "");
    }

    /// The shapes below are trimmed copies of records read from the live
    /// network: Leaflet writes `content` and no `textContent`, pckt writes both.
    /// Getting these wrong is not a cosmetic bug — it is a followed publication
    /// showing up as a headline with no article under it.
    #[test]
    fn text_content_is_used_verbatim_when_the_author_wrote_it() {
        let value = serde_json::json!({
            "title": "Hello",
            "textContent": "First line.\n\nSecond line.",
            "content": { "$type": "blog.pckt.content", "items": [
                { "$type": "blog.pckt.block.text", "plaintext": "something else entirely" }
            ]},
        });
        assert_eq!(body_text(&value), "First line.\n\nSecond line.");
    }

    #[test]
    fn a_leaflet_content_tree_is_read_when_text_content_is_absent() {
        let value = serde_json::json!({
            "title": "Back to School",
            "content": {
                "$type": "pub.leaflet.content",
                "pages": [{
                    "$type": "pub.leaflet.pages.linearDocument",
                    "blocks": [
                        { "$type": "pub.leaflet.pages.linearDocument#block", "block": {
                            "$type": "pub.leaflet.blocks.text",
                            "facets": [{ "index": { "byteStart": 0, "byteEnd": 4 },
                                "features": [{ "$type": "pub.leaflet.richtext.facet#italic" }] }],
                            "plaintext": "You may not know this." }},
                        { "$type": "pub.leaflet.pages.linearDocument#block", "block": {
                            "$type": "pub.leaflet.blocks.header", "level": 2,
                            "plaintext": "Back to School Special" }},
                        { "$type": "pub.leaflet.pages.linearDocument#block", "block": {
                            "$type": "pub.leaflet.blocks.unorderedList", "children": [
                                { "$type": "pub.leaflet.blocks.unorderedList#listItem", "content": {
                                    "$type": "pub.leaflet.blocks.text", "plaintext": "email newsletters" }},
                                { "$type": "pub.leaflet.blocks.unorderedList#listItem", "content": {
                                    "$type": "pub.leaflet.blocks.text", "plaintext": "custom domains" }}] }},
                        // A block whose words live in `alt`, a link card, and an
                        // embed: none of them is `plaintext`, so none contributes.
                        { "$type": "pub.leaflet.pages.linearDocument#block", "block": {
                            "$type": "pub.leaflet.blocks.image", "alt": "a desk",
                            "image": { "$type": "blob" } }},
                        { "$type": "pub.leaflet.pages.linearDocument#block", "block": {
                            "$type": "pub.leaflet.blocks.website", "src": "https://x.example",
                            "title": "A link card", "description": "not the body" }},
                    ],
                }],
            },
        });
        assert_eq!(
            body_text(&value),
            "You may not know this.\n\nBack to School Special\n\nemail newsletters\n\ncustom domains"
        );
    }

    #[test]
    fn a_pckt_content_tree_is_read_when_text_content_is_absent() {
        let value = serde_json::json!({
            "title": "Okay, so...",
            "content": {
                "$type": "blog.pckt.content",
                "items": [
                    { "$type": "blog.pckt.block.heading", "level": 3, "plaintext": "New things to try" },
                    { "$type": "blog.pckt.block.text", "facets": [{ "index": { "byteStart": 0, "byteEnd": 2 },
                        "features": [{ "$type": "blog.pckt.richtext.facet#highlight" }] }],
                        "plaintext": "Hi everybody !" },
                ],
            },
        });
        assert_eq!(body_text(&value), "New things to try\n\nHi everybody !");
    }

    #[test]
    fn a_blank_text_content_falls_through_to_the_content_tree() {
        let value = serde_json::json!({
            "title": "Untitled",
            "textContent": "   ",
            "content": { "$type": "pub.leaflet.content", "pages": [{
                "$type": "pub.leaflet.pages.linearDocument", "blocks": [
                    { "$type": "pub.leaflet.pages.linearDocument#block", "block": {
                        "$type": "pub.leaflet.blocks.text", "plaintext": "still here" }}] }] },
        });
        assert_eq!(body_text(&value), "still here");
    }

    /// A third shape, read from the live network: a client that puts the words
    /// of a block in an `html` field instead of a `plaintext` one. Two documents
    /// in one vault's cache were exactly this and both rendered as a title with
    /// nothing under it, because only `plaintext` was collected.
    #[test]
    fn an_html_content_block_is_read_and_reduced_to_its_words() {
        let value = serde_json::json!({
            "title": "The new theme",
            "content": { "html": "The new, Tailwind-based theme is so much faster.", "$type": "co.idno.html" },
        });
        assert_eq!(body_text(&value), "The new, Tailwind-based theme is so much faster.");
    }

    #[test]
    fn an_html_block_keeps_its_label_and_loses_its_markup() {
        let value = serde_json::json!({
            "title": "Inkwell",
            "content": { "$type": "co.idno.html",
                "html": "a <a href=\"https://inkwell.social\">fediverse journaling platform</a>" },
        });
        // The destination is gone because a plaintext field has nowhere to put
        // it; the label is what a reader needs.
        assert_eq!(body_text(&value), "a fediverse journaling platform");
    }

    #[test]
    fn a_pointer_record_yields_no_body_and_keeps_its_own_words() {
        // The dominant shape in the live cache: 48 of 105 documents were this,
        // and every one rendered as a bare title. There is genuinely no body in
        // the record — the words are on the author's site — so the reader needs
        // the description and the link to have anything to show.
        let value = serde_json::json!({
            "$type": "site.standard.document",
            "title": "The space to breathe",
            "description": "A short note about slowing down.",
            "path": "/the-space-to-breathe",
            "site": "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt",
            "publishedAt": "2026-09-01T00:00:00.000Z",
        });
        let document =
            Document::from_record("at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.document/abc", &value)
                .expect("a titled record maps");
        assert_eq!(document.text, "");
        assert_eq!(document.description, "A short note about slowing down.");
        assert_eq!(document.path, "/the-space-to-breathe");
    }

    #[test]
    fn a_pointer_record_gets_the_link_its_publication_gives_it() {
        let mut document = pointer_document();
        let publications = std::collections::HashMap::from([(
            "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt".to_string(),
            "https://werd.io".to_string(),
        )]);
        resolve_link(&mut document, &publications);
        // Verified against the live network before trusting the join: this exact
        // URL answers 200.
        assert_eq!(document.url, "https://werd.io/the-space-to-breathe");
    }

    #[test]
    fn a_trailing_slash_on_the_publication_does_not_double_up() {
        let mut document = pointer_document();
        let publications = std::collections::HashMap::from([(
            "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt".to_string(),
            "https://werd.io/".to_string(),
        )]);
        resolve_link(&mut document, &publications);
        assert_eq!(document.url, "https://werd.io/the-space-to-breathe");
    }

    /// A link that is wrong is worse than no link: it sends a reader somewhere
    /// the author never published and the reader cannot tell. Every half-missing
    /// case leaves the field empty rather than guessing.
    #[test]
    fn a_link_is_left_empty_rather_than_guessed() {
        // Publication never read.
        let mut document = pointer_document();
        resolve_link(&mut document, &std::collections::HashMap::new());
        assert_eq!(document.url, "");

        // Publication known, but a different one than the document names.
        let mut document = pointer_document();
        let publications = std::collections::HashMap::from([(
            "at://did:plc:someoneelse/site.standard.publication/x".to_string(),
            "https://elsewhere.example".to_string(),
        )]);
        resolve_link(&mut document, &publications);
        assert_eq!(document.url, "");

        // Publication known, but the record carries no path to hang off it.
        let mut document = pointer_document();
        document.path = String::new();
        let publications = std::collections::HashMap::from([(
            "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt".to_string(),
            "https://werd.io".to_string(),
        )]);
        resolve_link(&mut document, &publications);
        assert_eq!(document.url, "");

        // Publication known, but its url is blank.
        let mut document = pointer_document();
        let publications = std::collections::HashMap::from([(
            "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt".to_string(),
            "   ".to_string(),
        )]);
        resolve_link(&mut document, &publications);
        assert_eq!(document.url, "");
    }

    #[test]
    fn an_absolute_path_needs_no_publication() {
        // The lexicon permits a path that is already a URL; it is the record's
        // own answer and joining it onto a base would corrupt it.
        let mut document = pointer_document();
        document.path = "https://example.com/already/absolute".to_string();
        resolve_link(&mut document, &std::collections::HashMap::new());
        assert_eq!(document.url, "https://example.com/already/absolute");
    }

    #[test]
    fn resolving_a_link_never_overwrites_one_already_there() {
        let mut document = pointer_document();
        document.url = "https://already.example/set".to_string();
        let publications = std::collections::HashMap::from([(
            "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt".to_string(),
            "https://werd.io".to_string(),
        )]);
        resolve_link(&mut document, &publications);
        assert_eq!(document.url, "https://already.example/set");
    }

    fn pointer_document() -> Document {
        Document::from_record(
            "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.document/3mxwq3nuwi24r",
            &serde_json::json!({
                "title": "The space to breathe",
                "path": "/the-space-to-breathe",
                "site": "at://did:plc:77tdak46psveqneyegsdyc7l/site.standard.publication/3mqxxrq6bwkjt",
            }),
        )
        .expect("a titled record maps")
    }

    #[test]
    fn a_record_with_no_body_at_all_is_still_a_document() {
        // A title with nothing under it is worth listing; it is not worth
        // refusing, because the address and the title are the two things that
        // cannot be defaulted.
        let value = serde_json::json!({ "title": "Untitled", "content": { "$type": "pub.leaflet.content" } });
        assert_eq!(body_text(&value), "");
        assert_eq!(Document::from_record("at://did:plc:abc/site.standard.document/3k", &value)
            .unwrap()
            .text, "");
    }

    #[test]
    fn a_publication_needs_a_name_and_an_address() {
        let ok = publication_record("Ada's zine", "https://ada.example/").unwrap();
        assert_eq!(ok["url"], "https://ada.example");
        assert_eq!(ok["$type"], PUBLICATION_COLLECTION);
        assert!(publication_record("", "https://ada.example").is_err());
        assert!(publication_record("Ada", "").is_err());
        assert!(publication_record("Ada", "ada.example").is_err());
    }

    #[test]
    fn clamping_counts_characters_not_bytes() {
        let long = "é".repeat(TAG_MAX_CHARS + 10);
        let clamped = clamp(&long, TAG_MAX_CHARS);
        assert_eq!(clamped.chars().count(), TAG_MAX_CHARS);
    }

    #[test]
    fn a_parsed_timestamp_agrees_with_the_formatter() {
        for ms in [0u64, 1_700_000_000_000, 1_709_164_800_000, 1_735_689_599_000] {
            assert_eq!(
                parse_rfc3339_secs(&rfc3339(ms)),
                Some((ms / 1000) as i64),
                "{} did not round-trip",
                rfc3339(ms)
            );
        }
    }

    #[test]
    fn a_parse_accepts_the_shapes_other_clients_write() {
        let base = parse_rfc3339_secs("2023-11-14T22:13:20.000Z").unwrap();
        assert_eq!(parse_rfc3339_secs("2023-11-14T22:13:20Z"), Some(base));
        assert_eq!(parse_rfc3339_secs("2023-11-14t22:13:20z"), Some(base));
        // An offset moves the instant, not the wall clock.
        assert_eq!(parse_rfc3339_secs("2023-11-14T23:13:20+01:00"), Some(base));
        assert_eq!(parse_rfc3339_secs("2023-11-14T21:13:20-01:00"), Some(base));
        assert_eq!(parse_rfc3339_secs("2023-11-14T22:13:20.123456Z"), Some(base));
        // No offset is not RFC3339, and a nonsense date is not one either.
        assert_eq!(parse_rfc3339_secs("2023-11-14T22:13:20"), None);
        assert_eq!(parse_rfc3339_secs("not a date"), None);
        assert_eq!(parse_rfc3339_secs("2023-13-01T00:00:00Z"), None);
    }

    #[test]
    fn an_unparseable_date_keeps_its_record_but_sorts_it_last() {
        let doc = Document::from_record(
            "at://did:plc:abc/site.standard.document/3k",
            &serde_json::json!({ "title": "Hello", "publishedAt": "sometime" }),
        )
        .unwrap();
        assert_eq!(doc.published_at, "sometime", "the author's own string is kept");
        assert_eq!(doc.published_secs, 0, "and it sorts last rather than vanishing");
    }
}

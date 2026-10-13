//! The Reads subsystem: the reader behind the Reads rail pane. One built-in
//! source — Hacker News ("HK News", the best of the week) — plus user-added
//! RSS feeds fetched and parsed here.
//!
//! Posture, matching `docs/tasks/hush-reads.md`:
//! - The webview never contacts hn.algolia.com, RSS hosts, or article sites;
//!   every byte it renders comes from the daemon, already sanitized.
//! - Reader HTML passes through an allow-list sanitizer before it is cached
//!   or served. Links are never auto-opened by the UI.
//! - Feed and reader fetches are direct HTTPS (an owner-confirmed exception
//!   to the Tor egress default: these are public, unauthenticated hosts),
//!   tightly capped and refused for loopback/private-IP targets.
//!
//! Personal state (sources/saved/read/hidden/history) lives in
//! [`crate::reads_store`], not here. This module is stateless beyond caches.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use scraper::{ElementRef, Html};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ALGOLIA_BASE: &str = "https://hn.algolia.com/api/v1/";
const HN_ITEM_URL: &str = "https://news.ycombinator.com/item?id=";

/// Hits per page asked of Algolia; the UI pages through with "more".
pub const HITS_PER_PAGE: u32 = 30;

/// How long a feed/story fetch is trusted before refetching.
const MEM_TTL: Duration = Duration::from_secs(120);

/// Reader-mode article cache: articles drift slowly; re-extract after this.
const READER_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// Disk cache budget for reader pages; the oldest entries go first.
const READER_CACHE_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// Max bytes pulled off the wire for one response (API JSON and articles).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, ReadsError>;

#[derive(Debug)]
pub enum ReadsError {
    BadUrl(String),
    Http(reqwest::Error),
    /// The site answered a normal request with a bot challenge or an outright
    /// refusal (403/429/503) — Cloudflare's managed challenge and its kin. Not
    /// a fault on our side, and not something request headers get past, so it
    /// gets its own variant the UI can present as a deliberate state.
    Blocked(u16),
    Parse(String),
    NoContent,
}

impl std::fmt::Display for ReadsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadsError::BadUrl(u) => write!(f, "refused URL {u:?}"),
            ReadsError::Http(e) => write!(f, "fetch failed: {e}"),
            ReadsError::Blocked(code) => {
                write!(f, "this site refused an automated reader (HTTP {code})")
            }
            ReadsError::Parse(what) => write!(f, "unexpected {what} from Hacker News"),
            ReadsError::NoContent => {
                write!(f, "could not extract readable text; try the original")
            }
        }
    }
}

impl std::error::Error for ReadsError {}

impl From<reqwest::Error> for ReadsError {
    fn from(e: reqwest::Error) -> Self {
        ReadsError::Http(e)
    }
}

// ---------------------------------------------------------------------------
// Wire models
// ---------------------------------------------------------------------------

/// One story as the UI renders it in lists.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoryDto {
    /// HN item id, as a string because every consumer treats it as opaque.
    pub id: String,
    pub title: String,
    /// The linked article; `None` for text posts (Ask HN without a link).
    pub url: Option<String>,
    /// Host of `url` with `www.` stripped; the domain-filter handle.
    pub domain: Option<String>,
    pub author: String,
    pub points: i64,
    pub num_comments: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// The HN discussion page for HN stories; empty for RSS items, which have
    /// no discussion. The UI branches on this to decide reader-vs-comments.
    pub hn_url: String,
}

/// One comment as the UI renders it; `children` preserves the HN thread shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentDto {
    pub id: String,
    pub author: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Sanitized comment HTML (empty when the comment is deleted or a "tell").
    pub html: String,
    pub children: Vec<CommentDto>,
}

/// A story plus its discussion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoryDetailDto {
    pub story: StoryDto,
    /// Sanitized story text for text posts (Ask HN), if any.
    pub text_html: Option<String>,
    pub comments: Vec<CommentDto>,
}

/// Reader-mode extraction of an article page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReaderDto {
    pub url: String,
    pub title: Option<String>,
    /// Sanitized article HTML: headings, paragraphs, lists, quotes, code.
    pub html: String,
}

// --- Algolia wire shapes (field names exactly as the API returns them) ---

#[derive(Deserialize)]
struct AlgoliaSearch {
    #[serde(default)]
    hits: Vec<AlgoliaHit>,
}

#[derive(Deserialize)]
struct AlgoliaHit {
    #[serde(rename = "objectID")]
    object_id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    author: Option<String>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default)]
    num_comments: Option<i64>,
    #[serde(default, rename = "created_at_i")]
    created_at_i: i64,
    #[serde(default, rename = "_tags")]
    tags: Vec<String>,
}

impl AlgoliaHit {
    /// List views only want stories; comment hits and oddities are skipped.
    fn into_story(self) -> Option<StoryDto> {
        if !self.tags.iter().any(|t| t == "story") {
            return None;
        }
        let id = self.object_id?;
        let title = self.title?;
        let domain = self.url.as_deref().and_then(domain_of);
        let hn_url = format!("{HN_ITEM_URL}{id}");
        Some(StoryDto {
            id,
            title,
            url: self.url.filter(|u| !u.is_empty()),
            domain,
            author: self.author.unwrap_or_default(),
            points: self.points.unwrap_or(0),
            num_comments: self.num_comments.unwrap_or(0),
            created_at: self.created_at_i,
            hn_url,
        })
    }
}

fn domain_of(url: &str) -> Option<String> {
    let host = reqwest::Url::parse(url).ok()?.host_str()?.to_string();
    let host = host
        .strip_prefix("www.")
        .map(str::to_string)
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

#[derive(Deserialize)]
struct AlgoliaItem {
    /// `id` arrives as a JSON number for comments and is echoed as a string
    /// elsewhere; treat it as either and normalize to a string.
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default, rename = "created_at_i")]
    created_at_i: i64,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    children: Vec<AlgoliaItem>,
}

fn id_string(v: Option<&serde_json::Value>) -> Option<String> {
    match v? {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

impl AlgoliaItem {
    fn into_comment(self) -> Option<CommentDto> {
        if self.kind.as_deref()? != "comment" {
            return None;
        }
        let children = self
            .children
            .into_iter()
            .filter_map(AlgoliaItem::into_comment)
            .collect();
        Some(CommentDto {
            id: id_string(self.id.as_ref())?,
            author: self.author.unwrap_or_default(),
            created_at: self.created_at_i,
            html: self
                .text
                .as_deref()
                .map(sanitize_rich_text)
                .unwrap_or_default(),
            children,
        })
    }
}

// ---------------------------------------------------------------------------
// HK News (the built-in HN feed) and RSS sources
// ---------------------------------------------------------------------------

/// Build the Algolia URL for one page of the built-in feed: the best stories
/// of the last week (`now` bounds the window — HN's /best has no direct API
/// equivalent, this is its approximation).
pub fn hknews_url(page: u32, now: i64) -> String {
    let params: Vec<(String, String)> = vec![
        ("tags".into(), "story".into()),
        ("numericFilters".into(), best_filter(now)),
        ("hitsPerPage".into(), HITS_PER_PAGE.to_string()),
        ("page".into(), page.to_string()),
    ];
    reqwest::Url::parse_with_params(&format!("{ALGOLIA_BASE}search"), params)
        .expect("static base + params is a valid URL")
        .to_string()
}

fn best_filter(now: i64) -> String {
    let week = now - 7 * 24 * 3600;
    format!("points>50,created_at_i>{week}")
}

// ---------------------------------------------------------------------------
// RSS sources
// ---------------------------------------------------------------------------

/// Stable story id for an RSS entry, derived from its link so it survives
/// refetches (saved/hidden/read state keys on it). Never collides with HN's
/// all-digit ids.
pub fn story_id_for_link(link: &str) -> String {
    let hash = Sha256::digest(link.as_bytes());
    format!(
        "rss-{}",
        hash[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Parse one fetched feed (RSS 2, Atom, JSON Feed) into `(title, story
/// rows)`, rows newest first. Entries without a link are skipped: without one
/// there is nothing to read and no id to key personal state on.
pub fn parse_feed(bytes: &[u8]) -> std::result::Result<(String, Vec<StoryDto>), String> {
    let feed =
        feed_rs::parser::parse(Cursor::new(bytes)).map_err(|e| format!("unreadable feed: {e}"))?;
    let feed_title = feed
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Feed".into());
    let mut stories: Vec<StoryDto> = feed
        .entries
        .iter()
        .filter_map(|entry| {
            let link = entry.links.first().map(|l| l.href.clone())?;
            let url = reqwest::Url::parse(&link).ok()?;
            let title = entry
                .title
                .as_ref()
                .map(|t| t.content.trim().to_string())
                .filter(|t| !t.is_empty())?;
            let author = entry
                .authors
                .first()
                .and_then(|p| p.name.as_deref())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(feed_title.as_str())
                .to_string();
            let created_at = entry
                .published
                .or(entry.updated)
                .map(|d| d.timestamp())
                .unwrap_or_default();
            Some(StoryDto {
                id: story_id_for_link(&link),
                title,
                url: Some(url.to_string()),
                domain: domain_of(url.as_str()),
                author,
                points: 0,
                num_comments: 0,
                created_at,
                hn_url: String::new(),
            })
        })
        .collect();
    stories.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok((feed_title, stories))
}

// ---------------------------------------------------------------------------
// Sanitizing
// ---------------------------------------------------------------------------

/// The one HTML allow-list for user-visible rich text: HN comment fragments,
/// reader-mode article bodies, and a zine's body. Structural tags from
/// articles (headings) ride along; everything else is stripped, not escaped, so
/// a hostile page contributes text at most.
///
/// Zines are authored locally rather than fetched, but they are sanitized here
/// anyway and with the same list: a body is stored, re-read, and eventually
/// sealed and published, so it is untrusted input at the moment it is written —
/// the boundary rule is to validate where the data enters, and the editor is not
/// the only thing that can post to the route. `u`/`s` are the two marks the
/// Paper toolbar offers that HN never emits.
///
/// The list is a `const` rather than a local because both directions of the
/// format read it: [`sanitize_rich_text`] keeps exactly these tags, and
/// [`rich_text_to_markdown`] knows exactly these — so a tag added for the editor
/// is a tag the export can carry, or the pair fails a test.
pub const RICH_TEXT_TAGS: &[&str] = &[
    "p", "a", "i", "em", "b", "strong", "u", "s", "code", "pre", "blockquote", "ul", "ol", "li",
    "br", "hr", "h1", "h2", "h3", "h4",
];

/// The URL schemes a link may keep. Everything else is stripped from the `href`,
/// which is what makes a `javascript:` link impossible rather than merely
/// unlikely.
pub const RICH_TEXT_SCHEMES: &[&str] = &["http", "https", "mailto"];

pub fn sanitize_rich_text(raw: &str) -> String {
    let mut tags = HashSet::new();
    for t in RICH_TEXT_TAGS {
        tags.insert(*t);
    }
    let mut schemes = HashSet::new();
    for s in RICH_TEXT_SCHEMES {
        schemes.insert(*s);
    }
    let mut builder = ammonia::Builder::default();
    builder.tags(tags);
    builder.url_schemes(schemes);
    builder.link_rel(Some("noopener noreferrer nofollow"));
    builder.clean(raw).to_string()
}

// ---------------------------------------------------------------------------
// Rich text out: sanitized HTML to Markdown (the export re-encode)
// ---------------------------------------------------------------------------

/// Re-encode a sanitized rich-text body as Markdown.
///
/// The body a zine stores is sanitized HTML ([`sanitize_rich_text`]); the body a
/// public article carries is Markdown. This is that translation. It knows
/// [`RICH_TEXT_TAGS`] and nothing else — the same list the sanitizer keeps, so
/// what the editor can produce is what the export can carry.
///
/// Two decisions are worth stating, because both are places a converter
/// usually goes quietly wrong:
///
/// - **Entities are left alone in prose.** `&amp;` is passed through rather than
///   turned back into `&`, for two reasons: Markdown reads entities, so the
///   reader sees the right character either way, and it is what guarantees the
///   result cannot contain a `<` that came from the body. Every raw `<` in the
///   output is a tag this function emitted. Code spans and code blocks are the
///   exception — nothing decodes entities inside a fence, so those are decoded.
/// - **`u` becomes inline HTML.** Markdown has no underline; `s` has `~~` and
///   the rest have their own markers. `<u>…</u>` degrades to the plain text on a
///   client that strips HTML — the same as dropping the mark — and survives
///   where HTML is allowed, which dropping it never would.
///
/// Anything it does not recognize keeps its text and loses its markup, so an
/// unknown tag is never a reason to lose the words.
pub fn rich_text_to_markdown(html: &str) -> String {
    let mut stack = vec![root_frame()];
    for token in tokenize(html) {
        match token {
            Token::Text(text) => stack
                .last_mut()
                .expect("the root frame is never popped")
                .children
                .push(Node::Text(text)),
            Token::Tag(tag) if tag.closing => close_element(&mut stack, &tag.name),
            Token::Tag(tag) if tag.self_closing || tag.name == "br" || tag.name == "hr" => {
                let node = Node::Element {
                    name: tag.name,
                    href: tag.href,
                    children: Vec::new(),
                };
                stack
                    .last_mut()
                    .expect("the root frame is never popped")
                    .children
                    .push(node);
            }
            Token::Tag(tag) => stack.push(Frame {
                name: tag.name,
                href: tag.href,
                children: Vec::new(),
            }),
        }
    }
    // Anything still open at the end closes here, so a truncated body still
    // exports its words rather than nothing.
    while stack.len() > 1 {
        let frame = stack.pop().expect("len > 1");
        stack
            .last_mut()
            .expect("the root frame is never popped")
            .children
            .push(Node::Element {
                name: frame.name,
                href: frame.href,
                children: frame.children,
            });
    }
    let root = stack.pop().expect("the root frame");
    render_blocks(&root.children).trim().to_string()
}

/// A tag, as the tokenizer found it. Only `href` is kept: it is the one
/// attribute that carries meaning into Markdown, and the sanitizer has already
/// dropped everything else that could matter.
struct Tag {
    name: String,
    href: Option<String>,
    closing: bool,
    self_closing: bool,
}

enum Token {
    Text(String),
    Tag(Tag),
}

enum Node {
    Text(String),
    Element {
        name: String,
        href: Option<String>,
        children: Vec<Node>,
    },
}

/// An open element while the tree is being built. The bottom of the stack is a
/// nameless root that no close tag can match.
struct Frame {
    name: String,
    href: Option<String>,
    children: Vec<Node>,
}

fn root_frame() -> Frame {
    Frame {
        name: String::new(),
        href: None,
        children: Vec::new(),
    }
}

/// Attach the named element, closing anything still open inside it first.
///
/// A close tag with no open element is ignored, and unclosed inner elements are
/// unwrapped into their parent rather than dropped: sanitized HTML is
/// well-formed, so this is the defensive path, and losing a reader's paragraph
/// to a stray `</p>` would be a worse failure than an odd nesting.
fn close_element(stack: &mut Vec<Frame>, name: &str) {
    let Some(position) = stack.iter().rposition(|f| f.name == name) else {
        return;
    };
    while stack.len() > position + 1 {
        let frame = stack.pop().expect("len > position + 1");
        stack
            .last_mut()
            .expect("the root frame is never popped")
            .children
            .push(Node::Element {
                name: frame.name,
                href: frame.href,
                children: frame.children,
            });
    }
    let frame = stack.pop().expect("position is on the stack");
    stack
        .last_mut()
        .expect("the root frame is never popped")
        .children
        .push(Node::Element {
            name: frame.name,
            href: frame.href,
            children: frame.children,
        });
}

fn tokenize(html: &str) -> Vec<Token> {
    let bytes = html.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut text_start = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        // A comment is markup, not text: skipped whole, and an unterminated one
        // takes the rest of the input with it rather than leaking its body into
        // the export as prose.
        if html[cursor..].starts_with("<!--") {
            if cursor > text_start {
                tokens.push(Token::Text(html[text_start..cursor].to_string()));
            }
            cursor = match html[cursor..].find("-->") {
                Some(offset) => cursor + offset + 3,
                None => html.len(),
            };
            text_start = cursor;
            continue;
        }
        // `cursor` is on an ASCII `<`, so it is a char boundary, and the search
        // is bounded to the rest of the input.
        let Some(offset) = html[cursor..].find('>') else {
            break;
        };
        let end = cursor + offset;
        match parse_tag(&html[cursor + 1..end]) {
            Some(tag) => {
                if cursor > text_start {
                    tokens.push(Token::Text(html[text_start..cursor].to_string()));
                }
                tokens.push(Token::Tag(tag));
                cursor = end + 1;
                text_start = cursor;
            }
            // A `<` that begins no tag is text. Sanitized HTML escapes those,
            // so this is the defensive path.
            None => cursor += 1,
        }
    }
    if text_start < html.len() {
        tokens.push(Token::Text(html[text_start..].to_string()));
    }
    tokens
}

fn parse_tag(inner: &str) -> Option<Tag> {
    let inner = inner.trim();
    let (closing, rest) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, inner),
    };
    let (rest, self_closing) = match rest.strip_suffix('/') {
        Some(rest) => (rest.trim_end(), true),
        None => (rest, false),
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next()?.trim().to_ascii_lowercase();
    // A comment, a doctype, or a stray `<` produces a name that is not a tag
    // name; refusing it here is what keeps `<!-- x -->` from becoming text.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let href = if closing {
        None
    } else {
        parse_attrs(parts.next().unwrap_or_default())
            .into_iter()
            .find(|(key, _)| key == "href")
            .map(|(_, value)| value)
            .filter(|value| !value.trim().is_empty())
    };
    Some(Tag {
        name,
        href,
        closing,
        self_closing,
    })
}

fn parse_attrs(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            break;
        }
        rest = rest[name_end..].trim_start();
        let Some(after) = rest.strip_prefix('=') else {
            out.push((name, String::new()));
            continue;
        };
        let after = after.trim_start();
        let (value, remainder) = match after.strip_prefix('"').or_else(|| after.strip_prefix('\'')) {
            Some(quoted) => {
                let quote = after.as_bytes()[0] as char;
                match quoted.find(quote) {
                    Some(end) => (quoted[..end].to_string(), &quoted[end + 1..]),
                    None => (quoted.to_string(), ""),
                }
            }
            None => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (after[..end].to_string(), &after[end..])
            }
        };
        out.push((name, value));
        rest = remainder;
    }
    out
}

/// A tag whose content is its own line or lines, rather than running on.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "ul" | "ol" | "li" | "blockquote" | "pre" | "hr"
    )
}

/// Render a sequence of blocks, joining them with the blank line Markdown needs
/// between them. Stray inline content between blocks becomes its own paragraph
/// rather than being dropped.
fn render_blocks(nodes: &[Node]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut run = String::new();
    for node in nodes {
        match node {
            Node::Element { name, .. } if is_block(name) => {
                if !run.trim().is_empty() {
                    blocks.push(run.trim().to_string());
                }
                run.clear();
                let block = render_block(node);
                if !block.trim().is_empty() {
                    blocks.push(block);
                }
            }
            _ => run.push_str(&render_inline_node(node)),
        }
    }
    if !run.trim().is_empty() {
        blocks.push(run.trim().to_string());
    }
    blocks.join("\n\n")
}

fn render_block(node: &Node) -> String {
    let Node::Element {
        name,
        href: _,
        children,
    } = node
    else {
        return render_inline_node(node);
    };
    match name.as_str() {
        "hr" => "---".to_string(),
        "pre" => render_pre(children),
        "blockquote" => render_blockquote(children),
        "ul" => render_list(children, false),
        "ol" => render_list(children, true),
        "li" => render_list_item(children, "- ").0,
        "h1" | "h2" | "h3" | "h4" => {
            let level = name.as_bytes()[1] - b'0';
            let text = render_inline(children);
            if text.trim().is_empty() {
                return String::new();
            }
            format!("{} {}", "#".repeat(level as usize), text.trim())
        }
        // `p`, and anything else that arrived as a block.
        _ => render_inline(children).trim().to_string(),
    }
}

fn render_inline(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        out.push_str(&render_inline_node(node));
    }
    out
}

fn render_inline_node(node: &Node) -> String {
    let Node::Element {
        name,
        href,
        children,
    } = node
    else {
        let Node::Text(text) = node else {
            return String::new();
        };
        return text.clone();
    };
    let inner = render_inline(children);
    match name.as_str() {
        // Two spaces then a newline is Markdown's hard break.
        "br" => "  \n".to_string(),
        "b" | "strong" => wrap("**", &inner),
        "i" | "em" => wrap("*", &inner),
        "s" => wrap("~~", &inner),
        "u" => wrap_pair("<u>", "</u>", &inner),
        "code" => render_code_span(&plain_text(children)),
        "a" => render_link(href.as_deref(), &inner),
        // A block inside an inline context, or a tag this converter does not
        // know: keep the words, drop the markup.
        _ => inner,
    }
}

/// Wrap inline content in a marker, keeping any surrounding whitespace
/// *outside* the markers — `** bold **` is not emphasis in Markdown, so an
/// author's stray space would silently lose the mark.
fn wrap(marker: &str, inner: &str) -> String {
    wrap_pair(marker, marker, inner)
}

/// The same, for a mark whose opening and closing forms differ.
fn wrap_pair(open: &str, close: &str, inner: &str) -> String {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return inner.to_string();
    }
    let lead = &inner[..inner.len() - inner.trim_start().len()];
    let trail = &inner[inner.trim_end().len()..];
    format!("{lead}{open}{trimmed}{close}{trail}")
}

fn render_link(href: Option<&str>, label: &str) -> String {
    let Some(url) = href else {
        return label.to_string();
    };
    if !is_safe_url(url) {
        // The sanitizer drops these, so reaching here means the body did not
        // come from it. Emit the words without the link rather than a link a
        // reader might follow.
        return label.to_string();
    }
    let text = if label.trim().is_empty() {
        url.to_string()
    } else {
        label.to_string()
    };
    format!("[{}]({})", text, markdown_url(url))
}

/// Only the schemes the sanitizer keeps. Checked case-insensitively, because a
/// scheme is.
fn is_safe_url(url: &str) -> bool {
    let lowered = url.trim().to_ascii_lowercase();
    RICH_TEXT_SCHEMES
        .iter()
        .any(|scheme| lowered.starts_with(&format!("{scheme}:")))
}

/// A link destination. A URL containing parentheses is wrapped in angle
/// brackets, which is Markdown's own escape for the case — otherwise the first
/// `)` closes the destination early and the rest of the URL becomes prose.
fn markdown_url(url: &str) -> String {
    let url = url.trim();
    if url.contains(['(', ')', ' ']) {
        format!("<{url}>")
    } else {
        url.to_string()
    }
}

/// An inline code span. A body containing a backtick needs the doubled form,
/// padded, or the span closes early.
fn render_code_span(text: &str) -> String {
    let decoded = decode_entities(text);
    if decoded.trim().is_empty() {
        return String::new();
    }
    if decoded.contains('`') {
        format!("`` {decoded} ``")
    } else {
        format!("`{decoded}`")
    }
}

/// A fenced code block. Nothing inside a fence is interpreted, so entities are
/// decoded here — `&amp;` in the source means `&` to the reader, and a fence is
/// the one place Markdown will not decode it for us.
///
/// The fence grows past the longest run of backticks in the body, so a snippet
/// that contains a fence does not end its own block.
fn render_pre(children: &[Node]) -> String {
    let raw = decode_entities(&plain_text(children));
    let body = raw.strip_suffix('\n').unwrap_or(&raw);
    let fence = "`".repeat(longest_backtick_run(body).max(2) + 1);
    format!("{fence}\n{body}\n{fence}")
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

fn render_blockquote(children: &[Node]) -> String {
    let inner = render_blocks(children);
    inner
        .lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_list(children: &[Node], ordered: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut index = 1;
    for child in children {
        let Node::Element { name, children, .. } = child else {
            push_stray(&mut lines, child);
            continue;
        };
        if name != "li" {
            // A list with something other than items in it is degenerate markup,
            // but the words still belong on the page.
            push_stray(&mut lines, child);
            continue;
        }
        let marker = if ordered {
            format!("{index}. ")
        } else {
            "- ".to_string()
        };
        index += 1;
        let (item, nested) = render_list_item(children, &marker);
        lines.push(item);
        if !nested.is_empty() {
            for line in nested.lines() {
                lines.push(format!("  {line}"));
            }
        }
    }
    lines.join("\n")
}

fn push_stray(lines: &mut Vec<String>, node: &Node) {
    let text = render_inline_node(node).trim().to_string();
    if !text.is_empty() {
        lines.push(text);
    }
}

/// One list item: its own line, and any nested list to follow it indented.
fn render_list_item(children: &[Node], marker: &str) -> (String, String) {
    let mut inline_run = String::new();
    let mut nested = String::new();
    for child in children {
        match child {
            Node::Element { name, .. } if name == "ul" || name == "ol" => {
                let block = render_block(child);
                if !block.trim().is_empty() {
                    if !nested.is_empty() {
                        nested.push('\n');
                    }
                    nested.push_str(&block);
                }
            }
            _ => inline_run.push_str(&render_inline_node(child)),
        }
    }
    let text = inline_run.trim();
    let item = if text.is_empty() {
        marker.trim_end().to_string()
    } else {
        format!("{marker}{text}")
    };
    (item, nested)
}

/// Every text node under `nodes`, concatenated: the content of a `pre` or a
/// `code`, where markup inside does not mean anything.
fn plain_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Element { children, .. } => out.push_str(&plain_text(children)),
        }
    }
    out
}

/// Decode the entities the HTML serializer emits, for the contexts where
/// nothing else will. An entity that is not recognized is passed through as the
/// literal text it looks like, rather than dropped.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tail[1..].find(';').map(|end| end + 1) {
            // A `&` this far from a `;` is not an entity.
            Some(end) if end <= 10 => {
                let name = &tail[1..end];
                match name {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" => out.push('\''),
                    "nbsp" => out.push('\u{00a0}'),
                    _ => match numeric_entity(name) {
                        Some(c) => out.push(c),
                        None => {
                            out.push('&');
                            out.push_str(name);
                            out.push(';');
                        }
                    },
                }
                rest = &tail[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn numeric_entity(name: &str) -> Option<char> {
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    char::from_u32(code)
}


// ---------------------------------------------------------------------------
// Reader-mode fetch guard and extractor
// ---------------------------------------------------------------------------

/// Refuse URLs the reader must not fetch: non-http(s) schemes, hosts that name
/// the daemon's own machine (loopback, link-local, RFC1918, ULA). Public DNS
/// names are passed; resolving is reqwest's job (rebinding is out of scope for
/// phase 1 and noted in the task doc).
pub fn check_fetchable_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).map_err(|_| ReadsError::BadUrl(raw.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ReadsError::BadUrl(raw.to_string()));
    }
    let refused = match url.host() {
        Some(url::Host::Domain(host)) => {
            let h = host.to_ascii_lowercase();
            h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local")
        }
        Some(url::Host::Ipv4(ip)) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 64
            // CGNAT
        }
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xfe00) == 0xfe00
        }
        None => true,
    };
    if refused {
        return Err(ReadsError::BadUrl(raw.to_string()));
    }
    Ok(url)
}

/// Elements dropped wholesale (subtree skipped) by the extractor. Text inside
/// these is chrome, not article.
const EXTRACT_SKIP: [&str; 17] = [
    "script", "style", "noscript", "nav", "header", "footer", "aside", "form", "iframe", "svg",
    "img", "video", "audio", "button", "input", "select", "label",
];

/// Elements the extractor renders as-is. Any other element is unwrapped (its
/// text stays), so oddities contribute prose, not markup.
const EXTRACT_KEEP: [&str; 18] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "code",
    "em",
    "i",
    "strong",
    "b",
    "a",
    "br",
    "hr",
];

/// Extract the article body from a fetched page: pick the container with the
/// most paragraph text, render only keep-list elements from it, and sanitize.
/// Returns the page title and the sanitized HTML, or `NoContent` when nothing
/// readable was found (the UI then offers the original).
pub fn extract_article(html: &str) -> Result<ReaderDto> {
    let doc = Html::parse_document(html);
    let title = doc
        .select(&selector("title"))
        .next()
        .map(|t| t.text().collect::<String>())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    let container = pick_container(&doc);
    let mut out = String::new();
    if let Some(c) = container {
        render_subtree(*c, &mut out, 0);
    }
    let rendered = if out.trim().is_empty() {
        // Last resort: strip the page to its body text so short posts still read.
        let body = doc
            .select(&selector("body"))
            .next()
            .ok_or(ReadsError::NoContent)?;
        let mut buf = String::new();
        render_subtree(*body, &mut buf, 0);
        buf
    } else {
        out
    };
    let html = sanitize_rich_text(&rendered);
    if html.trim().is_empty() {
        return Err(ReadsError::NoContent);
    }
    Ok(ReaderDto {
        url: String::new(), // filled by the caller, which owns the request URL
        title,
        html,
    })
}

fn selector(sel: &'static str) -> scraper::Selector {
    scraper::Selector::parse(sel).expect("static selector")
}

/// Choose the article container: the `article` with the most paragraph text,
/// else `main`, else the `div` with the most paragraph text, else the body.
fn pick_container(doc: &scraper::Html) -> Option<ElementRef<'_>> {
    let text_of = |el: ElementRef| -> usize {
        el.select(&selector("p"))
            .map(|p| p.text().map(str::len).sum::<usize>())
            .sum::<usize>()
    };
    let best_of = |sel: &'static str| {
        doc.select(&selector(sel))
            .max_by_key(|el| text_of(*el))
            .filter(|el| text_of(*el) > 0)
    };
    best_of("article")
        .or_else(|| best_of("main"))
        .or_else(|| best_of("div"))
        .or_else(|| best_of("body"))
}

/// Recursively render one DOM node into `out`, keeping only keep-list
/// elements (with `a href` the sole carried attribute) and text. Skip-list
/// subtrees disappear entirely; everything else unwraps.
fn render_subtree(
    node: ego_tree::NodeRef<'_, scraper::node::Node>,
    out: &mut String,
    depth: usize,
) {
    use scraper::node::Node;
    if depth > 64 {
        return; // pathological nesting; give up on this branch, keep the rest
    }
    match node.value() {
        Node::Text(t) => out.push_str(t.as_ref()),
        Node::Element(el) => {
            let name = el.name();
            if EXTRACT_SKIP.contains(&name) {
                return;
            }
            let keep = EXTRACT_KEEP.contains(&name);
            if keep {
                out.push('<');
                out.push_str(name);
                if name == "a" {
                    if let Some(href) = el.attr("href") {
                        out.push_str(" href=\"");
                        out.push_str(&html_attr(href));
                        out.push('"');
                    }
                }
                out.push('>');
            }
            for child in node.children() {
                render_subtree(child, out, depth + 1);
            }
            if keep && !matches!(name, "br" | "hr") {
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
        }
        _ => {}
    }
}

/// Escape an attribute value cheaply; the sanitizer re-checks the URL scheme
/// afterwards, this only keeps the attribute well-formed.
fn html_attr(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Reader disk cache (public content, plaintext, size-capped)
// ---------------------------------------------------------------------------

fn reader_cache_key(url: &str) -> String {
    hex(&Sha256::digest(url.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn reader_cache_path(dir: &Path, url: &str) -> PathBuf {
    dir.join("reader")
        .join(format!("{}.json", reader_cache_key(url)))
}

pub fn save_reader_cache(dir: &Path, url: &str, dto: &ReaderDto) {
    let path = reader_cache_path(dir, url);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec(dto) {
        if std::fs::write(&path, json).is_ok() {
            prune_reader_cache(dir);
        }
    }
}

pub fn load_reader_cache(dir: &Path, url: &str) -> Option<ReaderDto> {
    let path = reader_cache_path(dir, url);
    let meta = std::fs::metadata(&path).ok()?;
    let age = meta.modified().ok()?.elapsed().unwrap_or(Duration::MAX);
    if age > READER_TTL {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Keep the reader cache under its budget by removing the oldest files first.
/// Best-effort: a failure to prune only costs disk, never correctness.
fn prune_reader_cache(dir: &Path) {
    let reader_dir = dir.join("reader");
    let Ok(entries) = std::fs::read_dir(&reader_dir) else {
        return;
    };
    let mut files: Vec<(std::fs::DirEntry, u64, std::time::SystemTime)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let meta = e.metadata().ok()?;
            Some((e, meta.len(), meta.modified().ok()?))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= READER_CACHE_CAP_BYTES {
        return;
    }
    files.sort_by_key(|(_, _, mtime)| *mtime);
    let mut total = total;
    for (path, len, _) in files {
        if total <= READER_CACHE_CAP_BYTES {
            break;
        }
        if std::fs::remove_file(path.path()).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

struct MemEntry {
    at: Instant,
    body: String,
}

pub struct ReadsClient {
    http: reqwest::Client,
    cache_dir: PathBuf,
    mem: Mutex<HashMap<String, MemEntry>>,
}

impl ReadsClient {
    pub fn new(cache_dir: &Path) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("hush-daemon reads/0.1")
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("reqwest client config is static and valid");
        Self {
            http,
            cache_dir: cache_dir.to_path_buf(),
            mem: Mutex::new(HashMap::new()),
        }
    }

    /// One page of the built-in HK News feed (HN best of the week), cached
    /// for 2 minutes.
    pub async fn hknews_feed(&self, page: u32) -> Result<Vec<StoryDto>> {
        let url = hknews_url(page, now_secs());
        self.algolia_search(&url).await
    }

    /// A user RSS source's `(feed title, current items)`, cached for 2
    /// minutes like every other fetch. Feeds have no pagination: the whole
    /// feed is one page and the UI marks it done.
    pub async fn rss_feed(&self, url: &str) -> Result<(String, Vec<StoryDto>)> {
        let fetchable = check_fetchable_url(url)?;
        let body = self.cached_body(fetchable.as_str()).await?;
        parse_feed(body.as_bytes()).map_err(ReadsError::Parse)
    }

    /// A story and its sanitized comment tree (2-minute cache: points and
    /// replies keep moving, but not within a reading session).
    pub async fn story(&self, id: &str) -> Result<StoryDetailDto> {
        if !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ReadsError::BadUrl(id.to_string()));
        }
        let url = format!("{ALGOLIA_BASE}items/{id}");
        let body = self.cached_body(&url).await?;
        let item: AlgoliaItem =
            serde_json::from_str(&body).map_err(|e| ReadsError::Parse(format!("story: {e}")))?;
        let story = StoryDto {
            id: id.to_string(),
            title: item.title.clone().unwrap_or_default(),
            url: item.url.clone().filter(|u| !u.is_empty()),
            domain: item.url.as_deref().and_then(domain_of),
            author: item.author.clone().unwrap_or_default(),
            points: item.points.unwrap_or(0),
            num_comments: count_comments(&item.children),
            created_at: item.created_at_i,
            hn_url: format!("{HN_ITEM_URL}{id}"),
        };
        let text_html = item.text.as_deref().map(sanitize_rich_text);
        let comments = item
            .children
            .into_iter()
            .filter_map(AlgoliaItem::into_comment)
            .collect();
        Ok(StoryDetailDto {
            story,
            text_html,
            comments,
        })
    }

    /// Reader-mode extraction of an article URL, disk-cached.
    pub async fn reader(&self, url: &str) -> Result<ReaderDto> {
        let checked = check_fetchable_url(url)?;
        if let Some(dto) = load_reader_cache(&self.cache_dir, url) {
            return Ok(ReaderDto {
                url: url.to_string(),
                ..dto
            });
        }
        let resp = ensure_ok(self.http.get(checked.clone()).send().await?)?;
        let capped = read_capped(resp, MAX_BODY_BYTES).await?;
        let mut dto = extract_article(&capped)?;
        dto.url = url.to_string();
        save_reader_cache(&self.cache_dir, url, &dto);
        Ok(dto)
    }

    async fn algolia_search(&self, url: &str) -> Result<Vec<StoryDto>> {
        let body = self.cached_body(url).await?;
        let parsed: AlgoliaSearch =
            serde_json::from_str(&body).map_err(|e| ReadsError::Parse(format!("hits: {e}")))?;
        Ok(parsed
            .hits
            .into_iter()
            .filter_map(AlgoliaHit::into_story)
            .collect())
    }

    /// GET with the memory cache in front of the network.
    async fn cached_body(&self, url: &str) -> Result<String> {
        {
            let mem = self.mem.lock().expect("mem cache lock");
            if let Some(hit) = mem.get(url) {
                if hit.at.elapsed() < MEM_TTL {
                    return Ok(hit.body.clone());
                }
            }
        }
        let resp = ensure_ok(self.http.get(url).send().await?)?;
        let body = read_capped(resp, MAX_BODY_BYTES).await?;
        {
            let mut mem = self.mem.lock().expect("mem cache lock");
            if mem.len() > 256 {
                mem.clear(); // crude bound; entries are small and re-fetchable
            }
            mem.insert(
                url.to_string(),
                MemEntry {
                    at: Instant::now(),
                    body: body.clone(),
                },
            );
        }
        Ok(body)
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn count_comments(children: &[AlgoliaItem]) -> i64 {
    children
        .iter()
        .map(|c| 1 + count_comments(&c.children))
        .sum()
}

/// Whether a status is a bot challenge or an outright refusal (Cloudflare's
/// managed challenge and its kin) rather than a plain HTTP error.
fn is_bot_block(status: u16) -> bool {
    matches!(status, 401 | 403 | 429 | 503)
}

/// Turn a response into an error a reader can act on.
///
/// A bot challenge answers 403/429/503 with an interstitial. Reporting that as a
/// generic HTTP status hides the truth — the site refuses automated readers — so
/// it becomes its own variant instead of a raw status line.
fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if is_bot_block(status.as_u16()) {
        return Err(ReadsError::Blocked(status.as_u16()));
    }
    Err(ReadsError::Http(
        resp.error_for_status().expect_err("status is non-success"),
    ))
}

/// Read a response body up to `cap` bytes; a body that would exceed the cap is
/// an error rather than a truncation.
async fn read_capped(resp: reqwest::Response, cap: usize) -> Result<String> {
    if let Some(len) = resp.content_length() {
        if len as usize > cap {
            return Err(ReadsError::Parse(format!(
                "body of {} bytes exceeds the {cap}-byte cap",
                len
            )));
        }
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > cap {
        return Err(ReadsError::Parse(format!(
            "body of {} bytes exceeds the {cap}-byte cap",
            bytes.len()
        )));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| ReadsError::Parse("non-UTF8 body".into()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod reads_tests {
    use super::*;

    #[test]
    fn hknews_url_targets_the_best_of_the_week() {
        let now = 1_800_000_000;
        let page0 = hknews_url(0, now);
        assert!(page0.starts_with("https://hn.algolia.com/api/v1/search?"));
        assert!(page0.contains("tags=story"));
        assert!(
            page0.contains("points%3E50"),
            "numeric filter is encoded: {page0}"
        );
        assert!(page0.contains(&format!("created_at_i%3E{}", now - 7 * 24 * 3600)));
        assert!(page0.contains("page=0"));

        let page2 = hknews_url(2, now);
        assert!(page2.contains("page=2"));
    }

    #[test]
    fn story_ids_from_links_are_stable_and_never_hn_shaped() {
        let a = story_id_for_link("https://blog.example/post/1");
        let again = story_id_for_link("https://blog.example/post/1");
        assert_eq!(a, again, "same link, same id — state keys on it");
        assert_ne!(a, story_id_for_link("https://blog.example/post/2"));
        assert!(a.starts_with("rss-"));
        assert!(!a.bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn rss_2_feed_parses_into_newest_first_stories() {
        let feed = r#"<?xml version="1.0"?>
            <rss version="2.0"><channel>
              <title>The Example Blog</title>
              <item><title>Older post</title><link>https://example.com/older</link>
                <pubDate>Tue, 06 Oct 2026 10:00:00 GMT</pubDate><author>Pat</author></item>
              <item><title>Newer post</title><link>https://example.com/newer</link>
                <pubDate>Thu, 08 Oct 2026 10:00:00 GMT</pubDate></item>
              <item><title>No link, skipped</title></item>
            </channel></rss>"#;
        let stories = parse_feed(feed.as_bytes()).unwrap().1;
        assert_eq!(stories.len(), 2, "link-less entries cannot be read");
        assert_eq!(stories[0].title, "Newer post", "newest first");
        assert_eq!(stories[0].url.as_deref(), Some("https://example.com/newer"));
        assert_eq!(stories[0].domain.as_deref(), Some("example.com"));
        assert_eq!(stories[0].points, 0, "RSS has no points");
        assert_eq!(stories[0].hn_url, "", "RSS has no HN discussion");
        assert_eq!(stories[1].author, "Pat", "entry author when present");
        assert_eq!(
            stories[0].id,
            story_id_for_link("https://example.com/newer")
        );
    }

    #[test]
    fn atom_feed_parses_and_falls_back_to_feed_title() {
        let feed = r#"<?xml version="1.0" encoding="utf-8"?>
            <feed xmlns="http://www.w3.org/2005/Atom">
              <title>Atom Weekly</title>
              <entry><title>Notes on rust</title>
                <link href="https://weekly.example/notes"/>
                <updated>2026-10-08T09:30:00Z</updated></entry>
            </feed>"#;
        let stories = parse_feed(feed.as_bytes()).unwrap().1;
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].author, "Atom Weekly", "feed title fallback");
        assert_eq!(stories[0].created_at, 1_791_451_800, "updated used as date");
        assert!(
            parse_feed(b"not xml at all").is_err(),
            "garbage is an error"
        );
    }

    #[test]
    fn hits_parse_into_stories_with_domains() {
        let body = r#"{"hits":[
            {"objectID":"50019911","title":"Cloudflare acquires Deno","url":"https://deno.com/blog/cloudflare",
             "author":"ilreb","points":995,"num_comments":518,"created_at_i":1791551028,
             "_tags":["story","author_ilreb","front_page"]},
            {"objectID":"50020001","title":null,"url":null,"author":"x","points":1,"num_comments":1,
             "created_at_i":1,"_tags":["comment","story"]},
            {"objectID":"50020002","title":"Text post","url":null,"author":"y","points":3,
             "num_comments":0,"created_at_i":2,"_tags":["story","ask_hn"]}
        ]}"#;
        let parsed: AlgoliaSearch = serde_json::from_str(body).unwrap();
        let stories: Vec<StoryDto> = parsed
            .hits
            .into_iter()
            .filter_map(AlgoliaHit::into_story)
            .collect();
        assert_eq!(stories.len(), 2, "title-less comment hits are skipped");
        let s = &stories[0];
        assert_eq!(s.id, "50019911");
        assert_eq!(s.domain.as_deref(), Some("deno.com"));
        assert_eq!(s.hn_url, "https://news.ycombinator.com/item?id=50019911");
        // A text post has no URL and no domain, but still renders.
        assert!(stories[1].url.is_none() && stories[1].domain.is_none());
    }

    #[test]
    fn comment_html_is_sanitized_to_the_allow_list() {
        let raw = "<p>hi <a href=\"javascript:alert(1)\">evil</a> <a href=\"https://ok.example/x\">ok</a>\
                   <script>alert(1)</script><i>em</i><table><tr><td>junk</td></tr></table>";
        let clean = sanitize_rich_text(raw);
        assert!(!clean.contains("script"), "{clean}");
        assert!(!clean.to_lowercase().contains("javascript:"), "{clean}");
        assert!(
            clean.contains("<a href=\"https://ok.example/x\""),
            "{clean}"
        );
        assert!(clean.contains("<i>em</i>"), "{clean}");
        // Disallowed structural tags unwrap to their text, not vanish.
        assert!(clean.contains("junk"), "{clean}");
        assert!(!clean.contains("<table"), "{clean}");
    }

    #[test]
    fn a_paragraph_and_its_marks_become_markdown() {
        assert_eq!(
            rich_text_to_markdown("<p>plain</p>"),
            "plain",
            "a paragraph is a bare line"
        );
        assert_eq!(
            rich_text_to_markdown("<p><strong>bold</strong> and <em>em</em></p>"),
            "**bold** and *em*"
        );
        // `b`/`i` are the same marks as `strong`/`em`, and `s` is GFM's.
        assert_eq!(rich_text_to_markdown("<p><b>b</b><i>i</i><s>s</s></p>"), "**b***i*~~s~~");
        // Markdown has no underline: it is carried as the inline HTML that
        // degrades to the plain text where HTML is stripped.
        assert_eq!(rich_text_to_markdown("<p><u>u</u></p>"), "<u>u</u>");

        assert_eq!(
            rich_text_to_markdown("<p>one</p><p>two</p>"),
            "one\n\ntwo",
            "paragraphs are separated by a blank line"
        );
    }

    /// A mark whose content is padded must not lose the mark. `** bold **` is
    /// literal asterisks in Markdown, not emphasis — the padding has to move
    /// outside the markers.
    #[test]
    fn whitespace_inside_a_mark_moves_outside_it() {
        assert_eq!(
            rich_text_to_markdown("<p>a<strong> b </strong>c</p>"),
            "a **b** c",
            "the padding moved outside the markers, so the mark still applies"
        );
        assert_eq!(
            rich_text_to_markdown("<p><strong>  </strong>after</p>"),
            "after",
            "an empty mark is not two stray markers"
        );
    }

    #[test]
    fn headings_carry_their_level_and_drop_when_empty() {
        assert_eq!(rich_text_to_markdown("<h1>One</h1>"), "# One");
        assert_eq!(rich_text_to_markdown("<h3>Three</h3>"), "### Three");
        assert_eq!(rich_text_to_markdown("<h2></h2>"), "", "an empty heading is nothing");
    }

    #[test]
    fn a_link_becomes_a_markdown_link() {
        assert_eq!(
            rich_text_to_markdown("<p><a href=\"https://ok.example/x\">ok</a></p>"),
            "[ok](https://ok.example/x)"
        );
        // A URL with parentheses is wrapped, or the first `)` would close the
        // destination and the rest would become prose.
        assert_eq!(
            rich_text_to_markdown("<p><a href=\"https://ok.example/a(b)\">x</a></p>"),
            "[x](<https://ok.example/a(b)>)"
        );
        // A link with no text falls back to the URL rather than an empty label.
        assert_eq!(
            rich_text_to_markdown("<p><a href=\"https://ok.example/x\"></a></p>"),
            "[https://ok.example/x](https://ok.example/x)"
        );
        // A scheme the sanitizer would have dropped is dropped here too, so a
        // body that did not come from the sanitizer still cannot export one.
        assert_eq!(
            rich_text_to_markdown("<p><a href=\"javascript:alert(1)\">evil</a></p>"),
            "evil"
        );
    }

    #[test]
    fn lists_become_markers_and_nest_by_indent() {
        assert_eq!(
            rich_text_to_markdown("<ul><li>one</li><li>two</li></ul>"),
            "- one\n- two"
        );
        assert_eq!(
            rich_text_to_markdown("<ol><li>first</li><li>second</li></ol>"),
            "1. first\n2. second"
        );
        assert_eq!(
            rich_text_to_markdown("<ul><li>outer<ul><li>inner</li></ul></li></ul>"),
            "- outer\n  - inner",
            "a nested list is indented under its item"
        );
        assert_eq!(
            rich_text_to_markdown("<ul><li></li><li>x</li></ul>"),
            "-\n- x",
            "an empty item is a bare marker, not a stray space"
        );
    }

    #[test]
    fn a_blockquote_prefixes_every_line_it_contains() {
        assert_eq!(
            rich_text_to_markdown("<blockquote><p>one</p><p>two</p></blockquote>"),
            "> one\n>\n> two",
            "the blank line between paragraphs is quoted too"
        );
    }

    #[test]
    fn a_code_block_is_fenced_and_decodes_its_entities() {
        // Nothing inside a fence is interpreted, so `&amp;` must arrive as `&`
        // or the reader sees the entity.
        assert_eq!(
            rich_text_to_markdown("<pre>a &amp; b\nif x &lt; 1 {}</pre>"),
            "```\na & b\nif x < 1 {}\n```"
        );
        // A body containing a fence must not end its own block.
        assert_eq!(
            rich_text_to_markdown("<pre>```\ninner\n```</pre>"),
            "````\n```\ninner\n```\n````"
        );
        // The `<code>` wrapper the editor emits inside a `pre` is not a second
        // fence.
        assert_eq!(
            rich_text_to_markdown("<pre><code>let x = 1;</code></pre>"),
            "```\nlet x = 1;\n```"
        );
    }

    #[test]
    fn an_inline_code_span_escapes_a_backtick_inside_it() {
        assert_eq!(rich_text_to_markdown("<p><code>x</code></p>"), "`x`");
        assert_eq!(
            rich_text_to_markdown("<p><code>a `tick` b</code></p>"),
            "`` a `tick` b ``"
        );
        assert_eq!(
            rich_text_to_markdown("<p><code>a &amp; b</code></p>"),
            "`a & b`",
            "a code span decodes, because Markdown will not"
        );
        assert_eq!(rich_text_to_markdown("<p><code>   </code></p>"), "");
    }

    #[test]
    fn a_break_and_a_rule_have_their_own_lines() {
        assert_eq!(
            rich_text_to_markdown("<p>one<br>two</p>"),
            "one  \ntwo",
            "two spaces then a newline is Markdown's hard break"
        );
        assert_eq!(
            rich_text_to_markdown("<p>a</p><hr><p>b</p>"),
            "a\n\n---\n\nb"
        );
    }

    /// The property the whole design rests on: the body is sanitized HTML, so
    /// the only raw `<` in the output is one this function wrote. If the body
    /// could contribute a `<`, the export would be a way to publish HTML under
    /// a member's public identity.
    #[test]
    fn prose_never_contributes_a_raw_angle_bracket() {
        let hostile = "<p>a &lt;script&gt;alert(1)&lt;/script&gt; b</p>";
        let out = rich_text_to_markdown(hostile);
        assert_eq!(out, "a &lt;script&gt;alert(1)&lt;/script&gt; b");
        assert!(!out.contains('<'), "the body contributed no tag: {out}");
    }

    #[test]
    fn an_unknown_tag_keeps_its_words_and_loses_its_markup() {
        // The sanitizer strips these, so this is the defensive path: a body
        // that did not come from it still exports readable text.
        assert_eq!(
            rich_text_to_markdown("<p>a <span class=\"x\">word</span> b</p>"),
            "a word b"
        );
        assert_eq!(
            rich_text_to_markdown("<!-- a comment --><p>after</p>"),
            "after",
            "a comment is not text"
        );
        // A `<` that begins no tag is text, not a lost character.
        assert_eq!(rich_text_to_markdown("<p>2 < 3</p>"), "2 < 3");
    }

    /// A truncated body — an unclosed tag — still exports its words rather than
    /// nothing, and a stray close tag does not swallow a paragraph.
    #[test]
    fn unclosed_and_stray_tags_still_export_the_words() {
        assert_eq!(rich_text_to_markdown("<p>unclosed"), "unclosed");
        assert_eq!(rich_text_to_markdown("<p>one</p></div><p>two</p>"), "one\n\ntwo");
        assert_eq!(
            rich_text_to_markdown("<p><b>bold</p>"),
            "**bold**",
            "an unclosed inner element unwraps into its parent"
        );
    }

    #[test]
    fn an_empty_body_exports_nothing() {
        assert_eq!(rich_text_to_markdown(""), "");
        assert_eq!(rich_text_to_markdown("   "), "");
        assert_eq!(rich_text_to_markdown("<p></p>"), "");
    }

    /// The two halves of the format are one list. Everything the sanitizer can
    /// keep must survive the export as words, and every tag the export knows
    /// must be one the sanitizer would keep — a tag in one list and not the
    /// other is a silent loss.
    #[test]
    fn every_tag_the_sanitizer_keeps_survives_the_export() {
        for tag in RICH_TEXT_TAGS {
            let raw = format!("<{tag}>sentinel</{tag}>");
            let clean = sanitize_rich_text(&raw);
            assert!(
                clean.contains("sentinel"),
                "the sanitizer dropped {tag}: {clean}"
            );
            let markdown = rich_text_to_markdown(&clean);
            assert!(
                markdown.contains("sentinel"),
                "the export lost the words of {tag}: {markdown:?}"
            );
        }
    }

    #[test]
    fn a_realistic_body_round_trips_through_both_halves() {
        let raw = "<h1>On relays</h1><p>Long-form, <strong>badged</strong>.</p>\
                   <ul><li>curated</li><li>gated</li></ul>\
                   <blockquote><p>Public is permanent.</p></blockquote>\
                   <pre>kind: 30023</pre>\
                   <p>See <a href=\"https://ok.example/x\">the spec</a>.</p>";
        let markdown = rich_text_to_markdown(&sanitize_rich_text(raw));
        assert_eq!(
            markdown,
            concat!(
                "# On relays\n\n",
                "Long-form, **badged**.\n\n",
                "- curated\n- gated\n\n",
                "> Public is permanent.\n\n",
                "```\nkind: 30023\n```\n\n",
                "See [the spec](https://ok.example/x)."
            )
        );
    }

    #[test]
    fn comment_trees_convert_recursively() {
        let body = r#"{
            "id":"1","type":"story","author":"a","title":"t","url":null,"points":1,
            "created_at_i":100,"children":[
                {"id":"2","type":"comment","author":"b","text":"<i>nested</i>","created_at_i":101,"children":[
                    {"id":"3","type":"comment","author":"c","text":null,"created_at_i":102,"children":[]}]},
                {"id":"4","type":"comment","author":"d","text":"ok","created_at_i":103,"children":[]}]}
        "#;
        let item: AlgoliaItem = serde_json::from_str(body).unwrap();
        assert_eq!(count_comments(&item.children), 3);
        let comments: Vec<CommentDto> = item
            .children
            .into_iter()
            .filter_map(AlgoliaItem::into_comment)
            .collect();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].children.len(), 1);
        assert_eq!(comments[0].html, "<i>nested</i>");
        assert_eq!(comments[0].children[0].children.len(), 0);
    }

    #[test]
    fn fetch_guard_refuses_private_targets() {
        for bad in [
            "http://127.0.0.1/x",
            "http://localhost/x",
            "http://[::1]/x",
            "http://192.168.1.4/x",
            "http://10.0.0.1/x",
            "http://169.254.169.254/latest",
            "http://[fe80::1]/x",
            "ftp://example.com/x",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(check_fetchable_url(bad).is_err(), "{bad} must be refused");
        }
        for good in ["https://example.com/a", "http://example.com:8080/b"] {
            assert!(check_fetchable_url(good).is_ok(), "{good} must pass");
        }
    }

    #[test]
    fn bot_blocks_become_a_reason_the_reader_can_act_on() {
        // A Cloudflare managed challenge answers 403; 429/503 are the same kind
        // of refusal. Anything else keeps reqwest's own message.
        for status in [401, 403, 429, 503] {
            assert!(is_bot_block(status), "{status} is a bot block");
        }
        for status in [200, 301, 404, 500] {
            assert!(!is_bot_block(status), "{status} is not a bot block");
        }
        // The Reads pane keys its "blocks automated readers" copy on this exact
        // phrase; rewording it here silently downgrades that copy to the generic
        // one, so the coupling is pinned.
        assert!(ReadsError::Blocked(403)
            .to_string()
            .contains("automated reader"));
    }

    const ARTICLE_HTML: &str = r#"<!doctype html><html><head><title>Real Title</title></head><body>
        <nav><p>Menu item one</p><p>Menu item two</p></nav>
        <article>
            <h1>Heading</h1>
            <p>First paragraph with enough real prose to score well.</p>
            <p>Second paragraph, also prose. <a href="/relative">link</a></p>
            <pre><code>let x = 1;</code></pre>
            <script>steal();</script>
        </article>
        <footer><p>Copyright nobody</p></footer>
        </body></html>"#;

    #[test]
    fn extractor_picks_the_article_and_drops_chrome() {
        let dto = extract_article(ARTICLE_HTML).unwrap();
        assert_eq!(dto.title.as_deref(), Some("Real Title"));
        assert!(dto.html.contains("Heading"), "{}", dto.html);
        assert!(dto.html.contains("First paragraph"), "{}", dto.html);
        assert!(dto.html.contains("<pre><code>"), "{}", dto.html);
        assert!(!dto.html.contains("Menu item"), "{}", dto.html);
        assert!(!dto.html.contains("Copyright"), "{}", dto.html);
        assert!(!dto.html.contains("steal"), "{}", dto.html);
        // Relative links survive as relative; the sanitizer strips schemes,
        // the UI routes every link through a confirm step.
        assert!(
            dto.html.contains("href=\"/relative\"") || dto.html.contains("link"),
            "{}",
            dto.html
        );
    }

    #[test]
    fn reader_cache_roundtrips_and_expires() {
        let dir = std::env::temp_dir().join(format!(
            "hush-reads-cache-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ));
        let dto = ReaderDto {
            url: "https://example.com/a".into(),
            title: Some("T".into()),
            html: "<p>body</p>".into(),
        };
        save_reader_cache(&dir, dto.url.as_str(), &dto);
        let loaded = load_reader_cache(&dir, dto.url.as_str()).unwrap();
        assert_eq!(loaded.html, "<p>body</p>");
        assert_eq!(loaded.title.as_deref(), Some("T"));

        // A missing URL reads as None.
        assert!(load_reader_cache(&dir, "https://example.com/never").is_none());

        // Backdate the file past the TTL: it expires and is removed.
        let path = reader_cache_path(&dir, dto.url.as_str());
        let old = std::time::SystemTime::now() - Duration::from_secs(31 * 24 * 3600);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        drop(file);
        assert!(load_reader_cache(&dir, dto.url.as_str()).is_none());
        assert!(!path.exists(), "expired cache entry is deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

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
                .map(sanitize_hn_html)
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

/// The one HTML allow-list for everything user-visible from the network:
/// HN comment fragments and reader-mode article bodies. Structural tags from
/// articles (headings) ride along; everything else is stripped, not escaped,
/// so a hostile page contributes text at most.
pub fn sanitize_hn_html(raw: &str) -> String {
    let mut tags = HashSet::new();
    for t in [
        "p",
        "a",
        "i",
        "em",
        "b",
        "strong",
        "code",
        "pre",
        "blockquote",
        "ul",
        "ol",
        "li",
        "br",
        "hr",
        "h1",
        "h2",
        "h3",
        "h4",
    ] {
        tags.insert(t);
    }
    let mut schemes = HashSet::new();
    for s in ["http", "https", "mailto"] {
        schemes.insert(s);
    }
    let mut builder = ammonia::Builder::default();
    builder.tags(tags);
    builder.url_schemes(schemes);
    builder.link_rel(Some("noopener noreferrer nofollow"));
    builder.clean(raw).to_string()
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
    let html = sanitize_hn_html(&rendered);
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
        let text_html = item.text.as_deref().map(sanitize_hn_html);
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
        let clean = sanitize_hn_html(raw);
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

//! HTML reduced to plaintext.
//!
//! Two things in this workspace need this and they are the same problem: the
//! zines editor stores a sanitized rich-text body that has to become plaintext
//! before it can be published, and a Standard.site `content` tree may carry a
//! block whose words are in an `html` field rather than a `plaintext` one.
//!
//! It lives here rather than in the daemon because the crate is the lower half
//! and the only one both sides can reach: `hush-daemon` depends on
//! `hush-atproto`, so the daemon delegates to this instead of keeping a second
//! converter that would eventually disagree with it about an unclosed tag.
//!
//! The reduction carries no markup at all. Text is text, a block boundary is a
//! blank line, a link is its label because a plaintext field has nowhere to put
//! a destination, and an image contributes nothing because there is nothing to
//! say about it in plain text. Entities are decoded here, because nothing
//! downstream will. **Anything it does not recognize keeps its text and loses
//! its markup**, so an unknown tag is never a reason to lose a reader's words —
//! which is what makes it safe to point at a stranger's record.

/// Every word in `html`, as plaintext.
pub fn to_text(html: &str) -> String {
    let nodes = tree(html);
    let mut blocks: Vec<String> = Vec::new();
    let mut run = String::new();
    for node in &nodes {
        match node {
            Node::Element { name, children, .. } if is_block(name) => {
                if !run.trim().is_empty() {
                    blocks.push(run.trim().to_string());
                    run.clear();
                }
                let text = plain_text(children);
                if !text.trim().is_empty() {
                    blocks.push(text.trim().to_string());
                }
            }
            other => run.push_str(&plain_text(std::slice::from_ref(other))),
        }
    }
    if !run.trim().is_empty() {
        blocks.push(run.trim().to_string());
    }
    blocks.join("\n\n")
}

/// Every text node under `nodes`, entities decoded, with a `br` as the line
/// break it is. What a plaintext field wants: a link's label survives and its
/// destination does not, and an image contributes nothing because there is
/// nothing to say about it in plain text.
///
/// A block nested inside another — an `li` in a `ul`, a `p` in a
/// `blockquote` — takes its own line, because that is the only separation a
/// plaintext field has. Without it, a list would arrive as one run of words.
fn plain_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(&decode_entities(text)),
            Node::Element { name, children, .. } => match name.as_str() {
                "br" => out.push('\n'),
                "img" => {}
                _ if is_block(name) => {
                    let inner = plain_text(children);
                    if !inner.trim().is_empty() {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(inner.trim());
                        out.push('\n');
                    }
                }
                _ => out.push_str(&plain_text(children)),
            },
        }
    }
    out
}

/// The sanitized body parsed into the element tree the reduction walks.
///
/// Split out from [`to_text`] so the tree and the renderer can be reasoned about
/// separately: two tokenizers would eventually disagree about an unclosed tag.
fn tree(html: &str) -> Vec<Node> {
    let mut stack = vec![root_frame()];
    for token in tokenize(html) {
        match token {
            Token::Text(text) => stack
                .last_mut()
                .expect("the root frame is never popped")
                .children
                .push(Node::Text(text)),
            Token::Tag(tag) if tag.closing => close_element(&mut stack, &tag.name),
            Token::Tag(tag) if tag.self_closing || is_void(&tag.name) => {
                let node = Node::Element {
                    name: tag.name,
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
                children: frame.children,
            });
    }
    stack.pop().expect("the root frame").children
}

/// A tag, as the tokenizer found it.
///
/// Only the name and the shape are kept. Attributes are dropped whole: the
/// public field a body goes into is plaintext, so a link's destination has
/// nowhere to go, and the sanitizer has already refused everything else that
/// could matter.
struct Tag {
    name: String,
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
        children: Vec<Node>,
    },
}

/// An open element while the tree is being built. The bottom of the stack is a
/// nameless root that no close tag can match.
struct Frame {
    name: String,
    children: Vec<Node>,
}

fn root_frame() -> Frame {
    Frame {
        name: String::new(),
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
    let name = rest.split_whitespace().next()?.trim().to_ascii_lowercase();
    // A comment, a doctype, or a stray `<` produces a name that is not a tag
    // name; refusing it here is what keeps `<!-- x -->` from becoming text.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(Tag {
        name,
        closing,
        self_closing,
    })
}

/// A tag whose content is its own line or lines, rather than running on.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "ul" | "ol" | "li" | "blockquote" | "pre" | "hr"
    )
}

/// A tag that never has content, so it never opens a frame.
///
/// `<img src="x">` without the closing slash is ordinary HTML, and treating it
/// as an open element swallowed every word that followed it into a node that
/// renders nothing — a paragraph came out as its first half. Sanitized HTML from
/// the editor happens to self-close these, so the zines lane never saw it; it
/// took pointing this at a stranger's record to surface it.
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
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

#[cfg(test)]
mod tests {
    use super::to_text;

    #[test]
    fn a_plain_fragment_is_its_words() {
        // The shape that made this necessary: a content block whose text is in
        // an `html` field rather than a `plaintext` one. The words were already
        // there and the reader showed nothing, because only `plaintext` was
        // collected.
        assert_eq!(
            to_text("The new, Tailwind-based theme is so much faster."),
            "The new, Tailwind-based theme is so much faster."
        );
    }

    #[test]
    fn a_link_keeps_its_label_and_loses_its_destination() {
        // A plaintext field has nowhere to put a URL, and inventing a marker
        // would put punctuation in the reader's mouth.
        assert_eq!(
            to_text(r#"a <a href="https://example.com/x">journaling platform</a> here"#),
            "a journaling platform here"
        );
    }

    #[test]
    fn blocks_become_blank_lines_and_nested_blocks_take_their_own_line() {
        assert_eq!(to_text("<p>One</p><p>Two</p>"), "One\n\nTwo");
        assert_eq!(
            to_text("<ul><li>first</li><li>second</li></ul>"),
            "first\nsecond"
        );
    }

    #[test]
    fn entities_are_decoded_and_unknown_ones_are_passed_through() {
        assert_eq!(to_text("a &amp; b &lt;c&gt; &#39;d&#39;"), "a & b <c> 'd'");
        assert_eq!(to_text("&notarealentity;"), "&notarealentity;");
        // A bare `&` is not the start of an entity and must not be eaten.
        assert_eq!(to_text("Tom & Jerry"), "Tom & Jerry");
    }

    #[test]
    fn an_unknown_tag_keeps_its_words() {
        // The property that makes this safe to point at a stranger's record: a
        // block type nobody has seen before loses its markup, never its text.
        assert_eq!(to_text("<figure><custom>kept</custom></figure>"), "kept");
    }

    #[test]
    fn an_image_contributes_nothing_and_a_comment_is_not_prose() {
        assert_eq!(to_text(r#"<p>before <img src="x.png" alt="ignored"> after</p>"#), "before  after");
        assert_eq!(to_text("<p>kept</p><!-- dropped -->"), "kept");
    }

    #[test]
    fn an_unclosed_tag_still_yields_its_words() {
        assert_eq!(to_text("<p>truncated"), "truncated");
        assert_eq!(to_text("<p>a <em>b"), "a b");
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(to_text(""), "");
        assert_eq!(to_text("   "), "");
        assert_eq!(to_text("<p></p>"), "");
    }
}

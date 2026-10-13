/**
 * A minimal Markdown renderer for NIP-23 bodies.
 *
 * A public post's body is Markdown written by a stranger and fetched over a
 * relay, so this is not a general Markdown implementation and must not become
 * one. It covers the block and inline forms an article actually uses — headings,
 * paragraphs, lists, quotes, fenced code, rules, emphasis, code spans and links —
 * and it is written so the safe case is the default:
 *
 * - Every line is HTML-escaped before anything else, so raw HTML in the source is
 *   inert text. Nothing here can emit a tag the source wrote.
 * - Images are not rendered. `![alt](url)` prints its alt text, because an image
 *   is a call-out to a stranger's server and this page will not make one.
 * - A link is emitted only for `http`, `https` or `mailto`. The reading pane
 *   intercepts the click and copies the URL rather than navigating.
 *
 * The output still goes through `sanitizeHtml`, which is the gate: this renderer
 * decides how a body *reads*, and the sanitizer decides what is *allowed*.
 */
import { escapeHtml } from "./richText.ts";

/** Markdown source to HTML. Never throws, never returns unescaped source. */
export function markdownToHtml(source: string): string {
  if (!source.trim()) return "";
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    const fence = /^\s*(```|~~~)/.exec(line);
    if (fence) {
      const marker = fence[1];
      const close = new RegExp(`^\\s*${marker}\\s*$`);
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !close.test(lines[i])) {
        code.push(lines[i]);
        i += 1;
      }
      i += 1; // the closing fence, or the end of the body
      out.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
      continue;
    }

    if (/^\s*$/.test(line)) {
      i += 1;
      continue;
    }

    if (isRule(line)) {
      out.push("<hr>");
      i += 1;
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = heading[1].length;
      out.push(`<h${level}>${inline(heading[2].trim())}</h${level}>`);
      i += 1;
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const quote: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        quote.push(lines[i].replace(/^\s*>\s?/, ""));
        i += 1;
      }
      out.push(`<blockquote>${markdownToHtml(quote.join("\n"))}</blockquote>`);
      continue;
    }

    const item = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(line);
    if (item) {
      const ordered = /\d/.test(item[1]);
      const items: string[] = [];
      while (i < lines.length) {
        const next = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(lines[i]);
        if (!next) break;
        items.push(`<li>${inline(next[2].trim())}</li>`);
        i += 1;
      }
      const tag = ordered ? "ol" : "ul";
      out.push(`<${tag}>${items.join("")}</${tag}>`);
      continue;
    }

    // A paragraph runs until a blank line or the start of another block.
    const para: string[] = [];
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !startsBlock(lines[i])) {
      para.push(lines[i].trim());
      i += 1;
    }
    if (para.length) out.push(`<p>${inline(para.join(" "))}</p>`);
    // Defensive: the block checks above consume every line they claim, so this
    // is unreachable. It is here so a future regex change cannot spin forever.
    else i += 1;
  }

  return out.join("\n");
}

/** Whether a line opens a block, and so ends a paragraph. */
function startsBlock(line: string): boolean {
  return (
    /^\s*(```|~~~)/.test(line) ||
    isRule(line) ||
    /^#{1,6}\s+/.test(line) ||
    /^\s*>\s?/.test(line) ||
    /^\s*([-*+]|\d+[.)])\s+/.test(line)
  );
}

/**
 * A thematic break: three or more `-`, `*` or `_`, optionally spaced out.
 *
 * The spaces matter: `* * *` is a rule in Markdown, and without this it reads as
 * a list item whose text is `* *`, which then reads as emphasis and prints as a
 * blank bullet. Checked before the list rule for the same reason.
 */
function isRule(line: string): boolean {
  return /^\s*([-*_])(?:\s*\1){2,}\s*$/.test(line);
}

/**
 * The inline forms, applied to already-escaped text.
 *
 * Code spans come first so their contents are not read as emphasis, and images
 * are handled before links so `![alt](url)` does not match as a link whose label
 * happens to start with `!`.
 *
 * A target is matched with one level of nesting allowed, because a URL can
 * contain parentheses — `javascript:alert(1)` is the case that matters. Matching
 * only up to the first `)` would leave the rest of the target behind as loose
 * text beside the label.
 */
function inline(text: string): string {
  const target = String.raw`\(((?:[^()\s]|\([^()]*\))+)\)`;
  let out = escapeHtml(text);
  out = out.replace(/`([^`]+)`/g, (_m, code) => `<code>${code}</code>`);
  out = out.replace(
    new RegExp(String.raw`!\[([^\]]*)\]${target}`, "g"),
    (_m, alt) => alt,
  );
  out = out.replace(new RegExp(String.raw`\[([^\]]+)\]${target}`, "g"), (_m, label, href) =>
    safeHref(href) ? `<a href="${href}" rel="noreferrer noopener">${label}</a>` : label,
  );
  out = out.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  out = out.replace(/(^|[^*])\*([^*]+)\*/g, "$1<em>$2</em>");
  out = out.replace(/__([^_]+)__/g, "<strong>$1</strong>");
  out = out.replace(/(^|[^_])_([^_]+)_/g, "$1<em>$2</em>");
  return out;
}

/**
 * Whether a link target is one this page will render as a link at all.
 *
 * `href` is the already-escaped text, so a scheme cannot be smuggled in behind
 * an entity: `javascript:` and `data:` survive escaping unchanged and are
 * refused here.
 */
function safeHref(href: string): boolean {
  return /^(https?:|mailto:)/i.test(href);
}

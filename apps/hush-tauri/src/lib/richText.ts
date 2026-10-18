/**
 * Rich-text support for mail bodies.
 *
 * A mail body is still a single opaque string on the wire and in the encrypted
 * log; nothing in `hush-core` or the daemon interprets it. What changed is that
 * the composer now writes a constrained HTML subset into that string instead of
 * plain text, and the renderer treats the string as markup.
 *
 * Because a body can arrive from another member (and a member is not trusted),
 * the rule is: never hand a body to `innerHTML` without passing it through
 * `sanitizeHtml` first. Everything here exists to make that the easy path.
 *
 * The subset is deliberately small — character formatting, headings, lists,
 * quotes, code and links — so the renderer and the composer agree on exactly
 * what can exist, and so a sanitized body never carries a construct the editor
 * cannot re-open for a draft.
 */

import { memoizeLru } from "./memo.ts";

/** Elements removed with their whole subtree: keeping their text would leak
 *  script/style bodies into the message. */
const DROP_TAGS = new Set([
  "script",
  "style",
  "iframe",
  "object",
  "embed",
  "template",
  "noscript",
  "svg",
  "math",
  "form",
  "input",
  "button",
  "textarea",
  "select",
  "option",
  "link",
  "meta",
  "base",
  "head",
  "title",
  "img",
  "video",
  "audio",
  "canvas",
]);

/** Elements kept, possibly with an unwrap for the ones that are structural. */
const ALLOWED_TAGS = new Set([
  "b",
  "strong",
  "i",
  "em",
  "u",
  "s",
  "strike",
  "del",
  "ins",
  "p",
  "div",
  "br",
  "span",
  "ul",
  "ol",
  "li",
  "blockquote",
  "pre",
  "code",
  "h1",
  "h2",
  "h3",
  "h4",
  "a",
]);

/** Attributes kept per tag. Anything else is dropped, which is what removes
 *  every `on*` handler and every `style`. */
const ALLOWED_ATTRS: Record<string, ReadonlySet<string>> = {
  a: new Set(["href", "title"]),
};

/** Only absolute http(s) and mailto links survive; everything else (notably
 *  `javascript:` and `data:`) is dropped. */
const SAFE_URL = /^(https?:\/\/|mailto:)/i;

/**
 * How many flattened bodies `htmlToText` keeps.
 *
 * Sized for the working set the mail list redraws in one pass — a long folder —
 * rather than for the whole store. The map holds a reference to the body string
 * the store already owns plus the flattened text, so a full cache costs roughly
 * the size of a few hundred previews, not a few hundred messages.
 */
const TEXT_CACHE_MAX = 512;

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Whether a string contains anything that parses as an element tag. */
export function hasHtmlTags(html: string): boolean {
  return /<\/?[a-z][a-z0-9]*(\s[^>]*)?>/i.test(html);
}

function sanitizeUrl(raw: string): string | null {
  // Control characters are how `java\nscript:` slips past naive checks.
  const cleaned = raw.replace(/[\u0000-\u001f\u007f]/g, "").trim();
  return SAFE_URL.test(cleaned) ? cleaned : null;
}

function sanitizeNode(node: Node): Node | null {
  if (node.nodeType === Node.TEXT_NODE) {
    return document.createTextNode(node.nodeValue ?? "");
  }
  if (node.nodeType !== Node.ELEMENT_NODE) return null;

  const el = node as Element;
  const tag = el.tagName.toLowerCase();

  if (DROP_TAGS.has(tag)) return null;

  const children = Array.from(el.childNodes);

  if (!ALLOWED_TAGS.has(tag)) {
    // Unknown but harmless (e.g. `<font>`, `<center>`): keep the text, lose the
    // element. Returning a fragment splices the children in place.
    const frag = document.createDocumentFragment();
    for (const child of children) {
      const clean = sanitizeNode(child);
      if (clean) frag.appendChild(clean);
    }
    return frag;
  }

  const out = document.createElement(tag);
  const allowed = ALLOWED_ATTRS[tag];
  if (allowed) {
    for (const attr of allowed) {
      const value = el.getAttribute(attr);
      if (value === null) continue;
      if (attr === "href") {
        const url = sanitizeUrl(value);
        if (url) out.setAttribute("href", url);
      } else {
        out.setAttribute(attr, value);
      }
    }
    if (tag === "a") out.setAttribute("rel", "noopener noreferrer");
  }
  for (const child of children) {
    const clean = sanitizeNode(child);
    if (clean) out.appendChild(clean);
  }
  return out;
}

/**
 * Return an HTML string that is safe to assign to `innerHTML`.
 *
 * Parsing goes through `DOMParser`, whose document is inert: scripts do not
 * run and no resource is fetched. The tree is then rebuilt from an allowlist
 * rather than filtered, so a construct nobody thought of is dropped by default
 * instead of passing through.
 */
export function sanitizeHtml(html: string): string {
  if (!html) return "";
  const doc = new DOMParser().parseFromString(html, "text/html");
  const out = document.createElement("div");
  for (const child of Array.from(doc.body.childNodes)) {
    const clean = sanitizeNode(child);
    if (clean) out.appendChild(clean);
  }
  return out.innerHTML;
}

/**
 * HTML for display when the stored body may predate the rich-text composer.
 *
 * A body with no tags is plain text: escape it and turn its newlines into
 * `<br>`, because an HTML renderer collapses raw newlines. A body with tags is
 * sanitized and used as markup.
 */
export function bodyToDisplayHtml(body: string): string {
  if (!body) return "";
  if (!hasHtmlTags(body)) return escapeHtml(body).replace(/\r?\n/g, "<br>");
  return sanitizeHtml(body);
}

/**
 * Flatten a body to plain text for previews and search.
 *
 * Block boundaries become newlines so a two-paragraph message does not read as
 * one run-on line in the list.
 *
 * Memoized, because this is not a string operation. `sanitizeHtml` is a
 * `DOMParser` document parse, a recursive walk and a serialize; the flattened
 * result is then parsed again into a `div` and swept twice. Measured in a
 * browser at ~0.1 ms for a 170-character body with a list and a link, and the
 * mail list asks for the same body's text twice per row — once for the search
 * haystack, once for the snippet — on every fill. A 200-message folder was
 * therefore ~40 ms of main thread per fill, on a path that runs on every mail
 * reload rather than only when the folder is opened. The body cannot change
 * under a given string, so the answer is computed once.
 */
const flattenBody = memoizeLru(parseBodyText, TEXT_CACHE_MAX);

export function htmlToText(html: string): string {
  if (!html) return "";
  // Plain text is already its own flattening. Returning early keeps the common
  // case — a body that never went through the rich-text composer — off the
  // cache entirely, which is also why the cache only ever holds tagged bodies.
  if (!hasHtmlTags(html)) return html;
  return flattenBody(html);
}

/** `htmlToText`'s parse, split out so the memo can wrap it. */
function parseBodyText(html: string): string {
  const div = document.createElement("div");
  div.innerHTML = sanitizeHtml(html);
  for (const br of Array.from(div.querySelectorAll("br"))) {
    br.replaceWith("\n");
  }
  for (const li of Array.from(div.querySelectorAll("li"))) li.append("\n");
  for (const block of Array.from(
    div.querySelectorAll("p, div, blockquote, pre, h1, h2, h3, h4"),
  )) {
    block.append("\n");
  }
  return (div.textContent ?? "")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/** Whether an editor body holds nothing worth sending or saving. */
export function isBlankHtml(html: string): boolean {
  return htmlToText(html).length === 0;
}

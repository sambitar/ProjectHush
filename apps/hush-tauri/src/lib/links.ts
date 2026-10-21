/**
 * Link handling for chat, decided without a DOM.
 *
 * `/link <url>` is a deliberate way to send a link, and the thread renderer
 * uses the same parser so an ordinary pasted URL becomes clickable too. Only
 * absolute `http(s)` URLs with no embedded credentials are accepted; anything
 * else stays inert text. The renderer builds anchors from text nodes, never
 * `innerHTML`, and every click goes through the shell's external-open
 * confirmation, so the webview never navigates to a peer-supplied address.
 */

/** The largest `/link` this build will send. */
export const MAX_LINK_CHARS = 4096;

export interface LinkCommand {
  /** The text after `/link`, trimmed; empty when the command has no URL. */
  url: string;
}

/**
 * Parse a submitted line as `/link <url>`.
 *
 * Returns null for ordinary text, including `/linked` and a command that is
 * not at the start of the line. A bare `/link` is a command with an empty URL
 * so the caller can ask for one rather than sending the literal slash.
 */
export function parseLinkCommand(text: string): LinkCommand | null {
  const trimmed = text.trimStart();
  const match = /^\/link(?:\s|$)/i.exec(trimmed);
  if (!match) return null;
  return { url: trimmed.slice(match[0].length).trim() };
}

/**
 * A URL this app is willing to hand to the shell, or null.
 *
 * `hush_open_url` accepts only `http` and `https`; this mirrors that rule and
 * additionally refuses credentials (`https://user:pass@host`), which are a
 * phishing shape rather than a useful destination.
 */
export function normalizeExternalLink(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed || trimmed.length > MAX_LINK_CHARS) return null;
  if (/\s/.test(trimmed)) return null;
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return null;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
  if (!parsed.hostname) return null;
  if (parsed.username || parsed.password) return null;
  return parsed.toString();
}

export type LinkPart =
  | { kind: "text"; text: string }
  | { kind: "link"; text: string; href: string };

/** The URL-shaped runs in a message. Deliberately not a general URL parser. */
const URL_RE = /https?:\/\/[^\s<>"']+/gi;

/**
 * Split one message into text and link parts.
 *
 * A trailing period, comma or unmatched bracket belongs to the sentence, not
 * the URL; a balanced `)` stays inside, so a Wikipedia-style address survives.
 * A candidate that fails `normalizeExternalLink` is left in the text, which is
 * why a `javascript:` or credential-bearing address cannot become clickable.
 */
export function splitLinks(text: string): LinkPart[] {
  const parts: LinkPart[] = [];
  let last = 0;
  for (const match of text.matchAll(URL_RE)) {
    const start = match.index ?? 0;
    const candidate = trimTrailingPunctuation(match[0]);
    if (!candidate) continue;
    const href = normalizeExternalLink(candidate);
    if (!href) continue;
    if (start > last) parts.push({ kind: "text", text: text.slice(last, start) });
    parts.push({ kind: "link", text: candidate, href });
    last = start + candidate.length;
  }
  if (last < text.length) parts.push({ kind: "text", text: text.slice(last) });
  return parts;
}

function trimTrailingPunctuation(raw: string): string {
  let url = raw;
  for (;;) {
    const last = url[url.length - 1];
    if (!last) return url;
    if (".!?;:,".includes(last)) {
      url = url.slice(0, -1);
      continue;
    }
    if (last === ")" && count(url, "(") < count(url, ")")) {
      url = url.slice(0, -1);
      continue;
    }
    if (last === "]" && count(url, "[") < count(url, "]")) {
      url = url.slice(0, -1);
      continue;
    }
    if (last === "}" && count(url, "{") < count(url, "}")) {
      url = url.slice(0, -1);
      continue;
    }
    return url;
  }
}

function count(value: string, char: string): number {
  let total = 0;
  for (const c of value) if (c === char) total += 1;
  return total;
}

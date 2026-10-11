/**
 * Turns the backend's styled segments into DOM, safely.
 *
 * IRC text is hostile input. Nothing here ever uses `innerHTML`; every run of
 * text becomes a text node, and only URLs we recognize become anchors. Colours
 * are the classic 16-entry mIRC palette; a per-user hash gives each nick a
 * stable colour.
 */

import { invoke } from "@tauri-apps/api/core";

import type { IrcSegment } from "./ircTypes";

/** The 16 standard mIRC colours, in index order. */
export const IRC_COLORS: readonly string[] = [
  "#ffffff", // 0 white
  "#000000", // 1 black
  "#00007f", // 2 blue
  "#009300", // 3 green
  "#ff0000", // 4 red
  "#7f0000", // 5 brown
  "#9c009c", // 6 purple
  "#fc7f00", // 7 orange
  "#ffff00", // 8 yellow
  "#00fc00", // 9 light green
  "#009393", // 10 teal
  "#00ffff", // 11 light cyan
  "#0000fc", // 12 light blue
  "#ff00ff", // 13 pink
  "#7f7f7f", // 14 grey
  "#d2d2d2", // 15 light grey
];

/**
 * Nick colours chosen to stay readable on the classic white background. These
 * are deliberately not the full palette: several mIRC colours are too light to
 * read on white.
 */
const NICK_COLORS: readonly string[] = [
  "#00007f",
  "#009300",
  "#a00000",
  "#7f007f",
  "#b35c00",
  "#005f5f",
  "#0000c0",
  "#7f3f00",
  "#2f6f00",
  "#8b008b",
  "#005080",
  "#a03000",
];

/** A stable colour for a nickname, so a user looks the same everywhere. */
export function nickColor(nick: string): string {
  let hash = 5381;
  for (let i = 0; i < nick.length; i += 1) {
    hash = ((hash << 5) + hash + nick.charCodeAt(i)) | 0;
  }
  const index = Math.abs(hash) % NICK_COLORS.length;
  return NICK_COLORS[index];
}

const URL_RE = /https?:\/\/[^\s<>"']+/gi;
const TRAILING = /[.,;:!?)\]}]+$/;

/** Append `text` to `parent`, turning URLs into anchors that open externally. */
export function linkifyInto(parent: HTMLElement, text: string): void {
  let last = 0;
  URL_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = URL_RE.exec(text)) !== null) {
    const raw = match[0];
    const url = raw.replace(TRAILING, "");
    if (match.index > last) {
      parent.appendChild(document.createTextNode(text.slice(last, match.index)));
    }
    const anchor = document.createElement("a");
    anchor.className = "irc-link";
    anchor.href = url;
    anchor.textContent = url;
    anchor.addEventListener("click", (event) => {
      event.preventDefault();
      void invoke("hush_open_url", { url }).catch(() => {});
    });
    parent.appendChild(anchor);
    last = match.index + url.length;
  }
  if (last < text.length) {
    parent.appendChild(document.createTextNode(text.slice(last)));
  }
}

function styleSpan(segment: IrcSegment): HTMLSpanElement {
  const span = document.createElement("span");
  let fg = segment.color != null ? IRC_COLORS[segment.color] : null;
  let bg = segment.bg != null ? IRC_COLORS[segment.bg] : null;
  if (segment.reverse) {
    // Reverse with unset colours means "swap with the window defaults".
    const newFg = bg ?? "#ffffff";
    const newBg = fg ?? "#000000";
    fg = newFg;
    bg = newBg;
  }
  if (fg) span.style.color = fg;
  if (bg) span.style.background = bg;
  if (segment.bold) span.style.fontWeight = "bold";
  if (segment.italic) span.style.fontStyle = "italic";
  if (segment.underline) span.style.textDecoration = "underline";
  return span;
}

/** Build a fragment for a styled message body. */
export function renderSegments(segments: IrcSegment[]): DocumentFragment {
  const fragment = document.createDocumentFragment();
  for (const segment of segments) {
    if (segment.text.length === 0) continue;
    const plain =
      !segment.bold &&
      !segment.italic &&
      !segment.underline &&
      !segment.reverse &&
      segment.color == null &&
      segment.bg == null;
    if (plain && !URL_RE.test(segment.text)) {
      fragment.appendChild(document.createTextNode(segment.text));
      URL_RE.lastIndex = 0;
      continue;
    }
    URL_RE.lastIndex = 0;
    const span = styleSpan(segment);
    linkifyInto(span, segment.text);
    fragment.appendChild(span);
  }
  return fragment;
}

/** The visible text of a set of segments, for previews and titles. */
export function segmentsToText(segments: IrcSegment[]): string {
  return segments.map((s) => s.text).join("");
}

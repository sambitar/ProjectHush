/**
 * Where a rebuilt message pane should open.
 *
 * Both message panes — the 1:1 thread (`.chat-messages`) and the conclave chat
 * (`.conclave-messages`) — are thrown away and rebuilt on every render. A
 * detached element has no layout, so its `scrollTop` silently clamps to zero:
 * the position has to be read off the outgoing DOM, decided while the new pane
 * is still detached, and applied once it is in the document.
 *
 * That contract lives here, shared by both panes, because it was written twice
 * and the copies drifted — the conclave copy restored a position belonging to a
 * different conclave, so opening a meeting landed the reader in the middle of
 * its history instead of on the newest message.
 */

/** How close to the end still counts as "reading the newest message". */
export const AT_BOTTOM_SLACK = 48;

/** The three scroll fields the pane contract reads and writes. */
export interface ScrollBox {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

/** The scroll state of the pane being replaced, keyed by what it was showing. */
export interface PaneScrollState {
  /**
   * The conversation or conclave the old DOM was showing, or null when it was
   * showing none. A position only means something inside one conversation, so
   * this is what decides whether `top` may be restored at all.
   */
  key: string | null;
  /** Whether the reader was pinned to the newest message. */
  atBottom: boolean;
  top: number;
}

/** What the fresh pane should do once it has layout. */
export type PaneScrollTarget =
  | { mode: "bottom" }
  | { mode: "restore"; top: number };

/** Is this pane scrolled to (or within a nudge of) its newest message? */
export function isAtBottom(el: ScrollBox): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < AT_BOTTOM_SLACK;
}

/**
 * Decide where the rebuilt pane opens.
 *
 * `key` is what the pane being built shows. A position captured from a
 * different key is discarded rather than restored: opening a conversation or a
 * conclave always lands on the newest message. Within the same key, a reader
 * who was already at the bottom follows the newest message, and a reader away
 * in history keeps their place.
 *
 * `forceBottom` is the window coming back to view. That is a fresh read, so the
 * newest message wins even over a reader who had scrolled away.
 */
export function paneScrollTarget(
  prev: PaneScrollState | null,
  key: string,
  forceBottom = false,
): PaneScrollTarget {
  if (forceBottom || !prev || prev.key !== key || prev.atBottom) {
    return { mode: "bottom" };
  }
  return { mode: "restore", top: prev.top };
}

/**
 * Apply a decided target to the live pane, once it is in the document.
 *
 * `restore` is clamped to the new height: the pane may have shrunk since the
 * position was captured, and an unclamped `scrollTop` is silently clipped by
 * the browser anyway — clamping here keeps the intent legible.
 */
export function applyPaneScroll(el: ScrollBox | null, target: PaneScrollTarget | null): void {
  if (!el || !target) return;
  el.scrollTop =
    target.mode === "bottom" ? el.scrollHeight : Math.min(target.top, el.scrollHeight);
}

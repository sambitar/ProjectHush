/**
 * The one scroll question the message panes still ask.
 *
 * This module used to own a whole carry protocol — read the position off the
 * outgoing pane, decide the target while the new one was still detached, apply
 * it once it had layout — because both message panes were thrown away and
 * rebuilt on every render, and a detached element's `scrollTop` silently clamps
 * to zero.
 *
 * The panes are no longer rebuilt: the shell is a set of keyed regions and the
 * scroll boxes are left in place (see `regions.ts`). Nothing has to be carried,
 * so all that remains is the rule the carry existed to serve: follow the newest
 * message only when the reader was already looking at it. Which end that is
 * depends on the pane: the two chat panes list oldest first, so the newest
 * message is at the bottom, and the mail reader lists newest first, so it is at
 * the top.
 */

/** How close to the newest message still counts as "reading it". */
export const AT_BOTTOM_SLACK = 48;

/** The three scroll fields the rule reads. */
export interface ScrollBox {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

/** Is this pane scrolled to (or within a nudge of) its newest message? */
export function isAtBottom(el: ScrollBox): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < AT_BOTTOM_SLACK;
}

/**
 * Is this pane showing its newest message, in a pane that lists newest first?
 *
 * The mirror of `isAtBottom`, and the same nudge: a mail thread puts its newest
 * message at the top, so the reader is on it when the box is at its top. The
 * slack is what stops a pane that has settled a few pixels below the top from
 * reading as "the reader has scrolled away" and quietly stopping.
 */
export function isAtTop(el: ScrollBox): boolean {
  return el.scrollTop < AT_BOTTOM_SLACK;
}

/**
 * Pin a message pane to its newest message.
 *
 * Written twice on purpose. The rows carry `content-visibility: auto`, so the
 * ones below the fold hold an estimated height until they are brought into
 * view — and the first assignment is what brings the last of them into view.
 * The second read is then the real bottom, so the pane does not settle a few
 * pixels short of it, which is the state the next arrival would read as "the
 * reader has scrolled away" and stop following.
 */
export function pinToBottom(el: ScrollBox): void {
  el.scrollTop = el.scrollHeight;
  el.scrollTop = el.scrollHeight;
}

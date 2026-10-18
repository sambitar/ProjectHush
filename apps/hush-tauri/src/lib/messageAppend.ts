import { dayLabel } from "./messageGroups.ts";

/**
 * Bringing a message pane up to date without rebuilding it.
 *
 * Three panes grow at one end while the reader is looking at the other: the 1:1
 * thread (`.chat-messages`) and the conclave chat (`.conclave-messages`) list
 * oldest first and take arrivals at the tail, and the mail reader
 * (`.mail-messages`) lists newest first and takes them at the head. All three
 * used to be thrown away and rebuilt whenever the poll saw a change, which is
 * what made a background arrival cost the reader their hover state, their text
 * selection, their scroll position, and (through the containment rule) the
 * remembered row heights of a long history.
 *
 * The decision of *what* to append lives here so the panes cannot drift, and so
 * it can be tested without a DOM: which messages the pane is missing, whether
 * the first of them continues the run already on screen, and whether the day
 * heading has to be repeated.
 *
 * The run rule mirrors the first paint in `renderMainChat` /
 * `renderConclaveDetail` exactly: a run is a stretch of messages with the same
 * direction, the same sender, and the same day, and only its last bubble carries
 * the tail. The day heading is about the day, not the run — a new sender on the
 * same day starts a group without a new heading. The mail reader has neither:
 * one message is one card, and the date is a header field on the card rather
 * than a separator above it, so it takes the plain plan and nothing else.
 */

/** The fields the plan reads off a message. Either pane's message fits. */
export interface AppendableMessage {
  id: string;
  outgoing: boolean;
  createdAt: number;
  /**
   * The sender's nick. Present in a multi-party pane (a conclave, where more
   * than one voice is "not me"); absent for this vault's own messages and for
   * the 1:1 thread, which has only two sides.
   */
  from?: string;
}

/** What makes two messages one run: direction, sender, and day. */
interface RunKey {
  outgoing: boolean;
  from: string;
  day: string;
}

function runKey(m: AppendableMessage): RunKey {
  return { outgoing: m.outgoing, from: m.from ?? "", day: dayLabel(m.createdAt) };
}

/** What the caller has to do to bring a pane up to date. */
export interface AppendPlan<T extends AppendableMessage> {
  /** The messages the pane is missing, in order. Empty when it is current. */
  fresh: T[];
  /**
   * True when `fresh[0]` continues the run already on screen: the bubbles go
   * into the last rendered group, whose last bubble loses its tail.
   */
  continuesRun: boolean;
  /**
   * The day heading to insert before the fresh messages, or null when the day
   * has not changed (including when the pane is empty and the heading is the
   * pane's first).
   */
  daySep: string | null;
}

/**
 * Do the pane's drawn ids match the store's list exactly, starting at `offset`?
 *
 * This is the whole of "the pane is in step": what it shows has to be one end of
 * the store's list, with nothing inserted, pruned or reordered in between. A
 * pane that disagrees with the store must never be patched into looking current
 * — a stale message the store no longer holds, or one the pane has skipped past,
 * is a lie that survives until the next rebuild.
 */
function matchesAt(
  renderedIds: readonly string[],
  messages: readonly { id: string }[],
  offset: number,
): boolean {
  if (offset < 0) return false;
  if (offset + renderedIds.length > messages.length) return false;
  for (let i = 0; i < renderedIds.length; i += 1) {
    if (renderedIds[i] !== messages[offset + i].id) return false;
  }
  return true;
}

/**
 * Decide what to append to a pane that already shows `renderedIds`, in order,
 * and lists oldest first.
 *
 * Returns null when the pane is out of step: `renderedIds` is not a prefix of
 * `messages`. That is a reload, a prune, or a switch to another conversation —
 * appending would paint a pane that contradicts the store, so the caller
 * rebuilds instead. A stale pane must never be patched into looking current.
 */
export function planAppend<T extends AppendableMessage>(
  renderedIds: readonly string[],
  messages: readonly T[],
): AppendPlan<T> | null {
  if (!matchesAt(renderedIds, messages, 0)) return null;
  const fresh = messages.slice(renderedIds.length);
  if (!fresh.length) return { fresh, continuesRun: false, daySep: null };
  const last = renderedIds.length ? runKey(messages[renderedIds.length - 1]) : null;
  const first = runKey(fresh[0]);
  return {
    fresh,
    continuesRun:
      last !== null &&
      last.outgoing === first.outgoing &&
      last.from === first.from &&
      last.day === first.day,
    daySep: last !== null && last.day === first.day ? null : first.day,
  };
}

/**
 * Decide what to prepend to a pane that already shows `renderedIds`, in order,
 * and lists newest first.
 *
 * Returns null on the same terms as `planAppend`, with the end swapped:
 * `renderedIds` has to be a suffix of `messages`. The returned messages are in
 * the pane's own order — newest first — so the caller draws them as it draws the
 * rest and puts the whole batch in front of the pane.
 *
 * Only the messages are returned, with no run and no day to consider: the mail
 * reader draws each message as its own card and carries the date in the card's
 * header, so there is no group to join and no separator to insert. That is also
 * why this is not `planAppend` with a flag — the two return different things,
 * and a caller that has to ignore half of its answer is a caller that will
 * eventually use the wrong half.
 */
export function planPrepend<T extends { id: string }>(
  renderedIds: readonly string[],
  messages: readonly T[],
): T[] | null {
  const start = messages.length - renderedIds.length;
  if (!matchesAt(renderedIds, messages, start)) return null;
  return messages.slice(0, start);
}

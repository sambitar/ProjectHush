/**
 * What the 1:1 composer draws, decided once.
 *
 * The box is the one place a repaint may not interrupt: a draft, its caret, its
 * undo history and its grown height all live in the element. So presence is
 * *patched* rather than keyed, and that only works if the patch and the builder
 * cannot drift apart — which is why every state-dependent string and flag is
 * decided here and read by both.
 *
 * The split follows what is structural. A mail-only trust has no box at all, it
 * has the reason and the way out instead (Task 24), and that is a key. A contact
 * being offline changes a hint, a placeholder and three disabled flags inside a
 * box that stays, and that is a patch.
 *
 * Pure and importless: the caller passes the name to show, so this stays a leaf
 * module the test runner can load (see `regionKeys.ts` for the same reason).
 */

export type ComposerState = "mail_only" | "online" | "offline";

/** The state the box is in: mail-only, or a live box with or without a peer. */
export function composerState(online: boolean, canChat: boolean): ComposerState {
  if (!canChat) return "mail_only";
  return online ? "online" : "offline";
}

/**
 * The line above the box, or null when there is nothing to explain.
 *
 * Always rendered, and hidden when there is nothing to say: a hint that appears
 * and disappears is a structure change, and a structure change is what detaches
 * the box mid-sentence.
 */
export function composerHint(
  shownName: string,
  state: ComposerState,
): string | null {
  if (state === "mail_only") {
    return `${shownName} is mail only — chat and conclaves are off for this trust. Use Mail, or change this in their menu in Trusts.`;
  }
  if (state === "offline") {
    return `${shownName} is offline. Chat and files need you both online — use Mail to reach them later.`;
  }
  return null;
}

/** The placeholder inside the box itself. */
export function composerPlaceholder(state: ComposerState): string {
  return state === "online" ? "Message" : "Waiting for them to come online…";
}

/**
 * Whether the box takes input at all.
 *
 * One answer for the textarea, the send button and the attach button, because
 * an offline peer must not be offered half a composer: all three were drawn from
 * the same flag and are patched from this one.
 */
export function composerEnabled(state: ComposerState): boolean {
  return state === "online";
}

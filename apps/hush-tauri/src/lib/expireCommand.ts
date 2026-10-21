/**
 * The `/expire` command, decided without a DOM.
 *
 * The composer hands every submitted line through this: a line that starts with
 * `/expire` is not sent as text, it opens the expiry dialog. The parsing and the
 * bounds are here so the composer, the dialog's Enter key and the test runner
 * all read the same rule.
 */

/** The shortest lifetime the dialog offers. */
export const MIN_EXPIRE_SECONDS = 5;

/** The longest lifetime the dialog offers: 24 hours. */
export const MAX_EXPIRE_SECONDS = 24 * 60 * 60;

/** What the dialog starts with. */
export const DEFAULT_EXPIRE_SECONDS = 30;

export interface ExpireCommand {
  /** The text after `/expire`, trimmed; empty when the command has no body. */
  body: string;
}

/**
 * Parse a submitted line as `/expire <body>`.
 *
 * Returns null for ordinary text, including `/expired` and a command that is not
 * at the start of the line. A bare `/expire` is a command with an empty body so
 * the caller can explain what is missing rather than sending the literal slash.
 */
export function parseExpireCommand(text: string): ExpireCommand | null {
  const trimmed = text.trimStart();
  const match = /^\/expire(?:\s|$)/i.exec(trimmed);
  if (!match) return null;
  return { body: trimmed.slice(match[0].length).trim() };
}

/**
 * The seconds a user typed, or null when it is not a whole number in range.
 *
 * `1e2`, `1.5`, `0`, and a value past a day are all refused; the dialog shows
 * the range instead of guessing at what was meant.
 */
export function parseExpireSeconds(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const seconds = Number(trimmed);
  if (!Number.isSafeInteger(seconds)) return null;
  if (seconds < MIN_EXPIRE_SECONDS || seconds > MAX_EXPIRE_SECONDS) return null;
  return seconds;
}

/** The range sentence the dialog shows under its field. */
export function expireRangeHint(): string {
  return `${MIN_EXPIRE_SECONDS} seconds to 24 hours. The message is deleted from both sides and never saved in either vault.`;
}

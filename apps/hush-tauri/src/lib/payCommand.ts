/**
 * The `/pay` command, decided without a DOM.
 *
 * The composer hands every submitted line through this: a line that starts
 * with `/pay` is not sent as text, it opens the USDC payment dialog. The
 * parsing lives here so the composer, the dialog and the test runner read the
 * same rule. The amount is optional and deliberately kept as typed; the
 * wallet's `parseUnits` is the one place that decides whether it is sendable.
 */

export interface PayCommand {
  /** The text after `/pay`, trimmed; empty when the command has no amount. */
  amount: string;
}

/**
 * Parse a submitted line as `/pay [amount]`.
 *
 * Returns null for ordinary text, including `/payment` and a command that is
 * not at the start of the line. A bare `/pay` is a command with an empty
 * amount so the dialog can open at its first step and ask for one.
 */
export function parsePayCommand(text: string): PayCommand | null {
  const trimmed = text.trimStart();
  const match = /^\/pay(?:\s|$)/i.exec(trimmed);
  if (!match) return null;
  return { amount: trimmed.slice(match[0].length).trim() };
}

/**
 * The `/torrent` command, decided without a DOM.
 *
 * `/torrent` opens the picker; the argument is deliberately not a torrent id.
 * Choosing is a local act with a list, so the command carries no destination
 * and the daemon resolves the hash from its own state.
 */

export interface TorrentCommand {
  /** Anything typed after `/torrent`, trimmed. Reserved for a future filter. */
  arg: string;
}

export function parseTorrentCommand(text: string): TorrentCommand | null {
  const trimmed = text.trimStart();
  const match = /^\/torrent(?:\s|$)/i.exec(trimmed);
  if (!match) return null;
  return { arg: trimmed.slice(match[0].length).trim() };
}

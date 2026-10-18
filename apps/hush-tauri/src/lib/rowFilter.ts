/**
 * Narrowing an already-drawn list, without redrawing it.
 *
 * The Trusts and Conclaves filters have always worked this way: the pane draws
 * every row, and a keystroke hides the ones that do not match. The four search
 * fields that narrow a list — Chats, Mail, Drops, and the torrents library —
 * re-rendered the whole shell on every keystroke instead. That is what made
 * them drop the caret after one character: the render replaced the very field
 * the keystroke came from, so focus fell to `body` and the second character
 * went nowhere. They now use the same rule, so a keystroke touches only rows.
 *
 * A filterable row carries its haystack in `data-filter-text`, already
 * lowercased, stamped by whichever builder draws it. It is deliberately not the
 * row's visible text: a mail row matches on the plain-text form of a rich body
 * that the row shows only a truncated preview of, so a row cannot be matched
 * against what it renders.
 */

/**
 * The haystack a filterable row matches on: the parts, lowercased, joined.
 *
 * `toLowerCase`, not `toLocaleLowerCase`: the haystack and the query have to
 * agree, and a locale-aware fold can move a character *away* from what the user
 * typed (in Turkish `"I"` folds to `"ı"`, so a query of `"i"` would stop
 * matching "Inbox"). Case-insensitive here means case-insensitive, not
 * locale-aware.
 *
 * The parts are joined with a newline, which is the separator a single-line
 * `<input>` cannot contain — the browser strips newlines from what is typed and
 * pasted — so no query can match across the seam between two parts.
 */
export function filterText(
  ...parts: readonly (string | null | undefined)[]
): string {
  return parts.filter((p) => !!p).join("\n").toLowerCase();
}

/**
 * Hide every row under `container` that does not match `query`; return how many
 * it kept.
 *
 * Rows are hidden, never removed, so a row that stops matching comes back with
 * its scroll position, its listeners, and its place in the order intact. The
 * caller uses the count to decide whether its "nothing matched" note belongs on
 * screen — this function does not know about that note, because the panes
 * disagree about when it applies (a mail folder with no mail at all is a
 * different sentence from a search that matched none).
 *
 * An empty query matches everything, which is what lets the first paint and
 * every keystroke after it make the same call.
 */
export function applyRowFilter(container: ParentNode, query: string): number {
  const needle = query.trim().toLowerCase();
  let shown = 0;
  for (const row of container.querySelectorAll<HTMLElement>("[data-filter-text]")) {
    const match = !needle || (row.dataset.filterText ?? "").includes(needle);
    row.hidden = !match;
    if (match) shown += 1;
  }
  return shown;
}

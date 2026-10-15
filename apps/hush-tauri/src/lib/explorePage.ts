/**
 * Discovery's pure rules.
 *
 * `app.ts` paints the Explore dialog; what the follow list should *become* after
 * a click is a decision, and it lives here so it can be tested without a DOM.
 *
 * The rule that matters is that a follow is keyed by `did`. That is not a detail:
 * it is what lets a member with no account follow anything at all, because a
 * `did` needs no resolver and a handle does. Matching on a handle would also
 * follow the wrong account the moment one was renamed.
 */
import type { AtprotoExploreEntryDto, AtprotoFollowDto } from "./api/types.ts";

/** One follow as the daemon accepts it. */
export interface FollowDraft {
  did?: string;
  handle?: string;
  name?: string;
}

/** Whether this vault already reads a publication, keyed by `did`. */
export function isFollowing(follows: AtprotoFollowDto[], did: string): boolean {
  return follows.some((f) => f.did === did);
}

/**
 * The follow list after following or unfollowing one publication.
 *
 * Two things it is careful about. Every existing follow is carried through as a
 * `did` and a `name` only — the daemon resolves a handle against the connected
 * account's PDS, and re-sending handles would make a list that reads fine with
 * no account suddenly depend on one. And unfollowing removes only the matching
 * `did`: a publication the crawl did not happen to find again stays read,
 * because the directory is not the authority on what the member chose.
 *
 * The list is returned whole because the lane is curated rather than subscribed.
 * The daemon has no add-one route, and giving it one would let the stored list
 * drift from the member's.
 */
export function toggleFollow(
  follows: AtprotoFollowDto[],
  entry: AtprotoExploreEntryDto,
): FollowDraft[] {
  const kept = follows.filter((f) => f.did !== entry.did);
  const wasFollowing = kept.length !== follows.length;
  const drafts = kept.map((f) => ({ did: f.did, name: f.name }));
  if (wasFollowing) return drafts;
  return [...drafts, { did: entry.did, name: entry.name }];
}

/** A run of the directory under the seed it was reached through. */
export interface ExploreGroup {
  via: string;
  entries: AtprotoExploreEntryDto[];
}

/**
 * The directory, narrowed by a search term and grouped by the seed each entry
 * came from.
 *
 * Grouped rather than annotated, because the provenance has two possible values
 * across sixty-six rows: printing `via leaflet.pub` on every one is sixty-six
 * repetitions of the same fact, and a reader cannot see the shape of it. Stated
 * once per group, it answers the question they actually have — what did this
 * seed lead me to — and it is still said, so the list never reads as a survey of
 * the network when it is one hop from two accounts.
 *
 * The search term matches name, handle, URL and description, because a reader
 * looking for "books" does not know or care which field the word is in.
 */
export function groupDirectory(
  entries: AtprotoExploreEntryDto[],
  filter: string,
): ExploreGroup[] {
  const wanted = filter.trim().toLowerCase();
  const kept = wanted
    ? entries.filter((entry) =>
        `${entry.name} ${entry.handle} ${entry.url} ${entry.description}`
          .toLowerCase()
          .includes(wanted),
      )
    : entries;

  const groups = new Map<string, AtprotoExploreEntryDto[]>();
  for (const entry of kept) {
    const via = entry.via.trim() || "elsewhere";
    const run = groups.get(via);
    if (run) run.push(entry);
    else groups.set(via, [entry]);
  }
  return [...groups.entries()]
    .map(([via, entries]) => ({ via, entries }))
    // Biggest neighbourhood first, then by name: the group a reader is most
    // likely to find something in is the one they should meet first, and the
    // order must not depend on which entry the daemon happened to sort first.
    .sort((a, b) => b.entries.length - a.entries.length || a.via.localeCompare(b.via));
}

/**
 * How a directory row's second line reads.
 *
 * One line, because a description is the author's own text and runs from five
 * characters to three hundred: unclamped, the rows swing fourfold in height and
 * a list of sixty-six becomes unreadable. The address is dropped when it is
 * already the name's own host, so a row does not say the same thing twice.
 */
export function rowSubtitle(entry: AtprotoExploreEntryDto): string {
  const where = (entry.url || entry.handle).trim();
  const description = entry.description.replace(/\s+/g, " ").trim();
  if (!where) return description;
  return description ? `${where} · ${description}` : where;
}

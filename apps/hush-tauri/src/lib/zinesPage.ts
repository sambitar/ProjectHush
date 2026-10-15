/**
 * The Zines front page's composition rules.
 *
 * The page is a newspaper front page: a masthead, a strip of the corpus's own
 * sections, one lead story, then everything else under section headings.
 *
 * The corpus is **mixed**: the members' private zines, and — when the reader has
 * turned the public lane on — public documents read from the open social web
 * (AT Protocol, the Standard.site lexicons). Which story leads, how the strip is
 * ordered, where a story is filed and what its byline says are the same
 * questions for either lane, so they are answered once, here, over a normalized
 * {@link FrontPageStory}. The page then asks the lane only one thing: whether to
 * print the `public post` badge.
 *
 * `app.ts` only paints what these return.
 */
import type { AtprotoDocumentDto, ZineSummaryDto } from "./api/types.ts";

/** Which lane a story came from. */
export type StorySource = "zine" | "post";

/** The badge every public post carries, on its card and on its own page. */
export const PUBLIC_POST_BADGE = "public post";

/** The badge a zine carries once its record is also public.
 *
 *  A zine is the members' paper by default, and this is the one way one of them
 *  can also be readable by anyone. The reader is about to read something that
 *  left the membership, so the card says so — the same reason a public post is
 *  badged. */
export const PUBLIC_ZINE_BADGE = "also public";

/**
 * A story on the front page, normalized from either lane.
 *
 * Normalizing at the boundary is what keeps the composition rules single: a
 * public post and a private zine differ in where they come from and in what the
 * page must say about them, not in how a newspaper is laid out. The original
 * record is carried along so the page can print the lane-specific parts — a
 * zine's revision and comment count, a post's publish date — without the
 * composition rules having to know about them.
 */
export interface FrontPageStory {
  source: StorySource;
  /** The stable id: a zine id, or a document's `at://` address. */
  id: string;
  title: string;
  /** Normalised section tags. */
  sections: string[];
  /** The byline line as printed. */
  byline: string;
  /**
   * Local mute. No wire effect: the story is still held and still counted.
   *
   * A public post cannot be muted yet — the lane holds no per-account mute list
   * — so it is always false. The field is here because the rule that reads it
   * (the lead, the `Muted posts` control) is the same for both lanes.
   */
  muted: boolean;
  /** Unix milliseconds, for the newest-first order. */
  created_ms: number;
  /** The private record, when this is a zine. */
  zine: ZineSummaryDto | null;
  /** The public record, when this is a post. */
  post: AtprotoDocumentDto | null;
}

/** One entry in the section strip: a section, and how many stories carry it. */
export interface ZinesSectionCount {
  name: string;
  count: number;
}

/** A run of stories under one heading. `name` is null for the unsectioned. */
export interface ZinesGroup {
  name: string | null;
  stories: FrontPageStory[];
}

/** A private zine as a front-page story. */
export function zineStory(zine: ZineSummaryDto): FrontPageStory {
  return {
    source: "zine",
    id: zine.zine_id,
    title: zine.title,
    sections: zine.sections,
    byline: zine.pen_name,
    muted: zine.muted,
    // A zine's clock is its last edit: that is when the reader last had
    // something new to read.
    created_ms: zine.updated_ms,
    zine,
    post: null,
  };
}

/** A public document as a front-page story. */
export function postStory(post: AtprotoDocumentDto): FrontPageStory {
  return {
    source: "post",
    id: post.uri,
    title: post.title,
    sections: post.tags,
    byline: publicByline(post.handle.trim() || post.did),
    muted: false,
    // A post's clock is when its author published it. The daemon parsed the
    // author's own RFC3339 string into seconds at ingest, because the raw string
    // is not sortable: the lexicon allows an offset and optional fractions.
    created_ms: post.published_secs * 1000,
    zine: null,
    post,
  };
}

/**
 * How a public post's author is named.
 *
 * The lane offers a handle when the member followed one, and a DID when they
 * pasted one instead: a record carries neither, so the name comes from the
 * follow. There is no display name to offer — Standard.site has no profile
 * record — so the page names what it actually holds, shortened, rather than
 * inventing a name for a stranger.
 */
export function publicByline(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "";
  return trimmed.length <= 12 ? trimmed : `${trimmed.slice(0, 8)}…`;
}

/**
 * The mixed corpus, newest first.
 *
 * Ties break on id so the order is stable rather than dependent on which lane
 * happened to be concatenated first.
 */
export function newestFirst(stories: FrontPageStory[]): FrontPageStory[] {
  return [...stories].sort(
    (a, b) => b.created_ms - a.created_ms || a.id.localeCompare(b.id),
  );
}

/**
 * The story the page prints big: a private zine when any is visible, and the
 * newest unmuted one within that lane.
 *
 * The front page is the members' paper first, so a public post leads only when
 * the visible set holds no zine at all. Within the preferred lane muting still
 * means "stop competing for attention", and the lead is the loudest place on the
 * page, so a muted story leads only when there is nothing else — a page needs a
 * headline rather than a gap above the runs.
 */
export function leadOf(stories: FrontPageStory[]): FrontPageStory | null {
  const zines = stories.filter((s) => s.source === "zine");
  const pool = zines.length ? zines : stories;
  return pool.find((s) => !s.muted) ?? pool[0] ?? null;
}

/**
 * The badge a story carries, or null for a zine that is still only private.
 *
 * A private zine is the page's default and says nothing about itself. A public
 * post is the exception on the page and says so everywhere it appears, because
 * the reader is about to read something that left the membership. A zine that
 * has been published is the other exception: its body is readable by anyone, so
 * the card says that rather than letting the reader assume otherwise.
 */
export function badgeOf(story: FrontPageStory): string | null {
  if (story.source === "post") return PUBLIC_POST_BADGE;
  return story.zine?.public ? PUBLIC_ZINE_BADGE : null;
}

/**
 * The section strip, derived from the stories the page would actually show
 * rather than from the daemon's index.
 *
 * The daemon's own facet counts the whole corpus; these counts follow the
 * pane's view controls. That difference is the point: a control that hides
 * stories must not leave a strip entry promising stories that are not there.
 *
 * Ordered by count then name — the same rule the daemon's facet uses, so the
 * strip does not reshuffle depending on which side computed it.
 */
export function sectionCounts(stories: FrontPageStory[]): ZinesSectionCount[] {
  const counts = new Map<string, number>();
  for (const story of stories) {
    for (const section of story.sections) {
      counts.set(section, (counts.get(section) ?? 0) + 1);
    }
  }
  return [...counts.entries()]
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

/**
 * The stories under their sections, in the strip's order, with the unsectioned
 * last.
 *
 * A story appears exactly once. A story carrying several sections is filed
 * under the first of them that `order` lists, so a double-tagged story is not
 * printed twice, and which heading it lands under does not depend on the order
 * the author typed the tags in. A story whose sections the strip does not list
 * — including one with no sections at all — still belongs on the page, so it
 * goes under `null`, which the page prints as "Unsorted".
 */
export function groupBySection(
  stories: FrontPageStory[],
  order: string[],
): ZinesGroup[] {
  const buckets = new Map<string | null, FrontPageStory[]>();
  for (const story of stories) {
    const name = order.find((s) => story.sections.includes(s)) ?? null;
    const run = buckets.get(name);
    if (run) run.push(story);
    else buckets.set(name, [story]);
  }

  const groups: ZinesGroup[] = [];
  for (const name of order) {
    const run = buckets.get(name);
    if (run) groups.push({ name, stories: run });
  }
  const unsorted = buckets.get(null);
  if (unsorted) groups.push({ name: null, stories: unsorted });
  return groups;
}

/**
 * What a story's byline line says, for a story with no byline at all.
 *
 * A held zine whose blob has not been opened has no byline yet, and an empty
 * line where a name goes reads as a rendering bug rather than as "not known".
 */
export function bylineOf(story: FrontPageStory): string {
  return story.byline.trim() || "unknown byline";
}

/**
 * The stories signed by one byline.
 *
 * Matched case-insensitively and in full, because this is a reader clicking the
 * name they just read: a substring match would quietly widen the page to
 * strangers whose names merely contain it. A byline is not an identity — it is
 * free text, and two pen names may share one — so the page says "by this
 * byline" rather than claiming to know an author.
 *
 * A null `pen` is no filter, so the caller does not need a branch.
 */
export function byByline(
  stories: FrontPageStory[],
  pen: string | null,
): FrontPageStory[] {
  if (!pen) return stories;
  const wanted = pen.trim().toLowerCase();
  if (!wanted) return stories;
  return stories.filter((story) => bylineOf(story).toLowerCase() === wanted);
}

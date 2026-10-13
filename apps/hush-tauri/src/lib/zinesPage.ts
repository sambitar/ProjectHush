/**
 * The Zines front page's composition rules.
 *
 * The page is a newspaper front page: a masthead, a strip of the corpus's own
 * sections, one lead story, then everything else under section headings. Which
 * story leads, how the strip is ordered, and where a story with no section goes
 * are rules rather than markup, so they live here where they can be tested
 * without a DOM. `app.ts` only paints what these return.
 */
import type { ZineSummaryDto } from "./api/types.ts";

/** One entry in the section strip: a section, and how many stories carry it. */
export interface ZinesSectionCount {
  name: string;
  count: number;
}

/** A run of stories under one heading. `name` is null for the unsectioned. */
export interface ZinesGroup {
  name: string | null;
  stories: ZineSummaryDto[];
}

/**
 * The story the page prints big: the newest one the reader has not muted.
 *
 * Muting means "stop competing for attention", and the lead is the loudest
 * place on the page, so a muted story leads only when there is nothing else —
 * a page still needs a headline rather than a gap above the runs.
 */
export function leadOf(stories: ZineSummaryDto[]): ZineSummaryDto | null {
  return stories.find((z) => !z.muted) ?? stories[0] ?? null;
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
export function sectionCounts(stories: ZineSummaryDto[]): ZinesSectionCount[] {
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
  stories: ZineSummaryDto[],
  order: string[],
): ZinesGroup[] {
  const buckets = new Map<string | null, ZineSummaryDto[]>();
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
export function bylineOf(story: ZineSummaryDto): string {
  return story.pen_name.trim() || "unknown byline";
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
  stories: ZineSummaryDto[],
  pen: string | null,
): ZineSummaryDto[] {
  if (!pen) return stories;
  const wanted = pen.trim().toLowerCase();
  if (!wanted) return stories;
  return stories.filter((story) => bylineOf(story).toLowerCase() === wanted);
}

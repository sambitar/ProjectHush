import { test } from "node:test";
import assert from "node:assert/strict";
import type { ZineSummaryDto } from "./api/types.ts";
import { byByline, bylineOf, groupBySection, leadOf, sectionCounts } from "./zinesPage.ts";

/**
 * Unit tests for the Zines front page's composition rules.
 *
 * Run with `npm test` — Node's own runner, type-stripping the TypeScript, the
 * same way the Paper wire helpers are tested. What is covered is the part a
 * reader would notice as a bug but a type check cannot see: a story filed under
 * two headings, a story filed under none, and a strip whose counts disagree
 * with the page.
 */

/** A story, with only the fields the composition rules read. */
function story(
  id: string,
  sections: string[],
  pen = "ada",
): ZineSummaryDto {
  return {
    zine_id: id,
    pen_name: pen,
    title: id,
    sections,
    created_ms: 0,
    updated_ms: 0,
    revision: 1,
    comments: 0,
    pending: 0,
    preview: "",
    authored: false,
    muted: false,
    unreadable: false,
  };
}

test("the strip counts every section a story carries", () => {
  const counts = sectionCounts([
    story("a", ["rust", "systems"]),
    story("b", ["rust"]),
  ]);
  assert.deepEqual(counts, [
    { name: "rust", count: 2 },
    { name: "systems", count: 1 },
  ]);
});

test("the strip is ordered by count, then by name", () => {
  // Equal counts are broken by name so the strip cannot reshuffle between
  // renders of the same corpus.
  const counts = sectionCounts([
    story("a", ["zeta", "alpha"]),
    story("b", ["beta"]),
  ]);
  assert.deepEqual(counts, [
    { name: "alpha", count: 1 },
    { name: "beta", count: 1 },
    { name: "zeta", count: 1 },
  ]);
});

test("the strip says nothing about stories with no sections", () => {
  assert.deepEqual(sectionCounts([story("a", [])]), []);
});

test("a story is filed under one heading, the first the strip lists", () => {
  const groups = groupBySection(
    [story("a", ["systems", "rust"])],
    ["rust", "systems"],
  );
  assert.deepEqual(groups, [{ name: "rust", stories: [story("a", ["systems", "rust"])] }]);
});

test("headings follow the strip, and the unsectioned come last", () => {
  const loose = story("loose", []);
  const groups = groupBySection(
    [story("b", ["systems"]), loose, story("a", ["rust"])],
    ["rust", "systems"],
  );
  assert.deepEqual(
    groups.map((g) => g.name),
    ["rust", "systems", null],
  );
  assert.deepEqual(groups[2].stories, [loose]);
});

test("a story the strip cannot place is unsorted rather than dropped", () => {
  const groups = groupBySection([story("a", ["nowhere"])], ["rust"]);
  assert.deepEqual(groups, [{ name: null, stories: [story("a", ["nowhere"])] }]);
});

test("the lead is the newest story that is not muted", () => {
  const newest = { ...story("newest", ["rust"]), muted: true };
  const older = story("older", ["rust"]);
  assert.equal(leadOf([newest, older])?.zine_id, "older");
});

test("a page of nothing but muted stories still leads with the newest", () => {
  const newest = { ...story("newest", []), muted: true };
  const older = { ...story("older", []), muted: true };
  assert.equal(leadOf([newest, older])?.zine_id, "newest");
});

test("an empty corpus has no lead", () => {
  assert.equal(leadOf([]), null);
});

test("grouping keeps every story exactly once", () => {
  const stories = [
    story("a", ["rust"]),
    story("b", ["rust", "systems"]),
    story("c", []),
    story("d", ["systems"]),
  ];
  const grouped = groupBySection(stories, sectionCounts(stories).map((s) => s.name));
  assert.equal(grouped.flatMap((g) => g.stories).length, stories.length);
  assert.deepEqual(
    grouped.flatMap((g) => g.stories.map((s) => s.zine_id)).sort(),
    ["a", "b", "c", "d"],
  );
});

test("a byline filter keeps only that byline, ignoring case", () => {
  const stories = [story("a", [], "Ada"), story("b", [], "Bo"), story("c", [], "ada")];
  assert.deepEqual(
    byByline(stories, "ada").map((s) => s.zine_id),
    ["a", "c"],
  );
});

test("a byline filter is whole-name, not a substring", () => {
  // The daemon's `pen` query matches a substring; the page must not, because a
  // reader clicking "ada" is asking for ada rather than for everyone whose name
  // happens to contain it.
  const stories = [story("a", [], "ada"), story("b", [], "adalovelace")];
  assert.deepEqual(
    byByline(stories, "ada").map((s) => s.zine_id),
    ["a"],
  );
});

test("no byline filter is every story", () => {
  const stories = [story("a", [], "ada"), story("b", [], "bo")];
  assert.equal(byByline(stories, null).length, 2);
  assert.equal(byByline(stories, "   ").length, 2);
});

test("a story with no byline yet is matched as the unknown byline", () => {
  // An unopened blob has no byline, and its card prints one so the line does not
  // start with a separator. The filter has to agree with what the card says.
  const stories = [story("a", [], "")];
  assert.equal(bylineOf(stories[0]), "unknown byline");
  assert.deepEqual(
    byByline(stories, "unknown byline").map((s) => s.zine_id),
    ["a"],
  );
});

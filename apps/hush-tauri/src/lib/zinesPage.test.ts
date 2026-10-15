import { test } from "node:test";
import assert from "node:assert/strict";
import type { AtprotoDocumentDto, ZineSummaryDto } from "./api/types.ts";
import type { FrontPageStory } from "./zinesPage.ts";
import {
  badgeOf,
  byByline,
  bylineOf,
  groupBySection,
  leadOf,
  newestFirst,
  postStory,
  publicByline,
  sectionCounts,
  zineStory,
  PUBLIC_POST_BADGE,
  PUBLIC_ZINE_BADGE,
} from "./zinesPage.ts";

/**
 * Unit tests for the Zines front page's composition rules.
 *
 * Run with `npm test` — Node's own runner, type-stripping the TypeScript, the
 * same way the Paper wire helpers are tested. What is covered is the part a
 * reader would notice as a bug but a type check cannot see: a story filed under
 * two headings, a story filed under none, a strip whose counts disagree with the
 * page, and — now that the corpus is mixed — a public post leading the members'
 * paper.
 */

/** A private zine, with only the fields the composition rules read. */
function zine(
  id: string,
  sections: string[],
  pen = "ada",
  over: Partial<ZineSummaryDto> = {},
): FrontPageStory {
  return zineStory({
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
    public: null,
    ...over,
  });
}

/** A public document, `published_secs` the way the daemon parsed it. */
function post(
  uri: string,
  sections: string[],
  published_secs = 0,
): FrontPageStory {
  return postStory({
    uri,
    did: "did:plc:aa",
    handle: "ada.test",
    title: uri,
    tags: sections,
    published_at: "2026-01-01T00:00:00.000Z",
    published_secs,
    path: "/p",
    description: "",
    url: "",
    preview: "",
    site: "at://did:plc:aa/site.standard.publication/self",
  });
}

test("the strip counts every section a story carries", () => {
  const counts = sectionCounts([
    zine("a", ["rust", "systems"]),
    zine("b", ["rust"]),
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
    zine("a", ["zeta", "alpha"]),
    zine("b", ["beta"]),
  ]);
  assert.deepEqual(counts, [
    { name: "alpha", count: 1 },
    { name: "beta", count: 1 },
    { name: "zeta", count: 1 },
  ]);
});

test("the strip says nothing about stories with no sections", () => {
  assert.deepEqual(sectionCounts([zine("a", [])]), []);
});

test("the strip counts both lanes together", () => {
  // The switch filters before composition, so once a post is on the page its
  // sections are the page's sections like anyone else's.
  const counts = sectionCounts([
    zine("a", ["rust"]),
    post("at://did:plc:aa/site.standard.document/one", ["rust", "atproto"]),
  ]);
  assert.deepEqual(counts, [
    { name: "rust", count: 2 },
    { name: "atproto", count: 1 },
  ]);
});

test("a story is filed under one heading, the first the strip lists", () => {
  const groups = groupBySection([zine("a", ["systems", "rust"])], ["rust", "systems"]);
  assert.deepEqual(
    groups.map((g) => g.stories.map((s) => s.id)),
    [["a"]],
  );
  assert.equal(groups[0].name, "rust");
});

test("headings follow the strip, and the unsectioned come last", () => {
  const loose = zine("loose", []);
  const groups = groupBySection(
    [zine("b", ["systems"]), loose, zine("a", ["rust"])],
    ["rust", "systems"],
  );
  assert.deepEqual(
    groups.map((g) => g.name),
    ["rust", "systems", null],
  );
  assert.deepEqual(groups[2].stories.map((s) => s.id), ["loose"]);
});

test("a story the strip cannot place is unsorted rather than dropped", () => {
  const groups = groupBySection([zine("a", ["nowhere"])], ["rust"]);
  assert.deepEqual(groups.map((g) => g.name), [null]);
  assert.deepEqual(groups[0].stories.map((s) => s.id), ["a"]);
});

test("the lead is the newest story that is not muted", () => {
  const newest = zine("newest", ["rust"], "ada", { muted: true });
  const older = zine("older", ["rust"]);
  assert.equal(leadOf([newest, older])?.id, "older");
});

test("a page of nothing but muted stories still leads with the newest", () => {
  const newest = zine("newest", [], "ada", { muted: true });
  const older = zine("older", [], "ada", { muted: true });
  assert.equal(leadOf([newest, older])?.id, "newest");
});

test("an empty corpus has no lead", () => {
  assert.equal(leadOf([]), null);
});

test("a public post never leads while any private zine is visible", () => {
  // The front page is the members' paper first. The post here is both newer and
  // unmuted, so only the lane rule can keep it off the lead.
  const stories = [
    post("at://did:plc:aa/site.standard.document/newer", ["atproto"], 9_000),
    zine("older", ["rust"]),
  ];
  assert.equal(leadOf(stories)?.id, "older");
});

test("a public post leads when the visible set holds no private zine", () => {
  // Newest first, as the page hands them over. A post cannot be muted yet, so
  // the lead is simply the newest of the lane that is left.
  const stories = newestFirst([
    post("at://did:plc:aa/site.standard.document/older", [], 100),
    post("at://did:plc:aa/site.standard.document/newest", [], 200),
  ]);
  assert.equal(
    leadOf(stories)?.id,
    "at://did:plc:aa/site.standard.document/newest",
  );
});

test("the mixed corpus is newest first across both lanes", () => {
  // A zine's clock is milliseconds and a post's `published_secs` is seconds, so
  // this pins the conversion as much as the order.
  const stories = newestFirst([
    zine("older-zine", [], "ada", { updated_ms: 100_000 }),
    post("at://did:plc:aa/site.standard.document/newer", [], 200),
    post("at://did:plc:aa/site.standard.document/oldest", [], 50),
  ]);
  assert.deepEqual(
    stories.map((s) => s.id),
    [
      "at://did:plc:aa/site.standard.document/newer",
      "older-zine",
      "at://did:plc:aa/site.standard.document/oldest",
    ],
  );
});

test("a post's clock is its published time in milliseconds", () => {
  assert.equal(
    post("at://did:plc:aa/site.standard.document/one", [], 1_234).created_ms,
    1_234_000,
  );
});

test("a public post carries the badge and a private zine carries none", () => {
  assert.equal(
    badgeOf(post("at://did:plc:aa/site.standard.document/one", [])),
    PUBLIC_POST_BADGE,
  );
  assert.equal(badgeOf(zine("a", [])), null);
});

test("a zine that has been published says so, because its body is public", () => {
  // A private zine is the page's default and says nothing about itself. One
  // whose record is on a public network is no longer only that: the reader is
  // about to read something anyone can read.
  const published = zine("a", [], "ada", {
    public: {
      did: "did:plc:writer",
      uri: "at://did:plc:writer/site.standard.document/deadbeef",
      cid: "bafy",
      published_at_ms: 1_700_000_000_000,
      published_ms: 1_700_000_000_000,
    },
  });
  assert.equal(badgeOf(published), PUBLIC_ZINE_BADGE);
});

test("a public post is named by its handle, shortened", () => {
  assert.equal(publicByline("ada.test"), "ada.test");
  assert.equal(
    publicByline("a-very-long-handle.example"),
    "a-very-l…",
    "a long name is shortened rather than wrapped",
  );
  assert.equal(publicByline("  "), "");
  // A follow added by DID has no handle, so the page names the DID it holds
  // rather than inventing a name for a stranger.
  assert.equal(post("at://did:plc:aa/site.standard.document/one", []).byline, "ada.test");
});

test("grouping keeps every story exactly once", () => {
  const stories = [
    zine("a", ["rust"]),
    zine("b", ["rust", "systems"]),
    zine("c", []),
    post("at://did:plc:aa/site.standard.document/d", ["systems"]),
  ];
  const grouped = groupBySection(stories, sectionCounts(stories).map((s) => s.name));
  assert.equal(grouped.flatMap((g) => g.stories).length, stories.length);
  assert.deepEqual(
    grouped.flatMap((g) => g.stories.map((s) => s.id)).sort(),
    ["a", "at://did:plc:aa/site.standard.document/d", "b", "c"],
  );
});

test("a byline filter keeps only that byline, ignoring case", () => {
  const stories = [zine("a", [], "Ada"), zine("b", [], "Bo"), zine("c", [], "ada")];
  assert.deepEqual(
    byByline(stories, "ada").map((s) => s.id),
    ["a", "c"],
  );
});

test("a byline filter is whole-name, not a substring", () => {
  // The daemon's `pen` query matches a substring; the page must not, because a
  // reader clicking "ada" is asking for ada rather than for everyone whose name
  // happens to contain it.
  const stories = [zine("a", [], "ada"), zine("b", [], "adalovelace")];
  assert.deepEqual(
    byByline(stories, "ada").map((s) => s.id),
    ["a"],
  );
});

test("no byline filter is every story", () => {
  const stories = [zine("a", [], "ada"), zine("b", [], "bo")];
  assert.equal(byByline(stories, null).length, 2);
  assert.equal(byByline(stories, "   ").length, 2);
});

test("a story with no byline yet is matched as the unknown byline", () => {
  // An unopened blob has no byline, and its card prints one so the line does not
  // start with a separator. The filter has to agree with what the card says.
  const stories = [zine("a", [], "")];
  assert.equal(bylineOf(stories[0]), "unknown byline");
  assert.deepEqual(
    byByline(stories, "unknown byline").map((s) => s.id),
    ["a"],
  );
});

test("a byline filter narrows the mixed corpus to the lane it names", () => {
  // A pen name and a handle are both bylines, and clicking one must not drag
  // the other lane's stories along with it.
  const stories = [zine("a", [], "ada"), post("at://did:plc:aa/site.standard.document/b", [])];
  assert.deepEqual(
    byByline(stories, "ada").map((s) => s.id),
    ["a"],
  );
});

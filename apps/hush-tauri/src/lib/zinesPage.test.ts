import { test } from "node:test";
import assert from "node:assert/strict";
import type {
  NeighborhoodCommentDto,
  NeighborhoodSummaryDto,
  ZineSummaryDto,
} from "./api/types.ts";
import type { FrontPageStory } from "./zinesPage.ts";
import {
  badgeOf,
  byByline,
  bylineOf,
  groupBySection,
  leadOf,
  newestFirst,
  postStory,
  flattenThread,
  publicByline,
  publicThread,
  sectionCounts,
  threadSize,
  THREAD_MAX_DEPTH,
  zineStory,
  zineExportAddress,
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
    export: null,
    ...over,
  });
}

/** A public post, `created_at` in seconds the way a relay sends it. */
function post(address: string, sections: string[], created_at = 0): FrontPageStory {
  return postStory({
    address,
    event_id: `event-${address}`,
    pubkey: "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2",
    d: address,
    title: address,
    summary: null,
    sections,
    created_at,
    published_at: null,
    reading_minutes: 4,
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
    post("30023:aa:one", ["rust", "nostr"]),
  ]);
  assert.deepEqual(counts, [
    { name: "rust", count: 2 },
    { name: "nostr", count: 1 },
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
  const stories = [post("30023:aa:newer", ["nostr"], 9_000), zine("older", ["rust"])];
  assert.equal(leadOf(stories)?.id, "older");
});

test("a public post leads when the visible set holds no private zine", () => {
  // Newest first, as the page hands them over. A post cannot be muted yet, so
  // the lead is simply the newest of the lane that is left.
  const stories = newestFirst([
    post("30023:aa:older", [], 100),
    post("30023:aa:newest", [], 200),
  ]);
  assert.equal(leadOf(stories)?.id, "30023:aa:newest");
});

test("the mixed corpus is newest first across both lanes", () => {
  // A zine's clock is milliseconds and a post's `created_at` is seconds, so this
  // pins the conversion as much as the order.
  const stories = newestFirst([
    zine("older-zine", [], "ada", { updated_ms: 100_000 }),
    post("30023:aa:newer", [], 200),
    post("30023:aa:oldest", [], 50),
  ]);
  assert.deepEqual(
    stories.map((s) => s.id),
    ["30023:aa:newer", "older-zine", "30023:aa:oldest"],
  );
});

test("a post's clock is its created_at in milliseconds", () => {
  assert.equal(post("30023:aa:one", [], 1_234).created_ms, 1_234_000);
});

test("a public post carries the badge and a private zine carries none", () => {
  assert.equal(badgeOf(post("30023:aa:one", [])), PUBLIC_POST_BADGE);
  assert.equal(badgeOf(zine("a", [])), null);
});

test("a zine that has been exported says so, because its body is public", () => {
  // A private zine is the page's default and says nothing about itself. One
  // whose article is on a public network is no longer only that: the reader is
  // about to read something anyone can read.
  const exported = zine("a", [], "ada", {
    export: {
      d: "deadbeef",
      pubkey: "a1b2c3d4e5f6a7b8",
      published_at: 1_700_000_000,
      last_event_id: "f".repeat(64),
      published_ms: 1_700_000_000_000,
    },
  });
  assert.equal(badgeOf(exported), PUBLIC_ZINE_BADGE);
  // A zine whose export record has no event yet — minted, never published — is
  // still an export: the address exists and the next attempt uses it.
  const minted = zine("b", [], "ada", {
    export: {
      d: "feedface",
      pubkey: "a1b2c3d4e5f6a7b8",
      published_at: 1_700_000_000,
      last_event_id: "",
      published_ms: 0,
    },
  });
  assert.equal(badgeOf(minted), PUBLIC_ZINE_BADGE);
});

test("an exported zine's address is the NIP-01 `a` tag", () => {
  // The one place this format is composed for a zine, and it has to agree with
  // `ExportPlan::address` in the daemon: a comment references exactly this.
  assert.equal(
    zineExportAddress({ pubkey: "a1b2c3", d: "deadbeef" }),
    "30023:a1b2c3:deadbeef",
  );
  // A key with stray whitespace is the same key, as the daemon treats it.
  assert.equal(
    zineExportAddress({ pubkey: " a1b2c3 ", d: "d" }),
    "30023:a1b2c3:d",
  );
});

test("a public post is named by its key, shortened", () => {
  // There is no display name in the read lane yet, so the page names the key it
  // holds rather than inventing one for a stranger.
  assert.equal(publicByline("a1b2c3d4e5f6a7b8"), "a1b2c3d4…");
  assert.equal(publicByline("short"), "short");
  assert.equal(publicByline("  "), "");
  assert.equal(post("30023:aa:one", []).byline, "a1b2c3d4…");
});

test("grouping keeps every story exactly once", () => {
  const stories = [
    zine("a", ["rust"]),
    zine("b", ["rust", "systems"]),
    zine("c", []),
    post("30023:aa:d", ["systems"]),
  ];
  const grouped = groupBySection(stories, sectionCounts(stories).map((s) => s.name));
  assert.equal(grouped.flatMap((g) => g.stories).length, stories.length);
  assert.deepEqual(
    grouped.flatMap((g) => g.stories.map((s) => s.id)).sort(),
    ["30023:aa:d", "a", "b", "c"],
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
  // A pen name and a shortened key are both bylines, and clicking one must not
  // drag the other lane's stories along with it.
  const stories = [zine("a", [], "ada"), post("30023:aa:b", [])];
  assert.deepEqual(
    byByline(stories, "ada").map((s) => s.id),
    ["a"],
  );
});

/** A comment as the daemon serves one. */
function comment(
  id: string,
  created_at: number,
  parent_id: string | null = null,
): NeighborhoodCommentDto {
  return {
    event_id: id,
    pubkey: "bb".repeat(32),
    content: `body ${id}`,
    created_at,
    parent_id,
  };
}

test("a flat thread becomes a tree, oldest first at every level", () => {
  const nodes = publicThread([
    comment("c3", 30, "c1"),
    comment("c1", 10),
    comment("c2", 20, "c1"),
    comment("c4", 40),
  ]);
  assert.deepEqual(
    nodes.map((n) => n.comment.event_id),
    ["c1", "c4"],
    "the roots are in time order",
  );
  assert.deepEqual(
    nodes[0].children.map((n) => n.comment.event_id),
    ["c2", "c3"],
    "and so is each level under it",
  );
  assert.equal(threadSize(nodes), 4, "nothing is dropped in the walk");
});

test("a reply whose parent is not held is shown, not lost", () => {
  // The parent may have been evicted by the bounded cache, or may not have
  // arrived yet. The reply's words are real either way.
  const nodes = publicThread([comment("c1", 10), comment("orphan", 20, "gone")]);
  assert.deepEqual(
    nodes.map((n) => n.comment.event_id),
    ["c1", "orphan"],
  );
  assert.equal(nodes[1].depth, 0);
  assert.equal(threadSize(nodes), 2);
});

test("depth is capped so a deep thread stays on the page", () => {
  const nodes = publicThread([
    comment("a", 1),
    comment("b", 2, "a"),
    comment("c", 3, "b"),
    comment("d", 4, "c"),
    comment("e", 5, "d"),
  ]);
  const depths: number[] = [];
  const walk = (list: ReturnType<typeof publicThread>): void => {
    for (const node of list) {
      depths.push(node.depth);
      walk(node.children);
    }
  };
  walk(nodes);
  assert.deepEqual(depths, [0, 1, 2, 2, 2]);
  assert.ok(depths.every((d) => d <= THREAD_MAX_DEPTH));
});

test("a cycle in the parents is shown as a top-level comment, not a hang", () => {
  // No honest relay produces this, but a hand-written event can, and the page
  // must not recurse forever building it.
  const nodes = publicThread([
    comment("a", 1, "b"),
    comment("b", 2, "a"),
  ]);
  assert.equal(threadSize(nodes), 2, "both comments are still shown");
  const flat: string[] = [];
  const walk = (list: ReturnType<typeof publicThread>): void => {
    for (const node of list) {
      flat.push(node.comment.event_id);
      walk(node.children);
    }
  };
  walk(nodes);
  assert.equal(new Set(flat).size, 2, "and exactly once each");
});

test("an empty thread is an empty tree", () => {
  assert.deepEqual(publicThread([]), []);
  assert.equal(threadSize([]), 0);
  assert.deepEqual(flattenThread([]), []);
});

test("a thread flattens in reading order with the depth it was drawn at", () => {
  // The page draws one row per comment and indents by `depth`, so the flat order
  // and the tree have to agree about both the sequence and the level.
  const nodes = publicThread([
    comment("c1", 10),
    comment("c2", 20, "c1"),
    comment("c3", 30, "c2"),
    comment("c4", 40),
  ]);
  const flat = flattenThread(nodes);
  assert.deepEqual(
    flat.map((n) => n.comment.event_id),
    ["c1", "c2", "c3", "c4"],
    "a reply is drawn under what it answers, before the next root",
  );
  assert.deepEqual(
    flat.map((n) => n.depth),
    [0, 1, 2, 0],
  );
  assert.equal(flat.length, threadSize(nodes), "flattening loses nothing");
});

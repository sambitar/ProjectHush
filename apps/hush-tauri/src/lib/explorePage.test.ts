import { test } from "node:test";
import assert from "node:assert/strict";
import { groupDirectory, isFollowing, rowSubtitle, toggleFollow } from "./explorePage.ts";
import type { AtprotoExploreEntryDto, AtprotoFollowDto } from "./api/types.ts";

function follow(did: string, name: string, handle = ""): AtprotoFollowDto {
  return { did, handle, name };
}

function entry(did: string, name: string): AtprotoExploreEntryDto {
  return {
    uri: `at://${did}/site.standard.publication/self`,
    did,
    handle: `${did}.test`,
    name,
    description: "",
    url: "",
    has_icon: false,
    via: "leaflet.pub",
  };
}

test("a follow is matched on the did, not the handle", () => {
  const follows = [follow("did:plc:abc", "Ada", "ada.test")];
  assert.equal(isFollowing(follows, "did:plc:abc"), true);
  // The handle moved and now belongs to someone else. Following by it would
  // silently swap who is read, which is why nothing here matches on one.
  assert.equal(isFollowing(follows, "ada.test"), false);
  assert.equal(isFollowing(follows, "did:plc:other"), false);
});

test("following adds a publication that was not followed", () => {
  const out = toggleFollow([], entry("did:plc:abc", "Ada Writes"));
  assert.deepEqual(out, [{ did: "did:plc:abc", name: "Ada Writes" }]);
});

test("following the same row twice unfollows rather than duplicating", () => {
  const once = toggleFollow([], entry("did:plc:abc", "Ada"));
  const twice = toggleFollow(
    once.map((f) => follow(f.did!, f.name!, f.handle ?? "")),
    entry("did:plc:abc", "Ada"),
  );
  assert.deepEqual(twice, []);
});

test("unfollowing removes only the matching publication", () => {
  const follows = [follow("did:plc:abc", "Ada Writes"), follow("did:plc:zed", "Zed")];
  const out = toggleFollow(follows, entry("did:plc:abc", "Ada Writes"));
  assert.deepEqual(out, [{ did: "did:plc:zed", name: "Zed" }]);
});

test("the payload carries only a did and a name, so the list needs no resolver", () => {
  // A handle in the payload is resolved against the connected account's PDS.
  // Carrying one through would make a list that reads fine with no account
  // suddenly require one.
  const follows = [follow("did:plc:abc", "Ada", "ada.test")];
  const out = toggleFollow(follows, entry("did:plc:zed", "Zed"));
  assert.deepEqual(out, [
    { did: "did:plc:abc", name: "Ada" },
    { did: "did:plc:zed", name: "Zed" },
  ]);
  for (const draft of out) {
    assert.equal("handle" in draft, false);
    assert.ok(draft.did);
  }
});

test("follows the directory did not find again are kept", () => {
  // The crawl is one hop from two seeds. A publication it missed this time is
  // still one the member chose, and dropping it would quietly unsubscribe them
  // from something they asked for.
  const follows = [follow("did:plc:gone", "Not In The Directory")];
  const out = toggleFollow(follows, entry("did:plc:abc", "Ada"));
  assert.deepEqual(
    out.map((f) => f.did),
    ["did:plc:gone", "did:plc:abc"],
  );
});

test("the list it was given is never mutated", () => {
  const follows = [follow("did:plc:abc", "Ada")];
  toggleFollow(follows, entry("did:plc:zed", "Zed"));
  assert.equal(follows.length, 1);
});

function pub(
  did: string,
  name: string,
  via: string,
  extra: Partial<AtprotoExploreEntryDto> = {},
): AtprotoExploreEntryDto {
  return { ...entry(did, name), via, ...extra };
}

test("the directory is grouped by the seed each entry came from", () => {
  // Grouped rather than annotated: two possible values across sixty-six rows
  // means a per-row label is the same fact printed sixty-six times, and the
  // reader never sees the shape of it.
  const groups = groupDirectory(
    [
      pub("did:plc:a", "Ada", "leaflet.pub"),
      pub("did:plc:b", "Bo", "pckt.blog"),
      pub("did:plc:c", "Cy", "leaflet.pub"),
    ],
    "",
  );
  assert.deepEqual(
    groups.map((g) => [g.via, g.entries.length]),
    [
      ["leaflet.pub", 2],
      ["pckt.blog", 1],
    ],
  );
});

test("the biggest neighbourhood is met first", () => {
  const groups = groupDirectory(
    [
      pub("did:plc:a", "Ada", "pckt.blog"),
      pub("did:plc:b", "Bo", "leaflet.pub"),
      pub("did:plc:c", "Cy", "leaflet.pub"),
    ],
    "",
  );
  assert.deepEqual(
    groups.map((g) => g.via),
    ["leaflet.pub", "pckt.blog"],
  );
});

test("an entry with no seed is still listed, not dropped", () => {
  const groups = groupDirectory([pub("did:plc:a", "Ada", "  ")], "");
  assert.deepEqual(
    groups.map((g) => g.via),
    ["elsewhere"],
  );
});

test("the filter matches name, handle, url and description", () => {
  const entries = [
    pub("did:plc:a", "Ada Writes", "leaflet.pub", { description: "about books" }),
    pub("did:plc:b", "Bo Notes", "pckt.blog", { url: "https://bo.example" }),
  ];
  const byName = groupDirectory(entries, "ada");
  assert.deepEqual(byName.flatMap((g) => g.entries.map((e) => e.name)), ["Ada Writes"]);
  // A reader looking for "books" does not know or care which field the word is
  // in, so every field is searched.
  const byDesc = groupDirectory(entries, "books");
  assert.deepEqual(byDesc.flatMap((g) => g.entries.map((e) => e.name)), ["Ada Writes"]);
  const byUrl = groupDirectory(entries, "bo.example");
  assert.deepEqual(byUrl.flatMap((g) => g.entries.map((e) => e.name)), ["Bo Notes"]);
  const byHandle = groupDirectory(entries, "DID:PLC:B");
  assert.deepEqual(byHandle.flatMap((g) => g.entries.map((e) => e.name)), ["Bo Notes"]);
});

test("a filter that matches nothing yields no groups rather than a crash", () => {
  assert.deepEqual(groupDirectory([pub("did:plc:a", "Ada", "leaflet.pub")], "zzz"), []);
});

test("a row's subtitle puts the address and the description on one line", () => {
  const both = rowSubtitle(
    pub("did:plc:a", "Ada", "leaflet.pub", {
      url: "https://ada.example",
      description: "a\n  blog   about books",
    }),
  );
  // Newlines and runs of spaces collapse: a description is free text and would
  // otherwise break the fixed-height row the list depends on.
  assert.equal(both, "https://ada.example · a blog about books");
});

test("a row with no address is just its description", () => {
  const only = rowSubtitle(
    pub("did:plc:a", "Ada", "leaflet.pub", { url: "", handle: "", description: "just words" }),
  );
  assert.equal(only, "just words");
  const none = rowSubtitle(pub("did:plc:a", "Ada", "leaflet.pub", { url: "", handle: "" }));
  assert.equal(none, "");
});

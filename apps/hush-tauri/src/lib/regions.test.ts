import { test } from "node:test";
import assert from "node:assert/strict";
import {
  canUpdateInPlace,
  planModalSync,
  planRegionSync,
  shapeMoved,
  updateRegionInPlace,
  type ModalOp,
  type ModalSlot,
  type RegionOp,
} from "./regions.ts";

const keep = (live: number): RegionOp => ({ kind: "keep", live });
const replace = (live: number | null): RegionOp => ({ kind: "replace", live });
const drop = (live: number): RegionOp => ({ kind: "drop", live });

// --- The cases the shell actually hits ---

test("an unchanged keyed region is kept, not rebuilt", () => {
  assert.deepEqual(
    planRegionSync(["rail", "chat:ana"], ["rail", "chat:ana"]),
    [keep(0), keep(1)],
  );
});

test("a null-key region is rebuilt every time, even against a null", () => {
  assert.deepEqual(planRegionSync([null], [null]), [replace(0)]);
});

test("switching the open conversation replaces the pane, never moves it", () => {
  assert.deepEqual(
    planRegionSync(["rail", "chat:ana"], ["rail", "chat:bo"]),
    [keep(0), replace(1)],
  );
});

test("switching rails drops the panes the new rail does not have", () => {
  // Chats -> Mail: the conv list and the chat pane go, the mail panes arrive.
  assert.deepEqual(
    planRegionSync(["rail", "convList", "chat:ana"], ["rail", null, null]),
    [keep(0), replace(1), replace(2)],
  );
});

test("the Mail list is kept behind the two columns that are rebuilt", () => {
  // The Mail rail's shape: an unkeyed rail, an unkeyed folder column, and the
  // folder's list keyed behind them. A mail reload rebuilds the first two and
  // must leave the third — the search field and the scroll box are in it.
  assert.deepEqual(
    planRegionSync(["rail", null, "mailList:inbox"], ["rail", null, "mailList:inbox"]),
    [keep(0), replace(1), keep(2)],
  );
});

test("changing folders replaces the Mail list rather than redrawing it", () => {
  assert.deepEqual(
    planRegionSync(["rail", null, "mailList:inbox"], ["rail", null, "mailList:sent"]),
    [keep(0), replace(1), replace(2)],
  );
});

test("a rail with more panes than the last one inserts the extras", () => {
  // Chats -> Reads: reads has three columns where chats had two.
  assert.deepEqual(
    planRegionSync(["rail", "convList", "chat:ana"], ["rail", null, null, null]),
    [keep(0), replace(1), replace(2), replace(null)],
  );
});

test("a rail with fewer panes than the last one drops the leftovers", () => {
  // Reads -> Torrents: three columns collapse to one.
  assert.deepEqual(
    planRegionSync(["rail", null, null, null], ["rail", null]),
    [keep(0), replace(1), drop(2), drop(3)],
  );
});

test("an empty parent takes every region as an insertion", () => {
  assert.deepEqual(
    planRegionSync([], ["rail", "convList", "chat:ana"]),
    [replace(null), replace(null), replace(null)],
  );
});

test("a key that moved is rebuilt, not relocated", () => {
  // Reordering would mean detaching the element, which is the one thing the
  // sync exists to avoid; a reorder means the pane was rebuilt anyway.
  assert.deepEqual(
    planRegionSync(["chat:ana", "rail"], ["rail", "chat:ana"]),
    [replace(0), replace(1)],
  );
});

test("a keyed region that disappears is dropped", () => {
  assert.deepEqual(
    planRegionSync(["rail", "chat:ana", "banner"], ["rail", "banner"]),
    [keep(0), replace(1), drop(2)],
  );
});

test("every live child is consumed exactly once", () => {
  const live = ["rail", "convList", "chat:ana", "banner"];
  const want = ["rail", null, "chat:ana"];
  const ops = planRegionSync(live, want);
  const touched = ops
    .filter((op) => op.kind !== "replace" || op.live !== null)
    .map((op) => (op as { live: number }).live);
  assert.deepEqual([...touched].sort((a, b) => a - b), [0, 1, 2, 3]);
});

// --- Which regions can be patched without a full sync ---

// --- Patching one region where it stands ---

test("a delta reaches its own region and leaves its neighbours alone", () => {
  const rail = fakeEl("rail");
  const list = fakeEl("mailList:inbox");
  let updates = 0;
  const painted = updateRegionInPlace(fakeShell(rail, list), {
    key: "mailList:inbox",
    build: () => null as never,
    update: () => {
      updates += 1;
    },
  });
  assert.equal(painted, true);
  assert.equal(updates, 1);
  assert.equal(rail.dataset.regionShape, undefined);
});

test("a region whose shape has not moved is not redrawn", () => {
  const list = fakeEl("mailList:inbox", "inbox\nnothing-new");
  let updates = 0;
  const painted = updateRegionInPlace(fakeShell(list), {
    key: "mailList:inbox",
    shape: () => "inbox\nnothing-new",
    build: () => null as never,
    update: () => {
      updates += 1;
    },
  });
  assert.equal(painted, true);
  assert.equal(updates, 0);
});

test("a moved shape is stamped before the update runs", () => {
  // The stamp describes the state the element is being drawn from, and it is
  // written first, so an update that reaches back into the executor cannot see
  // the shape it is replacing.
  const list = fakeEl("mailList:inbox", "inbox\nold");
  let drawnWhenUpdated: string | undefined;
  updateRegionInPlace(fakeShell(list), {
    key: "mailList:inbox",
    shape: () => "inbox\nnew",
    build: () => null as never,
    update: (el) => {
      drawnWhenUpdated = el.dataset.regionShape;
    },
  });
  assert.equal(drawnWhenUpdated, "inbox\nnew");
});

test("a region with no shape is patched every time it is asked for", () => {
  const list = fakeEl("mailList:inbox", "inbox\nstale-stamp");
  let updates = 0;
  updateRegionInPlace(fakeShell(list), {
    key: "mailList:inbox",
    build: () => null as never,
    update: () => {
      updates += 1;
    },
  });
  assert.equal(updates, 1);
});

test("a region under a wrapper is not found, and the caller rebuilds", () => {
  // The shell holds the regions, so a parent one level up — the root the shell
  // hangs in — holds none. Reading that as "patched" is what leaves a delta
  // unpainted, so it is false, which is the rebuild's cue.
  const wrapper: FakeEl = { dataset: {} };
  const shell = fakeShell(wrapper);
  let updates = 0;
  const painted = updateRegionInPlace(shell, {
    key: "mailList:inbox",
    build: () => null as never,
    update: () => {
      updates += 1;
    },
  });
  assert.equal(painted, false);
  assert.equal(updates, 0);
});

test("an unkeyed or update-less region is not patchable in place", () => {
  const shell = fakeShell(fakeEl("mailList:inbox"));
  assert.equal(
    updateRegionInPlace(shell, {
      key: null,
      build: () => null as never,
      update: () => {},
    }),
    false,
  );
  assert.equal(
    updateRegionInPlace(shell, { key: "mailList:inbox", build: () => null as never }),
    false,
  );
});

// --- The dialog layer ---

const slot = (key: string, shape = "same"): ModalSlot => ({ key, shape });
const kept = (live: number): ModalOp => ({ kind: "keep", live });
const rebuilt = (live: number | null): ModalOp => ({ kind: "rebuild", live });
const dropped = (live: number): ModalOp => ({ kind: "drop", live });

test("an untouched dialog is not rebuilt at all", () => {
  // The whole point: a background tick that changes nothing a dialog draws
  // must not detach it, or the caret in its field goes with it.
  assert.deepEqual(
    planModalSync([slot("palette"), slot("addFriend")], [slot("palette"), slot("addFriend")]),
    [kept(0), kept(1)],
  );
});

test("closing an early dialog keeps the ones behind it", () => {
  // The case that rules out pairing by position: with the palette gone, the
  // dialog behind it has moved up one slot, and reading that as a different
  // dialog would rebuild a form the reader is typing into.
  assert.deepEqual(
    planModalSync([slot("palette"), slot("addFriend")], [slot("addFriend")]),
    [kept(1), dropped(0)],
  );
});

test("only the dialog whose drawn facts moved is rebuilt", () => {
  assert.deepEqual(
    planModalSync(
      [slot("palette", "a"), slot("profile", "old")],
      [slot("palette", "a"), slot("profile", "new")],
    ),
    [kept(0), rebuilt(1)],
  );
});

test("a dialog opening over an open one is inserted, not swapped", () => {
  assert.deepEqual(
    planModalSync([slot("palette")], [slot("palette"), slot("zinesShare")]),
    [kept(0), rebuilt(null)],
  );
});

test("a dialog opening between two open ones leaves both alone", () => {
  assert.deepEqual(
    planModalSync([slot("palette"), slot("zinesShare")], [
      slot("palette"),
      slot("addFriend"),
      slot("zinesShare"),
    ]),
    [kept(0), rebuilt(null), kept(1)],
  );
});

test("every dialog closing drops every one of them, in order", () => {
  assert.deepEqual(planModalSync([slot("palette"), slot("addFriend")], []), [
    dropped(0),
    dropped(1),
  ]);
});

test("the first dialog of a pass has nothing to rebuild from", () => {
  assert.deepEqual(planModalSync([], [slot("palette")]), [rebuilt(null)]);
});

test("a different dialog in the same slot is a rebuild, not a reuse", () => {
  // One closes and another opens between two passes: the element that is there
  // belongs to the dialog that left, so it is dropped and the new one is built.
  assert.deepEqual(planModalSync([slot("palette")], [slot("addFriend")]), [
    rebuilt(null),
    dropped(0),
  ]);
});

test("a dialog that would have to move is rebuilt rather than reordered", () => {
  // Moving an element detaches it, which is what the whole sync exists to
  // avoid. The list is written in one fixed order, so this cannot happen from
  // the app's own state; if it ever did, a rebuild is the honest answer.
  assert.deepEqual(
    planModalSync([slot("addFriend"), slot("palette")], [slot("palette"), slot("addFriend")]),
    [kept(1), rebuilt(0)],
  );
});

test("a duplicate key is taken once and the spare is dropped", () => {
  assert.deepEqual(planModalSync([slot("palette"), slot("palette")], [slot("palette")]), [
    kept(0),
    dropped(1),
  ]);
});

// --- Fakes ---

interface FakeEl {
  dataset: { regionKey?: string; regionShape?: string };
}

/**
 * The smallest DOM `findRegion` and the patch need: a `children` list to walk
 * and a `dataset` to read the key and the stamp from. What is *not* here is the
 * tree — the patch takes its parent as an argument, so a flat list of children
 * is the whole of the contract.
 */
function fakeShell(...els: FakeEl[]): HTMLElement {
  return { children: els } as unknown as HTMLElement;
}

function fakeEl(key: string, shape?: string): FakeEl {
  return { dataset: { regionKey: key, regionShape: shape } };
}

// --- Whether a kept region's update has anything to do ---

test("a region with no shape is updated on every render", () => {
  // The cheap path, and the default: a shape is opt-in for updates that cost
  // more than the rebuild they replace.
  assert.equal(shapeMoved(undefined, undefined), true);
  assert.equal(shapeMoved(undefined, "drawn"), true);
});

test("a region drawn from the same shape is left alone", () => {
  assert.equal(shapeMoved("a\nb", "a\nb"), false);
});

test("a region whose drawn facts moved is redrawn", () => {
  assert.equal(shapeMoved("a\nb", "a\nc"), true);
});

test("an element from before the shape existed is redrawn once", () => {
  // No stamp yet, and a shape that happens to be the empty string: the two are
  // different answers, and reading them as the same would skip the first draw.
  assert.equal(shapeMoved("", undefined), true);
  assert.equal(shapeMoved("", ""), false);
});

test("a keyed region with an update can be brought forward in place", () => {
  const spec = { key: "mailList:inbox", build: () => null as never, update: () => {} };
  assert.equal(canUpdateInPlace(spec), true);
});

test("an unkeyed region is never patchable, because a render rebuilds it", () => {
  // The mail reload's whole question: the compose form, the drafts list and the
  // drop bodies are unkeyed, so a reload that reached them has to rebuild
  // rather than patch, or they would go stale.
  assert.equal(
    canUpdateInPlace({ key: null, build: () => null as never, update: () => {} }),
    false,
  );
});

test("a keyed region with nothing to run is not patchable either", () => {
  // False is the safe answer: the caller rebuilds, which always paints. Saying
  // true here would leave whatever the region draws as it was.
  assert.equal(canUpdateInPlace({ key: "rail", build: () => null as never }), false);
});

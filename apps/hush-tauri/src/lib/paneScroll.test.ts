import { test } from "node:test";
import assert from "node:assert/strict";
import {
  applyPaneScroll,
  AT_BOTTOM_SLACK,
  isAtBottom,
  paneScrollTarget,
  type PaneScrollState,
  type ScrollBox,
} from "./paneScroll.ts";

const box = (scrollHeight: number, scrollTop: number, clientHeight: number): ScrollBox => ({
  scrollHeight,
  scrollTop,
  clientHeight,
});

const state = (key: string | null, atBottom: boolean, top: number): PaneScrollState => ({
  key,
  atBottom,
  top,
});

// --- At the bottom? ---

test("a pane with no overflow counts as at the bottom", () => {
  assert.equal(isAtBottom(box(400, 0, 400)), true);
});

test("a pane within the slack counts as at the bottom", () => {
  assert.equal(isAtBottom(box(1000, 1000 - 400 - (AT_BOTTOM_SLACK - 1), 400)), true);
});

test("a pane scrolled past the slack is away from the bottom", () => {
  assert.equal(isAtBottom(box(1000, 1000 - 400 - AT_BOTTOM_SLACK, 400)), false);
});

// --- Where the rebuilt pane opens ---

test("a first paint with no captured position opens on the newest message", () => {
  assert.deepEqual(paneScrollTarget(null, "rushd"), { mode: "bottom" });
});

test("a reader already at the bottom follows the newest message", () => {
  assert.deepEqual(paneScrollTarget(state("rushd", true, 900), "rushd"), { mode: "bottom" });
});

test("a reader away in history keeps their place in the same conversation", () => {
  assert.deepEqual(paneScrollTarget(state("rushd", false, 320), "rushd"), {
    mode: "restore",
    top: 320,
  });
});

test("a position from another conversation is discarded, not restored", () => {
  // The regression: opening a conclave used to inherit the previous one's
  // scrollTop, landing the reader mid-history instead of on the newest message.
  assert.deepEqual(paneScrollTarget(state("nadia", false, 640), "rushd"), { mode: "bottom" });
});

test("a position captured with no pane open is discarded", () => {
  assert.deepEqual(paneScrollTarget(state(null, false, 640), "rushd"), { mode: "bottom" });
});

test("returning to the window overrides a scrolled-away reader", () => {
  assert.deepEqual(paneScrollTarget(state("rushd", false, 320), "rushd", true), {
    mode: "bottom",
  });
});

// --- Applying it ---

test("bottom mode pins to the end of the rebuilt pane", () => {
  const el = box(2000, 0, 400);
  applyPaneScroll(el, { mode: "bottom" });
  assert.equal(el.scrollTop, 2000);
});

test("restore mode puts the reader back where they were", () => {
  const el = box(2000, 0, 400);
  applyPaneScroll(el, { mode: "restore", top: 640 });
  assert.equal(el.scrollTop, 640);
});

test("restore mode is clamped when the pane shrank below the old position", () => {
  const el = box(500, 0, 400);
  applyPaneScroll(el, { mode: "restore", top: 1800 });
  assert.equal(el.scrollTop, 500);
});

test("applying nothing to nothing is a no-op", () => {
  assert.doesNotThrow(() => applyPaneScroll(null, null));
  const el = box(500, 0, 400);
  applyPaneScroll(el, null);
  assert.equal(el.scrollTop, 0);
});

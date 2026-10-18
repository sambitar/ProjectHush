import { test } from "node:test";
import assert from "node:assert/strict";
import {
  AT_BOTTOM_SLACK,
  isAtBottom,
  isAtTop,
  pinToBottom,
  type ScrollBox,
} from "./paneScroll.ts";

const box = (scrollHeight: number, scrollTop: number, clientHeight: number): ScrollBox => ({
  scrollHeight,
  scrollTop,
  clientHeight,
});

// The rule the rebuilt-pane carry used to serve, kept because both message
// panes still apply it: follow the newest message only if the reader was
// already at the bottom.

test("a pane with no overflow counts as at the bottom", () => {
  assert.equal(isAtBottom(box(400, 0, 400)), true);
});

test("a pane within the slack counts as at the bottom", () => {
  assert.equal(isAtBottom(box(1000, 1000 - 400 - (AT_BOTTOM_SLACK - 1), 400)), true);
});

test("a pane scrolled past the slack is away from the bottom", () => {
  assert.equal(isAtBottom(box(1000, 1000 - 400 - AT_BOTTOM_SLACK, 400)), false);
});

test("a pane scrolled to the top of a long history is away from the bottom", () => {
  assert.equal(isAtBottom(box(20_000, 0, 400)), false);
});

// --- The same rule at the other end: a pane that lists newest first ---
// The mail reader puts its newest message at the top, so "the reader is on the
// newest message" is a question about the top edge, and the nudge is the same
// one: a pane that has settled a few pixels below the top is still following.

test("a pane with no overflow counts as at the top", () => {
  assert.equal(isAtTop(box(400, 0, 400)), true);
});

test("a pane within the slack of the top counts as at the top", () => {
  assert.equal(isAtTop(box(20_000, AT_BOTTOM_SLACK - 1, 400)), true);
});

test("a pane scrolled past the slack is away from the top", () => {
  assert.equal(isAtTop(box(20_000, AT_BOTTOM_SLACK, 400)), false);
});

test("a pane scrolled to the bottom of a long history is away from the top", () => {
  assert.equal(isAtTop(box(20_000, 19_600, 400)), false);
});

// --- Pinning to the bottom ---

test("pinning lands on the real bottom, not the estimated one", () => {
  // What `content-visibility` does: rows below the fold are estimated until
  // they are scrolled into view. What a scroll box does: clamp scrollTop to
  // the height it currently believes it has. Together, one pass settles at the
  // estimate's limit and the second pass lands on the truth.
  let height = 1000;
  let top = 0;
  const el: ScrollBox = {
    clientHeight: 400,
    get scrollHeight() {
      return height;
    },
    get scrollTop() {
      return top;
    },
    set scrollTop(v: number) {
      top = Math.min(v, height - 400);
      // Reaching the bottom is what brings the last rows into view.
      if (top >= height - 400) height = 1400;
    },
  };
  pinToBottom(el);
  assert.equal(el.scrollTop, 1000);
  assert.equal(isAtBottom(el), true);
});

test("settling at the estimated bottom is what stops a pane following", () => {
  // The failure the second pass avoids, as the arithmetic it is: pinned at the
  // estimate's limit with the real content taller, the slack is long gone.
  const el = box(1400, 600, 400);
  assert.equal(isAtBottom(el), false);
});

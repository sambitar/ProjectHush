import { test } from "node:test";
import assert from "node:assert/strict";
import { planAppend, planPrepend, type AppendableMessage } from "./messageAppend.ts";
import { dayLabel } from "./messageGroups.ts";

const NOW = Date.now();
const YESTERDAY = NOW - 24 * 60 * 60 * 1000;
const LAST_WEEK = NOW - 7 * 24 * 60 * 60 * 1000;

function msg(
  id: string,
  outgoing: boolean,
  createdAt = NOW,
  from?: string,
): AppendableMessage {
  return { id, outgoing, createdAt, from };
}

// --- A pane that is current, or empty ---

test("an empty pane takes the whole thread", () => {
  const thread = [msg("a", false), msg("b", false)];
  const plan = planAppend([], thread);
  assert.ok(plan);
  assert.deepEqual(plan.fresh, thread);
  assert.equal(plan.continuesRun, false);
  assert.equal(plan.daySep, dayLabel(NOW));
});

test("a pane already showing the whole thread plans nothing", () => {
  const thread = [msg("a", false), msg("b", true)];
  const plan = planAppend(["a", "b"], thread);
  assert.ok(plan);
  assert.deepEqual(plan.fresh, []);
  assert.equal(plan.continuesRun, false);
  assert.equal(plan.daySep, null);
});

// --- Runs ---

test("an arrival after the contact's own message continues their run", () => {
  const thread = [msg("a", false), msg("b", false)];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.deepEqual(
    plan.fresh.map((m) => m.id),
    ["b"],
  );
  assert.equal(plan.continuesRun, true);
  assert.equal(plan.daySep, null);
});

test("an arrival after our own message starts a new run, with no new heading", () => {
  const thread = [msg("a", true), msg("b", false)];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.equal(plan.continuesRun, false);
  // Same day: a new run is not a new day, so the heading is not repeated.
  assert.equal(plan.daySep, null);
});

test("an arrival on a later day repeats the heading", () => {
  const thread = [msg("a", false, YESTERDAY), msg("b", false, NOW)];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.equal(plan.continuesRun, false);
  assert.equal(plan.daySep, dayLabel(NOW));
});

test("a second sender in a conclave starts a new run on the same day", () => {
  const thread = [msg("a", false, NOW, "rushd"), msg("b", false, NOW, "nadia")];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.equal(plan.continuesRun, false);
  assert.equal(plan.daySep, null);
});

test("a repeat from the same trustee continues their run", () => {
  const thread = [msg("a", false, NOW, "rushd"), msg("b", false, NOW, "rushd")];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.equal(plan.continuesRun, true);
});

test("our own message after a trustee's starts a new run", () => {
  const thread = [msg("a", false, NOW, "rushd"), msg("b", true, NOW)];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.equal(plan.continuesRun, false);
});

test("a payment line never joins the run around it", () => {
  const payment = { ...msg("p", false), kind: "payment" };
  const thread = [msg("a", false), payment, msg("b", false)];
  // Neither the bubble before it nor the one after may absorb the system line.
  assert.equal(planAppend(["a"], thread)?.continuesRun, false);
  assert.equal(planAppend(["a", "p"], thread)?.continuesRun, false);
});

// --- Several arrivals at once ---

test("a burst is planned as one append, in order", () => {
  const thread = [msg("a", false), msg("b", false), msg("c", true)];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.deepEqual(
    plan.fresh.map((m) => m.id),
    ["b", "c"],
  );
  assert.equal(plan.continuesRun, true);
});

test("a burst that spans midnight repeats the heading once", () => {
  const thread = [msg("a", false, LAST_WEEK), msg("b", false, YESTERDAY)];
  const plan = planAppend(["a"], thread);
  assert.ok(plan);
  assert.equal(plan.daySep, dayLabel(YESTERDAY));
});

// --- Out of step: the caller must rebuild ---

test("a pane whose first row is not the store's first message is out of step", () => {
  // The regression this guards: after a reload the pane can show a window of
  // history that no longer lines up with the store. Appending would leave the
  // reader with a pane that silently contradicts the thread.
  const thread = [msg("x", false), msg("a", false), msg("b", false)];
  assert.equal(planAppend(["a", "b"], thread), null);
});

test("a pane showing a message the store has dropped is out of step", () => {
  const thread = [msg("b", false)];
  assert.equal(planAppend(["a", "b"], thread), null);
});

test("a pane whose ids are reordered is out of step", () => {
  const thread = [msg("a", false), msg("b", false)];
  assert.equal(planAppend(["b"], thread), null);
});

// --- A pane that lists newest first (the mail reader) ---

/** A thread as the mail store holds it: newest first. */
const newestFirst = (...ids: string[]): { id: string }[] => ids.map((id) => ({ id }));

test("an empty newest-first pane takes the whole thread, newest first", () => {
  const thread = newestFirst("c", "b", "a");
  assert.deepEqual(planPrepend([], thread), thread);
});

test("a newest-first pane already showing the thread plans nothing", () => {
  assert.deepEqual(planPrepend(["c", "b"], newestFirst("c", "b")), []);
});

test("an arrival at the head is the only message planned", () => {
  // What a reply looks like from the other end: the pane shows the two oldest,
  // and the one that arrived is the new head of the store's list.
  const thread = newestFirst("d", "c", "b", "a");
  assert.deepEqual(planPrepend(["c", "b", "a"], thread), [{ id: "d" }]);
});

test("a burst at the head is planned in the pane's own order", () => {
  // The caller draws them in the order it gets, so the plan has to be the pane's
  // order (newest first) rather than the order they arrived in.
  const thread = newestFirst("d", "c", "b", "a");
  const fresh = planPrepend(["b", "a"], thread);
  assert.ok(fresh);
  assert.deepEqual(
    fresh.map((m) => m.id),
    ["d", "c"],
  );
});

test("a newest-first pane whose drawn ids are not the thread's tail is out of step", () => {
  // The head moved, which for a thread that only ever grows at the head means
  // something was inserted, pruned, or reordered in the middle — a reply whose
  // clock is older than the newest, a message trashed, a contact removed.
  const thread = newestFirst("d", "c", "b", "a");
  assert.equal(planPrepend(["d", "b", "a"], thread), null);
  assert.equal(planPrepend(["b"], thread), null);
});

test("a newest-first pane showing more than the thread holds is out of step", () => {
  assert.equal(planPrepend(["c", "b", "a"], newestFirst("b", "a")), null);
});

test("a newest-first pane showing a message the store has dropped is out of step", () => {
  assert.equal(planPrepend(["d", "c"], newestFirst("c", "b")), null);
});

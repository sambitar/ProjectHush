import { test } from "node:test";
import assert from "node:assert/strict";
import { applyRowFilter, filterText } from "./rowFilter.ts";

// --- The haystack a row matches on ---

test("parts are joined so the query is matched against all of them", () => {
  assert.equal(filterText("Inbox", "Quarterly report", "the body"), "inbox\nquarterly report\nthe body");
});

test("an absent part contributes nothing", () => {
  assert.equal(filterText("inbox", undefined, "body"), "inbox\nbody");
  assert.equal(filterText(undefined, null, "body"), "body");
});

test("an empty part contributes nothing rather than an empty line", () => {
  // A message with no subject must not make the seam between sender and body
  // matchable: "x\n\nbody" would let a query of "x body" through.
  assert.equal(filterText("x", "", "body"), "x\nbody");
});

test("the haystack is lowercased, so the caller compares like with like", () => {
  assert.equal(filterText("INBOX"), "inbox");
});

test("a query cannot match across the seam between two parts", () => {
  // The reason the separator is a newline: a single-line input cannot contain
  // one, so "inboxbody" must not match a row whose parts are "inbox" and "body".
  const row = fakeRow(filterText("inbox", "body"));
  const container = fakeList(row);
  assert.equal(applyRowFilter(container, "inboxbody"), 0);
  assert.equal(row.hidden, true);
});

// --- Narrowing a drawn list ---

test("an empty query keeps every row, so the first paint needs no special case", () => {
  const rows = [fakeRow("alpha"), fakeRow("beta")];
  assert.equal(applyRowFilter(fakeList(...rows), ""), 2);
  assert.deepEqual(rows.map((r) => r.hidden), [false, false]);
});

test("a query keeps the rows that contain it and hides the rest", () => {
  const rows = [fakeRow("alice"), fakeRow("bob"), fakeRow("alicia")];
  assert.equal(applyRowFilter(fakeList(...rows), "ali"), 2);
  assert.deepEqual(rows.map((r) => r.hidden), [false, true, false]);
});

test("the match is case-insensitive on both sides", () => {
  const rows = [fakeRow("alice")];
  assert.equal(applyRowFilter(fakeList(...rows), "ALICE"), 1);
  assert.equal(applyRowFilter(fakeList(...rows), "al"), 1);
});

test("the query is trimmed, so a trailing space does not empty the list", () => {
  const rows = [fakeRow("alice")];
  assert.equal(applyRowFilter(fakeList(...rows), "  ali  "), 1);
});

test("a row hidden by one query comes back when the query is cleared", () => {
  // Hiding rather than removing is the whole point: the row keeps its place,
  // its listeners, and its scroll position while it is out of the list.
  const row = fakeRow("alice");
  const container = fakeList(row);
  applyRowFilter(container, "zzz");
  assert.equal(row.hidden, true);
  applyRowFilter(container, "");
  assert.equal(row.hidden, false);
});

test("the count is what was kept, not what was hidden", () => {
  const rows = [fakeRow("alice"), fakeRow("bob"), fakeRow("alicia")];
  assert.equal(applyRowFilter(fakeList(...rows), "bob"), 1);
  assert.equal(applyRowFilter(fakeList(...rows), "o"), 1);
  assert.equal(applyRowFilter(fakeList(...rows), "zzz"), 0);
});

// --- Fakes ---

interface FakeRow {
  dataset: { filterText?: string };
  hidden: boolean;
}

/**
 * The smallest DOM `applyRowFilter` needs: `querySelectorAll` for the rows, and
 * a `hidden` on each to set. The selector is the caller's side of the contract
 * — a row is found by the attribute its builder stamped — so it is not
 * re-implemented here; these stand in for a container whose rows all carry one.
 */
function fakeList(...rows: FakeRow[]): ParentNode {
  return { querySelectorAll: () => rows } as unknown as ParentNode;
}

function fakeRow(text: string): FakeRow {
  return { dataset: { filterText: text }, hidden: false };
}

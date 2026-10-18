import { test } from "node:test";
import assert from "node:assert/strict";
import {
  candidatesToInvite,
  clearConclaveStore,
  conclaveListShape,
  conclaveUnreadCount,
  conclaveUnreadTotal,
  groupConclaveMessages,
  invitableCandidates,
  invitesAreCurrent,
  loadConclaves,
  markConclaveRead,
  type Conclave,
  type ConclaveMessage,
  type PendingInvite,
} from "./conclaveStore.ts";
import type { ConclaveDto } from "./api/types.ts";

function conclaveOf(
  trustees: { nick: string; state: "invited" | "joined" | "left" | "kicked" }[],
): Conclave {
  return {
    id: "c1",
    name: "test conclave",
    convener: "me",
    trustees: trustees.map((t, i) => ({ accountId: `a${i}`, ...t })),
    createdAt: 0,
    messages: [],
    ended: false,
  };
}

const contact = (nickname: string, reach: "full" | "mail_only" = "full") => ({
  nickname,
  reach,
});

test("candidatesToInvite keeps only contacts outside the roster", () => {
  const conclave = conclaveOf([
    { nick: "alice", state: "joined" },
    { nick: "bob", state: "invited" },
  ]);
  assert.deepEqual(candidatesToInvite(conclave, ["alice", "bob", "carol"]), ["carol"]);
});

test("invitableCandidates agrees with the modal: mail-only contacts are not offered", () => {
  const conclave = conclaveOf([]);
  assert.deepEqual(
    invitableCandidates(conclave, [contact("alice"), contact("dave", "mail_only")]),
    ["alice"],
  );
});

test("invitableCandidates excludes a left member: terminal states are never re-invited", () => {
  const conclave = conclaveOf([{ nick: "mahmood", state: "left" }]);
  assert.deepEqual(invitableCandidates(conclave, [contact("mahmood")]), []);
});

test("invitableCandidates excludes kicked and joined trustees alike", () => {
  const conclave = conclaveOf([
    { nick: "alice", state: "joined" },
    { nick: "bob", state: "kicked" },
  ]);
  assert.deepEqual(invitableCandidates(conclave, [contact("alice"), contact("bob")]), []);
});

test("invitableCandidates keeps full-reach newcomers", () => {
  const conclave = conclaveOf([
    { nick: "alice", state: "joined" },
    { nick: "bob", state: "left" },
  ]);
  assert.deepEqual(invitableCandidates(conclave, [contact("alice"), contact("bob"), contact("carol")]), [
    "carol",
  ]);
});

// --- Message grouping ---

const msg = (
  id: string,
  outgoing: boolean,
  from: string | undefined,
  ts: number,
): ConclaveMessage => ({
  id,
  outgoing,
  from,
  body: id,
  createdAt: ts,
});

const shape = (groups: ReturnType<typeof groupConclaveMessages>) =>
  groups.map((g) => `${g.outgoing ? ">" : "<"}${g.from || "me"}:${g.messages.length}`);

test("a run from one sender is one group, so the avatar and name appear once", () => {
  const now = Date.now();
  const groups = groupConclaveMessages([
    msg("a", false, "rushd", now),
    msg("b", false, "rushd", now + 1000),
    msg("c", false, "rushd", now + 2000),
  ]);
  assert.deepEqual(shape(groups), ["<rushd:3"]);
});

test("a different sender breaks the run even with no gap in time", () => {
  const now = Date.now();
  const groups = groupConclaveMessages([
    msg("a", false, "rushd", now),
    msg("b", false, "nadia", now + 1),
    msg("c", false, "rushd", now + 2),
  ]);
  assert.deepEqual(shape(groups), ["<rushd:1", "<nadia:1", "<rushd:1"]);
});

test("my own reply breaks the run", () => {
  const now = Date.now();
  const groups = groupConclaveMessages([
    msg("a", false, "rushd", now),
    msg("b", true, undefined, now + 1),
    msg("c", false, "rushd", now + 2),
  ]);
  assert.deepEqual(shape(groups), ["<rushd:1", ">me:1", "<rushd:1"]);
});

test("a new day breaks the run, so the day separator has a place to sit", () => {
  const yesterday = Date.now() - 24 * 60 * 60 * 1000;
  const now = Date.now();
  const groups = groupConclaveMessages([
    msg("a", false, "rushd", yesterday),
    msg("b", false, "rushd", now),
  ]);
  assert.deepEqual(shape(groups), ["<rushd:1", "<rushd:1"]);
  assert.notEqual(groups[0].day, groups[1].day);
});

test("grouping keeps every message exactly once, in order", () => {
  const now = Date.now();
  const groups = groupConclaveMessages([
    msg("a", false, "rushd", now),
    msg("b", true, undefined, now + 1),
    msg("c", false, "rushd", now + 2),
    msg("d", false, "rushd", now + 3),
  ]);
  assert.deepEqual(
    groups.flatMap((g) => g.messages.map((m) => m.id)),
    ["a", "b", "c", "d"],
  );
});

test("an empty log has no groups", () => {
  assert.deepEqual(groupConclaveMessages([]), []);
});

// --- Unread counts ---

const dto = (id: string, messageCount: number): ConclaveDto => ({
  id,
  name: `conclave ${id}`,
  convener: "me",
  trustees: [{ account_id: "a0", nick: "me", state: "joined" }],
  created_ms: 0,
  ended: false,
  messages: Array.from({ length: messageCount }, (_, i) => ({
    id: `${id}-m${i}`,
    outgoing: false,
    from: "alice",
    body: `hello ${i}`,
    ts_ms: i,
  })),
});

test("unread accumulates for background conclaves and clears on selection", () => {
  clearConclaveStore();
  // First load is a fresh app: history is read, only later growth counts.
  assert.equal(loadConclaves([dto("c1", 3), dto("c2", 0)]).changed, true);
  assert.equal(conclaveUnreadTotal(), 0);

  // Growth on a background conclave accumulates; the read one stays clean.
  loadConclaves([dto("c1", 3), dto("c2", 2)], "c1");
  assert.equal(conclaveUnreadCount("c1"), 0);
  assert.equal(conclaveUnreadCount("c2"), 2);
  assert.equal(conclaveUnreadTotal(), 2);

  // Selecting the conclave is the read.
  markConclaveRead("c2");
  assert.equal(conclaveUnreadTotal(), 0);
  clearConclaveStore();
});

test("unread counts several arriving batches and drops with a vanished conclave", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 0)]);
  loadConclaves([dto("c1", 2)], "c1");
  assert.equal(conclaveUnreadCount("c1"), 0, "c1 is the open pane");
  loadConclaves([dto("c1", 5)], "other");
  assert.equal(conclaveUnreadCount("c1"), 3, "5 - 2 background growth");
  loadConclaves([dto("c1", 6)], "other");
  assert.equal(conclaveUnreadCount("c1"), 4);

  // A conclave the daemon no longer returns has nothing left to be unread.
  loadConclaves([]);
  assert.equal(conclaveUnreadTotal(), 0);
  clearConclaveStore();
});

// --- Arrivals only, or a real change? ---

test("a first load is never arrivals only", () => {
  clearConclaveStore();
  const load = loadConclaves([dto("c1", 2)]);
  assert.equal(load.changed, true);
  assert.equal(load.grewOnly, false, "there is no list on screen to append to yet");
  clearConclaveStore();
});

test("an arrival in a conclave that already exists is arrivals only", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 2)]);
  const load = loadConclaves([dto("c1", 3)]);
  assert.equal(load.changed, true);
  assert.equal(load.grewOnly, true);
  clearConclaveStore();
});

test("a renamed conclave is not arrivals only", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 2)]);
  const renamed = { ...dto("c1", 2), name: "a different name" };
  const load = loadConclaves([renamed]);
  assert.equal(load.grewOnly, false, "the card's name is only drawn by a rebuild");
  clearConclaveStore();
});

test("a new conclave is not arrivals only", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 2)]);
  const load = loadConclaves([dto("c1", 2), dto("c2", 1)]);
  assert.equal(load.grewOnly, false);
  clearConclaveStore();
});

test("a pruned log is not arrivals only", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 5)]);
  const load = loadConclaves([dto("c1", 2)]);
  assert.equal(load.changed, true);
  assert.equal(load.grewOnly, false, "messages left the log; the pane cannot be appended to");
  clearConclaveStore();
});

test("an ended conclave is not arrivals only", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 2)]);
  const ended = { ...dto("c1", 3), ended: true };
  const load = loadConclaves([ended]);
  assert.equal(load.grewOnly, false, "the read-only note and the dropped composer are a rebuild");
  clearConclaveStore();
});

test("a trustee joining is not arrivals only", () => {
  clearConclaveStore();
  loadConclaves([dto("c1", 2)]);
  const joined = {
    ...dto("c1", 2),
    trustees: [
      { account_id: "a0", nick: "me", state: "joined" as const },
      { account_id: "a1", nick: "alice", state: "joined" as const },
    ],
  };
  const load = loadConclaves([joined]);
  assert.equal(load.grewOnly, false, "the members row and the card's count both change");
  clearConclaveStore();
});

// --- The invitation cards ---
//
// An invitation is not a conclave: it lives in its own box above the cards, and
// the patch that paints it asks whether the box on screen is already the one the
// store holds. These pin that read, because a false "yes" leaves a card the user
// can no longer answer and a false "no" throws away buttons mid-press.

const invite = (conclaveId: string, receivedAt = 1): PendingInvite => ({
  conclaveId,
  name: conclaveId,
  convener: "a0",
  fromAccount: "a1",
  fromNick: "alice",
  trustees: [],
  receivedAt,
});

test("the invitation box is current when it holds the same invitations in order", () => {
  assert.equal(invitesAreCurrent(["c1", "c2"], [invite("c1"), invite("c2")]), true);
});

test("a new invitation arriving makes the box out of date", () => {
  assert.equal(invitesAreCurrent(["c1"], [invite("c2"), invite("c1")]), false);
});

test("an invitation answered makes the box out of date", () => {
  assert.equal(invitesAreCurrent(["c1", "c2"], [invite("c1")]), false);
});

test("a different invitation in the same slot makes the box out of date", () => {
  // The count is the same, so only the ids can tell: one invitation replaced by
  // another must redraw, or the card left on screen names the wrong conclave.
  assert.equal(invitesAreCurrent(["c1"], [invite("c3")]), false);
});

test("an empty box is current only when there is nothing to show", () => {
  assert.equal(invitesAreCurrent([], []), true);
  assert.equal(invitesAreCurrent([], [invite("c1")]), false);
});

// --- The column's shape ---

test("the empty state is only for no conclaves and no invitations", () => {
  assert.equal(conclaveListShape(0, 0), "empty");
});

test("one conclave is a list, invitations or not", () => {
  assert.equal(conclaveListShape(1, 0), "list");
  assert.equal(conclaveListShape(3, 2), "list");
});

test("an invitation with no conclaves is a list too", () => {
  // The case the invitation patch turns on: the box has to appear above cards
  // that are not there yet, so the column is a list with an empty cards
  // container, not the empty state.
  assert.equal(conclaveListShape(0, 1), "list");
});

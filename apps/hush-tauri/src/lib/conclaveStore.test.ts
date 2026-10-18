import { test } from "node:test";
import assert from "node:assert/strict";
import {
  candidatesToInvite,
  clearConclaveStore,
  conclaveUnreadCount,
  conclaveUnreadTotal,
  groupConclaveMessages,
  invitableCandidates,
  loadConclaves,
  markConclaveRead,
  type Conclave,
  type ConclaveMessage,
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
  assert.equal(loadConclaves([dto("c1", 3), dto("c2", 0)]), true);
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

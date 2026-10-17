import { test } from "node:test";
import assert from "node:assert/strict";
import { candidatesToInvite, invitableCandidates, type Conclave } from "./conclaveStore.ts";

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

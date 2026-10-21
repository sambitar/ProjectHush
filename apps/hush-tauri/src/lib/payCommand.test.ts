import { test } from "node:test";
import assert from "node:assert/strict";
import { parsePayCommand } from "./payCommand.ts";

test("a bare command is recognised so the dialog can ask for an amount", () => {
  assert.deepEqual(parsePayCommand("/pay"), { amount: "" });
  assert.deepEqual(parsePayCommand("/pay   "), { amount: "" });
});

test("an amount is carried as typed", () => {
  assert.deepEqual(parsePayCommand("/pay 12.50"), { amount: "12.50" });
  assert.deepEqual(parsePayCommand("  /pay .5  "), { amount: ".5" });
});

test("ordinary text and lookalikes are not commands", () => {
  assert.equal(parsePayCommand("hello /pay 5"), null);
  assert.equal(parsePayCommand("/payment 5"), null);
  assert.equal(parsePayCommand("/pay-me"), null);
  assert.equal(parsePayCommand("please /pay 5"), null);
  assert.equal(parsePayCommand(""), null);
});

test("the keyword is case insensitive", () => {
  assert.deepEqual(parsePayCommand("/Pay 3"), { amount: "3" });
});

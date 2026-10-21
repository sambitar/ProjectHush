import { test } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_EXPIRE_SECONDS,
  MIN_EXPIRE_SECONDS,
  parseExpireCommand,
  parseExpireSeconds,
} from "./expireCommand.ts";

test("a command carries everything after the keyword", () => {
  assert.deepEqual(parseExpireCommand("/expire burn after reading"), {
    body: "burn after reading",
  });
  assert.deepEqual(parseExpireCommand("/expire   spaced   "), {
    body: "spaced",
  });
  assert.deepEqual(parseExpireCommand("/expire line one\nline two"), {
    body: "line one\nline two",
  });
});

test("a bare command is recognised so the caller can ask for a body", () => {
  assert.deepEqual(parseExpireCommand("/expire"), { body: "" });
  assert.deepEqual(parseExpireCommand("/expire   "), { body: "" });
});

test("ordinary text and lookalikes are not commands", () => {
  assert.equal(parseExpireCommand("hello /expire world"), null);
  assert.equal(parseExpireCommand("/expired yesterday"), null);
  assert.equal(parseExpireCommand("/expire-me"), null);
  assert.equal(parseExpireCommand("please /expire this"), null);
  assert.equal(parseExpireCommand(""), null);
});

test("the keyword is case insensitive", () => {
  assert.deepEqual(parseExpireCommand("/Expire now"), { body: "now" });
});

test("only whole seconds within the offered range are accepted", () => {
  assert.equal(parseExpireSeconds("30"), 30);
  assert.equal(parseExpireSeconds(" 30 "), 30);
  assert.equal(parseExpireSeconds(String(MIN_EXPIRE_SECONDS)), MIN_EXPIRE_SECONDS);
  assert.equal(parseExpireSeconds(String(MAX_EXPIRE_SECONDS)), MAX_EXPIRE_SECONDS);
  assert.equal(parseExpireSeconds(String(MIN_EXPIRE_SECONDS - 1)), null);
  assert.equal(parseExpireSeconds(String(MAX_EXPIRE_SECONDS + 1)), null);
  assert.equal(parseExpireSeconds("0"), null);
  assert.equal(parseExpireSeconds("1.5"), null);
  assert.equal(parseExpireSeconds("1e2"), null);
  assert.equal(parseExpireSeconds("+30"), null);
  assert.equal(parseExpireSeconds(""), null);
});

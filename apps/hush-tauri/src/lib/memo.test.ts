import { test } from "node:test";
import assert from "node:assert/strict";
import { memoizeLru } from "./memo.ts";

// The memo behind `htmlToText`. Its policy is the whole fix — a bounded,
// least-recently-used cache — and the parse it wraps needs a DOM this runner
// does not have, so the policy is pinned here rather than through its caller.

/** A memo over a recording counter, so "how many times did it compute" is an
 *  assertion rather than an inference. */
function counting<T>(value: (key: string) => T) {
  const calls: string[] = [];
  const memo = memoizeLru<T>((key) => {
    calls.push(key);
    return value(key);
  }, 2);
  return { memo, calls };
}

test("a key is computed once and served from the cache after that", () => {
  const { memo, calls } = counting((key) => key.toUpperCase());
  assert.equal(memo("alice"), "ALICE");
  assert.equal(memo("alice"), "ALICE");
  assert.deepEqual(calls, ["alice"]);
});

test("distinct keys do not collide", () => {
  const { memo, calls } = counting((key) => key.toUpperCase());
  assert.equal(memo("alice"), "ALICE");
  assert.equal(memo("bob"), "BOB");
  assert.deepEqual(calls, ["alice", "bob"]);
});

test("the cache stays at its bound, evicting the least recently used key", () => {
  const { memo, calls } = counting((key) => key.toUpperCase());
  memo("a");
  memo("b");
  // Both slots are full; "c" evicts "a", which is the least recently used.
  memo("c");
  assert.deepEqual(calls, ["a", "b", "c"]);
  // "b" and "c" are still cached.
  memo("b");
  memo("c");
  assert.deepEqual(calls, ["a", "b", "c"]);
  // "a" was evicted, so it is computed again.
  memo("a");
  assert.deepEqual(calls, ["a", "b", "c", "a"]);
});

test("reading a key refreshes it, so a hit survives the next eviction", () => {
  const { memo, calls } = counting((key) => key.toUpperCase());
  memo("a");
  memo("b");
  // "a" is read, which makes "b" the least recently used.
  memo("a");
  memo("c");
  // "c" evicted "b", not "a".
  memo("a");
  assert.deepEqual(calls, ["a", "b", "c"]);
  memo("b");
  assert.deepEqual(calls, ["a", "b", "c", "b"]);
});

test("a bound below one still keeps one entry, rather than caching nothing", () => {
  const calls: string[] = [];
  const memo = memoizeLru((key: string) => {
    calls.push(key);
    return key;
  }, 0);
  memo("a");
  memo("a");
  assert.deepEqual(calls, ["a"]);
});

test("a cached undefined is a hit, not a miss", () => {
  const calls: string[] = [];
  const memo = memoizeLru<string | undefined>((key) => {
    calls.push(key);
    return undefined;
  }, 4);
  assert.equal(memo("a"), undefined);
  assert.equal(memo("a"), undefined);
  assert.deepEqual(calls, ["a"]);
});

test("a non-integer bound is floored", () => {
  const calls: string[] = [];
  const memo = memoizeLru((key: string) => {
    calls.push(key);
    return key;
  }, 2.9);
  memo("a");
  memo("b");
  memo("c");
  memo("a");
  assert.deepEqual(calls, ["a", "b", "c", "a"]);
});

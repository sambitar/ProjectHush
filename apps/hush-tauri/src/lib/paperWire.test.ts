import { test } from "node:test";
import assert from "node:assert/strict";
import * as Y from "yjs";
import {
  Awareness,
  applyAwarenessUpdate,
  encodeAwarenessUpdate,
} from "y-protocols/awareness";
import {
  fromBase64,
  isDecodable,
  mergeBatch,
  relabelAwareness,
  shouldApplyState,
  toBase64,
} from "./paperWire.ts";

/**
 * Unit tests for the Paper bridge's wire helpers (Task 5 Step 1).
 *
 * Run with `npm test` — Node's own runner, type-stripping the TypeScript, so
 * there is no test framework to keep in step with the app. What is covered is
 * the envelope and the revision bookkeeping: the parts where a mistake is
 * silent (a dropped byte, a skipped apply) rather than a visible crash.
 */

/** A text update, produced the way any Yjs peer produces one. */
function update(text: string): Uint8Array {
  const doc = new Y.Doc();
  const content = doc.getText("content");
  content.insert(0, text);
  return Y.encodeStateAsUpdate(doc);
}

test("base64 round-trips every byte value", () => {
  const bytes = new Uint8Array(256);
  for (let i = 0; i < 256; i++) bytes[i] = i;
  assert.deepEqual(fromBase64(toBase64(bytes)), bytes);
});

test("base64 round-trips an empty payload as empty", () => {
  assert.equal(toBase64(new Uint8Array()), "");
  assert.deepEqual(fromBase64(""), new Uint8Array());
});

test("base64 survives a real v1 update byte for byte", () => {
  const bytes = update("the paper crosses the wire");
  assert.deepEqual(fromBase64(toBase64(bytes)), bytes);
});

test("a batch of one is sent as it is", () => {
  const one = update("a");
  assert.equal(mergeBatch([one]), one);
});

test("a batch of many merges into one update that carries all of them", () => {
  // One author typing twice, which is what a batch actually is: two updates
  // from the same document, the second a delta against the first. (Two
  // independent documents would merge in an order Yjs is free to choose.)
  const doc = new Y.Doc();
  const content = doc.getText("content");
  content.insert(0, "first ");
  const first = Y.encodeStateAsUpdate(doc);
  const before = Y.encodeStateVector(doc);
  content.insert(content.length, "second");
  const second = Y.encodeStateAsUpdate(doc, before);

  const merged = mergeBatch([first, second]);
  assert.ok(merged, "two updates merge into one");

  const applied = new Y.Doc();
  Y.applyUpdate(applied, merged);
  assert.equal(applied.getText("content").toString(), "first second");
});

test("an empty batch has nothing to send", () => {
  assert.equal(mergeBatch([]), null);
});

test("a higher revision is applied", () => {
  assert.equal(shouldApplyState(3, 4, false), true);
});

test("an unchanged revision is not re-applied", () => {
  assert.equal(shouldApplyState(4, 4, false), false);
});

test("a compaction is applied even when the revision has not moved", () => {
  // The daemon rebuilt its log into a snapshot: same revision, new bytes.
  assert.equal(shouldApplyState(4, 4, true), true);
});

test("zero bytes are not a decodable update", () => {
  assert.equal(isDecodable(new Uint8Array()), false);
  assert.equal(isDecodable(update("x")), true);
});

test("an awareness frame crosses the envelope with its cursor intact", () => {
  // This is exactly what a presence frame is: the sender's awareness update,
  // base64 in the daemon's request body, decoded on the far side. The daemon
  // never reads it, so the only thing that can break it is the envelope.
  const senderDoc = new Y.Doc();
  const sender = new Awareness(senderDoc);
  sender.setLocalState({
    user: { name: "alice", color: "#2b6cb0" },
    cursor: { anchor: 3, head: 5 },
  });
  const frame = toBase64(encodeAwarenessUpdate(sender, [sender.clientID]));

  const receiverDoc = new Y.Doc();
  const receiver = new Awareness(receiverDoc);
  applyAwarenessUpdate(receiver, fromBase64(frame), "remote");

  const seen = receiver.getStates().get(sender.clientID);
  assert.ok(seen, "the sender's state is now the receiver's");
  assert.deepEqual(seen.user, { name: "alice", color: "#2b6cb0" });
  assert.deepEqual(seen.cursor, { anchor: 3, head: 5 });

  sender.destroy();
  receiver.destroy();
});

test("a peer's account id is relabelled with the name our roster gives it", () => {
  const id = "05cde85f415c09d24220b5bda8dc1ce04364e9678c450960d7272376de9f0cbd37";
  const senderDoc = new Y.Doc();
  const sender = new Awareness(senderDoc);
  sender.setLocalState({
    user: { name: id, color: "#2f855a" },
    cursor: { anchor: 3, head: 5 },
  });
  const frame = encodeAwarenessUpdate(sender, [sender.clientID]);

  const relabelled = relabelAwareness(frame, (account) =>
    account === id ? "bob" : null,
  );

  const receiverDoc = new Y.Doc();
  const receiver = new Awareness(receiverDoc);
  applyAwarenessUpdate(receiver, relabelled, "remote");
  const seen = receiver.getStates().get(sender.clientID);
  assert.ok(seen);
  assert.deepEqual(
    seen.user,
    { name: "bob", color: "#2f855a" },
    "the label is the name, and the colour is still the peer's own",
  );
  assert.deepEqual(seen.cursor, { anchor: 3, head: 5 }, "the cursor survived");

  sender.destroy();
  receiver.destroy();
});

test("a frame is passed through untouched for an account the roster cannot place", () => {
  // An unknown peer keeps its id rather than losing its cursor: a long label
  // is better than a caret that silently is not there.
  const senderDoc = new Y.Doc();
  const sender = new Awareness(senderDoc);
  sender.setLocalState({ user: { name: "someone-else", color: "#2b6cb0" } });
  const frame = encodeAwarenessUpdate(sender, [sender.clientID]);

  const same = relabelAwareness(frame, () => null);
  assert.deepEqual(Array.from(same), Array.from(frame));

  sender.destroy();
});

test("bytes this build cannot read come back unchanged", () => {
  // Forwarding beats throwing: an unreadable frame is dropped by the caller,
  // and this function must not turn it into a different kind of nothing.
  for (const junk of [new Uint8Array(), new Uint8Array([0x01]), new Uint8Array([0xff, 0xff, 0xff])]) {
    assert.deepEqual(
      Array.from(relabelAwareness(junk, () => "x")),
      Array.from(junk),
    );
  }
});

test("a two-peer frame relabels each state on its own", () => {
  const aDoc = new Y.Doc();
  const a = new Awareness(aDoc);
  a.setLocalState({ user: { name: "acct-a", color: "#2b6cb0" } });
  const bDoc = new Y.Doc();
  const b = new Awareness(bDoc);
  b.setLocalState({ user: { name: "acct-b", color: "#2f855a" } });
  const frame = encodeAwarenessUpdate(a, [a.clientID]);
  const both = new Uint8Array([...frame, ...encodeAwarenessUpdate(b, [b.clientID]).subarray(1)]);
  // Rebuild the count byte, since concatenating two frames double-counts.
  both[0] = 2;

  const relabelled = relabelAwareness(both, (id) => (id === "acct-a" ? "alice" : "bob"));
  const receiverDoc = new Y.Doc();
  const receiver = new Awareness(receiverDoc);
  applyAwarenessUpdate(receiver, relabelled, "remote");
  assert.equal(receiver.getStates().get(a.clientID)?.user?.name, "alice");
  assert.equal(receiver.getStates().get(b.clientID)?.user?.name, "bob");

  a.destroy();
  b.destroy();
  receiver.destroy();
});

test("a leaving peer's frame removes its cursor on the far side", () => {
  const senderDoc = new Y.Doc();
  const sender = new Awareness(senderDoc);
  sender.setLocalState({ user: { name: "bob", color: "#2f855a" } });
  const hello = toBase64(encodeAwarenessUpdate(sender, [sender.clientID]));

  const receiverDoc = new Y.Doc();
  const receiver = new Awareness(receiverDoc);
  applyAwarenessUpdate(receiver, fromBase64(hello), "remote");
  assert.ok(receiver.getStates().has(sender.clientID));

  // Closing the paper clears the local state, which encodes as a removal.
  sender.setLocalState(null);
  const bye = toBase64(encodeAwarenessUpdate(sender, [sender.clientID]));
  applyAwarenessUpdate(receiver, fromBase64(bye), "remote");
  assert.equal(
    receiver.getStates().has(sender.clientID),
    false,
    "a peer that closed the paper leaves no cursor behind",
  );

  sender.destroy();
  receiver.destroy();
});

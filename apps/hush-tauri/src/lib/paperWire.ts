import * as Y from "yjs";

/**
 * The Paper bridge's wire helpers.
 *
 * Kept out of `paperStore.ts` on purpose: that module mounts ProseMirror and
 * talks to the daemon, neither of which belongs in a unit test. Everything here
 * is pure or depends only on Yjs, so it can be exercised with `node --test`
 * (see `paperWire.test.ts`) without a DOM or a running daemon.
 */

/** Base64 of an opaque byte payload. `btoa` takes a binary string, not bytes. */
export function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}

export function fromBase64(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

/**
 * Collapse the local edits batched since the last flush into one update.
 *
 * The daemon stores what it is given as one log entry and fans out one payload,
 * so a burst of keystrokes should cost one entry rather than one per character.
 * `null` means there was nothing to send.
 */
export function mergeBatch(batch: Uint8Array[]): Uint8Array | null {
  if (batch.length === 0) return null;
  return batch.length === 1 ? batch[0] : Y.mergeUpdates(batch);
}

/**
 * Whether the state the daemon just reported is worth applying.
 *
 * `revision` is the daemon's own counter and never restarts, so a higher one
 * means new bytes. `compacted` means the daemon replaced its log with a
 * snapshot: the revision may be unchanged, but the state that answers for it
 * was rebuilt, so it is applied again. Re-applying is always safe — Yjs folds
 * an update it already has into a no-op — so the risk is only wasted work.
 */
export function shouldApplyState(
  lastRev: number,
  revision: number,
  compacted: boolean,
): boolean {
  return revision > lastRev || compacted;
}

/**
 * Whether a payload is a decodable Yjs update.
 *
 * Zero bytes are not one — `Y.applyUpdate` throws `Unexpected end of array` on
 * an empty array — and "no state yet" is a normal answer for a paper nobody has
 * typed in, so the caller checks rather than catching.
 */
export function isDecodable(bytes: Uint8Array): boolean {
  return bytes.length > 0;
}

// --- Awareness frames ------------------------------------------------------
//
// The y-protocols awareness format, re-implemented here because the one thing
// this bridge needs to do to a peer's frame is read it: `applyAwarenessUpdate`
// is a black box, and a caret labelled with 64 hex characters is a worse label
// than a name. The encoding is a count, then per client a varint id, a varint
// clock, and a length-prefixed JSON blob.

function readVarUint(bytes: Uint8Array, at: number): [number, number] | null {
  let value = 0;
  let shift = 0;
  let i = at;
  while (i < bytes.length) {
    const byte = bytes[i++];
    value += (byte & 0x7f) * 2 ** shift;
    if (byte < 0x80) return [value, i];
    shift += 7;
    if (shift > 49) return null;
  }
  return null;
}

function writeVarUint(out: number[], value: number): void {
  let v = value;
  while (v > 0x7f) {
    out.push((v & 0x7f) | 0x80);
    v = Math.floor(v / 128);
  }
  out.push(v);
}

/**
 * Swap the display name in a peer's awareness frame.
 *
 * A peer publishes its **account id** as `user.name`, and that is deliberate:
 * it is the identity the relay authenticated, so a peer cannot label its caret
 * with someone else's name. It is not a name, though — the label is drawn next
 * to the caret, and a 64-character hash there is worse than no label at all. So
 * the receiver resolves the id against its own roster, the same source the
 * member list and the chat use, which is also what keeps the caret from
 * disagreeing with them.
 *
 * `nameFor` returns `null` for an account the roster cannot place, and the
 * frame is passed through untouched in that case: an unknown peer keeps its id
 * rather than losing its cursor. Unreadable bytes are returned unchanged too —
 * dropping a frame this build cannot parse is the caller's job, not this one's.
 */
export function relabelAwareness(
  bytes: Uint8Array,
  nameFor: (accountId: string) => string | null,
): Uint8Array {
  const count = readVarUint(bytes, 0);
  if (!count) return bytes;
  const out: number[] = [];
  writeVarUint(out, count[0]);

  let at = count[1];
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  for (let n = 0; n < count[0]; n++) {
    const client = readVarUint(bytes, at);
    if (!client) return bytes;
    const clock = readVarUint(bytes, client[1]);
    if (!clock) return bytes;
    const len = readVarUint(bytes, clock[1]);
    if (!len || len[1] + len[0] > bytes.length) return bytes;

    const raw = bytes.subarray(len[1], len[1] + len[0]);
    let body = raw;
    try {
      const state = JSON.parse(decoder.decode(raw)) as {
        user?: { name?: unknown };
      };
      const id = typeof state.user?.name === "string" ? state.user.name : null;
      const name = id ? nameFor(id) : null;
      if (name && state.user) {
        state.user.name = name;
        body = encoder.encode(JSON.stringify(state));
      }
    } catch {
      // Not JSON this build understands: forward it as it came.
    }

    writeVarUint(out, client[0]);
    writeVarUint(out, clock[0]);
    writeVarUint(out, body.length);
    for (const byte of body) out.push(byte);
    at = len[1] + len[0];
  }
  return new Uint8Array(out);
}

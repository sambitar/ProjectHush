import { test } from "node:test";
import assert from "node:assert/strict";
import { activeRingIn, voiceCueFor } from "./voiceCue.ts";
import type { VoiceViewDto } from "./api/types.ts";

// The cue is the only thing that makes a voice call audible, and it has three
// inputs that fight each other — which room is ringing, which room this vault
// opened, and whether the window is even in front. The decision is pinned here
// because the class that calls it has no tests at all.

const NOW = 1_000_000;

/** A live room, in the shape the daemon reports one. Defaults describe a ring
 *  aimed at this vault: someone else opened it, they are the only one in it,
 *  and its 30 s stamp is still running. */
function room(over: Partial<VoiceViewDto> = {}): VoiceViewDto {
  return {
    session_id: "s1",
    conclave_id: "c1",
    opener: "them",
    started_ms: 0,
    joined: [{ account: "them", muted: false }],
    i_am_in: false,
    ringing_until_ms: NOW + 30_000,
    ...over,
  };
}

/** The cue for these rooms, with the shell otherwise idle and in front. */
function cue(rooms: VoiceViewDto[], over: Partial<Parameters<typeof voiceCueFor>[0]> = {}) {
  return voiceCueFor({
    rooms,
    dismissed: new Set<string>(),
    me: "me",
    soundsEnabled: true,
    windowVisible: true,
    now: NOW,
    ...over,
  });
}

test("a live ring aimed at this vault plays the ringtone", () => {
  assert.equal(cue([room()]), "ring");
});

test("a ring this vault already answered or declined is silent", () => {
  assert.equal(cue([room()], { dismissed: new Set(["s1"]) }), "none");
});

test("a ring whose stamp has run out is silent", () => {
  assert.equal(cue([room({ ringing_until_ms: NOW - 1 })]), "none");
});

test("a ring with no stamp at all is silent, not an endless one", () => {
  // The daemon omits the field when it predates rings; absence is "no ring".
  assert.equal(cue([room({ ringing_until_ms: null })]), "none");
});

test("a room this vault is already in does not ring at it", () => {
  assert.equal(cue([room({ i_am_in: true })]), "none");
});

test("a room that already has a second trustee in it does not ring", () => {
  const answered = room({ joined: [{ account: "them", muted: false }, { account: "x", muted: false }] });
  assert.equal(cue([answered]), "none");
});

test("the newest live ring wins when two trustees open at once", () => {
  const older = room({ session_id: "old", ringing_until_ms: NOW + 1_000 });
  const newer = room({ session_id: "new", ringing_until_ms: NOW + 20_000 });
  assert.equal(activeRingIn([older, newer], new Set(), NOW)?.session_id, "new");
  assert.equal(activeRingIn([newer, older], new Set(), NOW)?.session_id, "new");
});

test("a room this vault opened and is alone in plays the ringback", () => {
  const mine = room({ opener: "me", i_am_in: true });
  assert.equal(cue([mine]), "tone");
});

test("a room someone else opened does not play the ringback, even alone in it", () => {
  // The ringback is for the caller waiting for an answer, not for a vault that
  // happens to be sitting in an emptied room.
  const theirs = room({ opener: "them", i_am_in: true });
  assert.equal(cue([theirs]), "none");
});

test("the ringback stops as soon as somebody joins", () => {
  const answered = room({
    opener: "me",
    i_am_in: true,
    joined: [{ account: "me", muted: false }, { account: "them", muted: false }],
  });
  assert.equal(cue([answered]), "none");
});

test("an incoming call outranks the ringback for a room already waiting", () => {
  const mine = room({ session_id: "mine", opener: "me", i_am_in: true });
  const theirs = room({ session_id: "theirs", conclave_id: "c2" });
  assert.equal(cue([mine, theirs]), "ring");
});

test("a hidden window still rings, because a call must reach the user", () => {
  assert.equal(cue([room()], { windowVisible: false }), "ring");
});

test("a hidden window does not play the ringback", () => {
  // Otherwise a minimised window with an open room loops a tone indefinitely.
  const mine = room({ opener: "me", i_am_in: true });
  assert.equal(cue([mine], { windowVisible: false }), "none");
});

test("the sound switch silences the ring and the ringback alike", () => {
  const mine = room({ session_id: "mine", opener: "me", i_am_in: true });
  assert.equal(cue([room(), mine], { soundsEnabled: false }), "none");
});

test("an unread status cannot mistake an unknown opener for this vault", () => {
  const orphan = room({ opener: "", i_am_in: true });
  assert.equal(cue([orphan], { me: "" }), "none");
});

test("no live rooms is silence", () => {
  assert.equal(cue([]), "none");
});

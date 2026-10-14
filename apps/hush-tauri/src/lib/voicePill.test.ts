import { test } from "node:test";
import assert from "node:assert/strict";
import { voicePill } from "./voicePill.ts";
import type { VoiceViewDto } from "./api/types.ts";

const ME = "aa";

function room(over: Partial<VoiceViewDto> = {}): VoiceViewDto {
  return {
    session_id: "v-1",
    conclave_id: "c-1",
    opener: "aa",
    started_ms: 1_000,
    joined: [
      { account: "aa", muted: false },
      { account: "bb", muted: true },
    ],
    i_am_in: true,
    ...over,
  };
}

test("no room is a start-voice action, both lights dark", () => {
  for (const state of [undefined, null]) {
    const pill = voicePill(state as VoiceViewDto | null | undefined, ME);
    assert.equal(pill.action, "open");
    assert.equal(pill.inRoom, false);
    assert.equal(pill.count, 0);
    assert.equal(pill.label, "Start voice");
    assert.equal(pill.led, "off");
    assert.equal(pill.micLed, "off");
  }
});

test("a room this vault has not joined is a join action, with its size", () => {
  const pill = voicePill(room(), "ee");
  assert.equal(pill.action, "join");
  assert.equal(pill.inRoom, false);
  assert.equal(pill.count, 2);
  assert.match(pill.label, /2/);
  assert.equal(pill.led, "off");
  assert.equal(pill.micLed, "off");
});

test("a room this vault is in is live, with mute and leave available", () => {
  const pill = voicePill(room(), ME);
  assert.equal(pill.action, "none");
  assert.equal(pill.inRoom, true);
  assert.equal(pill.muted, false);
  assert.equal(pill.led, "green");
  assert.equal(pill.micLed, "green");
});

test("my muted flag reads on the pill while i am in", () => {
  const pill = voicePill(room(), "bb");
  assert.equal(pill.inRoom, true);
  assert.equal(pill.muted, true);
  assert.match(pill.label, /muted/);
  // The room is still live; only this vault's mic is dark.
  assert.equal(pill.led, "green");
  assert.equal(pill.micLed, "off");
});

test("another member's mute is not mine", () => {
  const pill = voicePill(room(), ME);
  assert.equal(pill.inRoom, true);
  assert.equal(pill.muted, false);
  assert.equal(pill.micLed, "green");
});

test("an ended-by-leave view (empty room) is the start state again", () => {
  const pill = voicePill(room({ joined: [], i_am_in: false }), ME);
  assert.equal(pill.action, "open");
  assert.equal(pill.inRoom, false);
  assert.equal(pill.count, 0);
  assert.equal(pill.led, "off");
  assert.equal(pill.micLed, "off");
});

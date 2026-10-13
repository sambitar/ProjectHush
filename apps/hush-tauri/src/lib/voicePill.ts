import type { VoiceViewDto } from "./api/types.ts";

/**
 * What the voice pill reads and does, derived from one conclave's room state.
 *
 * The daemon's `i_am_in` is not consulted: membership is judged from the
 * `joined` list against this vault's account, so the pill and the room view
 * cannot disagree about the one fact every label hinges on.
 */
export interface VoicePillState {
  /** The pill's text, in the surface's plain voice. */
  label: string;
  /** The primary action when not in the room; `none` while in it. */
  action: "open" | "join" | "none";
  inRoom: boolean;
  /** This vault's own mute flag — never another member's. */
  muted: boolean;
  /** Joined trustees, this vault included when it is in. */
  count: number;
}

export function voicePill(
  state: VoiceViewDto | null | undefined,
  me: string,
): VoicePillState {
  const room = state ?? null;
  if (!room || room.joined.length === 0) {
    // No room — or the empty shape a leave-that-ended returns. Both are the
    // start state: nothing is live, the next action is opening one.
    return {
      label: "Start voice",
      action: "open",
      inRoom: false,
      muted: false,
      count: 0,
    };
  }
  const mine = room.joined.find((m) => m.account === me);
  if (!mine) {
    const n = room.joined.length;
    return {
      label: `Join voice · ${n}`,
      action: "join",
      inRoom: false,
      muted: false,
      count: n,
    };
  }
  if (mine.muted) {
    return {
      label: `Voice · ${room.joined.length} · muted`,
      action: "none",
      inRoom: true,
      muted: true,
      count: room.joined.length,
    };
  }
  return {
    label: `Voice · ${room.joined.length}`,
    action: "none",
    inRoom: true,
    muted: false,
    count: room.joined.length,
  };
}

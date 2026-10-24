import { invoke } from "@tauri-apps/api/core";
import type { VoiceViewDto } from "./api/types.ts";
import { isTauriShell } from "./tauriEnv.ts";

/**
 * The voice call cues: what the speakers should be doing about voice right now.
 *
 * - `ring` is the ringtone, for the vault being called.
 * - `tone` is the ringback, for the vault that opened the room and is still
 *   alone in it.
 *
 * The same two clips serve 1:1 calls: an offer waiting for an answer rings, and
 * an outgoing call that has not been answered yet plays the ringback. There is
 * one cue writer, so a conclave and a call cannot both make noise through
 * different paths.
 *
 * Both loop until the state says otherwise, so unlike the chat arrival sound
 * this is a level rather than an event: the caller sends the state it wants and
 * the shell makes that true. Re-sending the current cue is a no-op, which is
 * what lets the caller send it from the room poll without restarting the clip
 * every few seconds.
 */
export type VoiceCue = "none" | "ring" | "tone";

/**
 * Where a live 1:1 call stands, from the caller's point of view.
 *
 * `ringing` is an outgoing call nobody has answered; `connecting` and `active`
 * are both past that point, so they play nothing. An unanswered incoming offer
 * is `incoming`.
 */
export type CallCueState =
  | "incoming"
  | "ringing"
  | "connecting"
  | "active";

/**
 * The one ring a vault would prompt for, if any: the newest live ring it has
 * not answered or declined.
 *
 * Several trustees opening at once is the degenerate case — the newest is the
 * one on screen, and the pill still lists every room.
 */
export function activeRingIn(
  rooms: readonly VoiceViewDto[],
  dismissed: ReadonlySet<string>,
  now: number,
): VoiceViewDto | null {
  const rings = rooms
    .filter(
      (r) =>
        !r.i_am_in &&
        r.joined.length <= 1 &&
        (r.ringing_until_ms ?? 0) > now &&
        !dismissed.has(r.session_id),
    )
    .sort((a, b) => (b.ringing_until_ms ?? 0) - (a.ringing_until_ms ?? 0));
  return rings[0] ?? null;
}

/**
 * The cue to play, from the room list and the shell's own state.
 *
 * Pure, so the precedence is pinned by tests rather than by reading the class
 * that calls it:
 *
 * - The ring outranks the ringback. Two conclaves can put a vault in both
 *   states at once, and the call arriving is the one to attend to. A 1:1 offer
 *   opening while a ringback is already looping is the same case.
 * - A hidden window still gets the ring — that is the whole point of it, since
 *   a call is discovered by polling and the caller's stamp is 30 s — but not
 *   the ringback, which exists to reassure someone watching the waiting screen.
 *   A tone looping out of a minimised window is noise, not information.
 * - The sound switch silences both. It is the app's only sounds control, and a
 *   ringtone a muted user did not ask for is worse than a quiet room.
 */
export function voiceCueFor(input: {
  rooms: readonly VoiceViewDto[];
  dismissed: ReadonlySet<string>;
  /** This vault's account id; empty when the status has not been read yet. */
  me: string;
  soundsEnabled: boolean;
  windowVisible: boolean;
  now: number;
  /** The live 1:1 call this side is holding, if any. */
  call?: CallCueState | null;
}): VoiceCue {
  if (!input.soundsEnabled) return "none";
  if (input.call === "incoming") return "ring";
  if (activeRingIn(input.rooms, input.dismissed, input.now)) return "ring";
  if (!input.windowVisible) return "none";
  // The ringback lasts exactly until the far side answers: `connecting` and
  // `active` mean the answer already arrived.
  if (input.call === "ringing") return "tone";
  // A room opened by someone else is not a call this vault is placing, and
  // without an account id there is no way to tell the two apart.
  if (!input.me) return "none";
  const waiting = input.rooms.some(
    (r) => r.i_am_in && r.opener === input.me && r.joined.length <= 1,
  );
  return waiting ? "tone" : "none";
}

/**
 * Push the cue to the shell.
 *
 * Native only. The browser fallback the chat sound has would need the clips in
 * the webview bundle (900 kB of MP3 for a dev-only path), and this app's calls
 * only work in the shell anyway.
 */
export function setVoiceCue(cue: VoiceCue): void {
  if (!isTauriShell()) return;
  void invoke("set_voice_cue", { cue }).catch(() => {
    /* No audio device in the shell; the call UI still shows the state. */
  });
}

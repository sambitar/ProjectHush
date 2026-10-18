import { getCurrentWindow, UserAttentionType } from "@tauri-apps/api/window";
import { isTauriShell } from "./tauriEnv";

/**
 * Ask the OS for the user's attention, for something the user cannot see.
 *
 * This is the window manager's own channel for it: taskbar urgency on Linux, a
 * dock bounce on macOS, a taskbar flash on Windows. It does nothing while the
 * window is already focused, which is what we want — a prompt that is on screen
 * needs no second signal.
 *
 * `Critical` rather than `Informational` so the request keeps asking until the
 * window is looked at, the way a ringing call does.
 *
 * Silent on failure, and outside the Tauri shell. Every caller already has
 * another signal — the ring's chime is played by the host audio device — so
 * this is a second one, never the only one, and must not be able to break it.
 */
export function requestAttention(): void {
  if (!isTauriShell()) return;
  try {
    void getCurrentWindow()
      .requestUserAttention(UserAttentionType.Critical)
      .catch(() => {});
  } catch {
    /* No window manager support for it; the other signal still fires. */
  }
}

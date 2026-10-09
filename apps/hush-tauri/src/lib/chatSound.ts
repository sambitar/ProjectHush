import { invoke } from "@tauri-apps/api/core";
import soundUrl from "../assets/message-notification.wav";
import { isTauriShell } from "./tauriEnv";

/**
 * The chat arrival sound.
 *
 * In the desktop shell it is played natively by the Tauri process (`rodio`),
 * because the webview's audio path depends on the host's GStreamer plugin set
 * and silently fails when that is incomplete. In a plain browser (dev) it
 * falls back to an `Audio` element. One shared element, so repeated messages
 * cannot stack overlapping copies of the clip.
 */
let audio: HTMLAudioElement | null = null;

export function playChatSound(): void {
  if (isTauriShell()) {
    void invoke("play_chat_sound").catch(() => {
      /* No audio device in the shell; the badge still shows. */
    });
    return;
  }
  try {
    if (!audio) {
      audio = new Audio(soundUrl);
      audio.preload = "auto";
    }
    audio.currentTime = 0;
    void audio.play().catch(() => {
      /* Autoplay refused or no audio device; the badge still shows. */
    });
  } catch {
    /* No audio element available; ignore. */
  }
}

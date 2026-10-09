/**
 * The Tauri bridge for Drops.
 *
 * Every call degrades gracefully outside the desktop shell so the `--dev`
 * browser mode still renders the rail; it just reports Tor as unavailable and
 * refuses to open anything.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriShell } from "../tauriEnv";
import type { Drop } from "./types";

export type TorPhase =
  | "stopped"
  | "starting"
  | "ready"
  | "failed"
  | "unavailable";

export interface TorStatus {
  phase: TorPhase;
  socks_port: number | null;
  binary: string | null;
  message: string | null;
}

export interface DropOpenState {
  open: boolean;
  id: string | null;
  name: string | null;
}

const NO_SHELL: TorStatus = {
  phase: "unavailable",
  socks_port: null,
  binary: null,
  message: "Tor is only available in the Hush desktop app.",
};

/** Whether the Drops capability is present at all (i.e. the Tauri shell). */
export function dropsAvailable(): boolean {
  return isTauriShell();
}

export async function torStatus(): Promise<TorStatus> {
  if (!isTauriShell()) return NO_SHELL;
  try {
    return await invoke<TorStatus>("drops_tor_status");
  } catch {
    return { ...NO_SHELL, message: "Could not read the Tor status." };
  }
}

/** Bring Tor up and wait for readiness. Resolves with the final status. */
export async function torStart(timeoutMs = 90_000): Promise<TorStatus> {
  if (!isTauriShell()) return NO_SHELL;
  try {
    return await invoke<TorStatus>("drops_tor_start", { timeoutMs });
  } catch (e) {
    return { ...NO_SHELL, message: String(e) };
  }
}

export async function torStop(): Promise<TorStatus> {
  if (!isTauriShell()) return NO_SHELL;
  try {
    return await invoke<TorStatus>("drops_tor_stop");
  } catch (e) {
    return { ...NO_SHELL, message: String(e) };
  }
}

/** Whether the backend will allow development Drops on this machine. */
export async function devEnabled(): Promise<boolean> {
  if (!isTauriShell()) return false;
  try {
    return await invoke<boolean>("drops_dev_enabled");
  } catch {
    return false;
  }
}

export async function openState(): Promise<DropOpenState> {
  if (!isTauriShell()) return { open: false, id: null, name: null };
  try {
    return await invoke<DropOpenState>("drops_open_state");
  } catch {
    return { open: false, id: null, name: null };
  }
}

/**
 * Open a Drop. Validation happens again in Rust; this only picks the URL and
 * says whether it is the development entry.
 */
export async function openDrop(drop: Drop): Promise<void> {
  const url = drop.devOnly ? drop.devTarget : drop.onionAddress;
  if (!url) throw new Error("That Drop has no address to open.");
  await invoke("drops_open", {
    req: { id: drop.id, name: drop.name, url, dev: !!drop.devOnly },
  });
}

export async function closeDrop(): Promise<void> {
  if (!isTauriShell()) return;
  await invoke("drops_close");
}

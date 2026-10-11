/**
 * In-app updates.
 *
 * One small wrapper over `@tauri-apps/plugin-updater` and the `process`
 * plugin's relaunch, so the UI only ever deals with plain values. Nothing in
 * here throws on a *check*: a failed check is an outcome, not an exception,
 * because the two callers want opposite things from it. The startup check is a
 * courtesy the user never asked for and must fail silently; the button in
 * Settings is a request, and its failure has to be visible.
 *
 * The live `Update` handle holds a resource on the Rust side, so it is kept in
 * one module-level slot between "found an update" and "install it" — and
 * released again when the user says Later, so a dismissed prompt does not leak
 * the handle until the next check.
 *
 * Only the desktop shell has an updater; in a plain browser (the Vite dev
 * server) every entry point reports `unavailable` instead of throwing.
 */

import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { isTauriShell } from "./tauriEnv";

/** An available update, reduced to what the prompt shows. */
export interface AvailableUpdate {
  /** The version the release announces. */
  version: string;
  /** The version running right now, so the prompt can say "from X to Y". */
  currentVersion: string;
  /** Release notes from the manifest; null when the release had none. */
  notes: string | null;
  /** The release's publish date (RFC 3339), when the manifest carried one. */
  date: string | null;
}

export type CheckOutcome =
  /** A newer version is published. */
  | { kind: "available"; update: AvailableUpdate }
  /** The endpoint answered and this build is the newest one. */
  | { kind: "up-to-date" }
  /**
   * No answer is possible here: a plain browser, or a failed check during a
   * silent startup probe (which has no UI to carry the error).
   */
  | { kind: "unavailable"; reason: string }
  /** The check failed and the user asked for it, so the message is worth showing. */
  | { kind: "failed"; message: string };

/** Download progress, already summed so the UI does not add up chunk lengths. */
export interface UpdateProgress {
  /** Bytes received so far. */
  downloaded: number;
  /** Total bytes, or null when the server sent no content length. */
  total: number | null;
  /** 0–100, or null while `total` is unknown. */
  percent: number | null;
  /** True once the download is done and the install is running. */
  installing: boolean;
}

/** The update found by the last check, waiting to be installed or dismissed. */
let pending: Update | null = null;

/** The version of the running build, or null outside the desktop shell. */
export async function getCurrentVersion(): Promise<string | null> {
  if (!isTauriShell()) return null;
  try {
    return await getVersion();
  } catch {
    return null;
  }
}

/**
 * Ask the configured endpoint whether a newer release exists.
 *
 * Never throws. `silent` is what the startup probe sets: a failure there
 * becomes `unavailable` rather than `failed`, because a check the user did not
 * ask for has nowhere to show an error — it is logged and dropped.
 *
 * A previous update that was found but not installed is released before the
 * new answer replaces it, so repeated checks cannot pile up backend handles.
 */
export async function checkForUpdates(
  opts: { silent?: boolean } = {},
): Promise<CheckOutcome> {
  if (!isTauriShell()) {
    return {
      kind: "unavailable",
      reason: "Updates are only available in the desktop app.",
    };
  }
  await discardPendingUpdate();
  try {
    const update = await check();
    if (!update) return { kind: "up-to-date" };
    pending = update;
    return {
      kind: "available",
      update: {
        version: update.version,
        currentVersion: update.currentVersion,
        notes: update.body?.trim() ? update.body : null,
        date: update.date ?? null,
      },
    };
  } catch (e) {
    const message = friendlyError(e);
    if (opts.silent) {
      // Deliberately quiet: this runs behind the splash, uninvited.
      console.debug("[updater] startup check failed:", message);
      return { kind: "unavailable", reason: message };
    }
    return { kind: "failed", message };
  }
}

/**
 * Download the pending update, install it, and relaunch into it.
 *
 * Progress arrives as the plugin's `Started` / `Progress` / `Finished` events,
 * folded into a running byte count. Throws with a readable message if the
 * download or the signature check fails — the caller shows it and keeps the
 * app running; nothing here can leave the install half-applied, because the
 * plugin verifies the artifact before it replaces anything.
 *
 * On Windows the installer takes over and exits this process, so the relaunch
 * at the end is only reached on macOS and Linux.
 */
export async function downloadAndInstallUpdate(
  onProgress?: (progress: UpdateProgress) => void,
): Promise<void> {
  const update = pending;
  if (!update) {
    throw new Error("There is no checked-for update to install.");
  }
  let downloaded = 0;
  let total: number | null = null;
  const report = (installing: boolean) =>
    onProgress?.({
      downloaded,
      total,
      percent:
        total && total > 0 ? Math.min(100, (downloaded / total) * 100) : null,
      installing,
    });

  try {
    await update.downloadAndInstall((event: DownloadEvent) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? null;
        downloaded = 0;
        report(false);
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        report(false);
      } else {
        // Finished: the bytes are in, the install is about to run.
        report(true);
      }
    });
  } catch (e) {
    throw new Error(friendlyError(e));
  }
  // The handle is spent once it has installed; a second use would fail.
  pending = null;
  await relaunch();
}

/**
 * Let go of an update the user chose not to install.
 *
 * The `Update` holds a resource on the Rust side; without this, a dismissed
 * prompt would keep it until the next check replaced it.
 */
export async function discardPendingUpdate(): Promise<void> {
  const update = pending;
  pending = null;
  if (!update) return;
  try {
    await update.close();
  } catch {
    /* already released, or the resource was consumed by an install */
  }
}

/**
 * Turn whatever the plugin threw into something worth showing a person.
 *
 * The plugin surfaces Rust-side errors as bare strings, so the classification
 * is by keyword. The original text is kept on the end rather than replaced: it
 * is the only thing that makes an unclassified failure debuggable.
 */
function friendlyError(e: unknown): string {
  const raw = (e instanceof Error ? e.message : String(e)).trim();
  const lower = raw.toLowerCase();
  if (
    lower.includes("public key") ||
    lower.includes("pubkey") ||
    lower.includes("invalid encoding") ||
    lower.includes("base64")
  ) {
    return `The updater is not configured yet: the release public key in tauri.conf.json is missing or invalid. (${raw})`;
  }
  if (lower.includes("signature") || lower.includes("minisign") || lower.includes("verify")) {
    return `The update did not pass its signature check, so nothing was installed. (${raw})`;
  }
  if (
    lower.includes("dns") ||
    lower.includes("connect") ||
    lower.includes("timed out") ||
    lower.includes("timeout") ||
    lower.includes("network") ||
    lower.includes("unreachable")
  ) {
    return `Could not reach the update server. Check your connection and try again. (${raw})`;
  }
  if (lower.includes("404") || lower.includes("not found")) {
    return `The update server has no release manifest yet. (${raw})`;
  }
  return raw || "The update failed for an unknown reason.";
}

import { api } from "./api/daemon";
import type {
  TorboxUserDto,
  TorrentItemDto,
  TorrentsStateDto,
} from "./api/types";

/**
 * The Torrents pane's client-side store.
 *
 * The real work — TorBox calls, CDN downloads, encryption — happens in the
 * daemon's own pipeline, so this store is only a polling cache plus the small
 * amount of diffing the UI needs to fire "finished" / "failed" toasts even
 * when the pane is not on screen.
 *
 * Poll cadence is adaptive and stops entirely when there is nothing to watch:
 * - something active            → 3s
 * - pane open, nothing active   → 8s (so an add shows up promptly)
 * - pane closed, nothing active → stopped
 */

const ACTIVE_MS = 3000;
const OPEN_IDLE_MS = 8000;

const EMPTY: TorrentsStateDto = {
  has_key: false,
  plan: null,
  plan_label: null,
  email: null,
  max_concurrent: 2,
  items: [],
  library: [],
};

type Listener = (state: TorrentsStateDto) => void;

let cached: TorrentsStateDto | null = null;
let timer: number | null = null;
let paneOpen = false;
let inFlight = false;
const listeners = new Set<Listener>();

/** Ids seen active on the previous poll, so we can spot their finish/error. */
let previouslyActive = new Set<string>();

let onFinished: ((item: TorrentItemDto) => void) | null = null;
let onFailed: ((item: TorrentItemDto) => void) | null = null;

export function getTorrentsState(): TorrentsStateDto {
  return cached ?? EMPTY;
}

export function subscribeTorrents(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Called once by `App`: completion and failure toasts. */
export function setTorrentsHooks(hooks: {
  finished?: (item: TorrentItemDto) => void;
  failed?: (item: TorrentItemDto) => void;
}): void {
  onFinished = hooks.finished ?? null;
  onFailed = hooks.failed ?? null;
}

function emit(): void {
  const state = getTorrentsState();
  for (const fn of listeners) fn(state);
}

function hasActive(state: TorrentsStateDto): boolean {
  return state.items.some((i) =>
    ["queued", "sending", "caching", "ready", "downloading"].includes(i.status),
  );
}

function nextDelay(state: TorrentsStateDto): number | null {
  if (hasActive(state)) return ACTIVE_MS;
  if (paneOpen) return OPEN_IDLE_MS;
  return null; // nothing to watch and nobody looking: stop
}

function apply(next: TorrentsStateDto): TorrentsStateDto {
  const prev = cached;
  cached = next;
  detectTransitions(prev, next);
  emit();
  schedule();
  return next;
}

/** Fire hooks for items that finished or errored since the last poll. */
function detectTransitions(
  prev: TorrentsStateDto | null,
  next: TorrentsStateDto,
): void {
  const byId = new Map(next.items.map((i) => [i.id, i] as const));
  if (prev) {
    for (const id of previouslyActive) {
      const item = byId.get(id);
      if (!item) {
        // Gone from the queue: it either finished (filed into the library) or
        // was removed by the user. Only the former is a completion.
        const lib = next.library.find((e) => e.id === id);
        if (lib && onFinished) {
          onFinished({
            ...(prev.items.find((i) => i.id === id) ?? lib),
            status: "completed",
            name: lib.name,
          } as TorrentItemDto);
        }
        continue;
      }
      if (item.status === "error" && onFailed) onFailed(item);
    }
  }
  previouslyActive = new Set(
    next.items.filter((i) => i.status !== "error").map((i) => i.id),
  );
}

function schedule(): void {
  if (timer !== null) {
    window.clearTimeout(timer);
    timer = null;
  }
  const delay = nextDelay(getTorrentsState());
  if (delay === null) return;
  timer = window.setTimeout(() => void poll(), delay);
}

async function poll(): Promise<void> {
  if (inFlight) return;
  inFlight = true;
  try {
    const state = await api.torrentsState();
    if (state) apply(state);
  } catch {
    // A locked vault or a transient daemon blip: keep the last good state and
    // try again on the next tick rather than flashing an error.
    schedule();
  } finally {
    inFlight = false;
  }
}

/** Fetch once, now (used after a mutation or when the pane opens). */
export async function refreshTorrents(): Promise<TorrentsStateDto> {
  try {
    const state = await api.torrentsState();
    if (state) return apply(state);
  } catch {
    /* keep the last good state */
  }
  return getTorrentsState();
}

/** Tell the store whether the pane is on screen, so it can poll or stop. */
export function setTorrentsPaneOpen(open: boolean): void {
  paneOpen = open;
  if (open) {
    void poll();
  } else {
    schedule();
  }
}

/** Clear everything on lock; a fresh unlock reloads from the daemon. */
export function clearTorrentsStore(): void {
  if (timer !== null) window.clearTimeout(timer);
  timer = null;
  cached = null;
  previouslyActive = new Set();
}

// --- Mutations: each returns the daemon's new state and applies it at once. ---

export async function testTorboxKey(key: string): Promise<TorboxUserDto> {
  const info = await api.torrentsTestKey(key);
  if (!info) throw new Error("TorBox did not answer.");
  return info;
}

export async function saveTorboxKey(key: string): Promise<TorrentsStateDto> {
  return apply((await api.torrentsSetKey(key)) ?? getTorrentsState());
}

export async function refreshTorboxAccount(): Promise<TorrentsStateDto> {
  return apply((await api.torrentsRefresh()) ?? getTorrentsState());
}

export async function addTorrent(source: string): Promise<TorrentsStateDto> {
  return apply((await api.torrentsAdd(source)) ?? getTorrentsState());
}

export async function addTorrentFile(
  name: string,
  dataBase64: string,
): Promise<TorrentsStateDto> {
  return apply((await api.torrentsAddFile(name, dataBase64)) ?? getTorrentsState());
}

export async function pauseTorrent(id: string): Promise<TorrentsStateDto> {
  return apply((await api.torrentsPause(id)) ?? getTorrentsState());
}

export async function resumeTorrent(id: string): Promise<TorrentsStateDto> {
  return apply((await api.torrentsResume(id)) ?? getTorrentsState());
}

export async function retryTorrent(id: string): Promise<TorrentsStateDto> {
  return apply((await api.torrentsRetry(id)) ?? getTorrentsState());
}

export async function removeTorrent(id: string): Promise<TorrentsStateDto> {
  return apply((await api.torrentsRemove(id)) ?? getTorrentsState());
}

export async function setTorrentConcurrency(
  max: number,
): Promise<TorrentsStateDto> {
  return apply((await api.torrentsSetConcurrency(max)) ?? getTorrentsState());
}

export async function deleteLibraryEntry(
  id: string,
): Promise<TorrentsStateDto> {
  return apply((await api.torrentsDeleteLibrary(id)) ?? getTorrentsState());
}

/** Human wording for a pipeline state. */
export function torrentStatusLabel(item: TorrentItemDto): string {
  switch (item.status) {
    case "queued":
      return "Queued";
    case "sending":
      return "Sending to TorBox";
    case "caching": {
      // TorBox's own state explains a stall ("queued" = waiting for a slot
      // there), which otherwise looks identical to a hang.
      const remote = item.remote_state ? ` — ${item.remote_state}` : "";
      return item.cached
        ? "Cached — preparing"
        : `Caching on TorBox${remote} (${Math.round(item.progress * 100)}%)`;
    }
    case "ready":
      return "Ready — starting download";
    case "downloading": {
      // The transfer and the vault store are one status: the bytes can be
      // fully on disk while the file is still being encrypted into the vault,
      // which for a multi-GB file takes a while. Say so, or 100% looks stuck.
      if (item.dl_total > 0 && item.dl_done >= item.dl_total) {
        return "Sealing into the vault…";
      }
      const pct = item.dl_total
        ? Math.round((item.dl_done / item.dl_total) * 100)
        : 0;
      return `Downloading to vault (${pct}%)`;
    }
    case "completed":
      return "Completed";
    case "paused":
      return "Paused";
    case "error":
      return item.message ?? "Error";
  }
}

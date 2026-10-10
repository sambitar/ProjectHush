import { api } from "./api/daemon";
import type {
  ReadsFeedResponse,
  ReadsReaderDto,
  ReadsSourceDto,
  ReadsStateDto,
  ReadsStoryDetailDto,
  ReadsStoryDto,
} from "./api/types";

/** The built-in source every vault ships with: HN best of the week. */
export const HK_NEWS: ReadsSourceDto = {
  id: "hk-news",
  title: "HK News",
  url: "https://news.ycombinator.com/best",
  built_in: true,
};

/** Feed pages keep their fetch for this long; toggles clear the cache. */
const FEED_TTL_MS = 120_000;
/** Story detail (points and replies keep moving, but not mid-read). */
const DETAIL_TTL_MS = 180_000;

interface FeedEntry {
  at: number;
  resp: ReadsFeedResponse;
}

const feedCache = new Map<string, FeedEntry>();
const detailCache = new Map<string, { at: number; detail: ReadsStoryDetailDto }>();
let cachedState: ReadsStateDto | null = null;

/** Reset everything: on lock, on unlock-as-different-vault, on state change. */
export function clearReadsStore(): void {
  feedCache.clear();
  detailCache.clear();
  cachedState = null;
}

function feedKey(source: string, page: number): string {
  return `${source}:${page}`;
}

const EMPTY_STATE: ReadsStateDto = {
  sources: [],
  saved: [],
  hidden: [],
  read: [],
  history: [],
};

/** One feed page. Cached briefly; `refresh` bypasses and recaches. */
export async function loadFeed(
  source: string,
  page: number,
  refresh = false,
): Promise<ReadsFeedResponse> {
  const key = feedKey(source, page);
  const hit = feedCache.get(key);
  if (!refresh && hit && Date.now() - hit.at < FEED_TTL_MS) return hit.resp;
  const resp = (await api.readsFeed(source, page)) ?? { stories: [], done: true };
  feedCache.set(key, { at: Date.now(), resp });
  return resp;
}

/** The browsable sources: built-in first, then the user's feeds. */
export async function getSources(): Promise<ReadsSourceDto[]> {
  const sources = (await api.readsSources()) ?? [HK_NEWS];
  return sources.length ? sources : [HK_NEWS];
}

/**
 * Add an RSS feed; returns the new source list. Throws with a readable
 * message when the URL is refused or the feed is unreadable.
 */
export async function addSource(url: string): Promise<ReadsSourceDto[]> {
  const sources = await api.readsAddSource(url.trim());
  feedCache.clear();
  return sources ?? [];
}

export async function removeSource(id: string): Promise<ReadsSourceDto[]> {
  const sources = await api.readsRemoveSource(id);
  feedCache.clear();
  return sources ?? [];
}

export async function loadStory(id: string): Promise<ReadsStoryDetailDto> {
  const hit = detailCache.get(id);
  if (hit && Date.now() - hit.at < DETAIL_TTL_MS) return hit.detail;
  const detail = await api.readsStory(id);
  if (!detail) throw new Error("That story is unavailable right now.");
  detailCache.set(id, { at: Date.now(), detail });
  return detail;
}

/** Reader-mode extraction, daemon-cached; throws with a readable message. */
export async function loadReader(url: string): Promise<ReadsReaderDto> {
  const dto = await api.readsReader(url);
  if (!dto) throw new Error("The article could not be extracted.");
  return dto;
}

/** Personal state, cached until a toggle replaces it wholesale. */
export async function getState(): Promise<ReadsStateDto> {
  if (!cachedState) cachedState = (await api.readsState()) ?? EMPTY_STATE;
  return cachedState;
}

async function applyState(next: ReadsStateDto): Promise<ReadsStateDto> {
  cachedState = next;
  // Feeds embed saved/read annotations: drop them so the next paint refetches.
  feedCache.clear();
  return next;
}

export async function toggleSaved(story: ReadsStoryDto): Promise<ReadsStateDto> {
  return applyState((await api.readsToggleSaved(story)) ?? EMPTY_STATE);
}

export async function toggleHidden(id: string): Promise<ReadsStateDto> {
  return applyState((await api.readsToggleHidden(id)) ?? EMPTY_STATE);
}

export async function markRead(id: string, title: string): Promise<ReadsStateDto> {
  return applyState((await api.readsMarkRead(id, title)) ?? EMPTY_STATE);
}

/** "3 hours ago" for list rows; unix seconds in, human words out. */
export function timeAgo(unixSecs: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - unixSecs);
  if (secs < 60) return "just now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} day${days === 1 ? "" : "s"} ago`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months} mo ago`;
  return `${Math.floor(months / 12)} y ago`;
}

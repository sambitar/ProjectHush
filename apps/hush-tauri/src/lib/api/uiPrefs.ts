import { api } from "./daemon";

export interface UiPrefsDto {
  theme: "light" | "dark" | "system";
  avatars: Record<string, string>;
  muted: string[];
  blocked: string[];
  nick_overrides: Record<string, string>;
  /** 0 = idle auto-lock off (daemon); 5–1440 minutes otherwise */
  auto_lock_minutes: number;
}

const defaultPrefs = (): UiPrefsDto => ({
  theme: "system",
  avatars: {},
  muted: [],
  blocked: [],
  nick_overrides: {},
  auto_lock_minutes: 15,
});

let cache: UiPrefsDto | null = null;

export async function loadUiPrefs(): Promise<UiPrefsDto> {
  try {
    const remote = await api.uiPrefsGet();
    cache = remote ? normalize(remote) : defaultPrefs();
  } catch {
    cache = defaultPrefs();
  }
  return cache;
}

export function uiPrefsSnapshot(): UiPrefsDto {
  return cache ? { ...cache, avatars: { ...cache.avatars }, nick_overrides: { ...cache.nick_overrides } } : defaultPrefs();
}

export async function persistUiPrefs(next: UiPrefsDto): Promise<void> {
  cache = next;
  await api.uiPrefsPut(next);
}

export async function patchUiPrefs(
  patch: Partial<UiPrefsDto>,
): Promise<UiPrefsDto> {
  const base = cache ?? (await loadUiPrefs());
  const merged: UiPrefsDto = {
    ...base,
    ...patch,
    avatars: patch.avatars ?? base.avatars,
    nick_overrides: patch.nick_overrides ?? base.nick_overrides,
    muted: patch.muted ?? base.muted,
    blocked: patch.blocked ?? base.blocked,
  };
  await persistUiPrefs(merged);
  return merged;
}

export async function clearContactUiPrefs(nick: string): Promise<void> {
  const p = uiPrefsSnapshot();
  const nick_overrides = { ...p.nick_overrides };
  delete nick_overrides[nick];
  await patchUiPrefs({
    muted: p.muted.filter((n) => n !== nick),
    blocked: p.blocked.filter((n) => n !== nick),
    nick_overrides,
  });
}

function normalize(raw: UiPrefsDto): UiPrefsDto {
  const theme =
    raw.theme === "light" || raw.theme === "dark" || raw.theme === "system"
      ? raw.theme
      : "system";
  const auto_lock_minutes =
    typeof raw.auto_lock_minutes === "number" ? raw.auto_lock_minutes : 15;
  return {
    theme,
    avatars: raw.avatars ?? {},
    muted: raw.muted ?? [],
    blocked: raw.blocked ?? [],
    nick_overrides: raw.nick_overrides ?? {},
    auto_lock_minutes,
  };
}

export function getAutoLockMinutes(): number {
  return uiPrefsSnapshot().auto_lock_minutes;
}

export function getAutoLockMs(): number {
  const m = getAutoLockMinutes();
  if (m <= 0) return Number.POSITIVE_INFINITY;
  return m * 60 * 1000;
}

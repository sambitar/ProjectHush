import { api } from "./daemon";

export interface UiPrefsDto {
  theme: "light" | "dark" | "system";
  avatars: Record<string, string>;
  muted: string[];
  blocked: string[];
  nick_overrides: Record<string, string>;
}

const defaultPrefs = (): UiPrefsDto => ({
  theme: "system",
  avatars: {},
  muted: [],
  blocked: [],
  nick_overrides: {},
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

function normalize(raw: UiPrefsDto): UiPrefsDto {
  const theme =
    raw.theme === "light" || raw.theme === "dark" || raw.theme === "system"
      ? raw.theme
      : "system";
  return {
    theme,
    avatars: raw.avatars ?? {},
    muted: raw.muted ?? [],
    blocked: raw.blocked ?? [],
    nick_overrides: raw.nick_overrides ?? {},
  };
}

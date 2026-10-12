import { api } from "./daemon";

export interface UiPrefsDto {
  theme: "light" | "dark" | "system";
  avatars: Record<string, string>;
  muted: string[];
  blocked: string[];
  nick_overrides: Record<string, string>;
  /** 0 = idle auto-lock off (daemon); 5–1440 minutes otherwise */
  auto_lock_minutes: number;
  /** Mail record ids the user has opened. */
  mail_read: string[];
  mail_starred: string[];
  mail_archived: string[];
  mail_trashed: string[];
  /** Play a sound when a chat message arrives (global switch). */
  chat_sound: boolean;
  /** Contacts whose arrival sound is off, from the chat's own menu. */
  chat_sound_muted: string[];
  /** When true, this vault stops heartbeating so every contact sees it offline. */
  appear_offline: boolean;
}

const defaultPrefs = (): UiPrefsDto => ({
  theme: "system",
  avatars: {},
  muted: [],
  blocked: [],
  nick_overrides: {},
  auto_lock_minutes: 15,
  mail_read: [],
  mail_starred: [],
  mail_archived: [],
  mail_trashed: [],
  chat_sound: true,
  chat_sound_muted: [],
  appear_offline: false,
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
    mail_read: patch.mail_read ?? base.mail_read,
    mail_starred: patch.mail_starred ?? base.mail_starred,
    mail_archived: patch.mail_archived ?? base.mail_archived,
    mail_trashed: patch.mail_trashed ?? base.mail_trashed,
    chat_sound: patch.chat_sound ?? base.chat_sound,
    chat_sound_muted: patch.chat_sound_muted ?? base.chat_sound_muted,
    appear_offline: patch.appear_offline ?? base.appear_offline,
  };
  await persistUiPrefs(merged);
  return merged;
}

/**
 * Flip one mail flag on or off.
 *
 * The flags are lists in `ui-prefs.json`, so this reads the current list, adds
 * or removes the id, and writes it back. Callers pass the id they just acted on.
 */
export async function setMailFlag(
  flag: "mail_read" | "mail_starred" | "mail_archived" | "mail_trashed",
  id: string,
  on: boolean,
): Promise<void> {
  const p = uiPrefsSnapshot();
  const next = on ? Array.from(new Set([...p[flag], id])) : p[flag].filter((x) => x !== id);
  await patchUiPrefs({ [flag]: next });
}

/** Every mail flag in one snapshot, for deriving folder membership. */
export function mailFlags(): {
  read: Set<string>;
  starred: Set<string>;
  archived: Set<string>;
  trashed: Set<string>;
} {
  const p = uiPrefsSnapshot();
  return {
    read: new Set(p.mail_read),
    starred: new Set(p.mail_starred),
    archived: new Set(p.mail_archived),
    trashed: new Set(p.mail_trashed),
  };
}

export async function clearContactUiPrefs(nick: string): Promise<void> {
  const p = uiPrefsSnapshot();
  const nick_overrides = { ...p.nick_overrides };
  delete nick_overrides[nick];
  await patchUiPrefs({
    muted: p.muted.filter((n) => n !== nick),
    blocked: p.blocked.filter((n) => n !== nick),
    chat_sound_muted: p.chat_sound_muted.filter((n) => n !== nick),
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
    mail_read: raw.mail_read ?? [],
    mail_starred: raw.mail_starred ?? [],
    mail_archived: raw.mail_archived ?? [],
    mail_trashed: raw.mail_trashed ?? [],
    chat_sound: raw.chat_sound ?? true,
    chat_sound_muted: raw.chat_sound_muted ?? [],
    appear_offline: raw.appear_offline ?? false,
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

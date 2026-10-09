/** Session-only UI flags; contact prefs live in daemon ui-prefs.json (DR-05). */
import { loadUiPrefs, patchUiPrefs, uiPrefsSnapshot } from "./uiPrefs";

export interface MockSettings {
  autoLockMinutes: number;
  backupDone: boolean;
  recoveryRevealed: boolean;
}

let settings: MockSettings = {
  autoLockMinutes: 15,
  backupDone: false,
  recoveryRevealed: false,
};

export function getMockSettings(): MockSettings {
  return { ...settings };
}

export function setAutoLockMinutes(m: number): void {
  settings = { ...settings, autoLockMinutes: m };
}

export function setBackupDone(v: boolean): void {
  settings = { ...settings, backupDone: v };
}

export function setRecoveryRevealed(v: boolean): void {
  settings = { ...settings, recoveryRevealed: v };
}

export function isMuted(nick: string): boolean {
  return uiPrefsSnapshot().muted.includes(nick);
}

export async function setMuted(nick: string, v: boolean): Promise<void> {
  const p = uiPrefsSnapshot();
  const muted = new Set(p.muted);
  if (v) muted.add(nick);
  else muted.delete(nick);
  await patchUiPrefs({ muted: [...muted] });
}

/** Whether this contact's chat arrival sound is off (the chat menu's toggle). */
export function isChatSoundMuted(nick: string): boolean {
  return uiPrefsSnapshot().chat_sound_muted.includes(nick);
}

export async function setChatSoundMuted(
  nick: string,
  v: boolean,
): Promise<void> {
  const p = uiPrefsSnapshot();
  const muted = new Set(p.chat_sound_muted);
  if (v) muted.add(nick);
  else muted.delete(nick);
  await patchUiPrefs({ chat_sound_muted: [...muted] });
}

/** The global chat-sound switch, from Settings → Notifications. */
export function chatSoundEnabled(): boolean {
  return uiPrefsSnapshot().chat_sound;
}

export async function setChatSoundEnabled(v: boolean): Promise<void> {
  await patchUiPrefs({ chat_sound: v });
}

export function isBlocked(nick: string): boolean {
  return uiPrefsSnapshot().blocked.includes(nick);
}

export async function setBlocked(nick: string, v: boolean): Promise<void> {
  const p = uiPrefsSnapshot();
  const blocked = new Set(p.blocked);
  if (v) blocked.add(nick);
  else blocked.delete(nick);
  await patchUiPrefs({ blocked: [...blocked] });
}

export function displayNick(nick: string): string {
  return uiPrefsSnapshot().nick_overrides[nick] ?? nick;
}

export async function setNickOverride(nick: string, label: string): Promise<void> {
  const p = uiPrefsSnapshot();
  const nick_overrides = { ...p.nick_overrides, [nick]: label };
  await patchUiPrefs({ nick_overrides });
}

export function clearMockLocal(): void {
  settings = { ...settings, recoveryRevealed: false };
}

export async function refreshContactPrefs(): Promise<void> {
  await loadUiPrefs();
}

export function mockRecoveryCode(): string {
  return "hush-demo-recovery-0000";
}

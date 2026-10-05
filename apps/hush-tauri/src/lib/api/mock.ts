/** Mock-only settings until daemon exposes these endpoints. */
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

const muted = new Set<string>();
const blocked = new Set<string>();
const nickOverrides = new Map<string, string>();

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
  return muted.has(nick);
}

export function setMuted(nick: string, v: boolean): void {
  if (v) muted.add(nick);
  else muted.delete(nick);
}

export function isBlocked(nick: string): boolean {
  return blocked.has(nick);
}

export function setBlocked(nick: string, v: boolean): void {
  if (v) blocked.add(nick);
  else blocked.delete(nick);
}

export function displayNick(nick: string): string {
  return nickOverrides.get(nick) ?? nick;
}

export function setNickOverride(nick: string, label: string): void {
  nickOverrides.set(nick, label);
}

export function clearMockLocal(): void {
  muted.clear();
  blocked.clear();
  nickOverrides.clear();
  settings = { autoLockMinutes: 15, backupDone: false, recoveryRevealed: false };
}

export function mockRecoveryCode(): string {
  return "hush-demo-recovery-0000";
}

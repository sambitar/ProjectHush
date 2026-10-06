import { loadUiPrefs, patchUiPrefs, uiPrefsSnapshot } from "./api/uiPrefs";

export type ThemeMode = "light" | "dark" | "system";

export async function initThemeFromDaemon(): Promise<void> {
  await loadUiPrefs();
  applyTheme(getThemeMode());
}

export function getThemeMode(): ThemeMode {
  return uiPrefsSnapshot().theme;
}

export async function setThemeMode(mode: ThemeMode): Promise<void> {
  await patchUiPrefs({ theme: mode });
  applyTheme(mode);
}

export function applyTheme(mode: ThemeMode): void {
  const dark =
    mode === "dark" ||
    (mode === "system" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.documentElement.dataset.themeMode = mode;
}

export async function cycleTheme(): Promise<ThemeMode> {
  const order: ThemeMode[] = ["system", "light", "dark"];
  const i = order.indexOf(getThemeMode());
  const next = order[(i + 1) % order.length];
  await setThemeMode(next);
  return next;
}

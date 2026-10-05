export type ThemeMode = "light" | "dark" | "system";

const KEY = "hush_theme";

export function getThemeMode(): ThemeMode {
  const v = localStorage.getItem(KEY);
  if (v === "light" || v === "dark" || v === "system") return v;
  return "system";
}

export function setThemeMode(mode: ThemeMode): void {
  localStorage.setItem(KEY, mode);
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

export function cycleTheme(): ThemeMode {
  const order: ThemeMode[] = ["system", "light", "dark"];
  const i = order.indexOf(getThemeMode());
  const next = order[(i + 1) % order.length];
  setThemeMode(next);
  return next;
}

let reduceMotion = false;

export function initPerfGuard(): void {
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    reduceMotion = true;
    document.documentElement.dataset.reduceMotion = "true";
  }
}

export function shouldReduceMotion(): boolean {
  return reduceMotion;
}

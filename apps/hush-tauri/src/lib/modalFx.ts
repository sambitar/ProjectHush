/**
 * iPhone-style open/close transitions for the app's dialogs.
 *
 * On iOS, launching an app springs its window up out of the icon that launched
 * it while the home screen dims and pulls focus behind; closing runs the same
 * path in reverse, back into the icon. Here the dialog card is the window and
 * the "icon" is the control the user was pressing when the dialog mounted: the
 * zoom starts from — or collapses back into — that point. A dialog opened
 * without a press (an async prompt like the update notice) zooms from the
 * center of the screen instead.
 *
 * The app rebuilds its whole tree on every state change (`App.render()`), and
 * a background refresh can rebuild an open dialog the user never touched.
 * Those remounts must repaint instantly, so `enterModal` is told whether a
 * mount is a fresh open or a silent rebuild. Closing is a full re-render too
 * (a flag clears, `render()` wipes the tree), so the wiped backdrop is handed
 * to `leaveModal`, which reattaches it over the fresh UI, plays the reverse
 * zoom, and removes it. The launch point travels across rebuilds on inline
 * custom properties (`--fx-ox` / `--fx-oy`) that the caller copies from the
 * wiped element to its replacement, so a dialog closed after any number of
 * refreshes still zooms toward where it was opened from.
 */

/** The last pointerdown anywhere, with when it happened. */
let lastPress: { x: number; y: number; at: number } | null = null;

let tracking = false;

function trackPress(e: PointerEvent | MouseEvent): void {
  lastPress = { x: e.clientX, y: e.clientY, at: performance.now() };
}

function ensureTracking(): void {
  if (tracking) return;
  tracking = true;
  // Capture phase, so the press is recorded even if a handler stops it.
  if (window.PointerEvent) {
    document.addEventListener("pointerdown", trackPress, {
      capture: true,
      passive: true,
    });
  } else {
    document.addEventListener("mousedown", trackPress, true);
  }
}

function reducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Stamp the dialog's zoom origin: the control pressed within the last second
 * and a half (a real launch), or the screen center (an async prompt). Call on
 * a fresh mount; on a rebuild the caller copies the old element's stamp across
 * instead, so the origin stays the control the dialog was first opened from.
 */
export function stampModalOrigin(back: HTMLElement): void {
  ensureTracking();
  const press = lastPress;
  const fresh = press !== null && performance.now() - press.at < 1500;
  const x = fresh && press ? press.x : window.innerWidth / 2;
  const y = fresh && press ? press.y : window.innerHeight / 2;
  back.style.setProperty("--fx-ox", `${Math.round(x)}px`);
  back.style.setProperty("--fx-oy", `${Math.round(y)}px`);
}

/**
 * Play the zoom-in on a freshly mounted dialog. `animate` is false for a
 * rebuild of an already-open dialog, which must repaint without moving.
 */
export function enterModal(back: HTMLElement, animate: boolean): void {
  if (!animate || reducedMotion()) return;
  back.classList.add("fx-enter");
  // Drop the class once settled so the element is left in its natural state;
  // the timeout covers a missed animationend (a hidden webview can drop it).
  const done = (): void => back.classList.remove("fx-enter");
  back.addEventListener("animationend", done, { once: true });
  window.setTimeout(done, 600);
}

/**
 * Reverse zoom for a dialog whose render pass just dropped it. The element was
 * wiped with the rest of the tree; reattach it over the fresh UI, collapse it
 * back into its launch point, then take it away. Reduced motion skips the
 * theater — the wipe already did the job.
 */
export function leaveModal(back: HTMLElement): void {
  if (reducedMotion()) return;
  document.body.appendChild(back);
  back.classList.add("fx-leave");
  const done = (): void => back.remove();
  back.addEventListener("animationend", done, { once: true });
  window.setTimeout(done, 500);
}

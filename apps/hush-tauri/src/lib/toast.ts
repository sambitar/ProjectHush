/**
 * Visible, transient notifications ("toasts").
 *
 * The app already has a screen-reader live region (`App.announce`), but that
 * is deliberately silent on screen. Torrents need something a sighted user
 * notices without watching the pane — a download finishing while they are in
 * Chats, an API key being rejected — so this renders a small stack in the
 * corner and also carries `role="status"` for assistive tech.
 *
 * Kept deliberately tiny and dependency-free: one container, one factory.
 */

export type ToastKind = "info" | "success" | "error";

export interface ToastOptions {
  kind?: ToastKind;
  /** Auto-dismiss after this many ms; `0` keeps it until dismissed. */
  timeoutMs?: number;
  /** Optional single action button (e.g. "Retry"). */
  action?: { label: string; onClick: () => void };
}

const DEFAULT_TIMEOUT_MS = 6000;

let container: HTMLElement | null = null;

function ensureContainer(): HTMLElement {
  if (container && container.isConnected) return container;
  container = document.createElement("div");
  container.className = "toast-stack";
  container.setAttribute("aria-live", "polite");
  container.setAttribute("aria-relevant", "additions");
  document.body.appendChild(container);
  return container;
}

/**
 * Show a toast. Returns a function that dismisses it early, which is useful
 * when a later event supersedes the message (e.g. a retry succeeding).
 */
export function showToast(message: string, opts: ToastOptions = {}): () => void {
  const host = ensureContainer();
  const el = document.createElement("div");
  el.className = `toast toast-${opts.kind ?? "info"}`;
  el.setAttribute("role", "status");

  const text = document.createElement("span");
  text.className = "toast-text";
  text.textContent = message;
  el.appendChild(text);

  let timer: number | null = null;
  const dismiss = () => {
    if (timer !== null) window.clearTimeout(timer);
    el.classList.add("toast-leaving");
    window.setTimeout(() => el.remove(), 200);
  };

  if (opts.action) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "toast-action pressable";
    btn.textContent = opts.action.label;
    btn.onclick = () => {
      opts.action!.onClick();
      dismiss();
    };
    el.appendChild(btn);
  }

  const close = document.createElement("button");
  close.type = "button";
  close.className = "toast-close pressable";
  close.setAttribute("aria-label", "Dismiss");
  close.textContent = "×";
  close.onclick = dismiss;
  el.appendChild(close);

  host.appendChild(el);
  const timeout = opts.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  if (timeout > 0) timer = window.setTimeout(dismiss, timeout);
  return dismiss;
}

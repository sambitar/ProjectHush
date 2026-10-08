export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
}

/**
 * A full date and time, e.g. "Oct 7, 2026, 3:24 PM".
 *
 * Mail reads like mail: the header shows the moment a message was sent, not
 * "1h ago". `formatRelative` is still the right label for a list, where the
 * exact minute does not matter, but a message header is a record and gets the
 * absolute stamp. Locale is left to the platform so it matches `formatTime`.
 */
export function formatAbsolute(ts: number): string {
  return new Date(ts).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

export function formatRelative(ts: number): string {
  const diff = Date.now() - ts;
  const m = Math.floor(diff / 60000);
  if (m < 1) return "Just now";
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

/**
 * How long until `ts` — a deadline, not a timestamp.
 *
 * `formatRelative` is past-tense, so it reports any future instant as "Just
 * now". On a link that is still live that reads as "already expired", which is
 * the opposite of the truth, so deadlines get their own formatter.
 *
 * Rounded up, not down, for the same reason the daemon's notification rounds up:
 * a seven-day link must read "in 7 days" the moment it is created rather than
 * "in 6 days", which would understate how long the sender has to pass it on.
 */
export function formatUntil(ts: number): string {
  const diff = ts - Date.now();
  if (diff <= 0) return "already";
  const m = Math.ceil(diff / 60000);
  if (m < 1) return "in under a minute";
  if (m < 60) return `in ${m} minute${m === 1 ? "" : "s"}`;
  const h = Math.ceil(diff / 3600000);
  if (h < 24) return `in ${h} hour${h === 1 ? "" : "s"}`;
  const d = Math.ceil(diff / 86400000);
  return `in ${d} day${d === 1 ? "" : "s"}`;
}

export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => {
      const s = r.result as string;
      resolve(s.slice(s.indexOf(",") + 1));
    };
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });
}

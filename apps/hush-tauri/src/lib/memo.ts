/**
 * A one-argument memo with a bound.
 *
 * Built for derived values that are expensive to compute and cheap to keep.
 * The first caller is `htmlToText` in `richText.ts`: flattening a mail body
 * costs a `DOMParser` parse, a tree walk, a serialize, a second parse and two
 * sweeps, and the mail list asks for the same body's text twice per row on
 * every fill. The body does not change, so the answer should be computed once.
 *
 * Keys are held by reference, so a caller passing a string it already owns pays
 * only for the value, not a copy of the key. Eviction is least-recently-used:
 * the entry that is on screen is the one worth keeping, and the bound is what
 * keeps this from being a leak.
 *
 * One argument only. A memo keyed on several arguments needs a key function and
 * a decision about what equality means; nothing here has needed that yet, and
 * guessing at it now would be a framework rather than a helper.
 */
export function memoizeLru<T>(compute: (key: string) => T, max: number): (key: string) => T {
  // A bound below one would evict every entry the moment it was written, which
  // is a cache that only costs: keep one instead.
  const limit = Math.max(1, Math.floor(max));
  const cache = new Map<string, T>();
  return (key: string): T => {
    // `has` rather than an `undefined` check on `get`: `T` may legitimately be
    // `undefined`, and a miss must not be confused with a cached one.
    if (cache.has(key)) {
      const hit = cache.get(key) as T;
      // Re-insert so the most recently used key is the last to be evicted.
      cache.delete(key);
      cache.set(key, hit);
      return hit;
    }
    const value = compute(key);
    if (cache.size >= limit) {
      const oldest = cache.keys().next().value;
      if (oldest !== undefined) cache.delete(oldest);
    }
    cache.set(key, value);
    return value;
  };
}

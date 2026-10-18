/**
 * Keyed regions: keeping the parts of the shell that must survive a render.
 *
 * The shell used to be thrown away whole on every render (`root.innerHTML =
 * ""`). That is fine for most of it, and wrong for the message panes. An
 * element that leaves the document loses three things that cannot be carried
 * back cheaply:
 *
 * - its `scrollTop` (measured in a real engine, not assumed: a detached
 *   scroll box reports 0, and re-attaching it leaves it at 0);
 * - the hover and text-selection state of the rows inside it, because the rows
 *   themselves are new nodes;
 * - the row heights the `content-visibility` containment rule remembered, so
 *   the pane's height estimate restarts from the placeholder on every rebuild.
 *
 * So the shell is now a set of keyed regions. A region whose key is unchanged
 * is left exactly where it is — never removed and re-inserted, because that is
 * what detaches it — and is brought up to date in place by its `update`.
 *
 * `planRegionSync` is the whole decision and is pure, so it can be tested
 * without a DOM. `syncRegions` executes it.
 */

export interface RegionSpec {
  /**
   * Stable identity for this region, or `null` for one that is rebuilt on every
   * render. `null` is the honest default: a region that is rebuilt needs no
   * reasoning about what it draws from, so nothing can go stale. A region earns
   * a key only when something inside it must not be detached.
   */
  key: string | null;
  /** Build the region from scratch. */
  build: () => HTMLElement;
  /**
   * Bring an existing same-key element up to date without detaching it. Must
   * not reorder the parent's children: the executor is walking them.
   */
  update?: (el: HTMLElement) => void;
  /**
   * A signature of everything `build` draws, for an `update` that costs more
   * than the rebuild it replaces.
   *
   * The key decides when the element may be replaced; this decides whether the
   * update has anything to do. An update runs on every render while its region
   * is kept — which is the point, since it is the cheap path — but some of them
   * are not cheap: redrawing a story list, re-sanitizing an article, rebuilding
   * a comment tree. Those would run on every poll tick to arrive at the same
   * DOM. With a shape, the executor compares it against what the element was
   * last drawn from and skips the update when nothing it draws has moved.
   *
   * So it is not a second key: a key that moved replaces the element, and the
   * reason to keep the element — a scroll position, a caret, a remembered row
   * height — is a reason to keep it across changes the update can handle.
   */
  shape?: () => string;
}

/**
 * What to do with one slot, in order. `live` indexes the parent's children as
 * they were when the sync started; `null` means there was no child left to
 * consume, so the region is purely an insertion.
 */
export type RegionOp =
  | { kind: "keep"; live: number }
  | { kind: "replace"; live: number | null }
  | { kind: "drop"; live: number };

/**
 * Pair the wanted regions against the live children.
 *
 * Positional, not keyed by search: the shell's region order is fixed per rail,
 * so a child is kept only when the wanted region at that position carries the
 * same key. A key that moved elsewhere in the list is rebuilt rather than moved
 * — moving an element detaches it, which is the one thing this exists to avoid,
 * and a reorder means the pane was rebuilt anyway.
 *
 * A `null` want-key never matches, including another `null`: those regions are
 * always replaced.
 */
export function planRegionSync(
  liveKeys: readonly (string | null)[],
  wantKeys: readonly (string | null)[],
): RegionOp[] {
  const ops: RegionOp[] = [];
  let cursor = 0;
  for (const want of wantKeys) {
    const live = cursor < liveKeys.length ? liveKeys[cursor] : null;
    if (want !== null && live === want) {
      ops.push({ kind: "keep", live: cursor });
      cursor += 1;
      continue;
    }
    ops.push({ kind: "replace", live: cursor < liveKeys.length ? cursor : null });
    if (cursor < liveKeys.length) cursor += 1;
  }
  for (let i = cursor; i < liveKeys.length; i += 1) ops.push({ kind: "drop", live: i });
  return ops;
}

/**
 * Whether a kept region's update has anything to do.
 *
 * A region with no shape is always updated: running the update is the point of
 * keeping the element, and it is the cheap path by definition. A region with one
 * is skipped when the element was already drawn from that shape — which is what
 * keeps an expensive update (a redrawn story list, a re-sanitized article) from
 * running on every poll tick to arrive at the same DOM.
 *
 * `drawn` is what the element carries from the render that last drew it, so an
 * element from before the shape existed reports `undefined` and is redrawn once.
 */
export function shapeMoved(
  shape: string | undefined,
  drawn: string | undefined,
): boolean {
  return shape === undefined || shape !== drawn;
}

/**
 * One dialog as the sync sees it: what it is, and what it draws.
 */
export interface ModalSlot {
  /** Stable identity for the dialog: `addFriend`, `profile`, `zinesShare`. */
  key: string;
  /**
   * A signature of everything the dialog's builder draws — the same idea as a
   * region's shape, with more riding on it. A dialog is not a pane that can be
   * redrawn in place: it holds fields, the caret in one of them, and the focus
   * ring on the control the reader was about to press. Rebuilding one to arrive
   * at the same DOM throws all three away, and the reader sees it happen.
   *
   * So a dialog whose key *and* shape are unchanged is not touched at all.
   */
  shape: string;
}

/** What to do with one dialog, in order. See `planModalSync`. */
export type ModalOp =
  | { kind: "keep"; live: number }
  | { kind: "rebuild"; live: number | null }
  | { kind: "drop"; live: number };

/**
 * Pair the dialogs that are up against the ones this pass wants.
 *
 * Keyed, not positional like `planRegionSync`, because the dialog list is
 * sparse and it shifts: the shell's regions are the same slots in the same
 * order every render, while closing the first of three dialogs moves the other
 * two up. Pairing by position would rebuild them — a visible flash and a lost
 * caret — for what is really just one dialog leaving.
 *
 * A same-key slot is kept when it draws the same thing and still sits after the
 * previous kept slot; anything else is rebuilt, which is what the caller needs
 * to know when the element has to be removed and replaced. A kept element is
 * never moved — moving detaches it, the one thing this exists to avoid — so a
 * want that would reorder the stack rebuilds rather than reorders. The list is
 * written in one fixed order per render, so in practice only a close ever
 * changes it, and a close only ever drops.
 *
 * A key that is not among the live dialogs is a fresh open: `live` is null and
 * the caller inserts it after the previous slot. Every live dialog the wants do
 * not account for is dropped, in order.
 */
export function planModalSync(
  live: readonly ModalSlot[],
  want: readonly ModalSlot[],
): ModalOp[] {
  const used = live.map(() => false);
  const ops: ModalOp[] = [];
  let placed = -1;
  for (const w of want) {
    const at = live.findIndex((l, i) => !used[i] && l.key === w.key);
    if (at >= 0 && at > placed && live[at].shape === w.shape) {
      used[at] = true;
      placed = at;
      ops.push({ kind: "keep", live: at });
      continue;
    }
    if (at >= 0) used[at] = true;
    ops.push({ kind: "rebuild", live: at >= 0 ? at : null });
  }
  for (let i = 0; i < live.length; i += 1) {
    if (!used[i]) ops.push({ kind: "drop", live: i });
  }
  return ops;
}

/** Make `parent`'s children exactly the regions, reusing the ones that match. */
export function syncRegions(parent: HTMLElement, specs: readonly RegionSpec[]): void {
  const live = Array.from(parent.children) as HTMLElement[];
  const ops = planRegionSync(
    live.map((el) => el.dataset.regionKey ?? null),
    specs.map((spec) => spec.key),
  );
  let spec = 0;
  for (const op of ops) {
    if (op.kind === "keep") {
      const want = specs[spec];
      const el = live[op.live];
      // Read before the update runs: the shape has to describe the state the
      // element was drawn from, and an update is allowed to be the thing that
      // brings the element up to the state the next shape will describe.
      const shape = want.shape?.();
      if (shapeMoved(shape, el.dataset.regionShape)) {
        if (shape !== undefined) el.dataset.regionShape = shape;
        want.update?.(el);
      }
      spec += 1;
      continue;
    }
    if (op.kind === "replace") {
      const want = specs[spec];
      const shape = want.shape?.();
      const fresh = want.build();
      if (want.key !== null) fresh.dataset.regionKey = want.key;
      if (shape !== undefined) fresh.dataset.regionShape = shape;
      // Insert before the child this slot is replacing, so the regions that
      // survive keep their relative order and are never moved.
      const at = op.live === null ? null : live[op.live];
      parent.insertBefore(fresh, at);
      if (at) parent.removeChild(at);
      spec += 1;
      continue;
    }
    parent.removeChild(live[op.live]);
  }
}

/**
 * Whether a region can be brought up to date in place, without a full sync.
 *
 * False is the safe answer and the default one: an unkeyed region is rebuilt on
 * every render, so there is nothing to update — and a keyed region that carries
 * no `update` is one nothing here knows how to bring forward, so the caller
 * rebuilds instead of leaving it as it was. Only a keyed region with an update
 * is a promise that its contents can be redrawn under it.
 */
export function canUpdateInPlace(
  spec: RegionSpec,
): spec is RegionSpec & { key: string; update: (el: HTMLElement) => void } {
  return spec.key !== null && spec.update !== undefined;
}

/**
 * The live element for a keyed region among `parent`'s children, if it is there.
 *
 * `syncRegions` stamps the key on each element it builds, so a caller that has
 * one region to bring up to date — a background delta that reaches exactly one
 * pane — can find it without depending on its position or on a class name that
 * the region's own builder is free to change.
 */
export function findRegion(parent: HTMLElement, key: string): HTMLElement | null {
  for (const el of Array.from(parent.children) as HTMLElement[]) {
    if (el.dataset.regionKey === key) return el;
  }
  return null;
}

/**
 * Bring one keyed region up to date where it stands, without a full sync.
 *
 * For the deltas that reach exactly one pane: a caller that knows which region
 * its change lands in paints that region and nothing else. It is the same
 * decision `syncRegions` makes for a kept region, in the same order — the key
 * must be among `parent`'s children, the region must know how to bring itself
 * forward, and its shape decides whether the update has anything to do — so a
 * region kept by a render and a region patched by a delta cannot disagree.
 *
 * `parent` is the element whose *direct* children are the regions: the shell,
 * not the root the shell hangs in. A region one level further down is not found,
 * and finding nothing is the safe answer — the caller rebuilds, which paints the
 * delta anyway.
 *
 * Returns false when the region cannot be brought forward — it is not on screen,
 * or it carries no update — which is the caller's cue to rebuild the shell
 * rather than leave the delta unpainted.
 */
export function updateRegionInPlace(parent: HTMLElement, spec: RegionSpec): boolean {
  if (!canUpdateInPlace(spec)) return false;
  const el = findRegion(parent, spec.key);
  if (!el) return false;
  const shape = spec.shape?.();
  if (shapeMoved(shape, el.dataset.regionShape)) {
    if (shape !== undefined) el.dataset.regionShape = shape;
    spec.update(el);
  }
  return true;
}

/**
 * Rebuild the parts of a pane around one element that must not be detached.
 *
 * `anchor` stays exactly where it is; everything before it is replaced by
 * `before` (in order) and everything after it by `after`. This is how the
 * message panes work: their header, roster, tabs, banners and composer are
 * rebuilt on every render, while the scroll box between them is left alone so
 * the reader keeps their place and the rows keep their remembered heights.
 */
export function rebuildAround(
  pane: HTMLElement,
  anchor: HTMLElement,
  before: readonly (HTMLElement | null)[],
  after: readonly (HTMLElement | null)[],
): void {
  let node: ChildNode | null = pane.firstChild;
  while (node && node !== anchor) {
    const next = node.nextSibling;
    pane.removeChild(node);
    node = next;
  }
  for (const el of before) if (el) pane.insertBefore(el, anchor);
  node = anchor.nextSibling;
  while (node) {
    const next = node.nextSibling;
    pane.removeChild(node);
    node = next;
  }
  for (const el of after) if (el) pane.appendChild(el);
}

/**
 * Rebuild a pane's parts around one element that must stay where it is, using a
 * fresh pane drawn around its own counterpart of that element.
 *
 * This is `rebuildAround` for the usual case: the way to bring a pane up to date
 * is to draw a whole new one, but one element in it — a scroll box the reader has
 * scrolled — has to survive. `selector` marks that element in `fresh`; everything
 * before it and everything after it is moved onto `pane`, and `anchor` itself is
 * left untouched.
 *
 * Returns false when `fresh` does not hold the element, or holds it somewhere
 * other than at the top level, so the caller can replace the pane wholesale
 * instead of rebuilding it around a seam that is not there.
 */
export function transplantAround(
  pane: HTMLElement,
  anchor: HTMLElement,
  fresh: HTMLElement,
  selector: string,
): boolean {
  const seam = fresh.querySelector<HTMLElement>(selector);
  if (!seam || seam.parentElement !== fresh) return false;
  const before: HTMLElement[] = [];
  const after: HTMLElement[] = [];
  let node: ChildNode | null = fresh.firstChild;
  while (node && node !== seam) {
    const next = node.nextSibling;
    if (node instanceof HTMLElement) before.push(node);
    fresh.removeChild(node);
    node = next;
  }
  node = seam.nextSibling;
  while (node) {
    const next = node.nextSibling;
    if (node instanceof HTMLElement) after.push(node);
    fresh.removeChild(node);
    node = next;
  }
  rebuildAround(pane, anchor, before, after);
  return true;
}

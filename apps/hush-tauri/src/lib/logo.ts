/**
 * The Hush Relay brand mark — the "Relay Bridge": two pillars joined by a
 * curved bridge, read as an abstract H. Design rationale lives in
 * `docs/BRAND.md`; the canonical asset is `docs/brand/hush-relay.svg`.
 *
 * This is a separate module from `icons.ts` on purpose: the mark is a filled
 * brand asset with its own gradient, not a stroke icon in the lucide set.
 *
 * The gradient colors are the interface's own accent tokens (see
 * `styles/tokens.css`): dark-theme `--accent-strong` / `--accent` and
 * light-theme `--accent`. Keep them in step if the tokens change.
 */

/** The mark's path data — identical to `docs/brand/hush-relay.svg`. */
const PATHS = [
  // Left pillar
  "M48 53 Q48 47 55 51 L72 63 Q77 67 77 74 L77 126 Q77 134 71 138 L55 149 Q48 153 48 145 Z",
  // Right pillar
  "M152 53 Q152 47 145 51 L128 63 Q123 67 123 74 L123 126 Q123 134 129 138 L145 149 Q152 153 152 145 Z",
  // Connecting bridge
  "M75 93 C87 80 112 80 125 93 L125 111 C113 98 88 98 75 111 Z",
];

const STOPS: Array<[string, string]> = [
  ["0%", "#b3a9ff"],
  ["52%", "#8b7df0"],
  ["100%", "#6c5ce7"],
];

const SVG_NS = "http://www.w3.org/2000/svg";

/**
 * Build the Relay Bridge mark as an inline SVG element.
 *
 * The viewBox is tight around the drawn mark (the paths span x 48-152,
 * y 47-153 of the 200-unit asset grid), so `sizePx` is the *visible* size of
 * the H — not the size of the asset's outer padding.
 *
 * Modes:
 * - "brand" (default): the token gradient, for the lock screens and any
 *   surface where the mark is the identity.
 * - "state": paths filled with `currentColor`, no gradient — for the rail's
 *   connection light, where `.relay-shield` state classes set `color`
 *   (green/orange/red) and the mark recolors like any lucide icon.
 * - "busy": the gradient mark with a signal wave crossing it (left pillar,
 *   bridge, right pillar, via `.relay-flow` in `motion.css`). The universal
 *   "working" animation; the stagger is a per-path `animation-delay`, so the
 *   mark is a static full-strength logo under reduced motion.
 *
 * The gradient id is namespaced so the mark can be inlined next to other
 * SVGs without colliding. Two marks in one document share the id safely only
 * while their gradients are identical; screens currently render one brand
 * mark at a time.
 */
/** Stagger between the three paths' wave, in ms: the signal crosses the
 * bridge left to right and the cycle restarts. */
const FLOW_STAGGER_MS = 180;

export function relayMark(
  sizePx = 64,
  mode: "brand" | "state" | "busy" = "brand",
): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "33 33 134 134");
  svg.setAttribute("width", String(sizePx));
  svg.setAttribute("height", String(sizePx));
  if (mode === "state" || mode === "busy") {
    // "state" takes its name from the wrapping control; "busy" is named by
    // the label beside it. Neither may speak for itself.
    svg.setAttribute("aria-hidden", "true");
  } else {
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", "Hush Relay");
  }

  if (mode === "brand") {
    const defs = document.createElementNS(SVG_NS, "defs");
    const grad = document.createElementNS(SVG_NS, "linearGradient");
    grad.id = "hush-relay-gradient";
    grad.setAttribute("x1", "0");
    grad.setAttribute("y1", "0");
    grad.setAttribute("x2", "1");
    grad.setAttribute("y2", "1");
    for (const [offset, color] of STOPS) {
      const stop = document.createElementNS(SVG_NS, "stop");
      stop.setAttribute("offset", offset);
      stop.setAttribute("stop-color", color);
      grad.appendChild(stop);
    }
    defs.appendChild(grad);
    svg.appendChild(defs);
  }

  const fill = mode === "state" ? "currentColor" : "url(#hush-relay-gradient)";
  let index = 0;
  for (const d of PATHS) {
    const path = document.createElementNS(SVG_NS, "path");
    path.setAttribute("d", d);
    path.setAttribute("fill", fill);
    if (mode === "busy") {
      path.setAttribute("class", "relay-flow");
      path.style.animationDelay = `${index * FLOW_STAGGER_MS}ms`;
      index += 1;
    }
    svg.appendChild(path);
  }
  return svg;
}

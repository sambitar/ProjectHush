# Hush Relay — Logo Design & Philosophy

## Design concept: The Relay Bridge

The Hush Relay logo is built around a simple idea: **private connections between people, without exposing what passes between them.**

The symbol consists of two vertical pillars connected by a curved bridge, forming an abstract letter **H**. The two pillars represent separate individuals, devices, or trusted endpoints. The bridge represents the connection between them: a channel through which communication can flow while privacy remains central.

The shape is intentionally abstract. It does not depict a literal shield, padlock, fingerprint, or circuit board. Instead, it creates a distinctive visual identity that can become recognizable through consistent use.

## The philosophy behind the symbol

**1. Connection without exposure**

The two pillars remain distinct, yet they are joined by a shared bridge. This reflects Hush Relay's approach to communication: people can connect without surrendering ownership of their private lives. The relay facilitates communication; it does not need to understand the contents of that communication.

**2. Privacy through architecture**

The logo avoids traditional security imagery. A shield suggests defense against an external threat, while the bridge suggests a system designed around how trusted connections are established and maintained. Privacy is not merely a protective feature added to the product; it is a principle that informs its architecture.

**3. Trust without centralization of relationships**

The pillars represent independent endpoints rather than a central authority. The bridge connects them without visually placing one above the other. This reflects a platform built around trusted relationships, private conversations, and direct invitations rather than a publicly searchable social graph.

**4. Simplicity as a form of discipline**

The mark uses a small number of geometric shapes and smooth curves. Its simplicity is deliberate: a privacy-focused product should not need visual complexity to communicate technical sophistication. The logo should be easy to recognize, reproduce, and maintain.

## Visual identity

**Shape:** Two upright forms joined by a curved horizontal bridge. The resulting negative space gives the symbol its abstract H-like character.

**Color:** A gradient drawn from the interface's own accent tokens — light periwinkle-violet `#b3a9ff` through `#8b7df0` to deep violet `#6c5ce7` — suggests quiet intelligence, depth, and a contemporary digital environment. The midnight-blue interface provides contrast without the aggressive appearance of conventional cybersecurity branding.

**Typography:** A lowercase wordmark, `hushrelay.space`, complements the symbol with an approachable, understated character. It feels like a product people can use every day rather than an intimidating security appliance.

**Form:** Rounded edges and smooth transitions communicate calmness and approachability. The design avoids sharp, aggressive angles and unnecessary ornamentation.

## Designed for a digital environment

The logo is intended to function as a symbol first and a wordmark second. It can appear independently as an application icon, navigation mark, notification identity, or favicon.

Its vector construction makes it scalable and easy to reproduce in SVG. The mark can also be rendered in monochrome for contexts where gradients are unavailable or inappropriate, including compact interface elements and print.

## The larger meaning

Hush Relay is not intended to be just another messaging application. It brings together private communication, email, file storage, meetings, and other tools within a privacy-first environment.

The Relay Bridge represents that broader purpose: **a connection between separate people and private spaces, designed to carry information without making the information itself the center of the system.**

The symbol's meaning is deliberately subtle. Users do not need to understand the architecture behind it to recognize the mark. Over time, its consistency across the product can give it meaning of its own.

**Core principle: Connect privately. Keep control.**

The bridge brings people together; the space between them remains theirs.

---

## Brand assets

Vector sources live in `docs/brand/`. The mark's geometry (viewBox `0 0 200 200`,
three paths: left pillar, right pillar, bridge) is identical in both variants.

| File | Use |
|---|---|
| `docs/brand/hush-relay.svg` | Full-color mark. Diagonal gradient `#b3a9ff → #8b7df0 (52%) → #6c5ce7`, top-left to bottom-right — the app's accent tokens (`tokens.css`: dark-theme `--accent-strong`/`--accent`, light-theme `--accent`). |
| `docs/brand/hush-relay-mono.svg` | Monochrome mark. Fill is `currentColor`, so it inherits surrounding text color inline and prints black by default. |
| `docs/brand/hush-relay-icon.svg` | App-icon variant: same paths, viewBox tight around the mark (`33 33 134 134`) so it fills launcher/notification surfaces. Source for the raster set in `apps/hush-tauri/src-tauri/icons/` (regenerate: rasterize this file to a 1024px transparent PNG, then `npm run tauri icon <png>`). |
| `apps/hush-tauri/public/favicon.svg` | Copy of the color mark, referenced from `index.html` for the dev-server/browser view. |

Usage notes:

- Both files carry `role="img"` and a `<title>`, so they are accessible when inlined.
- The gradient id is namespaced (`hush-relay-gradient`) to avoid collisions when the
  mark is inlined into HTML alongside other SVGs.
- In the app rail the mark doubles as the **connection light** (`relayMark(…, "state")`
  in `src/lib/logo.ts`): paths filled with `currentColor`, so the existing
  `.relay-shield` state classes recolor it — green connected, orange while Tor comes
  up, red offline.
- Application/launcher icons remain raster (`apps/hush-tauri/src-tauri/icons/`), generated
  from the mark; the SVG files here are the canonical source.

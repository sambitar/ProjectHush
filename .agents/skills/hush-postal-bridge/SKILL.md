---
name: hush-postal-bridge
description: Standing context for any task touching postal mail (Postal Bridge), QR payloads, or the Android scanner. Read docs/plans/hush-postal-bridge.md first.
---

# Hush Postal Bridge (future plan)

Read [`docs/plans/hush-postal-bridge.md`](/home/sam/ProjectHush/docs/plans/hush-postal-bridge.md)
before working on anything that touches postal mail, QR payloads, or the Android scanner.
It is future-plan context, not current scope. If a task conflicts with it, stop and ask.

Hard constraints (details and rationale in the doc):

- The postal gateway is a **separate component** from the relay, with its own threat-model
  entry. Never merge its data model or logs into the relay.
- Gateway data minimization: store only what fulfillment needs; delete content/address/PDF
  artifacts after print-provider acceptance; no analytics, cookies, or telemetry.
- Return address is always Hush's service address, never the user's. Omitting sender
  details is **not** anonymity — docs and UI must say so plainly.
- Modes 1 (plain), 2 (sealed), 3 (invite/drop) stay separate modules sharing a common
  provider adapter and QR codec.
- QR payload is a versioned binary format; the decoder treats QRs as untrusted input and
  fails closed. A task that touches the QR format updates Section 4 and notes a version bump.
- Ask before starting any task that changes the trust boundary, adds stored data to the
  gateway, or adds a third-party service.

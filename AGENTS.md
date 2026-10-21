# ProjectHush — Zed instructions

Migrated from Cursor's `.cursor/rules/` on 2026-10-20. Cursor keeps reading its own copies; this file is the Zed source of truth.

## Implementation problems log

Keep a running log at `/home/sam/ProjectHush/DEEPSEEKPROBLEMS.MD`.

### When to append

Append an entry **when the problem happens**, not at the end of the slice. Anything that cost a round trip, produced a wrong result, or was worked around instead of fixed belongs in the log.

- A wrong assumption that survived until a test caught it
- A command or tool that silently did nothing, or returned empty output
- A test that passed without exercising the intended code path
- Something that could not be verified in this environment
- A claim that was made and then had to be qualified
- A workaround applied because the root cause was not found

### What each entry needs

- A short title, tagged `FIXED`, `WORKED AROUND`, or `OPEN`
- What was attempted
- The symptom, with the actual error text or observed output where there is one
- The resolution, or what is needed to resolve it
- The lesson, when there is a general one

### Rules

- Be honest about severity. Do not soften an `OPEN` entry into a passing note in the summary.
- Never delete an `OPEN` entry; move it to `FIXED` only with the evidence that closed it.
- Newest section first, grouped by date and slice.
- No emojis.
- State plainly when a problem is tooling-related rather than a repo problem.

## UI dialogs and forms

Every dialog follows the existing small-modal pattern. Do not invent a new shape, and do not leave an
input and a button stacked full-width — that is what makes a dialog look unfinished.

- **Container:** `.modal-backdrop` around `.modal` plus a width class (`min(400–440px, 100%)`). Set
  `role="dialog"`, `aria-modal="true"` and an `aria-label`.
- **Structure:** an `<h2 class="text-22">` short title, then one muted `text-13` meta/explanation
  line, then the form, then `.modal-actions`.
- **Fields:** wrap each one in a `<label>` using the `.trust-delete-field` / `.pay-trustee-field`
  pattern — `display:flex; flex-direction:column; gap: var(--space-2); margin-top: var(--space-4)`, a
  `text-13` label, and an `.input-field` with `padding: var(--space-2) var(--space-3)`. A bare
  unlabelled full-width input is not acceptable.
- **Actions:** always inside `.modal-actions` (right-aligned, `gap: var(--space-2)`), with
  `.btn-secondary` for Cancel/Back and `.btn-primary` for the single confirming action. The base
  `.btn-primary` is `width: 100%` for stacked forms; a dialog must override it through
  `.modal-actions`, never inherit it.
- **Errors:** `text-13`, `color: var(--danger)`, `margin-top: var(--space-3)`, after the fields and
  before the actions.
- **Confirmation steps:** restate the irreversible facts (amount, recipient, consequence) in a
  bordered summary block, not a paragraph.
- **Reference implementations:** `renderTrustDeleteModal` and `renderPayTrusteeModal` in
  `apps/hush-tauri/src/app.ts`, and the `.modal` / `.modal-actions` rules in
  `apps/hush-tauri/src/styles/app.css`.

## Project skills

- `hush-postal-bridge` — standing context for postal mail, QR payloads, and the Android scanner.

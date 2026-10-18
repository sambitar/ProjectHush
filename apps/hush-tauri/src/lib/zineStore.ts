/**
 * The Zines draft editor.
 *
 * A zine is a *string* of sanitized HTML in the vault, not a CRDT: the author
 * writes an article, and publishing appends a revision to a hash chain that
 * only their pen name can sign. So this editor is deliberately dumber than
 * Paper's — one ProseMirror document, `prosemirror-history` for undo, no Yjs,
 * no peers, no awareness, no timers. Nothing here talks to the network, which
 * is the whole of Phase 1.
 *
 * What it does share with Paper is the *shape* of the document and the
 * toolbar, and both come from `proseSchema` so the two surfaces cannot drift.
 *
 * The body crosses the wire twice and is sanitized on both crossings: on the
 * way in (`sanitizeHtml`, so a body that arrived from a peer cannot put a
 * construct in the editor the schema cannot hold), and on the way out (again,
 * so what is shown before publishing is what will be stored). The daemon
 * sanitizes a third time on arrival — that is the one that matters, and this
 * one exists so the author is not surprised.
 */

import { EditorState, Plugin } from "prosemirror-state";
import type { Command } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import { keymap } from "prosemirror-keymap";
import { baseKeymap, chainCommands, toggleMark } from "prosemirror-commands";
import { history, undo, redo } from "prosemirror-history";
import {
  splitListItem,
  liftListItem,
  sinkListItem,
} from "prosemirror-schema-list";
import {
  DOMParser as PMDOMParser,
  DOMSerializer,
  type Node as PMNode,
} from "prosemirror-model";
import { buildToolbar, richTextSchema as schema, toolbarButtons } from "./proseSchema";
import { sanitizeHtml } from "./richText";

/** The live draft editor, if a Zines pane has one open. */
let editor: EditorView | null = null;
/** The element the current editor was mounted into, so a re-render can tell
 *  "this is the same editor being re-parented" from "a new draft". */
let host: HTMLElement | null = null;
/** Whether anything has been typed since the editor was mounted. Publishing
 *  an untouched zine would append a revision identical to the last one. */
let dirty = false;

/**
 * A sanitized HTML string as a ProseMirror document.
 *
 * `DOMParser` (ProseMirror's, not the browser's) walks a detached element, so
 * a body that arrived from a peer cannot run anything while it is being read.
 * Content the schema cannot represent is dropped rather than kept as text:
 * a mark the toolbar cannot toggle would be lost the first time the article
 * was re-opened, and losing it on the way in is the honest version of that.
 */
function docFromHtml(html: string): PMNode {
  const clean = html ? sanitizeHtml(html) : "";
  if (!clean.trim()) {
    // An empty paragraph, or the doc would have no valid content and the
    // view would refuse to start.
    return schema.node("doc", null, [schema.node("paragraph")]);
  }
  const holder = document.createElement("div");
  holder.innerHTML = clean;
  return PMDOMParser.fromSchema(schema).parse(holder);
}

/** The document's current content, as sanitized HTML. */
function htmlFromDoc(doc: PMNode): string {
  const fragment = DOMSerializer.fromSchema(schema).serializeFragment(doc.content);
  const holder = document.createElement("div");
  holder.appendChild(fragment);
  return sanitizeHtml(holder.innerHTML);
}

/**
 * Build the draft editor into `container`.
 *
 * Idempotent for the same container: the App builds the host once per draft
 * and re-parents it on every re-render (the Zines rail's pane is rebuilt each
 * pass, not keyed), so a second call for the same host returns without touching
 * anything. Rebuilding would drop the undo history and the caret, and the caret
 * is where the author was typing.
 *
 * `onChange` fires on every document change. It must not re-render the pane:
 * the editor is a third-party view and re-parenting it mid-keystroke is how a
 * caret ends up in the wrong paragraph. The caller uses it to update the
 * Publish affordance in place.
 */
export function attachZineEditor(
  container: HTMLElement,
  initialHtml: string,
  onChange: () => void,
): void {
  if (editor && host === container) return;
  closeZineEditor();

  const toolbar = buildToolbar(
    toolbarButtons({ undo, redo }),
    () => editor!,
    "Zine formatting",
  );

  // The pressed states follow the editor's state. A plugin `update` callback
  // rather than a `dispatchTransaction` override, because ProseMirror
  // dispatches while the view is still constructing and a dispatch override
  // closing over `view` reads it in its temporal dead zone.
  const toolbarSync = new Plugin({
    view: () => ({
      update: () => toolbar.refresh(),
    }),
  });

  const keys: Record<string, Command> = {
    "Mod-b": (st, dispatch) => toggleMark(schema.marks.bold)(st, dispatch),
    "Mod-i": (st, dispatch) => toggleMark(schema.marks.italic)(st, dispatch),
    "Mod-u": (st, dispatch) => toggleMark(schema.marks.underline)(st, dispatch),
    "Mod-z": undo,
    "Mod-y": redo,
    "Mod-Shift-z": redo,
    Enter: chainCommands(
      (st, dispatch, view) => splitListItem(schema.nodes.list_item)(st, dispatch, view),
      baseKeymap.Enter,
    ),
    Tab: (st, dispatch, view) => sinkListItem(schema.nodes.list_item)(st, dispatch, view),
    "Shift-Tab": (st, dispatch, view) => liftListItem(schema.nodes.list_item)(st, dispatch, view),
  };

  const wrap = document.createElement("div");
  wrap.className = "zine-editor-wrap";
  wrap.appendChild(toolbar.el);

  // The scroll container is part of the frame, so it goes in *before* the view
  // exists: `new EditorView(place)` appends its own DOM into `place`, and
  // appending `view.dom` afterwards would move it back out and leave this
  // element detached — the editor would then render with none of its styling.
  const surface = document.createElement("div");
  surface.className = "zine-editor-surface";
  wrap.appendChild(surface);

  const view = new EditorView(surface, {
    state: EditorState.create({
      schema,
      doc: docFromHtml(initialHtml),
      plugins: [history(), toolbarSync, keymap(keys), keymap(baseKeymap)],
    }),
    attributes: {
      role: "textbox",
      "aria-multiline": "true",
      "aria-label": "Zine body",
      spellcheck: "true",
    },
    dispatchTransaction(tr) {
      const next = view.state.apply(tr);
      view.updateState(next);
      if (tr.docChanged) {
        dirty = true;
        onChange();
      }
      toolbar.refresh();
    },
  });

  // `view.dom` is already inside `surface`; appending it here would move it.
  container.appendChild(wrap);
  editor = view;
  host = container;
  dirty = false;
  toolbar.refresh();
}

/** The draft as it stands, sanitized and ready to publish. `""` when no
 *  editor is mounted. */
export function zineEditorHtml(): string {
  return editor ? htmlFromDoc(editor.state.doc) : "";
}

/** Whether the author has typed since the draft was opened. */
export function zineEditorIsDirty(): boolean {
  return dirty;
}

/** Whether the draft editor currently holds the caret. Used by the shell's
 *  global shortcut handler to leave Ctrl+B and friends to the editor. */
export function zineEditorHasFocus(): boolean {
  return !!editor && editor.hasFocus();
}

/** Tear the editor down. Called on publish, cancel, and whenever the pane
 *  closes: the draft only lives in this view, so leaving it mounted is how a
 *  cancelled draft comes back on the next visit. */
export function closeZineEditor(): void {
  if (editor) {
    editor.destroy();
    editor = null;
  }
  host = null;
  dirty = false;
}

/**
 * Split the section input into tags.
 *
 * The daemon normalizes again (lowercase, collapse whitespace, cap the count
 * and the length) and is the authority; this exists so the author sees the
 * tags they are about to publish rather than the raw string they typed, and so
 * a trailing comma does not produce an empty tag.
 */
export function parseSections(raw: string): string[] {
  const out: string[] = [];
  for (const part of raw.split(/[,\n]/)) {
    const label = part.trim().replace(/\s+/g, " ");
    if (!label) continue;
    const key = label.toLowerCase();
    if (out.some((s) => s.toLowerCase() === key)) continue;
    out.push(label);
    if (out.length >= SECTIONS_CAP) break;
  }
  return out;
}

/** Matches `SECTIONS_CAP` in `hush-daemon/src/zine_store.rs`: a section is a
 *  facet, not a keyword dump. */
const SECTIONS_CAP = 12;

/** The byline shown for a pen name that has none yet. */
export function penNameLabel(penName: { name: string; id: string } | null): string {
  if (!penName) return "No pen name yet";
  return penName.name || `${penName.id.slice(0, 8)}…`;
}

/** A zine id as a reader sees it: the seal key's first bytes. Short enough for
 *  a header, long enough to tell two pen names apart at a glance. */
export function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}

/**
 * The shared ProseMirror layer for the two collaborative editors.
 *
 * Conclave Paper and Zines are different in every way that matters to storage
 * — Paper is a Yjs CRDT fanned out over the Conclave, a zine is a revision
 * chain of sanitized HTML — but they are the *same editor* to the person
 * typing in them: bold, italic, underline, three heading levels, two list
 * kinds. That shape lives here once so the two surfaces cannot drift apart,
 * and so a change to the toolbar is a change in one file.
 *
 * This is deliberately *not* `richText.ts`. That module is the mail body
 * sanitizer: it treats a body as an untrusted string and exists to make
 * `innerHTML` safe. It runs on the *stored* value. This module builds the
 * editor that produces the value, and knows nothing about trust.
 *
 * v1 scope is the same for both editors. Richer constructs a zine's daemon
 * allow-list would accept (`s`, `code`, `pre`, `blockquote`, links) are
 * deliberately absent: a mark the toolbar cannot toggle and the schema cannot
 * round-trip would be dropped the first time a body was re-opened for an
 * edit. Adding one means adding it here, once, for both.
 */

import {
  Schema,
  type MarkType,
  type NodeType,
  type Node as PMNode,
} from "prosemirror-model";
import type { Command, EditorState } from "prosemirror-state";
import type { EditorView } from "prosemirror-view";
import { setBlockType, toggleMark } from "prosemirror-commands";
import {
  bulletList,
  orderedList,
  listItem,
  wrapInList,
} from "prosemirror-schema-list";
import { icon, type IconName } from "./icons";

/** Marks and nodes are referred to by these aliases so the two editor stores
 *  do not each re-derive them from the composite schema. */
export type RichTextMark = MarkType;
export type RichTextNode = NodeType;

export const richTextSchema = new Schema({
  nodes: {
    doc: { content: "block+" },
    paragraph: {
      group: "block",
      content: "inline*",
      parseDOM: [{ tag: "p" }],
      toDOM(): [string, number] {
        return ["p", 0];
      },
    },
    heading: {
      group: "block",
      content: "inline*",
      defining: true,
      attrs: { level: { default: 1 } },
      parseDOM: [1, 2, 3].map((level) => ({
        tag: `h${level}`,
        attrs: { level },
      })),
      toDOM(node: PMNode): [string, number] {
        return [`h${node.attrs.level}`, 0];
      },
    },
    bullet_list: {
      ...bulletList,
      content: "list_item+",
      group: "block",
    },
    ordered_list: {
      ...orderedList,
      content: "list_item+",
      group: "block",
    },
    list_item: {
      ...listItem,
      content: "paragraph block*",
    },
    text: { group: "inline" },
    hard_break: {
      group: "inline",
      inline: true,
      selectable: false,
      parseDOM: [{ tag: "br" }],
      toDOM(): [string, number] {
        return ["br", 0];
      },
    },
  },
  marks: {
    bold: {
      parseDOM: [
        { tag: "strong" },
        { tag: "b" },
        {
          style: "font-weight",
          getAttrs: (v: string) => (/^(bold|[5-9]\d{2,})$/.test(v) ? {} : false),
        },
      ],
      toDOM(): [string, number] {
        return ["strong", 0];
      },
    },
    italic: {
      parseDOM: [
        { tag: "em" },
        { tag: "i" },
        {
          style: "font-style",
          getAttrs: (v: string) => (v === "italic" ? {} : false),
        },
      ],
      toDOM(): [string, number] {
        return ["em", 0];
      },
    },
    underline: {
      parseDOM: [
        { tag: "u" },
        {
          style: "text-decoration",
          getAttrs: (v: string) => (v === "underline" ? {} : false),
        },
      ],
      toDOM(): [string, number] {
        return ["u", 0];
      },
    },
  },
});

/** Mark is active at the selection? (Drives a button's pressed state.) */
function markActive(state: EditorState, type: RichTextMark): boolean {
  const { from, $from, to, empty } = state.selection;
  if (empty) return !!type.isInSet(state.storedMarks || $from.marks());
  return state.doc.rangeHasMark(from, to, type);
}

/** Whether the selection sits inside a node of `type` (optionally with attrs). */
function blockType(
  state: EditorState,
  type: RichTextNode,
  attrs?: Record<string, unknown>,
): boolean {
  const $from = state.selection.$from;
  for (let d = $from.depth; d >= 0; d--) {
    const node = $from.node(d);
    if (node.type === type) {
      return attrs
        ? Object.entries(attrs).every(([k, v]) => node.attrs[k] === v)
        : true;
    }
  }
  return false;
}

export interface ToolbarButton {
  label?: string;
  icon?: IconName;
  title: string;
  active: (state: EditorState) => boolean;
  run: Command;
}

/** The undo/redo commands differ per editor: Paper's live on its
 *  `Y.UndoManager` (a CRDT undo is not a transaction undo), a zine's come
 *  from `prosemirror-history`. Everything else is shared. */
export interface ToolbarUndoRedo {
  undo: Command;
  redo: Command;
}

/**
 * The formatting toolbar's buttons, in order.
 *
 * Undo/redo come first because they are the two commands people reach for
 * without looking. Everything after them is a mark or a block type, so
 * `run` only ever needs the state, a dispatch, and occasionally the view.
 */
export function toolbarButtons({ undo, redo }: ToolbarUndoRedo): ToolbarButton[] {
  const { marks, nodes } = richTextSchema;
  return [
    {
      icon: "undo",
      title: "Undo (Ctrl+Z)",
      active: () => false,
      run: undo,
    },
    {
      icon: "redo",
      title: "Redo (Ctrl+Shift+Z)",
      active: () => false,
      run: redo,
    },
    {
      icon: "bold",
      title: "Bold (Ctrl+B)",
      active: (st) => markActive(st, marks.bold),
      run: (st, dispatch) => toggleMark(marks.bold)(st, dispatch),
    },
    {
      icon: "italic",
      title: "Italic (Ctrl+I)",
      active: (st) => markActive(st, marks.italic),
      run: (st, dispatch) => toggleMark(marks.italic)(st, dispatch),
    },
    {
      icon: "underline",
      title: "Underline (Ctrl+U)",
      active: (st) => markActive(st, marks.underline),
      run: (st, dispatch) => toggleMark(marks.underline)(st, dispatch),
    },
    {
      label: "H1",
      title: "Heading 1",
      active: (st) => blockType(st, nodes.heading, { level: 1 }),
      run: (st, dispatch) => setBlockType(nodes.heading, { level: 1 })(st, dispatch),
    },
    {
      label: "H2",
      title: "Heading 2",
      active: (st) => blockType(st, nodes.heading, { level: 2 }),
      run: (st, dispatch) => setBlockType(nodes.heading, { level: 2 })(st, dispatch),
    },
    {
      label: "H3",
      title: "Heading 3",
      active: (st) => blockType(st, nodes.heading, { level: 3 }),
      run: (st, dispatch) => setBlockType(nodes.heading, { level: 3 })(st, dispatch),
    },
    {
      label: "Body",
      title: "Plain paragraph",
      active: (st) => blockType(st, nodes.paragraph),
      run: (st, dispatch) => setBlockType(nodes.paragraph)(st, dispatch),
    },
    {
      icon: "list",
      title: "Bulleted list",
      active: (st) => blockType(st, nodes.bullet_list),
      run: (st, dispatch, view) => wrapInList(nodes.bullet_list)(st, dispatch, view),
    },
    {
      icon: "listOrdered",
      title: "Numbered list",
      active: (st) => blockType(st, nodes.ordered_list),
      run: (st, dispatch, view) => wrapInList(nodes.ordered_list)(st, dispatch, view),
    },
  ];
}

export interface BuiltToolbar {
  /** The toolbar element, ready to be appended. */
  el: HTMLElement;
  /** Repaint the pressed states from the view's current state. */
  refresh: () => void;
}

/**
 * Render `buttons` into a toolbar element.
 *
 * `getView` is a getter rather than the view itself because the toolbar is
 * built *before* the `EditorView` exists — y-prosemirror dispatches a
 * transaction while the view is still constructing, so nothing here may read
 * the view until a click or a state update, both of which happen long after.
 *
 * A button press must not move the caret, so `mousedown` is suppressed; the
 * command is applied to the live view and focus is handed straight back.
 */
export function buildToolbar(
  buttons: ToolbarButton[],
  getView: () => EditorView,
  ariaLabel: string,
): BuiltToolbar {
  const el = document.createElement("div");
  el.className = "rich-editor-toolbar paper-toolbar";
  el.setAttribute("role", "toolbar");
  el.setAttribute("aria-label", ariaLabel);

  const els: HTMLButtonElement[] = [];
  for (const b of buttons) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "rich-editor-btn pressable paper-toolbar-btn";
    button.title = b.title;
    button.setAttribute("aria-label", b.title);
    button.setAttribute("aria-pressed", "false");
    // Icons are the house icon set; the type buttons carry their label.
    if (b.icon) {
      button.appendChild(icon(b.icon));
    } else {
      const t = document.createElement("span");
      t.className = "paper-toolbar-label";
      t.textContent = b.label ?? "";
      button.appendChild(t);
    }
    button.addEventListener("mousedown", (e) => e.preventDefault());
    button.addEventListener("click", () => {
      const view = getView();
      view.focus();
      b.run(view.state, (tr) => view.dispatch(tr), view);
    });
    el.appendChild(button);
    els.push(button);
  }

  const refresh = (): void => {
    const view = getView();
    if (!view) return;
    for (const [i, b] of buttons.entries()) {
      const button = els[i];
      const on = b.active(view.state);
      button.classList.toggle("active", on);
      button.setAttribute("aria-pressed", on ? "true" : "false");
    }
  };

  return { el, refresh };
}

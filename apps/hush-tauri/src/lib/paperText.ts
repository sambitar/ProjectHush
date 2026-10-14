import * as Y from "yjs";

/**
 * Paper as plain text, for saving a conclave's paper into a note.
 *
 * The paper's Yjs fragment mirrors the rich-text schema (paragraph, three
 * heading levels, two list kinds, hard breaks), but a note's body is plain
 * text in a textarea — so the export flattens structure the way a person
 * would: headings carry a markdown-style prefix, list items get real bullet
 * and number markers, nesting indents, and everything else is its text.
 * Marks (bold, italic, underline) have no plain-text voice and drop out.
 *
 * Pure over the fragment: no editor, no network, no session.
 */

type Child = Y.XmlElement | Y.XmlText;

export function xmlFragmentToPlainText(frag: Y.XmlFragment): string {
  const lines = blocksText(frag.toArray() as Child[], 0);
  // Trailing blank lines are an artifact of empty paragraphs at the end.
  while (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.join("\n");
}

function blocksText(blocks: Child[], depth: number): string[] {
  const out: string[] = [];
  for (const block of blocks) {
    if (!(block instanceof Y.XmlElement)) continue;
    switch (block.nodeName) {
      case "paragraph":
        out.push(indent(depth) + inlineText(block.toArray() as Child[]));
        break;
      case "heading":
        out.push(
          indent(depth) +
            "#".repeat(headingLevel(block)) +
            " " +
            inlineText(block.toArray() as Child[]),
        );
        break;
      case "bullet_list":
        out.push(...listText(block, depth, "bullet"));
        break;
      case "ordered_list":
        out.push(...listText(block, depth, "ordered"));
        break;
      default:
        // A block a future schema adds: its text still belongs in the note.
        out.push(indent(depth) + inlineText(block.toArray() as Child[]));
    }
  }
  return out;
}

function listText(
  list: Y.XmlElement,
  depth: number,
  kind: "bullet" | "ordered",
): string[] {
  const lines: string[] = [];
  let number = 1;
  for (const child of list.toArray() as Child[]) {
    if (!(child instanceof Y.XmlElement) || child.nodeName !== "list_item") continue;
    const marker = kind === "bullet" ? "•" : `${number}.`;
    let firstBlock = true;
    for (const inner of child.toArray() as Child[]) {
      if (
        inner instanceof Y.XmlElement &&
        (inner.nodeName === "bullet_list" || inner.nodeName === "ordered_list")
      ) {
        lines.push(
          ...listText(
            inner,
            depth + 1,
            inner.nodeName === "bullet_list" ? "bullet" : "ordered",
          ),
        );
      } else if (inner instanceof Y.XmlElement) {
        const text = inlineText(inner.toArray() as Child[]);
        // The marker sits on the item's first block; continuation blocks
        // (an item with two paragraphs) hang under it.
        lines.push(indent(depth) + (firstBlock ? `${marker} ` : "  ") + text);
      }
      firstBlock = false;
    }
    if (kind === "ordered") number++;
  }
  return lines;
}

/** Inline content: text, hard breaks as real newlines, marks ignored. */
function inlineText(children: Child[]): string {
  let out = "";
  for (const child of children) {
    if (typeof child === "string") {
      out += child;
    } else if (child instanceof Y.XmlText) {
      // `toString()` would render formatted runs as literal tags
      // ("<bold>loud</bold>"), so the text comes from the delta inserts,
      // which carry the marks separately from the characters.
      out += child
        .toDelta()
        .map((d: { insert?: unknown }) =>
          typeof d.insert === "string" ? d.insert : "",
        )
        .join("");
    } else if (child.nodeName === "hard_break") {
      out += "\n";
    } else {
      out += inlineText(child.toArray() as Child[]);
    }
  }
  return out;
}

function headingLevel(heading: Y.XmlElement): number {
  const level = heading.getAttribute("level");
  return typeof level === "number" && level >= 1 && level <= 3 ? level : 1;
}

function indent(depth: number): string {
  return "  ".repeat(depth);
}

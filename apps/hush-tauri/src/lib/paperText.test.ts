import { test } from "node:test";
import assert from "node:assert/strict";
import * as Y from "yjs";
import { xmlFragmentToPlainText } from "./paperText.ts";

function el(name: string, ...children: (Y.XmlElement | Y.XmlText)[]): Y.XmlElement {
  const e = new Y.XmlElement(name);
  for (const [i, c] of children.entries()) e.insert(i, [c]);
  return e;
}

function text(s: string): Y.XmlText {
  const t = new Y.XmlText();
  t.insert(0, s);
  return t;
}

function fragOf(...blocks: Y.XmlElement[]): Y.XmlFragment {
  const doc = new Y.Doc();
  const frag = doc.getXmlFragment("prosemirror");
  frag.insert(0, blocks);
  return frag;
}

test("paragraphs become lines", () => {
  const s = xmlFragmentToPlainText(
    fragOf(el("paragraph", text("first")), el("paragraph", text("second"))),
  );
  assert.equal(s, "first\nsecond");
});

test("an empty paper exports as an empty string", () => {
  assert.equal(xmlFragmentToPlainText(fragOf()), "");
});

test("empty paragraphs keep a blank line between blocks", () => {
  const s = xmlFragmentToPlainText(
    fragOf(el("paragraph", text("above")), el("paragraph"), el("paragraph", text("below"))),
  );
  assert.equal(s, "above\n\nbelow");
});

test("headings keep their level as markdown prefixes", () => {
  const h = (level: number, s: string) => {
    const e = new Y.XmlElement("heading");
    // The editor writes the level as a number; the Yjs typing says string.
    e.setAttribute("level", level as unknown as string);
    e.insert(0, [text(s)]);
    return e;
  };
  const s = xmlFragmentToPlainText(fragOf(h(1, "Title"), h(2, "Section"), h(3, "Sub")));
  assert.equal(s, "# Title\n## Section\n### Sub");
});

test("a heading without a level attr reads as level one", () => {
  const s = xmlFragmentToPlainText(fragOf(el("heading", text("Bare"))));
  assert.equal(s, "# Bare");
});

test("bullet and ordered lists render with markers", () => {
  const item = (...inner: (Y.XmlElement | Y.XmlText)[]) => el("list_item", ...inner);
  const s = xmlFragmentToPlainText(
    fragOf(
      el(
        "bullet_list",
        item(el("paragraph", text("milk"))),
        item(el("paragraph", text("eggs"))),
      ),
      el(
        "ordered_list",
        item(el("paragraph", text("mix"))),
        item(el("paragraph", text("pour"))),
      ),
    ),
  );
  assert.equal(s, "• milk\n• eggs\n1. mix\n2. pour");
});

test("nested lists indent under their item", () => {
  const item = (...inner: (Y.XmlElement | Y.XmlText)[]) => el("list_item", ...inner);
  const s = xmlFragmentToPlainText(
    fragOf(
      el(
        "bullet_list",
        item(
          el("paragraph", text("outer")),
          el(
            "bullet_list",
            item(el("paragraph", text("inner one"))),
            item(el("paragraph", text("inner two"))),
          ),
        ),
      ),
    ),
  );
  assert.equal(s, "• outer\n  • inner one\n  • inner two");
});

test("a numbered item with two paragraphs hangs the second under the marker", () => {
  const item = (...inner: (Y.XmlElement | Y.XmlText)[]) => el("list_item", ...inner);
  const s = xmlFragmentToPlainText(
    fragOf(
      el(
        "ordered_list",
        item(el("paragraph", text("first line")), el("paragraph", text("second line"))),
      ),
    ),
  );
  assert.equal(s, "1. first line\n  second line");
});

test("hard breaks split lines inside one paragraph", () => {
  const s = xmlFragmentToPlainText(
    fragOf(el("paragraph", text("a"), el("hard_break"), text("b"))),
  );
  assert.equal(s, "a\nb");
});

test("bold and italic marks drop out, their text stays", () => {
  // A bold run is still a Y.XmlText; the mark lives in its deltas, not its shape.
  const bold = new Y.XmlText();
  bold.insert(0, "loud", { bold: {} });
  const s = xmlFragmentToPlainText(fragOf(el("paragraph", bold, text(" and quiet"))));
  assert.equal(s, "loud and quiet");
});

test("trailing blank paragraphs do not leave trailing newlines", () => {
  const s = xmlFragmentToPlainText(
    fragOf(el("paragraph", text("only")), el("paragraph"), el("paragraph")),
  );
  assert.equal(s, "only");
});

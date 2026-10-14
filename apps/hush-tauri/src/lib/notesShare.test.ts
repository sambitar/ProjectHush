import { test } from "node:test";
import assert from "node:assert/strict";
import { noteFileName, noteToFileText } from "./notesShare.ts";
import type { NoteDto } from "./api/types.ts";

function note(overrides: Partial<NoteDto> = {}): NoteDto {
  return {
    id: "n1",
    board_id: "default",
    title: "Shopping list",
    body: "buy apples",
    color: "#fff",
    x: 0,
    y: 0,
    z: 0,
    created_at: 0,
    updated_at: 0,
    checklist: [],
    ...overrides,
  };
}

test("body first, then the checklist as [x]/[ ] lines", () => {
  const text = noteToFileText(
    note({
      body: "plan\nmore",
      checklist: [
        { id: "c1", text: "done item", done: true },
        { id: "c2", text: "open item", done: false },
      ],
    }),
  );
  assert.equal(text, "plan\nmore\n\n[x] done item\n[ ] open item\n");
});

test("a note with no checklist has no trailing gap", () => {
  assert.equal(noteToFileText(note()), "buy apples\n");
});

test("an empty note serialises to nothing", () => {
  assert.equal(noteToFileText(note({ body: "  ", checklist: [] })), "");
});

test("a checklist-only note still gets content", () => {
  const text = noteToFileText(
    note({ body: "", checklist: [{ id: "c1", text: "only", done: false }] }),
  );
  assert.equal(text, "[ ] only\n");
});

test("the file name is the title plus .txt", () => {
  assert.equal(noteFileName(note()), "Shopping list.txt");
});

test("filesystem-hostile title characters fold to dashes", () => {
  assert.equal(noteFileName(note({ title: 'a/b\\c:d*e?f"g<h>i|j' })), "a-b-c-d-e-f-g-h-i-j.txt");
});

test("a blank title falls back to Note", () => {
  assert.equal(noteFileName(note({ title: "   " })), "Note.txt");
});

test("over-long titles are capped, keeping the .txt suffix", () => {
  const name = noteFileName(note({ title: "x".repeat(120) }));
  assert.equal(name.length, 80 + ".txt".length);
  assert.ok(name.endsWith(".txt"));
});

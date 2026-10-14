import type { NoteDto } from "./api/types";

/**
 * A note as the body of a text-file attachment: the body first, then the
 * checklist as `[x]`/`[ ]` lines. Plain text on purpose — the recipient opens
 * the file anywhere, no Hush needed, and it rides the ordinary attachment
 * pipeline rather than growing a new attachment kind.
 */
export function noteToFileText(note: NoteDto): string {
  const parts: string[] = [];
  const body = note.body.trimEnd();
  if (body) parts.push(body);
  if (note.checklist.length) {
    parts.push(
      note.checklist.map((i) => `[${i.done ? "x" : " "}] ${i.text}`).join("\n"),
    );
  }
  return parts.length ? `${parts.join("\n\n")}\n` : "";
}

/**
 * A file name for an attached note: the title, with filesystem-hostile
 * characters folded to `-` and a length cap, falling back to "Note".
 */
export function noteFileName(note: NoteDto): string {
  const base = (note.title.trim() || "Note")
    .replace(/[\\/:*?"<>|]+/g, "-")
    .slice(0, 80)
    .trim();
  return `${base || "Note"}.txt`;
}

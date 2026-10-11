import { api } from "./api/daemon";
import type { NoteDto, NotesStateDto } from "./api/types";

/**
 * The Notes pane's client cache. The board is small and lives entirely in one
 * encrypted sidecar, so the store mirrors the whole state: every mutation
 * returns the new state and replaces the cache wholesale (the Reads pattern).
 */

/** The empty board: before the first load, and after a lock. */
export const EMPTY_NOTES_STATE: NotesStateDto = {
  board_id: "default",
  notes: [],
  regions: [],
};

let cachedState: NotesStateDto | null = null;

/** Reset on lock and on unlock-as-a-different-vault. */
export function clearNotesStore(): void {
  cachedState = null;
}

/** Personal state, cached until a mutation replaces it. */
export async function getNotesState(): Promise<NotesStateDto> {
  if (!cachedState) cachedState = (await api.notesState()) ?? EMPTY_NOTES_STATE;
  return cachedState;
}

/**
 * The cache without a fetch: the empty board until the first load. For the
 * Vault tile's count, which must not trigger a request during a render.
 */
export function peekNotesState(): NotesStateDto {
  return cachedState ?? EMPTY_NOTES_STATE;
}

function applyState(next: NotesStateDto): NotesStateDto {
  cachedState = next;
  return next;
}

/**
 * Create (no `id`) or update (matching `id`) a note. Returns the new state and
 * the note's id so the caller can open the editor on it.
 */
export async function saveNote(draft: {
  id?: string;
  title: string;
  body: string;
  color?: string;
}): Promise<{ state: NotesStateDto; id: string }> {
  const resp = await api.notesSaveNote(draft);
  if (!resp) throw new Error("The note could not be saved.");
  return { state: applyState(resp.state), id: resp.id };
}

export async function deleteNote(id: string): Promise<NotesStateDto> {
  return applyState((await api.notesDeleteNote(id)) ?? EMPTY_NOTES_STATE);
}

/** Notes newest-edited first, for the list. Does not mutate the cache. */
export function newestFirst(state: NotesStateDto): NoteDto[] {
  return [...state.notes].sort((a, b) => b.updated_at - a.updated_at);
}

/** A one-line preview of a note body, for a list row. */
export function notePreview(note: NoteDto, max = 90): string {
  const flat = note.body.replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  return `${flat.slice(0, max - 1).trimEnd()}…`;
}

import { api } from "./api/daemon";
import type { VaultFileDto, VaultFilesStateDto } from "./api/types";

/**
 * The "Your files" pane's client cache.
 *
 * The library is one encrypted index in the vault, so every mutation returns
 * the whole state and replaces the cache wholesale — the Notes/Reads pattern.
 * The bytes never pass through here: open and save go through the blob routes.
 */

/** The empty library: before the first load, and after a lock. */
export const EMPTY_VAULT_FILES_STATE: VaultFilesStateDto = { files: [] };

let cachedState: VaultFilesStateDto | null = null;
let loaded = false;

/** Reset on lock and on unlock-as-a-different-vault. */
export function clearVaultFilesStore(): void {
  cachedState = null;
  loaded = false;
}

/** Whether the library has been read at least once this unlock. */
export function isVaultFilesLoaded(): boolean {
  return loaded;
}

/** Personal state, cached until a mutation replaces it. */
export async function getVaultFilesState(): Promise<VaultFilesStateDto> {
  if (!cachedState) {
    cachedState = (await api.vaultFiles()) ?? EMPTY_VAULT_FILES_STATE;
  }
  loaded = true;
  return cachedState;
}

/**
 * The cache without a fetch: the empty library until the first load. For the
 * Vault tile's count, which must not trigger a request during a render.
 */
export function peekVaultFilesState(): VaultFilesStateDto {
  return cachedState ?? EMPTY_VAULT_FILES_STATE;
}

function applyState(next: VaultFilesStateDto): VaultFilesStateDto {
  cachedState = next;
  loaded = true;
  return next;
}

/** Import a file the desktop shell picked, by local path. */
export async function importVaultFileFromPath(
  path: string,
): Promise<VaultFilesStateDto> {
  const resp = await api.vaultFileImportPath(path);
  if (!resp) throw new Error("The file could not be added.");
  return applyState(resp.state);
}

/** Import bytes already read in the webview (the browser build's path). */
export async function importVaultFileFromData(
  name: string,
  dataBase64: string,
): Promise<VaultFilesStateDto> {
  const resp = await api.vaultFileUpload(name, dataBase64);
  if (!resp) throw new Error("The file could not be added.");
  return applyState(resp.state);
}

export async function deleteVaultFile(id: string): Promise<VaultFilesStateDto> {
  return applyState((await api.vaultFileDelete(id)) ?? EMPTY_VAULT_FILES_STATE);
}

/** Newest import first, for the list. Does not mutate the cache. */
export function newestFirst(state: VaultFilesStateDto): VaultFileDto[] {
  return [...state.files].sort((a, b) => b.added_at - a.added_at);
}

/** Bytes the library accounts for, as the Vault tile prints them. */
export function vaultFilesBytes(state: VaultFilesStateDto): number {
  return state.files.reduce((sum, file) => sum + file.size, 0);
}

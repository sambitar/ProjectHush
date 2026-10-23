import { invoke } from "@tauri-apps/api/core";
import { api, getApiBase } from "./api/daemon";
import { isTauriShell } from "./tauriEnv";

export interface PickedVaultFile {
  path: string;
  name: string;
  size: number;
}

/**
 * Ask the desktop shell for files to add to the vault.
 *
 * Only paths come back: the daemon reads and encrypts the file itself, so a
 * large import never crosses the webview or the Tauri IPC bridge. A browser
 * build has no shell picker; it falls back to [`pickBrowserFiles`].
 */
export async function pickVaultFilesFromPc(): Promise<PickedVaultFile[]> {
  if (!isTauriShell()) return [];
  return invoke<PickedVaultFile[]>("hush_pick_vault_files");
}

/** A multi-select file input, for builds without the desktop shell. */
export function pickBrowserFiles(): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.style.display = "none";
    const finish = (files: File[]): void => {
      input.remove();
      resolve(files);
    };
    input.onchange = () => finish(Array.from(input.files ?? []));
    input.oncancel = () => finish([]);
    document.body.appendChild(input);
    input.click();
  });
}

/**
 * The minimum a message needs for its bytes to be resolvable: either they are
 * already in memory, or a vault blob id points at them. Both 1:1 chat messages
 * and conclave messages satisfy this.
 */
export interface FileBytesSource {
  fileData?: Uint8Array;
  blobId?: string;
}

export async function resolveMessageFileBytes(
  m: FileBytesSource,
): Promise<Uint8Array> {
  if (m.fileData?.length) return m.fileData;
  if (m.blobId) {
    const r = await api.blob(m.blobId.toLowerCase());
    if (!r?.data_base64) throw new Error("Could not load file from vault");
    const bin = atob(r.data_base64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  throw new Error("File not available on this device");
}

function toBlobPart(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

export async function downloadBase64File(
  name: string,
  dataBase64: string,
): Promise<void> {
  const bin = atob(dataBase64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  await saveBytesAsFile(name, bytes);
}

/**
 * Whether the desktop shell can ask the daemon to materialize a blob to a file
 * (Unix-socket transport only). TCP/browser builds fall back to fetching the
 * blob into the webview, which is fine for the 256 KiB-capped chat attachments
 * but must never be used for the multi-GB torrent library.
 */
function canStreamBlobs(): boolean {
  return isTauriShell() && getApiBase().startsWith("unix://");
}

/**
 * Open a vault blob in the OS handler. In the desktop shell the daemon
 * decrypts the file to a temp path and the shell opens that path, so the bytes
 * never enter the webview — opening a multi-GB torrent no longer OOMs the UI.
 */
export async function openVaultBlob(name: string, blobId: string): Promise<void> {
  if (canStreamBlobs()) {
    await invoke("hush_open_blob", { id: blobId.toLowerCase(), name });
    return;
  }
  const bytes = await resolveMessageFileBytes({ blobId });
  await openBytesInNewTab(name, bytes);
}

/**
 * Save a vault blob to a user-chosen path. Same streaming path as
 * [`openVaultBlob`]; the shell copies the daemon's temp file to the chosen
 * destination without the webview holding the bytes.
 */
export async function saveVaultBlob(
  name: string,
  blobId: string,
  defaultName?: string,
): Promise<void> {
  if (canStreamBlobs()) {
    await invoke("hush_save_blob", {
      id: blobId.toLowerCase(),
      name,
      defaultName: defaultName ?? name,
    });
    return;
  }
  const bytes = await resolveMessageFileBytes({ blobId });
  await saveBytesAsFile(name, bytes);
}

export function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const comma = result.indexOf(",");
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

async function saveBytesNative(name: string, bytes: Uint8Array): Promise<void> {
  await invoke("hush_save_file", {
    defaultName: name || "download",
    bytes: Array.from(bytes),
  });
}

async function openBytesNative(name: string, bytes: Uint8Array): Promise<void> {
  await invoke("hush_open_file", {
    fileName: name || "file",
    bytes: Array.from(bytes),
  });
}

export async function saveBytesAsFile(
  name: string,
  bytes: Uint8Array,
): Promise<void> {
  if (isTauriShell()) {
    await saveBytesNative(name, bytes);
    return;
  }
  const blob = new Blob([toBlobPart(bytes)]);
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name || "download";
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

export async function openBytesInNewTab(
  name: string,
  bytes: Uint8Array,
): Promise<void> {
  if (isTauriShell()) {
    await openBytesNative(name, bytes);
    return;
  }
  const blob = new Blob([toBlobPart(bytes)]);
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.target = "_blank";
  a.rel = "noopener noreferrer";
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 120_000);
}

export function attachFileActions(
  container: HTMLElement,
  fileName: string,
  loadBytes: () => Promise<Uint8Array>,
  blobId?: string | null,
): void {
  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.gap = "var(--space-2)";
  row.style.marginTop = "var(--space-3)";

  const openBtn = document.createElement("button");
  openBtn.type = "button";
  openBtn.className = "btn-secondary text-13 pressable";
  openBtn.textContent = "Open";
  openBtn.onclick = async () => {
    openBtn.disabled = true;
    try {
      if (blobId) {
        await openVaultBlob(fileName, blobId);
      } else {
        const bytes = await loadBytes();
        await openBytesInNewTab(fileName, bytes);
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Could not open file";
      if (msg.toLowerCase().includes("cancel")) return;
      window.alert(msg);
    } finally {
      openBtn.disabled = false;
    }
  };

  const saveBtn = document.createElement("button");
  saveBtn.type = "button";
  saveBtn.className = "btn-secondary text-13 pressable";
  saveBtn.textContent = "Save as…";
  saveBtn.onclick = async () => {
    saveBtn.disabled = true;
    try {
      if (blobId) {
        await saveVaultBlob(fileName, blobId);
      } else {
        const bytes = await loadBytes();
        await saveBytesAsFile(fileName, bytes);
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Could not save file";
      if (msg.toLowerCase().includes("cancel")) return;
      window.alert(msg);
    } finally {
      saveBtn.disabled = false;
    }
  };

  row.append(openBtn, saveBtn);
  container.appendChild(row);
}

import { api } from "./api/daemon";
import type { ChatMessage } from "./api/types";

export async function resolveMessageFileBytes(
  m: ChatMessage,
): Promise<Uint8Array> {
  if (m.fileData?.length) return m.fileData;
  if (m.blobId) {
    const r = await api.blob(m.blobId);
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

export function downloadBase64File(name: string, dataBase64: string): void {
  const bin = atob(dataBase64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  saveBytesAsFile(name, bytes);
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

export function saveBytesAsFile(name: string, bytes: Uint8Array): void {
  const blob = new Blob([toBlobPart(bytes)]);
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name || "download";
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function openBytesInNewTab(bytes: Uint8Array): void {
  const blob = new Blob([toBlobPart(bytes)]);
  const url = URL.createObjectURL(blob);
  window.open(url, "_blank", "noopener,noreferrer");
  window.setTimeout(() => URL.revokeObjectURL(url), 120_000);
}

export function attachFileActions(
  container: HTMLElement,
  fileName: string,
  loadBytes: () => Promise<Uint8Array>,
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
      const bytes = await loadBytes();
      openBytesInNewTab(bytes);
    } catch (e) {
      window.alert(e instanceof Error ? e.message : "Could not open file");
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
      const bytes = await loadBytes();
      saveBytesAsFile(fileName, bytes);
    } catch (e) {
      window.alert(e instanceof Error ? e.message : "Could not save file");
    } finally {
      saveBtn.disabled = false;
    }
  };

  row.append(openBtn, saveBtn);
  container.appendChild(row);
}

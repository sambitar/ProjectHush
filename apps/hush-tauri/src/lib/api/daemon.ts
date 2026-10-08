import { invoke } from "@tauri-apps/api/core";
import { isTauriShell } from "../tauriEnv";
import type {
  ChatRecordDto,
  ContactDto,
  InviteResponse,
  MailboxDto,
  RecvResponse,
  StatusResponse,
} from "./types";

type Transport =
  | { kind: "unix"; path: string }
  | { kind: "tcp"; base: string };

/**
 * How to reach the daemon. The desktop shell injects the real transport
 * (Unix socket by default, DR-02); a plain browser falls back to loopback HTTP.
 */
function transport(): Transport {
  const t = window.__HUSH_TRANSPORT__;
  if (t && (t.kind === "unix" || t.kind === "tcp")) return t;
  return { kind: "tcp", base: window.__HUSH_API__ || "http://127.0.0.1:9876" };
}

export function getApiBase(): string {
  const t = transport();
  return t.kind === "unix" ? `unix://${t.path}` : t.base;
}

export class AppError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AppError";
  }
}

interface RawReply {
  status: number;
  body: string;
}

async function send(
  method: string,
  path: string,
  body: string | undefined,
): Promise<RawReply> {
  const t = transport();
  if (t.kind === "unix") {
    if (!isTauriShell()) {
      throw new AppError(
        `The daemon is on a Unix socket (${t.path}); open the Hush desktop app to use it.`,
      );
    }
    const reply = await invoke<RawReply>("hush_api", {
      method,
      path,
      body: body ?? null,
    });
    return { status: reply.status, body: reply.body };
  }
  const res = await fetch(t.base + path, {
    method,
    headers: { "Content-Type": "application/json" },
    body,
  });
  return { status: res.status, body: await res.text() };
}

async function request<T>(
  path: string,
  opts: RequestInit = {},
): Promise<T | null> {
  const method = typeof opts.method === "string" ? opts.method : "GET";
  const body = typeof opts.body === "string" ? opts.body : undefined;
  let reply: RawReply;
  try {
    reply = await send(method, path, body);
  } catch (e) {
    if (e instanceof AppError) throw e;
    throw new AppError(
      `Can't reach the Hush daemon at ${getApiBase()}. Start it with: cargo run -p hush-daemon -- --dir <vault>`,
    );
  }
  if (reply.status < 200 || reply.status >= 300) {
    let msg = "Something went wrong";
    try {
      const j = JSON.parse(reply.body) as { error?: string };
      if (j.error) msg = j.error;
    } catch {
      if (reply.body && reply.body.length < 200) msg = reply.body;
    }
    if (reply.status === 401 && path.includes("unlock")) {
      throw new AppError("That passphrase didn't work. Try again.");
    }
    throw new AppError(msg);
  }
  if (reply.status === 204 || !reply.body) return null;
  return JSON.parse(reply.body) as T;
}

export const api = {
  status: () => request<StatusResponse>("/api/status"),
  unlock: (passphrase: string) =>
    request<null>("/api/unlock", {
      method: "POST",
      body: JSON.stringify({ passphrase }),
    }),
  lock: () => request<null>("/api/lock", { method: "POST" }),
  changePassphrase: (current_passphrase: string, new_passphrase: string) =>
    request<null>("/api/change-passphrase", {
      method: "POST",
      body: JSON.stringify({ current_passphrase, new_passphrase }),
    }),
  invite: () =>
    request<InviteResponse>("/api/invite", { method: "POST" }),
  contacts: () => request<ContactDto[]>("/api/contacts"),
  addContact: (nickname: string, invite: string) =>
    request<null>("/api/contacts/add", {
      method: "POST",
      body: JSON.stringify({ nickname, invite }),
    }),
  removeContact: (nickname: string) =>
    request<null>("/api/contacts/remove", {
      method: "POST",
      body: JSON.stringify({ nickname }),
    }),
  sendText: (to: string, text: string) =>
    request<null>("/api/send", {
      method: "POST",
      body: JSON.stringify({ to, text }),
    }),
  sendFileData: (to: string, name: string, data_base64: string) =>
    request<{ blob_id: string }>("/api/send-file-data", {
      method: "POST",
      body: JSON.stringify({ to, name, data_base64 }),
    }),
  presenceHeartbeat: () =>
    request<null>("/api/presence/heartbeat", { method: "POST" }),
  recv: () => request<RecvResponse>("/api/recv", { method: "POST" }),
  messages: () => request<ChatRecordDto[]>("/api/messages"),
  mail: () => request<MailboxDto>("/api/mail"),
  sendMail: (
    to: string,
    body: {
      thread_id?: string;
      subject?: string;
      body: string;
      attachments: { name: string; data_base64: string }[];
    },
  ) =>
    request<{ thread_id: string; queued: boolean }>("/api/mail/send", {
      method: "POST",
      body: JSON.stringify({ to, ...body }),
    }),
  saveDraft: (body: {
    id?: string;
    thread_id?: string;
    to: string;
    subject?: string;
    body: string;
    attachments: { name: string; data_base64: string }[];
  }) =>
    request<{ id: string }>("/api/mail/draft", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  deleteDraft: (id: string) =>
    request<null>(`/api/mail/draft/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  /** Remove a mail record from the log. Trash's "delete forever". */
  deleteMail: (id: string) =>
    request<null>(`/api/mail/${encodeURIComponent(id)}`, { method: "DELETE" }),
  setMailHandle: (handle: string) =>
    request<null>("/api/mail/handle", {
      method: "POST",
      body: JSON.stringify({ handle }),
    }),
  blob: (id: string) =>
    request<{ data_base64: string }>(`/api/blob/${encodeURIComponent(id)}`),
  backupExport: () =>
    request<{ filename: string; data_base64: string }>("/api/backup/export", {
      method: "POST",
    }),
  backupImport: (data_base64: string) =>
    request<null>("/api/backup/import", {
      method: "POST",
      body: JSON.stringify({ data_base64 }),
    }),
  recoveryPending: () =>
    request<{ code: string }>("/api/recovery/pending"),
  recoveryAcknowledge: () =>
    request<null>("/api/recovery/acknowledge", { method: "POST" }),
  uiPrefsGet: () => request<import("./uiPrefs").UiPrefsDto>("/api/ui-prefs"),
  uiPrefsPut: (prefs: import("./uiPrefs").UiPrefsDto) =>
    request<null>("/api/ui-prefs", {
      method: "PUT",
      body: JSON.stringify(prefs),
    }),
  linkDeviceStart: () =>
    request<import("./types").LinkDeviceStartResponse>(
      "/api/link-device/start",
      { method: "POST" },
    ),
  linkDevicePull: (source_api: string, link_id: string) =>
    request<null>("/api/link-device/pull", {
      method: "POST",
      body: JSON.stringify({ source_api, link_id }),
    }),
};

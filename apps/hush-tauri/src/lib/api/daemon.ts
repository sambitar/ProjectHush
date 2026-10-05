import type {
  ContactDto,
  InviteResponse,
  RecvResponse,
  StatusResponse,
} from "./types";

export function getApiBase(): string {
  return window.__HUSH_API__ || "http://127.0.0.1:9876";
}

export class AppError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AppError";
  }
}

async function request<T>(
  path: string,
  opts: RequestInit = {},
): Promise<T | null> {
  let res: Response;
  try {
    res = await fetch(getApiBase() + path, {
      headers: { "Content-Type": "application/json" },
      ...opts,
    });
  } catch {
    throw new AppError(
      `Can't reach the Hush daemon at ${getApiBase()}. Start it with: cargo run -p hush-daemon -- --dir <vault>`,
    );
  }
  if (!res.ok) {
    let msg = "Something went wrong";
    const body = await res.text();
    try {
      const j = JSON.parse(body) as { error?: string };
      if (j.error) msg = j.error;
    } catch {
      if (body && body.length < 200) msg = body;
    }
    if (res.status === 401 && path.includes("unlock")) {
      throw new AppError("That passphrase didn't work. Try again.");
    }
    throw new AppError(msg);
  }
  if (res.status === 204) return null;
  return res.json() as Promise<T>;
}

export const api = {
  status: () => request<StatusResponse>("/api/status"),
  unlock: (passphrase: string) =>
    request<null>("/api/unlock", {
      method: "POST",
      body: JSON.stringify({ passphrase }),
    }),
  lock: () => request<null>("/api/lock", { method: "POST" }),
  invite: () =>
    request<InviteResponse>("/api/invite", { method: "POST" }),
  contacts: () => request<ContactDto[]>("/api/contacts"),
  addContact: (nickname: string, invite: string) =>
    request<null>("/api/contacts/add", {
      method: "POST",
      body: JSON.stringify({ nickname, invite }),
    }),
  sendText: (to: string, text: string) =>
    request<null>("/api/send", {
      method: "POST",
      body: JSON.stringify({ to, text }),
    }),
  sendFileData: (to: string, name: string, data_base64: string) =>
    request<null>("/api/send-file-data", {
      method: "POST",
      body: JSON.stringify({ to, name, data_base64 }),
    }),
  recv: () => request<RecvResponse>("/api/recv", { method: "POST" }),
};

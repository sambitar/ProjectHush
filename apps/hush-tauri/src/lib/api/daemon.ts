import { invoke } from "@tauri-apps/api/core";
import { isTauriShell } from "../tauriEnv";
import type {
  ChatRecordDto,
  ContactDto,
  InviteResponse,
  MailboxDto,
  NotesNoteResponse,
  NotesStateDto,
  ConclaveDto,
  PendingInviteDto,
  RecvResponse,
  RelayConfigResponse,
  StatusResponse,
  ReadsFeedResponse,
  ReadsReaderDto,
  ReadsSourceDto,
  ReadsStateDto,
  ReadsStoryDetailDto,
  ReadsStoryDto,
  TorboxUserDto,
  TorrentsStateDto,
  WalletAddressDto,
  WalletChain,
  WalletCreateDto,
  WalletNetworkMode,
  WalletSendDto,
  WalletStateDto,
} from "./types";

type Transport =
  | { kind: "unix"; path: string }
  | { kind: "tcp"; base: string };

/**
 * `?api=http://127.0.0.1:9877` points one browser tab at one daemon, so a single
 * Vite dev server can drive both peers during development.
 */
function apiFromQuery(): string | null {
  const v = new URLSearchParams(window.location.search).get("api");
  return v ? v.replace(/\/+$/, "") : null;
}

/**
 * How to reach the daemon. The desktop shell injects the real transport
 * (Unix socket by default, DR-02); a plain browser falls back to loopback HTTP.
 */
function transport(): Transport {
  const t = window.__HUSH_TRANSPORT__;
  if (t && (t.kind === "unix" || t.kind === "tcp")) return t;
  return {
    kind: "tcp",
    base: window.__HUSH_API__ || apiFromQuery() || "http://127.0.0.1:9876",
  };
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
  /** Every conclave this vault holds, each with its own message log. */
  conclaves: () => request<ConclaveDto[]>("/api/conclaves"),
  createConclave: (name: string, trustees: string[]) =>
    request<ConclaveDto>("/api/conclaves", {
      method: "POST",
      body: JSON.stringify({ name, trustees }),
    }),
  /**
   * Send in a conclave. The daemon fans this out to every joined trustee over
   * their own 1:1 session, so the response says nothing about per-trustee
   * delivery.
   */
  sendConclaveMessage: (conclaveId: string, body: string) =>
    request<null>("/api/conclaves/message", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, body }),
    }),
  /**
   * Share a file in a conclave. The daemon stores a local blob and fans one
   * encrypted copy out to every joined trustee; the response carries the
   * sender's blob id.
   */
  sendConclaveFileData: (conclaveId: string, name: string, data_base64: string) =>
    request<{ blob_id: string }>("/api/conclaves/file", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, name, data_base64 }),
    }),
  /** Invite a contact. Convener only; the invitee must accept. */
  inviteConclaveTrustee: (conclaveId: string, nickname: string) =>
    request<null>("/api/conclaves/trustees/invite", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, nickname }),
    }),
  /** Remove a trustee. Convener only; converges to the joined trustees. */
  kickConclaveTrustee: (conclaveId: string, nickname: string) =>
    request<null>("/api/conclaves/trustees/kick", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, nickname }),
    }),
  /** Accept an invite: the only path that materialises a conclave locally. */
  acceptConclaveInvite: (conclaveId: string) =>
    request<ConclaveDto>("/api/conclaves/trustees/accept", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId }),
    }),
  declineConclaveInvite: (conclaveId: string) =>
    request<null>("/api/conclaves/trustees/decline", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId }),
    }),
  /** Leave a conclave. Any joined trustee may; the convener leaving ends it. */
  leaveConclave: (conclaveId: string) =>
    request<null>("/api/conclaves/trustees/leave", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId }),
    }),
  /** Invitations received but not yet accepted or declined. */
  pendingInvites: () => request<PendingInviteDto[]>("/api/conclaves/invites"),
  /**
   * End a conclave for everyone: the convener broadcasts an `ended` descriptor
   * so it becomes read-only on every peer. The log is kept on every vault.
   */
  endConclave: (conclaveId: string) =>
    request<null>("/api/conclaves/end", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId }),
    }),
  /**
   * Delete an ended conclave and its whole message history from this vault.
   * Local only: the other trustees keep their own copies, and the vault blobs
   * behind the shared files stay.
   */
  deleteConclave: (conclaveId: string) =>
    request<null>(`/api/conclaves/${encodeURIComponent(conclaveId)}`, {
      method: "DELETE",
    }),
  blob: (id: string) =>
    request<{ data_base64: string }>(`/api/blob/${encodeURIComponent(id)}`),
  /** The account's drop key and the page outsiders post to. */
  dropsKey: () => request<import("./types").DropsKeyDto>("/api/drops/key"),
  /** Create the drop keypair. Fails when one exists; rotate instead. */
  dropsKeyCreate: () =>
    request<import("./types").DropsKeyDto>("/api/drops/key", {
      method: "POST",
      body: JSON.stringify({}),
    }),
  /** Rotate: the retired key keeps draining until the relay TTL expires it. */
  dropsKeyRotate: () =>
    request<import("./types").DropsKeyDto>("/api/drops/rotate", {
      method: "POST",
      body: JSON.stringify({}),
    }),
  /** One drain pass now. The daemon also drains after every poll. */
  dropsDrain: () =>
    request<import("./types").DropsDrainDto>("/api/drops/drain", {
      method: "POST",
      body: JSON.stringify({}),
    }),
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
  /** The relay switch's state; readable while the vault is locked. */
  relayConfig: () => request<RelayConfigResponse>("/api/relay"),
  // --- Reads (Hacker News reader). Content routes serve public data and
  // work even while locked; state routes answer 401 until unlocked. ---
  readsFeed: (source: string, page = 0) =>
    request<ReadsFeedResponse>(
      `/api/reads/feed?source=${encodeURIComponent(source)}&page=${page}`,
    ),
  readsSources: () => request<ReadsSourceDto[]>("/api/reads/sources"),
  readsAddSource: (url: string) =>
    request<ReadsSourceDto[]>("/api/reads/sources", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ url }),
    }),
  readsRemoveSource: (id: string) =>
    request<ReadsSourceDto[]>(`/api/reads/sources/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  readsStory: (id: string) =>
    request<ReadsStoryDetailDto>(`/api/reads/story/${encodeURIComponent(id)}`),
  readsReader: (url: string) =>
    request<ReadsReaderDto>(`/api/reads/reader?url=${encodeURIComponent(url)}`),
  readsState: () => request<ReadsStateDto>("/api/reads/state"),
  readsToggleSaved: (story: ReadsStoryDto) =>
    request<ReadsStateDto>("/api/reads/saved", {
      method: "POST",
      body: JSON.stringify({ story }),
    }),
  readsToggleHidden: (id: string) =>
    request<ReadsStateDto>("/api/reads/hidden", {
      method: "POST",
      body: JSON.stringify({ id }),
    }),
  readsMarkRead: (id: string, title: string) =>
    request<ReadsStateDto>("/api/reads/read", {
      method: "POST",
      body: JSON.stringify({ id, title }),
    }),
  // --- Notes (private sticky-paper board). State routes answer 401 until
  // unlocked; there is no network path. ---
  notesState: () => request<NotesStateDto>("/api/notes/state"),
  notesSaveNote: (draft: {
    id?: string;
    title: string;
    body: string;
    color?: string;
  }) =>
    request<NotesNoteResponse>("/api/notes/note", {
      method: "POST",
      body: JSON.stringify(draft),
    }),
  notesDeleteNote: (id: string) =>
    request<NotesStateDto>(`/api/notes/note/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  // --- Torrents (TorBox-powered client). The API key lives in the vault
  // sidecar and is never sent back; the daemon does all TorBox traffic. ---
  torrentsState: () => request<TorrentsStateDto>("/api/torrents/state"),
  torrentsTestKey: (key: string) =>
    request<TorboxUserDto>("/api/torrents/test", {
      method: "POST",
      body: JSON.stringify({ key }),
    }),
  torrentsSetKey: (key: string) =>
    request<TorrentsStateDto>("/api/torrents/key", {
      method: "POST",
      body: JSON.stringify({ key }),
    }),
  torrentsRefresh: () =>
    request<TorrentsStateDto>("/api/torrents/refresh", { method: "POST" }),
  torrentsAdd: (source: string) =>
    request<TorrentsStateDto>("/api/torrents/add", {
      method: "POST",
      body: JSON.stringify({ source }),
    }),
  torrentsAddFile: (name: string, data_base64: string) =>
    request<TorrentsStateDto>("/api/torrents/add-file", {
      method: "POST",
      body: JSON.stringify({ name, data_base64 }),
    }),
  torrentsPause: (id: string) =>
    request<TorrentsStateDto>("/api/torrents/pause", {
      method: "POST",
      body: JSON.stringify({ id }),
    }),
  torrentsResume: (id: string) =>
    request<TorrentsStateDto>("/api/torrents/resume", {
      method: "POST",
      body: JSON.stringify({ id }),
    }),
  torrentsRetry: (id: string) =>
    request<TorrentsStateDto>("/api/torrents/retry", {
      method: "POST",
      body: JSON.stringify({ id }),
    }),
  torrentsRemove: (id: string) =>
    request<TorrentsStateDto>("/api/torrents/remove", {
      method: "POST",
      body: JSON.stringify({ id }),
    }),
  torrentsSetConcurrency: (max: number) =>
    request<TorrentsStateDto>("/api/torrents/concurrency", {
      method: "POST",
      body: JSON.stringify({ max }),
    }),
  torrentsDeleteLibrary: (id: string) =>
    request<TorrentsStateDto>(
      `/api/torrents/library/${encodeURIComponent(id)}`,
      { method: "DELETE" },
    ),
  /**
   * Flip the relay switch. `socks` is the shell Tor's SOCKS port, reported
   * when switching on; the daemon refuses to fall back to clearnet on its
   * own, so with the switch on and Tor down, requests fail until it connects.
   */
  setRelayMode: (onion: boolean, socks: string | null) =>
    request<null>("/api/relay/mode", {
      method: "POST",
      body: JSON.stringify({ onion, socks }),
    }),
  /** Attach the relay's v3 onion address; applied live, or staged while locked. */
  attachOnion: (onion: string) =>
    request<null>("/api/relay/onion", {
      method: "POST",
      body: JSON.stringify({ onion }),
    }),
  // --- Wallet (self-custody, Bitcoin and Ethereum). The phrase and the change
  // set are sealed in the vault sidecar, and the daemon serves these routes only
  // over the Unix socket, so a TCP-only daemon answers 404 here. ---
  walletState: () => request<WalletStateDto>("/api/wallet"),
  /** `mode` is the posture the wallet is created in. Sent explicitly because
   *  the daemon's own default is Tor, and a Tor wallet cannot be built before
   *  the shell has Tor listening. */
  walletCreate: (
    network: string,
    ethereumNetwork: string,
    mode: WalletNetworkMode,
  ) =>
    request<WalletCreateDto>("/api/wallet/create", {
      method: "POST",
      body: JSON.stringify({
        network,
        ethereum_network: ethereumNetwork,
        mode,
      }),
    }),
  walletRestore: (
    mnemonic: string,
    network: string,
    ethereumNetwork: string,
    mode: WalletNetworkMode,
  ) =>
    request<WalletStateDto>("/api/wallet/restore", {
      method: "POST",
      body: JSON.stringify({
        mnemonic,
        network,
        ethereum_network: ethereumNetwork,
        mode,
      }),
    }),
  walletAddress: (chain: WalletChain) =>
    request<WalletAddressDto>("/api/wallet/address", {
      method: "POST",
      body: JSON.stringify({ chain }),
    }),
  walletSync: (chain: WalletChain) =>
    request<WalletStateDto>("/api/wallet/sync", {
      method: "POST",
      body: JSON.stringify({ chain }),
    }),
  /** Amounts are base units as decimal strings: satoshi, wei, or a token's
   * smallest unit. See `walletStore.parseUnits` for the conversion. */
  walletSend: (send: {
    chain: WalletChain;
    to: string;
    amount: string;
    fee_rate?: number | null;
    token?: string | null;
  }) =>
    request<WalletSendDto>("/api/wallet/send", {
      method: "POST",
      body: JSON.stringify(send),
    }),
  walletSetMode: (mode: WalletNetworkMode) =>
    request<WalletStateDto>("/api/wallet/mode", {
      method: "POST",
      body: JSON.stringify({ mode }),
    }),
  /** Tell the daemon where the shell's Tor is listening, for the wallet.
   *  `null` says Tor is down; the wallet keeps its mode and refuses to reach
   *  the chain rather than falling back to a direct connection. */
  walletSetProxy: (socks: string | null) =>
    request<WalletStateDto>("/api/wallet/proxy", {
      method: "POST",
      body: JSON.stringify({ socks }),
    }),
  /** Track an ERC-20 token; the daemon reads its symbol and decimals. */
  walletTokenAdd: (address: string) =>
    request<WalletStateDto>("/api/wallet/token", {
      method: "POST",
      body: JSON.stringify({ address }),
    }),
  walletTokenRemove: (address: string) =>
    request<WalletStateDto>("/api/wallet/token/remove", {
      method: "POST",
      body: JSON.stringify({ address }),
    }),
  walletWipe: () => request<null>("/api/wallet/wipe", { method: "POST" }),
};

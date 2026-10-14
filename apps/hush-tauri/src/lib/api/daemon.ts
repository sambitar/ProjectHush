import { invoke } from "@tauri-apps/api/core";
import { isTauriShell } from "../tauriEnv";
import type {
  ChatRecordDto,
  ContactDto,
  IntroListsDto,
  InviteResponse,
  MailboxDto,
  NotesNoteResponse,
  NotesStateDto,
  PaperPresenceDto,
  PaperViewDto,
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
  VoiceStateDto,
  VoiceViewDto,
  ZineDraftDto,
  ZinesViewDto,
  ZinesWriteDto,
  ZineExportViewDto,
  ZineViewDto,
  PenNameDto,
  PublicIdentityDto,
  NeighborhoodsViewDto,
  NeighborhoodArticleDto,
  NeighborhoodCommentViewDto,
  NeighborhoodSettingsDto,
  NeighborhoodSettingsPatch,
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
  presenceOffline: () =>
    request<null>("/api/presence/offline", { method: "POST" }),
  recv: () => request<RecvResponse>("/api/recv", { method: "POST" }),
  messages: () => request<ChatRecordDto[]>("/api/messages"),
  /** Delete 1:1 chat messages with a contact; no bounds = the whole history. */
  deleteChats: (contact: string, from_ms?: number, to_ms?: number) =>
    request<{ deleted: number }>("/api/chats/delete", {
      method: "POST",
      body: JSON.stringify({ contact, from_ms, to_ms }),
    }),
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
  /** The introduction queues: requests to answer or relay, invites to accept. */
  introLists: () => request<IntroListsDto>("/api/conclaves/intros"),
  /**
   * Offer an introduction: ask a member this vault knows (the target) to share
   * their invite with another member it also knows (the seeker).
   */
  introduceMembers: (conclaveId: string, seeker: string, target: string) =>
    request<null>("/api/conclaves/intros/introduce", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, seeker, target }),
    }),
  /** Ask the conclave for an introduction to a member this vault cannot reach. */
  requestIntro: (conclaveId: string, target: string) =>
    request<null>("/api/conclaves/intros/request", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, target }),
    }),
  /** Relay a held introduction request on to its target. */
  relayIntroRequest: (conclaveId: string, seeker: string, target: string) =>
    request<null>("/api/conclaves/intros/relay", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, seeker, target }),
    }),
  /** Answer a request addressed to this vault: share the invite or decline. */
  answerIntroRequest: (conclaveId: string, seeker: string, accept: boolean) =>
    request<null>("/api/conclaves/intros/answer", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, seeker, accept }),
    }),
  /** Consume a delivered introduction: add the target as a contact. */
  acceptIntro: (conclaveId: string, target: string, nickname: string) =>
    request<ContactDto[]>("/api/conclaves/intros/accept", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, target, nickname }),
    }),
  /** Dismiss a delivered introduction without adding the contact. */
  dismissIntro: (conclaveId: string, target: string) =>
    request<null>("/api/conclaves/intros/dismiss", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, target }),
    }),
  /** Drop a held introduction request without relaying or answering it. */
  dismissIntroRequest: (conclaveId: string, seeker: string) =>
    request<null>("/api/conclaves/intros/requests/dismiss", {
      method: "POST",
      body: JSON.stringify({ conclave_id: conclaveId, seeker }),
    }),
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
  // --- Paper (the conclave's shared collaborative document). The daemon owns
  // the merged CRDT state; the editor sends opaque updates and reads back
  // merged ones. 401 until unlocked, 404 when the conclave has no paper. ---
  paperGet: (conclaveId: string) =>
    request<PaperViewDto>(`/api/paper/${encodeURIComponent(conclaveId)}`),
  paperCreate: (conclaveId: string, title: string) =>
    request<PaperViewDto>(`/api/paper/${encodeURIComponent(conclaveId)}/create`, {
      method: "POST",
      body: JSON.stringify({ title }),
    }),
  paperUpdate: (conclaveId: string, update: string) =>
    request<PaperViewDto>(`/api/paper/${encodeURIComponent(conclaveId)}/update`, {
      method: "POST",
      body: JSON.stringify({ update }),
    }),
  // Presence rides the DataChannel only: it has no relay fallback, so an empty
  // peer list means "nobody is on a channel", not "nobody is here".
  paperPresence: (conclaveId: string) =>
    request<PaperPresenceDto>(
      `/api/paper/${encodeURIComponent(conclaveId)}/presence`,
    ),
  paperPublishPresence: (conclaveId: string, frame: string) =>
    request<null>(`/api/paper/${encodeURIComponent(conclaveId)}/presence`, {
      method: "POST",
      body: JSON.stringify({ frame }),
    }),
  // --- Conclave voice (Task 23). The room state is in-memory on the daemon:
  // a restart or a lock is a leave, and 404 means no room is open. ---
  voiceState: () => request<VoiceStateDto>("/api/voice/state"),
  voiceGet: (conclaveId: string) =>
    request<VoiceViewDto>(`/api/voice/${encodeURIComponent(conclaveId)}`),
  voiceOpen: (conclaveId: string) =>
    request<VoiceViewDto>(`/api/voice/${encodeURIComponent(conclaveId)}/open`, {
      method: "POST",
    }),
  voiceJoin: (conclaveId: string) =>
    request<VoiceViewDto>(`/api/voice/${encodeURIComponent(conclaveId)}/join`, {
      method: "POST",
    }),
  voiceLeave: (conclaveId: string) =>
    request<VoiceViewDto>(`/api/voice/${encodeURIComponent(conclaveId)}/leave`, {
      method: "POST",
    }),
  voiceMute: (conclaveId: string, muted: boolean) =>
    request<VoiceViewDto>(`/api/voice/${encodeURIComponent(conclaveId)}/mute`, {
      method: "POST",
      body: JSON.stringify({ muted }),
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
  // --- Zines (a member's own publication, and the ones they hold). Local and
  // offline in Phase 1: authoring, storing, and reading in this vault. Every
  // route goes through the worker, so a locked vault answers 401 without any
  // of these being touched. The pen name is taken from the vault, never from
  // the request. ---
  zinesIndex: (filter: { section?: string; pen?: string } = {}) => {
    const q = new URLSearchParams();
    if (filter.section) q.set("section", filter.section);
    if (filter.pen) q.set("pen", filter.pen);
    const suffix = q.toString();
    return request<ZinesViewDto>(`/api/zines${suffix ? `?${suffix}` : ""}`);
  },
  zinesGet: (zineId: string) =>
    request<ZineViewDto>(`/api/zines/${encodeURIComponent(zineId)}`),
  zinesWrite: (draft: ZineDraftDto) =>
    request<ZinesWriteDto>("/api/zines", {
      method: "POST",
      body: JSON.stringify(draft),
    }),
  /** An explicit flag rather than a toggle, so a double-click cannot land in
   *  the state the user did not ask for. */
  zinesMute: (zineId: string, muted = true) =>
    request<ZinesViewDto>(`/api/zines/${encodeURIComponent(zineId)}/mute`, {
      method: "POST",
      body: JSON.stringify({ muted }),
    }),
  /** Forget a zine locally. Not a retraction. */
  zinesForget: (zineId: string) =>
    request<ZinesViewDto>(`/api/zines/${encodeURIComponent(zineId)}`, {
      method: "DELETE",
    }),
  zinesPenName: () => request<PenNameDto | null>("/api/zines/pen-name"),
  zinesSetPenName: (name: string) =>
    request<ZinesViewDto>("/api/zines/pen-name", {
      method: "POST",
      body: JSON.stringify({ name }),
    }),
  /** The public identity this vault publishes and comments under on the public
   *  Nostr network, or null before one is minted. */
  publicIdentity: () =>
    request<PublicIdentityDto | null>("/api/neighborhoods/identity"),
  /** Mint the public identity. Idempotent: a create when one exists returns it. */
  createPublicIdentity: () =>
    request<PublicIdentityDto>("/api/neighborhoods/identity", {
      method: "POST",
    }),
  // --- Neighborhoods: the public read lane (Task 22). The index is read from
  // the daemon's encrypted cache; the UI never talks to a relay. ---
  /** The cached public index. The filters are advisory. */
  neighborhoods: (filter?: { section?: string; pub?: string }) => {
    const query = new URLSearchParams();
    if (filter?.section) query.set("section", filter.section);
    if (filter?.pub) query.set("pub", filter.pub);
    const suffix = query.toString();
    return request<NeighborhoodsViewDto>(
      `/api/neighborhoods${suffix ? `?${suffix}` : ""}`,
    );
  },
  /** One public article with its body. 404 before it is held. */
  neighborhoodArticle: (address: string) =>
    request<NeighborhoodArticleDto>(
      `/api/neighborhoods/${encodeURIComponent(address)}`,
    ),
  /** Force a pull from the allowlisted relays (the switch-on path). */
  refreshNeighborhoods: () =>
    request<NeighborhoodsViewDto>("/api/neighborhoods/refresh", {
      method: "POST",
    }),
  /** Post a NIP-22 comment on a public article, signed by the public identity.
   *
   *  `parentId` answers a comment rather than the article. The article stays the
   *  root either way, which is how NIP-22 threads without a second kind. */
  neighborhoodComment: (address: string, body: string, parentId?: string) =>
    request<NeighborhoodCommentViewDto>(
      `/api/neighborhoods/${encodeURIComponent(address)}/comment`,
      {
        method: "POST",
        body: JSON.stringify({ body, parent_id: parentId ?? null }),
      },
    ),
  neighborhoodSettings: () =>
    request<NeighborhoodSettingsDto>("/api/neighborhoods/settings"),
  setNeighborhoodSettings: (patch: NeighborhoodSettingsPatch) =>
    request<NeighborhoodSettingsDto>("/api/neighborhoods/settings", {
      method: "POST",
      body: JSON.stringify(patch),
    }),
  // --- The wire: a grant hands the read key to one contact over the session
  // that already exists, and a comment is sealed to the author's pen name and
  // published to the bulletin lane. Both are queued by the worker, so a
  // `published: false` answer is a status rather than a lost write. ---
  /** Hand one contact this post's read key. Any holder may share. */
  zinesGrant: (zineId: string, contact: string) =>
    request<ZinesViewDto>(`/api/zines/${encodeURIComponent(zineId)}/grant`, {
      method: "POST",
      body: JSON.stringify({ contact }),
    }),
  zinesComment: (zineId: string, body: string) =>
    request<ZinesWriteDto>(`/api/zines/${encodeURIComponent(zineId)}/comment`, {
      method: "POST",
      body: JSON.stringify({ body }),
    }),
  /** Merge a pulled comment. This is a revision, and it republishes the post. */
  zinesAcceptComment: (zineId: string, commentHash: string) =>
    request<ZinesWriteDto>(
      `/api/zines/${encodeURIComponent(zineId)}/accept-comment`,
      { method: "POST", body: JSON.stringify({ comment_hash: commentHash }) },
    ),
  /** Refuse a pulled comment. Not a retraction: it was never published. */
  zinesDiscardComment: (zineId: string, commentHash: string) =>
    request<ZineViewDto>(
      `/api/zines/${encodeURIComponent(zineId)}/discard-comment`,
      { method: "POST", body: JSON.stringify({ comment_hash: commentHash }) },
    ),
  /** Publish this zine to the neighborhoods as a NIP-23 article (Task 11).
   *
   *  One-way: the article carries the title, the sections and the body as
   *  Markdown, and nothing else — no thread, no history, no byline. Exporting
   *  again after an edit replaces the article at the same address rather than
   *  publishing a second one, which is why this is safe to retry. */
  zinesExport: (zineId: string) =>
    request<ZineExportViewDto>(
      `/api/zines/${encodeURIComponent(zineId)}/export`,
      { method: "POST" },
    ),
};

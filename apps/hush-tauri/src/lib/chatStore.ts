import type {
  ChatMessage,
  ChatRecordDto,
  ExpireMessageDto,
  ReceivedDto,
} from "./api/types";
import { formatUnits } from "./walletStore.ts";

const threads = new Map<string, ChatMessage[]>();
/** Unread messages per contact nickname. */
const unread = new Map<string, number>();
let idSeq = 0;

/**
 * Live timers for expiring messages, keyed by contact and message id.
 *
 * A message is never written to the vault, so this timer is the only thing that
 * can delete it. `clearChatStore` clears them with the threads; `loadHistory`
 * keeps the live ones, because a history refresh must not make an unexpired
 * message disappear early.
 */
const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();

type ExpiryListener = (nick: string, id: string) => void;
const expiryListeners = new Set<ExpiryListener>();

function expiryKey(nick: string, id: string): string {
  return `${nick}\u0000${id}`;
}

function scheduleExpiry(nick: string, msg: ChatMessage): void {
  if (msg.expiresAt === undefined) return;
  const key = expiryKey(nick, msg.id);
  const previous = expiryTimers.get(key);
  if (previous !== undefined) clearTimeout(previous);
  const delay = Math.max(0, msg.expiresAt - Date.now());
  const timer = setTimeout(() => {
    expiryTimers.delete(key);
    if (removeEphemeral(nick, msg.id)) {
      for (const listener of expiryListeners) listener(nick, msg.id);
    }
  }, delay);
  expiryTimers.set(key, timer);
}

/**
 * Subscribe to a message reaching its deadline.
 *
 * The store owns the deletion; the caller only needs to repaint. Returns an
 * unsubscribe function, which the app does not need today but keeps the
 * listener set from becoming impossible to clean up.
 */
export function onEphemeralExpired(listener: ExpiryListener): () => void {
  expiryListeners.add(listener);
  return () => expiryListeners.delete(listener);
}

/** Remove one expiring message. Returns false when it was already gone. */
export function removeEphemeral(nick: string, id: string): boolean {
  const list = threads.get(nick);
  if (!list) return false;
  const at = list.findIndex((m) => m.id === id && m.expiresAt !== undefined);
  if (at < 0) return false;
  const [msg] = list.splice(at, 1);
  const timer = expiryTimers.get(expiryKey(nick, id));
  if (timer !== undefined) clearTimeout(timer);
  expiryTimers.delete(expiryKey(nick, id));
  // An unread badge must not outlive the message that earned it. A contact
  // whose badge was cleared by opening the chat has no entry here, so this
  // cannot underflow.
  if (!msg.outgoing && unread.has(nick)) {
    const left = (unread.get(nick) ?? 1) - 1;
    if (left > 0) unread.set(nick, left);
    else unread.delete(nick);
  }
  return true;
}

function nextId(): string {
  idSeq += 1;
  return `m-${Date.now()}-${idSeq}`;
}

export function clearChatStore(): void {
  for (const timer of expiryTimers.values()) clearTimeout(timer);
  expiryTimers.clear();
  threads.clear();
  unread.clear();
  idSeq = 0;
}

export function loadHistory(records: ChatRecordDto[]): void {
  // A history reload is a read of the vault, and expiring messages are not in
  // it. The live ones are kept in memory and re-armed, so a payment notice (or
  // any other refresh) cannot delete a message before its deadline.
  const now = Date.now();
  const live = [...threads.values()]
    .flat()
    .filter((m) => m.expiresAt !== undefined && m.expiresAt > now);
  clearChatStore();
  for (const r of records) {
    const base = {
      id: r.id,
      contactNick: r.contact_nick,
      outgoing: r.outgoing,
      createdAt: r.ts_ms,
      reactions: [] as string[],
    };
    if (r.payload.t === "text") {
      push(r.contact_nick, { ...base, kind: "text", text: r.payload.body });
    } else if (r.payload.t === "file") {
      push(r.contact_nick, {
        ...base,
        kind: "file",
        fileName: r.payload.name,
        fileSize: r.payload.size,
        blobId: r.payload.blob_id_hex.toLowerCase(),
      });
    } else if (r.payload.t === "payment") {
      push(r.contact_nick, {
        ...base,
        kind: "payment",
        amount: r.payload.amount,
        symbol: r.payload.symbol,
        txid: r.payload.txid,
      });
    }
    // Mail records belong to the Mail rail; the chat store ignores them.
  }
  for (const m of live) {
    const list = threads.get(m.contactNick) ?? [];
    if (list.some((x) => x.id === m.id)) continue;
    const at = list.findIndex((x) => x.createdAt > m.createdAt);
    if (at < 0) list.push(m);
    else list.splice(at, 0, m);
    threads.set(m.contactNick, list);
    scheduleExpiry(m.contactNick, m);
  }
}

export function getThread(nick: string): ChatMessage[] {
  const list = threads.get(nick) ?? [];
  const now = Date.now();
  // The timer is the deleter, but a backgrounded window can throttle it. A
  // message past its deadline must not be drawn again while the timer waits.
  if (!list.some((m) => m.expiresAt !== undefined && m.expiresAt <= now)) {
    return list;
  }
  return list.filter((m) => m.expiresAt === undefined || m.expiresAt > now);
}

export function getAllThreads(): Map<string, ChatMessage[]> {
  return threads;
}

export function markRead(nick: string): void {
  unread.delete(nick);
}

export function hasUnread(nick: string): boolean {
  return unread.has(nick);
}

/** Unread messages across every contact, for the rail badge. */
export function unreadTotal(): number {
  let total = 0;
  for (const n of unread.values()) total += n;
  return total;
}

export function lastPreview(nick: string): { text: string; time: number } | null {
  const t = getThread(nick);
  if (!t?.length) return null;
  const m = t[t.length - 1];
  const text =
    m.kind === "text"
      ? m.text ?? ""
      : m.kind === "payment"
        ? `${m.outgoing ? "Sent" : "Received"} ${formatUnits(
            m.amount ?? "0",
            6,
          )} ${m.symbol ?? "USDC"}`
        : `File: ${m.fileName ?? "attachment"}`;
  return { text, time: m.createdAt };
}

export function addOutgoingText(nick: string, text: string): ChatMessage {
  const msg: ChatMessage = {
    id: nextId(),
    contactNick: nick,
    outgoing: true,
    kind: "text",
    text,
    createdAt: Date.now(),
    reactions: [],
  };
  push(nick, msg);
  return msg;
}

export function addOutgoingFile(
  nick: string,
  name: string,
  size: number,
  data: Uint8Array,
): ChatMessage {
  const msg: ChatMessage = {
    id: nextId(),
    contactNick: nick,
    outgoing: true,
    kind: "file",
    fileName: name,
    fileSize: size,
    fileData: data,
    createdAt: Date.now(),
    reactions: [],
    sendProgress: 0,
  };
  push(nick, msg);
  return msg;
}

export function removeOutgoingMessage(nick: string, id: string): void {
  const list = threads.get(nick);
  if (!list) return;
  const i = list.findIndex((x) => x.id === id && x.outgoing);
  if (i >= 0) list.splice(i, 1);
}

export function setSendProgress(id: string, p: number): void {
  for (const list of threads.values()) {
    const m = list.find((x) => x.id === id);
    if (m) {
      m.sendProgress = p;
      return;
    }
  }
}

export function clearSendProgress(nick: string, id: string): void {
  const m = threads.get(nick)?.find((x) => x.id === id);
  if (m) m.sendProgress = undefined;
}

export function setMessageBlobId(
  nick: string,
  id: string,
  blobId: string,
): void {
  const m = threads.get(nick)?.find((x) => x.id === id);
  if (m) m.blobId = blobId.toLowerCase();
}

export function ingestIncoming(from: string, dto: ReceivedDto): ChatMessage {
  const msg: ChatMessage = {
    id: nextId(),
    contactNick: from,
    outgoing: false,
    kind: dto.kind === "file" ? "file" : "text",
    text: dto.text,
    fileName: dto.file_name,
    blobId: dto.blob_id?.toLowerCase(),
    fileSize: dto.file_size,
    createdAt: Date.now(),
    reactions: [],
  };
  push(from, msg);
  unread.set(from, (unread.get(from) ?? 0) + 1);
  return msg;
}

/**
 * Add an expiring line the daemon just reported.
 *
 * The payload is already out of the vault, so this is the only copy. A retried
 * delivery carries the same sender id and is ignored rather than drawn twice.
 */
export function ingestEphemeral(
  from: string,
  dto: ExpireMessageDto,
): ChatMessage | null {
  if (dto.expires_ms <= Date.now()) return null;
  if (!dto.text) return null;
  if (getThread(from).some((m) => m.id === dto.id)) return null;
  const msg: ChatMessage = {
    id: dto.id,
    contactNick: from,
    outgoing: false,
    kind: "text",
    text: dto.text,
    createdAt: Date.now(),
    expiresAt: dto.expires_ms,
    reactions: [],
  };
  push(from, msg);
  unread.set(from, (unread.get(from) ?? 0) + 1);
  scheduleExpiry(from, msg);
  return msg;
}

/** Add the sender's own expiring line, after the daemon has accepted it. */
export function addOutgoingEphemeral(
  nick: string,
  id: string,
  text: string,
  expiresAt: number,
): ChatMessage {
  const msg: ChatMessage = {
    id,
    contactNick: nick,
    outgoing: true,
    kind: "text",
    text,
    createdAt: Date.now(),
    expiresAt,
    reactions: [],
  };
  push(nick, msg);
  scheduleExpiry(nick, msg);
  return msg;
}

function push(nick: string, msg: ChatMessage): void {
  const list = threads.get(nick) ?? [];
  list.push(msg);
  threads.set(nick, list);
}

export function toggleReaction(msgId: string, emoji: string): void {
  for (const list of threads.values()) {
    const m = list.find((x) => x.id === msgId);
    if (m) {
      const i = m.reactions.indexOf(emoji);
      if (i >= 0) m.reactions.splice(i, 1);
      else m.reactions.push(emoji);
      return;
    }
  }
}

export function allFileMessages(): ChatMessage[] {
  const out: ChatMessage[] = [];
  for (const list of threads.values()) {
    for (const m of list) {
      if (m.kind === "file") out.push(m);
    }
  }
  return out.sort((a, b) => b.createdAt - a.createdAt);
}

export type FileDirection = "received" | "sent";

export function fileMessagesForContact(
  nick: string,
  direction: FileDirection,
): ChatMessage[] {
  return getThread(nick)
    .filter((m) => m.kind === "file")
    .filter((m) => (direction === "received" ? !m.outgoing : m.outgoing))
    .sort((a, b) => b.createdAt - a.createdAt);
}

export function fileCountForContact(nick: string): number {
  return getThread(nick).filter((m) => m.kind === "file").length;
}

export function lastFileTimeForContact(nick: string): number | null {
  const files = fileMessagesForContact(nick, "received").concat(
    fileMessagesForContact(nick, "sent"),
  );
  if (!files.length) return null;
  return Math.max(...files.map((m) => m.createdAt));
}

/** Contacts with at least one file, most recent exchange first. */
export function contactsWithFiles(contactNicks: string[]): string[] {
  return contactNicks
    .map((nick) => ({
      nick,
      at: lastFileTimeForContact(nick) ?? 0,
      n: fileCountForContact(nick),
    }))
    .filter((x) => x.n > 0)
    .sort((a, b) => b.at - a.at)
    .map((x) => x.nick);
}

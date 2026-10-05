import { RELAY_TTL_MS } from "./constants";
import type { ChatMessage, DeliveryState, ReceivedDto } from "./api/types";

const threads = new Map<string, ChatMessage[]>();
const unread = new Set<string>();
let idSeq = 0;

function nextId(): string {
  idSeq += 1;
  return `m-${Date.now()}-${idSeq}`;
}

export function clearChatStore(): void {
  threads.clear();
  unread.clear();
  idSeq = 0;
}

export function getThread(nick: string): ChatMessage[] {
  return threads.get(nick) ?? [];
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

export function lastPreview(nick: string): { text: string; time: number } | null {
  const t = threads.get(nick);
  if (!t?.length) return null;
  const m = t[t.length - 1];
  const text =
    m.kind === "text"
      ? m.text ?? ""
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
    delivery: "sealed",
    reactions: [],
  };
  push(nick, msg);
  advanceDelivery(msg);
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
    delivery: "sealed",
    reactions: [],
    sendProgress: 0,
  };
  push(nick, msg);
  return msg;
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

export function markOutgoingDelivered(nick: string, id: string): void {
  const m = threads.get(nick)?.find((x) => x.id === id);
  if (m) {
    m.delivery = "delivered";
    m.sendProgress = undefined;
  }
}

export function ingestIncoming(from: string, dto: ReceivedDto): ChatMessage {
  const msg: ChatMessage = {
    id: nextId(),
    contactNick: from,
    outgoing: false,
    kind: dto.kind === "file" ? "file" : "text",
    text: dto.text,
    fileName: dto.file_name,
    savedPath: dto.saved_path,
    createdAt: Date.now(),
    delivery: "delivered",
    reactions: [],
  };
  push(from, msg);
  unread.add(from);
  return msg;
}

function push(nick: string, msg: ChatMessage): void {
  const list = threads.get(nick) ?? [];
  list.push(msg);
  threads.set(nick, list);
}

function advanceDelivery(msg: ChatMessage): void {
  setTimeout(() => {
    msg.delivery = "queued";
    msg.queuedUntil = Date.now() + RELAY_TTL_MS;
  }, 400);
  setTimeout(() => {
    if (msg.delivery === "queued") msg.delivery = "delivered";
  }, 1200);
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

export function deliveryLabel(d: DeliveryState): string {
  switch (d) {
    case "sealed":
      return "Sealed";
    case "queued":
      return "In queue";
    case "delivered":
      return "Delivered";
    case "expired":
      return "Expired";
  }
}

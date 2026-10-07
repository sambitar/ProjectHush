import type { ChatRecordDto, ReceivedDto } from "./api/types";

export interface MailAttachmentRef {
  name: string;
  size: number;
  blobId: string;
}

export interface MailMessage {
  id: string;
  threadId: string;
  contactNick: string;
  outgoing: boolean;
  subject?: string;
  body: string;
  attachments: MailAttachmentRef[];
  createdAt: number;
}

export interface MailThread {
  threadId: string;
  contactNick: string;
  subject: string;
  lastAt: number;
  count: number;
}

/**
 * Mail lives on the same encrypted log as chat; the store only groups those
 * records into threads for the Mail rail. Nothing is persisted here.
 */
const byThread = new Map<string, MailMessage[]>();
let seq = 0;

function nextId(): string {
  seq += 1;
  return `mail-${Date.now()}-${seq}`;
}

export function clearMailStore(): void {
  byThread.clear();
  seq = 0;
}

function push(m: MailMessage): void {
  const list = byThread.get(m.threadId) ?? [];
  list.push(m);
  byThread.set(m.threadId, list);
}

export function loadMail(records: ChatRecordDto[]): void {
  clearMailStore();
  for (const r of records) {
    if (r.payload.t !== "mail") continue;
    push({
      id: r.id,
      threadId: r.payload.thread_id,
      contactNick: r.contact_nick,
      outgoing: r.outgoing,
      subject: r.payload.subject ?? undefined,
      body: r.payload.body,
      attachments: r.payload.attachments.map((a) => ({
        name: a.name,
        size: a.size,
        blobId: a.blob_id_hex.toLowerCase(),
      })),
      createdAt: r.ts_ms,
    });
  }
}

export function ingestIncomingMail(dto: ReceivedDto): MailMessage | null {
  if (dto.kind !== "mail" || !dto.thread_id) return null;
  const m: MailMessage = {
    id: nextId(),
    threadId: dto.thread_id,
    contactNick: dto.from,
    outgoing: false,
    subject: dto.subject ?? undefined,
    body: dto.text ?? "",
    attachments: (dto.attachments ?? []).map((a) => ({
      name: a.name,
      size: a.size,
      blobId: a.blob_id.toLowerCase(),
    })),
    createdAt: Date.now(),
  };
  push(m);
  return m;
}

export function addOutgoingMail(
  threadId: string,
  contactNick: string,
  subject: string | undefined,
  body: string,
  attachments: MailAttachmentRef[],
): MailMessage {
  const m: MailMessage = {
    id: nextId(),
    threadId,
    contactNick,
    outgoing: true,
    subject,
    body,
    attachments,
    createdAt: Date.now(),
  };
  push(m);
  return m;
}

export function getThread(threadId: string): MailMessage[] {
  return byThread.get(threadId) ?? [];
}

export function listThreads(): MailThread[] {
  const out: MailThread[] = [];
  for (const [threadId, msgs] of byThread) {
    if (!msgs.length) continue;
    const last = msgs[msgs.length - 1];
    out.push({
      threadId,
      contactNick: last.contactNick,
      subject: msgs[0].subject || "(no subject)",
      lastAt: last.createdAt,
      count: msgs.length,
    });
  }
  return out.sort((a, b) => b.lastAt - a.lastAt);
}

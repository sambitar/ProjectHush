import type { ChatRecordDto } from "./api/types";
import { htmlToText } from "./richText";

/** The Mail sidebar. Order is display order. */
export type MailFolder =
  | "inbox"
  | "drops"
  | "sent"
  | "outbox"
  | "drafts"
  | "starred"
  | "archive"
  | "trash";

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
  /** `queued` is mail the relay refused; the daemon retries it from the Outbox. */
  state: "sent" | "queued";
}

export interface MailDraft {
  id: string;
  /** Set when the draft is a reply, so sending keeps the conversation. */
  threadId?: string;
  to: string;
  subject: string;
  body: string;
  attachments: MailAttachmentRef[];
  updatedAt: number;
}

/**
 * An outsider drop, opened by the daemon and filed here. There is no sender
 * and no thread: the only provenance is the rendezvous slot it arrived on,
 * and rendering is quarantined (plain text, save-only attachments) no matter
 * what the body looks like.
 */
export interface DropMessage {
  id: string;
  /** The rendezvous slot — 32 hex chars, which published key claimed it. */
  rendezvousId: string;
  subject?: string;
  body: string;
  attachments: MailAttachmentRef[];
  /** When the dropper sealed it. */
  createdAt: number;
  /** When the daemon drained it. */
  receivedAt: number;
}

/** Read/star/archive/trash, straight out of `ui-prefs.json`. */
export interface MailFlags {
  read: Set<string>;
  starred: Set<string>;
  archived: Set<string>;
  trashed: Set<string>;
}

export function emptyFlags(): MailFlags {
  return {
    read: new Set(),
    starred: new Set(),
    archived: new Set(),
    trashed: new Set(),
  };
}

/**
 * Mail lives on the same encrypted log as chat; the daemon hands over the mail
 * records and the drafts, and this groups them. Nothing is persisted here — read
 * and folder state is in `ui-prefs`, which is what makes it survive a restart.
 */
let messages: MailMessage[] = [];
let drafts: MailDraft[] = [];
let drops: DropMessage[] = [];

/**
 * Contact nicknames currently in the vault's contact list.
 *
 * Mail from someone who is no longer a contact stays in the encrypted log — a
 * delete keeps history — but it must not surface in the Mail rail, or the
 * removed person would still appear there. `null` means the list has not been
 * loaded yet, in which case nothing is hidden.
 */
let knownContacts: Set<string> | null = null;

export function setKnownContacts(nicks: string[]): void {
  knownContacts = new Set(nicks);
}

/** Whether a message belongs to someone who is still a contact. */
function fromKnownContact(m: MailMessage): boolean {
  return knownContacts === null || knownContacts.has(m.contactNick);
}

/** Whether a draft is addressed to someone who is still a contact. */
function draftToKnownContact(d: MailDraft): boolean {
  if (knownContacts === null) return true;
  return knownContacts.has(d.to);
}

export function clearMailStore(): void {
  messages = [];
  drafts = [];
  drops = [];
  knownContacts = null;
}

function attachmentsOf(
  refs: { name: string; size: number; blob_id_hex: string }[],
): MailAttachmentRef[] {
  return refs.map((a) => ({
    name: a.name,
    size: a.size,
    blobId: a.blob_id_hex.toLowerCase(),
  }));
}

/** Rebuild the mailbox from the daemon. Folder state is not touched: it is not
 *  ours to hold, and a refresh racing a click must not undo the click.
 *
 *  Returns whether anything actually changed, so the poll can skip a repaint —
 *  and so a repaint cannot steal the caret out of a search box for nothing. */
export function loadMailbox(
  mail: ChatRecordDto[],
  draftRecords: ChatRecordDto[],
  dropRecords: ChatRecordDto[] = [],
): boolean {
  const nextMessages: MailMessage[] = [];
  for (const r of mail) {
    if (r.payload.t !== "mail") continue;
    nextMessages.push({
      id: r.id,
      threadId: r.payload.thread_id,
      contactNick: r.contact_nick,
      outgoing: r.outgoing,
      subject: r.payload.subject ?? undefined,
      body: r.payload.body,
      attachments: attachmentsOf(r.payload.attachments),
      createdAt: r.ts_ms,
      state: r.payload.state,
    });
  }

  const nextDrafts: MailDraft[] = [];
  for (const r of draftRecords) {
    if (r.payload.t !== "draft") continue;
    nextDrafts.push({
      id: r.id,
      threadId: r.payload.thread_id ?? undefined,
      to: r.payload.to,
      subject: r.payload.subject ?? "",
      body: r.payload.body,
      attachments: attachmentsOf(r.payload.attachments),
      updatedAt: r.payload.updated_ms || r.ts_ms,
    });
  }

  const nextDrops: DropMessage[] = [];
  for (const r of dropRecords) {
    if (r.payload.t !== "drop") continue;
    nextDrops.push({
      id: r.id,
      rendezvousId: r.payload.rendezvous_id,
      subject: r.payload.subject ?? undefined,
      body: r.payload.body,
      attachments: attachmentsOf(r.payload.attachments),
      createdAt: (r.payload.created_unix || 0) * 1000,
      receivedAt: r.ts_ms,
    });
  }

  const before = mailboxSignature();
  messages = nextMessages;
  drafts = nextDrafts;
  drops = nextDrops;
  return before !== mailboxSignature();
}

/** Everything the Mail rail renders, reduced to a comparable string. */
function mailboxSignature(): string {
  const mail = messages
    .map((m) => `${m.id}:${m.state}:${m.createdAt}:${m.attachments.length}`)
    .join("|");
  const draft = drafts
    .map((d) => `${d.id}:${d.updatedAt}:${d.attachments.length}`)
    .join("|");
  const drop = drops
    .map((d) => `${d.id}:${d.receivedAt}:${d.attachments.length}`)
    .join("|");
  return `${mail}#${draft}#${drop}`;
}

export function isUnread(m: MailMessage, flags: MailFlags): boolean {
  // Your own mail is never unread, and a message you have opened is in `read`.
  return !m.outgoing && !flags.read.has(m.id);
}

/**
 * Whether a message shows in a folder.
 *
 * Folder membership is derived, not stored: one message is legitimately in the
 * Inbox *and* in Starred at once, which is how mail clients behave. Trash wins
 * over everything, and Archive means "out of the Inbox" rather than "gone".
 */
export function inFolder(
  m: MailMessage,
  folder: MailFolder,
  flags: MailFlags,
): boolean {
  if (flags.trashed.has(m.id)) return folder === "trash";
  switch (folder) {
    case "trash":
      return false;
    // Drops are their own records with their own listing; a mail message is
    // never in the Drops folder.
    case "drops":
      return false;
    case "starred":
      return flags.starred.has(m.id);
    case "archive":
      return flags.archived.has(m.id);
    case "inbox":
      return !m.outgoing && !flags.archived.has(m.id);
    case "sent":
      return m.outgoing && m.state === "sent";
    case "outbox":
      return m.outgoing && m.state === "queued";
    case "drafts":
      // Drafts are their own records, not messages.
      return false;
  }
}

/** Messages in a folder, newest first. */
export function listFolder(folder: MailFolder, flags: MailFlags): MailMessage[] {
  return messages
    .filter((m) => inFolder(m, folder, flags) && fromKnownContact(m))
    .sort((a, b) => b.createdAt - a.createdAt);
}

export function searchMessages(rows: MailMessage[], query: string): MailMessage[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return rows;
  return rows.filter(
    (m) =>
      m.contactNick.toLowerCase().includes(needle) ||
      (m.subject ?? "").toLowerCase().includes(needle) ||
      htmlToText(m.body).toLowerCase().includes(needle),
  );
}

/**
 * One conversation, newest first — classic email style, where a reply shows at
 * the top of the thread above the mail it answers.
 */
export function getThread(threadId: string): MailMessage[] {
  return messages
    .filter((m) => m.threadId === threadId && fromKnownContact(m))
    .sort((a, b) => b.createdAt - a.createdAt);
}

/** A thread's display subject: the earliest message that set one. */
export function threadSubject(threadId: string): string {
  // The thread is newest first, so the earliest subject sits at the end.
  const msgs = getThread(threadId).filter((m) => m.subject);
  return msgs[msgs.length - 1]?.subject || "(no subject)";
}

/** The one-line summary shown under the subject in the list. */
export function previewText(body: string, attachmentCount = 0): string {
  const text = body.replace(/\s+/g, " ").trim();
  if (text) return text;
  if (attachmentCount) {
    return `${attachmentCount} attachment${attachmentCount === 1 ? "" : "s"}`;
  }
  return "(no content)";
}

export function preview(m: MailMessage): string {
  return previewText(htmlToText(m.body), m.attachments.length);
}

export function draftPreview(d: MailDraft): string {
  return previewText(htmlToText(d.body), d.attachments.length);
}

export function listDrafts(): MailDraft[] {
  return drafts
    .filter(draftToKnownContact)
    .sort((a, b) => b.updatedAt - a.updatedAt);
}

export function draftById(id: string): MailDraft | undefined {
  return drafts.find((d) => d.id === id);
}

/** Filed drops, newest arrival first. Drops have no sender to filter on. */
export function listDrops(): DropMessage[] {
  return [...drops].sort((a, b) => b.receivedAt - a.receivedAt);
}

export function getDropById(id: string): DropMessage | undefined {
  return drops.find((d) => d.id === id);
}

/** A drop you have not opened yet. Same read set the mail flags use. */
export function isDropUnread(d: DropMessage, flags: MailFlags): boolean {
  return !flags.read.has(d.id);
}

export function folderCount(folder: MailFolder, flags: MailFlags): number {
  if (folder === "drafts") return listDrafts().length;
  if (folder === "drops") return listDrops().length;
  return listFolder(folder, flags).length;
}

export function unreadCount(flags: MailFlags): number {
  return messages.filter(
    (m) => inFolder(m, "inbox", flags) && isUnread(m, flags) && fromKnownContact(m),
  ).length;
}

export function hasUnreadMail(flags: MailFlags): boolean {
  return unreadCount(flags) > 0;
}

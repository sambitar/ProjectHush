import type { ChatRecordDto } from "./api/types";

/** The Mail sidebar. Order is display order. */
export type MailFolder =
  | "inbox"
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

export function clearMailStore(): void {
  messages = [];
  drafts = [];
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

  const before = mailboxSignature();
  messages = nextMessages;
  drafts = nextDrafts;
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
  return `${mail}#${draft}`;
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
    .filter((m) => inFolder(m, folder, flags))
    .sort((a, b) => b.createdAt - a.createdAt);
}

export function searchMessages(rows: MailMessage[], query: string): MailMessage[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return rows;
  return rows.filter(
    (m) =>
      m.contactNick.toLowerCase().includes(needle) ||
      (m.subject ?? "").toLowerCase().includes(needle) ||
      m.body.toLowerCase().includes(needle),
  );
}

/** One conversation, oldest first — the order you read it in. */
export function getThread(threadId: string): MailMessage[] {
  return messages
    .filter((m) => m.threadId === threadId)
    .sort((a, b) => a.createdAt - b.createdAt);
}

/** A thread's display subject: the earliest message that set one. */
export function threadSubject(threadId: string): string {
  return getThread(threadId).find((m) => m.subject)?.subject || "(no subject)";
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
  return previewText(m.body, m.attachments.length);
}

export function listDrafts(): MailDraft[] {
  return [...drafts].sort((a, b) => b.updatedAt - a.updatedAt);
}

export function draftById(id: string): MailDraft | undefined {
  return drafts.find((d) => d.id === id);
}

export function folderCount(folder: MailFolder, flags: MailFlags): number {
  if (folder === "drafts") return drafts.length;
  return listFolder(folder, flags).length;
}

export function unreadCount(flags: MailFlags): number {
  return messages.filter(
    (m) => inFolder(m, "inbox", flags) && isUnread(m, flags),
  ).length;
}

export function hasUnreadMail(flags: MailFlags): boolean {
  return unreadCount(flags) > 0;
}

import type { ChatRecordDto } from "./api/types";
import { htmlToText } from "./richText.ts";
import { filterText } from "./rowFilter.ts";

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

/**
 * Mail attachments per contact, in one pass over the folders.
 *
 * `listFolder` filters and sorts the whole store, so answering "how many
 * attachments has this contact exchanged" one contact at a time pays for the
 * whole store once per contact. Both callers want every contact's count at once
 * — the Vault tile's file total and the Files list's per-row count — and asking
 * per contact made them quadratic: measured at ~20 ms for 50 contacts against
 * 2 000 messages, against ~0.8 ms for this. The counts are the same for every
 * caller, so they are derived once and read from the map.
 *
 * The mailbox is small enough that this is not a cache: it is one pass per call,
 * which is what the callers were paying per contact.
 */
export function mailAttachmentCounts(flags: MailFlags): Map<string, number> {
  const seen = new Set<string>();
  const counts = new Map<string, number>();
  for (const m of [
    ...listFolder("inbox", flags),
    ...listFolder("sent", flags),
    ...listFolder("archive", flags),
  ]) {
    // The three folders overlap: a reply that was archived still has its sent
    // copy, and one message must not contribute its attachments twice.
    if (seen.has(m.id)) continue;
    seen.add(m.id);
    if (!m.attachments.length) continue;
    counts.set(m.contactNick, (counts.get(m.contactNick) ?? 0) + m.attachments.length);
  }
  return counts;
}

/**
 * What a mail search looks at, as one lowercased haystack: who it is with, the
 * subject, and the body as plain text.
 *
 * The body is the part that matters and the part the row cannot supply — a mail
 * row draws a truncated preview, so a row matched against its own visible text
 * would miss a word that fell past the ellipsis. The list stamps this on the row
 * instead, which is why it lives here beside the folder rules rather than in the
 * view: it is the definition of what searching mail means, and the row is just
 * one consumer of it.
 */
export function mailSearchText(m: MailMessage): string {
  return filterText(m.contactNick, m.subject, htmlToText(m.body));
}

/**
 * What a drop search looks at: subject and body. A drop has no sender to match
 * on — its who column reads "Anonymous" for every one of them.
 */
export function dropSearchText(d: DropMessage): string {
  return filterText(d.subject, d.body);
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

/**
 * The Drops folder's own view: what arrived and has not been filed away.
 * Archived and trashed drops leave this list the way archived mail leaves the
 * Inbox — they surface in the shared Archive and Trash folders instead.
 */
export function listDropInbox(flags: MailFlags): DropMessage[] {
  return listDrops().filter(
    (d) => !flags.trashed.has(d.id) && !flags.archived.has(d.id),
  );
}

/**
 * Drops filed into one of the shared folders. Trash wins over archive, the
 * same rule `inFolder` applies to mail, and Starred is an overlay: a starred
 * drop still lives in the Drops list and appears here too.
 */
export function listDropsInFolder(
  folder: MailFolder,
  flags: MailFlags,
): DropMessage[] {
  if (folder !== "trash" && folder !== "archive" && folder !== "starred") {
    return [];
  }
  return listDrops().filter((d) => {
    if (flags.trashed.has(d.id)) return folder === "trash";
    if (folder === "starred") return flags.starred.has(d.id);
    return folder === "archive" && flags.archived.has(d.id);
  });
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
  // The Drops badge tracks what the folder shows, so filed-away drops do not
  // count toward it; the shared folders count their drops alongside mail.
  if (folder === "drops") return listDropInbox(flags).length;
  return listFolder(folder, flags).length + listDropsInFolder(folder, flags).length;
}

export function unreadCount(flags: MailFlags): number {
  return messages.filter(
    (m) => inFolder(m, "inbox", flags) && isUnread(m, flags) && fromKnownContact(m),
  ).length;
}

/** What one folder's badge says: how many, and whether it counts as unread. */
export interface FolderBadge {
  /** What to show. Zero means no badge at all, not a "0". */
  count: number;
  /** True when the number is unread rather than merely present, which the
   *  caller draws differently. */
  unread: boolean;
}

/**
 * What one folder's badge shows.
 *
 * Inbox and Drops lead with what is *unread* in them; every other folder shows
 * how much is in it at all. This is read by the folder column's first paint and
 * by the in-place patch a mail reload uses, so it is one function rather than
 * two expressions that have to agree.
 */
export function folderBadge(folder: MailFolder, flags: MailFlags): FolderBadge {
  const unread =
    folder === "inbox"
      ? unreadCount(flags)
      : folder === "drops"
        ? listDropInbox(flags).filter((d) => isDropUnread(d, flags)).length
        : 0;
  return { count: unread > 0 ? unread : folderCount(folder, flags), unread: unread > 0 };
}

export function hasUnreadMail(flags: MailFlags): boolean {
  return unreadCount(flags) > 0;
}

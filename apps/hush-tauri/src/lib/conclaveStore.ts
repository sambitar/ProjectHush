import type {
  ConclaveDto,
  ConclaveMessageDto,
  ConclaveTrusteeDto,
  ContactDto,
  IntroListsDto,
  PendingInviteDto,
  PendingIntroDto,
  PendingIntroRequestDto,
  TrusteeStateDto,
} from "./api/types";
import { dayLabel } from "./messageGroups.ts";

/**
 * The Conclaves rail's view of a conclave.
 *
 * A conclave is a local record in the vault, not a relay-side room: the daemon
 * owns the record and its message log, and this store only mirrors what it last
 * returned. Nothing is persisted here — the record lives in the encrypted log,
 * which is what makes it survive a restart.
 */
/** A file shared into a conclave: a vault blob reference plus its display name. */
export interface ConclaveMessageFile {
  name: string;
  /** Vault blob id; bytes are fetched via `/api/blob/{id}`. */
  blobId: string;
  size: number;
}

export interface ConclaveMessage {
  id: string;
  outgoing: boolean;
  /** The sender's nickname as this vault knows it; absent for our own messages. */
  from?: string;
  body: string;
  /** Present when the message carried a file; `body` is empty then. */
  file?: ConclaveMessageFile;
  createdAt: number;
}

/** A person in a conclave, and where they are in the consensual join flow. */
export interface ConclaveTrustee {
  /** The trustee's stable account id. */
  accountId: string;
  /** The local nickname for this trustee; a display hint only. */
  nick: string;
  state: TrusteeStateDto;
}

export interface Conclave {
  id: string;
  name: string;
  /** The convener's account id: the only authority for invite, kick and end. */
  convener: string;
  trustees: ConclaveTrustee[];
  createdAt: number;
  messages: ConclaveMessage[];
  /** True once ended: read-only on every peer. */
  ended: boolean;
}

/** An invitation received but not yet accepted or declined. */
export interface PendingInvite {
  conclaveId: string;
  name: string;
  convener: string;
  fromAccount: string;
  fromNick: string;
  trustees: ConclaveTrustee[];
  receivedAt: number;
}

/**
 * An introduction request waiting on this vault.
 *
 * When `target` is this vault's account, the seeker wants our invite and
 * consent is asked. Otherwise this vault is the courier: the request came from
 * the seeker over their channel, and the relay prompt asks whether to carry it
 * to a member this vault knows.
 */
export interface PendingIntroRequest {
  conclaveId: string;
  conclaveName: string;
  seeker: string;
  seekerNick: string;
  target: string;
  targetNick: string;
  via: string;
  receivedAt: number;
}

/** A delivered introduction waiting on an accept or dismiss. */
export interface PendingIntro {
  conclaveId: string;
  conclaveName: string;
  target: string;
  targetNick: string;
  via: string;
  receivedAt: number;
}

let conclaves: Conclave[] = [];
let pendingInvites: PendingInvite[] = [];
let pendingIntroRequests: PendingIntroRequest[] = [];
let pendingIntros: PendingIntro[] = [];

/**
 * Unread message counts per conclave id, keyed the way the list renders them.
 *
 * Counted here rather than derived from the log: the store only ever mirrors
 * the daemon's full message array, so "which of those have I seen" has to be
 * remembered across polls. A conclave's own selection clears its count (the
 * pane is the read), and the first load clears everything — a freshly opened
 * app has no unread history, only new arrivals.
 */
const unread = new Map<string, number>();
let everLoaded = false;

export function clearConclaveStore(): void {
  conclaves = [];
  pendingInvites = [];
  pendingIntroRequests = [];
  pendingIntros = [];
  unread.clear();
  everLoaded = false;
}

function toMessage(m: ConclaveMessageDto): ConclaveMessage {
  return {
    id: m.id,
    outgoing: m.outgoing,
    from: m.from ?? undefined,
    body: m.body,
    file: m.file
      ? {
          name: m.file.name,
          blobId: m.file.blob_id_hex.toLowerCase(),
          size: m.file.size,
        }
      : undefined,
    createdAt: m.ts_ms,
  };
}

function toTrustee(t: ConclaveTrusteeDto): ConclaveTrustee {
  return { accountId: t.account_id, nick: t.nick, state: t.state };
}

function toConclave(dto: ConclaveDto): Conclave {
  return {
    id: dto.id,
    name: dto.name,
    convener: dto.convener,
    trustees: dto.trustees.map(toTrustee),
    createdAt: dto.created_ms,
    messages: dto.messages.map(toMessage),
    ended: dto.ended,
  };
}

function toInvite(dto: PendingInviteDto): PendingInvite {
  return {
    conclaveId: dto.conclave_id,
    name: dto.name,
    convener: dto.convener,
    fromAccount: dto.from_account,
    fromNick: dto.from_nick,
    trustees: dto.trustees.map(toTrustee),
    receivedAt: dto.received_ms,
  };
}

/**
 * Everything the list renders, reduced to a comparable string, so the poll can
 * skip a repaint that would steal the caret for nothing.
 */
function signature(list: Conclave[]): string {
  return list
    .map((m) => {
      const last = m.messages[m.messages.length - 1];
      const roster = m.trustees.map((t) => `${t.accountId}:${t.state}`).join(",");
      return `${m.id}:${m.name}:${m.convener}:${roster}:${m.ended}:${m.messages.length}:${
        last?.id ?? ""
      }`;
    })
    .join("|");
}

/**
 * Everything the list renders about a conclave except its messages: the set of
 * conclaves, their names, their rosters, who convened them, and whether they
 * have ended. Two loads with the same structure differ only by arriving
 * messages.
 */
function structure(list: Conclave[]): string {
  return list
    .map((m) => {
      const roster = m.trustees.map((t) => `${t.accountId}:${t.state}`).join(",");
      return `${m.id}:${m.name}:${m.convener}:${roster}:${m.ended}`;
    })
    .join("|");
}

/**
 * Did this load only add messages to conclaves that were already there?
 *
 * True means the list's structure is untouched and no log was pruned, so the
 * only thing on screen that can have changed is a card's unread number and
 * preview, or the chat of the open conclave — all of which can be painted in
 * place. Anything else is a rebuild.
 */
function grewOnly(before: Conclave[], next: Conclave[]): boolean {
  if (structure(before) !== structure(next)) return false;
  const was = new Map(before.map((m) => [m.id, m.messages.length]));
  return next.every((m) => m.messages.length >= (was.get(m.id) ?? 0));
}

/** What a load changed, and whether it was only arrivals. */
export interface ConclaveLoad {
  /** Anything the list renders changed. */
  changed: boolean;
  /**
   * Everything that changed was a message arriving in a conclave that already
   * existed — no conclave added, renamed, ended or pruned, and no roster
   * change. The caller may then patch the cards and the open chat in place
   * instead of rebuilding the shell under the reader.
   */
  grewOnly: boolean;
}

/**
 * Rebuild the conclave list from the daemon. Returns what changed, and whether
 * the change was only messages arriving (see `ConclaveLoad`).
 *
 * `activeId` is the conclave the user is reading (if any): growth there is
 * the user's own conversation, so it never counts as unread. Growth anywhere
 * else accumulates until that conclave is selected — the notification number
 * the list renders.
 */
export function loadConclaves(dtos: ConclaveDto[], activeId?: string | null): ConclaveLoad {
  const next = dtos.map(toConclave);
  const before = signature(conclaves);
  const grew = grewOnly(conclaves, next);
  if (everLoaded) {
    const seen = new Map(conclaves.map((m) => [m.id, m.messages.length]));
    for (const m of next) {
      const was = seen.get(m.id);
      const growth = was === undefined ? m.messages.length : m.messages.length - was;
      if (m.id === activeId) {
        unread.delete(m.id);
      } else if (growth > 0) {
        unread.set(m.id, (unread.get(m.id) ?? 0) + growth);
      }
      seen.delete(m.id);
    }
    // A conclave that vanished (ended and pruned by the daemon) has nothing
    // left to be unread.
    for (const id of seen.keys()) unread.delete(id);
  }
  everLoaded = true;
  conclaves = next;
  return { changed: before !== signature(next), grewOnly: grew };
}

/** The pane is the read: selecting a conclave clears its number. */
export function markConclaveRead(id: string): void {
  unread.delete(id);
}

/** Unread messages for one conclave, for its card's badge. */
export function conclaveUnreadCount(id: string): number {
  return unread.get(id) ?? 0;
}

/** Unread messages across every conclave, for the rail badge. */
export function conclaveUnreadTotal(): number {
  let total = 0;
  for (const n of unread.values()) total += n;
  return total;
}

/** Replace the pending-invite list. Returns whether anything changed. */
export function loadPendingInvites(dtos: PendingInviteDto[]): boolean {
  const next = dtos.map(toInvite);
  const before = pendingInvites.map((i) => `${i.conclaveId}:${i.name}`).join("|");
  pendingInvites = next;
  const after = pendingInvites.map((i) => `${i.conclaveId}:${i.name}`).join("|");
  return before !== after;
}

/** Invitations awaiting an accept or decline, newest first. */
export function listPendingInvites(): PendingInvite[] {
  return [...pendingInvites].sort((a, b) => b.receivedAt - a.receivedAt);
}

/** Which shape the Conclaves list column has. */
export type ConclaveListShape = "empty" | "list";

/**
 * Whether the Conclaves column should show its empty state, or the list.
 *
 * Two callers need to agree on this and they are the two that can disagree
 * without anyone noticing: the column's first paint, which draws one shape or the
 * other, and the patch that paints an arriving invitation, which has to say
 * whether the shape it found on screen is still the right one. An invitation
 * arriving into the empty state changes the shape, and a rebuild is what draws
 * that; nothing arriving into the empty state means there is nothing to paint.
 */
export function conclaveListShape(
  conclaves: number,
  invites: number,
): ConclaveListShape {
  return conclaves === 0 && invites === 0 ? "empty" : "list";
}

/**
 * Whether the invitation cards on screen are the ones the store is holding, in
 * the order it holds them.
 *
 * The patch that paints invitations asks this before touching the box. Its cards
 * carry the buttons the user is about to press, and a background delta with
 * nothing to do with invitations — a presence flip, an unread count — runs that
 * patch many times for every time one actually arrives. Equal means leave the
 * box exactly where it is; anything else means it has to be redrawn, because a
 * card the store no longer holds is a card that can no longer be answered.
 */
export function invitesAreCurrent(
  rendered: readonly string[],
  pending: readonly PendingInvite[],
): boolean {
  return (
    rendered.length === pending.length &&
    rendered.every((id, i) => id === pending[i].conclaveId)
  );
}

function toIntroRequest(dto: PendingIntroRequestDto): PendingIntroRequest {
  return {
    conclaveId: dto.conclave_id,
    conclaveName: dto.conclave_name,
    seeker: dto.seeker,
    seekerNick: dto.seeker_nick,
    target: dto.target,
    targetNick: dto.target_nick,
    via: dto.via,
    receivedAt: dto.received_ms,
  };
}

function toIntro(dto: PendingIntroDto): PendingIntro {
  return {
    conclaveId: dto.conclave_id,
    conclaveName: dto.conclave_name,
    target: dto.target,
    targetNick: dto.target_nick,
    via: dto.via,
    receivedAt: dto.received_ms,
  };
}

/**
 * Replace the introduction queues. Returns whether anything changed, so the
 * poll can skip a repaint that would steal the caret for nothing.
 */
export function loadPendingIntros(dtos: IntroListsDto): boolean {
  const requests = dtos.requests.map(toIntroRequest);
  const intros = dtos.intros.map(toIntro);
  const reqSig = (list: PendingIntroRequest[]) =>
    list.map((r) => `${r.conclaveId}:${r.seeker}:${r.target}`).join("|");
  const introSig = (list: PendingIntro[]) =>
    list.map((i) => `${i.conclaveId}:${i.target}`).join("|");
  const changed =
    reqSig(requests) !== reqSig(pendingIntroRequests) ||
    introSig(intros) !== introSig(pendingIntros);
  pendingIntroRequests = requests;
  pendingIntros = intros;
  return changed;
}

/** Introduction requests awaiting consent or a relay decision, newest first. */
export function listPendingIntroRequests(): PendingIntroRequest[] {
  return [...pendingIntroRequests].sort((a, b) => b.receivedAt - a.receivedAt);
}

/** Delivered introductions awaiting an accept or dismiss, newest first. */
export function listPendingIntros(): PendingIntro[] {
  return [...pendingIntros].sort((a, b) => b.receivedAt - a.receivedAt);
}

/** Conclaves, newest first. Ended conclaves are included: they are read-only. */
export function listConclaves(): Conclave[] {
  return [...conclaves].sort((a, b) => b.createdAt - a.createdAt);
}

export function conclaveById(id: string): Conclave | undefined {
  return conclaves.find((m) => m.id === id);
}

/** The joined trustees: the fan-out set. */
export function joinedTrustees(m: Conclave): ConclaveTrustee[] {
  return m.trustees.filter((t) => t.state === "joined");
}

/** The invited trustees: asked but not yet answered. */
export function invitedTrustees(m: Conclave): ConclaveTrustee[] {
  return m.trustees.filter((t) => t.state === "invited");
}

/**
 * A run of consecutive messages from one sender, on one day.
 *
 * `from` is the sender's nick for an incoming run and empty for this vault's
 * own; `messages` is never empty.
 */
export interface ConclaveMessageGroup {
  outgoing: boolean;
  from: string;
  day: string;
  messages: ConclaveMessage[];
}

/**
 * Collapse consecutive messages from the same sender into one group.
 *
 * The 1:1 thread has grouped its bubbles since it landed (`groupMessages`), so
 * a run from one person reads as one block with a single tail. The conclave
 * chat did not: every message carried its own avatar, its own sender name and
 * its own tail, so three messages in a row from one trustee read as three
 * separate people. The rule is the 1:1 one plus the sender, because a conclave
 * has more than two voices and only one of them is "not me".
 */
export function groupConclaveMessages(messages: ConclaveMessage[]): ConclaveMessageGroup[] {
  const groups: ConclaveMessageGroup[] = [];
  for (const m of messages) {
    const day = dayLabel(m.createdAt);
    const from = m.from ?? "";
    const last = groups[groups.length - 1];
    if (last && last.outgoing === m.outgoing && last.from === from && last.day === day) {
      last.messages.push(m);
    } else {
      groups.push({ outgoing: m.outgoing, from, day, messages: [m] });
    }
  }
  return groups;
}

/** The one-line summary under a conclave's name in the list. */
export function conclavePreview(m: Conclave): string {
  const last = m.messages[m.messages.length - 1];
  if (!last) return "No messages yet";
  const who = last.outgoing ? "You" : last.from ?? "Someone";
  if (last.file) return `${who}: ${last.file.name}`;
  return `${who}: ${last.body.replace(/\s+/g, " ").trim()}`;
}

/** Contacts not already a trustee (invited or joined), for the invite picker. */
export function candidatesToInvite(conclave: Conclave, contactNicks: string[]): string[] {
  const present = new Set(conclave.trustees.map((t) => t.nick));
  return contactNicks.filter((n) => !present.has(n));
}

/** The contact fields the conclave invite helpers need. */
export type InvitableContact = Pick<ContactDto, "nickname" | "reach">;

/**
 * Full-reach contacts who may still join this conclave.
 *
 * The header Invite button's enabled state and the invite modal's picker must
 * agree, so both derive their list here: a mail-only trust cannot join a
 * conclave (Task 24), and anyone already a trustee is out whatever their state
 * — left and kicked are terminal, so a departed member cannot be re-invited.
 */
export function invitableCandidates(
  conclave: Conclave,
  contacts: InvitableContact[],
): string[] {
  return candidatesToInvite(
    conclave,
    contacts.filter((c) => c.reach === "full").map((c) => c.nickname),
  );
}

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
 * Everything the Conclaves rail renders, reduced to a comparable string, so the
 * poll can skip a repaint that would steal the caret for nothing.
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
 * Rebuild the conclave list from the daemon. Returns whether anything changed.
 *
 * `activeId` is the conclave the user is reading (if any): growth there is
 * the user's own conversation, so it never counts as unread. Growth anywhere
 * else accumulates until that conclave is selected — the notification number
 * the list renders.
 */
export function loadConclaves(dtos: ConclaveDto[], activeId?: string | null): boolean {
  const next = dtos.map(toConclave);
  const before = signature(conclaves);
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
  return before !== signature(next);
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

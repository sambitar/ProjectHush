import type {
  ConclaveDto,
  ConclaveMessageDto,
  ConclaveTrusteeDto,
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

export function clearConclaveStore(): void {
  conclaves = [];
  pendingInvites = [];
  pendingIntroRequests = [];
  pendingIntros = [];
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

/** Rebuild the conclave list from the daemon. Returns whether anything changed. */
export function loadConclaves(dtos: ConclaveDto[]): boolean {
  const next = dtos.map(toConclave);
  const before = signature(conclaves);
  conclaves = next;
  return before !== signature(next);
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
